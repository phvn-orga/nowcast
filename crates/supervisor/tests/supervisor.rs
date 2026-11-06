//! Behaviour beyond the conformance suite, on the fakes and a paused clock:
//! races between subscriptions and the brick's life, crashes in a row,
//! misbehaving bricks.

use std::sync::Arc;
use std::time::Duration;

use nowcast_conformance::probe;
use nowcast_supervisor::{BrickSupervisor, Config};
use nowcast_traits::fake::{FakeBrick, FakeDiscovery, FakeLauncher, FakeRouter};
use nowcast_traits::{AnchorId, BrickUnavailable, ResolveError, Router, Supervisor, UnknownFlow};
use nowcast_types::{
    BrickMessage, BrickName, FlowId, QualifiedFlowId, RequestId, ServiceMessage, StreamEvent,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};

type Subject = BrickSupervisor<FakeDiscovery, FakeLauncher, FakeRouter>;

const A: AnchorId = AnchorId(1);
const B: AnchorId = AnchorId(2);
/// Longer than any delay of the supervisor.
const LONG: Duration = Duration::from_secs(3600);

struct World {
    supervisor: Arc<Subject>,
    discovery: Arc<FakeDiscovery>,
    launcher: Arc<FakeLauncher>,
    router: Arc<FakeRouter>,
    config: Config,
}

fn world() -> World {
    world_with(Config::default())
}

fn world_with(config: Config) -> World {
    let discovery = Arc::new(FakeDiscovery::new());
    discovery.insert(probe::declaration(
        vec!["probe".into()],
        std::env::temp_dir(),
    ));
    let launcher = Arc::new(FakeLauncher::new());
    let router = Arc::new(FakeRouter::new());
    let supervisor = Arc::new(BrickSupervisor::start(
        discovery.clone(),
        launcher.clone(),
        router.clone(),
        config.clone(),
    ));
    World {
        supervisor,
        discovery,
        launcher,
        router,
        config,
    }
}

fn flow(id: &str) -> FlowId {
    FlowId::new(id).unwrap()
}

fn qualified(id: &str) -> QualifiedFlowId {
    QualifiedFlowId::new(probe::name(), flow(id))
}

fn unavailable() -> ResolveError {
    ResolveError::Unavailable(BrickUnavailable(probe::name()))
}

impl World {
    /// Subscribes anchor `A` to the probe's flow `id`; gives the flow.
    fn resolve(&self, id: &str) -> JoinHandle<Result<QualifiedFlowId, ResolveError>> {
        self.resolve_as(A, id)
    }

    fn resolve_as(
        &self,
        anchor: AnchorId,
        id: &str,
    ) -> JoinHandle<Result<QualifiedFlowId, ResolveError>> {
        let supervisor = self.supervisor.clone();
        let params = probe::params(id);
        tokio::spawn(async move {
            let resolved = supervisor
                .resolve(anchor, &probe::name(), probe::KIND, &params)
                .await;
            resolved.map(|r| r.flow)
        })
    }

    /// Takes the next launch and its `hello`.
    async fn launch(&self) -> FakeBrick {
        let mut brick = timeout(LONG, self.launcher.next()).await.expect("a launch");
        assert!(matches!(
            recv(&mut brick).await,
            ServiceMessage::Hello { .. }
        ));
        brick
    }

    /// Takes the next launch and answers its `hello`.
    async fn ready(&self) -> FakeBrick {
        let brick = self.launch().await;
        brick.to_service.send(BrickMessage::Ready).unwrap();
        brick
    }

    /// Subscribes anchor `A` to `id` on a fresh brick.
    async fn subscribed(&self, id: &str) -> FakeBrick {
        let pending = self.resolve(id);
        let mut brick = self.ready().await;
        serve(&mut brick, id).await;
        assert_eq!(pending.await.unwrap(), Ok(qualified(id)));
        brick
    }

    async fn no_launch_within(&self, within: Duration) {
        assert!(
            timeout(within, self.launcher.next()).await.is_err(),
            "launched again"
        );
    }
}

/// Lets every task run until all wait.
async fn settle() {
    sleep(Duration::from_millis(1)).await;
}

async fn recv(brick: &mut FakeBrick) -> ServiceMessage {
    timeout(LONG, brick.from_service.recv())
        .await
        .expect("a message from the service")
        .expect("the service holds the channel")
}

fn assert_quiet(brick: &mut FakeBrick) {
    assert_eq!(brick.from_service.try_recv(), Err(TryRecvError::Empty));
}

/// Takes the next `subscribe`, expected for `id`, and returns its `req`.
async fn subscribe_req(brick: &mut FakeBrick, id: &str) -> RequestId {
    match recv(brick).await {
        ServiceMessage::Subscribe { req, kind, params } => {
            assert_eq!(kind, probe::KIND);
            assert_eq!(params, probe::params(id));
            req
        }
        other => panic!("expected subscribe, got {other:?}"),
    }
}

/// Answers the next `subscribe`, expected for `id`, with flow `id`.
async fn serve(brick: &mut FakeBrick, id: &str) {
    let req = subscribe_req(brick, id).await;
    answer(brick, req, id);
}

fn answer(brick: &FakeBrick, req: RequestId, id: &str) {
    brick
        .to_service
        .send(BrickMessage::Subscribed {
            req,
            flow: flow(id),
        })
        .unwrap();
}

/// Answers the next `subscribe` with the flow named by its params, and
/// returns that name.
async fn serve_any(brick: &mut FakeBrick) -> String {
    let ServiceMessage::Subscribe { req, params, .. } = recv(brick).await else {
        panic!("expected subscribe");
    };
    let params: serde_json::Value = serde_json::from_slice(&params).unwrap();
    let id = params["id"].as_str().unwrap().to_owned();
    answer(brick, req, &id);
    id
}

async fn publish_acked(brick: &mut FakeBrick, req: u64, id: &str) {
    brick
        .to_service
        .send(BrickMessage::Publish {
            req: RequestId(req),
            flow: flow(id),
            data_type: probe::TYPE.into(),
            detail: b"{}".to_vec(),
        })
        .unwrap();
    assert_eq!(
        recv(brick).await,
        ServiceMessage::Ack {
            req: RequestId(req)
        }
    );
}

async fn next_event(stream: &mut UnboundedReceiver<StreamEvent>) -> StreamEvent {
    timeout(LONG, stream.recv())
        .await
        .expect("a stream event")
        .expect("the stream stays open")
}

async fn flow_error(stream: &mut UnboundedReceiver<StreamEvent>, id: &str) -> String {
    match next_event(stream).await {
        StreamEvent::FlowError { flow, message } if flow == qualified(id) => message,
        other => panic!("expected flow-error on {id}, got {other:?}"),
    }
}

async fn flow_ended(stream: &mut UnboundedReceiver<StreamEvent>, id: &str) {
    assert_eq!(
        next_event(stream).await,
        StreamEvent::FlowEnded {
            flow: qualified(id)
        }
    );
}

async fn shutdown(brick: &mut FakeBrick) {
    loop {
        match recv(brick).await {
            ServiceMessage::Shutdown => return,
            ServiceMessage::Release { .. } => {}
            other => panic!("expected shutdown, got {other:?}"),
        }
    }
}

#[tokio::test(start_paused = true)]
async fn concurrent_first_subscriptions_launch_the_brick_once() {
    let world = world();
    let a = world.resolve("a");
    let b = world.resolve("b");
    let mut brick = world.ready().await;
    let mut served = vec![serve_any(&mut brick).await, serve_any(&mut brick).await];
    served.sort();
    assert_eq!(served, ["a", "b"]);
    assert_eq!(a.await.unwrap(), Ok(qualified("a")));
    assert_eq!(b.await.unwrap(), Ok(qualified("b")));
    world.no_launch_within(LONG).await;
}

#[tokio::test(start_paused = true)]
async fn a_subscription_during_the_grace_period_cancels_the_stop() {
    let world = world();
    let mut brick = world.subscribed("a").await;
    world.router.remove_anchor(A);
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Release { flow: flow("a") }
    );
    sleep(world.config.grace / 2).await;
    let b = world.resolve_as(B, "b");
    serve(&mut brick, "b").await;
    assert_eq!(b.await.unwrap(), Ok(qualified("b")));
    sleep(world.config.grace * 3).await;
    assert_quiet(&mut brick);
    assert_eq!(brick.signals.try_recv(), Err(TryRecvError::Empty));
}

#[tokio::test(start_paused = true)]
async fn a_subscription_while_the_brick_stops_relaunches_it_once_it_exits() {
    let world = world();
    let mut brick = world.subscribed("a").await;
    world.router.remove_anchor(A);
    shutdown(&mut brick).await;
    let b = world.resolve("b");
    // The stopping brick is not reused, and not doubled.
    settle().await;
    assert_quiet(&mut brick);
    assert!(world.launcher.try_next().is_none());
    drop(brick);
    let mut brick = world.ready().await;
    serve(&mut brick, "b").await;
    assert_eq!(b.await.unwrap(), Ok(qualified("b")));
}

#[tokio::test(start_paused = true)]
async fn a_crashed_brick_whose_subscribers_leave_during_the_backoff_is_not_relaunched() {
    let world = world();
    let brick = world.subscribed("a").await;
    let mut stream = world.router.stream(A, None);
    drop(brick);
    flow_error(&mut stream, "a").await;
    world.router.remove_anchor(A);
    world.no_launch_within(LONG).await;
}

#[tokio::test(start_paused = true)]
async fn a_subscription_during_the_backoff_fails_at_once() {
    let world = world();
    let brick = world.subscribed("a").await;
    let mut stream = world.router.stream(A, None);
    drop(brick);
    flow_error(&mut stream, "a").await;
    let start = Instant::now();
    assert_eq!(
        world
            .supervisor
            .resolve(A, &probe::name(), probe::KIND, &probe::params("b"))
            .await,
        Err(unavailable())
    );
    assert_eq!(start.elapsed(), Duration::ZERO);
    // The flow that has subscribers is still relaunched.
    let mut brick = world.ready().await;
    serve(&mut brick, "a").await;
    publish_acked(&mut brick, 1, "a").await;
}

#[tokio::test(start_paused = true)]
async fn a_subscription_while_the_brick_relaunches_waits_for_ready() {
    let world = world();
    let brick = world.subscribed("a").await;
    drop(brick);
    let mut brick = world.launch().await;
    let b = world.resolve("b");
    settle().await;
    assert_quiet(&mut brick);
    brick.to_service.send(BrickMessage::Ready).unwrap();
    // Live flows are subscribed again first.
    serve(&mut brick, "a").await;
    serve(&mut brick, "b").await;
    assert_eq!(b.await.unwrap(), Ok(qualified("b")));
}

#[tokio::test(start_paused = true)]
async fn every_live_flow_gets_flow_error_and_is_subscribed_again_after_a_crash() {
    let world = world();
    let mut brick = world.subscribed("a").await;
    let b = world.resolve("b");
    serve(&mut brick, "b").await;
    b.await.unwrap().unwrap();
    let mut stream = world.router.stream(A, None);
    drop(brick);
    let mut errors = [next_event(&mut stream).await, next_event(&mut stream).await];
    errors.sort_by_key(|e| format!("{e:?}"));
    assert!(matches!(&errors[0], StreamEvent::FlowError { flow, .. } if *flow == qualified("a")));
    assert!(matches!(&errors[1], StreamEvent::FlowError { flow, .. } if *flow == qualified("b")));
    let mut brick = world.ready().await;
    let mut served = vec![serve_any(&mut brick).await, serve_any(&mut brick).await];
    served.sort();
    assert_eq!(served, ["a", "b"]);
    publish_acked(&mut brick, 1, "b").await;
    match next_event(&mut stream).await {
        StreamEvent::Update(event) => assert_eq!(event.flow, qualified("b")),
        other => panic!("expected an update, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn a_brick_exiting_right_after_ready_fails_the_subscription() {
    let world = world();
    let a = world.resolve("a");
    let brick = world.launch().await;
    brick.to_service.send(BrickMessage::Ready).unwrap();
    drop(brick);
    assert_eq!(a.await.unwrap(), Err(unavailable()));
    // Nobody holds a flow: no relaunch.
    world.no_launch_within(LONG).await;
}

#[tokio::test(start_paused = true)]
async fn crashes_in_a_row_back_off_longer_then_give_up() {
    let config = Config {
        max_restarts: 3,
        ..Config::default()
    };
    let world = world_with(config);
    let mut brick = world.subscribed("a").await;
    let mut stream = world.router.stream(A, None);
    let mut waits = Vec::new();
    for _ in 0..3 {
        drop(brick);
        let crashed = Instant::now();
        flow_error(&mut stream, "a").await;
        brick = world.ready().await;
        waits.push(crashed.elapsed());
        serve(&mut brick, "a").await;
    }
    let secs = Duration::from_secs;
    assert_eq!(waits, [secs(1), secs(2), secs(4)]);
    drop(brick);
    flow_error(&mut stream, "a").await;
    let message = flow_error(&mut stream, "a").await;
    assert!(message.contains("giving up"), "{message}");
    flow_ended(&mut stream, "a").await;
    world.no_launch_within(LONG).await;
    // A later subscription starts afresh.
    let b = world.resolve("b");
    let mut brick = world.ready().await;
    serve(&mut brick, "b").await;
    assert_eq!(b.await.unwrap(), Ok(qualified("b")));
}

#[tokio::test(start_paused = true)]
async fn a_stable_run_resets_the_backoff() {
    let world = world();
    let brick = world.subscribed("a").await;
    drop(brick);
    let mut brick = world.ready().await;
    serve(&mut brick, "a").await;
    sleep(world.config.stable_after).await;
    drop(brick);
    let crashed = Instant::now();
    let mut brick = world.ready().await;
    assert_eq!(crashed.elapsed(), world.config.first_backoff);
    serve(&mut brick, "a").await;
}

#[tokio::test(start_paused = true)]
async fn a_relaunch_missing_ready_counts_as_a_crash() {
    let world = world();
    let brick = world.subscribed("a").await;
    drop(brick);
    let mut relaunched = world.launch().await;
    shutdown(&mut relaunched).await;
    drop(relaunched);
    let stopped = Instant::now();
    let mut brick = world.ready().await;
    assert_eq!(stopped.elapsed(), world.config.first_backoff * 2);
    serve(&mut brick, "a").await;
}

#[tokio::test(start_paused = true)]
async fn answers_to_a_req_never_sent_are_ignored() {
    let world = world();
    let mut brick = world.subscribed("a").await;
    brick
        .to_service
        .send(BrickMessage::Subscribed {
            req: RequestId(999),
            flow: flow("x"),
        })
        .unwrap();
    // Nobody wants the flow the brick now serves.
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Release { flow: flow("x") }
    );
    brick
        .to_service
        .send(BrickMessage::Error {
            req: RequestId(998),
            code: "nope".into(),
            message: "nope".into(),
        })
        .unwrap();
    settle().await;
    assert_quiet(&mut brick);
    publish_acked(&mut brick, 1, "a").await;
}

#[tokio::test(start_paused = true)]
async fn a_late_answer_does_not_release_a_flow_subscribed_again() {
    let world = world();
    let first = world.resolve("a");
    let mut brick = world.ready().await;
    let req1 = subscribe_req(&mut brick, "a").await;
    // The brick answers too late: the subscription fails, and is retried.
    assert_eq!(first.await.unwrap(), Err(unavailable()));
    let second = world.resolve_as(B, "a");
    let req2 = subscribe_req(&mut brick, "a").await;
    answer(&brick, req1, "a");
    answer(&brick, req2, "a");
    assert_eq!(second.await.unwrap(), Ok(qualified("a")));
    // Anything sent since the retry, `release` included, comes first.
    publish_acked(&mut brick, 1, "a").await;
    assert_eq!(world.router.subscribers(&qualified("a")), 1);
}

#[tokio::test(start_paused = true)]
async fn a_refresh_on_an_unknown_flow_is_refused() {
    let world = world();
    let unknown = |f: QualifiedFlowId| Err(UnknownFlow(f));
    // No brick running.
    assert_eq!(
        world.supervisor.refresh(&qualified("a")),
        unknown(qualified("a"))
    );
    let mut brick = world.subscribed("a").await;
    // Another brick's flow, a flow never assigned.
    let other = QualifiedFlowId::new(BrickName::new("other").unwrap(), flow("a"));
    assert_eq!(world.supervisor.refresh(&other), unknown(other.clone()));
    assert_eq!(
        world.supervisor.refresh(&qualified("b")),
        unknown(qualified("b"))
    );
    // A released flow.
    world.router.remove_anchor(A);
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Release { flow: flow("a") }
    );
    assert_eq!(
        world.supervisor.refresh(&qualified("a")),
        unknown(qualified("a"))
    );
    assert_quiet(&mut brick);
}

#[tokio::test(start_paused = true)]
async fn a_subscription_given_up_by_its_client_releases_its_flow() {
    let world = world();
    let pending = world.resolve("a");
    let mut brick = world.ready().await;
    let req = subscribe_req(&mut brick, "a").await;
    pending.abort();
    settle().await;
    answer(&brick, req, "a");
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Release { flow: flow("a") }
    );
    let idle = Instant::now();
    shutdown(&mut brick).await;
    assert_eq!(idle.elapsed(), world.config.grace);
}

#[tokio::test(start_paused = true)]
async fn a_resolution_dropped_before_its_answer_is_read_undoes_the_subscription() {
    let world = world();
    let (name, params) = (probe::name(), probe::params("a"));
    // Never polled: the subscription is made, then dropped unread.
    let resolution = world.supervisor.resolve(A, &name, probe::KIND, &params);
    let mut brick = world.ready().await;
    serve(&mut brick, "a").await;
    settle().await;
    assert_eq!(world.router.subscribers(&qualified("a")), 1);
    drop(resolution);
    assert_eq!(world.router.subscribers(&qualified("a")), 0);
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Release { flow: flow("a") }
    );
}

#[tokio::test(start_paused = true)]
async fn a_flow_emptied_while_a_subscription_hands_it_out_again_is_kept() {
    let world = world();
    let mut brick = world.subscribed("a").await;
    let again = world.resolve_as(B, "a");
    let req = subscribe_req(&mut brick, "a").await;
    world.router.remove_anchor(A);
    settle().await;
    assert_quiet(&mut brick);
    answer(&brick, req, "a");
    assert_eq!(again.await.unwrap(), Ok(qualified("a")));
    sleep(world.config.grace * 3).await;
    assert_quiet(&mut brick);
    publish_acked(&mut brick, 1, "a").await;
}

#[tokio::test(start_paused = true)]
async fn a_flow_emptied_during_another_subscription_is_released_after_it() {
    let world = world();
    let mut brick = world.subscribed("a").await;
    let b = world.resolve("b");
    let req = subscribe_req(&mut brick, "b").await;
    world.router.remove_anchor(A);
    settle().await;
    assert_quiet(&mut brick);
    answer(&brick, req, "b");
    assert_eq!(b.await.unwrap(), Ok(qualified("b")));
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Release { flow: flow("a") }
    );
}

#[tokio::test(start_paused = true)]
async fn a_flow_the_relaunched_brick_refuses_ends() {
    let world = world();
    let brick = world.subscribed("a").await;
    let mut stream = world.router.stream(A, None);
    drop(brick);
    flow_error(&mut stream, "a").await;
    let mut brick = world.ready().await;
    let req = subscribe_req(&mut brick, "a").await;
    brick
        .to_service
        .send(BrickMessage::Error {
            req,
            code: "no_such_item".into(),
            message: "gone".into(),
        })
        .unwrap();
    let message = flow_error(&mut stream, "a").await;
    assert!(message.contains("no_such_item: gone"), "{message}");
    flow_ended(&mut stream, "a").await;
    // Nothing is left to serve.
    shutdown(&mut brick).await;
}

#[tokio::test(start_paused = true)]
async fn a_flow_the_relaunched_brick_renames_ends() {
    let world = world();
    let brick = world.subscribed("a").await;
    let mut stream = world.router.stream(A, None);
    drop(brick);
    flow_error(&mut stream, "a").await;
    let mut brick = world.ready().await;
    let req = subscribe_req(&mut brick, "a").await;
    answer(&brick, req, "renamed");
    assert_eq!(
        recv(&mut brick).await,
        ServiceMessage::Release {
            flow: flow("renamed")
        }
    );
    flow_error(&mut stream, "a").await;
    flow_ended(&mut stream, "a").await;
}

#[tokio::test(start_paused = true)]
async fn a_subscription_the_brick_never_answers_fails_as_unavailable() {
    let world = world();
    let pending = world.resolve("a");
    let mut brick = world.ready().await;
    subscribe_req(&mut brick, "a").await;
    let start = Instant::now();
    assert_eq!(pending.await.unwrap(), Err(unavailable()));
    assert_eq!(start.elapsed(), world.config.answer_timeout);
    // Nothing is left to serve.
    shutdown(&mut brick).await;
}

#[tokio::test(start_paused = true)]
async fn a_removed_brick_fails_the_subscriptions_waiting_on_it() {
    let world = world();
    let pending = world.resolve("a");
    let _brick = world.launch().await;
    world.discovery.remove(&probe::name());
    assert_eq!(
        pending.await.unwrap(),
        Err(ResolveError::UnknownBrick(probe::name()))
    );
}

#[tokio::test(start_paused = true)]
async fn a_brick_that_exits_after_shutdown_gets_no_signal() {
    let world = world();
    let mut brick = world.subscribed("a").await;
    world.router.remove_anchor(A);
    shutdown(&mut brick).await;
    // The brick exits: its output closes, its signals stay observable.
    let FakeBrick {
        to_service,
        mut signals,
        ..
    } = brick;
    drop(to_service);
    sleep(world.config.terminate_after + world.config.kill_after + LONG).await;
    assert_eq!(signals.try_recv(), Err(TryRecvError::Disconnected));
}
