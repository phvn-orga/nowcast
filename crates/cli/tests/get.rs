//! `nowcast get <flow>` against a fake service.

mod common;

use common::{Anchoring, Fake, nowcast, run, stderr, stdout};
use serde_json::{Value, json};

/// The hook scenario: an agent's hooks share its anchor, one subscribes, a
/// later one reads the last value.
#[tokio::test]
async fn prints_the_last_value_and_exits_0() {
    let fake = Fake::start(Anchoring::Shared).await;
    let subscribed = run(
        &mut nowcast(&fake),
        &[
            "--session",
            "agent-1",
            "subscribe",
            "probe",
            "item",
            r#"{"id":"a"}"#,
        ],
    )
    .await;
    assert!(subscribed.status.success(), "{}", stderr(&subscribed));
    fake.publish("probe/a", json!({ "number": 123, "state": "open" }));

    let output = run(
        &mut nowcast(&fake),
        &["--session", "agent-1", "get", "probe/a"],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "probe/a probe.item/v1 2026-11-06T10:00:00Z\n{\n  \"number\": 123,\n  \"state\": \"open\"\n}\n"
    );
    fake.assert_sessions_first();
}

#[tokio::test]
async fn prints_the_event_as_json() {
    let fake = Fake::start(Anchoring::Shared).await;
    let flow = fake.subscribe_shared("a");
    let event = fake.publish(&flow, json!({ "n": 1 }));

    let output = run(&mut nowcast(&fake), &["get", "probe/a", "--json"]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let printed: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(printed, event);
    assert_eq!(stdout(&output).lines().count(), 1);
    fake.assert_sessions_first();
}

#[tokio::test]
async fn a_flow_without_value_exits_0() {
    let fake = Fake::start(Anchoring::Shared).await;
    fake.subscribe_shared("a");

    let output = run(&mut nowcast(&fake), &["get", "probe/a"]).await;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "");
    assert!(
        stderr(&output).contains("no value yet"),
        "{}",
        stderr(&output)
    );

    let output = run(&mut nowcast(&fake), &["--json", "get", "probe/a"]).await;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "null\n");
    assert_eq!(stderr(&output), "");
    fake.assert_sessions_first();
}

/// From a plain shell, the tool is its own anchor: what `subscribe` made ended
/// with it, so a later `get` is not subscribed.
#[tokio::test]
async fn an_unsubscribed_flow_exits_1_with_the_code() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let subscribed = run(
        &mut nowcast(&fake),
        &["subscribe", "probe", "item", r#"{"id":"a"}"#],
    )
    .await;
    assert!(subscribed.status.success());

    let output = run(&mut nowcast(&fake), &["get", "probe/a"]).await;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "nowcast: unknown_flow: not subscribed to this flow\n"
    );

    let output = run(&mut nowcast(&fake), &["get", "probe/a", "--json"]).await;
    assert_eq!(output.status.code(), Some(1));
    let error: Value = serde_json::from_str(&stderr(&output)).unwrap();
    assert_eq!(
        error,
        json!({ "error": { "code": "unknown_flow", "message": "not subscribed to this flow" } })
    );
    fake.assert_sessions_first();
}

#[tokio::test]
async fn the_flow_id_is_one_percent_encoded_segment() {
    let fake = Fake::start(Anchoring::Shared).await;
    let flow = fake.subscribe_shared("org/repo#main é");
    fake.publish(&flow, json!(true));

    let output = run(&mut nowcast(&fake), &["--json", "get", &flow]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let request = fake.log().pop().unwrap();
    assert_eq!(request.path, "/v1/flows/probe%2Forg%2Frepo%23main%20%C3%A9");
}

#[tokio::test]
async fn an_invalid_flow_id_exits_2_without_a_call() {
    let fake = Fake::start(Anchoring::Shared).await;
    for flow in ["no-separator", "Bad/brick", "/x"] {
        let output = run(&mut nowcast(&fake), &["get", flow]).await;
        assert_eq!(output.status.code(), Some(2), "{flow}");
        assert!(
            stderr(&output).contains("invalid flow id"),
            "{}",
            stderr(&output)
        );
    }
    assert!(fake.log().is_empty());
}

#[tokio::test]
async fn an_absent_service_exits_3() {
    let fake = Fake::absent(Anchoring::Shared);
    let output = run(&mut nowcast(&fake), &["get", "probe/a"]).await;
    assert_eq!(output.status.code(), Some(3));
    assert!(
        stderr(&output).starts_with("nowcast: cannot reach the service at "),
        "{}",
        stderr(&output)
    );

    let output = run(&mut nowcast(&fake), &["--json", "get", "probe/a"]).await;
    let error: Value = serde_json::from_str(&stderr(&output)).unwrap();
    assert_eq!(error["error"]["code"], "service_unreachable");
}

#[tokio::test]
async fn an_answer_outside_the_contract_exits_4() {
    let fake = Fake::start(Anchoring::Shared).await;
    fake.subscribe_shared("a");
    fake.can("/v1/flows/probe%2Fa", 500, "text/plain", "boom");
    let output = run(&mut nowcast(&fake), &["get", "probe/a"]).await;
    assert_eq!(output.status.code(), Some(4));
    assert!(
        stderr(&output).contains("unexpected answer"),
        "{}",
        stderr(&output)
    );

    let fake = Fake::start(Anchoring::Shared).await;
    fake.subscribe_shared("a");
    fake.can("/v1/flows/probe%2Fa", 200, "application/json", "not json");
    let output = run(&mut nowcast(&fake), &["get", "probe/a"]).await;
    assert_eq!(output.status.code(), Some(4));
}

/// `--after`: the kept events after the id, each printed as `get` prints the
/// last value.
#[tokio::test]
async fn after_prints_the_history() {
    let fake = Fake::start(Anchoring::Shared).await;
    let flow = fake.subscribe_shared("a");
    let first = fake.publish(&flow, json!({ "n": 1 }));
    fake.publish(&flow, json!({ "n": 2 }));
    fake.publish(&flow, json!({ "n": 3 }));
    let id = first["id"].as_str().unwrap();

    let output = run(&mut nowcast(&fake), &["get", "probe/a", "--after", id]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let one = |n: u8| format!("probe/a probe.item/v1 2026-11-06T10:00:00Z\n{{\n  \"n\": {n}\n}}\n");
    assert_eq!(stdout(&output), format!("{}{}", one(2), one(3)));
    let request = fake.log().pop().unwrap();
    assert_eq!(
        (request.path.as_str(), request.query),
        ("/v1/flows/probe%2Fa", Some(format!("after={id}")))
    );
    fake.assert_sessions_first();
}

#[tokio::test]
async fn after_prints_the_answer_as_json() {
    let fake = Fake::start(Anchoring::Shared).await;
    let flow = fake.subscribe_shared("a");
    let event = fake.publish(&flow, json!({ "n": 1 }));

    let output = run(
        &mut nowcast(&fake),
        &["--json", "get", "probe/a", "--after", "0"],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let printed: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(printed, json!({ "gap": false, "events": [event] }));
}

#[tokio::test]
async fn after_prints_a_gap_first() {
    let fake = Fake::start(Anchoring::Shared).await;
    fake.subscribe_shared("a");
    fake.can(
        "/v1/flows/probe%2Fa",
        200,
        "application/json",
        r#"{"gap":true,"events":[]}"#,
    );
    let output = run(&mut nowcast(&fake), &["get", "probe/a", "--after", "0"]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), "flow-gap probe/a\n");
}

#[tokio::test]
async fn after_with_a_history_outside_the_contract_exits_4() {
    let fake = Fake::start(Anchoring::Shared).await;
    fake.subscribe_shared("a");
    fake.can(
        "/v1/flows/probe%2Fa",
        200,
        "application/json",
        r#"{"events":1}"#,
    );
    let output = run(&mut nowcast(&fake), &["get", "probe/a", "--after", "0"]).await;
    assert_eq!(output.status.code(), Some(4), "{}", stderr(&output));
}

#[tokio::test]
async fn after_that_is_not_an_id_exits_2_without_a_call() {
    let fake = Fake::start(Anchoring::Shared).await;
    let output = run(&mut nowcast(&fake), &["get", "probe/a", "--after", "x"]).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(fake.log().is_empty());
}
