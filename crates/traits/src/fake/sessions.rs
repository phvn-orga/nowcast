use std::collections::HashMap;
use std::sync::Mutex;

use nowcast_types::SessionId;

use crate::{AnchorId, Sessions};

#[derive(Default)]
pub struct FakeSessions {
    sessions: Mutex<HashMap<AnchorId, SessionId>>,
}

impl FakeSessions {
    pub fn new() -> Self {
        Self::default()
    }

    /// What the registry does when an anchor dies.
    pub fn forget(&self, anchor: AnchorId) {
        self.sessions.lock().unwrap().remove(&anchor);
    }
}

impl Sessions for FakeSessions {
    fn declare(&self, anchor: AnchorId, session: SessionId) {
        self.sessions.lock().unwrap().insert(anchor, session);
    }

    fn declare_if_absent(&self, anchor: AnchorId, session: SessionId) {
        self.sessions
            .lock()
            .unwrap()
            .entry(anchor)
            .or_insert(session);
    }

    fn current(&self, anchor: AnchorId) -> Option<SessionId> {
        self.sessions.lock().unwrap().get(&anchor).cloned()
    }

    fn declared(&self) -> Vec<(AnchorId, SessionId)> {
        let mut declared: Vec<_> = self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .map(|(&anchor, session)| (anchor, session.clone()))
            .collect();
        declared.sort_by_key(|&(anchor, _)| anchor);
        declared
    }
}
