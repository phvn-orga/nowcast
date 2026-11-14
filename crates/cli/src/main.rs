//! `nowcast`, the command-line client of the nowcast service: `watch`, `get`,
//! `refresh`, `input`, `subscribe`, `status`, `reload`, which restarts the
//! service, and `setup`, which installs the service's systemd units.
//! `nowcast daemon` runs the service itself, from the `nowcast-daemon`
//! library: one binary to install, at one version.
//!
//! Every command but `status` and `reload` declares a session (`PUT /v1/session`) before
//! any other call to the client API. The service attaches the tool to its anchor process: an
//! agent when the tool runs from one of its hooks, the tool itself when it
//! runs from a plain shell, in which case its subscriptions end with it.

mod args;
mod client;
mod commands;
mod config;
mod error;
mod native;
mod output;
mod sse;

use std::io;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{CommandFactory, FromArgMatches};

use crate::args::{BricksAction, Cli, Command};
use crate::client::Client;
use crate::error::CliError;

fn main() -> ExitCode {
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    match &cli.command {
        Command::Daemon => return daemon(&matches),
        // A brick runs on its own runtime, and talks to the service alone.
        Command::Brick { name, action } => return brick(name, action.is_some()),
        Command::Bricks { action } => {
            let result = match action {
                BricksAction::Enable { brick, force } => {
                    let service = (!force).then(|| client(&cli));
                    commands::bricks::enable(brick, *force, service)
                }
                BricksAction::Disable { brick } => commands::bricks::disable(brick),
                BricksAction::Configure { brick } => commands::bricks::configure(brick),
            };
            return exit_code(result, cli.json);
        }
        Command::Setup { bricks, remove } => {
            let result = if *remove {
                commands::setup::remove()
            } else {
                commands::setup::setup(bricks, &|| client(&cli))
            };
            return exit_code(result, cli.json);
        }
        _ => {}
    }
    let json = cli.json;
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(CliError::Io)
        .and_then(|runtime| runtime.block_on(run(cli)));
    exit_code(result, json)
}

/// The exit code of a command's `result`, its error reported first.
fn exit_code(result: Result<(), CliError>, json: bool) -> ExitCode {
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
    nowcast_daemon::run(&native::files())
}

/// Runs a native brick, as the service launches it, or its configuration.
fn brick(name: &str, configure: bool) -> ExitCode {
    let Some(native) = native::find(name) else {
        eprintln!("nowcast: no native brick {name:?}");
        return ExitCode::from(error::exit::USAGE);
    };
    if !configure {
        return (native.run)();
    }
    match native.configure {
        Some(configure) => configure(),
        None => {
            eprintln!("nowcast: the native brick {name} has nothing to configure");
            ExitCode::from(error::exit::USAGE)
        }
    }
}

/// A client of the service the options and environment designate.
fn client(cli: &Cli) -> Result<Client, CliError> {
    let socket = config::socket_path(cli.socket.clone(), std::env::var_os("XDG_RUNTIME_DIR"))?;
    let session = config::session(cli.session.clone(), std::process::id())?;
    Ok(Client::new(socket, session))
}

async fn run(cli: Cli) -> Result<(), CliError> {
    let client = client(&cli)?;
    let mut out = io::stdout().lock();
    let mut err = io::stderr();
    match cli.command {
        Command::Get { flow, after } => {
            commands::get(&client, &flow, after, cli.json, &mut out, &mut err).await
        }
        Command::Refresh { flow, brick, kind } => match (flow, brick) {
            (Some(flow), _) => commands::refresh(&client, &flow).await,
            (None, Some(brick)) => commands::refresh_brick(&client, &brick, kind.as_deref()).await,
            (None, None) => unreachable!("clap requires a flow or a brick"),
        },
        Command::Status => {
            // A socket given explicitly is not the one systemd holds.
            let ask_systemd = cli.socket.is_none();
            commands::status(&client, ask_systemd, cli.json, &mut out).await
        }
        Command::Reload => {
            let socket_given = cli.socket.is_some();
            commands::reload(&client, socket_given, cli.json, &mut out).await
        }
        Command::Input { flow } => commands::input(&client, &flow, io::stdin().lock()).await,
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
        Command::Daemon
        | Command::Brick { .. }
        | Command::Bricks { .. }
        | Command::Setup { .. } => {
            unreachable!("main runs these before any client command")
        }
    }
}
