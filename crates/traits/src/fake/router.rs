use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

use nowcast_types::{Event, EventId, QualifiedFlowId, StreamEvent, SubscriptionId};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::{AnchorId, Router, UnknownFlow, UnknownSubscription};

#[derive(Default)]
struct Flow {
    last: Option<Event>,
    subscriptions: HashMap<SubscriptionId, AnchorId>,
}

impl Flow {
    fn has_anchor(&self, anchor: AnchorId) -> bool {
        self.subscriptions.values().any(|&a| a == anchor)
    }

    /// Each subscribed anchor once.
    fn anchors(&self) -> Vec<AnchorId> {
        let mut anchors: Vec<_> = self.subscriptions.values().copied().collect();
        anchors.sort();
        anchors.dedup();
        anchors
    }
}

#[derive(Default)]
struct State {
    next_event: u64,
    next_subscription: u64,
    flows: HashMap<QualifiedFlowId, Flow>,
    subscriptions: HashMap<SubscriptionId, (AnchorId, QualifiedFlowId)>,
    streams: HashMap<AnchorId, Vec<UnboundedSender<StreamEvent>>>,
    empty: Vec<UnboundedSender<QualifiedFlowId>>,
}

impl State {
    fn deliver(&mut self, anchor: AnchorId, event: &StreamEvent) {
        if let Some(streams) = self.streams.get_mut(&anchor) {
            streams.retain(|s| s.send(event.clone()).is_ok());
        }
    }

    fn signal_empty(&mut self, flow: &QualifiedFlowId) {
        self.empty.retain(|s| s.send(flow.clone()).is_ok());
    }

    /// Removes a subscription, returning whether its anchor still has another
    /// one on the flow.
    fn remove_subscription(&mut self, subscription: &SubscriptionId) -> Option<bool> {
        let (anchor, flow_id) = self.subscriptions.remove(subscription)?;
        let flow = self.flows.get_mut(&flow_id)?;
        flow.subscriptions.remove(subscription);
        let anchor_left = flow.has_anchor(anchor);
        if flow.subscriptions.is_empty() {
            self.signal_empty(&flow_id);
        }
        Some(anchor_left)
    }
}

/// A complete router held in memory.
#[derive(Default)]
pub struct FakeRouter {
    state: Mutex<State>,
}

impl FakeRouter {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Router for FakeRouter {
    fn subscribe(
        &self,
        anchor: AnchorId,
        flow: QualifiedFlowId,
    ) -> (SubscriptionId, Option<Event>) {
        let mut state = self.state.lock().unwrap();
        state.next_subscription += 1;
        let id = SubscriptionId::new(format!("s-{}", state.next_subscription));
        state
            .subscriptions
            .insert(id.clone(), (anchor, flow.clone()));
        let flow = state.flows.entry(flow).or_default();
        flow.subscriptions.insert(id.clone(), anchor);
        (id, flow.last.clone())
    }

    fn unsubscribe(
        &self,
        anchor: AnchorId,
        subscription: &SubscriptionId,
    ) -> Result<(), UnknownSubscription> {
        let mut state = self.state.lock().unwrap();
        let flow = match state.subscriptions.get(subscription) {
            Some((owner, flow)) if *owner == anchor => flow.clone(),
            _ => return Err(UnknownSubscription(subscription.clone())),
        };
        if state.remove_subscription(subscription) == Some(false) {
            state.deliver(anchor, &StreamEvent::FlowEnded { flow });
        }
        Ok(())
    }

    fn publish(
        &self,
        flow: &QualifiedFlowId,
        data_type: String,
        data: Vec<u8>,
    ) -> Result<Event, UnknownFlow> {
        let mut state = self.state.lock().unwrap();
        if !state.flows.contains_key(flow) {
            return Err(UnknownFlow(flow.clone()));
        }
        state.next_event += 1;
        let event = Event {
            id: EventId(state.next_event),
            flow: flow.clone(),
            data_type,
            time: SystemTime::now(),
            data,
        };
        let entry = state.flows.get_mut(flow).expect("checked above");
        entry.last = Some(event.clone());
        let update = StreamEvent::Update(event.clone());
        for anchor in entry.anchors() {
            state.deliver(anchor, &update);
        }
        Ok(event)
    }

    fn last(&self, anchor: AnchorId, flow: &QualifiedFlowId) -> Result<Option<Event>, UnknownFlow> {
        let state = self.state.lock().unwrap();
        match state.flows.get(flow) {
            Some(f) if f.has_anchor(anchor) => Ok(f.last.clone()),
            _ => Err(UnknownFlow(flow.clone())),
        }
    }

    fn flow_error(&self, flow: &QualifiedFlowId, message: String) {
        let mut state = self.state.lock().unwrap();
        let Some(anchors) = state.flows.get(flow).map(Flow::anchors) else {
            return;
        };
        let event = StreamEvent::FlowError {
            flow: flow.clone(),
            message,
        };
        for anchor in anchors {
            state.deliver(anchor, &event);
        }
    }

    fn end_flow(&self, flow: &QualifiedFlowId) {
        let mut state = self.state.lock().unwrap();
        let Some(removed) = state.flows.remove(flow) else {
            return;
        };
        for id in removed.subscriptions.keys() {
            state.subscriptions.remove(id);
        }
        let event = StreamEvent::FlowEnded { flow: flow.clone() };
        for anchor in removed.anchors() {
            state.deliver(anchor, &event);
        }
    }

    fn stream(&self, anchor: AnchorId, since: Option<EventId>) -> UnboundedReceiver<StreamEvent> {
        let (tx, rx) = unbounded_channel();
        let mut state = self.state.lock().unwrap();
        if let Some(since) = since {
            let mut missed: Vec<_> = state
                .flows
                .values()
                .filter(|f| f.has_anchor(anchor))
                .filter_map(|f| f.last.clone())
                .filter(|e| e.id > since)
                .collect();
            missed.sort_by_key(|e| e.id);
            for event in missed {
                let _ = tx.send(StreamEvent::Update(event));
            }
        }
        state.streams.entry(anchor).or_default().push(tx);
        rx
    }

    fn subscribers(&self, flow: &QualifiedFlowId) -> usize {
        let state = self.state.lock().unwrap();
        state.flows.get(flow).map_or(0, |f| f.subscriptions.len())
    }

    fn empty_flows(&self) -> UnboundedReceiver<QualifiedFlowId> {
        let (tx, rx) = unbounded_channel();
        self.state.lock().unwrap().empty.push(tx);
        rx
    }

    fn remove_anchor(&self, anchor: AnchorId) {
        let mut state = self.state.lock().unwrap();
        state.streams.remove(&anchor);
        let owned: Vec<_> = state
            .subscriptions
            .iter()
            .filter(|(_, (a, _))| *a == anchor)
            .map(|(id, _)| id.clone())
            .collect();
        for id in owned {
            state.remove_subscription(&id);
        }
    }
}
