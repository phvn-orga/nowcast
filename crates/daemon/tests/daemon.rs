//! Runs the daemon binary: under socket activation, started by hand, and with
//! the real test brick behind it.

use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, IoSlice, IoSliceMut, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::{Duration, Instant};

use nowcast_test_brick::{CONTROL_VAR, Control, TYPE};
use nowcast_types::FlowId;
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
};
use rustix::process::{Pid, Signal, kill_process};
use tempfile::TempDir;
use tokio::time::timeout;

const DAEMON: &str = env!("CARGO_BIN_EXE_nowcast-daemon");
const SOCKET_ACTIVATE: &str = "/usr/bin/systemd-socket-activate";
const TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);

/// Set on the connector (see [`connector`]): the daemon's socket.
const CONNECT: &str = "NOWCAST_DAEMON_TEST_CONNECT";

/// A runtime and a configuration directory of its own for one daemon.
struct Dirs {
    root: TempDir,
}

impl Dirs {
    fn new() -> Self {
        Self {
            root: TempDir::new().unwrap(),
        }
    }

    fn runtime(&self) -> PathBuf {
        self.root.path().join("run")
    }

    fn socket_dir(&self) -> PathBuf {
        self.runtime().join("nowcast")
    }

    fn socket(&self) -> PathBuf {
        self.socket_dir().join("nowcast.sock")
    }

    /// `program` with the environment the daemon reads.
    fn config(&self) -> PathBuf {
        self.root.path().join("config/nowcast")
    }

    /// Activates the test brick, built if needed: its declaration names the
    /// bare program, which [`Dirs::command`] puts on the `PATH`.
    fn link_probe_brick(&self) {
        build_test_brick();
        let bricks = self.config().join("bricks");
        fs::create_dir_all(&bricks).unwrap();
        let manifest = nowcast_test_brick::manifest();
        std::os::unix::fs::symlink(manifest, bricks.join("probe.toml")).unwrap();
    }

    /// Declares a client kind for this test binary, so that the connector
    /// anchors on itself even when an ancestor matches another kind.
    fn declare_connector_kind(&self) {
        let clients = self.config().join("clients");
        fs::create_dir_all(&clients).unwrap();
        let exe = std::env::current_exe().unwrap();
        let kind = format!(
            "name = \"connector\"\ndescription = \"The test connector\"\n\n[anchor]\nexe = [{:?}]\n",
            exe.display().to_string()
        );
        fs::write(clients.join("connector.toml"), kind).unwrap();
    }

    fn command(&self, program: &str) -> Command {
        fs::create_dir_all(self.runtime()).unwrap();
        // As private as a real runtime directory, whatever the umask.
        fs::set_permissions(self.runtime(), fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = Command::new(program);
        command
            .env("XDG_RUNTIME_DIR", self.runtime())
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("HOME", self.root.path())
            .env("NOWCAST_LOG", "debug")
            .env("PATH", path_with_test_brick());
        for var in [
            "LISTEN_PID",
            "LISTEN_FDS",
            "LISTEN_FDNAMES",
            "LISTEN_PIDFDID",
        ] {
            command.env_remove(var);
        }
        command
    }
}

/// Builds the test brick once per test run. Cargo builds it for its own
/// package's tests only, under `cargo test --workspace`.
fn build_test_brick() {
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let mut cargo = Command::new(env!("CARGO"));
        cargo.args([
            "build",
            "--quiet",
            "-p",
            "nowcast-test-brick",
            "--bin",
            "nowcast-test-brick",
        ]);
        if !cfg!(debug_assertions) {
            cargo.arg("--release");
        }
        assert!(cargo.status().unwrap().success(), "the test brick builds");
    });
}

/// The `PATH` with the test brick's directory first.
fn path_with_test_brick() -> OsString {
    let brick = nowcast_test_brick::binary().expect("the profile directory of the tests");
    let dir = brick.parent().unwrap().to_path_buf();
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::iter::once(dir).chain(std::env::split_paths(&path))).unwrap()
}

/// A process killed when the test ends, whatever its outcome.
struct Running(Child);

impl Running {
    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn terminate(&self) {
        let pid = Pid::from_raw(self.pid() as i32).unwrap();
        kill_process(pid, Signal::TERM).unwrap();
    }

    fn kill(&mut self) {
        self.0.kill().unwrap();
        self.0.wait().unwrap();
    }

    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "the process did not exit");
            sleep(POLL);
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        sleep(POLL);
    }
}

/// Sends one request on `stream`, which stays open for the next one, and
/// returns the status and body of the answer.
fn request(stream: &UnixStream, method: &str, path: &str, body: &str) -> io::Result<(u16, String)> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    write!(
        &*stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    )?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let status = line
        .strip_prefix("HTTP/1.1 ")
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| io::Error::other(format!("not an HTTP response: {line:?}")))?;
    let mut length = 0;
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().map_err(io::Error::other)?;
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

/// Declares a session on a new connection and returns the HTTP status code.
fn declare_session(socket: &Path) -> io::Result<u16> {
    let stream = UnixStream::connect(socket)?;
    Ok(request(&stream, "PUT", "/v1/session", r#"{"id":"test"}"#)?.0)
}

fn exe(pid: u32) -> PathBuf {
    fs::read_link(format!("/proc/{pid}/exe")).unwrap()
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_first_connection_starts_the_daemon() {
    let dirs = Dirs::new();
    fs::create_dir_all(dirs.socket_dir()).unwrap();
    // systemd-socket-activate passes only the variables named with -E.
    let mut command = dirs.command(SOCKET_ACTIVATE);
    command.arg("--listen").arg(dirs.socket());
    for var in ["XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "HOME", "NOWCAST_LOG"] {
        command.args(["-E", var]);
    }
    let mut activator = Running(command.arg(DAEMON).spawn().unwrap());
    wait_until("the socket is listening", || dirs.socket().exists());

    // The activator replaces itself with the daemon on the first connection.
    let daemon = fs::canonicalize(DAEMON).unwrap();
    assert_ne!(
        exe(activator.pid()),
        daemon,
        "started before any connection"
    );
    assert_eq!(declare_session(&dirs.socket()).unwrap(), 204);
    assert_eq!(exe(activator.pid()), daemon);

    activator.terminate();
    assert!(activator.wait().success());
    // The socket belongs to whoever passed it, not to the daemon.
    assert!(dirs.socket().exists());
}

#[test]
fn a_daemon_started_by_hand_binds_a_private_socket() {
    let dirs = Dirs::new();
    let _daemon = Running(dirs.command(DAEMON).spawn().unwrap());
    wait_until("the socket is bound", || dirs.socket().exists());

    assert!(fs::metadata(dirs.socket()).unwrap().file_type().is_socket());
    assert_eq!(mode(&dirs.socket()), 0o600);
    assert_eq!(mode(&dirs.socket_dir()), 0o700);
    assert_eq!(declare_session(&dirs.socket()).unwrap(), 204);
}

#[test]
fn an_existing_directory_is_made_private() {
    let dirs = Dirs::new();
    fs::create_dir_all(dirs.socket_dir()).unwrap();
    fs::set_permissions(dirs.socket_dir(), fs::Permissions::from_mode(0o755)).unwrap();
    let _daemon = Running(dirs.command(DAEMON).spawn().unwrap());
    wait_until("the socket is bound", || dirs.socket().exists());

    assert_eq!(mode(&dirs.socket_dir()), 0o700);
    assert_eq!(mode(&dirs.socket()), 0o600);
}

#[test]
fn a_linked_socket_directory_is_refused() {
    let dirs = Dirs::new();
    let elsewhere = dirs.root.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let mut command = dirs.command(DAEMON);
    std::os::unix::fs::symlink(&elsewhere, dirs.socket_dir()).unwrap();
    let mut daemon = Running(command.spawn().unwrap());

    assert!(!daemon.wait().success());
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
}

#[test]
fn a_socket_directory_that_is_a_file_is_refused() {
    let dirs = Dirs::new();
    let mut command = dirs.command(DAEMON);
    fs::write(dirs.socket_dir(), "").unwrap();
    let mut daemon = Running(command.spawn().unwrap());

    assert!(!daemon.wait().success());
}

#[test]
fn a_runtime_directory_others_can_write_is_refused() {
    let dirs = Dirs::new();
    let mut command = dirs.command(DAEMON);
    fs::set_permissions(dirs.runtime(), fs::Permissions::from_mode(0o777)).unwrap();
    let mut daemon = Running(command.spawn().unwrap());

    assert!(!daemon.wait().success());
    assert!(!dirs.socket_dir().exists());
}

#[test]
fn a_stale_socket_is_replaced() {
    let dirs = Dirs::new();
    fs::create_dir_all(dirs.socket_dir()).unwrap();
    // A listener dropped without removing its file, as after a crash.
    drop(UnixListener::bind(dirs.socket()).unwrap());
    let _daemon = Running(dirs.command(DAEMON).spawn().unwrap());

    wait_until("the daemon answers", || {
        declare_session(&dirs.socket()).is_ok_and(|status| status == 204)
    });
}

#[test]
fn a_file_at_the_socket_path_is_not_removed() {
    let dirs = Dirs::new();
    let mut command = dirs.command(DAEMON);
    fs::create_dir(dirs.socket_dir()).unwrap();
    fs::write(dirs.socket(), "not a socket").unwrap();
    let mut daemon = Running(command.spawn().unwrap());

    assert!(!daemon.wait().success());
    assert_eq!(fs::read_to_string(dirs.socket()).unwrap(), "not a socket");
}

#[test]
fn a_socket_in_use_is_not_taken_over() {
    let dirs = Dirs::new();
    fs::create_dir_all(dirs.socket_dir()).unwrap();
    let other = UnixListener::bind(dirs.socket()).unwrap();
    let mut daemon = Running(dirs.command(DAEMON).spawn().unwrap());

    assert!(!daemon.wait().success());
    other.set_nonblocking(true).unwrap();
    UnixStream::connect(dirs.socket()).unwrap();
    assert!(other.accept().is_ok(), "the socket changed hands");
}

#[test]
fn sigterm_stops_the_daemon_and_removes_its_socket() {
    let dirs = Dirs::new();
    let mut daemon = Running(dirs.command(DAEMON).spawn().unwrap());
    wait_until("the socket is bound", || dirs.socket().exists());
    // An open connection does not hold the daemon back.
    let _connection = UnixStream::connect(dirs.socket()).unwrap();

    daemon.terminate();
    assert!(daemon.wait().success());
    assert!(!dirs.socket().exists());
}

#[test]
fn a_failed_start_removes_the_socket() {
    let dirs = Dirs::new();
    // The discovery cannot create its directories under a file.
    let config = dirs.root.path().join("config-file");
    fs::write(&config, "").unwrap();
    let mut command = dirs.command(DAEMON);
    let mut daemon = Running(command.env("XDG_CONFIG_HOME", &config).spawn().unwrap());

    assert!(!daemon.wait().success());
    assert!(
        dirs.socket_dir().exists(),
        "the daemon got as far as the bind"
    );
    assert!(!dirs.socket().exists());
}

#[test]
fn a_socket_replaced_meanwhile_is_left_on_exit() {
    let dirs = Dirs::new();
    let mut daemon = Running(dirs.command(DAEMON).spawn().unwrap());
    wait_until("the socket is bound", || dirs.socket().exists());
    // What `nowcast.socket` does when it starts: unlink, bind again.
    fs::remove_file(dirs.socket()).unwrap();
    let systemd = UnixListener::bind(dirs.socket()).unwrap();

    daemon.terminate();
    assert!(daemon.wait().success());
    systemd.set_nonblocking(true).unwrap();
    UnixStream::connect(dirs.socket()).unwrap();
    assert!(systemd.accept().is_ok(), "the replacing socket is gone");
}

#[test]
fn the_socket_unit_matches_the_daemon() {
    let unit = include_str!("../systemd/nowcast.socket");
    for line in [
        "ListenStream=%t/nowcast/nowcast.sock",
        "SocketMode=0600",
        "DirectoryMode=0700",
    ] {
        assert!(unit.lines().any(|l| l == line), "missing {line}");
    }
}

#[test]
fn the_service_unit_lets_the_daemon_stop_its_bricks() {
    let unit = include_str!("../systemd/nowcast.service");
    assert!(unit.lines().any(|l| l == "KillMode=mixed"));
}

/// Not a test of its own: the connector, this binary run again with
/// [`CONNECT`] set. It connects to the daemon, which anchors the connection
/// on it, hands the connection to the test over its stdin, closes its own
/// copy, then waits to be killed.
#[test]
fn connector() {
    let Some(socket) = std::env::var_os(CONNECT) else {
        return;
    };
    let daemon = UnixStream::connect(socket).unwrap();
    let fds = [daemon.as_fd()];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    assert!(control.push(SendAncillaryMessage::ScmRights(&fds)));
    sendmsg(
        io::stdin(),
        &[IoSlice::new(b"x")],
        &mut control,
        SendFlags::empty(),
    )
    .unwrap();
    // The test holds the connection now: closing it is up to the test.
    drop(daemon);
    loop {
        std::thread::park();
    }
}

/// Starts a connector and returns it with the connection it made, which the
/// daemon anchors on the connector.
fn connection_anchored_on_a_connector(dirs: &Dirs) -> (Running, UnixStream) {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let connector = Command::new(std::env::current_exe().unwrap())
        .args(["connector", "--exact"])
        .env(CONNECT, dirs.socket())
        .stdin(OwnedFd::from(theirs))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let connector = Running(connector);
    ours.set_read_timeout(Some(TIMEOUT)).unwrap();
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    recvmsg(
        &ours,
        &mut [IoSliceMut::new(&mut [0])],
        &mut control,
        RecvFlags::CMSG_CLOEXEC,
    )
    .unwrap();
    let connection = control
        .drain()
        .find_map(|message| match message {
            RecvAncillaryMessage::ScmRights(mut fds) => fds.next(),
            _ => None,
        })
        .expect("the connector sends its connection");
    (connector, UnixStream::from(connection))
}

/// Every byte but ASCII letters and digits percent-encoded, for a path
/// segment.
fn encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b if b.is_ascii_alphanumeric() => char::from(b).to_string(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

#[test]
fn the_session_of_an_anchor_ends_with_its_process() {
    let dirs = Dirs::new();
    dirs.link_probe_brick();
    dirs.declare_connector_kind();
    let _daemon = Running(dirs.command(DAEMON).spawn().unwrap());
    wait_until("the socket is bound", || dirs.socket().exists());
    let (mut anchor, connection) = connection_anchored_on_a_connector(&dirs);

    let (status, _) = request(&connection, "PUT", "/v1/session", r#"{"id":"agent"}"#).unwrap();
    assert_eq!(status, 204);
    let subscribe = r#"{"brick":"probe","kind":"item","params":{"id":"a"}}"#;
    let (status, body) = request(&connection, "POST", "/v1/subscriptions", subscribe).unwrap();
    assert_eq!(status, 201, "{body}");
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    let flow = format!("/v1/flows/{}", encode(body["flow"].as_str().unwrap()));
    // Subscribed, nothing published yet.
    assert_eq!(request(&connection, "GET", &flow, "").unwrap().0, 204);

    anchor.kill();
    // The connection outlives its anchor; the registry forgets the session.
    wait_until("the session ends", || {
        request(&connection, "GET", &flow, "").unwrap().0 == 428
    });
}

#[tokio::test]
async fn a_value_from_the_test_brick_reaches_the_stream() {
    let dirs = Dirs::new();
    dirs.link_probe_brick();
    let control = Control::bind(dirs.root.path().join("control")).unwrap();
    let mut command = dirs.command(DAEMON);
    let mut daemon = Running(command.env(CONTROL_VAR, control.path()).spawn().unwrap());
    wait_until("the socket is bound", || dirs.socket().exists());

    let client = UnixStream::connect(dirs.socket()).unwrap();
    let (status, _) = request(&client, "PUT", "/v1/session", r#"{"id":"agent"}"#).unwrap();
    assert_eq!(status, 204);
    let subscribe = r#"{"brick":"probe","kind":"item","params":{"id":"a"}}"#;
    let (status, body) = request(&client, "POST", "/v1/subscriptions", subscribe).unwrap();
    assert_eq!(status, 201, "{body}");
    let mut brick = timeout(TIMEOUT, control.accept()).await.unwrap().unwrap();

    // The stream is open, its headers read, before the brick publishes.
    let mut stream = UnixStream::connect(dirs.socket()).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    write!(stream, "GET /v1/stream HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    let mut received = Vec::new();
    read_until(&mut stream, &mut received, "\r\n\r\n");
    assert!(received.starts_with(b"HTTP/1.1 200"));

    let detail = br#"{"id":"a","note":"from the test brick"}"#;
    let flow = FlowId::new("a").unwrap();
    let published = brick.publish(&flow, TYPE, detail);
    timeout(TIMEOUT, published).await.unwrap().unwrap().unwrap();
    read_until(&mut stream, &mut received, "from the test brick");

    daemon.terminate();
    assert!(daemon.wait().success());
    // The brick does not outlive the daemon.
    timeout(TIMEOUT, brick.exited()).await.unwrap().unwrap();
}

/// Reads `stream` into `received` until it holds `text`.
fn read_until(stream: &mut UnixStream, received: &mut Vec<u8>, text: &str) {
    let mut buffer = [0; 4096];
    while !String::from_utf8_lossy(received).contains(text) {
        let n = stream.read(&mut buffer).unwrap();
        assert!(n > 0, "the stream ended before {text:?}");
        received.extend_from_slice(&buffer[..n]);
    }
}

// Exiting when unused, and stopping the bricks on the way out.

/// The idle delay of the tests, in seconds, for `NOWCAST_IDLE_TIMEOUT`.
const IDLE: &str = "0.5";
const IDLE_DELAY: Duration = Duration::from_millis(500);

/// What a daemon writes on stderr, read as it comes.
struct Logs(std::sync::Arc<std::sync::Mutex<String>>);

impl Logs {
    /// Starts `command` with its stderr read into the returned logs.
    fn spawn(command: &mut Command) -> (Running, Logs) {
        let mut child = command.stderr(Stdio::piped()).spawn().unwrap();
        let stderr = child.stderr.take().unwrap();
        let logs = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let writer = std::sync::Arc::clone(&logs);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut logs = writer.lock().unwrap();
                logs.push_str(&line);
                logs.push('\n');
            }
        });
        (Running(child), Logs(logs))
    }

    fn wait_for(&self, text: &str) {
        wait_until(&format!("{text:?} in the logs"), || {
            self.0.lock().unwrap().contains(text)
        });
    }
}

/// `program` started under `systemd-socket-activate` on the daemon's socket,
/// with the variables the daemon reads, and `NOWCAST_IDLE_TIMEOUT` when given.
fn activator(dirs: &Dirs, idle: Option<&str>) -> Command {
    fs::create_dir_all(dirs.socket_dir()).unwrap();
    let mut command = dirs.command(SOCKET_ACTIVATE);
    command.arg("--listen").arg(dirs.socket());
    for var in ["XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "HOME", "NOWCAST_LOG"] {
        command.args(["-E", var]);
    }
    if let Some(idle) = idle {
        command
            .arg("-E")
            .arg(format!("NOWCAST_IDLE_TIMEOUT={idle}"));
    }
    command.arg(DAEMON);
    command
}

#[test]
fn an_activated_daemon_exits_once_unused() {
    let dirs = Dirs::new();
    let mut daemon = Running(activator(&dirs, Some(IDLE)).spawn().unwrap());
    wait_until("the socket is listening", || dirs.socket().exists());

    assert_eq!(declare_session(&dirs.socket()).unwrap(), 204);
    let unused = Instant::now();
    assert!(daemon.wait().success());
    assert!(unused.elapsed() >= IDLE_DELAY, "exited before the delay");
    // The socket is systemd's, left for the next start.
    assert!(dirs.socket().exists());
}

#[test]
fn an_activated_daemon_waits_a_minute_by_default() {
    let dirs = Dirs::new();
    let (mut daemon, logs) = Logs::spawn(&mut activator(&dirs, None));
    wait_until("the socket is listening", || dirs.socket().exists());
    assert_eq!(declare_session(&dirs.socket()).unwrap(), 204);
    logs.wait_for("exits after 60s with no connection and no subscription");
    daemon.terminate();
    assert!(daemon.wait().success());
}

#[test]
fn a_daemon_started_by_hand_runs_until_stopped_by_default() {
    let dirs = Dirs::new();
    let (mut daemon, logs) = Logs::spawn(&mut dirs.command(DAEMON));
    logs.wait_for("runs until stopped");
    daemon.terminate();
    assert!(daemon.wait().success());
}

#[test]
fn an_invalid_idle_delay_is_refused() {
    let dirs = Dirs::new();
    let mut command = dirs.command(DAEMON);
    let mut daemon = Running(command.env("NOWCAST_IDLE_TIMEOUT", "soon").spawn().unwrap());
    assert!(!daemon.wait().success());
    assert!(!dirs.socket_dir().exists(), "refused before the bind");
}

/// Plays systemd's part in socket activation: holds the listening socket for
/// good, and starts a daemon on it as descriptor 3, with `LISTEN_PID` and
/// `LISTEN_FDS` set, whenever the test says a connection is waiting.
struct Systemd {
    listener: UnixListener,
}

impl Systemd {
    fn listen(dirs: &Dirs) -> Self {
        fs::create_dir_all(dirs.socket_dir()).unwrap();
        Self {
            listener: UnixListener::bind(dirs.socket()).unwrap(),
        }
    }

    fn start(&self, dirs: &Dirs) -> Running {
        let socket = OwnedFd::from(self.listener.try_clone().unwrap());
        // The socket comes in as stdin; the shell moves it to 3, then gives
        // way to the daemon under its own pid.
        let script = r#"exec 3<&0 0</dev/null; export LISTEN_PID=$$ LISTEN_FDS=1; exec "$0""#;
        let child = dirs
            .command("sh")
            .args(["-c", script, DAEMON])
            .env("NOWCAST_IDLE_TIMEOUT", IDLE)
            .stdin(Stdio::from(socket))
            .spawn()
            .unwrap();
        Running(child)
    }
}

#[test]
fn a_connection_after_an_idle_exit_starts_the_daemon_again() {
    let dirs = Dirs::new();
    let systemd = Systemd::listen(&dirs);
    let mut first = systemd.start(&dirs);
    assert_eq!(declare_session(&dirs.socket()).unwrap(), 204);
    assert!(first.wait().success(), "the first daemon exits once unused");

    // Nobody listens on the socket but systemd: the connection and its
    // request wait in the socket's queue.
    let client = UnixStream::connect(dirs.socket()).unwrap();
    write!(
        &client,
        "PUT /v1/session HTTP/1.1\r\nHost: localhost\r\nContent-Length: 13\r\n\r\n{{\"id\":\"next\"}}"
    )
    .unwrap();
    let _second = systemd.start(&dirs);
    let mut reader = BufReader::new(&client);
    client.set_read_timeout(Some(TIMEOUT)).unwrap();
    let mut status = String::new();
    reader.read_line(&mut status).unwrap();
    assert!(status.starts_with("HTTP/1.1 204"), "{status:?}");
}

#[tokio::test]
async fn a_subscription_keeps_an_unused_daemon_until_its_anchor_dies() {
    let dirs = Dirs::new();
    dirs.link_probe_brick();
    dirs.declare_connector_kind();
    let control = Control::bind(dirs.root.path().join("control")).unwrap();
    let mut command = dirs.command(DAEMON);
    command
        .env(CONTROL_VAR, control.path())
        .env("NOWCAST_IDLE_TIMEOUT", IDLE);
    let mut daemon = Running(command.spawn().unwrap());
    wait_until("the socket is bound", || dirs.socket().exists());
    let (mut anchor, connection) = connection_anchored_on_a_connector(&dirs);

    let (status, _) = request(&connection, "PUT", "/v1/session", r#"{"id":"agent"}"#).unwrap();
    assert_eq!(status, 204);
    let subscribe = r#"{"brick":"probe","kind":"item","params":{"id":"a"}}"#;
    let (status, body) = request(&connection, "POST", "/v1/subscriptions", subscribe).unwrap();
    assert_eq!(status, 201, "{body}");
    let brick = timeout(TIMEOUT, control.accept()).await.unwrap().unwrap();

    // No connection is left: the subscription alone keeps the daemon.
    drop(connection);
    sleep(IDLE_DELAY * 3);
    assert!(daemon.0.try_wait().unwrap().is_none(), "the daemon exited");

    anchor.kill();
    let dead = Instant::now();
    assert!(daemon.wait().success());
    // Well within the 5 s before `SIGTERM`: the brick, still in its grace
    // period, stopped on `shutdown`.
    assert!(
        dead.elapsed() < Duration::from_secs(4),
        "{:?}",
        dead.elapsed()
    );
    timeout(TIMEOUT, brick.exited()).await.unwrap().unwrap();
    assert!(!dirs.socket().exists());
}

/// A brick written in `sh`, declared as the probe: it answers `hello` with
/// `ready` at once, records everything the service sends it in `input`, and
/// on `SIGTERM` writes to `log` whether `shutdown` had arrived before, then
/// exits.
fn link_recording_brick(dirs: &Dirs, input: &Path, log: &Path) {
    let bricks = dirs.config().join("bricks");
    fs::create_dir_all(&bricks).unwrap();
    let schemas = nowcast_test_brick::manifest()
        .parent()
        .unwrap()
        .join("schemas");
    std::os::unix::fs::symlink(schemas, bricks.join("schemas")).unwrap();
    // `ready` is a length of 12 then the map {"v": 1, "t": "ready"}. A
    // background job reads /dev/null: it gets the input through descriptor 3.
    let script = r#"
        trap 'if grep -qa shutdown "$1"; then echo after-shutdown; else echo before-shutdown; fi >> "$0"; exit 0' TERM
        printf '\014\000\000\000\202\241v\001\241t\245ready'
        exec 3<&0
        cat <&3 > "$1" &
        wait
    "#;
    let manifest = format!(
        r#"manifest_version = 1
name = "probe"
version = "0.1.0"
description = "Records what the service sends."
protocol = 1

[run]
command = ["sh", "-c", '''{script}''', {log:?}, {input:?}]

[subscriptions.item]
description = "One item, named by its id."
schema = "schemas/item.json"
publishes = "probe.item/v1"

[types."probe.item/v1"]
schema = "schemas/probe-item-v1.json"
"#,
        log = log.display().to_string(),
        input = input.display().to_string(),
    );
    fs::write(bricks.join("probe.toml"), manifest).unwrap();
}

#[test]
fn sigterm_sends_shutdown_to_a_running_brick_before_any_signal_and_waits_for_it() {
    let dirs = Dirs::new();
    let input = dirs.root.path().join("brick.in");
    let log = dirs.root.path().join("brick.log");
    link_recording_brick(&dirs, &input, &log);
    let mut daemon = Running(dirs.command(DAEMON).spawn().unwrap());
    wait_until("the socket is bound", || dirs.socket().exists());

    // The brick never answers: the request stays in flight on a ready brick.
    let client = UnixStream::connect(dirs.socket()).unwrap();
    assert_eq!(
        request(&client, "PUT", "/v1/session", r#"{"id":"s"}"#)
            .unwrap()
            .0,
        204
    );
    let subscribe = r#"{"brick":"probe","kind":"item","params":{"id":"a"}}"#;
    write!(
        &client,
        "POST /v1/subscriptions HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{subscribe}",
        subscribe.len()
    )
    .unwrap();
    wait_until("the brick is asked to subscribe", || {
        fs::read(&input).is_ok_and(|bytes| bytes.windows(9).any(|w| w == b"subscribe"))
    });

    daemon.terminate();
    let terminated = Instant::now();
    // A second signal while the bricks stop changes nothing.
    sleep(Duration::from_secs(1));
    daemon.terminate();
    assert!(daemon.wait().success());
    // The brick ignored `shutdown`: `SIGTERM` came 5 s later, after it.
    assert!(terminated.elapsed() >= Duration::from_secs(5));
    assert_eq!(fs::read_to_string(&log).unwrap(), "after-shutdown\n");
}
