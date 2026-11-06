//! The session every command declares first, and where the tool finds the
//! service.

mod common;

use common::{Anchoring, Fake, bare, nowcast, run, stderr};
use serde_json::json;

fn declared(fake: &Fake) -> Vec<serde_json::Value> {
    fake.log()
        .into_iter()
        .filter(|r| r.path == "/v1/session")
        .map(|r| {
            assert_eq!(r.method, "PUT");
            r.body.unwrap()
        })
        .collect()
}

#[tokio::test]
async fn the_default_session_is_the_process() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let mut command = nowcast(&fake);
    command
        .args(["subscribe", "probe", "item", r#"{"id":"a"}"#])
        .stdout(std::process::Stdio::piped());
    let child = command.spawn().unwrap();
    let pid = child.id().unwrap();
    let output = child.wait_with_output().await.unwrap();
    assert!(output.status.success());
    assert_eq!(declared(&fake), [json!({ "id": format!("cli-{pid}") })]);
    fake.assert_sessions_first();
}

#[tokio::test]
async fn the_session_comes_from_the_flag_or_the_environment() {
    let fake = Fake::start(Anchoring::Shared).await;
    fake.subscribe_shared("a");
    run(
        &mut nowcast(&fake),
        &["--session", "from-flag", "get", "probe/a"],
    )
    .await;
    run(
        nowcast(&fake).env("NOWCAST_SESSION", "from-env"),
        &["get", "probe/a"],
    )
    .await;
    run(
        nowcast(&fake).env("NOWCAST_SESSION", "from-env"),
        &["get", "probe/a", "--session", "flag-wins"],
    )
    .await;
    assert_eq!(
        declared(&fake),
        [
            json!({ "id": "from-flag" }),
            json!({ "id": "from-env" }),
            json!({ "id": "flag-wins" }),
        ]
    );
    fake.assert_sessions_first();
}

#[tokio::test]
async fn an_empty_session_exits_2_without_a_call() {
    let fake = Fake::start(Anchoring::Shared).await;
    let output = run(&mut nowcast(&fake), &["--session", "", "get", "probe/a"]).await;
    assert_eq!(output.status.code(), Some(2));
    let output = run(
        nowcast(&fake).env("NOWCAST_SESSION", ""),
        &["get", "probe/a"],
    )
    .await;
    assert_eq!(output.status.code(), Some(2));
    assert!(fake.log().is_empty());
}

#[tokio::test]
async fn a_refused_declaration_stops_the_command() {
    let fake = Fake::start(Anchoring::Shared).await;
    fake.can(
        "/v1/session",
        400,
        "application/json",
        r#"{"error":{"code":"invalid_request","message":"bad id"}}"#,
    );
    let output = run(&mut nowcast(&fake), &["get", "probe/a"]).await;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stderr(&output), "nowcast: invalid_request: bad id\n");
    assert_eq!(fake.log().len(), 1, "nothing after the declaration");

    let fake = Fake::start(Anchoring::Shared).await;
    fake.can("/v1/session", 200, "text/plain", "ok");
    let output = run(&mut nowcast(&fake), &["get", "probe/a"]).await;
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(fake.log().len(), 1, "nothing after the declaration");
}

#[tokio::test]
async fn the_socket_defaults_to_the_runtime_dir() {
    let fake = Fake::start(Anchoring::Shared).await;
    fake.subscribe_shared("a");
    let output = run(
        bare().env("XDG_RUNTIME_DIR", fake.runtime_dir()),
        &["get", "probe/a"],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(fake.log().len(), 2);
}

#[tokio::test]
async fn the_socket_flag_wins_over_the_environment() {
    let fake = Fake::start(Anchoring::Shared).await;
    fake.subscribe_shared("a");
    let socket = fake.socket().to_str().unwrap().to_owned();
    let output = run(
        bare()
            .env("NOWCAST_SOCKET", "/nonexistent/nowcast.sock")
            .env("XDG_RUNTIME_DIR", "/nonexistent"),
        &["get", "probe/a", "--socket", &socket],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));

    let output = run(
        bare()
            .env("NOWCAST_SOCKET", &socket)
            .env("XDG_RUNTIME_DIR", "/nonexistent"),
        &["get", "probe/a"],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
}

#[tokio::test]
async fn no_runtime_dir_and_no_socket_exits_2() {
    let output = run(&mut bare(), &["get", "probe/a"]).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("XDG_RUNTIME_DIR"),
        "{}",
        stderr(&output)
    );
}
