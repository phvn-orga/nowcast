//! `nowcast subscribe <brick> <kind> [params]` against a fake service.

mod common;

use common::{Anchoring, Fake, Probe, nowcast, run, stderr, stdout};
use serde_json::{Value, json};

#[tokio::test]
async fn prints_the_subscription() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let output = run(
        &mut nowcast(&fake),
        &["subscribe", "probe", "item", r#"{"id":"a"}"#],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), "subscription s-1\nflow probe/a\n");

    let request = fake.log().pop().unwrap();
    assert_eq!(
        (request.method.as_str(), request.path.as_str()),
        ("POST", "/v1/subscriptions")
    );
    assert_eq!(
        request.body,
        Some(json!({ "brick": "probe", "kind": "item", "params": { "id": "a" } }))
    );
    fake.assert_sessions_first();
}

#[tokio::test]
async fn prints_the_last_value_when_there_is_one() {
    let fake = Fake::start(Anchoring::Shared).await;
    let flow = fake.subscribe_shared("a");
    let event = fake.publish(&flow, json!({ "n": 1 }));

    let output = run(
        &mut nowcast(&fake),
        &["subscribe", "probe", "item", r#"{"id":"a"}"#],
    )
    .await;
    assert_eq!(
        stdout(&output),
        "subscription s-2\nflow probe/a\nlast {\"n\":1}\n"
    );

    let output = run(
        &mut nowcast(&fake),
        &["--json", "subscribe", "probe", "item", r#"{"id":"a"}"#],
    )
    .await;
    let printed: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(
        printed,
        json!({ "subscription": "s-3", "flow": "probe/a", "last": event })
    );
    fake.assert_sessions_first();
}

#[tokio::test]
async fn service_errors_exit_1_with_their_code() {
    let cases = [
        (
            Probe::Ready,
            ["nope", "item", r#"{"id":"a"}"#],
            "unknown_brick",
        ),
        (
            Probe::Ready,
            ["probe", "nope", r#"{"id":"a"}"#],
            "unknown_kind",
        ),
        (
            Probe::Ready,
            ["probe", "item", r#"{"id":3}"#],
            "invalid_params",
        ),
        (
            Probe::Unavailable,
            ["probe", "item", r#"{"id":"a"}"#],
            "brick_unavailable",
        ),
        (
            Probe::Refusing,
            ["probe", "item", r#"{"id":"a"}"#],
            "subscription_refused",
        ),
    ];
    for (probe, args, code) in cases {
        let fake = Fake::start(Anchoring::PerProcess).await;
        fake.set_probe(probe);
        let mut argv = vec!["subscribe"];
        argv.extend(args);
        let output = run(&mut nowcast(&fake), &argv).await;
        assert_eq!(output.status.code(), Some(1), "{code}");
        assert_eq!(stdout(&output), "");
        assert!(
            stderr(&output).starts_with(&format!("nowcast: {code}: ")),
            "{code}: {}",
            stderr(&output)
        );

        argv.push("--json");
        let output = run(&mut nowcast(&fake), &argv).await;
        assert_eq!(output.status.code(), Some(1), "{code}");
        let error: Value = serde_json::from_str(&stderr(&output)).unwrap();
        assert_eq!(error["error"]["code"], code);
        fake.assert_sessions_first();
    }
}

#[tokio::test]
async fn the_refusal_message_carries_the_bricks_code() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    fake.set_probe(Probe::Refusing);
    let output = run(
        &mut nowcast(&fake),
        &["subscribe", "probe", "item", r#"{"id":"a"}"#],
    )
    .await;
    assert_eq!(
        stderr(&output),
        "nowcast: subscription_refused: no_such_item: nothing here\n"
    );
}

#[tokio::test]
async fn params_default_to_an_empty_object() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let output = run(&mut nowcast(&fake), &["subscribe", "probe", "item"]).await;
    assert_eq!(output.status.code(), Some(1));
    let request = fake.log().pop().unwrap();
    assert_eq!(request.body.unwrap()["params"], json!({}));
}

#[tokio::test]
async fn params_that_are_not_json_exit_2_without_a_call() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let output = run(&mut nowcast(&fake), &["subscribe", "probe", "item", "id=a"]).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("PARAMS is not JSON"),
        "{}",
        stderr(&output)
    );
    assert!(fake.log().is_empty());
}

#[tokio::test]
async fn a_subscription_without_its_flow_exits_4() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    fake.can(
        "/v1/subscriptions",
        201,
        "application/json",
        r#"{"subscription":"s-1"}"#,
    );
    let output = run(
        &mut nowcast(&fake),
        &["subscribe", "probe", "item", r#"{"id":"a"}"#],
    )
    .await;
    assert_eq!(output.status.code(), Some(4));
}
