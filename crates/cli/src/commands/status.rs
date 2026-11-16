//! `nowcast status`: what runs, and at which version.

use std::fmt::Write as _;
use std::io::Write;
use std::process::Command;

use console::Style;
use jiff::Timestamp;
use serde_json::{Value, json};

use crate::client::Client;
use crate::commands::setup::{Units, units_state};
use crate::display::{self, Health};
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
    let setup = units_state();
    if json {
        let setup = setup.map(Units::as_str);
        writeln!(
            out,
            "{}",
            json!({ "installed": INSTALLED, "running": running, "setup": setup })
        )?;
    } else {
        let colors = console::colors_enabled();
        let text = readable(
            INSTALLED,
            running.as_ref(),
            setup,
            ask_systemd,
            Timestamp::now(),
            colors,
        );
        out.write_all(text.as_bytes())?;
    }
    Ok(())
}

/// What `systemctl --user is-active` says of the socket and the service, one
/// line each; empty when systemd cannot be asked.
pub(crate) fn systemd_states() -> String {
    Command::new("systemctl")
        .args(["--user", "is-active", "nowcast.socket", "nowcast.service"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default()
}

/// Whether systemd holds the socket while the service does not run, or is
/// stopping: a connection would start it.
pub(crate) fn waits_for_a_connection(states: &str) -> bool {
    let mut lines = states.lines();
    let (socket, service) = (lines.next(), lines.next());
    socket == Some("active") && matches!(service, Some("inactive" | "failed" | "deactivating"))
}

/// The status for a human, `running` being the service's `GET /v1/status`
/// answer, `None` when it is stopped, and `setup` the state of the units,
/// shown when `nowcast setup` has to run. `systemd` says whether the socket
/// is the one systemd holds, which `nowcast reload` restarts. Times read
/// relative to `now`; `colors` colours the bricks' states.
fn readable(
    installed: &str,
    running: Option<&Value>,
    setup: Option<Units>,
    systemd: bool,
    now: Timestamp,
    colors: bool,
) -> String {
    let mut text = String::new();
    let Some(status) = running else {
        let _ = writeln!(text, "service    stopped");
        let _ = writeln!(text, "installed  {installed}");
        setup_line(&mut text, setup);
        return text;
    };
    let service = &status["service"];
    let version = service["version"].as_str();
    let up = service["started_at"]
        .as_str()
        .and_then(|time| display::since(time, now))
        .map_or_else(
            || "up for an unknown time".to_owned(),
            |d| format!("up {}", display::duration(d)),
        );
    let _ = writeln!(
        text,
        "service    {}, {up}",
        version.unwrap_or("unknown version")
    );
    if version.is_some_and(|v| v != installed) {
        // What `nowcast reload` would refuse is not offered.
        let how = match setup {
            Some(Units::Current) if systemd => "run nowcast reload to run it",
            Some(Units::Outdated) if systemd => "run nowcast setup, then nowcast reload",
            _ => "restart the service to run it",
        };
        let _ = writeln!(text, "installed  {installed}, not the service's: {how}");
    } else {
        let _ = writeln!(text, "installed  {installed}");
    }
    setup_line(&mut text, setup);

    let bricks = items(&status["bricks"]);
    section(&mut text, "bricks", bricks.is_empty());
    let width = bricks
        .iter()
        .map(|b| str(&b["name"]).len())
        .max()
        .unwrap_or(0);
    let state_width = bricks
        .iter()
        .map(|b| str(&b["state"]).len())
        .max()
        .unwrap_or(0);
    for brick in bricks {
        let state = str(&brick["state"]);
        let style = health_style(Health::of(state), colors);
        let mut details = Vec::new();
        if let Some(pid) = brick["pid"].as_u64() {
            details.push(format!("pid {pid}"));
        }
        match brick["crashes"].as_u64() {
            Some(0) | None => {}
            Some(1) => details.push("1 crash in a row".to_owned()),
            Some(n) => details.push(format!("{n} crashes in a row")),
        }
        let line = format!(
            "  {} {:width$}  {}  {}",
            style.apply_to(Health::of(state).symbol()),
            str(&brick["name"]),
            style.apply_to(format!("{state:state_width$}")),
            details.join(", "),
        );
        let _ = writeln!(text, "{}", line.trim_end());
    }

    let flows: Vec<&Value> = items(&status["flows"]).iter().collect();
    let flow = |id: &str| flows.iter().find(|f| str(&f["id"]) == id).copied();
    let sessions = items(&status["sessions"]);
    section(&mut text, "sessions", sessions.is_empty());
    let mut shown = Vec::new();
    for session in sessions {
        let id = str(&session["id"]);
        let _ = writeln!(text, "  {}", display::short_session(id));
        let subscribed: Vec<&str> = items(&session["subscriptions"])
            .iter()
            .map(|s| str(&s["flow"]))
            .collect();
        if subscribed.is_empty() {
            let _ = writeln!(text, "    no subscription");
        }
        let names: Vec<String> = subscribed
            .iter()
            .map(|f| display::flow_in_session(f, id))
            .collect();
        let width = names.iter().map(|n| n.chars().count()).max().unwrap_or(0);
        for (id, name) in subscribed.iter().zip(&names) {
            let _ = writeln!(text, "    {name:width$}  {}", flow_line(flow(id), now));
            shown.push(*id);
        }
    }
    // A flow no listed session holds: an anchor that died meanwhile.
    let others: Vec<&&Value> = flows
        .iter()
        .filter(|f| !shown.contains(&str(&f["id"])))
        .collect();
    if !others.is_empty() {
        section(&mut text, "other flows", false);
        let width = others
            .iter()
            .map(|f| str(&f["id"]).chars().count())
            .max()
            .unwrap_or(0);
        for f in others {
            let _ = writeln!(
                text,
                "  {:width$}  {}",
                str(&f["id"]),
                flow_line(Some(f), now)
            );
        }
    }
    text
}

/// A flow's subscribers and last event, as `status` lists it.
fn flow_line(flow: Option<&Value>, now: Timestamp) -> String {
    let Some(flow) = flow else {
        return "not published yet".to_owned();
    };
    let subscribers = match flow["subscribers"].as_u64() {
        Some(1) => "1 subscriber".to_owned(),
        n => format!("{} subscribers", n.unwrap_or(0)),
    };
    let last = match flow["last_event"]["time"].as_str() {
        Some(time) => format!("last event {}", display::ago(time, now)),
        None => "nothing published yet".to_owned(),
    };
    format!("{subscribers}, {last}")
}

/// The colour of a brick's state, none unless `colors`.
fn health_style(health: Health, colors: bool) -> Style {
    let style = match health {
        Health::Up => Style::new().green(),
        Health::Moving => Style::new().yellow(),
        Health::Idle => Style::new().dim(),
        Health::Failing => Style::new().red(),
    };
    style.force_styling(colors)
}

/// The line saying to run `nowcast setup`, when the units need it.
fn setup_line(text: &mut String, setup: Option<Units>) {
    let why = match setup {
        Some(Units::Missing) => "missing: run nowcast setup to let systemd start the service",
        Some(Units::Outdated) => "outdated: run nowcast setup again",
        Some(Units::Current) | None => return,
    };
    let _ = writeln!(text, "setup      {why}");
}

/// A section's heading after a blank line, `none` beside it when it is
/// empty.
fn section(text: &mut String, name: &str, empty: bool) {
    let _ = writeln!(text, "\n{name}{}", if empty { "  none" } else { "" });
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

    const NOW: &str = "2026-11-08T16:45:12Z";

    /// [`readable`] at [`NOW`], without colours.
    fn readable(
        installed: &str,
        running: Option<&Value>,
        setup: Option<Units>,
        systemd: bool,
    ) -> String {
        super::readable(
            installed,
            running,
            setup,
            systemd,
            NOW.parse().unwrap(),
            false,
        )
    }

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
            readable("0.2.0", None, Some(Units::Current), true),
            "service    stopped\ninstalled  0.2.0\n"
        );
        assert_eq!(
            readable("0.2.0", None, Some(Units::Missing), true),
            "service    stopped\ninstalled  0.2.0\n\
             setup      missing: run nowcast setup to let systemd start the service\n"
        );
    }

    #[test]
    fn a_running_service_shows_its_state() {
        let session = "5f1c2e9a-0b1c-4d2e-8f3a-4b5c6d7e8f90";
        let own = format!("claude-code/session/{session}");
        let status = json!({
            "service": { "version": "0.2.0", "started_at": "2026-11-08T14:41:00Z", "path": null },
            "bricks": [
                { "name": "claude-code", "state": "running", "pid": 868412, "crashes": 0 },
                { "name": "forge", "state": "stopped", "pid": null, "crashes": 0 },
                { "name": "gh", "state": "waiting", "pid": null, "crashes": 2 },
            ],
            "sessions": [
                { "id": session, "subscriptions": [
                    { "id": "s-12", "flow": own },
                    { "id": "s-13", "flow": "forge/x" },
                ] },
                { "id": "9a0b", "subscriptions": [] },
            ],
            "flows": [
                {
                    "id": own,
                    "subscribers": 1,
                    "last_event": { "id": "42", "time": "2026-11-08T16:45:09Z" },
                },
                { "id": "forge/x", "subscribers": 2, "last_event": null },
                { "id": "forge/orphan", "subscribers": 1, "last_event": null },
            ],
        });
        assert_eq!(
            readable("0.2.0", Some(&status), None, true),
            "\
service    0.2.0, up 2 h 04 min
installed  0.2.0

bricks
  ● claude-code  running  pid 868412
  ○ forge        stopped
  ▲ gh           waiting  2 crashes in a row

sessions
  5f1c2e9a
    claude-code/session/…  1 subscriber, last event 3 s ago
    forge/x                2 subscribers, nothing published yet
  9a0b
    no subscription

other flows
  forge/orphan  1 subscriber, nothing published yet
"
        );
    }

    #[test]
    fn colours_show_only_when_asked() {
        let status = json!({
            "service": { "version": "0.2.0", "started_at": NOW, "path": null },
            "bricks": [ { "name": "b", "state": "running", "pid": 1, "crashes": 0 } ],
            "sessions": [],
            "flows": [],
        });
        let plain = readable("0.2.0", Some(&status), None, true);
        assert!(!plain.contains('\x1b'), "{plain}");
        let now = NOW.parse().unwrap();
        let coloured = super::readable("0.2.0", Some(&status), None, true, now, true);
        assert!(coloured.contains("\x1b[32m"), "{coloured:?}");
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
            readable("0.2.0", Some(&status), Some(Units::Outdated), true),
            "\
service    0.1.0, up 4 min
installed  0.2.0, not the service's: run nowcast setup, then nowcast reload
setup      outdated: run nowcast setup again

bricks  none

sessions  none
"
        );
        let second = |text: String| text.lines().nth(1).unwrap().to_owned();
        assert_eq!(
            second(readable("0.2.0", Some(&status), Some(Units::Current), true)),
            "installed  0.2.0, not the service's: run nowcast reload to run it"
        );
        // Where reload would refuse: without the units, or on a socket given.
        let restart = "installed  0.2.0, not the service's: restart the service to run it";
        assert_eq!(
            second(readable("0.2.0", Some(&status), Some(Units::Missing), true)),
            restart
        );
        assert_eq!(
            second(readable(
                "0.2.0",
                Some(&status),
                Some(Units::Current),
                false
            )),
            restart
        );
    }
}
