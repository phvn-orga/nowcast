use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use nowcast_traits::{
    BrickConnection, BrickLauncher, BrickUnavailable, Discovery, DiscoveryChange, ResolveError,
    Router, Supervisor, UnknownFlow,
};
use nowcast_types::{BrickMessage, BrickName, QualifiedFlowId};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc::UnboundedReceiver;
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
        });
        tokio::spawn(follow_empty_flows(Arc::downgrade(&inner), empty));
        tokio::spawn(follow_discovery(Arc::downgrade(&inner), changes));
        Self { inner }
    }
}

impl<D: Discovery, L: BrickLauncher, R: Router> Supervisor for BrickSupervisor<D, L, R> {
    fn resolve(
        &self,
        brick: &BrickName,
        kind: &str,
        params: &[u8],
    ) -> impl Future<Output = Result<QualifiedFlowId, ResolveError>> + Send {
        let answer = match self.inner.discovery.brick(brick) {
            Some(_) => Ok(self
                .inner
                .update(|state, router| state.resolve(brick, kind, params, router))),
            None => Err(ResolveError::UnknownBrick(brick.clone())),
        };
        let brick = brick.clone();
        async move {
            // A dropped reply means the supervisor itself went away.
            answer?
                .await
                .unwrap_or(Err(ResolveError::Unavailable(BrickUnavailable(brick))))
        }
    }

    fn refresh(&self, flow: &QualifiedFlowId) -> Result<(), UnknownFlow> {
        self.inner.update(|state, _| state.refresh(flow))
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
        let launched = match self.discovery.brick(&brick) {
            Some(declaration) => self
                .launcher
                .launch(&declaration)
                .map(|connection| (declaration, connection)),
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the brick is no longer declared",
            )),
        };
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
                state.exited(&brick, generation, router);
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
        inner.update(|state, router| state.message(&brick, generation, msg, router));
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
