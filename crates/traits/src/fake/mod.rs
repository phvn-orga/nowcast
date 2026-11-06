//! In-memory fakes of every trait, usable from any crate's tests as is.
//!
//! [`FakeRouter`] is a complete in-memory router: it passes the router
//! conformance suite. The others hold state the test sets and expose what the
//! module under test did to them.

mod brick;
mod discovery;
mod identity;
mod router;
mod sessions;
mod supervisor;

pub use brick::{FakeBrick, FakeLauncher, Signal};
pub use discovery::FakeDiscovery;
pub use identity::FakeIdentity;
pub use router::FakeRouter;
pub use sessions::FakeSessions;
pub use supervisor::FakeSupervisor;
