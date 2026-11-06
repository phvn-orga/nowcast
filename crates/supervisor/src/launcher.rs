use std::io;
use std::process::Stdio;
use std::time::Duration;

use nowcast_codec::{CodecError, read_brick, write_service};
use nowcast_traits::{BrickConnection, BrickDeclaration, BrickLauncher, BrickProcess};
use nowcast_types::{BrickMessage, BrickName, ServiceMessage};
use rustix::io::Errno;
use rustix::process::{Pid, Signal as RawSignal, kill_process_group};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::TryRecvError;
use tokio::time::{Instant, timeout_at};
use tracing::{debug, info, warn};

/// Launches bricks as child processes speaking the brick protocol over their
/// standard input and output. Each line of their standard error becomes an
/// `info` event of the brick, for the journal; a line over 8 KiB is logged
/// in pieces of 8 KiB.
///
/// Each brick runs in its own process group, which receives the signals: a
/// brick launched through a wrapper (`sh -c`, `npx`, `uv run`) stops with
/// everything it started. Once the brick's process has exited, what is left
/// in its group is killed, so that a relaunch never meets an earlier
/// instance.
///
/// The connection's `from_brick` closes once the process has exited, so that
/// its end means the process is gone. The exit ends the output even if
/// another process still holds the pipe. A brick
/// whose output cannot be read any more (a cut frame, an oversized one) is
/// killed. A frame that reads whole but does not decode is logged and
/// skipped.
///
/// Must be used within a tokio runtime: each launch spawns the tasks that
/// write, read and wait on the process.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessLauncher;

#[derive(Debug, Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

/// The signals of one process, carried out by the task that waits on it, so
/// that a signal never reaches a process already reaped (whose pid may have
/// been reused).
struct Signals(UnboundedSender<Signal>);

impl BrickProcess for Signals {
    fn terminate(&mut self) {
        let _ = self.0.send(Signal::Term);
    }

    fn kill(&mut self) {
        let _ = self.0.send(Signal::Kill);
    }
}

impl BrickLauncher for ProcessLauncher {
    fn launch(&self, brick: &BrickDeclaration) -> io::Result<BrickConnection> {
        let (program, args) = brick.command.split_first().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "the brick's command is empty")
        })?;
        let mut child = Command::new(program)
            .args(args)
            .current_dir(&brick.dir)
            .envs(&brick.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()?;
        let piped = "the standard streams are piped";
        let stdin = child.stdin.take().expect(piped);
        let stdout = child.stdout.take().expect(piped);
        let stderr = child.stderr.take().expect(piped);
        info!(brick = %brick.name, pid = child.id(), "brick launched");
        // The brick leads its own group: the group id is its pid.
        let group = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(Pid::from_raw);

        let (to_brick, outgoing) = unbounded_channel();
        let (incoming, from_brick) = unbounded_channel();
        let (signals, signal_rx) = unbounded_channel();
        let (exited_tx, exited) = oneshot::channel();
        let name = &brick.name;
        tokio::spawn(write(name.clone(), stdin, outgoing));
        tokio::spawn(log(name.clone(), stderr));
        tokio::spawn(read(
            name.clone(),
            stdout,
            incoming,
            signals.clone(),
            exited,
        ));
        tokio::spawn(wait(name.clone(), child, group, signal_rx, exited_tx));
        Ok(BrickConnection {
            to_brick,
            from_brick,
            process: Box::new(Signals(signals)),
        })
    }
}

/// Writes the service's messages until the supervisor drops its sender,
/// which closes the brick's input.
async fn write(
    brick: BrickName,
    mut stdin: ChildStdin,
    mut outgoing: UnboundedReceiver<ServiceMessage>,
) {
    while let Some(msg) = outgoing.recv().await {
        if let Err(e) = write_service(&mut stdin, &msg).await {
            debug!(brick = %brick, "cannot write to the brick: {e}");
            return;
        }
    }
}

/// How long the output is still read once the process has exited, for what
/// it wrote before exiting.
const DRAIN: Duration = Duration::from_millis(100);

/// Reads the brick's messages until its output ends, then holds the channel
/// open until the process has exited.
///
/// The exit ends the output even if another process still holds the pipe,
/// and even if it keeps writing: from the exit on, the output is read for
/// [`DRAIN`] at most, which lets through what the brick wrote before exiting.
async fn read(
    brick: BrickName,
    stdout: ChildStdout,
    incoming: UnboundedSender<BrickMessage>,
    signals: UnboundedSender<Signal>,
    mut exited: oneshot::Receiver<()>,
) {
    let mut stdout = BufReader::new(stdout);
    let mut drain_until = None;
    loop {
        // Checked on every message, so that a writer that never pauses does
        // not hide the exit.
        if drain_until.is_none() && !matches!(exited.try_recv(), Err(TryRecvError::Empty)) {
            drain_until = Some(Instant::now() + DRAIN);
        }
        let read = match drain_until {
            // A read always ready would never time out.
            Some(deadline) if Instant::now() >= deadline => return,
            Some(deadline) => match timeout_at(deadline, read_brick(&mut stdout)).await {
                Ok(read) => read,
                // Cuts at most a frame the brick never finished writing.
                Err(_) => return,
            },
            None => tokio::select! {
                read = read_brick(&mut stdout) => read,
                _ = &mut exited => {
                    drain_until = Some(Instant::now() + DRAIN);
                    continue;
                }
            },
        };
        match read {
            Ok(Some(msg)) => {
                let _ = incoming.send(msg);
            }
            Ok(None) => break,
            Err(
                e
                @ (CodecError::Io(_) | CodecError::Truncated { .. } | CodecError::TooLarge { .. }),
            ) => {
                warn!(brick = %brick, "unreadable output, killing the brick: {e}");
                let _ = signals.send(Signal::Kill);
                break;
            }
            Err(e) => warn!(brick = %brick, "invalid message skipped: {e}"),
        }
    }
    if drain_until.is_none() {
        let _ = exited.await;
    }
}

/// The longest piece of a stderr line logged as one event. A longer line is
/// logged in pieces, so that a brick cannot make the service hold an
/// unbounded line.
const MAX_LOG_LINE: u64 = 8 * 1024;

/// Forwards each line of the brick's standard error to the journal.
async fn log(brick: BrickName, stderr: ChildStderr) {
    let mut stderr = BufReader::new(stderr);
    let mut line = Vec::new();
    loop {
        line.clear();
        let mut piece = (&mut stderr).take(MAX_LOG_LINE);
        match piece.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                let text = String::from_utf8_lossy(&line);
                info!(brick = %brick, "{}", text.trim_end());
            }
        }
    }
}

/// Waits for the process to exit, sending its group the signals asked for
/// meanwhile, then kills what is left in the group.
async fn wait(
    brick: BrickName,
    mut child: Child,
    group: Option<Pid>,
    mut signals: UnboundedReceiver<Signal>,
    exited: oneshot::Sender<()>,
) {
    let mut listening = true;
    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            signal = signals.recv(), if listening => match signal {
                Some(Signal::Term) => signal_group(&brick, group, RawSignal::TERM),
                Some(Signal::Kill) => signal_group(&brick, group, RawSignal::KILL),
                None => listening = false,
            },
        }
    };
    match status {
        Ok(status) => info!(brick = %brick, %status, "brick exited"),
        Err(e) => warn!(brick = %brick, "cannot wait for the brick: {e}"),
    }
    // A group id stays taken while any process is in the group, so the
    // brick's leftovers are still the ones reached.
    signal_group(&brick, group, RawSignal::KILL);
    let _ = exited.send(());
}

fn signal_group(brick: &BrickName, group: Option<Pid>, signal: RawSignal) {
    let Some(group) = group else {
        return;
    };
    match kill_process_group(group, signal) {
        // Nobody is left in the group.
        Ok(()) | Err(Errno::SRCH) => {}
        Err(e) => warn!(brick = %brick, "cannot signal the brick's group: {e}"),
    }
}
