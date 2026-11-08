use std::fmt;
use std::time::SystemTime;

use nowcast_types::{Event, EventId, QualifiedFlowId, StreamEvent, SubscriptionId};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::watch;

use crate::AnchorId;

/// A flow as [`Router::flows`] sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowSummary {
    pub flow: QualifiedFlowId,
    /// Its subscriptions, from every anchor, as [`Router::subscribers`] counts them.
    pub subscribers: usize,
    /// The id and time of its last kept event, `None` if it has published
    /// nothing yet.
    pub last: Option<(EventId, SystemTime)>,
}

/// The flow is unknown to the router, or the anchor is not subscribed to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownFlow(pub QualifiedFlowId);

impl fmt::Display for UnknownFlow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown flow {}", self.0)
    }
}

impl std::error::Error for UnknownFlow {}

/// The subscription does not exist, or belongs to another anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownSubscription(pub SubscriptionId);

impl fmt::Display for UnknownSubscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown subscription {}", self.0)
    }
}

impl std::error::Error for UnknownSubscription {}

/// How many events the router keeps per flow (contract section 3). Past it,
/// the oldest is dropped.
pub const FLOW_HISTORY: usize = 100;

/// The kept events of a flow after an id, as [`Router::history`] returns them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct History {
    /// Some events newer than the id are gone: dropped by the bound, or the id
    /// comes from before this run of the service.
    pub gap: bool,
    /// In increasing id order.
    pub events: Vec<Event>,
}

/// Flows, subscriptions and their delivery to anchors.
///
/// The router owns the subscriber count of each flow and the flow's history,
/// its last [`FLOW_HISTORY`] events. It signals through
/// [`Router::empty_flows`] when a flow loses its last subscriber, and keeps the
/// flow and its history until [`Router::end_flow`]: the supervisor decides when
/// that happens.
///
/// Event ids come from one counter shared by every flow, so that they increase
/// within each flow and a single `Last-Event-ID` can be compared across flows.
/// The counter starts above every id of a previous run: an id at or below its
/// start comes from before this run, whose history is lost.
pub trait Router: Send + Sync + 'static {
    /// Subscribes `anchor` to `flow`, creating the flow if needed. Every call
    /// issues a new subscription, even for the same anchor and flow.
    /// Returns the flow's last value along with it. The supervisor makes
    /// these calls, from [`crate::Supervisor::resolve`].
    fn subscribe(&self, anchor: AnchorId, flow: QualifiedFlowId)
    -> (SubscriptionId, Option<Event>);

    /// Removes one subscription of `anchor`. When it was the anchor's last
    /// subscription to the flow, the anchor's streams receive `flow-ended`.
    fn unsubscribe(
        &self,
        anchor: AnchorId,
        subscription: &SubscriptionId,
    ) -> Result<(), UnknownSubscription>;

    /// Adds a new value to the history of `flow`, dropping the oldest past
    /// [`FLOW_HISTORY`], and delivers it once to each stream of each
    /// subscribed anchor. The router assigns its id and time.
    fn publish(
        &self,
        flow: &QualifiedFlowId,
        data_type: String,
        data: Vec<u8>,
    ) -> Result<Event, UnknownFlow>;

    /// The last value of a flow `anchor` is subscribed to, `None` if the flow
    /// has published nothing yet.
    fn last(&self, anchor: AnchorId, flow: &QualifiedFlowId) -> Result<Option<Event>, UnknownFlow>;

    /// Whether `anchor` is subscribed to `flow`, without reading its value.
    fn is_subscribed(&self, anchor: AnchorId, flow: &QualifiedFlowId) -> bool {
        self.last(anchor, flow).is_ok()
    }

    /// The kept events of a flow `anchor` is subscribed to whose id is greater
    /// than `after`, with whether some of them are gone.
    fn history(
        &self,
        anchor: AnchorId,
        flow: &QualifiedFlowId,
        after: EventId,
    ) -> Result<History, UnknownFlow>;

    /// Delivers `flow-error` to the anchors subscribed to `flow`.
    fn flow_error(&self, flow: &QualifiedFlowId, message: String);

    /// Deletes the flow: its subscribed anchors receive `flow-ended`, its
    /// subscriptions and history are dropped. Does not signal an empty flow.
    fn end_flow(&self, flow: &QualifiedFlowId);

    /// Opens a stream of everything delivered to `anchor`, including flows it
    /// subscribes to later. First sends, without `since`, the last value of
    /// each subscribed flow. With `since`, it sends `flow-gap` for each
    /// subscribed flow whose [`Router::history`] after `since` has a gap, then
    /// the kept events of every subscribed flow whose id is greater than
    /// `since`, in increasing id order. The stream closes when the anchor is
    /// removed.
    fn stream(&self, anchor: AnchorId, since: Option<EventId>) -> UnboundedReceiver<StreamEvent>;

    /// How many subscriptions `flow` has, from every anchor.
    fn subscribers(&self, flow: &QualifiedFlowId) -> usize;

    /// Every flow the router holds, by flow id: those left without a
    /// subscriber included, until they are ended.
    fn flows(&self) -> Vec<FlowSummary>;

    /// The subscriptions of `anchor` with their flow, by flow then
    /// subscription id; none for an anchor the router does not know.
    fn subscriptions(&self, anchor: AnchorId) -> Vec<(SubscriptionId, QualifiedFlowId)>;

    /// Receives each flow whose last subscription goes away, at that moment.
    fn empty_flows(&self) -> UnboundedReceiver<QualifiedFlowId>;

    /// Whether any subscription exists, from any anchor, on any flow. The
    /// receiver sees every change, made in the same step as the operation
    /// that causes it: an `end_flow` or a `remove_anchor` as much as an
    /// `unsubscribe`. The server keeps the service running while it is
    /// `true`.
    fn has_subscriptions(&self) -> watch::Receiver<bool>;

    /// Drops every subscription and stream of a dead anchor, signalling the
    /// flows this leaves empty. No `flow-ended` is sent: nobody is listening.
    fn remove_anchor(&self, anchor: AnchorId);
}
