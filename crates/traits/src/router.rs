use std::fmt;

use nowcast_types::{Event, EventId, QualifiedFlowId, StreamEvent, SubscriptionId};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::AnchorId;

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

/// Flows, subscriptions and their delivery to anchors.
///
/// The router owns the subscriber count of each flow and the flow's last value.
/// It signals through [`Router::empty_flows`] when a flow loses its last
/// subscriber, and keeps the flow and its last value until
/// [`Router::end_flow`]: the supervisor decides when that happens.
///
/// Event ids come from one counter shared by every flow, so that they increase
/// within each flow and a single `Last-Event-ID` can be compared across flows.
pub trait Router: Send + Sync + 'static {
    /// Subscribes `anchor` to `flow`, creating the flow if needed. Every call
    /// issues a new subscription, even for the same anchor and flow.
    /// Returns the flow's last value along with it.
    fn subscribe(&self, anchor: AnchorId, flow: QualifiedFlowId)
    -> (SubscriptionId, Option<Event>);

    /// Removes one subscription of `anchor`. When it was the anchor's last
    /// subscription to the flow, the anchor's streams receive `flow-ended`.
    fn unsubscribe(
        &self,
        anchor: AnchorId,
        subscription: &SubscriptionId,
    ) -> Result<(), UnknownSubscription>;

    /// Stores a new value for `flow` and delivers it once to each stream of
    /// each subscribed anchor. The router assigns its id and time.
    fn publish(
        &self,
        flow: &QualifiedFlowId,
        data_type: String,
        data: Vec<u8>,
    ) -> Result<Event, UnknownFlow>;

    /// The last value of a flow `anchor` is subscribed to, `None` if the flow
    /// has published nothing yet.
    fn last(&self, anchor: AnchorId, flow: &QualifiedFlowId) -> Result<Option<Event>, UnknownFlow>;

    /// Delivers `flow-error` to the anchors subscribed to `flow`.
    fn flow_error(&self, flow: &QualifiedFlowId, message: String);

    /// Deletes the flow: its subscribed anchors receive `flow-ended`, its
    /// subscriptions and last value are dropped. Does not signal an empty flow.
    fn end_flow(&self, flow: &QualifiedFlowId);

    /// Opens a stream of everything delivered to `anchor`, including flows it
    /// subscribes to later. With `since`, first sends the last value of each
    /// subscribed flow whose id is greater than `since`; without it, sends no
    /// past value. The stream closes when the anchor is removed.
    fn stream(&self, anchor: AnchorId, since: Option<EventId>) -> UnboundedReceiver<StreamEvent>;

    /// How many subscriptions `flow` has, from every anchor.
    fn subscribers(&self, flow: &QualifiedFlowId) -> usize;

    /// Receives each flow whose last subscription goes away, at that moment.
    fn empty_flows(&self) -> UnboundedReceiver<QualifiedFlowId>;

    /// Drops every subscription and stream of a dead anchor, signalling the
    /// flows this leaves empty. No `flow-ended` is sent: nobody is listening.
    fn remove_anchor(&self, anchor: AnchorId);
}
