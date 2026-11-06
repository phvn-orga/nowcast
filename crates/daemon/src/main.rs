//! The nowcast daemon: assembles the modules and serves the client API on the
//! service's Unix socket.
//!
//! Started by systemd on the first connection (`systemd/nowcast.socket`), it
//! takes the listening socket systemd passes. Started by hand, it binds
//! `$XDG_RUNTIME_DIR/nowcast/nowcast.sock` itself. It stops on `SIGTERM` or
//! `SIGINT`, closing every connection.
//!
//! Logs go to stderr, which the journal collects under systemd. `NOWCAST_LOG`
//! sets the filter, in `tracing_subscriber::EnvFilter` syntax, `info` by
//! default.

mod socket;

use std::error::Error;
use std::fs::File;
use std::io::IsTerminal;
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;
use std::process::ExitCode;
use std::sync::Arc;

use nowcast_discovery::{DiscoveryConfig, FsDiscovery};
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

type Supervisor = BrickSupervisor<FsDiscovery, ProcessLauncher, FlowRouter>;

fn main() -> ExitCode {
    init_logging();
    // Before the runtime starts any thread: taking the socket edits the
    // environment.
    let socket = match socket::open() {
        Ok(socket) => socket,
        Err(e) => {
            tracing::error!("cannot open the socket: {e}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .enable_all()
        .build();
    let result = match runtime {
        Ok(runtime) => runtime.block_on(run(socket)),
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

async fn run(socket: Socket) -> Result<(), Box<dyn Error>> {
    // Dropped last, when `run` returns, whether it succeeds or fails.
    let Socket {
        listener,
        bound: _bound,
    } = socket;
    listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(listener)?;
    // Registered before serving, so that a signal is never missed.
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;

    let config = DiscoveryConfig::from_env()?;
    let discovery = Arc::new(
        FsDiscovery::start(config).map_err(|e| format!("cannot start the discovery: {e}"))?,
    );
    let identity = {
        let discovery = Arc::clone(&discovery);
        Arc::new(PeerIdentity::new(move || discovery.client_kinds()))
    };
    let router = Arc::new(FlowRouter::new());
    let (supervisor, sessions) = supervisor_and_sessions(&router, &discovery, &identity);
    let server = Server {
        router,
        supervisor,
        discovery,
        identity,
        sessions,
    };

    tokio::select! {
        () = server.serve(listener) => {}
        _ = terminate.recv() => tracing::info!("SIGTERM received, stopping"),
        _ = interrupt.recv() => tracing::info!("SIGINT received, stopping"),
    }
    // The server is dropped by now, and every connection closed with it.
    Ok(())
}

/// The brick supervisor and the session registry. Both spawn their tasks on
/// the runtime this runs in.
///
/// The bricks a supervisor launches are killed when the runtime shuts down,
/// as `main` returns: each one's process handle lives in a task of the
/// runtime and kills the process when dropped.
fn supervisor_and_sessions(
    router: &Arc<FlowRouter>,
    discovery: &Arc<FsDiscovery>,
    identity: &Arc<PeerIdentity>,
) -> (Arc<Supervisor>, Arc<SessionRegistry>) {
    let supervisor = BrickSupervisor::start(
        Arc::clone(discovery),
        Arc::new(ProcessLauncher),
        Arc::clone(router),
        Config::default(),
    );
    let sessions = SessionRegistry::new(Arc::clone(identity), Arc::clone(router));
    (Arc::new(supervisor), Arc::new(sessions))
}
