use std::sync::{Mutex, MutexGuard, PoisonError};

use nowcast_traits::{AnchorId, FlowSummary, History, Router, UnknownFlow, UnknownSubscription};
use nowcast_types::{Event, EventId, QualifiedFlowId, StreamEvent, SubscriptionId};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::watch;

use crate::state::State;

/// The service's router: flows, subscriptions and their delivery to anchors,
/// held in memory.
///
/// Every operation runs under one lock and delivers through unbounded
/// channels, which never block. So each operation is atomic: a publication
/// reaches every stream in the order of event ids, and a subscription made
/// while publications go on misses none of them, since it either returns a
/// value as `last` or receives it on the anchor's streams.
pub struct FlowRouter {
    state: Mutex<State>,
}

impl Default for FlowRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl FlowRouter {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::new()),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // No operation panics while holding the lock, so the state is
        // consistent even if the lock is somehow poisoned.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Router for FlowRouter {
    fn subscribe(
        &self,
        anchor: AnchorId,
        flow: QualifiedFlowId,
    ) -> (SubscriptionId, Option<Event>) {
        let mut state = self.state();
        let subscribed = state.subscribe(anchor, flow);
        state.sync_has_subscriptions();
        subscribed
    }

    fn unsubscribe(
        &self,
        anchor: AnchorId,
        subscription: &SubscriptionId,
    ) -> Result<(), UnknownSubscription> {
        let mut state = self.state();
        let unsubscribed = state.unsubscribe(anchor, subscription);
        state.sync_has_subscriptions();
        unsubscribed
    }

    fn publish(
        &self,
        flow: &QualifiedFlowId,
        data_type: String,
        data: Vec<u8>,
    ) -> Result<Event, UnknownFlow> {
        self.state().publish(flow, data_type, data)
    }

    fn last(&self, anchor: AnchorId, flow: &QualifiedFlowId) -> Result<Option<Event>, UnknownFlow> {
        self.state().last(anchor, flow)
    }

    fn is_subscribed(&self, anchor: AnchorId, flow: &QualifiedFlowId) -> bool {
        self.state().is_subscribed(anchor, flow)
    }

    fn history(
        &self,
        anchor: AnchorId,
        flow: &QualifiedFlowId,
        after: EventId,
    ) -> Result<History, UnknownFlow> {
        self.state().history(anchor, flow, after)
    }

    fn flow_error(&self, flow: &QualifiedFlowId, message: String) {
        self.state().flow_error(flow, message);
    }

    fn end_flow(&self, flow: &QualifiedFlowId) {
        let mut state = self.state();
        state.end_flow(flow);
        state.sync_has_subscriptions();
    }

    fn stream(&self, anchor: AnchorId, since: Option<EventId>) -> UnboundedReceiver<StreamEvent> {
        self.state().stream(anchor, since)
    }

    fn subscribers(&self, flow: &QualifiedFlowId) -> usize {
        self.state().subscribers(flow)
    }

    fn flows(&self) -> Vec<FlowSummary> {
        self.state().flows()
    }

    fn subscriptions(&self, anchor: AnchorId) -> Vec<(SubscriptionId, QualifiedFlowId)> {
        self.state().subscriptions(anchor)
    }

    fn empty_flows(&self) -> UnboundedReceiver<QualifiedFlowId> {
        self.state().empty_flows()
    }

    fn has_subscriptions(&self) -> watch::Receiver<bool> {
        self.state().has_subscriptions()
    }

    fn remove_anchor(&self, anchor: AnchorId) {
        let mut state = self.state();
        state.remove_anchor(anchor);
        state.sync_has_subscriptions();
    }
}
