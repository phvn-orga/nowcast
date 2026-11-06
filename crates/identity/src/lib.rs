//! Peer identity: the anchor process and client kind of a connection.
//!
//! For each connection on the service's Unix socket, [`PeerIdentity`] takes a
//! pidfd on the connected process from the kernel (`SO_PEERPIDFD`), walks up
//! its ancestors, and anchors the connection on the nearest process whose
//! executable matches a declared client kind, the connected process included.
//! When none matches, the connected process anchors itself, with no kind. The
//! anchor is held by a pidfd, which signals its death.
//!
//! The client kinds are read again for each connection, so that a kind the
//! user changes while the service runs applies from the next connection.
//!
//! This identity tells processes of the same user apart, it does not
//! authenticate them: a program of that user can launch an agent or replace
//! its executable (`00-cadrage.md`, the accepted security limit).
//!
//! Linux only, 6.5 or later.

mod kinds;
mod peer;
mod process;

pub use kinds::KindMatcher;
pub use peer::PeerIdentity;
