//! `nowcast reload`: restarts the service systemd runs, so that it runs the
//! installed binary, and prints its version before and after.
//!
//! The clients take back what a restart loses (the session, the
//! subscriptions) as after an idle stop; systemd holds the socket meanwhile.

use std::io::Write;

use serde_json::json;

use crate::client::Client;
use crate::commands::setup::{SERVICE, Units, systemctl, units_state};
use crate::commands::status::{systemd_states, waits_for_a_connection};
use crate::error::CliError;

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
    // A service that does not answer is what a restart is for.
    let before = client.service_version().await.ok().flatten();
    systemctl(&["restart", SERVICE])?;
    let after = client.service_version().await?;
    if json {
        writeln!(out, "{}", json!({ "before": before, "after": after }))?;
    } else {
        let name = |v: &Option<String>| v.clone().unwrap_or_else(|| "an unknown version".into());
        writeln!(
            out,
            "service restarted: {} → {}",
            name(&before),
            name(&after)
        )?;
    }
    Ok(())
}
