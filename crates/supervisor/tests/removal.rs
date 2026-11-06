//! A brick removed from discovery while a subscription to it is being
//! resolved: the subscription answers `unknown_brick`, whichever comes first.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nowcast_conformance::probe;
use nowcast_supervisor::{BrickSupervisor, Config};
use nowcast_traits::fake::{FakeBrick, FakeDiscovery, FakeLauncher, FakeRouter};
use nowcast_traits::{
    AnchorId, BrickDeclaration, ClientKind, Discovery, DiscoveryChange, ResolveError, Router,
    Supervisor,
};
use nowcast_types::{BrickMessage, BrickName, FlowId, QualifiedFlowId, ServiceMessage};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const A: AnchorId = AnchorId(1);
const B: AnchorId = AnchorId(2);
const WAIT: Duration = Duration::from_secs(10);

/// Discovery whose next `brick` call, once armed, finds the brick and then
/// removes it, giving the supervisor up to 500 ms to handle the removal (seen
/// as the end of flow `a`) before the caller goes on with what it found.
struct RacingDiscovery {
    inner: FakeDiscovery,
    router: Arc<FakeRouter>,
    armed: AtomicBool,
}

impl Discovery for RacingDiscovery {
    fn brick(&self, name: &BrickName) -> Option<Arc<BrickDeclaration>> {
        let found = self.inner.brick(name);
        if self.armed.swap(false, Ordering::SeqCst) {
            // From another thread, so that any worker can pick up the
            // removal while this one waits.
            std::thread::scope(|s| {
                s.spawn(|| self.inner.remove(name));
            });
            let deadline = std::time::Instant::now() + Duration::from_millis(500);
            while std::time::Instant::now() < deadline
                && self.router.last(A, &qualified("a")).is_ok()
            {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        found
    }

    fn bricks(&self) -> Vec<Arc<BrickDeclaration>> {
        self.inner.bricks()
    }

    fn client_kinds(&self) -> Vec<ClientKind> {
        self.inner.client_kinds()
    }

    fn changes(&self) -> broadcast::Receiver<DiscoveryChange> {
        self.inner.changes()
    }
}

struct World {
    supervisor: Arc<BrickSupervisor<RacingDiscovery, FakeLauncher, FakeRouter>>,
    discovery: Arc<RacingDiscovery>,
    launcher: Arc<FakeLauncher>,
}

fn world() -> World {
    let router = Arc::new(FakeRouter::new());
    let inner = FakeDiscovery::new();
    inner.insert(probe::declaration(
        vec!["probe".into()],
        std::env::temp_dir(),
    ));
    let discovery = Arc::new(RacingDiscovery {
        inner,
        router: router.clone(),
        armed: AtomicBool::new(false),
    });
    let launcher = Arc::new(FakeLauncher::new());
    let supervisor = Arc::new(BrickSupervisor::start(
        discovery.clone(),
        launcher.clone(),
        router,
        Config::default(),
    ));
    World {
        supervisor,
        discovery,
        launcher,
    }
}

fn flow(id: &str) -> FlowId {
    FlowId::new(id).unwrap()
}

fn qualified(id: &str) -> QualifiedFlowId {
    QualifiedFlowId::new(probe::name(), flow(id))
}

impl World {
    fn resolve(
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
}

async fn recv(brick: &mut FakeBrick) -> ServiceMessage {
    timeout(WAIT, brick.from_service.recv())
        .await
        .expect("a message from the service")
        .expect("the service holds the channel")
}

/// Answers the next `subscribe`, skipping what comes before it.
async fn serve(brick: &mut FakeBrick, id: &str) {
    loop {
        if let ServiceMessage::Subscribe { req, .. } = recv(brick).await {
            let subscribed = BrickMessage::Subscribed {
                req,
                flow: flow(id),
            };
            brick.to_service.send(subscribed).unwrap();
            return;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_brick_removed_during_a_subscription_takes_none() {
    let world = world();
    let first = world.resolve(A, "a");
    let mut brick = timeout(WAIT, world.launcher.next()).await.unwrap();
    assert!(matches!(
        recv(&mut brick).await,
        ServiceMessage::Hello { .. }
    ));
    brick.to_service.send(BrickMessage::Ready).unwrap();
    serve(&mut brick, "a").await;
    assert_eq!(first.await.unwrap(), Ok(qualified("a")));

    world.discovery.armed.store(true, Ordering::SeqCst);
    let second = world.resolve(B, "b");
    serve(&mut brick, "b").await;
    assert_eq!(
        timeout(WAIT, second).await.unwrap().unwrap(),
        Err(ResolveError::UnknownBrick(probe::name()))
    );
}

#[tokio::test(start_paused = true)]
async fn a_brick_removed_before_its_launch_is_unknown() {
    let world = world();
    world.discovery.armed.store(true, Ordering::SeqCst);
    assert_eq!(
        world.resolve(A, "a").await.unwrap(),
        Err(ResolveError::UnknownBrick(probe::name()))
    );
    assert!(world.launcher.try_next().is_none());
}
