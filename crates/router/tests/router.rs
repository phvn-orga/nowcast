//! Behaviour beyond the conformance suite: the issue's acceptance criteria
//! from other angles, edge cases and concurrent use.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use nowcast_router::FlowRouter;
use nowcast_traits::{AnchorId, Router, UnknownFlow, UnknownSubscription};
use nowcast_types::{BrickName, Event, FlowId, QualifiedFlowId, StreamEvent, SubscriptionId};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::error::TryRecvError;

const A: AnchorId = AnchorId(1);
const B: AnchorId = AnchorId(2);
const TYPE: &str = "probe.item/v1";

fn flow(id: &str) -> QualifiedFlowId {
    QualifiedFlowId::new(BrickName::new("probe").unwrap(), FlowId::new(id).unwrap())
}

fn publish(router: &FlowRouter, id: &str, data: &str) -> Event {
    router
        .publish(&flow(id), TYPE.into(), data.as_bytes().to_vec())
        .unwrap()
}

/// Everything waiting on a stream, without blocking.
fn drain(stream: &mut UnboundedReceiver<StreamEvent>) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    while let Ok(event) = stream.try_recv() {
        events.push(event);
    }
    events
}

fn ended(id: &str) -> StreamEvent {
    StreamEvent::FlowEnded { flow: flow(id) }
}

#[test]
fn unsubscribing_one_anchor_keeps_the_other_receiving() {
    let router = FlowRouter::new();
    let (a, _) = router.subscribe(A, flow("x"));
    router.subscribe(B, flow("x"));
    let mut b = router.stream(B, None);
    router.unsubscribe(A, &a).unwrap();
    let sent = publish(&router, "x", "1");
    assert_eq!(drain(&mut b), vec![StreamEvent::Update(sent)]);
    assert_eq!(router.subscribers(&flow("x")), 1);
}

#[test]
fn only_the_anchors_last_subscription_ends_the_flow_for_it() {
    let router = FlowRouter::new();
    let (first, _) = router.subscribe(A, flow("x"));
    let (second, _) = router.subscribe(A, flow("x"));
    let mut stream = router.stream(A, None);
    router.unsubscribe(A, &first).unwrap();
    assert_eq!(drain(&mut stream), vec![]);
    router.unsubscribe(A, &second).unwrap();
    assert_eq!(drain(&mut stream), vec![ended("x")]);
}

#[test]
fn subscription_ids_are_never_reused() {
    let router = FlowRouter::new();
    let (first, _) = router.subscribe(A, flow("x"));
    assert_eq!(first, SubscriptionId::new("s-1"));
    router.unsubscribe(A, &first).unwrap();
    let (second, _) = router.subscribe(A, flow("x"));
    let (other, _) = router.subscribe(A, flow("y"));
    assert_ne!(second, first);
    assert_ne!(other, first);
    assert_ne!(other, second);
}

#[test]
fn unknown_subscriptions_are_refused() {
    let router = FlowRouter::new();
    let unknown = SubscriptionId::new("s-404");
    assert_eq!(
        router.unsubscribe(A, &unknown),
        Err(UnknownSubscription(unknown.clone()))
    );
    router.subscribe(A, flow("x"));
    assert_eq!(
        router.unsubscribe(A, &unknown),
        Err(UnknownSubscription(unknown))
    );
}

#[test]
fn event_ids_come_from_one_counter_for_every_flow() {
    let router = FlowRouter::new();
    router.subscribe(A, flow("x"));
    router.subscribe(B, flow("y"));
    let ids: Vec<u64> = ["x", "y", "x", "y", "y"]
        .iter()
        .map(|f| publish(&router, f, "1").id.0)
        .collect();
    let first = ids[0];
    assert_eq!(ids, (first..first + 5).collect::<Vec<_>>());
}

#[test]
fn event_ids_keep_increasing_across_a_restart() {
    let before = FlowRouter::new();
    before.subscribe(A, flow("x"));
    let last_before = (0..3).map(|_| publish(&before, "x", "1").id).last();
    // A restart takes far longer than this; the gap only has to exceed the
    // three events published above, counted in microseconds.
    thread::sleep(Duration::from_millis(5));
    let after = FlowRouter::new();
    after.subscribe(A, flow("x"));
    let first_after = publish(&after, "x", "1").id;
    assert!(
        Some(first_after) > last_before,
        "{first_after:?} after {last_before:?}"
    );
}

#[test]
fn a_new_subscriber_gets_the_latest_value_after_an_unsubscription() {
    let router = FlowRouter::new();
    let (a, _) = router.subscribe(A, flow("x"));
    let sent = publish(&router, "x", "1");
    router.unsubscribe(A, &a).unwrap();
    let (_, last) = router.subscribe(B, flow("x"));
    assert_eq!(last, Some(sent));
}

#[test]
fn an_empty_flow_still_accepts_publications() {
    let router = FlowRouter::new();
    let (a, _) = router.subscribe(A, flow("x"));
    router.unsubscribe(A, &a).unwrap();
    let sent = publish(&router, "x", "while empty");
    let (_, last) = router.subscribe(B, flow("x"));
    assert_eq!(last, Some(sent));
}

#[test]
fn an_ended_flow_is_gone() {
    let router = FlowRouter::new();
    let mut empty = router.empty_flows();
    let (a, _) = router.subscribe(A, flow("x"));
    publish(&router, "x", "1");
    router.end_flow(&flow("x"));
    assert_eq!(
        empty.try_recv(),
        Err(TryRecvError::Empty),
        "no empty signal"
    );
    assert_eq!(
        router.publish(&flow("x"), TYPE.into(), b"2".to_vec()),
        Err(UnknownFlow(flow("x")))
    );
    assert_eq!(router.unsubscribe(A, &a), Err(UnknownSubscription(a)));
    let (_, last) = router.subscribe(A, flow("x"));
    assert_eq!(last, None, "a flow created again starts without a value");
}

#[test]
fn ending_an_unknown_flow_does_nothing() {
    let router = FlowRouter::new();
    router.subscribe(A, flow("x"));
    let mut stream = router.stream(A, None);
    router.end_flow(&flow("y"));
    assert_eq!(drain(&mut stream), vec![]);
    assert_eq!(router.subscribers(&flow("x")), 1);
}

#[test]
fn ending_a_flow_reaches_an_anchor_once_whatever_its_subscriptions() {
    let router = FlowRouter::new();
    router.subscribe(A, flow("x"));
    router.subscribe(A, flow("x"));
    router.subscribe(A, flow("y"));
    let mut stream = router.stream(A, None);
    router.end_flow(&flow("x"));
    assert_eq!(drain(&mut stream), vec![ended("x")]);
    let sent = publish(&router, "y", "1");
    assert_eq!(drain(&mut stream), vec![StreamEvent::Update(sent)]);
}

#[test]
fn flow_errors_are_signals_only() {
    let router = FlowRouter::new();
    router.subscribe(A, flow("x"));
    router.subscribe(A, flow("x"));
    let mut stream = router.stream(A, None);
    router.flow_error(&flow("x"), "down".into());
    router.flow_error(&flow("unknown"), "ignored".into());
    let error = StreamEvent::FlowError {
        flow: flow("x"),
        message: "down".into(),
    };
    assert_eq!(drain(&mut stream), vec![error], "once per stream");
    let mut later = router.stream(A, None);
    assert_eq!(drain(&mut later), vec![], "not replayed");
    assert_eq!(router.last(A, &flow("x")), Ok(None));
}

#[test]
fn a_stream_opened_before_any_subscription_follows_later_ones() {
    let router = FlowRouter::new();
    let mut stream = router.stream(A, None);
    assert_eq!(drain(&mut stream), vec![]);
    router.subscribe(A, flow("x"));
    let sent = publish(&router, "x", "1");
    assert_eq!(drain(&mut stream), vec![StreamEvent::Update(sent)]);
}

#[test]
fn a_stream_keeps_its_anchor_known_after_its_last_unsubscription() {
    let router = FlowRouter::new();
    let mut stream = router.stream(A, None);
    let (a, _) = router.subscribe(A, flow("x"));
    router.unsubscribe(A, &a).unwrap();
    router.subscribe(A, flow("y"));
    let sent = publish(&router, "y", "1");
    assert_eq!(
        drain(&mut stream),
        vec![ended("x"), StreamEvent::Update(sent)]
    );
}

#[test]
fn replayed_values_come_in_id_order() {
    let router = FlowRouter::new();
    for f in ["a", "b", "c", "d"] {
        router.subscribe(A, flow(f));
    }
    let sent: Vec<u64> = ["c", "a", "d", "b", "a"]
        .iter()
        .map(|f| publish(&router, f, "1").id.0)
        .collect();
    let ids: Vec<u64> = drain(&mut router.stream(A, None))
        .into_iter()
        .map(|e| match e {
            StreamEvent::Update(e) => e.id.0,
            other => panic!("expected an update, got {other:?}"),
        })
        .collect();
    // The last value of c, d, b and a, in the order they were published.
    assert_eq!(ids, vec![sent[0], sent[2], sent[3], sent[4]]);
}

#[test]
fn a_closed_stream_does_not_disturb_the_others() {
    let router = FlowRouter::new();
    router.subscribe(A, flow("x"));
    drop(router.stream(A, None));
    let mut open = router.stream(A, None);
    let sent = publish(&router, "x", "1");
    assert_eq!(drain(&mut open), vec![StreamEvent::Update(sent)]);
}

#[test]
fn every_watcher_receives_empty_flows() {
    let router = FlowRouter::new();
    let mut first = router.empty_flows();
    drop(router.empty_flows());
    let mut second = router.empty_flows();
    let (a, _) = router.subscribe(A, flow("x"));
    router.unsubscribe(A, &a).unwrap();
    assert_eq!(first.try_recv(), Ok(flow("x")));
    assert_eq!(second.try_recv(), Ok(flow("x")));
}

#[test]
fn removing_an_anchor_signals_each_emptied_flow_once() {
    let router = FlowRouter::new();
    let mut empty = router.empty_flows();
    router.subscribe(A, flow("x"));
    router.subscribe(A, flow("x"));
    router.subscribe(A, flow("y"));
    router.subscribe(A, flow("shared"));
    router.subscribe(B, flow("shared"));
    let mut b = router.stream(B, None);
    router.remove_anchor(A);
    let mut signalled = vec![empty.try_recv().unwrap(), empty.try_recv().unwrap()];
    signalled.sort();
    assert_eq!(signalled, vec![flow("x"), flow("y")]);
    assert_eq!(empty.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(router.subscribers(&flow("x")), 0);
    assert_eq!(router.subscribers(&flow("shared")), 1);
    assert_eq!(drain(&mut b), vec![], "B is told nothing");
}

#[test]
fn removing_an_anchor_keeps_the_last_values_of_its_flows() {
    let router = FlowRouter::new();
    router.subscribe(A, flow("x"));
    let sent = publish(&router, "x", "1");
    router.remove_anchor(A);
    assert_eq!(router.last(A, &flow("x")), Err(UnknownFlow(flow("x"))));
    let (_, last) = router.subscribe(B, flow("x"));
    assert_eq!(last, Some(sent));
}

#[test]
fn removing_an_unknown_anchor_does_nothing() {
    let router = FlowRouter::new();
    let mut empty = router.empty_flows();
    router.subscribe(A, flow("x"));
    router.remove_anchor(B);
    assert_eq!(empty.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(router.subscribers(&flow("x")), 1);
}

#[test]
fn removing_an_anchor_closes_a_stream_with_no_subscription() {
    let router = FlowRouter::new();
    let mut stream = router.stream(A, None);
    router.remove_anchor(A);
    assert_eq!(stream.try_recv(), Err(TryRecvError::Disconnected));
}

#[test]
fn concurrent_publications_reach_each_stream_in_id_order() {
    const THREADS: usize = 8;
    const PER_THREAD: usize = 200;
    let router = Arc::new(FlowRouter::new());
    router.subscribe(A, flow("x"));
    let mut stream = router.stream(A, None);
    let publishers: Vec<_> = (0..THREADS)
        .map(|_| {
            let router = Arc::clone(&router);
            thread::spawn(move || {
                for _ in 0..PER_THREAD {
                    publish(&router, "x", "1");
                }
            })
        })
        .collect();
    for publisher in publishers {
        publisher.join().unwrap();
    }
    let ids: Vec<u64> = drain(&mut stream)
        .into_iter()
        .map(|e| match e {
            StreamEvent::Update(e) => e.id.0,
            other => panic!("expected an update, got {other:?}"),
        })
        .collect();
    assert_eq!(ids.len(), THREADS * PER_THREAD);
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "ids strictly increase");
}
