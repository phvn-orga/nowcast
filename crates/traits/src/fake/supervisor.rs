use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nowcast_types::{BrickName, FlowId, QualifiedFlowId};

use crate::{
    AnchorId, BrickUnavailable, InputError, ResolveError, Resolved, Router, Supervisor, UnknownFlow,
};

#[derive(Default)]
struct State {
    failures: HashMap<BrickName, ResolveError>,
    /// The kind of each flow resolved.
    resolved: HashMap<QualifiedFlowId, String>,
    refreshes: Vec<QualifiedFlowId>,
    inputs: Vec<(QualifiedFlowId, String, Vec<u8>)>,
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

    /// The inputs relayed, in order: flow, type and JSON bytes.
    pub fn inputs(&self) -> Vec<(QualifiedFlowId, String, Vec<u8>)> {
        self.state.lock().unwrap().inputs.clone()
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
        state.resolved.insert(flow.clone(), kind.to_owned());
        let (subscription, last) = self.router.subscribe(anchor, flow.clone());
        Ok(Resolved {
            subscription,
            flow,
            last,
        })
    }

    fn refresh(&self, flow: &QualifiedFlowId) -> Result<(), UnknownFlow> {
        let mut state = self.state.lock().unwrap();
        if !state.resolved.contains_key(flow) {
            return Err(UnknownFlow(flow.clone()));
        }
        state.refreshes.push(flow.clone());
        Ok(())
    }

    fn kind(&self, flow: &QualifiedFlowId) -> Option<String> {
        self.state.lock().unwrap().resolved.get(flow).cloned()
    }

    fn input(
        &self,
        flow: &QualifiedFlowId,
        data_type: &str,
        detail: Vec<u8>,
    ) -> Result<(), InputError> {
        let mut state = self.state.lock().unwrap();
        if !state.resolved.contains_key(flow) {
            return Err(InputError::UnknownFlow(UnknownFlow(flow.clone())));
        }
        if let Some(ResolveError::Unavailable(e)) = state.failures.get(flow.brick()) {
            return Err(InputError::Unavailable(e.clone()));
        }
        state
            .inputs
            .push((flow.clone(), data_type.to_owned(), detail));
        Ok(())
    }
}
