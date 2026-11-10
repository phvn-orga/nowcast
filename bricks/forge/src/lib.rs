//! The forge brick, native to nowcast: a branch's PR and its pipelines,
//! whatever the forge. The `nowcast` binary runs it as `nowcast brick
//! forge`; its declaration and schemas are [`FILES`].
//!
//! The brick is organised by usages, not by forges:
//!
//! - [`registry`] is the core: the providers, the flows and their
//!   publication;
//! - [`provider`] is what a forge implements, and [`model`] the neutral
//!   output every provider maps its forge onto;
//! - [`tools`] holds what a provider may use (conditional HTTP, a pace
//!   machine, remote parsing), and nothing obliges it to;
//! - [`github`] is the GitHub provider.

mod github;
mod model;
mod provider;
mod registry;
#[cfg(test)]
mod testing;
mod tools;

use std::process::ExitCode;

use jiff::Timestamp;
use nowcast_codec::{read_service, write_brick};
use nowcast_types::{BrickMessage, ServiceMessage};
use tokio::io::{Stdout, stdin, stdout};
use tokio::sync::mpsc::unbounded_channel;

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
        "schemas/params-branch.json",
        include_str!("../schemas/params-branch.json"),
    ),
    ("schemas/pr-v1.json", include_str!("../schemas/pr-v1.json")),
    (
        "schemas/pipelines-v1.json",
        include_str!("../schemas/pipelines-v1.json"),
    ),
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

fn log(message: impl std::fmt::Display) {
    eprintln!("{NAME}: {message}");
}

/// The providers the brick carries.
fn providers(http: &Http) -> Vec<Box<dyn Provider>> {
    let api = std::env::var(github::API_VAR).unwrap_or_else(|_| github::API.to_owned());
    vec![Box::new(GitHub::new(http.clone(), api))]
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
                    ServiceMessage::Input { .. } | ServiceMessage::Ack { .. } => vec![],
                }
            }
            Some((target, outcome)) = updates.recv() => core.update(target, outcome, Timestamp::now()),
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
