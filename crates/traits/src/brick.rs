use std::io;

use nowcast_types::{BrickMessage, ServiceMessage};
use tokio::sync::mpsc;

use crate::BrickDeclaration;

/// Signals to a running brick process.
pub trait BrickProcess: Send + 'static {
    /// Sends `SIGTERM`.
    fn terminate(&mut self);

    /// Sends `SIGKILL`.
    fn kill(&mut self);
}

/// A running brick, seen as protocol messages.
///
/// `from_brick` closes when the brick's output closes, whether it exited or
/// crashed: the supervisor tells the two apart by whether it asked it to stop.
pub struct BrickConnection {
    pub to_brick: mpsc::UnboundedSender<ServiceMessage>,
    pub from_brick: mpsc::UnboundedReceiver<BrickMessage>,
    pub process: Box<dyn BrickProcess>,
}

/// Launches brick processes and speaks the brick protocol with them.
pub trait BrickLauncher: Send + Sync + 'static {
    /// Runs `brick.command` in `brick.dir`, a bare program name looked up in
    /// `PATH`, with `brick.env` added to the service's environment.
    fn launch(&self, brick: &BrickDeclaration) -> io::Result<BrickConnection>;
}
