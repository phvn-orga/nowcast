//! The attachment rule and the death notification on real process trees.
//!
//! Each tree is a chain of shells, each a copy of `/bin/sh` placed at a path
//! that a test kind matches or not, ending with this test binary run again as
//! the connecting process (see [`connector`]). The test accepts its
//! connection and identifies it.

use std::ffi::OsStr;
use std::io::Read;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nowcast_identity::PeerIdentity;
use nowcast_traits::{ClientKind, Identity};
use nowcast_types::ClientKindName;
use rustix::process::{Pid, Signal, kill_process};
use tempfile::TempDir;
use tokio::net::{UnixListener, UnixStream};
use tokio::time::timeout;

/// Set on the connecting process: the socket to connect to.
const CONNECT: &str = "NOWCAST_IDENTITY_TEST_CONNECT";

/// Set on the connecting process: before connecting, start a child that
/// connects too, so that the anchor has two connected descendants.
const TWICE: &str = "NOWCAST_IDENTITY_TEST_TWICE";

/// Set on the connecting process: exit right after connecting.
const LEAVE: &str = "NOWCAST_IDENTITY_TEST_LEAVE";

/// Long enough for a loaded CI runner to start a process.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long a future that must not resolve is given to do so anyway.
const GRACE: Duration = Duration::from_millis(300);

/// Each shell writes its pid to the file named by `$0`, then runs the rest of
/// its arguments, the next shell or the connector. `; true` keeps the shell
/// alive as the parent instead of replacing itself.
const LAYER: &str = r#"echo $$ > "$0"; "$@"; true"#;

/// Not a test of its own: the connecting process of a tree, this binary run
/// again with [`CONNECT`] set. It stays connected until the test closes its
/// end of the connection.
#[test]
fn connector() {
    let Some(socket) = std::env::var_os(CONNECT) else {
        return;
    };
    let child = std::env::var_os(TWICE).map(|_| {
        Command::new(std::env::current_exe().unwrap())
            .args(["connector", "--exact", "--test-threads=1"])
            .env_remove(TWICE)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("start the second connector")
    });
    let mut stream = StdUnixStream::connect(socket).expect("connect to the test");
    if std::env::var_os(LEAVE).is_none() {
        let _ = stream.read(&mut [0]);
    }
    if let Some(mut child) = child {
        let _ = child.wait();
    }
}

/// Serializes writing executables and spawning processes: a process forked
/// while another thread has a copy open for writing would keep it open, and
/// executing that copy would fail with "text file busy".
static SPAWN: Mutex<()> = Mutex::new(());

struct Tree {
    /// The shells, their pid files and the connector's errors. Under the
    /// build directory rather than the system temporary one, which may be
    /// mounted without the right to execute.
    _shells: TempDir,
    /// The socket, kept apart because its path must stay short.
    _sockets: TempDir,
    listener: UnixListener,
    errors: PathBuf,
    launcher: Child,
    pid_files: Vec<PathBuf>,
    /// The test's end of the connections made by the connectors, the first
    /// one first.
    streams: Vec<UnixStream>,
}

impl Tree {
    /// Starts `shells`, paths relative to a fresh directory where each
    /// becomes a copy of `/bin/sh`, each the parent of the next, the last the
    /// parent of the connector. Returns once the connector has connected.
    async fn spawn(shells: &[&str]) -> Self {
        Self::spawn_with(shells, &[]).await
    }

    /// [`Tree::spawn`], with `env` set on the connector.
    async fn spawn_with(shells: &[&str], env: &[(&str, &str)]) -> Self {
        let dir = TempDir::new_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let sockets = TempDir::new().unwrap();
        let socket = sockets.path().join("socket");
        let listener = UnixListener::bind(&socket).unwrap();
        let mut command: Option<Command> = None;
        let mut pid_files = Vec::new();
        let guard = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        for (i, shell) in shells.iter().enumerate() {
            let exe = dir.path().join(shell);
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            std::fs::copy("/bin/sh", &exe).unwrap();
            let pid_file = dir.path().join(format!("{i}.pid"));
            let args = [
                exe.as_os_str(),
                OsStr::new("-c"),
                OsStr::new(LAYER),
                pid_file.as_os_str(),
            ];
            match &mut command {
                None => {
                    let mut first = Command::new(&exe);
                    first.args(&args[1..]);
                    command = Some(first);
                }
                Some(command) => {
                    command.args(args);
                }
            }
            pid_files.push(pid_file);
        }
        let mut command = command.expect("at least one shell");
        // Kept out of the test output, where a shell reports its killed child,
        // and shown if the tree fails to connect.
        let errors = dir.path().join("stderr");
        command
            .arg(std::env::current_exe().unwrap())
            .args(["connector", "--exact", "--nocapture", "--test-threads=1"])
            .env(CONNECT, &socket)
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&errors).unwrap());
        let launcher = command.spawn().unwrap();
        drop(guard);
        let mut tree = Self {
            _shells: dir,
            _sockets: sockets,
            listener,
            errors,
            launcher,
            pid_files,
            streams: Vec::new(),
        };
        tree.accept().await;
        tree
    }

    /// Waits for the next connection and returns the test's end of it.
    async fn accept(&mut self) -> &UnixStream {
        let Ok(accepted) = timeout(PATIENCE, self.listener.accept()).await else {
            let errors = std::fs::read_to_string(&self.errors).unwrap_or_default();
            panic!("the connector did not connect in time: {errors}");
        };
        let (stream, _) = accepted.unwrap();
        self.streams.push(stream);
        self.streams.last().unwrap()
    }

    /// The pid of the shell at `index` in the chain, the launcher first.
    fn shell(&self, index: usize) -> u32 {
        let text = std::fs::read_to_string(&self.pid_files[index]).unwrap();
        text.trim().parse().unwrap()
    }

    /// The pid of the connector.
    fn connector(&self) -> u32 {
        let cred = rustix::net::sockopt::socket_peercred(self.stream()).unwrap();
        cred.pid.as_raw_nonzero().get() as u32
    }

    fn stream(&self) -> &UnixStream {
        &self.streams[0]
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        // Closing the connections ends the connectors, then each shell in turn.
        self.streams.clear();
        let _ = self.launcher.wait();
    }
}

fn kind(name: &str, exe: &str) -> ClientKind {
    ClientKind {
        name: ClientKindName::new(name).unwrap(),
        description: String::new(),
        exe: vec![exe.into()],
    }
}

/// Patterns that only the shells of these tests match.
fn kinds() -> Vec<ClientKind> {
    vec![
        kind("near", "*/nowcast-near/*"),
        kind("far", "*/nowcast-far/*"),
    ]
}

fn named(name: &str) -> Option<ClientKindName> {
    Some(ClientKindName::new(name).unwrap())
}

#[tokio::test]
async fn a_matching_process_between_launcher_and_child_anchors() {
    let tree = Tree::spawn(&["launcher/sh", "agent/nowcast-near/1.0/sh"]).await;
    let identity = PeerIdentity::fixed(kinds());
    let anchor = identity.identify(tree.stream().as_fd()).unwrap();
    assert_eq!(anchor.kind, named("near"));
    assert_eq!(identity.pid(anchor.id), Some(tree.shell(1)));
    assert_ne!(tree.shell(1), tree.connector());
}

#[tokio::test]
async fn of_two_matching_ancestors_the_nearest_wins() {
    let tree = Tree::spawn(&[
        "outer/nowcast-near/1.0/sh",
        "inner/nowcast-near/2.0/sh",
        "plain/sh",
    ])
    .await;
    let identity = PeerIdentity::fixed(kinds());
    let anchor = identity.identify(tree.stream().as_fd()).unwrap();
    assert_eq!(anchor.kind, named("near"));
    assert_eq!(identity.pid(anchor.id), Some(tree.shell(1)));
}

#[tokio::test]
async fn the_nearest_wins_whatever_its_kind() {
    let tree = Tree::spawn(&["outer/nowcast-near/1.0/sh", "inner/nowcast-far/1.0/sh"]).await;
    let identity = PeerIdentity::fixed(kinds());
    let anchor = identity.identify(tree.stream().as_fd()).unwrap();
    assert_eq!(anchor.kind, named("far"));
    assert_eq!(identity.pid(anchor.id), Some(tree.shell(1)));
}

#[tokio::test]
async fn with_no_match_the_connected_process_anchors_with_no_kind() {
    let tree = Tree::spawn(&["launcher/sh", "middle/sh"]).await;
    let identity = PeerIdentity::fixed(kinds());
    let anchor = identity.identify(tree.stream().as_fd()).unwrap();
    assert_eq!(anchor.kind, None);
    assert_eq!(identity.pid(anchor.id), Some(tree.connector()));
}

#[tokio::test]
async fn killing_the_anchor_signals_its_death() {
    let tree = Tree::spawn(&["launcher/sh", "agent/nowcast-near/1.0/sh"]).await;
    let identity = PeerIdentity::fixed(kinds());
    let anchor = identity.identify(tree.stream().as_fd()).unwrap();
    let pid = tree.shell(1);
    assert_eq!(identity.pid(anchor.id), Some(pid));

    // One waiter per session or connection: several on one anchor is normal.
    let mut exited = Box::pin(identity.exited(anchor.id));
    let mut also = Box::pin(identity.exited(anchor.id));
    assert!(
        timeout(GRACE, &mut exited).await.is_err(),
        "the anchor is still alive"
    );
    assert!(
        timeout(GRACE, &mut also).await.is_err(),
        "the anchor is still alive"
    );

    let pid = Pid::from_raw(pid as i32).unwrap();
    kill_process(pid, Signal::KILL).unwrap();
    timeout(PATIENCE, exited)
        .await
        .expect("the death of the anchor is signalled");
    timeout(PATIENCE, also)
        .await
        .expect("every waiter learns of the death");
    assert_eq!(identity.pid(anchor.id), None);
    timeout(PATIENCE, identity.exited(anchor.id))
        .await
        .expect("waiting on a dead anchor resolves at once");
}

#[tokio::test]
async fn connections_of_one_anchor_share_its_id() {
    let mut tree = Tree::spawn_with(
        &["launcher/sh", "agent/nowcast-near/1.0/sh"],
        &[(TWICE, "1")],
    )
    .await;
    let second = tree.accept().await;
    let second_pid = rustix::net::sockopt::socket_peercred(second).unwrap().pid;
    let identity = PeerIdentity::fixed(kinds());
    let second = identity.identify(second.as_fd()).unwrap();
    let first = identity.identify(tree.stream().as_fd()).unwrap();
    assert_ne!(
        second_pid.as_raw_nonzero().get() as u32,
        tree.connector(),
        "two connected processes"
    );
    assert_eq!(first, second);
    assert_eq!(first.kind, named("near"));
    assert_eq!(identity.pid(first.id), Some(tree.shell(1)));

    let other = Tree::spawn(&["launcher/sh", "agent/nowcast-near/1.0/sh"]).await;
    let another = identity.identify(other.stream().as_fd()).unwrap();
    assert_ne!(first.id, another.id);
}

#[tokio::test]
async fn this_process_connecting_twice_gets_one_anchor() {
    let dir = TempDir::new().unwrap();
    let socket = dir.path().join("socket");
    let listener = UnixListener::bind(&socket).unwrap();
    let identity = PeerIdentity::fixed(kinds());
    let mut anchors = Vec::new();
    for _ in 0..2 {
        let _client = UnixStream::connect(&socket).await.unwrap();
        let (accepted, _) = listener.accept().await.unwrap();
        anchors.push(identity.identify(accepted.as_fd()).unwrap());
    }
    assert_eq!(anchors[0], anchors[1]);
    assert_eq!(anchors[0].kind, None);
    assert_eq!(identity.pid(anchors[0].id), Some(std::process::id()));
}

#[tokio::test]
async fn a_kind_changed_between_two_connections_applies_to_the_second() {
    let dir = TempDir::new().unwrap();
    let socket = dir.path().join("socket");
    let listener = UnixListener::bind(&socket).unwrap();
    // Stands for discovery, whose kinds change when a file of the user's
    // client kinds directory does.
    let current = Arc::new(Mutex::new(kinds()));
    let source = Arc::clone(&current);
    let identity = PeerIdentity::new(move || source.lock().unwrap().clone());
    let connect = async || {
        let _client = UnixStream::connect(&socket).await.unwrap();
        let (accepted, _) = listener.accept().await.unwrap();
        identity.identify(accepted.as_fd()).unwrap()
    };

    assert_eq!(connect().await.kind, None);
    // A pattern that matches this test binary, the connected process.
    current
        .lock()
        .unwrap()
        .push(kind("self", "*/process_tree-*"));
    let anchor = connect().await;
    assert_eq!(anchor.kind, named("self"));
    assert_eq!(identity.pid(anchor.id), Some(std::process::id()));
}

#[tokio::test]
async fn an_unknown_anchor_has_no_pid_and_counts_as_exited() {
    let identity = PeerIdentity::fixed(kinds());
    let unknown = nowcast_traits::AnchorId(42);
    assert_eq!(identity.pid(unknown), None);
    timeout(PATIENCE, identity.exited(unknown))
        .await
        .expect("nothing to wait for");
}

#[tokio::test]
async fn a_connected_process_already_reaped_is_reported_as_not_found() {
    let dir = TempDir::new().unwrap();
    let socket = dir.path().join("socket");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut connector = {
        let _guard = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        Command::new(std::env::current_exe().unwrap())
            .args(["connector", "--exact", "--test-threads=1"])
            .env(CONNECT, &socket)
            .env(LEAVE, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let (accepted, _) = timeout(PATIENCE, listener.accept())
        .await
        .expect("the connector connects")
        .unwrap();
    assert!(connector.wait().unwrap().success());
    let identity = PeerIdentity::fixed(kinds());
    let error = identity.identify(accepted.as_fd()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{error}");
}
