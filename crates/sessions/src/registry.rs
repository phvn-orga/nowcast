use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use nowcast_traits::{AnchorId, Identity, Router, Sessions};
use nowcast_types::SessionId;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinSet;

type Map = Mutex<HashMap<AnchorId, SessionId>>;

/// The service's session registry: the session of each anchor, held in
/// memory, and one background task that watches the declared anchors.
///
/// [`Sessions::declare`] and [`Sessions::current`] only take a lock and never
/// wait, so they need no runtime. An anchor declared for the first time is
/// sent to the task, which waits for all the anchors together. When one
/// exits, the task forgets its session and only then calls
/// [`Router::remove_anchor`]: once the router has removed an anchor,
/// [`Sessions::current`] answers `None` for it.
///
/// Anchor ids are never reused, so a declaration that arrives after the
/// anchor's death (a request in flight, a descendant process that outlived
/// it) is watched again: [`Identity::exited`] resolves at once for a dead
/// anchor, and the session and anything subscribed meanwhile go again.
///
/// Dropping the registry stops the task and its watchers.
pub struct SessionRegistry {
    sessions: Arc<Map>,
    /// Anchors declared for the first time, to the watching task.
    declared: UnboundedSender<AnchorId>,
}

impl SessionRegistry {
    /// Starts the watching task on the current Tokio runtime, which must be
    /// able to run `identity`'s [`Identity::exited`] (with IO enabled, for the
    /// service's identity).
    ///
    /// # Panics
    ///
    /// When called outside a Tokio runtime.
    pub fn new<I: Identity, R: Router>(identity: Arc<I>, router: Arc<R>) -> Self {
        let sessions = Arc::default();
        let (declared, anchors) = unbounded_channel();
        tokio::spawn(watch(identity, router, Arc::clone(&sessions), anchors));
        Self { sessions, declared }
    }
}

impl Sessions for SessionRegistry {
    fn declare(&self, anchor: AnchorId, session: SessionId) {
        let first = lock(&self.sessions).insert(anchor, session).is_none();
        if first {
            // Fails only once the task has stopped with its runtime, when
            // nothing is watched any more anyway.
            let _ = self.declared.send(anchor);
        }
    }

    fn current(&self, anchor: AnchorId) -> Option<SessionId> {
        lock(&self.sessions).get(&anchor).cloned()
    }

    fn declared(&self) -> Vec<(AnchorId, SessionId)> {
        let mut declared: Vec<_> = lock(&self.sessions)
            .iter()
            .map(|(&anchor, session)| (anchor, session.clone()))
            .collect();
        declared.sort_by_key(|&(anchor, _)| anchor);
        declared
    }
}

fn lock(sessions: &Map) -> MutexGuard<'_, HashMap<AnchorId, SessionId>> {
    // Every operation is a single map call, so the map is consistent even if
    // the lock is somehow poisoned.
    sessions.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The registry's task: one watcher per declared anchor, until the registry
/// is dropped. Returning drops the watchers, which aborts them.
async fn watch<I: Identity, R: Router>(
    identity: Arc<I>,
    router: Arc<R>,
    sessions: Arc<Map>,
    mut anchors: UnboundedReceiver<AnchorId>,
) {
    let mut watchers = JoinSet::new();
    loop {
        tokio::select! {
            declared = anchors.recv() => {
                // `None`: the registry was dropped.
                let Some(anchor) = declared else { return };
                let identity = Arc::clone(&identity);
                watchers.spawn(async move {
                    identity.exited(anchor).await;
                    anchor
                });
            }
            // Disabled while there is no watcher, until the next declaration.
            Some(exited) = watchers.join_next() => {
                // An error means `Identity::exited` panicked: whether the
                // anchor died is unknown, so it is left as it is.
                if let Ok(anchor) = exited {
                    lock(&sessions).remove(&anchor);
                    router.remove_anchor(anchor);
                }
            }
        }
    }
}
