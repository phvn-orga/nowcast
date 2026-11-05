//! Contract section 4, the service's side of the brick protocol and the brick
//! lifecycle, plus the end of a removed brick from section 2. Checked on a
//! [`Supervisor`] wired to the fakes, the probe declared.
//!
//! The tests run on a paused clock: the supervisor must take its delays from
//! `tokio::time`, and the contract's 5 s and 30 s pass instantly.

use std::sync::Arc;
use std::time::Duration;

use nowcast_traits::fake::{FakeBrick, FakeDiscovery, FakeLauncher, FakeRouter, Signal};
use nowcast_traits::{AnchorId, ResolveError, Router, Supervisor, UnknownFlow};
use nowcast_types::{
    BrickMessage, BrickName, FlowId, PROTOCOL_VERSION, PublishErrorCode, QualifiedFlowId,
    READY_TIMEOUT, RequestId, ServiceMessage, StreamEvent,
};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};

use crate::probe;

/// Starts a supervisor on the given fakes.
pub trait SupervisorSubject {
    type Supervisor: Supervisor;
    fn start(
        discovery: Arc<FakeDiscovery>,
        launcher: Arc<FakeLauncher>,
        router: Arc<FakeRouter>,
    ) -> Self::Supervisor;
}

/// The contract's grace period before stopping an idle brick.
const GRACE: Duration = Duration::from_secs(30);
/// Longer than any delay of the supervisor, however its open delays are set.
const LONG: Duration = Duration::from_secs(600);
const A: AnchorId = AnchorId(1);

struct World<S> {
    supervisor: Arc<S>,
    discovery: Arc<FakeDiscovery>,
    launcher: Arc<FakeLauncher>,
    router: Arc<FakeRouter>,
}

fn world<S: SupervisorSubject>() -> World<S::Supervisor> {
    let discovery = Arc::new(FakeDiscovery::new());
    discovery.insert(probe::declaration(vec!["probe".into()]));
    let launcher = Arc::new(FakeLauncher::new());
    let router = Arc::new(FakeRouter::new());
    let supervisor = Arc::new(S::start(
        discovery.clone(),
        launcher.clone(),
        router.clone(),
    ));
    World {
        supervisor,
        discovery,
        launcher,
        router,
    }
}

fn flow(id: &str) -> FlowId {
    FlowId::new(id).unwrap()
}

fn qualified(id: &str) -> QualifiedFlowId {
    QualifiedFlowId::new(probe::name(), flow(id))
}

impl<S: Supervisor> World<S> {
    fn resolve(&self, id: &str) -> JoinHandle<Result<QualifiedFlowId, ResolveError>> {
        let supervisor = self.supervisor.clone();
        let params = probe::params(id);
        tokio::spawn(async move {
            supervisor
                .resolve(&probe::name(), probe::KIND, &params)
                .await
        })
    }

    /// Takes the next launch and answers its `hello`.
    async fn ready_brick(&self) -> FakeBrick {
        let mut brick = timeout(LONG, self.launcher.next()).await.expect("a launch");
        assert_eq!(
            recv(&mut brick).await,
            ServiceMessage::Hello {
                name: probe::name(),
                protocol: PROTOCOL_VERSION
            }
        );
        brick.to_service.send(BrickMessage::Ready).unwrap();
        brick
    }

    /// Resolves `id` on a fresh brick and subscribes anchor `A` to it, as the
    /// server does.
    async fn subscribed(&self, id: &str) -> FakeBrick {
        let pending = self.resolve(id);
        let mut brick = self.ready_brick().await;
        serve(&mut brick, id).await;
        assert_eq!(pending.await.unwrap(), Ok(qualified(id)));
        self.router.subscribe(A, qualified(id));
        brick
    }
}

async fn recv(brick: &mut FakeBrick) -> ServiceMessage {
    timeout(LONG, brick.from_service.recv())
        .await
        .expect("a message from the service")
        .expect("the service holds the channel")
}

/// Answers the next `subscribe`, expected for `id`, with flow `id`.
async fn serve(brick: &mut FakeBrick, id: &str) {
    let ServiceMessage::Subscribe { req, kind, params } = recv(brick).await else {
        panic!("expected subscribe");
    };
    assert_eq!(kind, probe::KIND);
    assert_eq!(params, probe::params(id));
    brick
        .to_service
        .send(BrickMessage::Subscribed {
            req,
            flow: flow(id),
        })
        .unwrap();
}

/// Waits for the first sign of the service stopping the brick: `shutdown`, a
/// signal, or the channel closed.
async fn stopped(brick: &mut FakeBrick) {
    let sign = timeout(LONG, async {
        loop {
            tokio::select! {
                msg = brick.from_service.recv() => match msg {
                    Some(ServiceMessage::Shutdown) | None => return,
                    Some(_) => {}
                },
                Some(_) = brick.signals.recv() => return,
            }
        }
    })
    .await;
    assert!(sign.is_ok(), "the brick is never stopped");
}

async fn publish(brick: &mut FakeBrick, req: u64, id: &str, data_type: &str, detail: &[u8]) {
    brick
        .to_service
        .send(BrickMessage::Publish {
            req: RequestId(req),
            flow: flow(id),
            data_type: data_type.into(),
            detail: detail.to_vec(),
        })
        .unwrap();
}

/// No brick runs before its first subscription; the first one launches it and
/// the service says `hello` with the brick's name and protocol.
pub async fn the_first_subscription_launches_the_brick<S: SupervisorSubject>() {
    let world = world::<S>();
    sleep(Duration::from_secs(1)).await;
    assert!(world.launcher.try_next().is_none(), "launched too early");
    let _pending = world.resolve("a");
    world.ready_brick().await;
}

/// `subscribe` carries the kind and the params as JSON bytes; the flow the
/// brick answers is prefixed with its name.
pub async fn subscribe_maps_to_a_prefixed_flow<S: SupervisorSubject>() {
    let world = world::<S>();
    let pending = world.resolve("a");
    let mut brick = world.ready_brick().await;
    serve(&mut brick, "a").await;
    assert_eq!(pending.await.unwrap(), Ok(qualified("a")));
}

/// A running brick serves the next subscriptions: no second launch.
pub async fn a_running_brick_is_reused<S: SupervisorSubject>() {
    let world = world::<S>();
    let mut brick = world.subscribed("a").await;
    let pending = world.resolve("b");
    serve(&mut brick, "b").await;
    assert_eq!(pending.await.unwrap(), Ok(qualified("b")));
    assert!(world.launcher.try_next().is_none());
}

/// Without `ready` within 5 s, the brick is stopped and the subscription fails
/// with `brick_unavailable`.
pub async fn a_brick_missing_ready_is_unavailable<S: SupervisorSubject>() {
    let world = world::<S>();
    let start = Instant::now();
    let mut pending = world.resolve("a");
    let mut brick = world.launcher.next().await;
    assert!(
        timeout(READY_TIMEOUT - Duration::from_millis(100), &mut pending)
            .await
            .is_err(),
        "gave up before the ready timeout"
    );
    let result = pending.await.unwrap();
    assert_eq!(
        result,
        Err(ResolveError::Unavailable(nowcast_traits::BrickUnavailable(
            probe::name()
        )))
    );
    assert!(start.elapsed() >= READY_TIMEOUT);
    stopped(&mut brick).await;
}

/// A brick answering `subscribe` with `error` fails the subscription with its
/// code and message.
pub async fn a_refused_subscription_fails<S: SupervisorSubject>() {
    let world = world::<S>();
    let pending = world.resolve("a");
    let mut brick = world.ready_brick().await;
    let ServiceMessage::Subscribe { req, .. } = recv(&mut brick).await else {
        panic!("expected subscribe");
    };
    brick
        .to_service
        .send(BrickMessage::Error {
            req,
            code: "no_such_item".into(),
            message: "No item a.".into(),
        })
        .unwrap();
    assert_eq!(
        pending.await.unwrap(),
        Err(ResolveError::Refused {
            code: "no_such_item".into(),
            message: "No item a.".into()
        })
    );
}

/// A valid `publish` is acknowledged with its `req` and reaches the router.
pub async fn a_publish_is_acked_and_routed<S: SupervisorSubject>() {
    let world = world::<S>();
    let mut brick = world.subscribed("a").await;
    publish(&mut brick, 7, "a", probe::TYPE, br#"{"n":1}"#).await;
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Ack { req: RequestId(7) }
    );
    let last = world.router.last(A, &qualified("a")).unwrap().unwrap();
    assert_eq!(last.data_type, probe::TYPE);
    assert_eq!(last.data, br#"{"n":1}"#);
}

async fn refused_publish<S: SupervisorSubject>(
    id: &str,
    data_type: &str,
    detail: &[u8],
    code: PublishErrorCode,
) {
    let world = world::<S>();
    let mut brick = world.subscribed("a").await;
    publish(&mut brick, 9, id, data_type, detail).await;
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Error {
            req: RequestId(9),
            code
        }
    );
    assert_eq!(world.router.last(A, &qualified("a")), Ok(None));
}

/// A detail that is not JSON is refused with `invalid_json`.
pub async fn a_publish_of_invalid_json_is_refused<S: SupervisorSubject>() {
    refused_publish::<S>(
        "a",
        probe::TYPE,
        b"{not json",
        PublishErrorCode::InvalidJson,
    )
    .await;
}

/// A flow never assigned is refused with `unknown_flow`.
pub async fn a_publish_on_an_unknown_flow_is_refused<S: SupervisorSubject>() {
    refused_publish::<S>("never", probe::TYPE, b"{}", PublishErrorCode::UnknownFlow).await;
}

/// A type missing from the declaration is refused with `undeclared_type`.
pub async fn a_publish_of_an_undeclared_type_is_refused<S: SupervisorSubject>() {
    refused_publish::<S>(
        "a",
        "probe.other/v1",
        b"{}",
        PublishErrorCode::UndeclaredType,
    )
    .await;
}

/// A released flow is unknown again: publishing on it is refused.
pub async fn a_publish_on_a_released_flow_is_refused<S: SupervisorSubject>() {
    let world = world::<S>();
    let mut brick = world.subscribed("a").await;
    // Another anchor keeps the brick busy on flow b.
    let pending = world.resolve("b");
    serve(&mut brick, "b").await;
    pending.await.unwrap().unwrap();
    world.router.subscribe(AnchorId(2), qualified("b"));
    world.router.remove_anchor(A);
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Release { flow: flow("a") }
    );
    publish(&mut brick, 11, "a", probe::TYPE, b"{}").await;
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Error {
            req: RequestId(11),
            code: PublishErrorCode::UnknownFlow
        }
    );
}

/// A brick's `flow-error` reaches the flow's clients.
pub async fn flow_errors_are_relayed<S: SupervisorSubject>() {
    let world = world::<S>();
    let brick = world.subscribed("a").await;
    let mut stream = world.router.stream(A, None);
    brick
        .to_service
        .send(BrickMessage::FlowError {
            flow: flow("a"),
            message: "source down".into(),
        })
        .unwrap();
    assert_eq!(
        timeout(LONG, stream.recv()).await.unwrap(),
        Some(StreamEvent::FlowError {
            flow: qualified("a"),
            message: "source down".into()
        })
    );
}

/// A refresh is relayed to the brick with its own flow id; an unknown flow is
/// refused.
pub async fn refresh_is_relayed<S: SupervisorSubject>() {
    let world = world::<S>();
    let mut brick = world.subscribed("a").await;
    world.supervisor.refresh(&qualified("a")).unwrap();
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Refresh { flow: flow("a") }
    );
    assert_eq!(
        world.supervisor.refresh(&qualified("never")),
        Err(UnknownFlow(qualified("never")))
    );
}

/// When a flow loses its last subscriber, the brick receives `release`.
pub async fn an_empty_flow_is_released<S: SupervisorSubject>() {
    let world = world::<S>();
    let mut brick = world.subscribed("a").await;
    world.router.remove_anchor(A);
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Release { flow: flow("a") }
    );
}

/// An idle brick is kept for the grace period: a subscription within it reuses
/// the brick.
pub async fn an_idle_brick_is_kept_for_the_grace_period<S: SupervisorSubject>() {
    let world = world::<S>();
    let mut brick = world.subscribed("a").await;
    world.router.remove_anchor(A);
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Release { flow: flow("a") }
    );
    sleep(GRACE - Duration::from_secs(1)).await;
    let pending = world.resolve("b");
    serve(&mut brick, "b").await;
    assert_eq!(pending.await.unwrap(), Ok(qualified("b")));
    assert!(world.launcher.try_next().is_none());
}

/// Once no flow has a subscriber for the grace period, the brick receives
/// `shutdown`.
pub async fn an_idle_brick_shuts_down_after_the_grace_period<S: SupervisorSubject>() {
    let world = world::<S>();
    let mut brick = world.subscribed("a").await;
    let idle = Instant::now();
    world.router.remove_anchor(A);
    loop {
        match recv(&mut brick).await {
            ServiceMessage::Release { .. } => {}
            ServiceMessage::Shutdown => break,
            other => panic!("expected shutdown, got {other:?}"),
        }
    }
    assert!(idle.elapsed() >= GRACE, "shut down before the grace period");
}

/// A brick that does not exit after `shutdown` gets `SIGTERM`, then `SIGKILL`.
pub async fn a_brick_ignoring_shutdown_is_terminated_then_killed<S: SupervisorSubject>() {
    let world = world::<S>();
    let mut brick = world.subscribed("a").await;
    world.router.remove_anchor(A);
    while recv(&mut brick).await != ServiceMessage::Shutdown {}
    let signal = async |brick: &mut FakeBrick| timeout(LONG, brick.signals.recv()).await;
    assert_eq!(signal(&mut brick).await, Ok(Some(Signal::Term)));
    assert_eq!(signal(&mut brick).await, Ok(Some(Signal::Kill)));
}

/// On a crash, each flow of the brick gets `flow-error`; while subscribers
/// remain, the brick is relaunched and every live flow subscribed again.
pub async fn a_crashed_brick_is_relaunched<S: SupervisorSubject>() {
    let world = world::<S>();
    let brick = world.subscribed("a").await;
    let mut stream = world.router.stream(A, None);
    drop(brick);
    match timeout(LONG, stream.recv()).await.unwrap() {
        Some(StreamEvent::FlowError { flow, .. }) => assert_eq!(flow, qualified("a")),
        other => panic!("expected flow-error, got {other:?}"),
    }
    let mut brick = world.ready_brick().await;
    serve(&mut brick, "a").await;
    publish(&mut brick, 1, "a", probe::TYPE, b"{}").await;
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Ack { req: RequestId(1) }
    );
}

/// A brick that crashes with no subscriber left is not relaunched.
pub async fn a_crash_without_subscribers_is_not_relaunched<S: SupervisorSubject>() {
    let world = world::<S>();
    let mut brick = world.subscribed("a").await;
    world.router.remove_anchor(A);
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Release { flow: flow("a") }
    );
    drop(brick);
    assert!(
        timeout(LONG, world.launcher.next()).await.is_err(),
        "relaunched with nobody subscribed"
    );
}

/// A brick removed while it runs stops as at the loss of its last subscriber,
/// and its flows receive `flow-ended`.
pub async fn a_removed_brick_stops_and_its_flows_end<S: SupervisorSubject>() {
    let world = world::<S>();
    let mut brick = world.subscribed("a").await;
    let mut stream = world.router.stream(A, None);
    world.discovery.remove(&probe::name());
    assert_eq!(
        timeout(LONG, stream.recv()).await.unwrap(),
        Some(StreamEvent::FlowEnded {
            flow: qualified("a")
        })
    );
    stopped(&mut brick).await;
}

/// A brick discovery does not hold cannot be subscribed to.
pub async fn an_unknown_brick_is_refused<S: SupervisorSubject>() {
    let world = world::<S>();
    let nope = BrickName::new("nope").unwrap();
    assert_eq!(
        world.supervisor.resolve(&nope, "item", b"{}").await,
        Err(ResolveError::UnknownBrick(nope))
    );
}

/// Runs the service-side rules of contract section 4 against a supervisor.
#[macro_export]
macro_rules! supervisor_suite {
    ($subject:ty) => {
        mod supervisor_suite {
            #[allow(unused_imports)]
            use super::*;
            $crate::__tests!(
                block_on_paused,
                supervisor,
                $subject,
                [
                    the_first_subscription_launches_the_brick,
                    subscribe_maps_to_a_prefixed_flow,
                    a_running_brick_is_reused,
                    a_brick_missing_ready_is_unavailable,
                    a_refused_subscription_fails,
                    a_publish_is_acked_and_routed,
                    a_publish_of_invalid_json_is_refused,
                    a_publish_on_an_unknown_flow_is_refused,
                    a_publish_of_an_undeclared_type_is_refused,
                    a_publish_on_a_released_flow_is_refused,
                    flow_errors_are_relayed,
                    refresh_is_relayed,
                    an_empty_flow_is_released,
                    an_idle_brick_is_kept_for_the_grace_period,
                    an_idle_brick_shuts_down_after_the_grace_period,
                    a_brick_ignoring_shutdown_is_terminated_then_killed,
                    a_crashed_brick_is_relaunched,
                    a_crash_without_subscribers_is_not_relaunched,
                    a_removed_brick_stops_and_its_flows_end,
                    an_unknown_brick_is_refused,
                ]
            );
        }
    };
}
