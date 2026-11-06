//! `nowcast refresh <flow>` against a fake service.

mod common;

use std::process::Output;

use common::{Anchoring, Fake, nowcast, run, stderr, stdout};
use serde_json::{Value, json};

/// The hook scenario: a later hook of the agent asks for a fresh value of a
/// flow its anchor is subscribed to.
#[tokio::test]
async fn asks_for_a_refresh_and_prints_nothing() {
    let fake = Fake::start(Anchoring::Shared).await;
    let flow = fake.subscribe_shared("org/repo#main");
    for json in [false, true] {
        let mut args = vec!["refresh", &flow];
        if json {
            args.push("--json");
        }
        let output = run(&mut nowcast(&fake), &args).await;
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        assert_eq!(stdout(&output), "");
        assert_eq!(stderr(&output), "");
    }
    let calls: Vec<_> = fake
        .log()
        .into_iter()
        .map(|r| (r.method, r.path, r.status))
        .collect();
    let refresh = (
        "POST".to_owned(),
        "/v1/flows/probe%2Forg%2Frepo%23main/refresh".to_owned(),
        202,
    );
    let session = ("PUT".to_owned(), "/v1/session".to_owned(), 204);
    assert_eq!(calls, [session.clone(), refresh.clone(), session, refresh]);
    fake.assert_sessions_first();
}

/// The two commands' code and stderr, side by side.
async fn get_and_refresh(fake: &Fake, flow: &str, json: bool) -> [(Option<i32>, String); 2] {
    let mut results = Vec::new();
    for command in ["get", "refresh"] {
        let mut args = vec![command, flow];
        if json {
            args.push("--json");
        }
        let output: Output = run(&mut nowcast(fake), &args).await;
        results.push((output.status.code(), stderr(&output)));
    }
    results.try_into().unwrap()
}

/// From a plain shell, the anchor holds no subscription: the flow is unknown,
/// as `get` reports it.
#[tokio::test]
async fn an_unsubscribed_flow_fails_like_get() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    for json in [false, true] {
        let [get, refresh] = get_and_refresh(&fake, "probe/a", json).await;
        assert_eq!(refresh, get);
        assert_eq!(refresh.0, Some(1));
    }
    let output = run(&mut nowcast(&fake), &["refresh", "probe/a", "--json"]).await;
    let error: Value = serde_json::from_str(&stderr(&output)).unwrap();
    assert_eq!(
        error,
        json!({ "error": { "code": "unknown_flow", "message": "not subscribed to this flow" } })
    );
    fake.assert_sessions_first();
}

/// A service that forgot the session between the declaration and the call.
#[tokio::test]
async fn a_missing_session_fails_like_get() {
    let fake = Fake::start(Anchoring::Shared).await;
    let body = r#"{"error":{"code":"session_required","message":"Declare the session first."}}"#;
    fake.can("/v1/flows/probe%2Fa", 428, "application/json", body);
    fake.can("/v1/flows/probe%2Fa/refresh", 428, "application/json", body);
    for json in [false, true] {
        let [get, refresh] = get_and_refresh(&fake, "probe/a", json).await;
        assert_eq!(refresh, get);
        assert_eq!(refresh.0, Some(1));
    }
    let output = run(&mut nowcast(&fake), &["refresh", "probe/a"]).await;
    assert_eq!(
        stderr(&output),
        "nowcast: session_required: Declare the session first.\n"
    );
}

#[tokio::test]
async fn an_absent_service_fails_like_get() {
    let fake = Fake::absent(Anchoring::Shared);
    for json in [false, true] {
        let [get, refresh] = get_and_refresh(&fake, "probe/a", json).await;
        assert_eq!(refresh, get);
        assert_eq!(refresh.0, Some(3));
    }
}

#[tokio::test]
async fn an_invalid_flow_id_exits_2_without_a_call() {
    let fake = Fake::start(Anchoring::Shared).await;
    for flow in ["no-separator", "Bad/brick", "/x"] {
        let [get, refresh] = get_and_refresh(&fake, flow, false).await;
        assert_eq!(refresh, get, "{flow}");
        assert_eq!(refresh.0, Some(2), "{flow}");
    }
    assert!(fake.log().is_empty());
}

/// `202` is the only success: a `200` or `204` is outside the contract.
#[tokio::test]
async fn another_success_status_exits_4() {
    for status in [200, 204] {
        let fake = Fake::start(Anchoring::Shared).await;
        fake.subscribe_shared("a");
        fake.can("/v1/flows/probe%2Fa/refresh", status, "", "");
        let output = run(&mut nowcast(&fake), &["refresh", "probe/a"]).await;
        assert_eq!(output.status.code(), Some(4), "{status}");
        assert!(
            stderr(&output).contains("unexpected answer"),
            "{}",
            stderr(&output)
        );
    }
}
