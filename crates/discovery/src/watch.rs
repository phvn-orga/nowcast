//! The discovery: reads the bricks and client kinds directories, then follows
//! them with inotify (contract sections 2 and 5).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use nowcast_traits::{BrickDeclaration, ClientKind, Discovery, DiscoveryChange};
use nowcast_types::BrickName;
use tokio::sync::broadcast;

use crate::client_kind::load_client_kinds;
use crate::dirs::DiscoveryConfig;
use crate::manifest::load_manifest;

/// How many changes a slow receiver may fall behind before it lags.
const CHANGES_CAPACITY: usize = 64;

/// Why a discovery cannot start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("cannot create {path}: {source}")]
    CreateDir { path: PathBuf, source: io::Error },
    #[error("cannot watch {path}: {source}")]
    Watch {
        path: PathBuf,
        source: notify::Error,
    },
}

/// A [`Discovery`] on the filesystem.
///
/// A brick is active while the bricks directory holds `<name>.toml`, a link
/// (or a file) to a valid declaration of `<name>`. Any change in the
/// directory triggers a new read of it, and the difference with what was held
/// is announced. The directory alone is followed: editing a linked file in
/// place is seen at the next change in the directory.
///
/// The client kinds directory is followed the same way; a change there
/// updates [`Discovery::client_kinds`] and announces nothing, since
/// [`DiscoveryChange`] covers bricks only.
pub struct FsDiscovery {
    state: Arc<State>,
    // Dropping the watcher stops following the directories.
    _watcher: RecommendedWatcher,
}

struct State {
    config: DiscoveryConfig,
    bricks: RwLock<BTreeMap<BrickName, Arc<BrickDeclaration>>>,
    client_kinds: RwLock<Vec<ClientKind>>,
    changes: broadcast::Sender<DiscoveryChange>,
    /// Serializes rescans, so that two of them cannot interleave their
    /// differences.
    rescan: Mutex<()>,
}

impl FsDiscovery {
    /// Creates the directories when missing, follows them, then reads them.
    /// Returns once both are followed, so that no change after the call is
    /// missed.
    pub fn start(config: DiscoveryConfig) -> Result<Self, StartError> {
        for dir in [&config.bricks, &config.clients] {
            std::fs::create_dir_all(dir).map_err(|source| StartError::CreateDir {
                path: dir.clone(),
                source,
            })?;
        }
        let state = Arc::new(State {
            config,
            bricks: RwLock::default(),
            client_kinds: RwLock::default(),
            changes: broadcast::Sender::new(CHANGES_CAPACITY),
            rescan: Mutex::new(()),
        });

        let handler_state = Arc::clone(&state);
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            handler_state.on_event(event);
        })
        .map_err(|source| StartError::Watch {
            path: state.config.bricks.clone(),
            source,
        })?;
        // Following before the first read: a change in between triggers one
        // more read rather than being lost.
        for dir in [&state.config.bricks, &state.config.clients] {
            watcher
                .watch(dir, RecursiveMode::NonRecursive)
                .map_err(|source| StartError::Watch {
                    path: dir.clone(),
                    source,
                })?;
        }
        state.rescan_bricks();
        state.rescan_client_kinds();
        Ok(Self {
            state,
            _watcher: watcher,
        })
    }
}

impl Discovery for FsDiscovery {
    fn brick(&self, name: &BrickName) -> Option<Arc<BrickDeclaration>> {
        read(&self.state.bricks).get(name).cloned()
    }

    fn bricks(&self) -> Vec<Arc<BrickDeclaration>> {
        read(&self.state.bricks).values().cloned().collect()
    }

    fn client_kinds(&self) -> Vec<ClientKind> {
        read(&self.state.client_kinds).clone()
    }

    fn changes(&self) -> broadcast::Receiver<DiscoveryChange> {
        self.state.changes.subscribe()
    }
}

impl State {
    fn on_event(&self, event: notify::Result<Event>) {
        let event = match event {
            Ok(event) => event,
            Err(e) => {
                tracing::warn!("watching the declarations failed, reading them again: {e}");
                self.rescan_bricks();
                self.rescan_client_kinds();
                return;
            }
        };
        if !changes_entries(&event.kind) {
            return;
        }
        // An event without a path, or a queue overflow, may hide anything.
        let everything = event.paths.is_empty() || event.need_rescan();
        let touches = |dir: &Path| everything || event.paths.iter().any(|p| p.starts_with(dir));
        if touches(&self.config.bricks) {
            self.rescan_bricks();
        }
        if touches(&self.config.clients) {
            self.rescan_client_kinds();
        }
    }

    /// Reads the bricks directory again and announces the difference.
    fn rescan_bricks(&self) {
        let _serial = self.rescan.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(mut next) = scan_bricks(&self.config.bricks) else {
            return;
        };
        let mut changes = Vec::new();
        {
            let mut bricks = self.bricks.write().unwrap_or_else(PoisonError::into_inner);
            for (name, declaration) in next.iter_mut() {
                match bricks.get(name) {
                    None => changes.push(DiscoveryChange::BrickAdded(name.clone())),
                    // Unchanged: keep the declaration already handed out.
                    Some(held) if **held == **declaration => *declaration = Arc::clone(held),
                    Some(_) => changes.push(DiscoveryChange::BrickChanged(name.clone())),
                }
            }
            for name in bricks.keys() {
                if !next.contains_key(name) {
                    changes.push(DiscoveryChange::BrickRemoved(name.clone()));
                }
            }
            *bricks = next;
        }
        // Announced once the new state is visible.
        for change in changes {
            tracing::info!(?change, "brick declarations changed");
            // No receiver is not an error: nobody follows the changes yet.
            let _ = self.changes.send(change);
        }
    }

    fn rescan_client_kinds(&self) {
        let _serial = self.rescan.lock().unwrap_or_else(PoisonError::into_inner);
        let kinds = load_client_kinds(&self.config.embedded_clients, &self.config.clients);
        *self
            .client_kinds
            .write()
            .unwrap_or_else(PoisonError::into_inner) = kinds;
    }
}

/// Whether an event may change the entries of a directory. Opening or reading
/// a file does not, and reading the directory's own files must not trigger a
/// new read.
fn changes_entries(kind: &EventKind) -> bool {
    match kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        _ => true,
    }
}

/// The valid declarations the bricks directory activates, by name. `None`
/// when the directory cannot be read: what is held stays as is.
fn scan_bricks(dir: &Path) -> Option<BTreeMap<BrickName, Arc<BrickDeclaration>>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(dir = %dir.display(), "cannot read the bricks directory: {e}");
            return None;
        }
    };
    let mut bricks = BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(declaration) = activated(&path) {
            bricks.insert(declaration.name.clone(), Arc::new(declaration));
        }
    }
    Some(bricks)
}

/// The declaration an entry of the bricks directory activates. Anything other
/// than a valid declaration under a link named `<name>.toml` is left out and
/// logged, files not ending in `.toml` silently.
fn activated(path: &Path) -> Option<BrickDeclaration> {
    let file = path.file_name()?.to_str();
    let Some(stem) = file.and_then(|f| f.strip_suffix(".toml")) else {
        if file.is_none() {
            tracing::warn!(path = %path.display(), "brick link name is not UTF-8, ignored");
        }
        return None;
    };
    let declaration = match load_manifest(path) {
        Ok(declaration) => declaration,
        Err(e) => {
            tracing::warn!(path = %path.display(), "brick declaration refused: {e}");
            return None;
        }
    };
    if declaration.name.as_str() != stem {
        tracing::warn!(
            path = %path.display(),
            "brick declaration refused: it declares {:?}, so its link must be named {}.toml",
            declaration.name.as_str(),
            declaration.name,
        );
        return None;
    }
    Some(declaration)
}

fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use notify::event::{AccessKind, AccessMode, CreateKind, ModifyKind, RemoveKind, RenameMode};

    use super::*;

    #[test]
    fn reading_a_file_does_not_trigger_a_read() {
        for access in [
            AccessKind::Open(AccessMode::Any),
            AccessKind::Read,
            AccessKind::Close(AccessMode::Read),
        ] {
            assert!(!changes_entries(&EventKind::Access(access)), "{access:?}");
        }
    }

    #[test]
    fn changing_an_entry_triggers_a_read() {
        for kind in [
            EventKind::Create(CreateKind::Any),
            EventKind::Remove(RemoveKind::Any),
            EventKind::Modify(ModifyKind::Name(RenameMode::To)),
            EventKind::Access(AccessKind::Close(AccessMode::Write)),
            EventKind::Any,
        ] {
            assert!(changes_entries(&kind), "{kind:?}");
        }
    }
}
