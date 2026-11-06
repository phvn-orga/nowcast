//! The registry on real processes, with the service's identity and router:
//! killing an anchor process with `SIGKILL` removes its subscriptions and
//! releases the flows this leaves without a subscriber.
//!
//! The connecting process is this test binary run again (see [`connector`]).
//! It is either its own anchor, or the child of a copy of `/bin/sh` placed
//! where a test client kind matches it.

use std::io::Read;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nowcast_identity::PeerIdentity;
use nowcast_router::FlowRouter;
use nowcast_sessions::SessionRegistry;
use nowcast_traits::{ClientKind, Identity, Router, Sessions};
use nowcast_types::{BrickName, ClientKindName, FlowId, QualifiedFlowId, SessionId};
use tempfile::TempDir;
use tokio::net::{UnixListener, UnixStream};
use tokio::time::timeout;

/// Set on the connecting process: the socket to connect to.
const CONNECT: &str = "NOWCAST_SESSIONS_TEST_CONNECT";

/// Long enough for a loaded CI runner to start a process.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long a future that must not resolve is given to do so anyway.
const GRACE: Duration = Duration::from_millis(300);

/// Not a test of its own: the connecting process, this binary run again with
/// [`CONNECT`] set. It stays connected until the test closes its end.
#[test]
fn connector() {
    let Some(socket) = std::env::var_os(CONNECT) else {
        return;
    };
    let mut stream = StdUnixStream::connect(socket).expect("connect to the test");
    let _ = stream.read(&mut [0]);
}

/// Serializes copying the shell and spawning processes: a process forked
/// while another thread has the copy open for writing would keep it open,
/// and executing the copy would fail with "text file busy".
static SPAWN: Mutex<()> = Mutex::new(());

/// A connected client, whose anchor is `anchor`.
struct Client {
    /// The anchor process: the connector itself, or the shell above it.
    anchor: Child,
    /// The test's end of the connector's connection.
    connection: UnixStream,
    /// The shell, under the build directory rather than the system temporary
    /// one, which may be mounted without the right to execute.
    _dir: TempDir,
}

impl Client {
    /// Starts a connector, under a shell the test kind matches when
    /// `under_a_kind`, and returns once it has connected.
    async fn start(listener: &UnixListener, socket: &Path, under_a_kind: bool) -> Self {
        let dir = TempDir::new_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let connector = std::env::current_exe().unwrap();
        let guard = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        let mut command = if under_a_kind {
            let shell = dir.path().join("nowcast-agent/sh");
            std::fs::create_dir_all(shell.parent().unwrap()).unwrap();
            std::fs::copy("/bin/sh", &shell).unwrap();
            let mut command = Command::new(&shell);
            // `; true` keeps the shell alive as the parent instead of
            // replacing itself with the connector.
            command.args(["-c", r#""$@"; true"#, "sh"]).arg(connector);
            command
        } else {
            Command::new(connector)
        };
        let anchor = command
            .args(["connector", "--exact", "--test-threads=1"])
            .env(CONNECT, socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        drop(guard);
        let (connection, _) = timeout(PATIENCE, listener.accept())
            .await
            .expect("the connector connects")
            .unwrap();
        Self {
            anchor,
            connection,
            _dir: dir,
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // The connector, if it outlived its shell, ends when the connection
        // closes, right after this.
        let _ = self.anchor.kill();
        let _ = self.anchor.wait();
    }
}

fn flow(id: &str) -> QualifiedFlowId {
    QualifiedFlowId::new(BrickName::new("probe").unwrap(), FlowId::new(id).unwrap())
}

/// Declares a session for a client's anchor, subscribes it to its own flow
/// and to one shared with this test process, an anchor that stays alive,
/// then kills the anchor.
async fn killing_the_anchor_releases_its_flows(under_a_kind: bool) {
    let sockets = TempDir::new().unwrap();
    let socket = sockets.path().join("socket");
    let listener = UnixListener::bind(&socket).unwrap();
    let identity = Arc::new(PeerIdentity::fixed(vec![ClientKind {
        name: ClientKindName::new("agent").unwrap(),
        description: String::new(),
        exe: vec!["*/nowcast-agent/*".into()],
    }]));
    let router = Arc::new(FlowRouter::new());
    let sessions = SessionRegistry::new(Arc::clone(&identity), Arc::clone(&router));
    let mut empty = router.empty_flows();

    let mut client = Client::start(&listener, &socket, under_a_kind).await;
    let anchor = identity.identify(client.connection.as_fd()).unwrap();
    assert_eq!(anchor.kind.is_some(), under_a_kind);
    assert_eq!(identity.pid(anchor.id), Some(client.anchor.id()));

    let _own = UnixStream::connect(&socket).await.unwrap();
    let (accepted, _) = listener.accept().await.unwrap();
    let survivor = identity.identify(accepted.as_fd()).unwrap();
    assert_eq!(identity.pid(survivor.id), Some(std::process::id()));

    sessions.declare(anchor.id, SessionId::new("one"));
    sessions.declare(survivor.id, SessionId::new("other"));
    router.subscribe(anchor.id, flow("own"));
    router.subscribe(anchor.id, flow("shared"));
    router.subscribe(survivor.id, flow("shared"));
    let mut stream = router.stream(anchor.id, None);
    sessions.declare(anchor.id, SessionId::new("two"));
    assert!(
        timeout(GRACE, empty.recv()).await.is_err(),
        "nothing is released while the anchor lives"
    );
    assert_eq!(router.subscribers(&flow("own")), 1);

    client.anchor.kill().unwrap();
    let released = timeout(PATIENCE, empty.recv())
        .await
        .expect("the anchor's death releases its flow")
        .unwrap();
    assert_eq!(released, flow("own"));
    assert_eq!(sessions.current(anchor.id), None);
    assert_eq!(router.subscribers(&flow("own")), 0);
    assert_eq!(router.subscribers(&flow("shared")), 1);
    assert_eq!(stream.recv().await, None, "the anchor's stream closes");
    assert!(
        empty.try_recv().is_err(),
        "the shared flow keeps the survivor"
    );
    assert_eq!(sessions.current(survivor.id), Some(SessionId::new("other")));
}

#[tokio::test]
async fn killing_a_process_anchored_on_itself_releases_its_flows() {
    killing_the_anchor_releases_its_flows(false).await;
}

/// The connector outlives its shell and stays connected until the test ends:
/// the anchor's death counts, not the end of a connection.
#[tokio::test]
async fn killing_a_client_kind_anchor_releases_its_flows() {
    killing_the_anchor_releases_its_flows(true).await;
}
