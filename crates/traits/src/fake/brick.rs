use std::io;

use nowcast_types::{BrickMessage, BrickName, ServiceMessage};
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::{BrickConnection, BrickDeclaration, BrickLauncher, BrickProcess};

/// A signal the supervisor sent to a fake brick process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Term,
    Kill,
}

/// The brick's side of a fake launch: what the test reads and answers.
///
/// Dropping it closes the brick's output, which the supervisor sees as the
/// process ending.
pub struct FakeBrick {
    pub name: BrickName,
    pub from_service: UnboundedReceiver<ServiceMessage>,
    pub to_service: UnboundedSender<BrickMessage>,
    pub signals: UnboundedReceiver<Signal>,
}

struct FakeProcess(UnboundedSender<Signal>);

impl BrickProcess for FakeProcess {
    fn terminate(&mut self) {
        let _ = self.0.send(Signal::Term);
    }

    fn kill(&mut self) {
        let _ = self.0.send(Signal::Kill);
    }
}

/// Launches no process: each launch hands a [`FakeBrick`] to the test.
pub struct FakeLauncher {
    launched_tx: UnboundedSender<FakeBrick>,
    launched: Mutex<UnboundedReceiver<FakeBrick>>,
}

impl Default for FakeLauncher {
    fn default() -> Self {
        let (launched_tx, launched) = unbounded_channel();
        Self {
            launched_tx,
            launched: Mutex::new(launched),
        }
    }
}

impl FakeLauncher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Waits for the next launch.
    pub async fn next(&self) -> FakeBrick {
        self.launched
            .lock()
            .await
            .recv()
            .await
            .expect("the launcher holds a sender")
    }

    /// The next launch if one already happened.
    pub fn try_next(&self) -> Option<FakeBrick> {
        self.launched.try_lock().ok()?.try_recv().ok()
    }
}

impl BrickLauncher for FakeLauncher {
    fn launch(&self, brick: &BrickDeclaration) -> io::Result<BrickConnection> {
        let (to_brick, from_service) = unbounded_channel();
        let (to_service, from_brick) = unbounded_channel();
        let (signal_tx, signals) = unbounded_channel();
        let _ = self.launched_tx.send(FakeBrick {
            name: brick.name.clone(),
            from_service,
            to_service,
            signals,
        });
        Ok(BrickConnection {
            to_brick,
            from_brick,
            process: Box::new(FakeProcess(signal_tx)),
        })
    }
}
