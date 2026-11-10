//! `nowcast status` against a fake service.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::{Anchoring, Fake, bare, nowcast, run, stderr, stdout};
use serde_json::{Value, json};

const INSTALLED: &str = env!("CARGO_PKG_VERSION");

const MISSING: &str = "setup      missing: run nowcast setup to let systemd start the service\n";

fn status(version: &str) -> Value {
    json!({
        "service": { "version": version, "started_at": "2026-11-08T16:41:00Z", "path": null },
        "bricks": [ { "name": "probe", "state": "running", "pid": 4242, "crashes": 0 } ],
        "sessions": [
            { "id": "s", "subscriptions": [ { "id": "s-1", "flow": "probe/a" } ] },
        ],
        "flows": [ { "id": "probe/a", "subscribers": 1, "last_event": null } ],
    })
}

#[tokio::test]
async fn prints_the_state_without_declaring_a_session() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let body = status(INSTALLED);
    fake.can("/v1/status", 200, "application/json", &body.to_string());

    let output = run(&mut nowcast(&fake), &["status"]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.starts_with(&format!(
            "service    {INSTALLED}, running since 2026-11-08T16:41:00Z\ninstalled  {INSTALLED}\n"
        )),
        "{text}"
    );
    assert!(text.contains("  probe  running, pid 4242\n"), "{text}");

    let output = run(&mut nowcast(&fake), &["status", "--json"]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let printed: Value = serde_json::from_str(&stdout(&output)).unwrap();
    let expected = json!({ "installed": INSTALLED, "running": body, "setup": "missing" });
    assert_eq!(printed, expected);

    let calls: Vec<_> = fake.log().into_iter().map(|r| (r.method, r.path)).collect();
    let status = ("GET".to_owned(), "/v1/status".to_owned());
    assert_eq!(calls, [status.clone(), status]);
}

#[tokio::test]
async fn flags_a_service_running_another_version() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    fake.can(
        "/v1/status",
        200,
        "application/json",
        &status("0.0.1").to_string(),
    );
    let output = run(&mut nowcast(&fake), &["status"]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let expected = format!(
        "service    0.0.1, running since 2026-11-08T16:41:00Z\n\
         installed  {INSTALLED}, not the service's: run nowcast reload to run it\n"
    );
    assert!(
        stdout(&output).starts_with(&expected),
        "{}",
        stdout(&output)
    );
}

#[tokio::test]
async fn a_service_not_listening_is_stopped() {
    let fake = Fake::absent(Anchoring::PerProcess);
    let output = run(&mut nowcast(&fake), &["status"]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!("service    stopped\ninstalled  {INSTALLED}\n{MISSING}")
    );
    let output = run(&mut nowcast(&fake), &["status", "--json"]).await;
    let printed: Value = serde_json::from_str(&stdout(&output)).unwrap();
    let expected = json!({ "installed": INSTALLED, "running": null, "setup": "missing" });
    assert_eq!(printed, expected);
}

#[tokio::test]
async fn a_service_older_than_the_route_says_to_restart_it() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let error = json!({ "error": { "code": "not_found", "message": "no such route" } });
    fake.can("/v1/status", 404, "application/json", &error.to_string());
    let output = run(&mut nowcast(&fake), &["status"]).await;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let message = stderr(&output);
    assert!(
        message.starts_with("nowcast: service_too_old: "),
        "{message}"
    );
    assert!(message.contains("nowcast reload"), "{message}");
}

/// `nowcast` finding the fake's socket through `XDG_RUNTIME_DIR`, as when
/// systemd holds it, with a `systemctl` that prints `states`.
fn through_systemd(fake: &Fake, bin: &Path, states: &str) -> tokio::process::Command {
    let systemctl = bin.join("systemctl");
    std::fs::write(&systemctl, format!("#!/bin/sh\nprintf '{states}'\n")).unwrap();
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let mut command = bare();
    command
        .env("XDG_RUNTIME_DIR", fake.runtime_dir())
        .env("PATH", path);
    command
}

#[tokio::test]
async fn a_service_systemd_would_start_is_stopped_and_left_alone() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    fake.can(
        "/v1/status",
        200,
        "application/json",
        &status(INSTALLED).to_string(),
    );
    let bin = tempfile::tempdir().unwrap();

    let mut command = through_systemd(&fake, bin.path(), r"active\ninactive\n");
    let output = run(&mut command, &["status"]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!("service    stopped\ninstalled  {INSTALLED}\n{MISSING}")
    );
    assert!(fake.log().is_empty(), "no request reaches the socket");

    // Running: asked as usual.
    let mut command = through_systemd(&fake, bin.path(), r"active\nactive\n");
    let output = run(&mut command, &["status"]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).starts_with(&format!("service    {INSTALLED}")));
    assert_eq!(fake.log().len(), 1);
}
