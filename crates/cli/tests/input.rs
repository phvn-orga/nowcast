//! `nowcast input <flow>` against a fake service.

mod common;

use common::{Anchoring, Fake, nowcast, run_with_stdin, stderr, stdout};
use serde_json::json;

/// The hook scenario: a later hook of the agent sends a value to the brick of
/// a flow its anchor is subscribed to.
#[tokio::test]
async fn sends_stdin_and_prints_nothing() {
    let fake = Fake::start(Anchoring::Shared).await;
    let flow = fake.subscribe_shared("org/repo#main");
    let output = run_with_stdin(&mut nowcast(&fake), &["input", &flow], "{\"n\": 1}\n").await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), "");
    assert_eq!(stderr(&output), "");
    let log = fake.log();
    let input = log.last().unwrap();
    assert_eq!(
        (input.method.as_str(), input.path.as_str(), input.status),
        ("POST", "/v1/flows/probe%2Forg%2Frepo%23main/input", 202)
    );
    assert_eq!(input.body, Some(json!({ "n": 1 })));
    fake.assert_sessions_first();
}

/// From a plain shell, the anchor holds no subscription: the flow is unknown.
#[tokio::test]
async fn an_unsubscribed_flow_exits_1() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let output = run_with_stdin(&mut nowcast(&fake), &["input", "probe/a"], "{}").await;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "nowcast: unknown_flow: not subscribed to this flow\n"
    );
}

/// Stdin that is not one JSON value, or an invalid flow id, exits 2 before
/// any call.
#[tokio::test]
async fn invalid_usage_exits_2_without_a_call() {
    let fake = Fake::start(Anchoring::Shared).await;
    for (flow, stdin) in [
        ("probe/a", ""),
        ("probe/a", "{} {}"),
        ("no-separator", "{}"),
    ] {
        let output = run_with_stdin(&mut nowcast(&fake), &["input", flow], stdin).await;
        assert_eq!(output.status.code(), Some(2), "{flow} {stdin:?}");
    }
    assert!(fake.log().is_empty());
}
