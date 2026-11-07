//! The claude-code brick, native to nowcast: the quotas of the Claude Code
//! account and the measures of its running sessions. The `nowcast` binary
//! runs it as `nowcast brick claude-code`; its declaration and schemas are
//! [`FILES`].
//!
//! - `quotas`: every window of the account, the freshest reading of the usage
//!   API (read every 60 s while the flow has a subscriber, never twice within
//!   15 s, backing off on `429`), of Claude Code's copy of its answer, and of
//!   the sessions' last responses.
//! - `session {id}`: a session's own flow. It sends `claude-code.measure/v1`
//!   there and reads back its entry. The flow's release ends the session.
//! - `sessions`, `sessions {id}`: the running sessions, or one.
//!
//! The token is read from `$HOME/.claude/.credentials.json` and never
//! refreshed; the copy from `$HOME/.claude.json`. [`usage::API_VAR`] replaces
//! the API's address, for tests.

mod pacer;
mod quotas;
mod state;
mod usage;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::SystemTime;

use jiff::Timestamp;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use nowcast_codec::{read_service, write_brick};
use nowcast_types::{BrickMessage, ServiceMessage};
use tokio::io::{Stdout, stdin, stdout};
use tokio::sync::mpsc::unbounded_channel;
use tokio::time::Instant;

use crate::pacer::Pacer;
use crate::quotas::ApiStatus;
use crate::state::State;
use crate::usage::{Fetched, Paths, Token};

/// The brick's name, as its declaration gives it.
pub const NAME: &str = "claude-code";

/// The declaration and its schemas, by path relative to the declaration's
/// directory.
pub const FILES: &[(&str, &str)] = &[
    ("brick.toml", include_str!("../brick.toml")),
    (
        "schemas/params-quotas.json",
        include_str!("../schemas/params-quotas.json"),
    ),
    (
        "schemas/params-session.json",
        include_str!("../schemas/params-session.json"),
    ),
    (
        "schemas/params-sessions.json",
        include_str!("../schemas/params-sessions.json"),
    ),
    (
        "schemas/quotas-v1.json",
        include_str!("../schemas/quotas-v1.json"),
    ),
    (
        "schemas/session-v1.json",
        include_str!("../schemas/session-v1.json"),
    ),
    (
        "schemas/sessions-v1.json",
        include_str!("../schemas/sessions-v1.json"),
    ),
    (
        "schemas/measure-v1.json",
        include_str!("../schemas/measure-v1.json"),
    ),
];

/// Runs the brick on standard input and output until `shutdown` or the end
/// of its input. Standard error is its log.
pub fn run() -> ExitCode {
    // The one TLS provider the binary carries.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            log(format_args!("cannot start: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let status = runtime.block_on(serve());
    // Standard input is read on a blocking thread that may sit in a read.
    runtime.shutdown_background();
    status
}

fn log(message: impl std::fmt::Display) {
    eprintln!("{NAME}: {message}");
}

/// The quotas' reading of the usage API, made at the first subscription and
/// kept after: its pace and a reading in flight outlive a flow released and
/// subscribed again. It reads only while the flow has a subscriber.
struct Reader {
    pacer: Pacer,
    in_flight: bool,
    copy_seen: Option<SystemTime>,
    /// Follows Claude Code's directory, to read again as soon as the token
    /// changes. `None` when it cannot be watched: the next reading then
    /// waits for its turn.
    _watcher: Option<RecommendedWatcher>,
}

async fn serve() -> ExitCode {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        log("HOME is not set");
        return ExitCode::FAILURE;
    };
    let paths = Paths::under(&home);
    let api = std::env::var(usage::API_VAR).unwrap_or_else(|_| usage::API.to_owned());
    let client = match usage::client() {
        Ok(client) => client,
        Err(e) => {
            log(format_args!("cannot build the HTTP client: {e}"));
            return ExitCode::FAILURE;
        }
    };

    let (from_service_tx, mut from_service) = unbounded_channel();
    tokio::spawn(async move {
        let mut input = stdin();
        loop {
            match read_service(&mut input).await {
                Ok(Some(msg)) => {
                    if from_service_tx.send(msg).is_err() {
                        return;
                    }
                }
                Ok(None) => return,
                Err(e) => {
                    log(format_args!("unreadable input: {e}"));
                    return;
                }
            }
        }
    });
    let mut output = stdout();
    let (fetched_tx, mut fetched) = unbounded_channel::<Fetched>();
    let (changed_tx, mut changed) = unbounded_channel::<()>();
    let mut state = State::default();
    let mut reader: Option<Reader> = None;

    loop {
        let due = reader
            .as_ref()
            .filter(|r| !r.in_flight && state.wants_quotas())
            .map(|r| r.pacer.next());
        tokio::select! {
            msg = from_service.recv() => {
                let Some(msg) = msg else { return ExitCode::SUCCESS };
                if msg == ServiceMessage::Shutdown {
                    return ExitCode::SUCCESS;
                }
                let refresh_quotas = matches!(&msg, ServiceMessage::Refresh { flow } if flow.as_str() == "quotas");
                let out = state.on_service(msg, Timestamp::now());
                if !send(&mut output, out).await {
                    return ExitCode::FAILURE;
                }
                match (&mut reader, state.wants_quotas()) {
                    (None, true) => {
                        reader = Some(Reader {
                            pacer: Pacer::new(Instant::now()),
                            in_flight: false,
                            copy_seen: None,
                            _watcher: watch(&paths, changed_tx.clone()),
                        });
                    }
                    (Some(r), true) if refresh_quotas => r.pacer.hurry(Instant::now()),
                    _ => {}
                }
            }
            () = sleep_until(due) => {
                let Some(r) = reader.as_mut() else { continue };
                let now = Timestamp::now();
                r.pacer.started(Instant::now());
                let mut out = Vec::new();
                if state.quotas().status() != ApiStatus::Ok
                    && let Some((answer, fetched_at)) = usage::read_copy(&paths.claude_json, &mut r.copy_seen)
                {
                    out.extend(state.on_copy(&answer, fetched_at));
                }
                match usage::read_token(&paths.credentials, now) {
                    Token::Valid(token) => {
                        r.in_flight = true;
                        let (client, api, tx) = (client.clone(), api.clone(), fetched_tx.clone());
                        tokio::spawn(async move {
                            let _ = tx.send(usage::fetch(&client, &api, &token).await);
                        });
                    }
                    Token::Expired => out.extend(state.on_status(ApiStatus::TokenExpired, now)),
                    Token::Missing => out.extend(state.on_status(ApiStatus::NoCredentials, now)),
                    Token::Unreadable => out.extend(state.on_status(ApiStatus::Unreadable, now)),
                }
                if !send(&mut output, out).await {
                    return ExitCode::FAILURE;
                }
            }
            Some(result) = fetched.recv() => {
                let now = Timestamp::now();
                let Some(r) = reader.as_mut() else { continue };
                r.in_flight = false;
                let out = match result {
                    Fetched::Answer(answer) => {
                        r.pacer.succeeded(Instant::now());
                        state.on_answer(&answer, now, now)
                    }
                    Fetched::Unauthorized => state.on_status(ApiStatus::TokenExpired, now),
                    Fetched::Limited(retry) => {
                        r.pacer.limited(Instant::now(), retry);
                        state.on_status(ApiStatus::RateLimited, now)
                    }
                    Fetched::Unreadable(why) => {
                        log(format_args!("usage API: {why}"));
                        state.on_status(ApiStatus::Unreadable, now)
                    }
                    Fetched::Failed(why) => {
                        // Unreachable is not a verdict on the source: the
                        // status stays, the next reading comes in its turn.
                        log(format_args!("usage API unreachable: {why}"));
                        Vec::new()
                    }
                };
                if !send(&mut output, out).await {
                    return ExitCode::FAILURE;
                }
            }
            Some(()) = changed.recv() => {
                if let Some(r) = reader.as_mut()
                    && state.quotas().status() != ApiStatus::Ok
                {
                    r.pacer.hurry(Instant::now());
                }
            }
        }
    }
}

/// Writes the brick's messages. `false` once the service is gone.
async fn send(output: &mut Stdout, messages: Vec<BrickMessage>) -> bool {
    for msg in messages {
        if let Err(e) = write_brick(output, &msg).await {
            log(format_args!("cannot write to the service: {e}"));
            return false;
        }
    }
    true
}

async fn sleep_until(due: Option<Instant>) {
    match due {
        Some(due) => tokio::time::sleep_until(due).await,
        None => std::future::pending().await,
    }
}

/// Follows the directory of Claude Code's token: Claude Code replaces the
/// file when it refreshes the token. Every change to it sends on `changed`.
fn watch(
    paths: &Paths,
    changed: tokio::sync::mpsc::UnboundedSender<()>,
) -> Option<RecommendedWatcher> {
    let file = paths.credentials.file_name()?.to_owned();
    let dir = paths.credentials.parent()?.to_owned();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && event.paths.iter().any(|p| p.file_name() == Some(&file))
        {
            let _ = changed.send(());
        }
    })
    .inspect_err(|e| log(format_args!("cannot follow the token: {e}")))
    .ok()?;
    watcher
        .watch(&dir, RecursiveMode::NonRecursive)
        .inspect_err(|e| log(format_args!("cannot follow {}: {e}", dir.display())))
        .ok()?;
    Some(watcher)
}
