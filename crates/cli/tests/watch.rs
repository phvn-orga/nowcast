//! `nowcast watch` against a fake service.

mod common;

use std::time::Duration;

use common::{Anchoring, Fake, Probe, Watching, nowcast};
use serde_json::{Value, json};

/// The central mod scenario: `watch` runs under an agent whose mods
/// subscribe; it prints every kind of event.
#[tokio::test]
async fn prints_updates_errors_and_ends() {
    let fake = Fake::start(Anchoring::Shared).await;
    let flow = fake.subscribe_shared("a");
    let mut watch = Watching::spawn(&mut nowcast(&fake), &[]);
    fake.wait_streams(1).await;

    fake.publish(&flow, json!({ "n": 1 }));
    assert_eq!(watch.next_line().await, r#"update probe/a {"n":1}"#);
    fake.flow_error(&flow, "source down");
    assert_eq!(watch.next_line().await, "flow-error probe/a source down");
    fake.end_flow(&flow);
    assert_eq!(watch.next_line().await, "flow-ended probe/a");

    watch.stop().await;
    fake.assert_sessions_first();
}

#[tokio::test]
async fn prints_json_lines() {
    let fake = Fake::start(Anchoring::Shared).await;
    let flow = fake.subscribe_shared("a");
    let mut watch = Watching::spawn(&mut nowcast(&fake), &["--json"]);
    fake.wait_streams(1).await;

    let event = fake.publish(&flow, json!({ "n": 1 }));
    let line: Value = serde_json::from_str(&watch.next_line().await).unwrap();
    assert_eq!(line, json!({ "event": "update", "data": event }));
    fake.flow_error(&flow, "source down");
    let line: Value = serde_json::from_str(&watch.next_line().await).unwrap();
    assert_eq!(
        line,
        json!({ "event": "flow-error", "data": { "flow": flow, "message": "source down" } })
    );
    fake.end_flow(&flow);
    let line: Value = serde_json::from_str(&watch.next_line().await).unwrap();
    assert_eq!(
        line,
        json!({ "event": "flow-ended", "data": { "flow": flow } })
    );
    watch.stop().await;
}

#[tokio::test]
async fn opens_the_stream_as_server_sent_events() {
    let fake = Fake::start(Anchoring::Shared).await;
    let watch = Watching::spawn(&mut nowcast(&fake), &[]);
    fake.wait_streams(1).await;
    let stream = fake.log().pop().unwrap();
    assert_eq!(
        (stream.method.as_str(), stream.path.as_str()),
        ("GET", "/v1/stream")
    );
    assert_eq!(stream.accept.as_deref(), Some("text/event-stream"));
    assert_eq!(stream.last_event_id, None);
    watch.stop().await;
}

/// The stream breaks: the tool declares the session again and resumes after
/// the last event it printed.
#[tokio::test]
async fn reconnects_and_resumes_after_the_last_event() {
    let fake = Fake::start(Anchoring::Shared).await;
    let flow = fake.subscribe_shared("a");
    let mut watch = Watching::spawn(&mut nowcast(&fake), &[]);
    fake.wait_streams(1).await;
    let seen = fake.publish(&flow, json!({ "n": 1 }));
    assert_eq!(watch.next_line().await, r#"update probe/a {"n":1}"#);

    fake.close_streams();
    fake.publish(&flow, json!({ "n": 2 }));
    fake.wait_streams(1).await;
    assert_eq!(watch.next_line().await, r#"update probe/a {"n":2}"#);

    let log = fake.log();
    let streams: Vec<_> = log.iter().filter(|r| r.path == "/v1/stream").collect();
    assert_eq!(streams.len(), 2);
    assert_eq!(streams[1].last_event_id.as_deref(), seen["id"].as_str());
    let sessions = log.iter().filter(|r| r.path == "/v1/session").count();
    assert_eq!(sessions, 2, "declared again before the new stream");

    let stderr = watch.stop().await;
    assert!(
        stderr.contains("the service closed the stream; retrying"),
        "{stderr}"
    );
    assert!(stderr.contains("connected"), "{stderr}");
    fake.assert_sessions_first();
}

#[tokio::test]
async fn waits_for_a_service_that_is_not_there_yet() {
    let fake = Fake::absent(Anchoring::Shared);
    let mut watch = Watching::spawn(&mut nowcast(&fake), &[]);
    tokio::time::sleep(Duration::from_millis(500)).await;
    fake.listen();
    let flow = fake.subscribe_shared("a");
    fake.wait_streams(1).await;
    fake.publish(&flow, json!({ "n": 1 }));
    assert_eq!(watch.next_line().await, r#"update probe/a {"n":1}"#);

    let stderr = watch.stop().await;
    assert_eq!(
        stderr.matches("cannot reach the service").count(),
        1,
        "said once: {stderr}"
    );
    fake.assert_sessions_first();
}

/// From a plain shell, `watch --subscribe` holds its own subscriptions.
#[tokio::test]
async fn subscribes_before_opening_the_stream() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let mut watch = Watching::spawn(
        &mut nowcast(&fake),
        &[
            "--subscribe",
            "probe",
            "item",
            r#"{"id":"a"}"#,
            "--subscribe",
            "probe",
            "item",
            r#"{"id":"b"}"#,
        ],
    );
    fake.wait_streams(1).await;
    let paths: Vec<_> = fake.log().into_iter().map(|r| r.path).collect();
    assert_eq!(
        paths,
        [
            "/v1/session",
            "/v1/subscriptions",
            "/v1/subscriptions",
            "/v1/stream"
        ]
    );
    fake.publish("probe/b", json!({ "n": 1 }));
    assert_eq!(watch.next_line().await, r#"update probe/b {"n":1}"#);

    let stderr = watch.stop().await;
    assert!(stderr.contains("subscribed to probe/a"), "{stderr}");
    assert!(stderr.contains("subscribed to probe/b"), "{stderr}");
    fake.assert_sessions_first();
}

/// A restarted service forgot the session and the subscriptions: the tool
/// declares, subscribes again, and takes every last value from the start.
#[tokio::test]
async fn subscribes_again_after_a_restart() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let mut watch = Watching::spawn(
        &mut nowcast(&fake),
        &["--subscribe", "probe", "item", r#"{"id":"a"}"#],
    );
    fake.wait_streams(1).await;
    fake.publish("probe/a", json!({ "n": 1 }));
    assert_eq!(watch.next_line().await, r#"update probe/a {"n":1}"#);

    fake.restart();
    fake.wait_streams(1).await;
    fake.publish("probe/a", json!({ "n": 2 }));
    assert_eq!(watch.next_line().await, r#"update probe/a {"n":2}"#);

    let log = fake.log();
    let after_restart: Vec<_> = log
        .iter()
        .skip_while(|r| r.path != "/v1/stream")
        .skip(1)
        .map(|r| (r.path.as_str(), r.status))
        .collect();
    assert_eq!(
        after_restart,
        [
            ("/v1/session", 204),
            ("/v1/flows/probe%2Fa", 404),
            ("/v1/subscriptions", 201),
            ("/v1/stream", 200),
        ]
    );
    assert_eq!(log.last().unwrap().last_event_id, None);
    watch.stop().await;
    fake.assert_sessions_first();
}

/// The stream broke but the service kept the subscription: no second one.
#[tokio::test]
async fn keeps_a_subscription_the_service_still_has() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let watch = Watching::spawn(
        &mut nowcast(&fake),
        &["--subscribe", "probe", "item", r#"{"id":"a"}"#],
    );
    fake.wait_streams(1).await;
    fake.close_streams();
    fake.wait_streams(1).await;
    let subscriptions = fake
        .log()
        .iter()
        .filter(|r| r.path == "/v1/subscriptions")
        .count();
    assert_eq!(subscriptions, 1);
    watch.stop().await;
}

/// A flow that ended is not subscribed again on reconnection.
#[tokio::test]
async fn an_ended_flow_is_let_go() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let mut watch = Watching::spawn(
        &mut nowcast(&fake),
        &["--subscribe", "probe", "item", r#"{"id":"a"}"#],
    );
    fake.wait_streams(1).await;
    fake.end_flow("probe/a");
    assert_eq!(watch.next_line().await, "flow-ended probe/a");
    fake.restart();
    fake.wait_streams(1).await;
    let subscriptions = fake
        .log()
        .iter()
        .filter(|r| r.path == "/v1/subscriptions")
        .count();
    assert_eq!(subscriptions, 1);
    watch.stop().await;
}

#[tokio::test]
async fn a_subscription_the_service_refuses_exits_1() {
    for (probe, args, code) in [
        (
            Probe::Ready,
            ["nope", "item", r#"{"id":"a"}"#],
            "unknown_brick",
        ),
        (Probe::Ready, ["probe", "item", "{}"], "invalid_params"),
        (
            Probe::Refusing,
            ["probe", "item", r#"{"id":"a"}"#],
            "subscription_refused",
        ),
    ] {
        let fake = Fake::start(Anchoring::PerProcess).await;
        fake.set_probe(probe);
        let mut argv = vec!["--subscribe"];
        argv.extend(args);
        let watch = Watching::spawn(&mut nowcast(&fake), &argv);
        let (status, stderr) = watch.wait().await;
        assert_eq!(status, Some(1), "{code}: {stderr}");
        assert!(stderr.contains(&format!("nowcast: {code}: ")), "{stderr}");
        fake.assert_sessions_first();
    }
}

/// A brick not ready yet is worth waiting for.
#[tokio::test]
async fn an_unavailable_brick_is_retried() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    fake.set_probe(Probe::Unavailable);
    let mut watch = Watching::spawn(
        &mut nowcast(&fake),
        &["--subscribe", "probe", "item", r#"{"id":"a"}"#],
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    fake.set_probe(Probe::Ready);
    fake.wait_streams(1).await;
    fake.publish("probe/a", json!(1));
    assert_eq!(watch.next_line().await, "update probe/a 1");
    let stderr = watch.stop().await;
    assert!(stderr.contains("brick_unavailable"), "{stderr}");
}

#[tokio::test]
async fn invalid_params_exit_2_without_a_call() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let watch = Watching::spawn(
        &mut nowcast(&fake),
        &["--subscribe", "probe", "item", "id=a"],
    );
    let (status, stderr) = watch.wait().await;
    assert_eq!(status, Some(2), "{stderr}");
    assert!(fake.log().is_empty());
}

#[tokio::test]
async fn unreadable_and_unknown_events_are_skipped() {
    let fake = Fake::start(Anchoring::Shared).await;
    let flow = fake.subscribe_shared("a");
    let mut watch = Watching::spawn(&mut nowcast(&fake), &[]);
    fake.wait_streams(1).await;
    fake.send_raw(
        ": keep-alive\n\nevent: later-kind\ndata: {}\n\nevent: update\nid: 9\ndata: {oops\n\n",
    );
    fake.publish(&flow, json!({ "n": 1 }));
    assert_eq!(watch.next_line().await, r#"update probe/a {"n":1}"#);
    let stderr = watch.stop().await;
    assert!(stderr.contains("skipped a update"), "{stderr}");
}

#[tokio::test]
async fn a_stream_that_is_not_server_sent_events_exits_4() {
    let fake = Fake::start(Anchoring::Shared).await;
    fake.can("/v1/stream", 200, "application/json", "{}");
    let watch = Watching::spawn(&mut nowcast(&fake), &[]);
    let (status, stderr) = watch.wait().await;
    assert_eq!(status, Some(4), "{stderr}");
}

#[tokio::test]
async fn nothing_more_than_the_events() {
    let fake = Fake::start(Anchoring::Shared).await;
    let flow = fake.subscribe_shared("a");
    fake.publish(&flow, json!({ "n": 1 }));
    let mut watch = Watching::spawn(&mut nowcast(&fake), &[]);
    assert_eq!(watch.next_line().await, r#"update probe/a {"n":1}"#);
    assert_eq!(watch.line_within(Duration::from_millis(300)).await, None);
    watch.stop().await;
}
