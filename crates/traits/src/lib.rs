//! Traits between modules, each with an in-memory fake in [`fake`].
//!
//! Who owns what:
//!
//! - The [`Router`] owns flows and subscriptions: it counts the subscribers of
//!   each flow, keeps its last value, and signals when a flow has no subscriber
//!   left. It never starts or stops a brick.
//! - The [`Supervisor`] owns bricks: it decides to start or stop one, maps a
//!   subscription to a flow, and feeds what bricks publish into the router.
//! - [`Discovery`] owns the declarations of bricks and client kinds.
//! - [`Identity`] maps a connection to its anchor process.
//! - [`Sessions`] maps an anchor to the session it declared.

mod anchor;
mod brick;
mod discovery;
pub mod fake;
mod identity;
mod router;
mod sessions;
mod supervisor;

pub use anchor::{Anchor, AnchorId};
pub use brick::{BrickConnection, BrickLauncher, BrickProcess};
pub use discovery::{
    BrickDeclaration, ClientKind, Discovery, DiscoveryChange, SubscriptionKind, TypeDeclaration,
};
pub use identity::Identity;
pub use router::{Router, UnknownFlow, UnknownSubscription};
pub use sessions::Sessions;
pub use supervisor::{BrickUnavailable, ResolveError, Supervisor};
