//! `nowcast reload`: restarts the service systemd runs, so that it runs the
//! installed binary, and prints its version before and after.
//!
//! The clients take back what a restart loses (the session, the
//! subscriptions) as after an idle stop; systemd holds the socket meanwhile.

use std::io::Write;
use std::time::Duration;

use serde_json::json;

use crate::client::Client;
use crate::commands::setup::{SERVICE, Units, systemctl, units_state};
use crate::commands::status::{systemd_states, waits_for_a_connection};
use crate::error::CliError;

/// How long the running service has to tell its version before the restart:
/// one that no longer answers is what a restart is for.
const BEFORE_TIMEOUT: Duration = Duration::from_secs(1);

const UNKNOWN: &str = "an unknown version";

pub async fn reload(
    client: &Client,
    socket_given: bool,
    json: bool,
    out: &mut impl Write,
) -> Result<(), CliError> {
    if socket_given {
        return Err(CliError::Usage(
            "reload restarts the service systemd runs, not one on a socket given \
             (--socket, NOWCAST_SOCKET)"
                .into(),
        ));
    }
    match units_state() {
        Some(Units::Current) => {}
        Some(Units::Outdated) => {
            return Err(CliError::Setup(
                "the systemd units are outdated: run nowcast setup, then nowcast reload".into(),
            ));
        }
        Some(Units::Missing) | None => {
            return Err(CliError::Setup(
                "systemd does not run the service: run nowcast setup".into(),
            ));
        }
    }
    if waits_for_a_connection(&systemd_states()) {
        if json {
            writeln!(out, "{}", json!({ "before": null, "after": null }))?;
        } else {
            writeln!(
                out,
                "service stopped: the next connection starts the installed version"
            )?;
        }
        return Ok(());
    }
    let before = tokio::time::timeout(BEFORE_TIMEOUT, client.service_version())
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    systemctl(&["restart", SERVICE])?;
    let after = client
        .service_version()
        .await
        .map_err(|e| restarted(e, before.as_deref()))?;
    if json {
        writeln!(out, "{}", json!({ "before": before, "after": after }))?;
    } else {
        writeln!(
            out,
            "service restarted: {} → {}",
            before.as_deref().unwrap_or(UNKNOWN),
            after.as_deref().unwrap_or(UNKNOWN)
        )?;
    }
    Ok(())
}

/// `error`, from the service after systemd restarted it, saying so and which
/// version ran `before`.
fn restarted(error: CliError, before: Option<&str>) -> CliError {
    let note = format!(
        "systemd restarted the service, which ran {}",
        before.unwrap_or(UNKNOWN)
    );
    match error {
        CliError::Unreachable { socket, reason } => CliError::Unreachable {
            socket,
            reason: format!("{reason}; {note}"),
        },
        CliError::Api { code, message } => CliError::Api {
            code,
            message: format!("{message}; {note}"),
        },
        CliError::Protocol(message) => CliError::Protocol(format!("{message}; {note}")),
        other => other,
    }
}
