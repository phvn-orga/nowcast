//! The test brick process. See the library documentation for what it does
//! and how a test drives it.
//!
//! Exit status: 0 after `shutdown` or the end of its input, 1 when its input
//! is unreadable or its output closed, [`CONFIG_STATUS`] on a bad
//! configuration, [`CRASH_STATUS`] on a crash on demand.

mod brick;

use std::env::{self, VarError};
use std::fmt::Display;
use std::future;
use std::path::PathBuf;
use std::process::ExitCode;

use nowcast_codec::{CodecError, read_service, write_brick};
use nowcast_test_brick::wire::Reply;
use nowcast_test_brick::{CONFIG_STATUS, CONTROL_VAR, CRASH_STATUS, MODE_VAR, Mode};
use nowcast_types::ServiceMessage;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::brick::{Brick, Effect};

const FAILURE_STATUS: i32 = 1;

struct Config {
    mode: Mode,
    control: Option<PathBuf>,
}

fn config() -> Result<Config, String> {
    let mode = match env::var(MODE_VAR) {
        Ok(mode) => mode.parse().map_err(|e| format!("{MODE_VAR}: {e}"))?,
        Err(VarError::NotPresent) => Mode::default(),
        Err(e) => return Err(format!("{MODE_VAR}: {e}")),
    };
    let control = env::var_os(CONTROL_VAR)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    Ok(Config { mode, control })
}

fn main() -> ExitCode {
    let status = match config() {
        Ok(config) => match tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
        {
            Ok(runtime) => {
                let status = runtime.block_on(run(config));
                // Standard input is read on a blocking thread that may sit in
                // a read forever: do not wait for it.
                runtime.shutdown_background();
                status
            }
            Err(e) => {
                log(format_args!("cannot start: {e}"));
                FAILURE_STATUS
            }
        },
        Err(e) => {
            log(e);
            CONFIG_STATUS
        }
    };
    ExitCode::from(u8::try_from(status).unwrap_or(1))
}

/// Standard error is the brick's log, collected by the service.
fn log(message: impl Display) {
    eprintln!("nowcast-test-brick: {message}");
}

/// Serves the service on standard input and output, and the test on the
/// control socket, until an effect ends the process. Returns its status.
async fn run(config: Config) -> i32 {
    let (mut commands, mut replies) = match &config.control {
        Some(path) => match UnixStream::connect(path).await {
            Ok(stream) => {
                let (reader, writer) = stream.into_split();
                (Some(read_lines(reader)), Some(writer))
            }
            Err(e) => {
                log(format_args!(
                    "cannot reach the control socket {path:?}: {e}"
                ));
                return CONFIG_STATUS;
            }
        },
        None => (None, None),
    };
    let mut service = read_service_messages();
    let mut stdout = tokio::io::stdout();
    let mut brick = Brick::new(config.mode);

    loop {
        let effects = tokio::select! {
            msg = service.recv() => match msg {
                Some(Ok(msg)) => brick.on_service(msg),
                // The service closed our input: nobody is left to serve.
                None => return 0,
                Some(Err(e)) => {
                    log(format_args!("unreadable input: {e}"));
                    return FAILURE_STATUS;
                }
            },
            line = recv_line(&mut commands), if !brick.is_awaiting() => match line {
                Some(line) => brick.on_control(&line),
                None => {
                    log("the control connection closed");
                    commands = None;
                    replies = None;
                    continue;
                }
            },
        };
        for effect in effects {
            match effect {
                Effect::Send(msg) => {
                    if let Err(e) = write_brick(&mut stdout, &msg).await {
                        log(format_args!("cannot write to the service: {e}"));
                        return FAILURE_STATUS;
                    }
                }
                Effect::Reply(reply) => {
                    if let Some(writer) = &mut replies
                        && let Err(e) = write_reply(writer, &reply).await
                    {
                        log(format_args!("cannot reply to the test: {e}"));
                        replies = None;
                    }
                }
                Effect::Exit(status) => {
                    if status == CRASH_STATUS {
                        log("crashing on demand");
                    }
                    return status;
                }
            }
        }
    }
}

/// Reads standard input on its own task, since a frame read is not cancel
/// safe. The channel closes at the end of the input, or after an error.
fn read_service_messages() -> UnboundedReceiver<Result<ServiceMessage, CodecError>> {
    let (tx, rx) = unbounded_channel();
    tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        while let Some(msg) = read_service(&mut stdin).await.transpose() {
            let failed = msg.is_err();
            if tx.send(msg).is_err() || failed {
                break;
            }
        }
    });
    rx
}

/// Reads the control lines on their own task. The channel closes at the end
/// of the connection, or after an error.
fn read_lines(reader: OwnedReadHalf) -> UnboundedReceiver<String> {
    let (tx, rx) = unbounded_channel();
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    log(format_args!("unreadable control connection: {e}"));
                    break;
                }
            }
        }
    });
    rx
}

/// The next control line, `None` once the connection closed. Without
/// control, never resolves.
async fn recv_line(commands: &mut Option<UnboundedReceiver<String>>) -> Option<String> {
    match commands {
        Some(commands) => commands.recv().await,
        None => future::pending().await,
    }
}

async fn write_reply(writer: &mut OwnedWriteHalf, reply: &Reply) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(reply).map_err(std::io::Error::other)?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await
}
