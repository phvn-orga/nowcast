//! Runs the real chain for one test: the `nowcast-daemon` binary, the test
//! brick it launches, and the `nowcast` tool as its client.
//!
//! Each [`Service`] has a directory of its own for its socket, its bricks and
//! its client kinds, so tests run in parallel. The brick is the test brick,
//! declared as `probe` and driven over its control socket.
//!
//! Client kinds: every kind compiled into the service is switched off, so that
//! a tool started by the test anchors on itself even when the test runs under
//! a real agent, and one kind, `agent`, matches [`Agent`], the stand-in for
//! an agent whose hooks run the tool.

#![allow(dead_code)]

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use nowcast_test_brick::{CONTROL_VAR, Control, ControlledBrick, MODE_VAR, Mode};
use nowcast_types::ServiceMessage;
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader, Lines};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::time::{sleep, timeout};

/// The longest wait on anything that should happen at once.
pub const TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);

const AGENT: &str = env!("CARGO_BIN_EXE_nowcast-e2e-agent");
/// The session [`Agent`] declares, as an agent's hooks pass its session id.
pub const AGENT_SESSION: &str = "agent-session";

/// Builds the binaries of the other packages once per test run. Cargo builds
/// them for their own packages' tests only, so under `cargo test --workspace`
/// but not under `cargo test -p nowcast-e2e`.
fn build_binaries() {
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let mut cargo = std::process::Command::new(env!("CARGO"));
        cargo.args(["build", "--quiet", "--bins"]);
        for package in ["nowcast-daemon", "nowcast-cli", "nowcast-test-brick"] {
            cargo.args(["-p", package]);
        }
        if !cfg!(debug_assertions) {
            cargo.arg("--release");
        }
        assert!(cargo.status().unwrap().success(), "the binaries build");
    });
}

/// Where Cargo puts the binaries: the parent of the `deps` directory holding
/// the test executable.
fn profile_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().unwrap().parent().unwrap().to_path_buf()
}

pub fn daemon_binary() -> PathBuf {
    profile_dir().join("nowcast-daemon")
}

pub fn cli_binary() -> PathBuf {
    profile_dir().join("nowcast")
}

/// The `PATH` with the test brick's directory first: its declaration names
/// the bare program.
fn path_with_test_brick() -> std::ffi::OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::iter::once(profile_dir()).chain(std::env::split_paths(&path)))
        .unwrap()
}

/// Polls `check` until it gives a value, for [`TIMEOUT`] at most.
pub async fn wait_for<T>(what: &str, mut check: impl AsyncFnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(value) = check().await {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        sleep(POLL).await;
    }
}

/// One daemon with the test brick activated.
pub struct Service {
    root: TempDir,
    control: Control,
    daemon: Child,
}

impl Service {
    pub async fn start() -> Self {
        Self::start_with(Mode::Ready).await
    }

    /// Every brick process the daemon launches runs in `mode`.
    pub async fn start_with(mode: Mode) -> Self {
        build_binaries();
        let root = TempDir::new().unwrap();
        let config = root.path().join("config/nowcast");

        let bricks = config.join("bricks");
        fs::create_dir_all(&bricks).unwrap();
        std::os::unix::fs::symlink(nowcast_test_brick::manifest(), bricks.join("probe.toml"))
            .unwrap();

        let clients = config.join("clients");
        fs::create_dir_all(&clients).unwrap();
        for (file, _) in nowcast_discovery::EMBEDDED_CLIENTS {
            let name = file.strip_suffix(".toml").unwrap();
            fs::write(clients.join(file), kind(name, None)).unwrap();
        }
        let agent = fs::canonicalize(AGENT).unwrap();
        fs::write(clients.join("agent.toml"), kind("agent", Some(&agent))).unwrap();

        let runtime = root.path().join("run");
        fs::create_dir(&runtime).unwrap();
        // As private as a real runtime directory, whatever the umask.
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let control = Control::bind(root.path().join("control")).unwrap();
        let log = fs::File::create(root.path().join("daemon.log")).unwrap();
        let daemon = Command::new(daemon_binary())
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("HOME", root.path())
            .env("PATH", path_with_test_brick())
            .env("NOWCAST_LOG", "debug")
            .env(CONTROL_VAR, control.path())
            .env(MODE_VAR, mode.as_str())
            .env_remove("LISTEN_PID")
            .env_remove("LISTEN_FDS")
            .env_remove("LISTEN_FDNAMES")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut service = Self {
            root,
            control,
            daemon,
        };
        let socket = service.socket();
        wait_for("the daemon accepts connections", async || {
            assert!(
                service.daemon.try_wait().unwrap().is_none(),
                "the daemon exited"
            );
            UnixStream::connect(&socket).ok().map(drop)
        })
        .await;
        service
    }

    fn runtime(&self) -> PathBuf {
        self.root.path().join("run")
    }

    pub fn socket(&self) -> PathBuf {
        self.runtime().join("nowcast/nowcast.sock")
    }

    /// The `nowcast` tool, finding the socket through `XDG_RUNTIME_DIR` as
    /// from a shell, with no session given.
    pub fn cli(&self) -> Command {
        let mut command = Command::new(cli_binary());
        command
            .env("XDG_RUNTIME_DIR", self.runtime())
            .env_remove("NOWCAST_SOCKET")
            .env_remove("NOWCAST_SESSION")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    /// Runs `nowcast` with `args` to its end.
    pub async fn run(&self, args: &[&str]) -> Ran {
        Ran::from(run_to_end(self.cli().args(args)).await)
    }

    /// Runs `nowcast` with `args` through `sh -c`, as typed in a terminal.
    pub async fn run_from_shell(&self, args: &[&str]) -> Ran {
        let mut command = self.cli();
        let command = command.as_std_mut();
        let mut shell = Command::new("sh");
        shell
            .arg("-c")
            .arg("\"$0\" \"$@\"")
            .arg(command.get_program())
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        for (name, value) in command.get_envs() {
            match value {
                Some(value) => shell.env(name, value),
                None => shell.env_remove(name),
            };
        }
        Ran::from(run_to_end(&mut shell).await)
    }

    /// Starts `nowcast --json watch` with one `--subscribe probe item` per
    /// id.
    pub fn watch(&self, ids: &[&str]) -> Watch {
        let mut command = self.cli();
        command.args(["--json", "watch"]);
        for id in ids {
            command.args(["--subscribe", "probe", "item", &params(id)]);
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = AsyncBufReader::new(child.stdout.take().unwrap()).lines();
        let stderr = AsyncBufReader::new(child.stderr.take().unwrap()).lines();
        Watch {
            child,
            stdout,
            stderr,
        }
    }

    /// Starts an [`Agent`].
    pub fn agent(&self) -> Agent {
        let mut child = Command::new(AGENT)
            .env("XDG_RUNTIME_DIR", self.runtime())
            .env("NOWCAST_SESSION", AGENT_SESSION)
            .env_remove("NOWCAST_SOCKET")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = AsyncBufReader::new(child.stdout.take().unwrap()).lines();
        Agent {
            child,
            stdin,
            stdout,
        }
    }

    /// The next brick process the daemon launches.
    pub async fn brick(&self) -> ControlledBrick {
        timeout(TIMEOUT, self.control.accept())
            .await
            .expect("the daemon launches the brick")
            .unwrap()
    }

    /// A connection to the daemon, anchored on the test process.
    pub fn connect(&self) -> Connection {
        let stream = UnixStream::connect(self.socket()).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        Connection(stream)
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        // The daemon's log goes with a failure, in the test's own output.
        if std::thread::panicking()
            && let Ok(log) = fs::read_to_string(self.root.path().join("daemon.log"))
        {
            eprintln!("--- daemon log ---\n{log}--- end of daemon log ---");
        }
    }
}

/// A client kind declaration: matching `exe`, or no process at all, which
/// switches off an embedded kind of the same name.
fn kind(name: &str, exe: Option<&Path>) -> String {
    let patterns = exe
        .map(|exe| format!("{:?}", exe.display().to_string()))
        .unwrap_or_default();
    format!("name = {name:?}\ndescription = \"End-to-end tests\"\n\n[anchor]\nexe = [{patterns}]\n")
}

/// The params of the probe's subscription to `id`.
pub fn params(id: &str) -> String {
    serde_json::json!({ "id": id }).to_string()
}

async fn run_to_end(command: &mut Command) -> std::process::Output {
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    timeout(TIMEOUT * 2, child.wait_with_output())
        .await
        .expect("the tool ends")
        .unwrap()
}

/// What a run of the tool gave.
#[derive(Debug)]
pub struct Ran {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl From<std::process::Output> for Ran {
    fn from(output: std::process::Output) -> Self {
        Self {
            status: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

impl Ran {
    /// The one JSON value printed on stdout, after a success.
    pub fn json(&self) -> Value {
        assert_eq!(self.status, Some(0), "{self:?}");
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| panic!("{e}: {self:?}"))
    }

    /// The error code printed with `--json`, after a failure.
    pub fn error_code(&self) -> String {
        assert_eq!(self.status, Some(1), "{self:?}");
        let error: Value =
            serde_json::from_str(&self.stderr).unwrap_or_else(|e| panic!("{e}: {self:?}"));
        error["error"]["code"].as_str().unwrap().to_owned()
    }
}

/// A running `nowcast --json watch`.
pub struct Watch {
    child: Child,
    stdout: Lines<AsyncBufReader<ChildStdout>>,
    stderr: Lines<AsyncBufReader<ChildStderr>>,
}

impl Watch {
    /// Waits for the tool to report its subscription to `flow` on stderr.
    pub async fn subscribed(&mut self, flow: &str) {
        let expected = format!("nowcast: subscribed to {flow}");
        loop {
            let line = timeout(TIMEOUT, self.stderr.next_line())
                .await
                .expect("watch reports its subscription")
                .unwrap()
                .expect("watch is running");
            if line == expected {
                return;
            }
        }
    }

    /// The next event printed, as `{"event": <name>, "data": <data>}`.
    pub async fn next(&mut self) -> Value {
        let line = timeout(TIMEOUT, self.stdout.next_line())
            .await
            .expect("watch prints an event")
            .unwrap()
            .expect("watch is running");
        serde_json::from_str(&line).unwrap()
    }

    pub async fn kill(&mut self) {
        self.child.kill().await.unwrap();
    }
}

/// A stand-in for an agent: the tool it runs is anchored on it.
pub struct Agent {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<AsyncBufReader<ChildStdout>>,
}

impl Agent {
    /// Runs `nowcast` with `args` as a child of the agent, like a hook.
    pub async fn run(&mut self, args: &[&str]) -> Ran {
        let mut argv = vec![cli_binary().display().to_string()];
        argv.extend(args.iter().map(|arg| arg.to_string()));
        let mut line = serde_json::to_vec(&argv).unwrap();
        line.push(b'\n');
        self.stdin.write_all(&line).await.unwrap();
        self.stdin.flush().await.unwrap();
        let line = timeout(TIMEOUT * 2, self.stdout.next_line())
            .await
            .expect("the hook ends")
            .unwrap()
            .expect("the agent is running");
        let ran: Value = serde_json::from_str(&line).unwrap();
        Ran {
            status: ran["status"].as_i64().map(|status| status as i32),
            stdout: ran["stdout"].as_str().unwrap().to_owned(),
            stderr: ran["stderr"].as_str().unwrap().to_owned(),
        }
    }

    /// Kills the agent and waits for its end.
    pub async fn kill(&mut self) -> ExitStatus {
        self.child.kill().await.unwrap();
        self.child.wait().await.unwrap()
    }
}

/// Waits until the brick has received a message `matches` accepts, and
/// returns everything it received.
pub async fn received(
    brick: &mut ControlledBrick,
    what: &str,
    matches: impl Fn(&ServiceMessage) -> bool,
) -> Vec<ServiceMessage> {
    wait_for(what, async || {
        let messages = brick.received().await.unwrap();
        messages.iter().any(&matches).then_some(messages)
    })
    .await
}

/// A raw HTTP/1.1 connection to the daemon, for what the tool has no command
/// for.
pub struct Connection(UnixStream);

impl Connection {
    /// Sends one request and returns the status and body of the answer.
    pub fn request(&self, method: &str, path: &str, body: &str) -> (u16, String) {
        self.try_request(method, path, body).unwrap()
    }

    fn try_request(&self, method: &str, path: &str, body: &str) -> io::Result<(u16, String)> {
        write!(
            &self.0,
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        )?;
        let mut reader = BufReader::new(&self.0);
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
}

/// Every byte but ASCII letters and digits percent-encoded, for a path
/// segment.
pub fn encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b if b.is_ascii_alphanumeric() => char::from(b).to_string(),
            b => format!("%{b:02X}"),
        })
        .collect()
}
