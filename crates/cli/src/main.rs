//! `nowcast`, the command-line client of the nowcast service: `watch`, `get`,
//! `refresh`, `subscribe`. `nowcast daemon` runs the service itself, from the
//! `nowcast-daemon` library: one binary to install, at one version.
//!
//! Every command declares a session (`PUT /v1/session`) before any other call
//! to the client API. The service attaches the tool to its anchor process: an
//! agent when the tool runs from one of its hooks, the tool itself when it
//! runs from a plain shell, in which case its subscriptions end with it.

mod args;
mod client;
mod commands;
mod config;
mod error;
mod output;
mod sse;

use std::io;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{CommandFactory, FromArgMatches};

use crate::args::{Cli, Command};
use crate::client::Client;
use crate::error::CliError;

fn main() -> ExitCode {
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    if let Command::Daemon = cli.command {
        return daemon(&matches);
    }
    let json = cli.json;
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(CliError::Io)
        .and_then(|runtime| runtime.block_on(run(cli)));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error.report(json);
            ExitCode::from(error.exit_code())
        }
    }
}

/// Runs the service, before anything else starts: the daemon builds its own
/// runtime and logging, and takes its socket while the process has one thread.
fn daemon(matches: &clap::ArgMatches) -> ExitCode {
    let refused = args::client_options_given(matches);
    if !refused.is_empty() {
        let message = format!("{} cannot be used with daemon", refused.join(", "));
        Cli::command()
            .error(ErrorKind::ArgumentConflict, message)
            .exit();
    }
    nowcast_daemon::run()
}

async fn run(cli: Cli) -> Result<(), CliError> {
    let socket = config::socket_path(cli.socket, std::env::var_os("XDG_RUNTIME_DIR"))?;
    let session = config::session_id(cli.session, std::process::id())?;
    let client = Client::new(socket, session);
    let mut out = io::stdout().lock();
    let mut err = io::stderr();
    match cli.command {
        Command::Get { flow, after } => {
            commands::get(&client, &flow, after, cli.json, &mut out, &mut err).await
        }
        Command::Refresh { flow } => commands::refresh(&client, &flow).await,
        Command::Subscribe {
            brick,
            kind,
            params,
        } => {
            let params = params.as_deref().unwrap_or("{}");
            commands::subscribe(&client, &brick, &kind, params, cli.json, &mut out).await
        }
        Command::Watch {
            subscribe,
            last_event_id,
        } => {
            let resume = last_event_id.as_deref();
            commands::watch(&client, &subscribe, resume, cli.json, &mut out, &mut err).await
        }
        Command::Daemon => unreachable!("main runs the daemon before any client command"),
    }
}
