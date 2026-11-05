use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use nowcast_types::BrickName;
use tokio::sync::broadcast;

use crate::{BrickDeclaration, ClientKind, Discovery, DiscoveryChange};

/// Declarations the test sets directly, announcing each change.
pub struct FakeDiscovery {
    bricks: Mutex<BTreeMap<BrickName, Arc<BrickDeclaration>>>,
    client_kinds: Mutex<Vec<ClientKind>>,
    changes: broadcast::Sender<DiscoveryChange>,
}

impl Default for FakeDiscovery {
    fn default() -> Self {
        Self {
            bricks: Mutex::default(),
            client_kinds: Mutex::default(),
            changes: broadcast::Sender::new(64),
        }
    }
}

impl FakeDiscovery {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces a brick.
    pub fn insert(&self, brick: BrickDeclaration) {
        let name = brick.name.clone();
        let previous = self
            .bricks
            .lock()
            .unwrap()
            .insert(name.clone(), Arc::new(brick));
        let change = match previous {
            Some(_) => DiscoveryChange::BrickChanged(name),
            None => DiscoveryChange::BrickAdded(name),
        };
        let _ = self.changes.send(change);
    }

    pub fn remove(&self, name: &BrickName) {
        if self.bricks.lock().unwrap().remove(name).is_some() {
            let _ = self
                .changes
                .send(DiscoveryChange::BrickRemoved(name.clone()));
        }
    }

    pub fn set_client_kinds(&self, kinds: Vec<ClientKind>) {
        *self.client_kinds.lock().unwrap() = kinds;
    }
}

impl Discovery for FakeDiscovery {
    fn brick(&self, name: &BrickName) -> Option<Arc<BrickDeclaration>> {
        self.bricks.lock().unwrap().get(name).cloned()
    }

    fn bricks(&self) -> Vec<Arc<BrickDeclaration>> {
        self.bricks.lock().unwrap().values().cloned().collect()
    }

    fn client_kinds(&self) -> Vec<ClientKind> {
        self.client_kinds.lock().unwrap().clone()
    }

    fn changes(&self) -> broadcast::Receiver<DiscoveryChange> {
        self.changes.subscribe()
    }
}
