use std::fmt;
use std::future::Future;

use nowcast_types::{ApiError, ApiErrorCode, BrickName, Event, QualifiedFlowId, SubscriptionId};

use crate::{AnchorId, UnknownFlow};

/// Where a brick is in its life (contract section 4), as
/// [`Supervisor::bricks`] reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrickState {
    /// Launched, waiting for `ready`.
    Starting,
    /// Ready, its grace period included.
    Running,
    /// Being stopped.
    Stopping,
    /// Crashed, waiting to be relaunched.
    Waiting,
    /// Given up on after too many crashes in a row, until a subscription
    /// relaunches it.
    GivenUp,
}

/// A brick the supervisor looks after, as [`Supervisor::bricks`] reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrickSummary {
    pub name: BrickName,
    pub state: BrickState,
    /// Its process's, `None` without a process or when unknown.
    pub pid: Option<u32>,
    /// Its crashes in a row, the ones the restart limit bounds.
    pub crashes: u32,
}

/// A brick cannot serve for now: its process could not be launched, or did
/// not answer `hello` with `ready` within [`nowcast_types::READY_TIMEOUT`]
/// and was stopped. The client gets `503` `brick_unavailable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrickUnavailable {
    pub brick: BrickName,
    /// Why its last launch failed, when it did: `program "node" not found in
    /// PATH /usr/bin:/bin`.
    pub cause: Option<String>,
}

impl BrickUnavailable {
    pub fn new(brick: BrickName) -> Self {
        Self { brick, cause: None }
    }
}

impl fmt::Display for BrickUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.cause {
            Some(cause) => write!(f, "brick {} cannot be launched: {cause}", self.brick),
            None => write!(f, "brick {} did not answer in time", self.brick),
        }
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

/// Why an input did not reach its brick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputError {
    /// The brick serves no such flow. The client gets `404` `unknown_flow`.
    UnknownFlow(UnknownFlow),
    /// The brick is not ready: starting, or waiting to be relaunched. The
    /// client gets `503` `brick_unavailable`.
    Unavailable(BrickUnavailable),
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownFlow(e) => e.fmt(f),
            Self::Unavailable(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for InputError {}

/// A subscription made by [`Supervisor::resolve`]: what the client's `201`
/// answer carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub subscription: SubscriptionId,
    pub flow: QualifiedFlowId,
    /// The flow's last value, `None` if it has published nothing yet.
    pub last: Option<Event>,
}

/// Starts and stops bricks on demand.
///
/// The supervisor launches a brick at its first subscription, stops it once
/// none of its flows has a subscriber (told by [`crate::Router::empty_flows`]),
/// relaunches it after a crash, and feeds what it publishes into the router.
///
/// It also makes the router subscriptions to its bricks' flows, so that each
/// one is ordered with the brick's publications and with its own decisions
/// to release a flow: a value the brick publishes right after `subscribed`
/// is not lost, and a flow left by its previous subscribers is not released
/// under a new one.
pub trait Supervisor: Send + Sync + 'static {
    /// Maps a subscription to its flow, launching the brick if needed, and
    /// subscribes `anchor` to that flow in the router. `params` are JSON
    /// bytes, already validated against the kind's schema.
    ///
    /// Every value the brick publishes on the flow once it has answered
    /// `subscribed` is either [`Resolved::last`] or delivered to the anchor's
    /// streams. On error, no subscription is made. Dropping the future before
    /// it completes leaves no subscription behind either: a client gone
    /// while its brick starts keeps nothing alive.
    fn resolve(
        &self,
        anchor: AnchorId,
        brick: &BrickName,
        kind: &str,
        params: &[u8],
    ) -> impl Future<Output = Result<Resolved, ResolveError>> + Send;

    /// Relays a refresh request to the brick serving `flow`.
    fn refresh(&self, flow: &QualifiedFlowId) -> Result<(), UnknownFlow>;

    /// Relays a refresh scope to `brick`, in one `refresh-scope`, if it runs
    /// with flows. `scope` is JSON bytes, already validated against the
    /// schema the brick declares. A stopped brick is not launched for it,
    /// and one not ready does not get it.
    fn refresh_scope(&self, brick: &BrickName, scope: Vec<u8>);

    /// The subscription kind the brick serves `flow` for, `None` if it serves
    /// no such flow. Two kinds mapped to one flow keep the first.
    fn kind(&self, flow: &QualifiedFlowId) -> Option<String>;

    /// Every brick with a process, waiting to be relaunched or given up on,
    /// by name. Any other brick is stopped, and left out.
    fn bricks(&self) -> Vec<BrickSummary>;

    /// Relays an input to the brick serving `flow`. `detail` is JSON bytes,
    /// already validated against the schema of `data_type`. Inputs are sent
    /// in the order of the calls; none is kept for a brick that is not ready.
    fn input(
        &self,
        flow: &QualifiedFlowId,
        data_type: &str,
        detail: Vec<u8>,
    ) -> Result<(), InputError>;
}
