//! The system brick, native to nowcast: what the machine is doing, CPU,
//! memory, GPUs, disks, network, temperatures and pressure, as one flow,
//! `machine` (`system.machine/v1`); and what the process tree of a
//! subscriber's anchor costs, on the machine's scale, as one flow per anchor,
//! `process/<pid>-<start>` (`system.process/v1`). The brick only sees a
//! process, whatever the client.
//! The `nowcast` binary runs it as `nowcast brick system`; its declaration
//! and schemas are [`FILES`].
//!
//! While a flow has a subscriber, the brick samples it every 2 s, the first
//! time 200 ms after the flow opens, and publishes when the rounded detail
//! changes, and after every `refresh`. A process tree is read from `/proc`
//! alone. For the machine, it reads `/proc` and `/sys`, runs `nvidia-smi` for
//! an NVIDIA card, asks nl80211 for the Wi-Fi link, and pings the gateway and
//! two public resolvers (9.9.9.9, 1.1.1.1): its only traffic.

mod disk;
mod gpu;
mod net;
mod pressure;
mod probe;
mod process;
mod sample;
mod sensors;
mod state;
#[cfg(test)]
mod testing;

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use nowcast_codec::{read_service, write_brick};
use nowcast_types::{AnchorProcess, BrickMessage, ServiceMessage};
use tokio::io::{Stdout, stdin, stdout};
use tokio::sync::mpsc::unbounded_channel;
use tokio::time::Instant;

use crate::process::{Tracker, Units};
use crate::sample::Sampler;
use crate::state::{Sampling, State, Target};

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
    (
        "schemas/params-process.json",
        include_str!("../schemas/params-process.json"),
    ),
    (
        "schemas/process-v1.json",
        include_str!("../schemas/process-v1.json"),
    ),
];

/// Time between two samples.
const PERIOD: Duration = Duration::from_secs(2);

/// Time between the reading a flow opens with and its first sample: the
/// shortest that gives a CPU usage (`sysinfo`'s minimum), for the machine as
/// for a process tree.
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
struct Running<S> {
    sampler: S,
    read_at: Instant,
    due: Instant,
}

impl<S> Running<S> {
    fn primed(sampler: S, now: Instant) -> Self {
        Self {
            sampler,
            read_at: now,
            due: now + FIRST,
        }
    }

    /// Samples as soon as a usage can be computed again.
    fn hurry(&mut self) {
        self.due = self.due.min((self.read_at + FIRST).max(Instant::now()));
    }

    /// Marks a sample starting now; the next comes at a fixed pace.
    fn read(&mut self) -> Instant {
        self.read_at = Instant::now();
        self.due = self.read_at + PERIOD;
        self.read_at
    }
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
    let mut machine: Option<Running<Sampler>> = None;
    let mut processes: HashMap<AnchorProcess, Running<Tracker>> = HashMap::new();
    let units = Units::of_system();

    loop {
        let due = machine
            .iter()
            .map(|r| r.due)
            .chain(processes.values().map(|r| r.due))
            .min();
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
                    Sampling::Start(Target::Machine) => {
                        let mut sampler = Sampler::new(Path::new("/"));
                        sampler.prime();
                        machine = Some(Running::primed(sampler, Instant::now()));
                    }
                    Sampling::Start(Target::Process(anchor)) => {
                        let now = Instant::now();
                        let mut tracker = Tracker::new(Path::new("/"), anchor, units);
                        // The first reading only primes the CPU usage.
                        tracker.read(now.into_std());
                        processes.insert(anchor, Running::primed(tracker, now));
                    }
                    Sampling::Stop(Target::Machine) => machine = None,
                    Sampling::Stop(Target::Process(anchor)) => {
                        processes.remove(&anchor);
                    }
                    Sampling::Now(Target::Machine) => machine.iter_mut().for_each(Running::hurry),
                    Sampling::Now(Target::Process(anchor)) => {
                        processes.get_mut(&anchor).into_iter().for_each(Running::hurry);
                    }
                    Sampling::Unchanged => {}
                }
            }
            () = sleep_until(due) => {
                let now = Instant::now();
                // Process trees first: they read in a few milliseconds, while
                // a machine sample can wait on its probes for a second or more.
                for (&anchor, r) in processes.iter_mut().filter(|(_, r)| r.due <= now) {
                    let at = r.read();
                    // Nothing once the process is gone: its `release` follows.
                    let Some(detail) = r.sampler.read(at.into_std()) else { continue };
                    if !send(&mut output, state.on_sample(Target::Process(anchor), &detail)).await {
                        return ExitCode::FAILURE;
                    }
                }
                if let Some(r) = machine.as_mut().filter(|r| r.due <= now) {
                    r.read();
                    let detail = r.sampler.sample().await;
                    if !send(&mut output, state.on_sample(Target::Machine, &detail)).await {
                        return ExitCode::FAILURE;
                    }
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
