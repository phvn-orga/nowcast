use std::time::Duration;

use crate::{BrickName, FlowId, PublishErrorCode, RequestId};

/// The major version of the brick protocol, sent as `v` in every message.
pub const PROTOCOL_VERSION: u32 = 1;

/// How long a brick has to answer `hello` with `ready`.
pub const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// A message from the service to a brick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceMessage {
    /// Expects `ready`.
    Hello { name: BrickName, protocol: u32 },
    /// Expects `subscribed` or `error`.
    Subscribe {
        req: RequestId,
        kind: String,
        /// The JSON bytes of the subscription params.
        params: Vec<u8>,
    },
    /// The last subscriber of the flow left.
    Release { flow: FlowId },
    /// The brick publishes again once it has re-read the source.
    Refresh { flow: FlowId },
    /// The brick re-reads what the scope touches, and publishes again only
    /// what changed. Sent only to a brick that declares the scope's schema
    /// (`[refresh] scope`), already checked against it.
    RefreshScope {
        /// The JSON bytes of the scope.
        scope: Vec<u8>,
    },
    /// A subscriber of the flow sent a detail, already checked against the
    /// schema of `data_type`, the type the flow's kind accepts.
    Input {
        flow: FlowId,
        data_type: String,
        /// The JSON bytes of the detail.
        detail: Vec<u8>,
    },
    /// The brick exits.
    Shutdown,
    /// The `publish` with this `req` was accepted.
    Ack { req: RequestId },
    /// The `publish` with this `req` was refused.
    Error {
        req: RequestId,
        code: PublishErrorCode,
    },
}

impl ServiceMessage {
    /// The message type, `t` in the protocol.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::Subscribe { .. } => "subscribe",
            Self::Release { .. } => "release",
            Self::Refresh { .. } => "refresh",
            Self::RefreshScope { .. } => "refresh-scope",
            Self::Input { .. } => "input",
            Self::Shutdown => "shutdown",
            Self::Ack { .. } => "ack",
            Self::Error { .. } => "error",
        }
    }
}

/// A message from a brick to the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrickMessage {
    /// Answers `hello`.
    Ready,
    /// Answers `subscribe` with the flow the subscription maps to.
    Subscribed { req: RequestId, flow: FlowId },
    /// Answers `subscribe` when the brick cannot serve it. The brick picks the code.
    Error {
        req: RequestId,
        code: String,
        message: String,
    },
    /// A new value for a flow. Expects `ack` or `error`.
    Publish {
        req: RequestId,
        flow: FlowId,
        /// The published type, for example `forge.pr/v1`.
        data_type: String,
        /// The JSON bytes of the detail.
        detail: Vec<u8>,
    },
    /// The brick cannot serve the flow, relayed to its clients.
    FlowError { flow: FlowId, message: String },
}

impl BrickMessage {
    /// The message type, `t` in the protocol.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Subscribed { .. } => "subscribed",
            Self::Error { .. } => "error",
            Self::Publish { .. } => "publish",
            Self::FlowError { .. } => "flow-error",
        }
    }
}
