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
/// Linux only, 6.5 or later (`SO_PEERPIDFD`). [`Identity::identify`] fails
/// with [`io::ErrorKind::Unsupported`] on an older kernel, and with
/// [`io::ErrorKind::NotFound`] when the connected process has already exited.
pub struct PeerIdentity {
    kinds: Box<dyn Fn() -> Vec<ClientKind> + Send + Sync>,
    matcher: Mutex<Option<Compiled>>,
    anchors: Mutex<Anchors>,
}

/// A matcher with the kinds it was compiled from.
struct Compiled {
    kinds: Vec<ClientKind>,
    matcher: Arc<KindMatcher>,
}

impl PeerIdentity {
    /// Compares each connection against the client kinds `kinds` returns at
    /// that moment, typically `move || discovery.client_kinds()`, so that a
    /// kind changed on disk applies from the next connection. The patterns
    /// are compiled again only when the kinds differ from the previous call.
    pub fn new(kinds: impl Fn() -> Vec<ClientKind> + Send + Sync + 'static) -> Self {
        Self {
            kinds: Box::new(kinds),
            matcher: Mutex::default(),
            anchors: Mutex::default(),
        }
    }

    /// Compares every connection against the same `kinds`.
    pub fn fixed(kinds: Vec<ClientKind>) -> Self {
        Self::new(move || kinds.clone())
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

    /// The matcher for the current kinds, compiled again only when they
    /// changed: an invalid pattern is logged once per change, not once per
    /// connection.
    fn matcher(&self) -> Arc<KindMatcher> {
        let kinds = (self.kinds)();
        // The cache is replaced in one statement, so a panic while it was
        // held leaves it either old or new, both valid.
        let mut cached = self.matcher.lock().unwrap_or_else(|e| e.into_inner());
        match &*cached {
            Some(compiled) if compiled.kinds == kinds => Arc::clone(&compiled.matcher),
            _ => {
                let matcher = Arc::new(KindMatcher::new(&kinds));
                *cached = Some(Compiled {
                    kinds,
                    matcher: Arc::clone(&matcher),
                });
                matcher
            }
        }
    }
}

impl Identity for PeerIdentity {
    fn identify(&self, socket: BorrowedFd<'_>) -> io::Result<Anchor> {
        let connected = Process::peer_of(socket)?;
        let (anchor, kind) = self
            .matcher()
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use nowcast_types::ClientKindName;

    use super::*;

    fn kind(name: &str, exe: &str) -> ClientKind {
        ClientKind {
            name: ClientKindName::new(name).unwrap(),
            description: String::new(),
            exe: vec![exe.into()],
        }
    }

    #[test]
    fn the_matcher_is_compiled_again_only_when_the_kinds_change() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let identity = PeerIdentity::new(move || {
            // The first two calls see one kind, the next ones another.
            if counter.fetch_add(1, Ordering::SeqCst) < 2 {
                vec![kind("codex", "*/bin/codex")]
            } else {
                vec![kind("codex", "*/bin/codex-new")]
            }
        });
        let first = identity.matcher();
        let same = identity.matcher();
        let changed = identity.matcher();
        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&same, &changed));
        assert!(Arc::ptr_eq(&changed, &identity.matcher()));
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }
}
