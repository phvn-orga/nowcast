use std::io;
use std::process::Stdio;

use nowcast_codec::{CodecError, read_brick, write_service};
use nowcast_traits::{BrickConnection, BrickDeclaration, BrickLauncher, BrickProcess};
use nowcast_types::{BrickMessage, BrickName, ServiceMessage};
use rustix::process::{Pid, Signal as RawSignal, kill_process};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

/// Launches bricks as child processes speaking the brick protocol over their
/// standard input and output. Each line of their standard error becomes an
/// `info` event of the brick, for the journal.
///
/// The connection's `from_brick` closes once the process has exited, so that
/// its end means the process is gone. The exit ends the output even if a
/// process the brick spawned still holds the pipe. A brick
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
            .kill_on_drop(true)
            .spawn()?;
        let piped = "the standard streams are piped";
        let stdin = child.stdin.take().expect(piped);
        let stdout = child.stdout.take().expect(piped);
        let stderr = child.stderr.take().expect(piped);
        info!(brick = %brick.name, pid = child.id(), "brick launched");

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
        tokio::spawn(wait(name.clone(), child, signal_rx, exited_tx));
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

/// Reads the brick's messages until its output ends or the process exits,
/// whichever comes first, then holds the channel open until the process has
/// exited.
///
/// The exit ends the output even if a process the brick spawned still holds
/// the pipe. What the brick wrote before exiting is still read: the read
/// goes first while the pipe holds whole frames.
async fn read(
    brick: BrickName,
    stdout: ChildStdout,
    incoming: UnboundedSender<BrickMessage>,
    signals: UnboundedSender<Signal>,
    mut exited: oneshot::Receiver<()>,
) {
    let mut stdout = BufReader::new(stdout);
    loop {
        let read = tokio::select! {
            biased;
            read = read_brick(&mut stdout) => read,
            // Cuts at most a frame the brick never finished writing.
            _ = &mut exited => return,
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
    let _ = exited.await;
}

/// Forwards each line of the brick's standard error to the journal.
async fn log(brick: BrickName, stderr: ChildStderr) {
    let mut stderr = BufReader::new(stderr);
    let mut line = Vec::new();
    loop {
        line.clear();
        match stderr.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                let text = String::from_utf8_lossy(&line);
                info!(brick = %brick, "{}", text.trim_end());
            }
        }
    }
}

/// Waits for the process to exit, sending it the signals asked for meanwhile.
async fn wait(
    brick: BrickName,
    mut child: Child,
    mut signals: UnboundedReceiver<Signal>,
    exited: oneshot::Sender<()>,
) {
    let mut listening = true;
    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            signal = signals.recv(), if listening => match signal {
                Some(Signal::Term) => terminate(&brick, &child),
                Some(Signal::Kill) => {
                    if let Err(e) = child.start_kill() {
                        warn!(brick = %brick, "cannot kill the brick: {e}");
                    }
                }
                None => listening = false,
            },
        }
    };
    match status {
        Ok(status) => info!(brick = %brick, %status, "brick exited"),
        Err(e) => warn!(brick = %brick, "cannot wait for the brick: {e}"),
    }
    let _ = exited.send(());
}

fn terminate(brick: &BrickName, child: &Child) {
    // Not reaped yet, as `wait` has not returned: the pid is still the brick's.
    let Some(pid) = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(Pid::from_raw)
    else {
        return;
    };
    if let Err(e) = kill_process(pid, RawSignal::TERM) {
        warn!(brick = %brick, "cannot terminate the brick: {e}");
    }
}
