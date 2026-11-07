//! The claude-code brick, native to nowcast, run by a real daemon as
//! `nowcast brick claude-code` once enabled.

mod harness;

use harness::{Service, wait_for};
use serde_json::json;

/// A session sends its measures on its own flow; a reader of the list and
/// of the quotas sees them, the quotas from the session's own readings.
#[tokio::test]
async fn session_measures_reach_the_list_and_the_quotas() {
    let service = Service::start().await;
    service.enable("claude-code").await;
    let mut agent = service.agent();

    // Discovery follows the bricks directory: the brick shows once it has
    // seen the link.
    wait_for("the brick is known", async || {
        let ran = agent
            .run(&[
                "--json",
                "subscribe",
                "claude-code",
                "session",
                r#"{"id":"s1"}"#,
            ])
            .await;
        (ran.status == Some(0)).then_some(())
    })
    .await;
    for (kind, params) in [("sessions", "{}"), ("quotas", "{}")] {
        let ran = agent
            .run(&["--json", "subscribe", "claude-code", kind, params])
            .await;
        assert_eq!(ran.status, Some(0), "{ran:?}");
    }

    let measure = json!({
        "model": "claude-opus-5-5",
        "context": { "tokens": 1000, "window": 200000, "percent": 1 },
        "tokens": { "input": 1, "output": 2, "cache_creation": 3, "cache_read": 4 },
        "rate_limits": [{ "name": "five_hour", "percent": 23.5, "resets_at": "2026-11-07T18:00:00Z" }],
    });
    let sent = agent
        .run_with_stdin(&["input", "claude-code/session/s1"], &measure.to_string())
        .await;
    assert_eq!(sent.status, Some(0), "{sent:?}");

    let list = wait_for("the session in the list", async || {
        let got = agent.run(&["--json", "get", "claude-code/sessions"]).await;
        let data = got.json()["data"].clone();
        (data["sessions"].as_array().is_some_and(|s| !s.is_empty())).then_some(data)
    })
    .await;
    assert_eq!(list["sessions"][0]["session"], "s1");
    assert_eq!(list["sessions"][0]["tokens"]["cache_read"], 4);

    // No Claude Code files in this home: the usage API is not read, the
    // session's reading still counts.
    let quotas = wait_for("the quotas", async || {
        let got = agent.run(&["--json", "get", "claude-code/quotas"]).await;
        let data = got.json()["data"].clone();
        (data["usage_api"]["status"] == "no_credentials" && data["windows"] != json!([]))
            .then_some(data)
    })
    .await;
    assert_eq!(quotas["windows"][0]["name"], "five_hour");
    assert_eq!(quotas["windows"][0]["source"], "session");

    // Another process cannot take the session's own flow.
    let mut other = service.agent();
    let taken = other
        .run(&["--json", "subscribe", "claude-code", "session", r#"{"id":"s1"}"#])
        .await;
    assert_eq!(taken.error_code(), "subscription_refused", "{taken:?}");
    assert!(taken.stderr.contains("taken"), "{taken:?}");
    drop(other);

    // A measure that does not match the accepted type never reaches it.
    let refused = agent
        .run_with_stdin(
            &["--json", "input", "claude-code/session/s1"],
            r#"{"cost_usd":"x"}"#,
        )
        .await;
    assert_eq!(refused.error_code(), "invalid_input");
}
