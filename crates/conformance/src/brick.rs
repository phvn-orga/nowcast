//! Contract section 4, the brick's side: a brick executable, in any language,
//! driven over its standard input and output as the service would. Its output
//! is read with [`crate::reference`], strictly: anything but well-formed frames
//! on standard output fails the suite.

use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use nowcast_types::{
    BrickMessage, BrickName, FlowId, PROTOCOL_VERSION, READY_TIMEOUT, RequestId, ServiceMessage,
};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time::timeout;

use crate::reference;

/// A brick executable and a subscription it can serve.
pub trait BrickSubject {
    /// The command launching the brick. The suite pipes its input and output.
    fn command() -> std::process::Command;
    fn name() -> BrickName;
    /// A subscription kind and JSON params the brick serves.
    fn subscription() -> (String, Vec<u8>);
}

/// How long a brick has to answer anything but `hello`, or to exit.
const ANSWER: Duration = Duration::from_secs(10);

struct Running {
    child: Child,
    stdin: ChildStdin,
    /// The frames of standard output, read by a task so that waiting on them
    /// with a timeout never cuts a frame. Closes at the end of the output.
    frames: UnboundedReceiver<Result<Vec<u8>, String>>,
}

impl Running {
    fn launch<S: BrickSubject>() -> Self {
        let mut child = tokio::process::Command::from(S::command())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("the brick launches");
        let stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, frames) = unbounded_channel();
        tokio::spawn(async move {
            while let Some(frame) = reference::read_frame(&mut stdout).await.transpose() {
                let failed = frame.is_err();
                if tx.send(frame).is_err() || failed {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            frames,
        }
    }

    /// Launches the brick and completes `hello`.
    async fn ready<S: BrickSubject>() -> Self {
        let mut brick = Self::launch::<S>();
        brick.send(&hello::<S>()).await;
        assert_eq!(brick.recv(READY_TIMEOUT).await, BrickMessage::Ready);
        brick
    }

    async fn send(&mut self, msg: &ServiceMessage) {
        self.stdin
            .write_all(&reference::encode_service(msg))
            .await
            .expect("the brick reads its input");
        self.stdin.flush().await.unwrap();
    }

    /// The next message, `None` once the output is closed.
    async fn try_recv(&mut self, within: Duration) -> Option<BrickMessage> {
        let frame = timeout(within, self.frames.recv())
            .await
            .unwrap_or_else(|_| panic!("no message within {within:?}"))?
            .unwrap_or_else(|e| panic!("unreadable output: {e}"));
        Some(reference::decode_brick(&frame).unwrap_or_else(|e| panic!("invalid message: {e}")))
    }

    async fn recv(&mut self, within: Duration) -> BrickMessage {
        self.try_recv(within)
            .await
            .expect("the brick closed its output")
    }

    /// The next message that is not a `publish`, acknowledging publishes on the
    /// way as the service would.
    async fn recv_answer(&mut self) -> BrickMessage {
        loop {
            match self.recv(ANSWER).await {
                BrickMessage::Publish { req, .. } => self.send(&ServiceMessage::Ack { req }).await,
                other => return other,
            }
        }
    }

    async fn subscribe<S: BrickSubject>(&mut self, req: u64) -> FlowId {
        let (kind, params) = S::subscription();
        let req = RequestId(req);
        self.send(&ServiceMessage::Subscribe { req, kind, params })
            .await;
        match self.recv_answer().await {
            BrickMessage::Subscribed {
                req: answered,
                flow,
            } => {
                assert_eq!(answered, req, "subscribed echoes the req");
                flow
            }
            other => panic!("expected subscribed, got {other:?}"),
        }
    }

    async fn exit(mut self) -> ExitStatus {
        timeout(ANSWER, self.child.wait())
            .await
            .expect("the brick exits")
            .unwrap()
    }
}

fn hello<S: BrickSubject>() -> ServiceMessage {
    ServiceMessage::Hello {
        name: S::name(),
        protocol: PROTOCOL_VERSION,
    }
}

/// The brick answers `hello` with `ready` within 5 s.
pub async fn answers_hello_with_ready<S: BrickSubject>() {
    Running::ready::<S>().await;
}

/// `subscribe` is answered with `subscribed`, same `req`, and a flow id of at
/// most 256 bytes.
pub async fn answers_subscribe_with_its_req<S: BrickSubject>() {
    let mut brick = Running::ready::<S>().await;
    brick.subscribe::<S>(41).await;
}

/// The same subscription content gives the same flow id, including after a
/// restart of the brick.
pub async fn flow_ids_are_deterministic<S: BrickSubject>() {
    let mut brick = Running::ready::<S>().await;
    let first = brick.subscribe::<S>(1).await;
    assert_eq!(brick.subscribe::<S>(2).await, first);
    drop(brick);
    let mut restarted = Running::ready::<S>().await;
    assert_eq!(restarted.subscribe::<S>(1).await, first);
}

/// After `refresh`, the brick publishes on the flow: `req`, the flow, the
/// type, and a JSON detail.
pub async fn publishes_after_a_refresh<S: BrickSubject>() {
    let mut brick = Running::ready::<S>().await;
    let flow = brick.subscribe::<S>(1).await;
    // Acknowledge what the brick publishes on its own before asking.
    while let Ok(frame) = timeout(Duration::from_millis(500), brick.frames.recv()).await {
        let frame = frame.expect("the brick keeps its output open").unwrap();
        if let BrickMessage::Publish { req, .. } = reference::decode_brick(&frame).unwrap() {
            brick.send(&ServiceMessage::Ack { req }).await;
        }
    }
    brick
        .send(&ServiceMessage::Refresh { flow: flow.clone() })
        .await;
    match brick.recv(ANSWER).await {
        BrickMessage::Publish {
            req,
            flow: published,
            data_type,
            detail,
        } => {
            assert_eq!(published, flow);
            assert!(!data_type.is_empty());
            serde_json::from_slice::<serde_json::Value>(&detail).expect("the detail is JSON");
            brick.send(&ServiceMessage::Ack { req }).await;
        }
        other => panic!("expected publish, got {other:?}"),
    }
}

/// `release` gets no answer; the brick keeps serving.
pub async fn release_gets_no_answer<S: BrickSubject>() {
    let mut brick = Running::ready::<S>().await;
    let flow = brick.subscribe::<S>(1).await;
    brick
        .send(&ServiceMessage::Release { flow: flow.clone() })
        .await;
    assert_eq!(brick.subscribe::<S>(2).await, flow);
}

/// On `shutdown` the brick exits, its output holding nothing but frames.
pub async fn shutdown_ends_the_brick<S: BrickSubject>() {
    let mut brick = Running::ready::<S>().await;
    brick.subscribe::<S>(1).await;
    brick.send(&ServiceMessage::Shutdown).await;
    while brick.try_recv(ANSWER).await.is_some() {}
    brick.exit().await;
}

/// Runs the brick-side rules of contract section 4 against a brick executable.
#[macro_export]
macro_rules! brick_suite {
    ($subject:ty) => {
        mod brick_suite {
            #[allow(unused_imports)]
            use super::*;
            $crate::__tests!(
                block_on,
                brick,
                $subject,
                [
                    answers_hello_with_ready,
                    answers_subscribe_with_its_req,
                    flow_ids_are_deterministic,
                    publishes_after_a_refresh,
                    release_gets_no_answer,
                    shutdown_ends_the_brick,
                ]
            );
        }
    };
}
