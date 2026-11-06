//! Runs the daemon binary: under socket activation, and started by hand.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::thread::sleep;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};
use tempfile::TempDir;

const DAEMON: &str = env!("CARGO_BIN_EXE_nowcast-daemon");
const SOCKET_ACTIVATE: &str = "/usr/bin/systemd-socket-activate";
const TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);

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
    fn command(&self, program: &str) -> Command {
        fs::create_dir_all(self.runtime()).unwrap();
        let mut command = Command::new(program);
        command
            .env("XDG_RUNTIME_DIR", self.runtime())
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("HOME", self.root.path())
            .env("NOWCAST_LOG", "debug");
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

/// Declares a session over `socket` and returns the HTTP status code.
fn declare_session(socket: &Path) -> io::Result<u16> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    let body = r#"{"id":"test"}"#;
    write!(
        stream,
        "PUT /v1/session HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let status = response
        .strip_prefix("HTTP/1.1 ")
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse().ok());
    status.ok_or_else(|| io::Error::other(format!("not an HTTP response: {response:?}")))
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
