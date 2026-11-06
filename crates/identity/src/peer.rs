//! [`PeerIdentity`], the [`Identity`] read from the kernel.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use nowcast_traits::{Anchor, AnchorId, ClientKind, Identity};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;

use crate::kinds::KindMatcher;
use crate::process::{Ancestors, Process, has_exited};

/// How often [`Identity::exited`] checks the anchor when the pidfd cannot be
/// watched by the runtime, which should not happen on a supported kernel.
const FALLBACK_POLL: Duration = Duration::from_secs(1);

/// Finds the anchor of each connection from the kernel's identity of the
/// connected process, and watches the anchor's death through a pidfd.
///
/// Linux only, 6.5 or later (`SO_PEERPIDFD`).
pub struct PeerIdentity {
    kinds: KindMatcher,
    anchors: Mutex<Anchors>,
}

impl PeerIdentity {
    /// Compares connections against `kinds`, the client kinds as discovery
    /// loaded them.
    pub fn new(kinds: &[ClientKind]) -> Self {
        Self {
            kinds: KindMatcher::new(kinds),
            anchors: Mutex::default(),
        }
    }

    /// The pid of `anchor`, for logs. `None` once the anchor is known to have
    /// exited, or for an id this identity never issued.
    pub fn pid(&self, anchor: AnchorId) -> Option<u32> {
        let anchors = self.anchors();
        let entry = anchors.by_id.get(&anchor)?;
        (!has_exited(entry.pidfd.as_fd())).then_some(entry.key.pid as u32)
    }

    fn anchors(&self) -> MutexGuard<'_, Anchors> {
        // The registry is consistent after every statement, so a panic while
        // it was held leaves nothing half done.
        self.anchors.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Identity for PeerIdentity {
    fn identify(&self, socket: BorrowedFd<'_>) -> io::Result<Anchor> {
        let connected = Process::peer_of(socket)?;
        let (anchor, kind) = self
            .kinds
            .attach(Ancestors::of(connected), |process| process.exe.as_deref())
            .ok_or_else(|| io::Error::other("no process to anchor the connection"))?;
        Ok(Anchor {
            id: self.anchors().register(anchor),
            kind,
        })
    }

    fn exited(&self, anchor: AnchorId) -> impl Future<Output = ()> + Send {
        // An id that is not registered was pruned after its process exited,
        // or was never issued: either way there is nothing to wait for.
        let pidfd = self
            .anchors()
            .by_id
            .get(&anchor)
            .map(|entry| Arc::clone(&entry.pidfd));
        async move {
            if let Some(pidfd) = pidfd {
                wait_for_exit(&pidfd).await;
            }
        }
    }
}

/// Resolves once the process held by `pidfd` has exited.
async fn wait_for_exit(pidfd: &OwnedFd) {
    // Each waiter registers its own duplicate: the runtime refuses the same
    // descriptor twice, and two waiters on one anchor are normal.
    let watched = pidfd.try_clone().and_then(|fd| {
        // SAFETY: the descriptor is owned by the `AsyncFd` and closed only
        // when it drops, so it stays open on the same pidfd meanwhile.
        unsafe { AsyncFd::register_with_interest(fd, Interest::READABLE) }.map_err(Into::into)
    });
    if let Ok(watched) = watched
        && watched.readable().await.is_ok()
    {
        return;
    }
    while !has_exited(pidfd.as_fd()) {
        tokio::time::sleep(FALLBACK_POLL).await;
    }
}

/// A process, named for the lifetime of the system: a pid is reused, a pid
/// with its start time is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ProcessKey {
    pid: i32,
    start: u64,
}

struct Entry {
    key: ProcessKey,
    pidfd: Arc<OwnedFd>,
}

/// The anchors issued so far whose process may still be alive.
#[derive(Default)]
struct Anchors {
    last: u64,
    by_process: HashMap<ProcessKey, AnchorId>,
    by_id: HashMap<AnchorId, Entry>,
}

impl Anchors {
    /// The id of `process`, the same for every connection it anchors.
    fn register(&mut self, process: Process) -> AnchorId {
        let key = ProcessKey {
            pid: process.pid,
            start: process.start,
        };
        if let Some(&id) = self.by_process.get(&key) {
            return id;
        }
        self.prune();
        self.last += 1;
        let id = AnchorId(self.last);
        self.by_process.insert(key, id);
        self.by_id.insert(
            id,
            Entry {
                key,
                pidfd: Arc::new(process.pidfd),
            },
        );
        id
    }

    /// Forgets the anchors whose process has exited, so that the registry
    /// stays the size of the live anchors. A waiter already holds its pidfd.
    fn prune(&mut self) {
        let by_process = &mut self.by_process;
        self.by_id.retain(|_, entry| {
            let alive = !has_exited(entry.pidfd.as_fd());
            if !alive {
                by_process.remove(&entry.key);
            }
            alive
        });
    }
}
