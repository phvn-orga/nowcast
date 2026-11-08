//! The nowcast daemon: assembles the modules and serves the client API on the
//! service's Unix socket. [`run`] is the whole service; the `nowcast` binary
//! runs it as `nowcast daemon`.
//!
//! Started by systemd on the first connection (`systemd/nowcast.socket`), it
//! takes the listening socket systemd passes. Started by hand, it binds
//! `$XDG_RUNTIME_DIR/nowcast/nowcast.sock` itself.
//!
//! It stops on `SIGTERM` or `SIGINT`, and, when systemd passed the socket,
//! once it has had no connection and no subscription for 60 s
//! (`IDLE_TIMEOUT`): systemd keeps the socket and starts it again on the
//! next connection. `NOWCAST_IDLE_TIMEOUT` sets that delay in seconds
//! (decimals allowed); set, it applies to a daemon started by hand too,
//! which otherwise runs until stopped. On every stop, the connections close
//! first, then each brick gets `shutdown`, `SIGTERM` 5 s later and `SIGKILL`
//! 5 s after that, the supervisor's delays, until it exits.
//!
//! Logs go to stderr, which the journal collects under systemd. `NOWCAST_LOG`
//! sets the filter, in `tracing_subscriber::EnvFilter` syntax, `info` by
//! default.

mod socket;

use std::error::Error;
use std::ffi::OsString;
use std::fs::File;
use std::io::IsTerminal;
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use nowcast_discovery::{
    DiscoveryConfig, FsDiscovery, NativeFiles, native_dir_from_env, refresh_natives,
};
use nowcast_identity::PeerIdentity;
use nowcast_router::FlowRouter;
use nowcast_server::Server;
use nowcast_sessions::SessionRegistry;
use nowcast_supervisor::{BrickSupervisor, Config, ProcessLauncher};
use nowcast_traits::Discovery;
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;

use crate::socket::Socket;

/// Worker threads of the runtime: enough that one blocked task does not
/// stall the others, few enough for a mostly idle service.
const WORKER_THREADS: usize = 2;

/// The variable setting the idle delay, in seconds.
const IDLE_VAR: &str = "NOWCAST_IDLE_TIMEOUT";

/// How long a socket-activated daemon runs with no connection and no
/// subscription before exiting. Longer than a brick's grace period and its
/// stop together (30 s, then 5 s and 5 s), so that by then the bricks have
/// stopped on their own and the exit does not wait for them.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// What the bricks get, beyond the supervisor's own stop delays, to exit
/// once `SIGKILL` is sent, before the daemon stops waiting for them.
const STOP_MARGIN: Duration = Duration::from_secs(2);

type Supervisor = BrickSupervisor<FsDiscovery, ProcessLauncher, FlowRouter>;

/// Runs the service until it is stopped or idle, and returns its exit status.
///
/// `natives` are the native bricks the binary embeds: those written once
/// under `$XDG_DATA_HOME/nowcast/bricks/` are rewritten to match them before
/// the bricks directory is read.
///
/// Reads the environment and edits it while taking the socket: call it from
/// the main thread, before any other thread starts.
pub fn run(natives: &[NativeFiles]) -> ExitCode {
    init_logging();
    let idle = match idle_setting(std::env::var_os(IDLE_VAR)) {
        Ok(idle) => idle,
        Err(e) => {
            tracing::error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    // Before the runtime starts any thread: taking the socket edits the
    // environment.
    let socket = match socket::open() {
        Ok(socket) => socket,
        Err(e) => {
            tracing::error!("cannot open the socket: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Exiting is safe only when systemd holds the socket and starts the
    // daemon again: a daemon started by hand runs until stopped, unless
    // told otherwise.
    let idle = idle.or(socket.bound.is_none().then_some(IDLE_TIMEOUT));
    match idle {
        Some(idle) => tracing::info!("exits after {idle:?} with no connection and no subscription"),
        None => tracing::info!("runs until stopped"),
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .enable_all()
        .build();
    let result = match runtime {
        Ok(runtime) => runtime.block_on(serve(socket, idle, natives)),
        Err(e) => Err(format!("cannot start the runtime: {e}").into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// The idle delay [`IDLE_VAR`] sets, `None` when unset or empty.
fn idle_setting(value: Option<OsString>) -> Result<Option<Duration>, String> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let invalid = || format!("{IDLE_VAR} must be a number of seconds, not {value:?}");
    let seconds: f64 = value
        .to_str()
        .ok_or_else(invalid)?
        .trim()
        .parse()
        .map_err(|_| invalid())?;
    // Refuses a negative, infinite or oversized number.
    Duration::try_from_secs_f64(seconds)
        .map(Some)
        .map_err(|_| invalid())
}

fn init_logging() {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .with_env_var("NOWCAST_LOG")
        .from_env_lossy();
    let logs = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal());
    // The journal stamps each line itself.
    if stderr_is_journal() {
        logs.without_time().init();
    } else {
        logs.init();
    }
}

/// Whether stderr is connected to the journal: systemd then sets
/// `JOURNAL_STREAM` to its `<device>:<inode>`. Comparing them tells the
/// journal apart from a variable inherited by a process started from a shell.
fn stderr_is_journal() -> bool {
    let Some(stream) = std::env::var_os("JOURNAL_STREAM") else {
        return false;
    };
    let stderr = std::io::stderr().as_fd().try_clone_to_owned();
    let Ok(metadata) = stderr.map(File::from).and_then(|file| file.metadata()) else {
        return false;
    };
    stream.to_str() == Some(format!("{}:{}", metadata.dev(), metadata.ino()).as_str())
}

async fn serve(
    socket: Socket,
    idle: Option<Duration>,
    natives: &[NativeFiles],
) -> Result<(), Box<dyn Error>> {
    // Dropped last, when `serve` returns, whether it succeeds or fails.
    let Socket {
        listener,
        bound: _bound,
    } = socket;
    listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(listener)?;
    // Registered before serving, so that a signal is never missed.
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;

    match native_dir_from_env() {
        Ok(dir) => refresh_natives(&dir, natives),
        Err(e) => tracing::warn!("native bricks left as is: {e}"),
    }
    let config = DiscoveryConfig::from_env()?;
    let discovery = Arc::new(
        FsDiscovery::start(config).map_err(|e| format!("cannot start the discovery: {e}"))?,
    );
    let identity = {
        let discovery = Arc::clone(&discovery);
        Arc::new(PeerIdentity::new(move || discovery.client_kinds()))
    };
    let router = Arc::new(FlowRouter::new());
    let delays = Config::default();
    let (supervisor, sessions) =
        supervisor_and_sessions(&router, &discovery, &identity, delays.clone());
    let server = Server {
        router,
        supervisor: Arc::clone(&supervisor),
        discovery,
        identity,
        sessions,
        started_at: SystemTime::now(),
        validators: Default::default(),
    };
    let serving = async move {
        match idle {
            Some(idle) => server.serve_until_idle(listener, idle).await,
            None => server.serve(listener).await,
        }
    };

    tokio::select! {
        () = serving => {}
        _ = terminate.recv() => tracing::info!("SIGTERM received, stopping"),
        _ = interrupt.recv() => tracing::info!("SIGINT received, stopping"),
    }
    // The server is dropped by now, and every connection closed with it: no
    // subscription can launch a brick any more. A signal arriving from here
    // on changes nothing, the stop being bounded.
    let bound = delays.terminate_after + delays.kill_after + STOP_MARGIN;
    if tokio::time::timeout(bound, supervisor.shutdown())
        .await
        .is_err()
    {
        // Their process handles kill them when the runtime shuts down.
        tracing::warn!("bricks still running after {bound:?}, killing them");
    } else {
        tracing::info!("every brick stopped");
    }
    Ok(())
}

/// The brick supervisor and the session registry. Both spawn their tasks on
/// the runtime this runs in.
fn supervisor_and_sessions(
    router: &Arc<FlowRouter>,
    discovery: &Arc<FsDiscovery>,
    identity: &Arc<PeerIdentity>,
    delays: Config,
) -> (Arc<Supervisor>, Arc<SessionRegistry>) {
    let supervisor = BrickSupervisor::start(
        Arc::clone(discovery),
        Arc::new(ProcessLauncher),
        Arc::clone(router),
        delays,
    );
    let sessions = SessionRegistry::new(Arc::clone(identity), Arc::clone(router));
    (Arc::new(supervisor), Arc::new(sessions))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_idle_delay_outlasts_a_brick_stop() {
        let delays = Config::default();
        assert!(IDLE_TIMEOUT > delays.grace + delays.terminate_after + delays.kill_after);
    }

    #[test]
    fn the_idle_delay_is_a_number_of_seconds() {
        let set = |value: &str| idle_setting(Some(value.into()));
        assert_eq!(idle_setting(None), Ok(None));
        assert_eq!(set(""), Ok(None));
        assert_eq!(set("90"), Ok(Some(Duration::from_secs(90))));
        assert_eq!(set(" 0.5 "), Ok(Some(Duration::from_millis(500))));
        assert_eq!(set("0"), Ok(Some(Duration::ZERO)));
        for bad in ["-1", "1s", "inf", "NaN", "1e300", "x"] {
            assert!(set(bad).is_err(), "{bad} accepted");
        }
    }
}
