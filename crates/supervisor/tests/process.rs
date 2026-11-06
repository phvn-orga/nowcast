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
        Err(ResolveError::Unavailable(BrickUnavailable(probe::name())))
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

#[tokio::test]
async fn a_brick_that_cannot_be_launched_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let control = Control::bind(dir.path().join("control.sock")).unwrap();
    let declaration = probe::declaration(
        vec![dir.path().join("missing").to_string_lossy().into_owned()],
        dir.path().to_owned(),
    );
    let harness = Harness::with(declaration, fast(), control, dir);
    assert_eq!(
        harness.resolve("a").await,
        Err(ResolveError::Unavailable(BrickUnavailable(probe::name())))
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

#[tokio::test]
async fn a_brick_ends_when_it_exits_even_if_its_child_holds_its_output() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("child.pid");
    // A `ready` frame: length 12, then the map {"v": 1, "t": "ready"}.
    let ready = r"\014\000\000\000\202\241v\001\241t\245ready";
    let script = format!(r#"sleep 1000 & echo $! > "$0"; printf '{ready}'; exit 3"#);
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
    kill_pid_in(&pid_file);
    // What the brick wrote before exiting still arrives, then the end.
    assert_eq!(
        received.expect("the exit ends the output"),
        (Some(BrickMessage::Ready), None)
    );
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

#[tokio::test]
async fn the_brick_runs_in_its_directory_with_its_environment_and_its_stderr_is_logged() {
    let captured = Captured::default();
    let writer = captured.clone();
    // Another crate of the workspace may enable the `ansi` feature, which
    // would put escape codes between a field's name and its value.
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    // The test runtime runs every task on this thread.
    let _guard = tracing::subscriber::set_default(subscriber);

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
