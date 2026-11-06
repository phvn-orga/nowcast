//! The router's bookkeeping, kept free of locking so that every operation is
//! one plain method on [`State`].
//!
//! Subscriptions are indexed both ways: by flow, to fan a publication out to
//! the anchors subscribed to it, and by anchor, to check who owns a
//! subscription, replay last values on a new stream and drop a dead anchor
//! without scanning every flow. Every method keeps the two indexes in step.

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use nowcast_traits::{AnchorId, UnknownFlow, UnknownSubscription};
use nowcast_types::{Event, EventId, QualifiedFlowId, StreamEvent, SubscriptionId};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// A flow that has had at least one subscriber and has not been ended.
#[derive(Default)]
struct Flow {
    last: Option<Event>,
    /// Every subscription to the flow, with the anchor that holds it.
    subscriptions: HashMap<SubscriptionId, AnchorId>,
}

impl Flow {
    fn has_anchor(&self, anchor: AnchorId) -> bool {
        self.subscriptions.values().any(|&a| a == anchor)
    }

    /// Each subscribed anchor once, whatever its number of subscriptions.
    fn anchors(&self) -> HashSet<AnchorId> {
        self.subscriptions.values().copied().collect()
    }
}

/// What the router knows of one anchor: its subscriptions and open streams.
#[derive(Default)]
struct Anchor {
    subscriptions: HashMap<SubscriptionId, QualifiedFlowId>,
    streams: Vec<UnboundedSender<StreamEvent>>,
}

impl Anchor {
    /// Sends `event` to every open stream, forgetting the closed ones.
    fn deliver(&mut self, event: &StreamEvent) {
        self.streams.retain(|s| s.send(event.clone()).is_ok());
    }

    fn is_unused(&mut self) -> bool {
        self.streams.retain(|s| !s.is_closed());
        self.subscriptions.is_empty() && self.streams.is_empty()
    }
}

#[derive(Default)]
pub(crate) struct State {
    /// The id of the last event issued, shared by every flow. Starts at the
    /// clock, in microseconds since 1970, so that ids keep increasing across
    /// a restart of the service: a client's `Last-Event-ID` from before the
    /// restart stays below every new id.
    last_event: u64,
    /// The number of subscriptions issued so far.
    issued_subscriptions: u64,
    flows: HashMap<QualifiedFlowId, Flow>,
    anchors: HashMap<AnchorId, Anchor>,
    empty_watchers: Vec<UnboundedSender<QualifiedFlowId>>,
}

impl State {
    pub(crate) fn new() -> Self {
        // A clock set before 1970 only loses the guarantee across restarts.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_micros()).unwrap_or(u64::MAX / 2)
            });
        Self {
            last_event: now,
            ..Self::default()
        }
    }

    pub(crate) fn subscribe(
        &mut self,
        anchor: AnchorId,
        flow: QualifiedFlowId,
    ) -> (SubscriptionId, Option<Event>) {
        self.issued_subscriptions += 1;
        let id = SubscriptionId::new(format!("s-{:x}", self.issued_subscriptions));
        let entry = self.flows.entry(flow.clone()).or_default();
        entry.subscriptions.insert(id.clone(), anchor);
        let last = entry.last.clone();
        self.anchors
            .entry(anchor)
            .or_default()
            .subscriptions
            .insert(id.clone(), flow);
        (id, last)
    }

    pub(crate) fn unsubscribe(
        &mut self,
        anchor: AnchorId,
        subscription: &SubscriptionId,
    ) -> Result<(), UnknownSubscription> {
        let unknown = || UnknownSubscription(subscription.clone());
        let owner = self.anchors.get_mut(&anchor).ok_or_else(unknown)?;
        let flow_id = owner
            .subscriptions
            .remove(subscription)
            .ok_or_else(unknown)?;
        if let Some(flow) = self.flows.get_mut(&flow_id) {
            flow.subscriptions.remove(subscription);
            if !flow.has_anchor(anchor) {
                owner.deliver(&StreamEvent::FlowEnded {
                    flow: flow_id.clone(),
                });
            }
            if flow.subscriptions.is_empty() {
                self.signal_empty(&flow_id);
            }
        }
        self.forget_if_unused(anchor);
        Ok(())
    }

    pub(crate) fn publish(
        &mut self,
        flow_id: &QualifiedFlowId,
        data_type: String,
        data: Vec<u8>,
    ) -> Result<Event, UnknownFlow> {
        let flow = self
            .flows
            .get_mut(flow_id)
            .ok_or_else(|| UnknownFlow(flow_id.clone()))?;
        self.last_event += 1;
        let event = Event {
            id: EventId(self.last_event),
            flow: flow_id.clone(),
            data_type,
            time: SystemTime::now(),
            data,
        };
        flow.last = Some(event.clone());
        let update = StreamEvent::Update(event.clone());
        for anchor in flow.anchors() {
            if let Some(anchor) = self.anchors.get_mut(&anchor) {
                anchor.deliver(&update);
            }
        }
        Ok(event)
    }

    pub(crate) fn last(
        &self,
        anchor: AnchorId,
        flow_id: &QualifiedFlowId,
    ) -> Result<Option<Event>, UnknownFlow> {
        match self.flows.get(flow_id) {
            Some(flow) if flow.has_anchor(anchor) => Ok(flow.last.clone()),
            _ => Err(UnknownFlow(flow_id.clone())),
        }
    }

    pub(crate) fn flow_error(&mut self, flow_id: &QualifiedFlowId, message: String) {
        let Some(flow) = self.flows.get(flow_id) else {
            return;
        };
        let event = StreamEvent::FlowError {
            flow: flow_id.clone(),
            message,
        };
        for anchor in flow.anchors() {
            if let Some(anchor) = self.anchors.get_mut(&anchor) {
                anchor.deliver(&event);
            }
        }
    }

    pub(crate) fn end_flow(&mut self, flow_id: &QualifiedFlowId) {
        let Some(flow) = self.flows.remove(flow_id) else {
            return;
        };
        let event = StreamEvent::FlowEnded {
            flow: flow_id.clone(),
        };
        let anchors = flow.anchors();
        for (subscription, anchor) in flow.subscriptions {
            if let Some(anchor) = self.anchors.get_mut(&anchor) {
                anchor.subscriptions.remove(&subscription);
            }
        }
        for anchor in anchors {
            if let Some(entry) = self.anchors.get_mut(&anchor) {
                entry.deliver(&event);
            }
            self.forget_if_unused(anchor);
        }
    }

    pub(crate) fn stream(
        &mut self,
        anchor: AnchorId,
        since: Option<EventId>,
    ) -> UnboundedReceiver<StreamEvent> {
        let (tx, rx) = unbounded_channel();
        let entry = self.anchors.entry(anchor).or_default();
        let flows: HashSet<&QualifiedFlowId> = entry.subscriptions.values().collect();
        let mut replay: Vec<&Event> = flows
            .into_iter()
            .filter_map(|flow| self.flows.get(flow)?.last.as_ref())
            .filter(|event| since.is_none_or(|since| event.id > since))
            .collect();
        replay.sort_by_key(|event| event.id);
        for event in replay {
            // The receiver is still in hand: the send cannot fail.
            let _ = tx.send(StreamEvent::Update(event.clone()));
        }
        entry.streams.retain(|s| !s.is_closed());
        entry.streams.push(tx);
        rx
    }

    pub(crate) fn subscribers(&self, flow_id: &QualifiedFlowId) -> usize {
        self.flows
            .get(flow_id)
            .map_or(0, |flow| flow.subscriptions.len())
    }

    pub(crate) fn empty_flows(&mut self) -> UnboundedReceiver<QualifiedFlowId> {
        let (tx, rx) = unbounded_channel();
        self.empty_watchers.push(tx);
        rx
    }

    pub(crate) fn remove_anchor(&mut self, anchor: AnchorId) {
        // Dropping the entry drops its senders, which closes its streams.
        let Some(entry) = self.anchors.remove(&anchor) else {
            return;
        };
        for (subscription, flow_id) in entry.subscriptions {
            let Some(flow) = self.flows.get_mut(&flow_id) else {
                continue;
            };
            flow.subscriptions.remove(&subscription);
            // Becomes true once per flow: on the anchor's last subscription to
            // it, and only if no other anchor holds one.
            if flow.subscriptions.is_empty() {
                self.signal_empty(&flow_id);
            }
        }
    }

    fn signal_empty(&mut self, flow_id: &QualifiedFlowId) {
        self.empty_watchers
            .retain(|watcher| watcher.send(flow_id.clone()).is_ok());
    }

    /// Drops an anchor that holds no subscription and no open stream, so that
    /// short-lived anchors do not pile up.
    fn forget_if_unused(&mut self, anchor: AnchorId) {
        if self.anchors.get_mut(&anchor).is_some_and(Anchor::is_unused) {
            self.anchors.remove(&anchor);
        }
    }
}
