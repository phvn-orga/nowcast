use std::future::Future;
use std::io;
use std::os::fd::BorrowedFd;

use crate::{Anchor, AnchorId};

/// Maps a connection to its anchor process.
///
/// It receives the client kinds already loaded by discovery and compares them
/// with the chain of ancestors of the connected process.
pub trait Identity: Send + Sync + 'static {
    /// The anchor of the process connected on `socket`. Two connections from
    /// the same anchor process get the same [`AnchorId`].
    fn identify(&self, socket: BorrowedFd<'_>) -> io::Result<Anchor>;

    /// Resolves once the anchor process has exited.
    fn exited(&self, anchor: AnchorId) -> impl Future<Output = ()> + Send;
}
