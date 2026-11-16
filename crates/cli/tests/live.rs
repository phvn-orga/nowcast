//! `nowcast live` against a fake service: what it refuses before taking the
//! screen. The screen itself is tested on a test backend, in the binary.

mod common;

use std::os::unix::fs::PermissionsExt;

use common::{Anchoring, Fake, bare, nowcast, run, stderr};
use serde_json::json;

#[tokio::test]
async fn a_service_not_listening_is_reported_stopped() {
    let fake = Fake::absent(Anchoring::PerProcess);
    let output = run(&mut nowcast(&fake), &["live"]).await;
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(
        stderr(&output).contains("the service is stopped, and nowcast live does not start it"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test]
async fn a_service_systemd_would_start_is_left_alone() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let bin = tempfile::tempdir().unwrap();
    let systemctl = bin.path().join("systemctl");
    std::fs::write(&systemctl, "#!/bin/sh\nprintf 'active\\ninactive\\n'\n").unwrap();
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap()
    );
    let mut command = bare();
    command
        .env("XDG_RUNTIME_DIR", fake.runtime_dir())
        .env("PATH", path);
    let output = run(&mut command, &["live"]).await;
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(fake.log().is_empty(), "no request reaches the socket");
}

#[tokio::test]
async fn without_a_terminal_it_points_to_status_and_watch() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let status = json!({
        "service": { "version": "0.0.1", "started_at": "2026-11-08T16:41:00Z", "path": null },
        "bricks": [], "sessions": [], "flows": [],
    });
    fake.can("/v1/status", 200, "application/json", &status.to_string());
    fake.can("/v1/observe", 200, "text/event-stream", ": open\n\n");
    let output = run(&mut nowcast(&fake), &["live"]).await;
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let message = stderr(&output);
    assert!(
        message.contains("nowcast live needs a terminal"),
        "{message}"
    );
    assert!(
        message.contains("nowcast status") && message.contains("nowcast watch"),
        "{message}"
    );
    // No session declared: live belongs to none.
    let calls: Vec<_> = fake.log().into_iter().map(|r| r.path).collect();
    assert_eq!(calls, ["/v1/status", "/v1/observe"]);
}

#[tokio::test]
async fn a_service_without_the_observe_route_says_to_reload() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let status = json!({
        "service": { "version": "0.0.1", "started_at": "2026-11-08T16:41:00Z", "path": null },
        "bricks": [], "sessions": [], "flows": [],
    });
    fake.can("/v1/status", 200, "application/json", &status.to_string());
    let error = json!({ "error": { "code": "not_found", "message": "no such route" } });
    fake.can("/v1/observe", 404, "application/json", &error.to_string());
    let output = run(&mut nowcast(&fake), &["live"]).await;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let message = stderr(&output);
    assert!(
        message.starts_with("nowcast: service_too_old: "),
        "{message}"
    );
    assert!(message.contains("nowcast reload"), "{message}");
}
