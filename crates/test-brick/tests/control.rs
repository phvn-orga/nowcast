//! The test brick process driven over its control socket, the test playing
//! the service on its standard input and output.

use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use nowcast_codec::{read_brick, write_service};
use nowcast_test_brick::{
    CONFIG_STATUS, CONTROL_VAR, CRASH_STATUS, Control, ControlledBrick, KIND, MODE_VAR, Mode, NAME,
    REFUSAL_CODE, TYPE,
};
use nowcast_types::{
    BrickMessage, BrickName, FlowId, PROTOCOL_VERSION, PublishErrorCode, RequestId, ServiceMessage,
};
use tempfile::TempDir;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(10);

struct Launched {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    _dir: TempDir,
}

fn command(mode: Option<&str>) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nowcast-test-brick"));
    command
        .env_remove(MODE_VAR)
        .env_remove(CONTROL_VAR)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    if let Some(mode) = mode {
        command.env(MODE_VAR, mode);
    }
    command
}

impl Launched {
    /// Launches the brick in `mode` with a control socket, and takes its
    /// connection.
    async fn new(mode: Mode) -> (Self, ControlledBrick) {
        let dir = tempfile::tempdir().unwrap();
        let control = Control::bind(dir.path().join("control.sock")).unwrap();
        let mut command = command(Some(mode.as_str()));
        command.env(CONTROL_VAR, control.path());
        let mut child = command.spawn().unwrap();
        let brick = timeout(WAIT, control.accept())
            .await
            .expect("the brick connects")
            .unwrap();
        let launched = Self {
            stdin: child.stdin.take().unwrap(),
            stdout: child.stdout.take().unwrap(),
            child,
            _dir: dir,
        };
        (launched, brick)
    }

    /// Launches the brick and completes `hello`.
    async fn ready() -> (Self, ControlledBrick) {
        let (mut launched, brick) = Self::new(Mode::Ready).await;
        launched.send(&hello()).await;
        assert_eq!(launched.recv().await, BrickMessage::Ready);
        (launched, brick)
    }

    async fn send(&mut self, msg: &ServiceMessage) {
        write_service(&mut self.stdin, msg).await.unwrap();
    }

    async fn try_recv(&mut self) -> Option<BrickMessage> {
        timeout(WAIT, read_brick(&mut self.stdout))
            .await
            .expect("a message from the brick")
            .expect("a well-formed message")
    }

    async fn recv(&mut self) -> BrickMessage {
        self.try_recv()
            .await
            .expect("the brick keeps its output open")
    }

    /// Asserts that nothing comes for a while.
    async fn silent(&mut self) {
        let next = timeout(Duration::from_millis(300), read_brick(&mut self.stdout)).await;
        assert!(next.is_err(), "unexpected {next:?}");
    }

    async fn subscribe(&mut self, req: u64, id: &str) -> BrickMessage {
        self.send(&subscribe(req, id)).await;
        self.recv().await
    }

    async fn exit(mut self) -> ExitStatus {
        timeout(WAIT, self.child.wait())
            .await
            .expect("the brick exits")
            .unwrap()
    }
}

fn hello() -> ServiceMessage {
    ServiceMessage::Hello {
        name: BrickName::new(NAME).unwrap(),
        protocol: PROTOCOL_VERSION,
    }
}

fn subscribe(req: u64, id: &str) -> ServiceMessage {
    ServiceMessage::Subscribe {
        req: RequestId(req),
        kind: KIND.into(),
        params: serde_json::to_vec(&serde_json::json!({ "id": id })).unwrap(),
    }
}

fn flow(id: &str) -> FlowId {
    FlowId::new(id).unwrap()
}

/// Runs a command on the brick while the test answers as the service:
/// expects one `publish`, checks it, and answers it with `answer`.
async fn publish(
    launched: &mut Launched,
    brick: &mut ControlledBrick,
    data_type: &str,
    detail: &[u8],
    answer: Option<PublishErrorCode>,
) -> Result<(), PublishErrorCode> {
    let a = flow("a");
    let call = brick.publish(&a, data_type, detail);
    let serve = async {
        let BrickMessage::Publish {
            req,
            flow: published,
            data_type: published_type,
            detail: published_detail,
        } = launched.recv().await
        else {
            panic!("expected publish");
        };
        assert_eq!(
            (
                published,
                published_type.as_str(),
                published_detail.as_slice()
            ),
            (flow("a"), data_type, detail)
        );
        let reply = match answer {
            None => ServiceMessage::Ack { req },
            Some(code) => ServiceMessage::Error { req, code },
        };
        launched.send(&reply).await;
    };
    let (result, ()) = tokio::join!(call, serve);
    result.unwrap()
}

#[tokio::test]
async fn a_publish_returns_the_service_s_answer() {
    let (mut launched, mut brick) = Launched::ready().await;
    launched.subscribe(1, "a").await;
    let valid = publish(&mut launched, &mut brick, TYPE, br#"{"n":1}"#, None).await;
    assert_eq!(valid, Ok(()));
    let invalid_json = publish(
        &mut launched,
        &mut brick,
        TYPE,
        b"{not json",
        Some(PublishErrorCode::InvalidJson),
    )
    .await;
    assert_eq!(invalid_json, Err(PublishErrorCode::InvalidJson));
    let undeclared = publish(
        &mut launched,
        &mut brick,
        "probe.other/v1",
        b"{}",
        Some(PublishErrorCode::UndeclaredType),
    )
    .await;
    assert_eq!(undeclared, Err(PublishErrorCode::UndeclaredType));
}

#[tokio::test]
async fn publish_reqs_grow_across_refreshes_and_commands() {
    let (mut launched, mut brick) = Launched::ready().await;
    launched.subscribe(1, "a").await;
    launched
        .send(&ServiceMessage::Refresh { flow: flow("a") })
        .await;
    let BrickMessage::Publish { req: first, .. } = launched.recv().await else {
        panic!("expected publish");
    };
    launched.send(&ServiceMessage::Ack { req: first }).await;
    let a = flow("a");
    let call = brick.publish(&a, TYPE, b"{}");
    let serve = async {
        let BrickMessage::Publish { req, .. } = launched.recv().await else {
            panic!("expected publish");
        };
        assert!(req.0 > first.0, "{req:?} after {first:?}");
        launched.send(&ServiceMessage::Ack { req }).await;
    };
    let (result, ()) = tokio::join!(call, serve);
    assert_eq!(result.unwrap(), Ok(()));
}

#[tokio::test]
async fn flow_error_on_demand() {
    let (mut launched, mut brick) = Launched::ready().await;
    launched.subscribe(1, "a").await;
    brick.flow_error(&flow("a"), "source down").await.unwrap();
    assert_eq!(
        launched.recv().await,
        BrickMessage::FlowError {
            flow: flow("a"),
            message: "source down".into()
        }
    );
}

#[tokio::test]
async fn received_lists_what_the_service_sent() {
    let (mut launched, mut brick) = Launched::ready().await;
    launched.subscribe(4, "a").await;
    launched
        .send(&ServiceMessage::Refresh { flow: flow("a") })
        .await;
    let BrickMessage::Publish { req, .. } = launched.recv().await else {
        panic!("expected publish");
    };
    launched.send(&ServiceMessage::Ack { req }).await;
    launched
        .send(&ServiceMessage::Release { flow: flow("a") })
        .await;
    // Messages are read in order on one stream: once the brick has answered
    // this subscribe, it holds everything sent before.
    launched.subscribe(5, "b").await;
    assert_eq!(
        brick.received().await.unwrap(),
        vec![
            hello(),
            subscribe(4, "a"),
            ServiceMessage::Refresh { flow: flow("a") },
            ServiceMessage::Ack { req },
            ServiceMessage::Release { flow: flow("a") },
            subscribe(5, "b"),
        ]
    );
}

#[tokio::test]
async fn crash_on_demand() {
    let (mut launched, brick) = Launched::ready().await;
    launched.subscribe(1, "a").await;
    timeout(WAIT, brick.crash()).await.unwrap().unwrap();
    assert_eq!(launched.try_recv().await, None, "the output closes");
    assert_eq!(launched.exit().await.code(), Some(CRASH_STATUS));
}

#[tokio::test]
async fn no_ready_on_demand() {
    let (mut launched, mut brick) = Launched::new(Mode::NeverReady).await;
    launched.send(&hello()).await;
    launched.silent().await;
    assert_eq!(brick.received().await.unwrap(), vec![hello()]);
    launched.send(&ServiceMessage::Shutdown).await;
    assert_eq!(launched.try_recv().await, None);
    assert!(launched.exit().await.success());
}

#[tokio::test]
async fn refusing_refuses_every_subscription() {
    let (mut launched, _brick) = Launched::new(Mode::Refusing).await;
    launched.send(&hello()).await;
    assert_eq!(launched.recv().await, BrickMessage::Ready);
    for (req, id) in [(1, "a"), (2, "b")] {
        match launched.subscribe(req, id).await {
            BrickMessage::Error {
                req: answered,
                code,
                ..
            } => {
                assert_eq!(answered, RequestId(req));
                assert_eq!(code, REFUSAL_CODE);
            }
            other => panic!("expected error, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn shutdown_exits_cleanly() {
    let (mut launched, brick) = Launched::ready().await;
    launched.subscribe(1, "a").await;
    launched.send(&ServiceMessage::Shutdown).await;
    timeout(WAIT, brick.exited()).await.unwrap().unwrap();
    assert_eq!(launched.try_recv().await, None);
    assert!(launched.exit().await.success());
}

#[tokio::test]
async fn the_end_of_its_input_ends_the_brick() {
    let (launched, brick) = Launched::ready().await;
    let Launched {
        mut child, stdin, ..
    } = launched;
    drop(stdin);
    timeout(WAIT, brick.exited()).await.unwrap().unwrap();
    let status = timeout(WAIT, child.wait()).await.unwrap().unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn the_brick_serves_on_once_the_control_closes() {
    let (mut launched, brick) = Launched::ready().await;
    drop(brick);
    assert_eq!(
        launched.subscribe(1, "a").await,
        BrickMessage::Subscribed {
            req: RequestId(1),
            flow: flow("a")
        }
    );
}

#[tokio::test]
async fn an_unreachable_control_socket_fails_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let mut command = command(None);
    command.env(CONTROL_VAR, dir.path().join("nobody.sock"));
    let status = timeout(WAIT, command.status()).await.unwrap().unwrap();
    assert_eq!(status.code(), Some(CONFIG_STATUS));
}

#[tokio::test]
async fn an_unknown_mode_fails_the_start() {
    let status = timeout(WAIT, command(Some("sleepy")).status())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(CONFIG_STATUS));
}
