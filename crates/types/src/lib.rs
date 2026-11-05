//! Values shared by every module: names, identifiers, events, protocol messages and error codes.
//!
//! These types carry no encoding. The brick codec maps the protocol messages to
//! MessagePack, the HTTP server maps events and errors to JSON.

mod error;
mod event;
mod flow;
mod id;
mod name;
mod protocol;

pub use error::{ApiError, ApiErrorCode, PublishErrorCode, UnknownCode};
pub use event::{Event, SPEC_VERSION, StreamEvent};
pub use flow::{FLOW_ID_MAX_LEN, FlowId, FlowIdError, QualifiedFlowId, QualifiedFlowIdError};
pub use id::{EventId, RequestId, SessionId, SubscriptionId};
pub use name::{BrickName, ClientKindName, NameError};
pub use protocol::{BrickMessage, PROTOCOL_VERSION, READY_TIMEOUT, ServiceMessage};
