//! The scenarios of the whole chain, with real processes: the daemon, the
//! test brick it launches, and the `nowcast` tool as client. The test drives
//! the brick over its control socket.

mod harness;

use std::time::{Duration, Instant};

use harness::{AGENT_SESSION, Service, TIMEOUT, params, received};
use nowcast_test_brick::{ControlledBrick, Mode, TYPE};
use nowcast_types::{FlowId, PublishErrorCode, ServiceMessage};
use serde_json::{Value, json};
use tokio::time::timeout;

/// The flow `id` of the test brick, as the brick names it.
fn flow(id: &str) -> FlowId {
    FlowId::new(id).unwrap()
}

/// Makes the brick publish `detail`, of the probe's type, on the flow `id`, and
/// returns the service's answer.
async fn publish(
    brick: &mut ControlledBrick,
    id: &str,
    detail: &Value,
) -> Result<(), PublishErrorCode> {
    let detail = detail.to_string();
    brick
        .publish(&flow(id), TYPE, detail.as_bytes())
        .await
        .unwrap()
}

fn is_subscribe(message: &ServiceMessage) -> bool {
    matches!(message, ServiceMessage::Subscribe { .. })
}

fn is_release_of(id: &str) -> impl Fn(&ServiceMessage) -> bool {
    move |message| matches!(message, ServiceMessage::Release { flow } if flow.as_str() == id)
}

/// Whether `event`, as `watch --json` prints it, is an update carrying `detail`.
fn is_update(event: &Value, detail: &Value) -> bool {
    event["event"] == "update" && event["data"]["data"] == *detail
}

#[tokio::test]
async fn a_watched_flow_receives_what_the_brick_publishes() {
    let service = Service::start().await;
    let mut watch = service.watch(&["a"]);
    let mut brick = service.brick().await;
    watch.subscribed("probe/a").await;

    let first = json!({ "id": "a", "n": 1 });
    assert_eq!(publish(&mut brick, "a", &first).await, Ok(()));
    let event = watch.next().await;
    assert_eq!(event["event"], "update");
    let cloud_event = &event["data"];
    assert_eq!(cloud_event["specversion"], "1.0");
    assert_eq!(cloud_event["source"], "nowcast://bricks/probe");
    assert_eq!(cloud_event["type"], TYPE);
    assert_eq!(cloud_event["subject"], "probe/a");
    assert_eq!(
        cloud_event["dataschema"],
        "nowcast://bricks/probe/types/probe.item/v1"
    );
    assert!(cloud_event["id"].is_string(), "{cloud_event}");
    assert!(cloud_event["time"].is_string(), "{cloud_event}");
    assert_eq!(cloud_event["data"], first);

    // The stream stays open: the next value arrives live.
    let second = json!({ "id": "a", "n": 2 });
    assert_eq!(publish(&mut brick, "a", &second).await, Ok(()));
    let event = watch.next().await;
    assert!(is_update(&event, &second), "{event}");
    assert_ne!(event["data"]["id"], cloud_event["id"]);
}

#[tokio::test]
async fn a_late_subscriber_gets_the_last_value() {
    let service = Service::start().await;
    let mut first = service.watch(&["a"]);
    let mut brick = service.brick().await;
    first.subscribed("probe/a").await;
    for n in [1, 2] {
        let detail = json!({ "id": "a", "n": n });
        assert_eq!(publish(&mut brick, "a", &detail).await, Ok(()));
        assert!(is_update(&first.next().await, &detail));
    }
    let last = json!({ "id": "a", "n": 2 });

    // `subscribe` answers with the last value.
    let subscribed = service
        .run(&["--json", "subscribe", "probe", "item", &params("a")])
        .await
        .json();
    assert_eq!(subscribed["flow"], "probe/a");
    assert_eq!(subscribed["last"]["data"], last);

    // A stream opened later starts with it.
    let mut second = service.watch(&["a"]);
    second.subscribed("probe/a").await;
    let event = second.next().await;
    assert!(is_update(&event, &last), "{event}");
}

/// A hook-based agent that subscribes to a flow another session already
/// follows reads what it published before, and resumes from an id.
#[tokio::test]
async fn a_late_subscriber_reads_the_history() {
    let service = Service::start().await;
    let mut first = service.watch(&["a"]);
    let mut brick = service.brick().await;
    first.subscribed("probe/a").await;
    let details: Vec<Value> = (1..=3).map(|n| json!({ "id": "a", "n": n })).collect();
    for detail in &details {
        assert_eq!(publish(&mut brick, "a", detail).await, Ok(()));
        assert!(is_update(&first.next().await, detail));
    }

    let mut agent = service.agent();
    agent
        .run(&["--json", "subscribe", "probe", "item", &params("a")])
        .await
        .json();
    let all = agent
        .run(&["--json", "get", "probe/a", "--after", "0"])
        .await
        .json();
    let read: Vec<&Value> = all["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| &e["data"])
        .collect();
    assert_eq!(read, details.iter().collect::<Vec<_>>());
    // `0` predates this run of the service: the history may miss older events.
    assert_eq!(all["gap"], true);

    let first_id = all["events"][0]["id"].as_str().unwrap();
    let after = agent
        .run(&["--json", "get", "probe/a", "--after", first_id])
        .await
        .json();
    assert_eq!(after["gap"], false);
    assert_eq!(after["events"].as_array().unwrap().len(), 2);
    assert_eq!(after["events"][0]["data"], details[1]);
}

#[tokio::test]
async fn a_refresh_reaches_the_brick_which_publishes_again() {
    let service = Service::start().await;
    let mut watch = service.watch(&["a"]);
    let mut brick = service.brick().await;
    watch.subscribed("probe/a").await;

    // `refresh` acts on the flows of its anchor: the agent subscribes, then
    // refreshes, each through a hook of its own.
    let mut agent = service.agent();
    let subscribed = agent
        .run(&["--json", "subscribe", "probe", "item", &params("a")])
        .await
        .json();
    assert_eq!(subscribed["flow"], "probe/a");
    let refreshed = agent.run(&["--json", "refresh", "probe/a"]).await;
    assert_eq!(refreshed.status, Some(0), "{refreshed:?}");
    assert_eq!(refreshed.stdout, "", "{refreshed:?}");

    received(
        &mut brick,
        "the brick gets the refresh",
        |message| matches!(message, ServiceMessage::Refresh { flow } if flow.as_str() == "a"),
    )
    .await;
    // The test brick answers a refresh with a publication of its own.
    let event = watch.next().await;
    assert!(
        is_update(&event, &json!({ "id": "a", "refresh": 1 })),
        "{event}"
    );
}

#[tokio::test]
async fn an_invalid_publication_is_refused_and_never_reaches_clients() {
    let service = Service::start().await;
    let mut watch = service.watch(&["a"]);
    let mut brick = service.brick().await;
    watch.subscribed("probe/a").await;

    let refused = [
        (
            flow("a"),
            TYPE,
            &b"{not json"[..],
            PublishErrorCode::InvalidJson,
        ),
        (
            flow("a"),
            "probe.other/v1",
            &b"{}"[..],
            PublishErrorCode::UndeclaredType,
        ),
        (flow("b"), TYPE, &b"{}"[..], PublishErrorCode::UnknownFlow),
    ];
    for (flow, data_type, detail, code) in refused {
        let published = brick.publish(&flow, data_type, detail).await.unwrap();
        assert_eq!(published, Err(code), "{flow:?} {data_type}");
    }
    let valid = json!({ "id": "a", "valid": true });
    assert_eq!(publish(&mut brick, "a", &valid).await, Ok(()));

    // The first event the client sees is the valid one.
    let event = watch.next().await;
    assert!(is_update(&event, &valid), "{event}");
}

#[tokio::test]
async fn a_crashed_brick_is_relaunched_and_its_flows_resume() {
    let service = Service::start().await;
    let mut watch = service.watch(&["a"]);
    let mut brick = service.brick().await;
    watch.subscribed("probe/a").await;
    // `flow-error` is not replayed to a stream opened later: wait for the
    // stream to be open, seen as a first value reaching it.
    let before = json!({ "id": "a", "before": "crash" });
    assert_eq!(publish(&mut brick, "a", &before).await, Ok(()));
    assert!(is_update(&watch.next().await, &before));

    brick.crash().await.unwrap();
    let event = watch.next().await;
    assert_eq!(event["event"], "flow-error", "{event}");
    assert_eq!(event["data"]["flow"], "probe/a");

    // Relaunched after the backoff, and subscribed to the live flow again.
    let mut brick = service.brick().await;
    let messages = received(
        &mut brick,
        "the relaunched brick is subscribed",
        is_subscribe,
    )
    .await;
    let ServiceMessage::Subscribe { kind, params, .. } =
        messages.iter().find(|m| is_subscribe(m)).unwrap()
    else {
        unreachable!()
    };
    assert_eq!(kind, "item");
    assert_eq!(
        serde_json::from_slice::<Value>(params).unwrap(),
        json!({ "id": "a" })
    );

    let detail = json!({ "id": "a", "after": "crash" });
    assert_eq!(publish(&mut brick, "a", &detail).await, Ok(()));
    let event = watch.next().await;
    assert!(is_update(&event, &detail), "{event}");
}

#[tokio::test]
async fn a_brick_that_never_gets_ready_is_unavailable() {
    let service = Service::start_with(Mode::NeverReady).await;
    let params = params("a");
    let args = ["--json", "subscribe", "probe", "item", &params];
    let started = Instant::now();
    let (ran, brick) = tokio::join!(service.run(&args), service.brick(),);

    assert_eq!(ran.error_code(), "brick_unavailable");
    // The contract gives the brick 5 s to answer `hello`.
    assert!(
        started.elapsed() >= Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    // And stops it.
    timeout(TIMEOUT, brick.exited())
        .await
        .expect("the brick is stopped")
        .unwrap();
}

#[tokio::test]
async fn the_subscriptions_of_a_dead_anchor_are_released() {
    let service = Service::start().await;
    let mut agent = service.agent();

    // Each hook is a short-lived tool; the subscription lives with the agent.
    let subscribed = agent
        .run(&["--json", "subscribe", "probe", "item", &params("a")])
        .await
        .json();
    assert_eq!(subscribed["flow"], "probe/a");
    assert_eq!(subscribed["last"], Value::Null);
    let mut brick = service.brick().await;
    let got = agent.run(&["--json", "get", "probe/a"]).await;
    assert_eq!(got.json(), Value::Null, "subscribed, nothing published yet");

    let detail = json!({ "id": "a", "for": AGENT_SESSION });
    assert_eq!(publish(&mut brick, "a", &detail).await, Ok(()));
    let got = agent.run(&["--json", "get", "probe/a"]).await.json();
    assert_eq!(got["data"], detail);
    // Another anchor does not see the agent's flows.
    let elsewhere = service.run(&["--json", "get", "probe/a"]).await;
    assert_eq!(elsewhere.error_code(), "unknown_flow");
    assert!(
        !brick
            .received()
            .await
            .unwrap()
            .iter()
            .any(is_release_of("a")),
        "released while the agent lives"
    );

    agent.kill().await;
    received(&mut brick, "the flow is released", is_release_of("a")).await;
}

#[tokio::test]
async fn a_brick_without_subscribers_stops_after_the_grace_period() {
    let service = Service::start().await;
    let mut watch = service.watch(&["a"]);
    let mut brick = service.brick().await;
    watch.subscribed("probe/a").await;

    watch.kill().await;
    let left = Instant::now();
    received(&mut brick, "the flow is released", is_release_of("a")).await;
    // The contract's grace period is 30 s; give the stop its own delays.
    let grace = Duration::from_secs(30);
    timeout(grace + TIMEOUT, brick.exited())
        .await
        .expect("the brick stops after the grace period")
        .unwrap();
    assert!(
        left.elapsed() >= grace,
        "stopped after {:?}",
        left.elapsed()
    );
}

#[tokio::test]
async fn the_tool_from_a_plain_shell_is_its_own_anchor() {
    let service = Service::start().await;

    let (ran, mut shell) = service
        .run_from_shell(&["--json", "subscribe", "probe", "item", &params("a")])
        .await;
    let subscribed = ran.json();
    assert_eq!(subscribed["flow"], "probe/a");
    assert!(subscribed["subscription"].is_string(), "{subscribed}");

    // The subscription ended with the tool, not with the shell, still alive.
    let mut brick = service.brick().await;
    received(&mut brick, "the flow is released", is_release_of("a")).await;
    assert!(shell.is_alive(), "the shell outlives the tool");
    let (got, _) = service.run_from_shell(&["--json", "get", "probe/a"]).await;
    assert_eq!(got.error_code(), "unknown_flow");
}
