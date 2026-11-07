use std::time::SystemTime;

use crate::{EventId, QualifiedFlowId};

/// The CloudEvents `specversion` of every event.
pub const SPEC_VERSION: &str = "1.0";

/// A value published on a flow, as a CloudEvents event.
///
/// `source`, `subject` and `dataschema` derive from the flow and the type, so
/// they cannot disagree with them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: EventId,
    pub flow: QualifiedFlowId,
    /// The published type, for example `forge.pr/v1`.
    pub data_type: String,
    pub time: SystemTime,
    /// The brick's JSON detail, passed through as is once its syntax is checked.
    pub data: Vec<u8>,
}

impl Event {
    /// `nowcast://bricks/<brick>`
    pub fn source(&self) -> String {
        format!("nowcast://bricks/{}", self.flow.brick())
    }

    /// The qualified flow id.
    pub fn subject(&self) -> &QualifiedFlowId {
        &self.flow
    }

    /// `nowcast://bricks/<brick>/types/<type>`
    pub fn dataschema(&self) -> String {
        format!("{}/types/{}", self.source(), self.data_type)
    }
}

/// What the event stream of a session carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// A new value on a flow.
    Update(Event),
    /// The brick cannot serve the flow.
    FlowError {
        flow: QualifiedFlowId,
        message: String,
    },
    /// The flow no longer exists: brick removed, subscription deleted.
    FlowEnded { flow: QualifiedFlowId },
    /// Some of what the client missed on the flow is gone: the history's bound
    /// dropped it, or it went with a previous run of the service.
    FlowGap { flow: QualifiedFlowId },
}

impl StreamEvent {
    /// The SSE `event:` name.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Update(_) => "update",
            Self::FlowError { .. } => "flow-error",
            Self::FlowEnded { .. } => "flow-ended",
            Self::FlowGap { .. } => "flow-gap",
        }
    }

    pub fn flow(&self) -> &QualifiedFlowId {
        match self {
            Self::Update(event) => &event.flow,
            Self::FlowError { flow, .. } | Self::FlowEnded { flow } | Self::FlowGap { flow } => {
                flow
            }
        }
    }
}
