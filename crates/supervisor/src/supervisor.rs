use std::collections::VecDeque;
use std::future::Future;
use std::mem;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use nowcast_traits::{
    AnchorId, BrickConnection, BrickLauncher, BrickSummary, BrickUnavailable, Discovery,
    DiscoveryChange, InputError, ResolveError, Resolved, Router, Supervisor, UnknownFlow,
};
use nowcast_types::{BrickMessage, BrickName, QualifiedFlowId};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::{oneshot, watch};
use tracing::warn;

use crate::Config;
use crate::state::{Effect, State, Timer};

/// The service's supervisor: launches bricks with a [`BrickLauncher`], feeds
/// what they publish into a [`Router`], and follows the router's empty flows
/// and discovery's removed bricks.
///
/// The state of every brick sits behind one lock, and each event (a call, a
/// message from a brick, its exit, a timer, an empty flow, a removal) is one
/// transition under it. Reading each brick's output and waiting on timers
/// happen in tasks, which report to the state.
pub struct BrickSupervisor<D, L, R> {
    inner: Arc<Inner<D, L, R>>,
}

struct Inner<D, L, R> {
    discovery: Arc<D>,
    launcher: Arc<L>,
    router: Arc<R>,
    state: Mutex<State>,
    /// Turns `true` once [`BrickSupervisor::shutdown`] has stopped every
    /// brick.
    drained: watch::Sender<bool>,
}

impl<D: Discovery, L: BrickLauncher, R: Router> BrickSupervisor<D, L, R> {
    /// Starts following the router's empty flows and discovery's changes.
    /// Must be called within a tokio runtime, where the supervisor spawns its
    /// tasks.
    pub fn start(discovery: Arc<D>, launcher: Arc<L>, router: Arc<R>, config: Config) -> Self {
        let empty = router.empty_flows();
        let changes = discovery.changes();
        let inner = Arc::new(Inner {
            discovery,
            launcher,
            router,
            state: Mutex::new(State::new(config)),
            drained: watch::Sender::new(false),
        });
        tokio::spawn(follow_empty_flows(Arc::downgrade(&inner), empty));
        tokio::spawn(follow_discovery(Arc::downgrade(&inner), changes));
        Self { inner }
    }

    /// Stops every brick for good, and returns once each one's process has
    /// exited, with what it left in its process group.
    ///
    /// Each brick gets `shutdown`, then `SIGTERM` after
    /// [`Config::terminate_after`], then `SIGKILL` after [`Config::kill_after`],
    /// stopping at the step where it exits; a brick already stopping goes on
    /// from where it is, and one waiting to be relaunched after a crash is
    /// forgotten. Subscriptions in progress fail as unavailable, the flows
    /// end in the router, and every later subscription fails as unavailable.
    pub async fn shutdown(&self) {
        let mut drained = self.inner.drained.subscribe();
        self.inner.update(|state, router| state.shutdown(router));
        // The sender lives in `inner`, which `self` holds.
        let _ = drained.wait_for(|drained| *drained).await;
    }
}

impl<D: Discovery, L: BrickLauncher, R: Router> Supervisor for BrickSupervisor<D, L, R> {
    fn resolve(
        &self,
        anchor: AnchorId,
        brick: &BrickName,
        kind: &str,
        params: &[u8],
    ) -> impl Future<Output = Result<Resolved, ResolveError>> + Send {
        let discovery = &self.inner.discovery;
        // Checked under the lock: a removal is handled under it too, so it
        // comes either before the check or after the subscription is
        // registered, which it then fails.
        let reply = self.inner.update(|state, router| {
            discovery
                .brick(brick)
                .map(|_| state.resolve(anchor, brick, kind, params, router))
        });
        let answer = match reply {
            Some(reply) => Ok(Answer {
                reply,
                anchor,
                router: self.inner.router.clone(),
                taken: false,
            }),
            None => Err(ResolveError::UnknownBrick(brick.clone())),
        };
        let brick = brick.clone();
        async move {
            let mut answer = answer?;
            let resolved = (&mut answer.reply).await;
            answer.taken = true;
            // A dropped reply means the supervisor itself went away.
            resolved.unwrap_or(Err(ResolveError::Unavailable(BrickUnavailable::new(brick))))
        }
    }

    fn refresh(&self, flow: &QualifiedFlowId) -> Result<(), UnknownFlow> {
        self.inner.update(|state, _| state.refresh(flow))
    }

    fn kind(&self, flow: &QualifiedFlowId) -> Option<String> {
        self.inner.lock().kind(flow)
    }

    fn bricks(&self) -> Vec<BrickSummary> {
        self.inner.lock().bricks()
    }

    fn input(
        &self,
        flow: &QualifiedFlowId,
        data_type: &str,
        detail: Vec<u8>,
    ) -> Result<(), InputError> {
        self.inner
            .update(|state, _| state.input(flow, data_type, detail))
    }
}

/// The reply to a `resolve` in progress. Dropped before its value is taken,
/// it closes the reply and undoes a subscription already made, so that a
/// `resolve` dropped at any point leaves no subscription behind.
struct Answer<R: Router> {
    reply: oneshot::Receiver<Result<Resolved, ResolveError>>,
    anchor: AnchorId,
    router: Arc<R>,
    taken: bool,
}

impl<R: Router> Drop for Answer<R> {
    fn drop(&mut self) {
        if self.taken {
            return;
        }
        // Once closed, the supervisor sees the client gone and subscribes no
        // more: only a value already sent remains to undo.
        self.reply.close();
        if let Ok(Ok(resolved)) = self.reply.try_recv() {
            let _ = self.router.unsubscribe(self.anchor, &resolved.subscription);
        }
    }
}

impl<D: Discovery, L: BrickLauncher, R: Router> Inner<D, L, R> {
    fn lock(&self) -> MutexGuard<'_, State> {
        // No transition panics while holding the lock, so the state is
        // consistent even if the lock is somehow poisoned.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs one transition, then carries out the effects it asked for, all
    /// under the lock so that no other transition sees them half done.
    fn update<T>(self: &Arc<Self>, transition: impl FnOnce(&mut State, &dyn Router) -> T) -> T {
        let mut state = self.lock();
        let out = transition(&mut state, &*self.router);
        self.apply(&mut state);
        if state.is_drained() {
            self.drained
                .send_if_modified(|drained| !mem::replace(drained, true));
        }
        out
    }

    fn apply(self: &Arc<Self>, state: &mut State) {
        let mut effects = VecDeque::from(state.take_effects());
        while let Some(effect) = effects.pop_front() {
            match effect {
                Effect::Launch { brick, generation } => {
                    self.launch(state, brick, generation);
                    effects.extend(state.take_effects());
                }
                Effect::Timer { after, timer } => {
                    let inner = Arc::downgrade(self);
                    tokio::spawn(async move {
                        tokio::time::sleep(after).await;
                        if let Some(inner) = inner.upgrade() {
                            inner.fire(&timer);
                        }
                    });
                }
            }
        }
    }

    fn fire(self: &Arc<Self>, timer: &Timer) {
        self.update(|state, router| state.fired(timer, router));
    }

    /// Launches the brick as discovery declares it now, so that a changed
    /// declaration applies from the next launch.
    fn launch(self: &Arc<Self>, state: &mut State, brick: BrickName, generation: u64) {
        let router = &*self.router;
        let Some(declaration) = self.discovery.brick(&brick) else {
            // Removed since the subscription was checked: its removal is on
            // its way, handled here so that waiters learn the brick is gone.
            state.removed(&brick, router);
            state.exited(&brick, generation, router);
            return;
        };
        let launched = self
            .launcher
            .launch(&declaration)
            .map(|connection| (declaration, connection));
        match launched {
            Ok((declaration, connection)) => {
                let BrickConnection {
                    to_brick,
                    from_brick,
                    process,
                } = connection;
                tokio::spawn(read_brick(
                    Arc::downgrade(self),
                    brick.clone(),
                    generation,
                    from_brick,
                ));
                state.launched(&brick, generation, declaration, to_brick, process, router);
            }
            Err(e) => {
                warn!(brick = %brick, "cannot launch the brick: {e}");
                state.launch_failed(&brick, generation, e.to_string(), router);
            }
        }
    }
}

/// Feeds one launch's messages to the state, then its end.
async fn read_brick<D: Discovery, L: BrickLauncher, R: Router>(
    inner: Weak<Inner<D, L, R>>,
    brick: BrickName,
    generation: u64,
    mut from_brick: UnboundedReceiver<BrickMessage>,
) {
    while let Some(msg) = from_brick.recv().await {
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let discovery = inner.discovery.clone();
        inner.update(|state, router| {
            // A brick removed from discovery while one of its subscriptions
            // was in flight: the removal waits in discovery's changes, and
            // handling it first fails that subscription as unknown_brick
            // instead of completing it.
            if matches!(msg, BrickMessage::Subscribed { .. }) && discovery.brick(&brick).is_none() {
                state.removed(&brick, router);
            }
            state.message(&brick, generation, msg, router)
        });
    }
    if let Some(inner) = inner.upgrade() {
        inner.update(|state, router| state.exited(&brick, generation, router));
    }
}

async fn follow_empty_flows<D: Discovery, L: BrickLauncher, R: Router>(
    inner: Weak<Inner<D, L, R>>,
    mut empty: UnboundedReceiver<QualifiedFlowId>,
) {
    while let Some(flow) = empty.recv().await {
        let Some(inner) = inner.upgrade() else {
            return;
        };
        inner.update(|state, router| state.emptied(&flow, router));
    }
}

async fn follow_discovery<D: Discovery, L: BrickLauncher, R: Router>(
    inner: Weak<Inner<D, L, R>>,
    mut changes: broadcast::Receiver<DiscoveryChange>,
) {
    loop {
        let change = changes.recv().await;
        let Some(inner) = inner.upgrade() else {
            return;
        };
        match change {
            Ok(DiscoveryChange::BrickRemoved(brick)) => {
                inner.update(|state, router| state.removed(&brick, router));
            }
            // A changed declaration applies from the next launch.
            Ok(DiscoveryChange::BrickAdded(_) | DiscoveryChange::BrickChanged(_)) => {}
            Err(RecvError::Lagged(missed)) => {
                warn!(missed, "missed discovery changes, checking every brick");
                let discovery = inner.discovery.clone();
                inner.update(|state, router| {
                    for brick in state.names() {
                        if discovery.brick(&brick).is_none() {
                            state.removed(&brick, router);
                        }
                    }
                });
            }
            Err(RecvError::Closed) => return,
        }
    }
}
