//! `nowcast watch` against a fake service.

mod common;

use std::time::Duration;

use common::{Anchoring, Fake, Probe, Watching, nowcast};
use serde_json::{Value, json};

/// The status lines of `watch --json`, as consumers match them.
const CONNECTED: &str = r#"{"status":"connected"}"#;
const CLOSED: &str = r#"{"status":"retrying","reason":"the service closed the stream"}"#;

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
    assert_eq!(watch.next_line().await, CONNECTED);
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
    let stderr = watch.stop().await;
    assert_eq!(stderr, "", "no status on a first connection");
}

/// An id that cannot go back as `Last-Event-ID` is not kept: the tool still
/// reconnects, resuming after the last usable id.
#[tokio::test]
async fn an_id_unfit_for_a_header_does_not_stop_reconnection() {
    let fake = Fake::start(Anchoring::Shared).await;
    let flow = fake.subscribe_shared("a");
    let mut watch = Watching::spawn(&mut nowcast(&fake), &[]);
    fake.wait_streams(1).await;
    let seen = fake.publish(&flow, json!({ "n": 1 }));
    assert_eq!(watch.next_line().await, r#"update probe/a {"n":1}"#);
    fake.send_raw(
        "event: update\nid: \u{e9}\u{7f}\ndata: {\"subject\":\"probe/a\",\"data\":2}\n\n",
    );
    assert_eq!(watch.next_line().await, "update probe/a 2");

    fake.close_streams();
    fake.wait_streams(1).await;
    fake.publish(&flow, json!({ "n": 3 }));
    assert_eq!(watch.next_line().await, r#"update probe/a {"n":3}"#);
    let stream = fake
        .log()
        .into_iter()
        .rfind(|r| r.path == "/v1/stream")
        .unwrap();
    assert_eq!(stream.last_event_id.as_deref(), seen["id"].as_str());
    let stderr = watch.stop().await;
    assert!(stderr.contains("ignored an update id"), "{stderr}");
}

/// `--json` says when the stream opens, the first time included, and once
/// per outage when it is lost.
#[tokio::test]
async fn json_status_lines_follow_the_connection() {
    let fake = Fake::start(Anchoring::Shared).await;
    let flow = fake.subscribe_shared("a");
    let mut watch = Watching::spawn(&mut nowcast(&fake), &["--json"]);
    assert_eq!(watch.next_line().await, CONNECTED);
    fake.wait_streams(1).await;

    fake.close_streams();
    assert_eq!(watch.next_line().await, CLOSED);
    assert_eq!(watch.next_line().await, CONNECTED);
    fake.wait_streams(1).await;
    let event = fake.publish(&flow, json!({ "n": 1 }));
    let line: Value = serde_json::from_str(&watch.next_line().await).unwrap();
    assert_eq!(line, json!({ "event": "update", "data": event }));

    // The human text stays on stderr.
    let stderr = watch.stop().await;
    assert!(
        stderr.contains("the service closed the stream; retrying"),
        "{stderr}"
    );
    assert!(stderr.contains("nowcast: connected"), "{stderr}");
}

/// A service that is not there yet: one `retrying`, however many attempts.
#[tokio::test]
async fn json_says_retrying_once_per_outage() {
    let fake = Fake::absent(Anchoring::Shared);
    let mut watch = Watching::spawn(&mut nowcast(&fake), &["--json"]);
    let line: Value = serde_json::from_str(&watch.next_line().await).unwrap();
    assert_eq!(line["status"], "retrying");
    let reason = line["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("cannot reach the service at "),
        "{reason}"
    );
    assert_eq!(line.as_object().unwrap().len(), 2, "{line}");
    tokio::time::sleep(Duration::from_millis(700)).await;
    fake.listen();
    assert_eq!(watch.next_line().await, CONNECTED);
    watch.stop().await;
}

/// The first stream asks to resume after `--last-event-id`: only newer
/// values come.
#[tokio::test]
async fn the_first_connection_resumes_after_the_given_id() {
    let fake = Fake::start(Anchoring::Shared).await;
    let a = fake.subscribe_shared("a");
    let b = fake.subscribe_shared("b");
    let seen = fake.publish(&a, json!({ "n": 1 }));
    fake.publish(&b, json!({ "n": 2 }));
    let id = seen["id"].as_str().unwrap();

    let mut watch = Watching::spawn(&mut nowcast(&fake), &["--last-event-id", id]);
    assert_eq!(watch.next_line().await, r#"update probe/b {"n":2}"#);
    assert_eq!(watch.line_within(Duration::from_millis(300)).await, None);
    let stream = fake.log().pop().unwrap();
    assert_eq!(stream.path, "/v1/stream");
    assert_eq!(stream.last_event_id.as_deref(), Some(id));
    watch.stop().await;
}

/// A watch from a plain shell stops; the next one, given the id of the last
/// update the first printed, takes up where it left off.
#[tokio::test]
async fn a_restarted_watch_resumes_from_the_printed_id() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let args = ["--json", "--subscribe", "probe", "item", r#"{"id":"a"}"#];
    let mut first = Watching::spawn(&mut nowcast(&fake), &args);
    assert_eq!(first.next_line().await, CONNECTED);
    fake.wait_streams(1).await;
    fake.publish("probe/a", json!({ "n": 1 }));
    let line: Value = serde_json::from_str(&first.next_line().await).unwrap();
    first.stop().await;
    let id = line["data"]["id"].as_str().unwrap().to_owned();

    let mut args = args.to_vec();
    args.extend(["--last-event-id", &id]);
    let mut second = Watching::spawn(&mut nowcast(&fake), &args);
    assert_eq!(second.next_line().await, CONNECTED);
    assert_eq!(second.line_within(Duration::from_millis(300)).await, None);
    let event = fake.publish("probe/a", json!({ "n": 2 }));
    let line: Value = serde_json::from_str(&second.next_line().await).unwrap();
    assert_eq!(line, json!({ "event": "update", "data": event }));
    second.stop().await;
}

/// The service restarted before the watch: what it publishes since is newer
/// than the given id, so it comes.
#[tokio::test]
async fn a_given_id_from_before_a_service_restart_loses_nothing() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let old = fake.publish("probe/a", json!({ "n": 1 }));
    fake.restart();
    let mut watch = Watching::spawn(
        &mut nowcast(&fake),
        &[
            "--subscribe",
            "probe",
            "item",
            r#"{"id":"a"}"#,
            "--last-event-id",
            old["id"].as_str().unwrap(),
        ],
    );
    fake.wait_streams(1).await;
    fake.publish("probe/a", json!({ "n": 2 }));
    assert_eq!(watch.next_line().await, r#"update probe/a {"n":2}"#);
    watch.stop().await;
}

/// The service restarts during the watch and loses the subscriptions: the
/// given id belongs to the lost state too, so every last value comes again.
#[tokio::test]
async fn a_lost_subscription_drops_the_given_id() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    fake.publish("probe/a", json!({ "n": 1 }));
    let mut watch = Watching::spawn(
        &mut nowcast(&fake),
        &[
            "--subscribe",
            "probe",
            "item",
            r#"{"id":"a"}"#,
            "--last-event-id",
            "999",
        ],
    );
    fake.wait_streams(1).await;
    assert_eq!(watch.line_within(Duration::from_millis(300)).await, None);

    fake.restart();
    fake.wait_streams(1).await;
    let streams: Vec<_> = fake
        .log()
        .into_iter()
        .filter(|r| r.path == "/v1/stream")
        .map(|r| r.last_event_id)
        .collect();
    assert_eq!(streams, [Some("999".to_owned()), None]);
    watch.stop().await;
}

#[tokio::test]
async fn a_last_event_id_unfit_for_a_header_exits_2_without_a_call() {
    let fake = Fake::start(Anchoring::Shared).await;
    let watch = Watching::spawn(&mut nowcast(&fake), &["--last-event-id", "4\n2"]);
    let (status, stderr) = watch.wait().await;
    assert_eq!(status, Some(2), "{stderr}");
    assert!(stderr.contains("--last-event-id"), "{stderr}");
    assert!(fake.log().is_empty());
}
