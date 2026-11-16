//! The system brick, native to nowcast: what the machine is doing, CPU,
//! memory, GPUs, disks, network, temperatures and pressure, as one flow,
//! `machine` (`system.machine/v1`). The `nowcast` binary runs it as `nowcast
//! brick system`; its declaration and schemas are [`FILES`].
//!
//! While the flow has a subscriber, the brick samples the machine every 2 s,
//! the first time 200 ms after the flow opens, and publishes when the
//! rounded detail changes, and after every `refresh`. It reads `/proc` and
//! `/sys`, runs `nvidia-smi` for an NVIDIA card, asks nl80211 for the Wi-Fi
//! link, and pings the gateway and two public resolvers (9.9.9.9, 1.1.1.1):
//! its only traffic.

mod disk;
mod gpu;
mod net;
mod pressure;
mod probe;
mod sample;
mod sensors;
mod state;
#[cfg(test)]
mod testing;

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use nowcast_codec::{read_service, write_brick};
use nowcast_types::{BrickMessage, ServiceMessage};
use tokio::io::{Stdout, stdin, stdout};
use tokio::sync::mpsc::unbounded_channel;
use tokio::time::Instant;

use crate::sample::Sampler;
use crate::state::{Sampling, State};

/// The brick's name, as its declaration gives it.
pub const NAME: &str = "system";

/// The declaration and its schemas, by path relative to the declaration's
/// directory.
pub const FILES: &[(&str, &str)] = &[
    (
        "brick.toml",
        include_str!(concat!(env!("OUT_DIR"), "/brick.toml")),
    ),
    (
        "schemas/params-machine.json",
        include_str!("../schemas/params-machine.json"),
    ),
    (
        "schemas/machine-v1.json",
        include_str!("../schemas/machine-v1.json"),
    ),
];

/// Time between two samples.
const PERIOD: Duration = Duration::from_secs(2);

/// Time between the reading a flow opens with and its first sample: the
/// shortest that gives a CPU usage (`sysinfo`'s minimum).
const FIRST: Duration = sysinfo::MINIMUM_CPU_UPDATE_INTERVAL;

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

/// A sampler and when it was primed or last sampled.
struct Running {
    sampler: Sampler,
    read_at: Instant,
    due: Instant,
}

async fn serve() -> ExitCode {
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
    let mut state = State::default();
    let mut running: Option<Running> = None;

    loop {
        let due = running.as_ref().map(|r| r.due);
        tokio::select! {
            msg = from_service.recv() => {
                let Some(msg) = msg else { return ExitCode::SUCCESS };
                if msg == ServiceMessage::Shutdown {
                    return ExitCode::SUCCESS;
                }
                let (out, sampling) = state.on_service(msg);
                if !send(&mut output, out).await {
                    return ExitCode::FAILURE;
                }
                match sampling {
                    Sampling::Start => {
                        let mut sampler = Sampler::new(Path::new("/"));
                        sampler.prime();
                        let now = Instant::now();
                        running = Some(Running { sampler, read_at: now, due: now + FIRST });
                    }
                    Sampling::Stop => running = None,
                    Sampling::Now => {
                        if let Some(r) = running.as_mut() {
                            r.due = r.due.min((r.read_at + FIRST).max(Instant::now()));
                        }
                    }
                    Sampling::Unchanged => {}
                }
            }
            () = sleep_until(due) => {
                let Some(r) = running.as_mut() else { continue };
                r.read_at = Instant::now();
                let detail = r.sampler.sample().await;
                // A fixed pace, from the sample's start.
                r.due = r.read_at + PERIOD;
                if !send(&mut output, state.on_sample(&detail)).await {
                    return ExitCode::FAILURE;
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
