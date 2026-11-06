use std::fmt;
use std::future::Future;

use nowcast_types::{ApiError, ApiErrorCode, BrickName, QualifiedFlowId};

use crate::UnknownFlow;

/// A brick did not answer `hello` with `ready` within
/// [`nowcast_types::READY_TIMEOUT`]. It is stopped, and the client gets `503`
/// `brick_unavailable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrickUnavailable(pub BrickName);

impl fmt::Display for BrickUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "brick {} did not answer in time", self.0)
    }
}

impl std::error::Error for BrickUnavailable {}

impl From<BrickUnavailable> for ApiError {
    fn from(e: BrickUnavailable) -> Self {
        Self {
            code: ApiErrorCode::BrickUnavailable,
            message: e.to_string(),
        }
    }
}

/// Why a subscription could not be mapped to a flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    UnknownBrick(BrickName),
    Unavailable(BrickUnavailable),
    /// The brick answered `subscribe` with `error`. The client gets `422`
    /// `subscription_refused`.
    Refused {
        code: String,
        message: String,
    },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBrick(name) => write!(f, "unknown brick {name}"),
            Self::Unavailable(e) => e.fmt(f),
            Self::Refused { code, message } => write!(f, "brick refused: {code}: {message}"),
        }
    }
}

impl std::error::Error for ResolveError {}

impl From<ResolveError> for ApiError {
    fn from(e: ResolveError) -> Self {
        match e {
            ResolveError::UnknownBrick(name) => Self {
                code: ApiErrorCode::UnknownBrick,
                message: format!("unknown brick {name}"),
            },
            ResolveError::Unavailable(e) => e.into(),
            ResolveError::Refused { code, message } => Self {
                code: ApiErrorCode::SubscriptionRefused,
                message: format!("{code}: {message}"),
            },
        }
    }
}

/// Starts and stops bricks on demand.
///
/// The supervisor launches a brick at its first subscription, stops it once
/// none of its flows has a subscriber (told by [`crate::Router::empty_flows`]),
/// relaunches it after a crash, and feeds what it publishes into the router.
pub trait Supervisor: Send + Sync + 'static {
    /// Maps a subscription to its flow, launching the brick if needed. `params`
    /// are JSON bytes, already validated against the kind's schema.
    fn resolve(
        &self,
        brick: &BrickName,
        kind: &str,
        params: &[u8],
    ) -> impl Future<Output = Result<QualifiedFlowId, ResolveError>> + Send;

    /// Relays a refresh request to the brick serving `flow`.
    fn refresh(&self, flow: &QualifiedFlowId) -> Result<(), UnknownFlow>;
}
