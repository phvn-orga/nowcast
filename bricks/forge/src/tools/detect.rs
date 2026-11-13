//! The detection registry a provider may use: detectors shared by every
//! target of a scope (a repository, a project) that depends on them, and
//! running only while one does. A detector polls, wakes the targets what
//! moved touches, which then read in detail, and waits for its period or
//! to be asked. What a detector polls, what moved and which targets a move
//! touches are the provider's ([`Scope`], [`Detector`]).
//!
//! A detector polls every [`PERIOD`](super::throttle::PERIOD), slower when
//! its answers keep changing ([`Throttle`]), and at [`SLOWEST`] at least
//! while the provider says its budget runs low. A target's `refresh` has
//! the detectors it depends on poll at once and opens their fast window:
//! they poll at the provider's window pace for [`WINDOW_FOR`]. After a
//! failed poll, a detector waits the provider's normal pace, unless a
//! `refresh` asks it to poll.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::future::Future;
use std::hash::Hash;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;
use tokio::time::Instant;

use super::http;
use super::pace::{Rhythm, WINDOW_FOR};
use super::throttle::{SLOWEST, Throttle};

/// How long a `refresh` waits for its detectors before the target reads
/// anyway, on a slow or unreachable forge.
const ASKED_FOR: Duration = Duration::from_secs(5);

/// What a provider detects by, such as a repository: what its detectors
/// share, and how its targets relate to them.
pub trait Scope: Send + Sync + Sized + 'static {
    /// What a target is about, for a detector to tell whether a move
    /// touches it.
    type Interest: Send + 'static;
    /// What tells a detector of the scope from the others.
    type Key: Clone + Eq + Hash + Send + 'static;
    /// An item a detector saw move.
    type Moved: Send + 'static;
    type Detector: Detector<Moved = Self::Moved>;

    /// The provider's paces: `normal` after a failed poll or without a
    /// token, `window` after a `refresh`.
    const RHYTHM: Rhythm;

    /// The detectors a target of `interest` depends on.
    fn keys(interest: &Self::Interest) -> Vec<Self::Key>;

    /// Whether `moved` touches a target of `interest`.
    fn touches(moved: &Self::Moved, interest: &Self::Interest) -> bool;

    /// The detector of `key`, before its first poll.
    fn detector(&self, key: &Self::Key) -> Self::Detector;
}

/// What one detector reads on the forge, and how it compares its answers.
pub trait Detector: Send + 'static {
    type Moved;

    /// One poll.
    fn poll(&mut self) -> impl Future<Output = Polled<Self::Moved>> + Send;

    /// Forgets the token a `401` refused, as a target would.
    fn drop_token(&mut self) -> impl Future<Output = ()> + Send;

    /// Whether the last poll's answers said the budget runs low.
    fn budget_low(&self) -> bool {
        false
    }

    /// Where the detector reads, for the log.
    fn at(&self) -> String;
}

/// What a poll got.
pub enum Polled<M> {
    /// No token: the detector waits the normal pace, which asking its
    /// source again sooner would not change.
    NoToken,
    /// The first answer, which only sets what the next ones are compared
    /// with.
    First,
    /// An answer, `changed` or not for the throttle, and what moved since
    /// the last one.
    Answered {
        changed: bool,
        moved: Vec<M>,
    },
    Failed(http::Failure),
}

/// The scopes detected, shared by every target of the provider.
pub struct Detection<S: Scope> {
    scopes: Arc<Mutex<HashMap<String, Weak<Watched<S>>>>>,
}

impl<S: Scope> Default for Detection<S> {
    fn default() -> Self {
        Self {
            scopes: Arc::default(),
        }
    }
}

impl<S: Scope> Detection<S> {
    /// Registers a target of the scope `key` about `interest`, `scope`
    /// making the scope when no target holds it: the detectors it depends
    /// on run while it stays registered.
    pub fn watch(
        &self,
        key: String,
        scope: impl FnOnce() -> S,
        interest: S::Interest,
    ) -> Detected<S> {
        let watched = {
            let mut scopes = self.scopes.lock().unwrap();
            scopes.retain(|_, watched| watched.strong_count() > 0);
            if let Some(watched) = scopes.get(&key).and_then(Weak::upgrade) {
                watched
            } else {
                let watched = Arc::new(Watched {
                    scope: scope(),
                    state: Mutex::default(),
                });
                scopes.insert(key, Arc::downgrade(&watched));
                watched
            }
        };
        let (wake, wakes) = unbounded_channel();
        let id = watched.add(interest, wake);
        Detected {
            watcher: Watcher { watched, id },
            wakes,
        }
    }

    /// The scopes detected and their detectors running, for tests.
    #[cfg(test)]
    pub fn running(&self) -> Vec<(String, usize)> {
        let scopes = self.scopes.lock().unwrap();
        let mut running: Vec<(String, usize)> = scopes
            .iter()
            .filter_map(|(key, watched)| {
                let watched = watched.upgrade()?;
                let count = watched.state.lock().unwrap().detectors.len();
                Some((key.clone(), count))
            })
            .collect();
        running.sort();
        running
    }
}

/// One scope detected, held by the targets registered on it.
pub struct Watched<S: Scope> {
    pub scope: S,
    state: Mutex<State<S>>,
}

struct State<S: Scope> {
    last_id: u64,
    /// Each target registered, and where to wake it.
    watchers: HashMap<u64, (S::Interest, UnboundedSender<()>)>,
    detectors: HashMap<S::Key, Running>,
}

impl<S: Scope> Default for State<S> {
    fn default() -> Self {
        Self {
            last_id: 0,
            watchers: HashMap::new(),
            detectors: HashMap::new(),
        }
    }
}

/// A detector's task, stopped when dropped.
struct Running {
    task: AbortHandle,
    /// Asks for a poll now, answered once done.
    asks: UnboundedSender<oneshot::Sender<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl<S: Scope> Watched<S> {
    /// Registers a target, starting the detectors it needs that no other
    /// target started.
    fn add(self: &Arc<Self>, interest: S::Interest, wake: UnboundedSender<()>) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.last_id += 1;
        let id = state.last_id;
        for key in S::keys(&interest) {
            if let Entry::Vacant(vacant) = state.detectors.entry(key) {
                let (asks, asked) = unbounded_channel();
                let poller = Poller::new(self, vacant.key());
                let task = tokio::spawn(poller.run(asked)).abort_handle();
                vacant.insert(Running { task, asks });
            }
        }
        state.watchers.insert(id, (interest, wake));
        id
    }

    /// Unregisters target `id`, stopping the detectors no target needs.
    fn remove(&self, id: u64) {
        let mut state = self.state.lock().unwrap();
        state.watchers.remove(&id);
        let State {
            watchers,
            detectors,
            ..
        } = &mut *state;
        detectors.retain(|key, _| watchers.values().any(|(i, _)| S::keys(i).contains(key)));
    }

    /// Wakes the targets one of `moved` touches.
    fn wake(&self, moved: &[S::Moved]) {
        let state = self.state.lock().unwrap();
        for (interest, wake) in state.watchers.values() {
            if moved.iter().any(|m| S::touches(m, interest)) {
                let _ = wake.send(());
            }
        }
    }
}

/// A target's registration; dropping it unregisters the target.
pub struct Watcher<S: Scope> {
    watched: Arc<Watched<S>>,
    id: u64,
}

impl<S: Scope> Watcher<S> {
    /// Updates what the target is about.
    pub fn learn(&self, update: impl FnOnce(&mut S::Interest)) {
        let mut state = self.watched.state.lock().unwrap();
        if let Some((interest, _)) = state.watchers.get_mut(&self.id) {
            update(interest);
        }
    }

    /// Has the detectors the target depends on poll now, opening their fast
    /// window, and returns once they did, could not, or [`ASKED_FOR`] passed.
    pub async fn poll_now(&self) {
        let answers: Vec<oneshot::Receiver<()>> = {
            let state = self.watched.state.lock().unwrap();
            let keys = state
                .watchers
                .get(&self.id)
                .map(|(i, _)| S::keys(i))
                .unwrap_or_default();
            keys.iter()
                .filter_map(|key| state.detectors.get(key))
                .map(|running| {
                    let (answer, answered) = oneshot::channel();
                    let _ = running.asks.send(answer);
                    answered
                })
                .collect()
        };
        let all = async {
            for answered in answers {
                let _ = answered.await;
            }
        };
        let _ = tokio::time::timeout(ASKED_FOR, all).await;
    }
}

impl<S: Scope> Drop for Watcher<S> {
    fn drop(&mut self) {
        self.watched.remove(self.id);
    }
}

/// A target's registration, and what its detectors send it.
pub struct Detected<S: Scope> {
    pub watcher: Watcher<S>,
    wakes: UnboundedReceiver<()>,
}

impl<S: Scope> Detected<S> {
    /// Waits until `deadline`, for a `refresh`, or for a detector to say the
    /// target moved; what piled up counts as one. A `refresh` calls
    /// `refreshed`, and has the target's detectors poll first: what they
    /// wake is the reading that follows. A rate limit (`limited`) holds the
    /// wait whatever comes. `false` once the core is gone.
    pub async fn wait(
        &mut self,
        deadline: Instant,
        refreshes: &mut UnboundedReceiver<()>,
        limited: bool,
        mut refreshed_now: impl FnMut(),
    ) -> bool {
        let mut refreshed = false;
        loop {
            // A pending `refresh` wins over a wake or the deadline, for its
            // detectors to poll before the reading.
            tokio::select! {
                biased;
                refresh = refreshes.recv() => {
                    if refresh.is_none() {
                        return false;
                    }
                    while refreshes.try_recv().is_ok() {}
                    refreshed = true;
                    refreshed_now();
                    if !limited {
                        break;
                    }
                }
                Some(()) = self.wakes.recv() => {
                    if !limited {
                        break;
                    }
                }
                () = tokio::time::sleep_until(deadline) => break,
            }
        }
        if refreshed {
            self.watcher.poll_now().await;
        }
        while self.wakes.try_recv().is_ok() {}
        true
    }
}

/// One detector of a scope: the provider's part, and its pace.
pub struct Poller<S: Scope> {
    watched: Weak<Watched<S>>,
    pub(crate) detector: S::Detector,
    pub(crate) throttle: Throttle,
    /// Until when a `refresh` keeps the pace fast.
    pub(crate) window_until: Option<Instant>,
    /// Whether the last poll failed: a lasting failure is logged once, and
    /// the next poll waits the normal pace.
    pub(crate) failing: bool,
    /// Whether a `401` dropped the token since the last poll that answered:
    /// the next `401`s keep the token asked since, which asking again would
    /// not change.
    dropped: bool,
}

impl<S: Scope> Poller<S> {
    pub(crate) fn new(watched: &Arc<Watched<S>>, key: &S::Key) -> Self {
        Self {
            watched: Arc::downgrade(watched),
            detector: watched.scope.detector(key),
            throttle: Throttle::default(),
            window_until: None,
            failing: false,
            dropped: false,
        }
    }

    /// Polls, wakes what moved, waits for its period or to be asked, and
    /// again. Those who asked are answered once the poll that follows is
    /// done; while it cannot poll (a rate limit, no token), at once.
    async fn run(mut self, mut asked: UnboundedReceiver<oneshot::Sender<()>>) {
        let mut waiting: Vec<oneshot::Sender<()>> = Vec::new();
        loop {
            let limited = self.poll().await;
            for answer in waiting.drain(..) {
                let _ = answer.send(());
            }
            let now = Instant::now();
            let deadline = now + limited.unwrap_or_else(|| self.period(now));
            loop {
                tokio::select! {
                    () = tokio::time::sleep_until(deadline) => break,
                    answer = asked.recv() => {
                        let Some(answer) = answer else {
                            return;
                        };
                        self.window_until = Some(Instant::now() + WINDOW_FOR);
                        if limited.is_some() {
                            let _ = answer.send(());
                            continue;
                        }
                        waiting.push(answer);
                        while let Ok(answer) = asked.try_recv() {
                            waiting.push(answer);
                        }
                        break;
                    }
                }
            }
        }
    }

    /// The normal pace after a failed poll; else the window pace after a
    /// `refresh`, else the throttle's; at [`SLOWEST`] at least while the
    /// budget runs low.
    pub(crate) fn period(&self, now: Instant) -> Duration {
        if self.failing {
            return S::RHYTHM.normal;
        }
        let period = if self.window_until.is_some_and(|until| now < until) {
            S::RHYTHM.window
        } else {
            self.throttle.period()
        };
        if self.detector.budget_low() {
            period.max(SLOWEST)
        } else {
            period
        }
    }

    /// One poll, and how long to wait when it cannot poll: what a rate
    /// limit asks, or the normal pace without a token.
    pub(crate) async fn poll(&mut self) -> Option<Duration> {
        match self.detector.poll().await {
            Polled::NoToken => Some(S::RHYTHM.normal),
            Polled::First => {
                self.answered();
                None
            }
            Polled::Answered { changed, moved } => {
                self.answered();
                self.throttle.answered(changed);
                if !moved.is_empty()
                    && let Some(watched) = self.watched.upgrade()
                {
                    watched.wake(&moved);
                }
                None
            }
            Polled::Failed(failure) => self.failed(failure).await,
        }
    }

    /// A poll that answered.
    fn answered(&mut self) {
        self.failing = false;
        self.dropped = false;
    }

    /// A failed poll, and how long a rate limit asks to wait. A refused
    /// token (`401`) is forgotten, as by a target, once until a poll
    /// answers; a `403` keeps it.
    async fn failed(&mut self, failure: http::Failure) -> Option<Duration> {
        if failure == http::Failure::Unauthorized && !std::mem::replace(&mut self.dropped, true) {
            self.detector.drop_token().await;
        }
        if !std::mem::replace(&mut self.failing, true) {
            let why = match &failure {
                http::Failure::Unreachable(why) => format!("unreachable: {why}"),
                http::Failure::Unreadable(why) => why.clone(),
                other => format!("{other:?}"),
            };
            crate::log(format_args!("{}: {why}", self.detector.at()));
        }
        match failure {
            http::Failure::Limited(wait) => Some(wait.unwrap_or(S::RHYTHM.normal)),
            _ => None,
        }
    }
}

/// Helpers for the tests of a provider's detection.
#[cfg(test)]
pub mod testing {
    use super::*;

    /// A scope with a target per interest, and no detector running: the
    /// test polls them itself.
    pub fn watched<S: Scope>(
        scope: S,
        interests: Vec<S::Interest>,
    ) -> (Arc<Watched<S>>, Vec<UnboundedReceiver<()>>) {
        let watched = Arc::new(Watched {
            scope,
            state: Mutex::default(),
        });
        let mut wakes = Vec::new();
        let mut state = watched.state.lock().unwrap();
        for (id, interest) in interests.into_iter().enumerate() {
            let (wake, woken) = unbounded_channel();
            state.watchers.insert(id as u64, (interest, wake));
            wakes.push(woken);
        }
        drop(state);
        (watched, wakes)
    }

    /// Which targets were woken since the last call.
    pub fn woken(wakes: &mut [UnboundedReceiver<()>]) -> Vec<usize> {
        let mut woken = Vec::new();
        for (i, wake) in wakes.iter_mut().enumerate() {
            if wake.try_recv().is_ok() {
                woken.push(i);
            }
            while wake.try_recv().is_ok() {}
        }
        woken
    }
}
