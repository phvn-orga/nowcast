//! Real processes: the supervisor driving the test brick through
//! [`ProcessLauncher`], and the launcher on its own. The clock is real, so the
//! delays are shortened.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use nowcast_conformance::probe;
use nowcast_supervisor::{BrickSupervisor, Config, ProcessLauncher};
use nowcast_test_brick::{CONTROL_VAR, Control, ControlledBrick, MODE_VAR, Mode};
use nowcast_traits::fake::{FakeDiscovery, FakeRouter};
use nowcast_traits::{
    AnchorId, BrickDeclaration, BrickLauncher, BrickUnavailable, ResolveError, Router, Supervisor,
};
use nowcast_types::{
    BrickMessage, FlowId, PublishErrorCode, QualifiedFlowId, ServiceMessage, StreamEvent,
};
use tempfile::TempDir;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{Instant, sleep, timeout};

type Subject = BrickSupervisor<FakeDiscovery, ProcessLauncher, FakeRouter>;

const A: AnchorId = AnchorId(1);
/// Long enough for a process to start, answer or exit.
const WAIT: Duration = Duration::from_secs(10);
/// How long to watch for something that must not happen.
const QUIET: Duration = Duration::from_millis(300);

fn fast() -> Config {
    Config {
        ready_timeout: Duration::from_secs(2),
        answer_timeout: Duration::from_secs(2),
        grace: Duration::from_millis(300),
        terminate_after: Duration::from_millis(500),
        kill_after: Duration::from_millis(500),
        first_backoff: Duration::from_millis(50),
        ..Config::default()
    }
}

/// The test brick binary. Cargo builds it for this package's tests only
/// under `cargo test --workspace`, so it is built here when missing or stale.
fn test_brick() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let mut cargo = std::process::Command::new(env!("CARGO"));
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
        assert!(
            cargo.status().expect("cargo runs").success(),
            "the test brick builds"
        );
        nowcast_test_brick::binary().expect("the profile directory of the tests")
    })
}

fn flow(id: &str) -> FlowId {
    FlowId::new(id).unwrap()
}

fn qualified(id: &str) -> QualifiedFlowId {
    QualifiedFlowId::new(probe::name(), flow(id))
}

/// A supervisor launching the test brick, as the probe, in `mode`.
struct Harness {
    supervisor: Subject,
    router: Arc<FakeRouter>,
    control: Control,
    _dir: TempDir,
}

impl Harness {
    fn new(mode: Mode, config: Config) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let control = Control::bind(dir.path().join("control.sock")).unwrap();
        let command = vec![test_brick().to_string_lossy().into_owned()];
        let mut declaration = probe::declaration(command, dir.path().to_owned());
        declaration.env.insert(
            CONTROL_VAR.into(),
            control.path().to_string_lossy().into_owned(),
        );
        declaration
            .env
            .insert(MODE_VAR.into(), mode.as_str().into());
        Self::with(declaration, config, control, dir)
    }

    fn with(declaration: BrickDeclaration, config: Config, control: Control, dir: TempDir) -> Self {
        let discovery = Arc::new(FakeDiscovery::new());
        discovery.insert(declaration);
        let router = Arc::new(FakeRouter::new());
        let supervisor =
            BrickSupervisor::start(discovery, Arc::new(ProcessLauncher), router.clone(), config);
        Self {
            supervisor,
            router,
            control,
            _dir: dir,
        }
    }

    /// Subscribes anchor `A` to `id`; gives the flow.
    async fn resolve(&self, id: &str) -> Result<QualifiedFlowId, ResolveError> {
        let params = probe::params(id);
        timeout(
            WAIT,
            self.supervisor
                .resolve(A, &probe::name(), probe::KIND, &params),
        )
        .await
        .expect("resolve answers")
        .map(|resolved| resolved.flow)
    }

    /// The next brick process to start.
    async fn launched(&self) -> ControlledBrick {
        timeout(WAIT, self.control.accept())
            .await
            .expect("a brick process starts")
            .unwrap()
    }

    async fn no_launch(&self) {
        assert!(
            timeout(QUIET, self.control.accept()).await.is_err(),
            "a brick process started"
        );
    }

    /// Resolves `id`, subscribes anchor `A` to it and opens its stream.
    async fn subscribed(&self, id: &str) -> (ControlledBrick, UnboundedReceiver<StreamEvent>) {
        assert_eq!(self.resolve(id).await, Ok(qualified(id)));
        let brick = self.launched().await;
        (brick, self.router.stream(A, None))
    }
}

async fn next_event(stream: &mut UnboundedReceiver<StreamEvent>) -> StreamEvent {
    timeout(WAIT, stream.recv())
        .await
        .expect("a stream event")
        .expect("the stream stays open")
}

async fn exited(brick: ControlledBrick) {
    timeout(WAIT, brick.exited())
        .await
        .expect("the brick process ends")
        .unwrap();
}

#[tokio::test]
async fn the_test_brick_runs_from_the_first_subscription_to_the_end_of_the_grace() {
    let config = fast();
    let harness = Harness::new(Mode::Ready, config.clone());
    harness.no_launch().await;
    let (brick, _stream) = harness.subscribed("a").await;
    let idle = Instant::now();
    harness.router.remove_anchor(A);
    exited(brick).await;
    assert!(idle.elapsed() >= config.grace, "stopped before the grace");
    harness.no_launch().await;
}

#[tokio::test]
async fn a_test_brick_never_ready_fails_the_subscription_with_the_timeout_error() {
    let config = Config {
        ready_timeout: Duration::from_millis(500),
        ..fast()
    };
    let harness = Harness::new(Mode::NeverReady, config.clone());
    let start = Instant::now();
    assert_eq!(
        harness.resolve("a").await,
        Err(ResolveError::Unavailable(BrickUnavailable::new(
            probe::name()
        )))
    );
    assert!(start.elapsed() >= config.ready_timeout);
    exited(harness.launched().await).await;
}

#[tokio::test]
async fn invalid_json_is_refused_and_never_reaches_the_router() {
    let harness = Harness::new(Mode::Ready, fast());
    let (mut brick, mut stream) = harness.subscribed("a").await;
    let refused = brick.publish(&flow("a"), probe::TYPE, b"{not json").await;
    assert_eq!(refused.unwrap(), Err(PublishErrorCode::InvalidJson));
    assert_eq!(harness.router.last(A, &qualified("a")), Ok(None));
    let acked = brick.publish(&flow("a"), probe::TYPE, br#"{"n":1}"#).await;
    assert_eq!(acked.unwrap(), Ok(()));
    // The valid value is the first and only one the clients see.
    match next_event(&mut stream).await {
        StreamEvent::Update(event) => assert_eq!(event.data, br#"{"n":1}"#),
        other => panic!("expected an update, got {other:?}"),
    }
}

#[tokio::test]
async fn after_a_crash_flows_keep_their_ids_and_receive_values_again() {
    let harness = Harness::new(Mode::Ready, fast());
    let (brick, mut stream) = harness.subscribed("a").await;
    brick.crash().await.unwrap();
    match next_event(&mut stream).await {
        StreamEvent::FlowError { flow, .. } => assert_eq!(flow, qualified("a")),
        other => panic!("expected flow-error, got {other:?}"),
    }
    let mut relaunched = harness.launched().await;
    let subscribed_again = async {
        loop {
            let received = relaunched.received().await.unwrap();
            if received
                .iter()
                .any(|m| matches!(m, ServiceMessage::Subscribe { params, .. } if *params == probe::params("a")))
            {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
    };
    timeout(WAIT, subscribed_again)
        .await
        .expect("subscribed again");
    let acked = relaunched
        .publish(&flow("a"), probe::TYPE, br#"{"n":2}"#)
        .await;
    assert_eq!(acked.unwrap(), Ok(()));
    match next_event(&mut stream).await {
        StreamEvent::Update(event) => {
            assert_eq!(event.flow, qualified("a"));
            assert_eq!(event.data, br#"{"n":2}"#);
        }
        other => panic!("expected an update, got {other:?}"),
    }
}

/// What a subscription to a brick launched from `declaration`, in `dir`,
/// fails with.
async fn launch_failure(declaration: BrickDeclaration, dir: TempDir) -> ResolveError {
    let control = Control::bind(dir.path().join("control.sock")).unwrap();
    let harness = Harness::with(declaration, fast(), control, dir);
    harness.resolve("a").await.unwrap_err()
}

fn unavailable_because(cause: String) -> ResolveError {
    ResolveError::Unavailable(BrickUnavailable {
        brick: probe::name(),
        cause: Some(cause),
    })
}

#[tokio::test]
async fn a_brick_that_cannot_be_launched_is_unavailable_with_the_cause() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing").to_string_lossy().into_owned();
    let declaration = probe::declaration(vec![missing.clone()], dir.path().to_owned());
    assert_eq!(
        launch_failure(declaration, dir).await,
        unavailable_because(format!("program {missing:?} not found"))
    );
}

#[tokio::test]
async fn a_program_missing_from_the_path_names_the_path_searched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().into_owned();
    let mut declaration = probe::declaration(
        vec!["nowcast-no-such-program".into()],
        dir.path().to_owned(),
    );
    declaration.env.insert("PATH".into(), path.clone());
    assert_eq!(
        launch_failure(declaration, dir).await,
        unavailable_because(format!(
            "program \"nowcast-no-such-program\" not found in PATH {path}"
        ))
    );
}

#[tokio::test]
async fn a_program_that_is_not_executable_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("brick").to_string_lossy().into_owned();
    std::fs::write(&program, "#!/bin/sh\n").unwrap();
    let declaration = probe::declaration(vec![program.clone()], dir.path().to_owned());
    assert_eq!(
        launch_failure(declaration, dir).await,
        unavailable_because(format!("program {program:?} is not executable"))
    );
}

#[tokio::test]
async fn a_program_whose_interpreter_is_missing_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("brick").to_string_lossy().into_owned();
    std::fs::write(&program, "#!/nonexistent/interpreter\n").unwrap();
    std::fs::set_permissions(
        &program,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let declaration = probe::declaration(vec![program.clone()], dir.path().to_owned());
    assert_eq!(
        launch_failure(declaration, dir).await,
        unavailable_because(format!(
            "program {program:?} cannot be run: the interpreter it names \
             (#! line or dynamic loader) is missing"
        ))
    );
}

/// A brick running `script` with `sh`, a program looked up in `PATH`.
fn shell(script: &str, dir: &Path) -> BrickDeclaration {
    probe::declaration(
        vec!["sh".into(), "-c".into(), script.into()],
        dir.to_owned(),
    )
}

#[tokio::test]
async fn the_launcher_speaks_the_protocol_over_standard_input_and_output() {
    let dir = tempfile::tempdir().unwrap();
    let declaration = probe::declaration(
        vec![test_brick().to_string_lossy().into_owned()],
        dir.path().to_owned(),
    );
    let mut connection = ProcessLauncher.launch(&declaration).unwrap();
    connection
        .to_brick
        .send(ServiceMessage::Hello {
            name: probe::name(),
            protocol: nowcast_types::PROTOCOL_VERSION,
        })
        .unwrap();
    let ready = timeout(WAIT, connection.from_brick.recv()).await.unwrap();
    assert_eq!(ready, Some(BrickMessage::Ready));
    connection.to_brick.send(ServiceMessage::Shutdown).unwrap();
    assert_eq!(
        timeout(WAIT, connection.from_brick.recv()).await.unwrap(),
        None
    );
}

/// Kills the process whose pid is in `file`, if it got there.
fn kill_pid_in(file: &Path) {
    if let Ok(pid) = std::fs::read_to_string(file) {
        let _ = std::process::Command::new("kill").arg(pid.trim()).status();
    }
}

/// Whether the process whose pid is in `file` still runs: a zombie does not.
fn runs(file: &Path) -> bool {
    let pid = std::fs::read_to_string(file).expect("the pid is written");
    std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())).is_ok_and(|stat| {
        !stat
            .rsplit(')')
            .next()
            .unwrap_or("")
            .trim_start()
            .starts_with('Z')
    })
}

/// Waits for the process whose pid is in `file` to end.
async fn ends(file: &Path) {
    let ended = timeout(WAIT, async {
        while runs(file) {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    kill_pid_in(file);
    assert!(ended.is_ok(), "the process still runs");
}

/// Waits for `file` to hold something.
async fn written(file: &Path) {
    timeout(WAIT, async {
        while std::fs::metadata(file).map_or(true, |m| m.len() == 0) {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the file is written");
}

/// A `ready` frame: length 12, then the map {"v": 1, "t": "ready"}, for
/// `printf`.
const READY_FRAME: &str = r"\014\000\000\000\202\241v\001\241t\245ready";

#[tokio::test]
async fn a_kill_reaches_the_processes_the_brick_started() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("child.pid");
    let mut declaration = shell(r#"sleep 1000 & echo $! > "$0"; wait"#, dir.path());
    declaration
        .command
        .push(pid_file.to_string_lossy().into_owned());
    let mut connection = ProcessLauncher.launch(&declaration).unwrap();
    written(&pid_file).await;
    connection.process.kill();
    assert_eq!(
        timeout(WAIT, connection.from_brick.recv()).await.unwrap(),
        None
    );
    ends(&pid_file).await;
}

#[tokio::test]
async fn a_process_writing_after_the_brick_exited_does_not_hold_its_output_open() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("writer.pid");
    // The writer leaves the brick's process group, so it outlives the brick.
    let script = format!(
        r#"setsid sh -c 'echo $$ > "$0"; while :; do printf "{READY_FRAME}"; done' "$0" &
        while [ ! -s "$0" ]; do sleep 0.01; done; exit 3"#
    );
    let mut declaration = shell(&script, dir.path());
    declaration
        .command
        .push(pid_file.to_string_lossy().into_owned());
    let mut connection = ProcessLauncher.launch(&declaration).unwrap();
    let ended = timeout(WAIT, async {
        while connection.from_brick.recv().await.is_some() {}
    })
    .await;
    kill_pid_in(&pid_file);
    assert!(ended.is_ok(), "the output stays open");
}

#[tokio::test]
async fn a_brick_ends_when_it_exits_even_if_its_child_holds_its_output() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("child.pid");
    let script = format!(r#"sleep 1000 & echo $! > "$0"; printf '{READY_FRAME}'; exit 3"#);
    let mut declaration = shell(&script, dir.path());
    declaration
        .command
        .push(pid_file.to_string_lossy().into_owned());
    let mut connection = ProcessLauncher.launch(&declaration).unwrap();
    let received = timeout(WAIT, async {
        (
            connection.from_brick.recv().await,
            connection.from_brick.recv().await,
        )
    })
    .await;
    // What the brick wrote before exiting still arrives, then the end.
    assert_eq!(
        received.expect("the exit ends the output"),
        (Some(BrickMessage::Ready), None)
    );
    // What the brick left behind is killed with it.
    ends(&pid_file).await;
}

#[tokio::test]
async fn sigterm_ends_a_brick_that_ignores_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let mut connection = ProcessLauncher
        .launch(&shell("exec sleep 1000", dir.path()))
        .unwrap();
    connection.to_brick.send(ServiceMessage::Shutdown).unwrap();
    assert!(timeout(QUIET, connection.from_brick.recv()).await.is_err());
    connection.process.terminate();
    assert_eq!(
        timeout(WAIT, connection.from_brick.recv()).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn sigkill_ends_a_brick_that_ignores_sigterm() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("trapped");
    let script = r#"trap '' TERM; : > "$0"; exec sleep 1000"#;
    let mut declaration = shell(script, dir.path());
    declaration
        .command
        .push(marker.to_string_lossy().into_owned());
    let mut connection = ProcessLauncher.launch(&declaration).unwrap();
    timeout(WAIT, async {
        while !marker.exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the brick traps SIGTERM");
    connection.process.terminate();
    assert!(timeout(QUIET, connection.from_brick.recv()).await.is_err());
    connection.process.kill();
    assert_eq!(
        timeout(WAIT, connection.from_brick.recv()).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn shutdown_stops_the_test_brick_through_its_protocol() {
    // `SIGTERM` would come after the test's wait: the brick must exit on
    // `shutdown` alone, whatever the machine's load.
    let config = Config {
        terminate_after: WAIT * 2,
        ..fast()
    };
    let harness = Harness::new(Mode::Ready, config);
    let (brick, _stream) = harness.subscribed("a").await;
    timeout(WAIT, harness.supervisor.shutdown())
        .await
        .expect("the brick stops on shutdown");
    exited(brick).await;
    harness.no_launch().await;
}

#[tokio::test]
async fn shutdown_kills_a_brick_that_ignores_shutdown_and_sigterm() {
    let config = fast();
    let dir = tempfile::tempdir().unwrap();
    let control = Control::bind(dir.path().join("control.sock")).unwrap();
    let pid_file = dir.path().join("brick.pid");
    let script =
        format!(r#"trap '' TERM; echo $$ > "$0"; printf '{READY_FRAME}'; exec sleep 1000"#);
    let mut declaration = shell(&script, dir.path());
    declaration
        .command
        .push(pid_file.to_string_lossy().into_owned());
    let harness = Harness::with(declaration, config.clone(), control, dir);
    // Ready, and never answers the subscription.
    let stopped = async {
        written(&pid_file).await;
        sleep(QUIET).await;
        let start = Instant::now();
        timeout(WAIT, harness.supervisor.shutdown())
            .await
            .expect("the brick is killed");
        assert!(start.elapsed() >= config.terminate_after + config.kill_after);
        assert!(!runs(&pid_file), "the brick still runs");
    };
    let (resolved, ()) = tokio::join!(harness.resolve("a"), stopped);
    assert_eq!(
        resolved,
        Err(ResolveError::Unavailable(BrickUnavailable::new(
            probe::name()
        )))
    );
}

/// Collects what the tracing subscriber writes.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Captures the logs of this thread until the guard drops. The test runtime
/// runs every task on this thread.
fn capture_logs() -> (Captured, tracing::subscriber::DefaultGuard) {
    let captured = Captured::default();
    let writer = captured.clone();
    // Another crate of the workspace may enable the `ansi` feature, which
    // would put escape codes between a field's name and its value.
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    (captured, tracing::subscriber::set_default(subscriber))
}

#[tokio::test]
async fn a_long_stderr_line_is_logged_in_bounded_chunks() {
    let (captured, _guard) = capture_logs();
    let dir = tempfile::tempdir().unwrap();
    let script = "head -c 20000 /dev/zero | tr '\\0' x >&2";
    let mut connection = ProcessLauncher.launch(&shell(script, dir.path())).unwrap();
    assert_eq!(
        timeout(WAIT, connection.from_brick.recv()).await.unwrap(),
        None
    );
    // Each chunk is one event: the run of `x` it carries.
    let chunks = || -> Vec<usize> {
        let text = String::from_utf8_lossy(&captured.0.lock().unwrap()).into_owned();
        text.lines()
            .filter_map(|l| {
                l.split_whitespace()
                    .find(|word| word.bytes().all(|b| b == b'x'))
            })
            .map(str::len)
            .collect()
    };
    let logged = async {
        while chunks().iter().sum::<usize>() < 20000 {
            sleep(Duration::from_millis(10)).await;
        }
    };
    timeout(WAIT, logged)
        .await
        .expect("the whole line is logged");
    assert!(
        chunks().iter().all(|&len| len <= 8 * 1024),
        "{:?}",
        chunks()
    );
}

#[tokio::test]
async fn the_brick_runs_in_its_directory_with_its_environment_and_its_stderr_is_logged() {
    let (captured, _guard) = capture_logs();

    let dir = tempfile::tempdir().unwrap();
    let mut declaration = shell(r#"echo "in $(pwd) with $PROBE_VAR" >&2"#, dir.path());
    declaration
        .env
        .insert("PROBE_VAR".into(), "probe-value".into());
    let mut connection = ProcessLauncher.launch(&declaration).unwrap();
    assert_eq!(
        timeout(WAIT, connection.from_brick.recv()).await.unwrap(),
        None
    );

    let expected = format!(
        "in {} with probe-value",
        dir.path().canonicalize().unwrap().display()
    );
    let logged = async {
        loop {
            let text = String::from_utf8_lossy(&captured.0.lock().unwrap()).into_owned();
            if let Some(line) = text.lines().find(|l| l.contains(&expected)) {
                return line.to_owned();
            }
            sleep(Duration::from_millis(10)).await;
        }
    };
    let line = timeout(WAIT, logged)
        .await
        .expect("the stderr line is logged");
    assert!(line.contains("brick=probe"), "{line}");
}

#[tokio::test]
async fn an_empty_command_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let declaration = probe::declaration(vec![], dir.path().to_owned());
    let error = ProcessLauncher.launch(&declaration).err().expect("refused");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}
