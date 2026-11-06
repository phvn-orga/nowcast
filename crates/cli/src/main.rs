//! `nowcast`, the command-line client of the nowcast service: `watch`, `get`,
//! `refresh`, `subscribe`.
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

use clap::Parser;

use crate::args::{Cli, Command};
use crate::client::Client;
use crate::error::CliError;

fn main() -> ExitCode {
    let cli = Cli::parse();
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

async fn run(cli: Cli) -> Result<(), CliError> {
    let socket = config::socket_path(cli.socket, std::env::var_os("XDG_RUNTIME_DIR"))?;
    let session = config::session_id(cli.session, std::process::id())?;
    let client = Client::new(socket, session);
    let mut out = io::stdout().lock();
    let mut err = io::stderr();
    match cli.command {
        Command::Get { flow } => commands::get(&client, &flow, cli.json, &mut out, &mut err).await,
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
    }
}
