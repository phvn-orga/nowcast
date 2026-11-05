use std::collections::HashSet;
use std::future::Future;
use std::io;
use std::os::fd::BorrowedFd;
use std::sync::Mutex;

use tokio::sync::watch;

use crate::{Anchor, AnchorId, Identity};

/// Answers every connection with the anchor the test set, `AnchorId(1)` with no
/// kind by default.
pub struct FakeIdentity {
    anchor: Mutex<Anchor>,
    dead: watch::Sender<HashSet<AnchorId>>,
}

impl Default for FakeIdentity {
    fn default() -> Self {
        Self {
            anchor: Mutex::new(Anchor {
                id: AnchorId(1),
                kind: None,
            }),
            dead: watch::Sender::new(HashSet::new()),
        }
    }
}

impl FakeIdentity {
    pub fn new() -> Self {
        Self::default()
    }

    /// The anchor of the next connections.
    pub fn set_anchor(&self, anchor: Anchor) {
        *self.anchor.lock().unwrap() = anchor;
    }

    /// Makes [`Identity::exited`] resolve for `anchor`.
    pub fn kill(&self, anchor: AnchorId) {
        self.dead.send_modify(|dead| {
            dead.insert(anchor);
        });
    }
}

impl Identity for FakeIdentity {
    fn identify(&self, _socket: BorrowedFd<'_>) -> io::Result<Anchor> {
        Ok(self.anchor.lock().unwrap().clone())
    }

    fn exited(&self, anchor: AnchorId) -> impl Future<Output = ()> + Send {
        let mut dead = self.dead.subscribe();
        async move {
            // The sender lives in `self`, which outlives the caller's use.
            let _ = dead.wait_for(|dead| dead.contains(&anchor)).await;
        }
    }
}
