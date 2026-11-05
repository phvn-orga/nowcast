use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Mutex;

use nowcast_types::{BrickName, FlowId, QualifiedFlowId};

use crate::{BrickUnavailable, ResolveError, Supervisor, UnknownFlow};

#[derive(Default)]
struct State {
    failures: HashMap<BrickName, ResolveError>,
    resolved: HashSet<QualifiedFlowId>,
    refreshes: Vec<QualifiedFlowId>,
}

/// Resolves `<brick>/<kind>:<params>` without any brick, unless the test set
/// the brick to fail.
#[derive(Default)]
pub struct FakeSupervisor {
    state: Mutex<State>,
}

impl FakeSupervisor {
    pub fn new() -> Self {
        Self::default()
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
    fn resolve(
        &self,
        brick: &BrickName,
        kind: &str,
        params: &[u8],
    ) -> impl Future<Output = Result<QualifiedFlowId, ResolveError>> + Send {
        let mut state = self.state.lock().unwrap();
        let result = match state.failures.get(brick) {
            Some(error) => Err(error.clone()),
            None => {
                let id = format!("{kind}:{}", String::from_utf8_lossy(params));
                let flow = FlowId::new(id).expect("fake flow ids stay under the length limit");
                let flow = QualifiedFlowId::new(brick.clone(), flow);
                state.resolved.insert(flow.clone());
                Ok(flow)
            }
        };
        std::future::ready(result)
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
