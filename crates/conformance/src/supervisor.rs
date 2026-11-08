//! Contract section 4, the service's side of the brick protocol and the brick
//! lifecycle, plus the end of a removed brick from section 2. Checked on a
//! [`Supervisor`] wired to the fakes, the probe declared. The supervisor makes
//! the router subscriptions itself, so the suite subscribes through
//! [`Supervisor::resolve`] only.
//!
//! The tests run on a paused clock: the supervisor must take its delays from
//! `tokio::time`, and the contract's 5 s and 30 s pass instantly.

use std::sync::Arc;
use std::time::Duration;

use nowcast_traits::fake::{FakeBrick, FakeDiscovery, FakeLauncher, FakeRouter, Signal};
use nowcast_traits::{
    AnchorId, BrickUnavailable, InputError, ResolveError, Resolved, Router, Supervisor, UnknownFlow,
};
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
    discovery.insert(probe::declaration(
        vec!["probe".into()],
        std::env::temp_dir(),
    ));
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
    /// Subscribes anchor `A` to the probe's flow `id`, as the server does.
    fn resolve(&self, id: &str) -> JoinHandle<Result<Resolved, ResolveError>> {
        self.resolve_as(A, id)
    }

    fn resolve_as(&self, anchor: AnchorId, id: &str) -> JoinHandle<Result<Resolved, ResolveError>> {
        let supervisor = self.supervisor.clone();
        let params = probe::params(id);
        tokio::spawn(async move {
            supervisor
                .resolve(anchor, &probe::name(), probe::KIND, &params)
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

    /// Subscribes anchor `A` to `id` on a fresh brick.
    async fn subscribed(&self, id: &str) -> FakeBrick {
        let pending = self.resolve(id);
        let mut brick = self.ready_brick().await;
        serve(&mut brick, id).await;
        assert_eq!(flow_of(pending).await, Ok(qualified(id)));
        brick
    }
}

/// The flow a pending resolution ends up on.
async fn flow_of(
    pending: JoinHandle<Result<Resolved, ResolveError>>,
) -> Result<QualifiedFlowId, ResolveError> {
    pending.await.unwrap().map(|resolved| resolved.flow)
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
/// brick answers is prefixed with its name, and the anchor is subscribed to it
/// in the router.
pub async fn subscribe_maps_to_a_prefixed_flow<S: SupervisorSubject>() {
    let world = world::<S>();
    let pending = world.resolve("a");
    let mut brick = world.ready_brick().await;
    serve(&mut brick, "a").await;
    let resolved = pending.await.unwrap().unwrap();
    assert_eq!(resolved.flow, qualified("a"));
    assert_eq!(resolved.last, None);
    assert_eq!(world.router.subscribers(&qualified("a")), 1);
    assert_eq!(world.router.unsubscribe(A, &resolved.subscription), Ok(()));
}

/// A running brick serves the next subscriptions: no second launch.
pub async fn a_running_brick_is_reused<S: SupervisorSubject>() {
    let world = world::<S>();
    let mut brick = world.subscribed("a").await;
    let pending = world.resolve("b");
    serve(&mut brick, "b").await;
    assert_eq!(flow_of(pending).await, Ok(qualified("b")));
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
    let result = flow_of(pending).await;
    assert_eq!(
        result,
        Err(ResolveError::Unavailable(
            nowcast_traits::BrickUnavailable::new(probe::name())
        ))
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
        flow_of(pending).await,
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
    let pending = world.resolve_as(AnchorId(2), "b");
    serve(&mut brick, "b").await;
    pending.await.unwrap().unwrap();
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

/// The supervisor knows the kind each flow was subscribed with, and relays an
/// input to the brick with its own flow id, in order; an unknown flow is
/// refused, and so is an input while the brick waits to be relaunched.
pub async fn input_is_relayed<S: SupervisorSubject>() {
    let world = world::<S>();
    let mut brick = world.subscribed("a").await;
    assert_eq!(
        world.supervisor.kind(&qualified("a")),
        Some(probe::KIND.into())
    );
    assert_eq!(world.supervisor.kind(&qualified("never")), None);
    for n in [b"1", b"2"] {
        world
            .supervisor
            .input(&qualified("a"), probe::INPUT_TYPE, n.to_vec())
            .unwrap();
    }
    for n in [b"1", b"2"] {
        assert_eq!(
            recv(&mut brick).await,
            ServiceMessage::Input {
                flow: flow("a"),
                data_type: probe::INPUT_TYPE.into(),
                detail: n.to_vec(),
            }
        );
    }
    assert_eq!(
        world
            .supervisor
            .input(&qualified("never"), probe::INPUT_TYPE, b"{}".to_vec()),
        Err(InputError::UnknownFlow(UnknownFlow(qualified("never"))))
    );
    let mut stream = world.router.stream(A, None);
    drop(brick);
    match timeout(LONG, stream.recv()).await.unwrap() {
        Some(StreamEvent::FlowError { flow, .. }) => assert_eq!(flow, qualified("a")),
        other => panic!("expected flow-error, got {other:?}"),
    }
    assert_eq!(
        world
            .supervisor
            .input(&qualified("a"), probe::INPUT_TYPE, b"{}".to_vec()),
        Err(InputError::Unavailable(
            BrickUnavailable::new(probe::name())
        ))
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
    assert_eq!(flow_of(pending).await, Ok(qualified("b")));
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

/// A value the brick publishes right after `subscribed` is not lost: it is
/// acknowledged, and is either the subscription's `last` or delivered to the
/// anchor's stream.
pub async fn a_value_published_right_after_subscribed_is_not_lost<S: SupervisorSubject>() {
    let world = world::<S>();
    let mut stream = world.router.stream(A, None);
    let pending = world.resolve("a");
    let mut brick = world.ready_brick().await;
    let ServiceMessage::Subscribe { req, .. } = recv(&mut brick).await else {
        panic!("expected subscribe");
    };
    // Both sent at once: the service reads them back to back.
    brick
        .to_service
        .send(BrickMessage::Subscribed {
            req,
            flow: flow("a"),
        })
        .unwrap();
    publish(&mut brick, 1, "a", probe::TYPE, br#"{"first":true}"#).await;
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Ack { req: RequestId(1) }
    );
    let resolved = pending.await.unwrap().unwrap();
    let value = match resolved.last {
        Some(event) => event,
        None => match timeout(LONG, stream.recv()).await {
            Ok(Some(StreamEvent::Update(event))) => event,
            other => panic!("expected the first value, got {other:?}"),
        },
    };
    assert_eq!(value.flow, qualified("a"));
    assert_eq!(value.data, br#"{"first":true}"#);
}

/// A subscription abandoned while the brick answers leaves no subscriber: the
/// flow is released.
pub async fn an_abandoned_subscription_leaves_no_subscriber<S: SupervisorSubject>() {
    let world = world::<S>();
    let pending = world.resolve("a");
    let mut brick = world.ready_brick().await;
    let ServiceMessage::Subscribe { req, .. } = recv(&mut brick).await else {
        panic!("expected subscribe");
    };
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    brick
        .to_service
        .send(BrickMessage::Subscribed {
            req,
            flow: flow("a"),
        })
        .unwrap();
    match recv(&mut brick).await {
        ServiceMessage::Release { flow: released } => assert_eq!(released, flow("a")),
        // Stopping the idle brick releases it as well.
        ServiceMessage::Shutdown => {}
        other => panic!("expected release, got {other:?}"),
    }
    assert_eq!(world.router.subscribers(&qualified("a")), 0);
}

/// A flow whose last subscriber leaves while another subscription to it is in
/// flight stays served: the brick never receives `release` for it.
pub async fn a_flow_left_while_subscribed_again_stays_served<S: SupervisorSubject>() {
    let world = world::<S>();
    let mut brick = world.subscribed("a").await;
    // Lets the first subscription settle.
    sleep(Duration::from_secs(10)).await;
    let pending = world.resolve_as(AnchorId(2), "a");
    let ServiceMessage::Subscribe { req, .. } = recv(&mut brick).await else {
        panic!("expected subscribe");
    };
    world.router.remove_anchor(A);
    sleep(Duration::from_secs(1)).await;
    brick
        .to_service
        .send(BrickMessage::Subscribed {
            req,
            flow: flow("a"),
        })
        .unwrap();
    assert_eq!(flow_of(pending).await, Ok(qualified("a")));
    let mut stream = world.router.stream(AnchorId(2), None);
    publish(&mut brick, 1, "a", probe::TYPE, b"{}").await;
    // Anything sent since the new subscription, `release` included, comes
    // before the answer to this publish.
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Ack { req: RequestId(1) }
    );
    assert!(matches!(
        timeout(LONG, stream.recv()).await,
        Ok(Some(StreamEvent::Update(_)))
    ));
    assert_eq!(world.router.subscribers(&qualified("a")), 1);
}

/// Of two subscriptions to the same flow in flight together, the first one
/// abandoned does not release the flow under the second: the brick never
/// receives `release` after the second `subscribe`, and the second subscriber
/// gets the flow's values.
pub async fn an_abandoned_subscription_does_not_release_a_flow_subscribed_again<
    S: SupervisorSubject,
>() {
    let world = world::<S>();
    let first = world.resolve_as(A, "a");
    let mut brick = world.ready_brick().await;
    let ServiceMessage::Subscribe { req: req1, .. } = recv(&mut brick).await else {
        panic!("expected subscribe");
    };
    let second = world.resolve_as(AnchorId(2), "a");
    let ServiceMessage::Subscribe { req: req2, .. } = recv(&mut brick).await else {
        panic!("expected subscribe");
    };
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    for req in [req1, req2] {
        brick
            .to_service
            .send(BrickMessage::Subscribed {
                req,
                flow: flow("a"),
            })
            .unwrap();
    }
    assert_eq!(flow_of(second).await, Ok(qualified("a")));
    let mut stream = world.router.stream(AnchorId(2), None);
    publish(&mut brick, 1, "a", probe::TYPE, b"{}").await;
    // Anything sent since the second `subscribe`, `release` included, comes
    // before the answer to this publish.
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Ack { req: RequestId(1) }
    );
    assert!(matches!(
        timeout(LONG, stream.recv()).await,
        Ok(Some(StreamEvent::Update(_)))
    ));
    assert_eq!(world.router.subscribers(&qualified("a")), 1);
}

/// A brick discovery does not hold cannot be subscribed to.
pub async fn an_unknown_brick_is_refused<S: SupervisorSubject>() {
    let world = world::<S>();
    let nope = BrickName::new("nope").unwrap();
    assert_eq!(
        world.supervisor.resolve(A, &nope, "item", b"{}").await,
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
                    input_is_relayed,
                    an_empty_flow_is_released,
                    an_idle_brick_is_kept_for_the_grace_period,
                    an_idle_brick_shuts_down_after_the_grace_period,
                    a_brick_ignoring_shutdown_is_terminated_then_killed,
                    a_crashed_brick_is_relaunched,
                    a_crash_without_subscribers_is_not_relaunched,
                    a_removed_brick_stops_and_its_flows_end,
                    a_value_published_right_after_subscribed_is_not_lost,
                    an_abandoned_subscription_leaves_no_subscriber,
                    a_flow_left_while_subscribed_again_stays_served,
                    an_abandoned_subscription_does_not_release_a_flow_subscribed_again,
                    an_unknown_brick_is_refused,
                ]
            );
        }
    };
}
