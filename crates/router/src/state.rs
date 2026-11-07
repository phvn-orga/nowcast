//! The router's bookkeeping, kept free of locking so that every operation is
//! one plain method on [`State`].
//!
//! Subscriptions are indexed both ways: by flow, to fan a publication out to
//! the anchors subscribed to it, and by anchor, to check who owns a
//! subscription, replay a flow's history on a new stream and drop a dead anchor
//! without scanning every flow. Every method keeps the two indexes in step.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use nowcast_traits::{AnchorId, FLOW_HISTORY, History, UnknownFlow, UnknownSubscription};
use nowcast_types::{Event, EventId, QualifiedFlowId, StreamEvent, SubscriptionId};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::watch;

/// A flow that has had at least one subscriber and has not been ended.
#[derive(Default)]
struct Flow {
    /// The last [`FLOW_HISTORY`] events, oldest first.
    history: VecDeque<Event>,
    /// The id of the newest event the bound dropped.
    dropped: Option<EventId>,
    /// Every subscription to the flow, with the anchor that holds it.
    subscriptions: HashMap<SubscriptionId, AnchorId>,
}

impl Flow {
    fn last(&self) -> Option<&Event> {
        self.history.back()
    }

    /// The kept events whose id is greater than `since`. The history is in
    /// increasing id order, so the first one is found by binary search.
    fn after(&self, since: EventId) -> impl Iterator<Item = &Event> {
        let first = self.history.partition_point(|event| event.id <= since);
        self.history.range(first..)
    }

    /// Whether the bound dropped an event newer than `since`.
    fn dropped_after(&self, since: EventId) -> bool {
        self.dropped.is_some_and(|dropped| dropped > since)
    }

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
    /// The counter's value at startup: an id at or below it comes from a
    /// previous run, whose history is lost.
    start: u64,
    /// The number of subscriptions issued so far.
    issued_subscriptions: u64,
    flows: HashMap<QualifiedFlowId, Flow>,
    anchors: HashMap<AnchorId, Anchor>,
    empty_watchers: Vec<UnboundedSender<QualifiedFlowId>>,
    /// Whether any subscription exists, updated by every operation that adds
    /// or drops one.
    has_subscriptions: watch::Sender<bool>,
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
            start: now,
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
        let last = entry.last().cloned();
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
        flow.history.push_back(event.clone());
        if flow.history.len() > FLOW_HISTORY {
            flow.dropped = flow.history.pop_front().map(|dropped| dropped.id);
        }
        let update = StreamEvent::Update(event.clone());
        for anchor in flow.anchors() {
            if let Some(anchor) = self.anchors.get_mut(&anchor) {
                anchor.deliver(&update);
            }
        }
        Ok(event)
    }

    pub(crate) fn is_subscribed(&self, anchor: AnchorId, flow_id: &QualifiedFlowId) -> bool {
        self.flows
            .get(flow_id)
            .is_some_and(|flow| flow.has_anchor(anchor))
    }

    pub(crate) fn last(
        &self,
        anchor: AnchorId,
        flow_id: &QualifiedFlowId,
    ) -> Result<Option<Event>, UnknownFlow> {
        match self.flows.get(flow_id) {
            Some(flow) if flow.has_anchor(anchor) => Ok(flow.last().cloned()),
            _ => Err(UnknownFlow(flow_id.clone())),
        }
    }

    pub(crate) fn history(
        &self,
        anchor: AnchorId,
        flow_id: &QualifiedFlowId,
        after: EventId,
    ) -> Result<History, UnknownFlow> {
        match self.flows.get(flow_id) {
            Some(flow) if flow.has_anchor(anchor) => Ok(History {
                gap: self.gap(flow, after),
                events: flow.after(after).cloned().collect(),
            }),
            _ => Err(UnknownFlow(flow_id.clone())),
        }
    }

    /// Whether some events of `flow` newer than `since` are gone.
    fn gap(&self, flow: &Flow, since: EventId) -> bool {
        since.0 <= self.start || flow.dropped_after(since)
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
        let mut flows: Vec<(&QualifiedFlowId, &Flow)> = self
            .anchors
            .get(&anchor)
            .into_iter()
            .flat_map(|entry| entry.subscriptions.values())
            .collect::<HashSet<_>>()
            .into_iter()
            .filter_map(|id| Some((id, self.flows.get(id)?)))
            .collect();
        flows.sort_by_key(|(id, _)| *id);
        let mut replay: Vec<&Event> = match since {
            None => flows.iter().filter_map(|(_, flow)| flow.last()).collect(),
            Some(since) => {
                for (id, flow) in &flows {
                    if self.gap(flow, since) {
                        // The receiver is still in hand: the send cannot fail.
                        let _ = tx.send(StreamEvent::FlowGap {
                            flow: (*id).clone(),
                        });
                    }
                }
                flows
                    .iter()
                    .flat_map(|(_, flow)| flow.after(since))
                    .collect()
            }
        };
        replay.sort_by_key(|event| event.id);
        for event in replay {
            let _ = tx.send(StreamEvent::Update(event.clone()));
        }
        let entry = self.anchors.entry(anchor).or_default();
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

    pub(crate) fn has_subscriptions(&self) -> watch::Receiver<bool> {
        self.has_subscriptions.subscribe()
    }

    /// Publishes whether any subscription is left. Every operation that adds
    /// or drops a subscription calls it before returning, under the router's
    /// lock, so that the value never lags behind an operation.
    pub(crate) fn sync_has_subscriptions(&self) {
        let any = self.anchors.values().any(|a| !a.subscriptions.is_empty());
        self.has_subscriptions.send_if_modified(|was| {
            let changed = *was != any;
            *was = any;
            changed
        });
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
