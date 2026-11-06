use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use nowcast_types::{BrickName, FlowId, QualifiedFlowId};

use crate::{AnchorId, BrickUnavailable, ResolveError, Resolved, Router, Supervisor, UnknownFlow};

#[derive(Default)]
struct State {
    failures: HashMap<BrickName, ResolveError>,
    resolved: HashSet<QualifiedFlowId>,
    refreshes: Vec<QualifiedFlowId>,
}

/// Resolves `<brick>/<kind>:<params>` without any brick, unless the test set
/// the brick to fail, and subscribes the anchor to it in the router it was
/// given.
pub struct FakeSupervisor {
    router: Arc<dyn Router>,
    state: Mutex<State>,
}

impl FakeSupervisor {
    pub fn new(router: Arc<dyn Router>) -> Self {
        Self {
            router,
            state: Mutex::default(),
        }
    }

    /// The brick misses its `ready` timeout from now on.
    pub fn set_unavailable(&self, brick: BrickName) {
        let error = ResolveError::Unavailable(BrickUnavailable(brick.clone()));
        self.state.lock().unwrap().failures.insert(brick, error);
    }

    /// The brick refuses every subscription from now on.
    pub fn set_refusal(&self, brick: BrickName, code: &str, message: &str) {
        let error = ResolveError::Refused {
            code: code.into(),
            message: message.into(),
        };
        self.state.lock().unwrap().failures.insert(brick, error);
    }

    /// The flows a refresh was asked for, in order.
    pub fn refreshes(&self) -> Vec<QualifiedFlowId> {
        self.state.lock().unwrap().refreshes.clone()
    }
}

impl Supervisor for FakeSupervisor {
    // An `async fn` does nothing until polled: dropped before that, the
    // resolution subscribes nobody.
    async fn resolve(
        &self,
        anchor: AnchorId,
        brick: &BrickName,
        kind: &str,
        params: &[u8],
    ) -> Result<Resolved, ResolveError> {
        let mut state = self.state.lock().unwrap();
        if let Some(error) = state.failures.get(brick) {
            return Err(error.clone());
        }
        let id = format!("{kind}:{}", String::from_utf8_lossy(params));
        let flow = FlowId::new(id).expect("fake flow ids stay under the length limit");
        let flow = QualifiedFlowId::new(brick.clone(), flow);
        state.resolved.insert(flow.clone());
        let (subscription, last) = self.router.subscribe(anchor, flow.clone());
        Ok(Resolved {
            subscription,
            flow,
            last,
        })
    }

    fn refresh(&self, flow: &QualifiedFlowId) -> Result<(), UnknownFlow> {
        let mut state = self.state.lock().unwrap();
        if !state.resolved.contains(flow) {
            return Err(UnknownFlow(flow.clone()));
        }
        state.refreshes.push(flow.clone());
        Ok(())
    }
}
