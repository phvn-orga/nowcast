//! `nowcast status`: what runs, and at which version.

use std::fmt::Write as _;
use std::io::Write;
use std::process::Command;

use serde_json::{Value, json};

use crate::client::Client;
use crate::error::CliError;

/// This tool's version: the installed binary's.
const INSTALLED: &str = env!("CARGO_PKG_VERSION");

pub async fn status(
    client: &Client,
    ask_systemd: bool,
    json: bool,
    out: &mut impl Write,
) -> Result<(), CliError> {
    let running = if ask_systemd && waits_for_a_connection(&systemd_states()) {
        None
    } else {
        client.status().await?
    };
    if json {
        writeln!(
            out,
            "{}",
            json!({ "installed": INSTALLED, "running": running })
        )?;
    } else {
        out.write_all(readable(INSTALLED, running.as_ref()).as_bytes())?;
    }
    Ok(())
}

/// What `systemctl --user is-active` says of the socket and the service, one
/// line each; empty when systemd cannot be asked.
fn systemd_states() -> String {
    Command::new("systemctl")
        .args(["--user", "is-active", "nowcast.socket", "nowcast.service"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default()
}

/// Whether systemd holds the socket while the service does not run, or is
/// stopping: a connection would start it.
fn waits_for_a_connection(states: &str) -> bool {
    let mut lines = states.lines();
    let (socket, service) = (lines.next(), lines.next());
    socket == Some("active") && matches!(service, Some("inactive" | "failed" | "deactivating"))
}

/// The status for a human, `running` being the service's `GET /v1/status`
/// answer, `None` when it is stopped.
fn readable(installed: &str, running: Option<&Value>) -> String {
    let mut text = String::new();
    let Some(status) = running else {
        let _ = writeln!(text, "service    stopped");
        let _ = writeln!(text, "installed  {installed}");
        return text;
    };
    let service = &status["service"];
    let version = service["version"].as_str();
    let since = service["started_at"].as_str().unwrap_or("an unknown time");
    let _ = writeln!(
        text,
        "service    {}, running since {since}",
        version.unwrap_or("unknown version")
    );
    if version.is_some_and(|v| v != installed) {
        let _ = writeln!(
            text,
            "installed  {installed}, not the service's: restart the service to run it"
        );
    } else {
        let _ = writeln!(text, "installed  {installed}");
    }

    let bricks = items(&status["bricks"]);
    let width = bricks
        .iter()
        .map(|b| str(&b["name"]).len())
        .max()
        .unwrap_or(0);
    section(&mut text, "bricks", bricks.is_empty());
    for brick in bricks {
        let mut line = format!("  {:width$}  {}", str(&brick["name"]), str(&brick["state"]));
        if let Some(pid) = brick["pid"].as_u64() {
            let _ = write!(line, ", pid {pid}");
        }
        match brick["crashes"].as_u64() {
            Some(0) | None => {}
            Some(1) => line.push_str(", 1 crash in a row"),
            Some(n) => {
                let _ = write!(line, ", {n} crashes in a row");
            }
        }
        let _ = writeln!(text, "{line}");
    }

    let sessions = items(&status["sessions"]);
    section(&mut text, "sessions", sessions.is_empty());
    for session in sessions {
        let _ = writeln!(text, "  {}", str(&session["id"]));
        let subscriptions = items(&session["subscriptions"]);
        if subscriptions.is_empty() {
            let _ = writeln!(text, "    no subscription");
        }
        for subscription in subscriptions {
            let (flow, id) = (str(&subscription["flow"]), str(&subscription["id"]));
            let _ = writeln!(text, "    {flow} ({id})");
        }
    }

    let flows = items(&status["flows"]);
    section(&mut text, "flows", flows.is_empty());
    for flow in flows {
        let subscribers = match flow["subscribers"].as_u64() {
            Some(1) => "1 subscriber".to_owned(),
            n => format!("{} subscribers", n.unwrap_or(0)),
        };
        let last = match &flow["last_event"] {
            Value::Null => "nothing published yet".to_owned(),
            event => format!(
                "last event {} at {}",
                str(&event["id"]),
                str(&event["time"])
            ),
        };
        let _ = writeln!(text, "  {}  {subscribers}, {last}", str(&flow["id"]));
    }
    text
}

/// A section's heading, `none` beside it when it is empty.
fn section(text: &mut String, name: &str, empty: bool) {
    let _ = writeln!(text, "{name}{}", if empty { "  none" } else { "" });
}

fn items(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

fn str(value: &Value) -> &str {
    value.as_str().unwrap_or("?")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_socket_held_by_systemd_without_its_service_waits_for_a_connection() {
        assert!(waits_for_a_connection("active\ninactive\n"));
        assert!(waits_for_a_connection("active\nfailed\n"));
        // Stopping once idle: systemd would start it again for the connection.
        assert!(waits_for_a_connection("active\ndeactivating\n"));
        assert!(!waits_for_a_connection("active\nactive\n"));
        assert!(!waits_for_a_connection("active\nactivating\n"));
        // No socket unit: nothing starts the service on a connection.
        assert!(!waits_for_a_connection("inactive\ninactive\n"));
        // systemd could not be asked.
        assert!(!waits_for_a_connection(""));
    }

    #[test]
    fn a_stopped_service_shows_the_installed_version() {
        assert_eq!(
            readable("0.2.0", None),
            "service    stopped\ninstalled  0.2.0\n"
        );
    }

    #[test]
    fn a_running_service_shows_its_state() {
        let status = json!({
            "service": { "version": "0.2.0", "started_at": "2026-11-08T16:41:00Z", "path": null },
            "bricks": [
                { "name": "claude-code", "state": "running", "pid": 868412, "crashes": 0 },
                { "name": "forge", "state": "stopped", "pid": null, "crashes": 0 },
                { "name": "gh", "state": "waiting", "pid": null, "crashes": 2 },
            ],
            "sessions": [
                { "id": "5f1c", "subscriptions": [ { "id": "s-12", "flow": "claude-code/session/5f1c" } ] },
                { "id": "9a0b", "subscriptions": [] },
            ],
            "flows": [
                {
                    "id": "claude-code/session/5f1c",
                    "subscribers": 1,
                    "last_event": { "id": "42", "time": "2026-11-08T16:45:12Z" },
                },
                { "id": "forge/x", "subscribers": 2, "last_event": null },
            ],
        });
        assert_eq!(
            readable("0.2.0", Some(&status)),
            "\
service    0.2.0, running since 2026-11-08T16:41:00Z
installed  0.2.0
bricks
  claude-code  running, pid 868412
  forge        stopped
  gh           waiting, 2 crashes in a row
sessions
  5f1c
    claude-code/session/5f1c (s-12)
  9a0b
    no subscription
flows
  claude-code/session/5f1c  1 subscriber, last event 42 at 2026-11-08T16:45:12Z
  forge/x  2 subscribers, nothing published yet
"
        );
    }

    #[test]
    fn a_service_on_another_version_than_the_installed_one_is_flagged() {
        let status = json!({
            "service": { "version": "0.1.0", "started_at": "2026-11-08T16:41:00Z", "path": null },
            "bricks": [],
            "sessions": [],
            "flows": [],
        });
        assert_eq!(
            readable("0.2.0", Some(&status)),
            "\
service    0.1.0, running since 2026-11-08T16:41:00Z
installed  0.2.0, not the service's: restart the service to run it
bricks  none
sessions  none
flows  none
"
        );
    }
}
