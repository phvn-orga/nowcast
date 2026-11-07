//! Behaviour beyond the conformance suite, on the in-memory fakes: the
//! issue's acceptance criteria, the order of the cleanup, and the edge cases
//! of watching anchors.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use nowcast_sessions::SessionRegistry;
use nowcast_traits::fake::{FakeIdentity, FakeRouter};
use nowcast_traits::{AnchorId, History, Router, Sessions, UnknownFlow, UnknownSubscription};
use nowcast_types::{
    BrickName, Event, EventId, FlowId, QualifiedFlowId, SessionId, StreamEvent, SubscriptionId,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::timeout;

const A: AnchorId = AnchorId(1);
const B: AnchorId = AnchorId(2);
const C: AnchorId = AnchorId(3);

/// Far longer than the registry needs to react to a death.
const PATIENCE: Duration = Duration::from_secs(10);

fn flow(id: &str) -> QualifiedFlowId {
    QualifiedFlowId::new(BrickName::new("probe").unwrap(), FlowId::new(id).unwrap())
}

fn session(id: &str) -> Option<SessionId> {
    Some(SessionId::new(id))
}

/// A [`FakeRouter`] that reports each anchor removed, along with the session
/// the registry still answered for it at that moment.
struct Spy {
    router: FakeRouter,
    sessions: OnceLock<Weak<SessionRegistry>>,
    removed: UnboundedSender<(AnchorId, Option<SessionId>)>,
}

impl Router for Spy {
    fn subscribe(
        &self,
        anchor: AnchorId,
        flow: QualifiedFlowId,
    ) -> (SubscriptionId, Option<Event>) {
        self.router.subscribe(anchor, flow)
    }

    fn unsubscribe(
        &self,
        anchor: AnchorId,
        subscription: &SubscriptionId,
    ) -> Result<(), UnknownSubscription> {
        self.router.unsubscribe(anchor, subscription)
    }

    fn publish(
        &self,
        flow: &QualifiedFlowId,
        data_type: String,
        data: Vec<u8>,
    ) -> Result<Event, UnknownFlow> {
        self.router.publish(flow, data_type, data)
    }

    fn last(&self, anchor: AnchorId, flow: &QualifiedFlowId) -> Result<Option<Event>, UnknownFlow> {
        self.router.last(anchor, flow)
    }

    fn history(
        &self,
        anchor: AnchorId,
        flow: &QualifiedFlowId,
        after: EventId,
    ) -> Result<History, UnknownFlow> {
        self.router.history(anchor, flow, after)
    }

    fn flow_error(&self, flow: &QualifiedFlowId, message: String) {
        self.router.flow_error(flow, message);
    }

    fn end_flow(&self, flow: &QualifiedFlowId) {
        self.router.end_flow(flow);
    }

    fn stream(&self, anchor: AnchorId, since: Option<EventId>) -> UnboundedReceiver<StreamEvent> {
        self.router.stream(anchor, since)
    }

    fn subscribers(&self, flow: &QualifiedFlowId) -> usize {
        self.router.subscribers(flow)
    }

    fn empty_flows(&self) -> UnboundedReceiver<QualifiedFlowId> {
        self.router.empty_flows()
    }

    fn has_subscriptions(&self) -> tokio::sync::watch::Receiver<bool> {
        self.router.has_subscriptions()
    }

    fn remove_anchor(&self, anchor: AnchorId) {
        let session = self
            .sessions
            .get()
            .and_then(Weak::upgrade)
            .and_then(|sessions| sessions.current(anchor));
        self.router.remove_anchor(anchor);
        let _ = self.removed.send((anchor, session));
    }
}

/// A registry on the fakes, started on the test's runtime.
struct Registry {
    identity: Arc<FakeIdentity>,
    router: Arc<Spy>,
    sessions: Arc<SessionRegistry>,
    removed: UnboundedReceiver<(AnchorId, Option<SessionId>)>,
}

impl Registry {
    fn new() -> Self {
        let identity = Arc::new(FakeIdentity::new());
        let (removed, removals) = unbounded_channel();
        let router = Arc::new(Spy {
            router: FakeRouter::new(),
            sessions: OnceLock::new(),
            removed,
        });
        let sessions = Arc::new(SessionRegistry::new(
            Arc::clone(&identity),
            Arc::clone(&router),
        ));
        router.sessions.set(Arc::downgrade(&sessions)).unwrap();
        Self {
            identity,
            router,
            sessions,
            removed: removals,
        }
    }

    /// The next anchor the registry removes from the router. The registry
    /// had already forgotten its session by then.
    async fn removal(&mut self) -> AnchorId {
        let (anchor, session) = timeout(PATIENCE, self.removed.recv())
            .await
            .expect("the registry removes the dead anchor")
            .expect("the router outlives the test");
        assert_eq!(session, None, "the session of {anchor} is forgotten first");
        anchor
    }

    /// Waits until the registry has handled every declaration made so far:
    /// it handles them in order, so once a later anchor's death went through,
    /// the earlier ones are watched. Uses an anchor of its own, `barrier`.
    async fn settle(&mut self, barrier: AnchorId) {
        self.sessions.declare(barrier, SessionId::new("barrier"));
        self.identity.kill(barrier);
        assert_eq!(self.removal().await, barrier);
    }
}

#[tokio::test]
async fn changing_the_session_keeps_the_subscriptions() {
    let mut registry = Registry::new();
    let sessions = &registry.sessions;
    sessions.declare(A, SessionId::new("one"));
    let router = &registry.router;
    router.subscribe(A, flow("x"));
    let mut stream = router.stream(A, None);

    sessions.declare(A, SessionId::new("two"));
    registry.settle(C).await;

    assert_eq!(registry.sessions.current(A), session("two"));
    let router = &registry.router;
    assert_eq!(router.subscribers(&flow("x")), 1);
    let sent = router
        .publish(&flow("x"), "t".into(), b"1".to_vec())
        .unwrap();
    assert_eq!(stream.try_recv(), Ok(StreamEvent::Update(sent)));
}

#[tokio::test]
async fn killing_the_anchor_removes_its_subscriptions_and_releases_its_flows() {
    let mut registry = Registry::new();
    let mut empty = registry.router.empty_flows();
    registry.sessions.declare(A, SessionId::new("a"));
    registry.sessions.declare(B, SessionId::new("b"));
    let router = &registry.router;
    router.subscribe(A, flow("own"));
    router.subscribe(A, flow("shared"));
    router.subscribe(B, flow("shared"));
    let mut stream = router.stream(A, None);

    registry.identity.kill(A);
    assert_eq!(registry.removal().await, A);

    assert_eq!(registry.sessions.current(A), None);
    assert_eq!(empty.try_recv(), Ok(flow("own")));
    assert!(empty.try_recv().is_err(), "the shared flow keeps B");
    let router = &registry.router;
    assert_eq!(router.subscribers(&flow("own")), 0);
    assert_eq!(router.subscribers(&flow("shared")), 1);
    assert_eq!(stream.recv().await, None, "the dead anchor's stream closes");
    assert_eq!(registry.sessions.current(B), session("b"));
}

#[tokio::test]
async fn declaring_again_does_not_watch_the_anchor_twice() {
    let mut registry = Registry::new();
    for i in 0..100 {
        registry
            .sessions
            .declare(A, SessionId::new(format!("s{i}")));
    }
    registry.settle(B).await;
    // Each watcher holds the identity: the test, the task and one watcher, A's.
    assert_eq!(Arc::strong_count(&registry.identity), 3);

    registry.identity.kill(A);
    assert_eq!(registry.removal().await, A);
    registry.settle(C).await;
    assert!(registry.removed.try_recv().is_err(), "A is removed once");
}

#[tokio::test]
async fn an_anchor_dead_before_its_declaration_is_removed() {
    let mut registry = Registry::new();
    registry.identity.kill(A);
    registry.sessions.declare(A, SessionId::new("late"));
    registry.router.subscribe(A, flow("x"));
    assert_eq!(registry.removal().await, A);
    assert_eq!(registry.sessions.current(A), None);
    assert_eq!(registry.router.subscribers(&flow("x")), 0);
}

#[tokio::test]
async fn a_declaration_after_the_anchors_removal_is_removed_again() {
    let mut registry = Registry::new();
    registry.sessions.declare(A, SessionId::new("one"));
    registry.identity.kill(A);
    assert_eq!(registry.removal().await, A);

    // A request in flight, or a descendant that outlived the anchor.
    registry.sessions.declare(A, SessionId::new("two"));
    registry.router.subscribe(A, flow("x"));
    assert_eq!(registry.removal().await, A);
    assert_eq!(registry.sessions.current(A), None);
    assert_eq!(registry.router.subscribers(&flow("x")), 0);
}

#[tokio::test]
async fn of_many_anchors_only_the_dead_ones_go() {
    const ANCHORS: u64 = 500;
    let mut registry = Registry::new();
    let mut empty = registry.router.empty_flows();
    let own = |anchor: u64| flow(&format!("own-{anchor}"));
    for anchor in 1..=ANCHORS {
        let id = AnchorId(anchor);
        registry
            .sessions
            .declare(id, SessionId::new(anchor.to_string()));
        registry.router.subscribe(id, own(anchor));
        registry.router.subscribe(id, flow("shared"));
    }

    let dead: HashSet<_> = (1..=ANCHORS).filter(|a| a % 2 == 0).collect();
    for &anchor in &dead {
        registry.identity.kill(AnchorId(anchor));
    }
    let mut removed = HashSet::new();
    for _ in 0..dead.len() {
        removed.insert(registry.removal().await.0);
    }
    assert_eq!(removed, dead);

    let mut released = HashSet::new();
    while let Ok(flow) = empty.try_recv() {
        released.insert(flow);
    }
    assert_eq!(released, dead.iter().map(|&a| own(a)).collect());
    for anchor in 1..=ANCHORS {
        let alive = !dead.contains(&anchor);
        let current = registry.sessions.current(AnchorId(anchor));
        assert_eq!(current.is_some(), alive, "anchor {anchor}");
        assert_eq!(
            registry.router.subscribers(&own(anchor)),
            usize::from(alive)
        );
    }
    let alive = ANCHORS as usize - dead.len();
    assert_eq!(registry.router.subscribers(&flow("shared")), alive);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn declaring_needs_no_runtime() {
    let mut registry = Registry::new();
    let sessions = Arc::clone(&registry.sessions);
    std::thread::spawn(move || {
        assert!(tokio::runtime::Handle::try_current().is_err());
        sessions.declare(A, SessionId::new("one"));
        assert_eq!(sessions.current(A), session("one"));
    })
    .join()
    .unwrap();
    registry.identity.kill(A);
    assert_eq!(registry.removal().await, A);
}

#[tokio::test]
async fn dropping_the_registry_stops_its_task_and_watchers() {
    let registry = Registry::new();
    registry.sessions.declare(A, SessionId::new("alive"));
    let Registry {
        identity,
        router,
        sessions,
        ..
    } = registry;
    drop(sessions);
    // The task holds both, and each watcher the identity.
    timeout(PATIENCE, async {
        while Arc::strong_count(&identity) > 1 || Arc::strong_count(&router) > 1 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the task and its watchers end");
}
