//! Contract section 3, the rules on subscriptions, last values and the event
//! stream that the router carries, checked on the [`Router`] trait. The HTTP
//! surface of the same rules is in [`crate::api`].

use std::time::Duration;

use nowcast_traits::{AnchorId, FLOW_HISTORY, FlowSummary, History, Router, UnknownFlow, UnknownSubscription};
use nowcast_types::{BrickName, Event, EventId, FlowId, QualifiedFlowId, StreamEvent};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::timeout;

/// Builds a fresh router, with no flow and no subscription.
pub trait RouterSubject {
    type Router: Router;
    fn new() -> Self::Router;
}

const A: AnchorId = AnchorId(1);
const B: AnchorId = AnchorId(2);
const TYPE: &str = "probe.item/v1";

fn flow(id: &str) -> QualifiedFlowId {
    QualifiedFlowId::new(BrickName::new("probe").unwrap(), FlowId::new(id).unwrap())
}

fn publish(router: &impl Router, flow: &QualifiedFlowId, data: &str) -> Event {
    router
        .publish(flow, TYPE.into(), data.as_bytes().to_vec())
        .expect("the flow has a subscriber")
}

async fn next(stream: &mut UnboundedReceiver<StreamEvent>) -> StreamEvent {
    timeout(Duration::from_secs(1), stream.recv())
        .await
        .expect("an event within a second")
        .expect("the stream is open")
}

fn assert_quiet(stream: &mut UnboundedReceiver<StreamEvent>) {
    if let Ok(event) = stream.try_recv() {
        panic!("unexpected event {event:?}");
    }
}

fn update(event: StreamEvent) -> Event {
    match event {
        StreamEvent::Update(e) => e,
        other => panic!("expected an update, got {other:?}"),
    }
}

/// Each subscriber gets its own subscription id, even within one anchor and
/// one flow.
pub async fn subscription_ids_are_per_subscriber<S: RouterSubject>() {
    let router = S::new();
    let (a1, _) = router.subscribe(A, flow("x"));
    let (a2, _) = router.subscribe(A, flow("x"));
    let (b1, _) = router.subscribe(B, flow("x"));
    assert_ne!(a1, a2);
    assert_ne!(a1, b1);
    assert_ne!(a2, b1);
    assert_eq!(router.subscribers(&flow("x")), 3);
}

/// Unsubscribing one subscriber leaves the other on the shared flow, with no
/// `flow-ended`.
pub async fn unsubscribing_one_keeps_the_other<S: RouterSubject>() {
    let router = S::new();
    let (first, _) = router.subscribe(A, flow("x"));
    let (_second, _) = router.subscribe(A, flow("x"));
    let mut stream = router.stream(A, None);
    router.unsubscribe(A, &first).unwrap();
    let sent = publish(&router, &flow("x"), "1");
    assert_eq!(update(next(&mut stream).await), sent);
    assert_quiet(&mut stream);
    assert_eq!(router.last(A, &flow("x")), Ok(Some(sent)));
}

/// A subscription is private to its subscriber: another anchor cannot delete it.
pub async fn a_subscription_belongs_to_its_anchor<S: RouterSubject>() {
    let router = S::new();
    let (id, _) = router.subscribe(A, flow("x"));
    assert_eq!(
        router.unsubscribe(B, &id),
        Err(UnknownSubscription(id.clone()))
    );
    assert_eq!(router.unsubscribe(A, &id), Ok(()));
    assert_eq!(router.unsubscribe(A, &id), Err(UnknownSubscription(id)));
}

/// `last` is `null` until the flow publishes, then the latest event.
pub async fn subscribing_returns_the_last_value<S: RouterSubject>() {
    let router = S::new();
    let (_, last) = router.subscribe(A, flow("x"));
    assert_eq!(last, None);
    publish(&router, &flow("x"), "1");
    let latest = publish(&router, &flow("x"), "2");
    let (_, last) = router.subscribe(B, flow("x"));
    assert_eq!(last, Some(latest));
}

/// `GET /v1/flows/{flow}`: the last event, nothing before the first
/// publication, and only for a flow the anchor is subscribed to.
pub async fn last_value_is_for_subscribers_only<S: RouterSubject>() {
    let router = S::new();
    router.subscribe(A, flow("x"));
    assert_eq!(router.last(A, &flow("x")), Ok(None));
    let sent = publish(&router, &flow("x"), "1");
    assert_eq!(router.last(A, &flow("x")), Ok(Some(sent)));
    assert_eq!(router.last(B, &flow("x")), Err(UnknownFlow(flow("x"))));
    assert_eq!(router.last(A, &flow("y")), Err(UnknownFlow(flow("y"))));
}

/// `is_subscribed` answers as `last` does, without the value: for the
/// subscribed anchor only, until its last subscription goes.
pub async fn is_subscribed_follows_the_subscriptions<S: RouterSubject>() {
    let router = S::new();
    let (subscription, _) = router.subscribe(A, flow("x"));
    publish(&router, &flow("x"), "1");
    assert!(router.is_subscribed(A, &flow("x")));
    assert!(!router.is_subscribed(B, &flow("x")));
    assert!(!router.is_subscribed(A, &flow("y")));
    router.unsubscribe(A, &subscription).unwrap();
    assert!(!router.is_subscribed(A, &flow("x")));
}

/// Event ids increase within a flow.
pub async fn event_ids_increase_within_a_flow<S: RouterSubject>() {
    let router = S::new();
    router.subscribe(A, flow("x"));
    router.subscribe(A, flow("y"));
    let ids: Vec<EventId> = ["x", "y", "x", "x", "y"]
        .iter()
        .map(|f| publish(&router, &flow(f), "1").id)
        .collect();
    assert!(ids[0] < ids[2] && ids[2] < ids[3], "flow x: {ids:?}");
    assert!(ids[1] < ids[4], "flow y: {ids:?}");
}

/// The event carries the flow as subject, the published type, and the detail
/// as is.
pub async fn events_carry_the_flow_type_and_detail<S: RouterSubject>() {
    let router = S::new();
    router.subscribe(A, flow("x"));
    let detail = br#"{ "spaced" : [1, 2] }"#.to_vec();
    let event = router
        .publish(&flow("x"), TYPE.into(), detail.clone())
        .unwrap();
    assert_eq!(event.subject(), &flow("x"));
    assert_eq!(event.data_type, TYPE);
    assert_eq!(event.data, detail);
}

/// A flow nobody subscribed to cannot be published on.
pub async fn publishing_needs_a_known_flow<S: RouterSubject>() {
    let router = S::new();
    assert_eq!(
        router.publish(&flow("x"), TYPE.into(), b"1".to_vec()),
        Err(UnknownFlow(flow("x")))
    );
}

/// The stream carries every flow the anchor is subscribed to, including
/// subscriptions made after it opened, and nothing else.
pub async fn the_stream_follows_the_anchors_subscriptions<S: RouterSubject>() {
    let router = S::new();
    router.subscribe(A, flow("x"));
    router.subscribe(B, flow("other"));
    let mut stream = router.stream(A, None);
    router.subscribe(A, flow("later"));
    let x = publish(&router, &flow("x"), "1");
    publish(&router, &flow("other"), "1");
    let later = publish(&router, &flow("later"), "1");
    assert_eq!(update(next(&mut stream).await), x);
    assert_eq!(update(next(&mut stream).await), later);
    assert_quiet(&mut stream);
}

/// One update per stream, however many subscriptions the anchor holds on the
/// flow.
pub async fn one_update_per_stream<S: RouterSubject>() {
    let router = S::new();
    router.subscribe(A, flow("x"));
    router.subscribe(A, flow("x"));
    let mut first = router.stream(A, None);
    let mut second = router.stream(A, None);
    let sent = publish(&router, &flow("x"), "1");
    assert_eq!(update(next(&mut first).await), sent);
    assert_eq!(update(next(&mut second).await), sent);
    assert_quiet(&mut first);
    assert_quiet(&mut second);
}

/// On reconnection, every kept event newer than `Last-Event-ID` is sent again,
/// in increasing id order across flows.
pub async fn reconnecting_replays_the_history_after_the_id<S: RouterSubject>() {
    let router = S::new();
    router.subscribe(A, flow("x"));
    router.subscribe(A, flow("y"));
    router.subscribe(A, flow("z"));
    publish(&router, &flow("z"), "old");
    let seen = publish(&router, &flow("x"), "1");
    let x2 = publish(&router, &flow("x"), "2");
    let y = publish(&router, &flow("y"), "1");
    let x3 = publish(&router, &flow("x"), "3");
    let mut stream = router.stream(A, Some(seen.id));
    for sent in [x2, y, x3] {
        assert_eq!(update(next(&mut stream).await), sent);
    }
    assert_quiet(&mut stream);
}

/// A flow keeps its last [`FLOW_HISTORY`] events. A reconnection whose id is
/// older than a dropped event gets `flow-gap` for that flow first.
pub async fn the_history_is_bounded<S: RouterSubject>() {
    let router = S::new();
    router.subscribe(A, flow("x"));
    let sent: Vec<Event> = (0..FLOW_HISTORY + 2)
        .map(|n| publish(&router, &flow("x"), &n.to_string()))
        .collect();
    let kept = &sent[2..];

    let mut stream = router.stream(A, Some(sent[1].id));
    for event in kept {
        assert_eq!(update(next(&mut stream).await), *event);
    }
    assert_quiet(&mut stream);

    let mut stream = router.stream(A, Some(sent[0].id));
    assert_eq!(
        next(&mut stream).await,
        StreamEvent::FlowGap { flow: flow("x") }
    );
    for event in kept {
        assert_eq!(update(next(&mut stream).await), *event);
    }
    assert_quiet(&mut stream);
}

/// `Last-Event-ID: 0` predates every run of the service: each subscribed
/// flow gets `flow-gap`, then the whole kept history follows.
pub async fn an_id_from_before_the_run_is_a_gap<S: RouterSubject>() {
    let router = S::new();
    router.subscribe(A, flow("x"));
    router.subscribe(A, flow("y"));
    let x1 = publish(&router, &flow("x"), "1");
    let x2 = publish(&router, &flow("x"), "2");
    let mut stream = router.stream(A, Some(EventId(0)));
    assert_eq!(
        next(&mut stream).await,
        StreamEvent::FlowGap { flow: flow("x") }
    );
    assert_eq!(
        next(&mut stream).await,
        StreamEvent::FlowGap { flow: flow("y") }
    );
    assert_eq!(update(next(&mut stream).await), x1);
    assert_eq!(update(next(&mut stream).await), x2);
    assert_quiet(&mut stream);
}

/// `GET /v1/flows/{flow}?after=<id>`: the kept events after the id, with the
/// same gap rule as the stream, and only for a flow the anchor is subscribed
/// to.
pub async fn the_history_reads_after_an_id<S: RouterSubject>() {
    let router = S::new();
    router.subscribe(A, flow("x"));
    let x1 = publish(&router, &flow("x"), "1");
    let x2 = publish(&router, &flow("x"), "2");
    let x3 = publish(&router, &flow("x"), "3");
    assert_eq!(
        router.history(A, &flow("x"), x1.id),
        Ok(History {
            gap: false,
            events: vec![x2, x3.clone()],
        })
    );
    assert_eq!(
        router.history(A, &flow("x"), x3.id),
        Ok(History {
            gap: false,
            events: vec![],
        })
    );
    assert!(router.history(A, &flow("x"), EventId(0)).unwrap().gap);
    assert_eq!(
        router.history(B, &flow("x"), x1.id),
        Err(UnknownFlow(flow("x")))
    );
}

/// The history goes with the flow: a flow subscribed again after it ended
/// starts empty.
pub async fn ending_a_flow_drops_its_history<S: RouterSubject>() {
    let router = S::new();
    router.subscribe(A, flow("x"));
    let sent = publish(&router, &flow("x"), "1");
    router.end_flow(&flow("x"));
    let (_, last) = router.subscribe(A, flow("x"));
    assert_eq!(last, None);
    let history = router
        .history(A, &flow("x"), EventId(sent.id.0 - 1))
        .unwrap();
    assert_eq!(history.events, vec![]);
}

/// A stream opened without `Last-Event-ID` first sends the last value of each
/// subscribed flow, so that nothing published before it opened is lost.
pub async fn a_fresh_stream_sends_every_last_value<S: RouterSubject>() {
    let router = S::new();
    router.subscribe(A, flow("x"));
    router.subscribe(A, flow("y"));
    router.subscribe(A, flow("empty"));
    router.subscribe(B, flow("other"));
    publish(&router, &flow("x"), "1");
    let x = publish(&router, &flow("x"), "2");
    let y = publish(&router, &flow("y"), "1");
    publish(&router, &flow("other"), "1");
    let mut stream = router.stream(A, None);
    let mut sent = vec![
        update(next(&mut stream).await),
        update(next(&mut stream).await),
    ];
    sent.sort_by_key(|e| e.id);
    assert_eq!(sent, vec![x, y]);
    assert_quiet(&mut stream);
}

/// `flow-error` reaches the anchors subscribed to the flow.
pub async fn flow_errors_reach_subscribers<S: RouterSubject>() {
    let router = S::new();
    router.subscribe(A, flow("x"));
    let mut a = router.stream(A, None);
    let mut b = router.stream(B, None);
    router.flow_error(&flow("x"), "down".into());
    assert_eq!(
        next(&mut a).await,
        StreamEvent::FlowError {
            flow: flow("x"),
            message: "down".into()
        }
    );
    assert_quiet(&mut b);
}

/// `flow-ended` when the anchor's subscription is deleted.
pub async fn unsubscribing_ends_the_flow_for_the_anchor<S: RouterSubject>() {
    let router = S::new();
    let (id, _) = router.subscribe(A, flow("x"));
    router.subscribe(B, flow("x"));
    let mut a = router.stream(A, None);
    let mut b = router.stream(B, None);
    router.unsubscribe(A, &id).unwrap();
    assert_eq!(
        next(&mut a).await,
        StreamEvent::FlowEnded { flow: flow("x") }
    );
    assert_quiet(&mut b);
    assert_eq!(router.last(A, &flow("x")), Err(UnknownFlow(flow("x"))));
}

/// `flow-ended` for every subscriber when the flow no longer exists.
pub async fn ending_a_flow_reaches_every_subscriber<S: RouterSubject>() {
    let router = S::new();
    router.subscribe(A, flow("x"));
    router.subscribe(B, flow("x"));
    let mut a = router.stream(A, None);
    let mut b = router.stream(B, None);
    router.end_flow(&flow("x"));
    assert_eq!(
        next(&mut a).await,
        StreamEvent::FlowEnded { flow: flow("x") }
    );
    assert_eq!(
        next(&mut b).await,
        StreamEvent::FlowEnded { flow: flow("x") }
    );
    assert_eq!(router.subscribers(&flow("x")), 0);
    assert_eq!(router.last(A, &flow("x")), Err(UnknownFlow(flow("x"))));
}

/// The router signals a flow the moment its last subscription goes, and keeps
/// its last value for a subscriber who comes back before the flow ends.
pub async fn an_empty_flow_is_signalled_and_kept<S: RouterSubject>() {
    let router = S::new();
    let mut empty = router.empty_flows();
    let (a, _) = router.subscribe(A, flow("x"));
    let (b, _) = router.subscribe(B, flow("x"));
    let sent = publish(&router, &flow("x"), "1");
    router.unsubscribe(A, &a).unwrap();
    assert!(empty.try_recv().is_err(), "one subscriber is left");
    router.unsubscribe(B, &b).unwrap();
    assert_eq!(empty.try_recv(), Ok(flow("x")));
    assert_eq!(router.subscribers(&flow("x")), 0);
    let (_, last) = router.subscribe(A, flow("x"));
    assert_eq!(last, Some(sent));
}

/// A dead anchor loses its subscriptions and streams, and the flows it leaves
/// empty are signalled.
pub async fn removing_an_anchor_drops_its_subscriptions<S: RouterSubject>() {
    let router = S::new();
    let mut empty = router.empty_flows();
    router.subscribe(A, flow("x"));
    router.subscribe(A, flow("shared"));
    router.subscribe(B, flow("shared"));
    let mut stream = router.stream(A, None);
    router.remove_anchor(A);
    assert_eq!(empty.try_recv(), Ok(flow("x")));
    assert!(empty.try_recv().is_err(), "shared still has B");
    assert_eq!(router.subscribers(&flow("shared")), 1);
    assert_eq!(
        timeout(Duration::from_secs(1), stream.recv()).await,
        Ok(None),
        "the stream closes without flow-ended"
    );
}

/// Whether any subscription exists follows every way one is made or
/// dropped, and a receiver sees each change.
pub async fn the_router_tells_whether_any_subscription_exists<S: RouterSubject>() {
    let router = S::new();
    let mut any = router.has_subscriptions();
    assert!(!*any.borrow_and_update());

    let (a, _) = router.subscribe(A, flow("x"));
    assert!(any.has_changed().unwrap());
    assert!(*any.borrow_and_update());
    router.unsubscribe(A, &a).unwrap();
    assert!(!*any.borrow_and_update(), "unsubscribed");

    router.subscribe(A, flow("x"));
    router.subscribe(B, flow("y"));
    router.remove_anchor(A);
    assert!(*any.borrow_and_update(), "B is still subscribed");
    router.end_flow(&flow("y"));
    assert!(!*any.borrow_and_update(), "the flow ended");

    // A stream and a flow kept with no subscriber are not subscriptions.
    let _stream = router.stream(A, None);
    assert!(!*router.has_subscriptions().borrow());
}

/// `GET /v1/status` reads each anchor's subscriptions, and every flow with its
/// subscribers and last event, an empty flow kept included until it ends.
pub async fn the_router_tells_its_flows_and_subscriptions<S: RouterSubject>() {
    let router = S::new();
    assert_eq!(router.flows(), vec![]);
    assert_eq!(router.subscriptions(A), vec![]);

    let (ay, _) = router.subscribe(A, flow("y"));
    let (ax, _) = router.subscribe(A, flow("x"));
    let (bx, _) = router.subscribe(B, flow("x"));
    let sent = publish(&router, &flow("x"), "1");
    assert_eq!(
        router.subscriptions(A),
        vec![(ax, flow("x")), (ay.clone(), flow("y"))]
    );
    assert_eq!(router.subscriptions(B), vec![(bx, flow("x"))]);
    assert_eq!(
        router.flows(),
        vec![
            FlowSummary {
                flow: flow("x"),
                subscribers: 2,
                last: Some((sent.id, sent.time)),
            },
            FlowSummary {
                flow: flow("y"),
                subscribers: 1,
                last: None,
            },
        ]
    );

    router.unsubscribe(A, &ay).unwrap();
    router.remove_anchor(B);
    assert_eq!(router.subscriptions(B), vec![]);
    let flows = router.flows();
    assert_eq!(flows[1].flow, flow("y"));
    assert_eq!(flows[1].subscribers, 0, "kept until ended");
    router.end_flow(&flow("y"));
    assert_eq!(router.flows().len(), 1);
}

/// Runs the router rules of contract section 3.
#[macro_export]
macro_rules! router_suite {
    ($subject:ty) => {
        mod router_suite {
            #[allow(unused_imports)]
            use super::*;
            $crate::__tests!(
                block_on,
                router,
                $subject,
                [
                    subscription_ids_are_per_subscriber,
                    unsubscribing_one_keeps_the_other,
                    a_subscription_belongs_to_its_anchor,
                    subscribing_returns_the_last_value,
                    last_value_is_for_subscribers_only,
                    is_subscribed_follows_the_subscriptions,
                    event_ids_increase_within_a_flow,
                    events_carry_the_flow_type_and_detail,
                    publishing_needs_a_known_flow,
                    the_stream_follows_the_anchors_subscriptions,
                    one_update_per_stream,
                    reconnecting_replays_the_history_after_the_id,
                    the_history_is_bounded,
                    an_id_from_before_the_run_is_a_gap,
                    the_history_reads_after_an_id,
                    ending_a_flow_drops_its_history,
                    a_fresh_stream_sends_every_last_value,
                    flow_errors_reach_subscribers,
                    unsubscribing_ends_the_flow_for_the_anchor,
                    ending_a_flow_reaches_every_subscriber,
                    an_empty_flow_is_signalled_and_kept,
                    removing_an_anchor_drops_its_subscriptions,
                    the_router_tells_whether_any_subscription_exists,
                    the_router_tells_its_flows_and_subscriptions,
                ]
            );
        }
    };
}
