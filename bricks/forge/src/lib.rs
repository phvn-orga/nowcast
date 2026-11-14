//! The forge brick, native to nowcast: a branch's PR and its pipelines, and
//! a repository's issues, whatever the forge. The `nowcast` binary runs it
//! as `nowcast brick forge`, and its configuration, which tells where each
//! forge's token comes from, as `nowcast brick forge configure`; its
//! declaration and schemas are [`FILES`].
//!
//! The brick is organised by usages, not by forges:
//!
//! - [`registry`] is the core: the providers, the flows and their
//!   publication;
//! - [`provider`] is what a forge implements, and [`model`] the neutral
//!   output every provider maps its forge onto;
//! - [`tools`] holds what a provider may use (conditional HTTP, a pace
//!   machine, remote parsing), and nothing obliges it to;
//! - [`github`] is the GitHub provider, [`azure`] the Azure DevOps one;
//! - [`credentials`] reads the token source each forge's user chose, which
//!   [`configure()`] asks for.
//!
//! Standard error is the brick's log, which the service journals: errors
//! only, unless [`LOG_VAR`] is `debug` in the service's environment. Then
//! each line `forge: debug: ...` tells one step, to follow a refresh that
//! changes nothing:
//!
//! - `refresh <flow> target=<target>`, a flow's refresh, and
//!   `refresh-scope repo=<repo> types=<kinds> number=<n> targets=<targets>`,
//!   a scoped one, with the targets it covers;
//! - `poll <detector> answer=<answer> moved=<items> woke=<targets>`, a
//!   detector's poll: `first`, `changed`, `unchanged` (a `304` on GitHub),
//!   `no-token` or `failed:<why>`;
//! - `read <target> outcome=<ok or failure> published=<kinds>` or
//!   `unchanged`, a target's reading.
//!
//! A target reads as `branch/<repo>#<branch>`, `pr/<repo>!<number>`,
//! `issues/<tracker>` or `issue/<tracker>#<number>`. No line carries a
//! remote as written, which may hold credentials, nor a token.

/// Writes a debug line to the log, only when [`LOG_VAR`] asks for it; the
/// arguments are not even formatted otherwise.
macro_rules! debug {
    ($($arg:tt)*) => {
        if $crate::debug_on() {
            $crate::log(format_args!("debug: {}", format_args!($($arg)*)));
        }
    };
}

mod azure;
mod configure;
mod credentials;
mod github;
mod model;
mod provider;
mod registry;
#[cfg(test)]
mod testing;
mod tools;

use std::process::ExitCode;
use std::sync::LazyLock;

use jiff::Timestamp;
use nowcast_codec::{read_service, write_brick};
use nowcast_types::{BrickMessage, ServiceMessage};
use tokio::io::{Stdout, stdin, stdout};
use tokio::sync::mpsc::unbounded_channel;
use tokio::time::Instant;

use crate::azure::Azure;
use crate::credentials::Credentials;
use crate::github::GitHub;
use crate::provider::Provider;
use crate::registry::Core;
use crate::tools::http::Http;

/// The brick's name, as its declaration gives it.
pub const NAME: &str = "forge";

/// The declaration and its schemas, by path relative to the declaration's
/// directory.
pub const FILES: &[(&str, &str)] = &[
    (
        "brick.toml",
        include_str!(concat!(env!("OUT_DIR"), "/brick.toml")),
    ),
    (
        "schemas/params-pr.json",
        include_str!("../schemas/params-pr.json"),
    ),
    ("schemas/pr-v1.json", include_str!("../schemas/pr-v1.json")),
    (
        "schemas/pipelines-v1.json",
        include_str!("../schemas/pipelines-v1.json"),
    ),
    (
        "schemas/params-issues.json",
        include_str!("../schemas/params-issues.json"),
    ),
    (
        "schemas/params-issue.json",
        include_str!("../schemas/params-issue.json"),
    ),
    (
        "schemas/issues-v1.json",
        include_str!("../schemas/issues-v1.json"),
    ),
    (
        "schemas/issue-v1.json",
        include_str!("../schemas/issue-v1.json"),
    ),
    ("schemas/scope.json", include_str!("../schemas/scope.json")),
];

/// Runs the brick on standard input and output until `shutdown` or the end
/// of its input. Standard error is its log.
pub fn run() -> ExitCode {
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

/// Asks, in the user's terminal, where each forge's token comes from, as
/// `nowcast brick forge configure`. Fails when cancelled or not saved.
pub fn configure() -> ExitCode {
    configure::run()
}

fn log(message: impl std::fmt::Display) {
    eprintln!("{NAME}: {message}");
}

/// `items` joined by commas, `none` when there is none, for the log.
fn list(items: impl Iterator<Item = impl std::fmt::Display>) -> String {
    let list = items.map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    if list.is_empty() { "none".into() } else { list }
}

/// The environment variable that turns the debug lines on, when `debug`.
pub const LOG_VAR: &str = "NOWCAST_FORGE_LOG";

static DEBUG: LazyLock<bool> =
    LazyLock::new(|| debug_asked(std::env::var(LOG_VAR).ok().as_deref()));

/// Whether the debug lines are written.
fn debug_on() -> bool {
    *DEBUG
}

/// Whether `value`, that of [`LOG_VAR`], asks for the debug lines.
fn debug_asked(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim().eq_ignore_ascii_case("debug"))
}

/// The providers the brick carries, each reading the token the user chose
/// for its forge.
fn providers(http: &Http) -> Vec<Box<dyn Provider>> {
    let api = |var, default: &str| std::env::var(var).unwrap_or_else(|_| default.to_owned());
    let credentials = Credentials::system();
    vec![
        Box::new(GitHub::new(
            http.clone(),
            api(github::API_VAR, github::API),
            credentials.clone(),
        )),
        Box::new(Azure::new(
            http.clone(),
            api(azure::API_VAR, azure::API),
            credentials,
        )),
    ]
}

async fn serve() -> ExitCode {
    let http = match Http::new() {
        Ok(http) => http,
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
    let (updates_tx, mut updates) = unbounded_channel();
    let mut core = Core::new(providers(&http), updates_tx);

    loop {
        let out = tokio::select! {
            msg = from_service.recv() => {
                let Some(msg) = msg else { return ExitCode::SUCCESS };
                match msg {
                    ServiceMessage::Shutdown => return ExitCode::SUCCESS,
                    ServiceMessage::Hello { .. } => vec![BrickMessage::Ready],
                    ServiceMessage::Subscribe { req, kind, params } => core.subscribe(req, &kind, &params),
                    ServiceMessage::Release { flow } => {
                        core.release(&flow);
                        vec![]
                    }
                    ServiceMessage::Refresh { flow } => {
                        core.refresh(&flow);
                        vec![]
                    }
                    ServiceMessage::Error { req, code } => {
                        log(format_args!("publication {req} refused: {code:?}"));
                        vec![]
                    }
                    ServiceMessage::RefreshScope { scope } => {
                        core.refresh_scope(&scope);
                        vec![]
                    }
                    ServiceMessage::Input { .. }
                    | ServiceMessage::Ack { .. } => vec![],
                }
            }
            Some((target, outcome)) = updates.recv() => core.update(target, outcome, Timestamp::now(), Instant::now()),
        };
        if !send(&mut output, out).await {
            return ExitCode::FAILURE;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_debug_lines_are_off_unless_asked() {
        for value in [None, Some(""), Some("info"), Some("1"), Some("debugging")] {
            assert!(!debug_asked(value), "{value:?}");
        }
        assert!(debug_asked(Some("debug")));
        assert!(debug_asked(Some(" DEBUG\n")));
        assert!(
            std::env::var_os(LOG_VAR).is_some() || !debug_on(),
            "off without the variable"
        );
    }
}
