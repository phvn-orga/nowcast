//! The test's side of the control socket.

use std::io;
use std::path::{Path, PathBuf};

use nowcast_types::{FlowId, PublishErrorCode, ServiceMessage};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};

use crate::wire::{Command, Reply};

/// The control socket, listened on by the test. Every test brick process
/// started with [`crate::CONTROL_VAR`] set to its path connects to it once, at
/// start.
pub struct Control {
    listener: UnixListener,
    path: PathBuf,
}

impl Control {
    /// Listens on `path`, which must not exist yet.
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_owned();
        Ok(Self {
            listener: UnixListener::bind(&path)?,
            path,
        })
    }

    /// The socket path, the value of [`crate::CONTROL_VAR`].
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Waits for the next brick process to start. Cancel safe.
    pub async fn accept(&self) -> io::Result<ControlledBrick> {
        let (stream, _) = self.listener.accept().await?;
        Ok(ControlledBrick::new(stream))
    }
}

/// One running test brick process, driven over its control connection.
///
/// Each call sends one command and waits for its reply. The calls are not
/// cancel safe: a call dropped before its reply leaves the reply unread, and
/// the next call would take it for its own.
pub struct ControlledBrick {
    lines: Lines<BufReader<OwnedReadHalf>>,
    writer: OwnedWriteHalf,
}

impl ControlledBrick {
    fn new(stream: UnixStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            lines: BufReader::new(reader).lines(),
            writer,
        }
    }

    /// Makes the brick send `publish`, and returns the service's answer: `Ok`
    /// for `ack`, the code of its `error` otherwise. Any flow, type and bytes
    /// go, so a test can send what the service must refuse.
    pub async fn publish(
        &mut self,
        flow: &FlowId,
        data_type: &str,
        detail: &[u8],
    ) -> io::Result<Result<(), PublishErrorCode>> {
        let reply = self
            .call(&Command::Publish {
                flow: flow.as_str().to_owned(),
                data_type: data_type.to_owned(),
                detail: detail.to_vec(),
            })
            .await?;
        match reply {
            Reply::Acked => Ok(Ok(())),
            Reply::Refused { code } => code.parse().map(Err).map_err(invalid_data),
            other => Err(unexpected(other)),
        }
    }

    /// Makes the brick send `flow-error`. Returns once it is written.
    pub async fn flow_error(&mut self, flow: &FlowId, message: &str) -> io::Result<()> {
        let reply = self
            .call(&Command::FlowError {
                flow: flow.as_str().to_owned(),
                message: message.to_owned(),
            })
            .await?;
        match reply {
            Reply::Done => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    /// Every message the service sent this process so far, in order.
    pub async fn received(&mut self) -> io::Result<Vec<ServiceMessage>> {
        match self.call(&Command::Received).await? {
            Reply::Received { messages } => messages
                .into_iter()
                .map(ServiceMessage::try_from)
                .collect::<Result<_, _>>()
                .map_err(invalid_data),
            other => Err(unexpected(other)),
        }
    }

    /// Makes the brick exit at once with status [`crate::CRASH_STATUS`], as a
    /// crash. Returns once the process has closed the connection.
    pub async fn crash(mut self) -> io::Result<()> {
        self.send(&Command::Crash).await?;
        self.exited().await
    }

    /// Waits for the process to end, seen as its connection closing.
    pub async fn exited(mut self) -> io::Result<()> {
        match self.lines.next_line().await? {
            None => Ok(()),
            Some(line) => Err(invalid_data(format!("unexpected line {line:?}"))),
        }
    }

    async fn send(&mut self, command: &Command) -> io::Result<()> {
        let mut line = serde_json::to_vec(command).map_err(io::Error::other)?;
        line.push(b'\n');
        self.writer.write_all(&line).await?;
        self.writer.flush().await
    }

    async fn call(&mut self, command: &Command) -> io::Result<Reply> {
        self.send(command).await?;
        let line = self.lines.next_line().await?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the brick exited before replying",
            )
        })?;
        match serde_json::from_str(&line).map_err(invalid_data)? {
            Reply::Invalid { message } => Err(io::Error::new(io::ErrorKind::InvalidInput, message)),
            reply => Ok(reply),
        }
    }
}

fn invalid_data(e: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

fn unexpected(reply: Reply) -> io::Error {
    invalid_data(format!("unexpected reply {reply:?}"))
}
