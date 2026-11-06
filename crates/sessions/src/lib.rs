//! Session registry: the session each anchor declared, and the cleanup of an
//! anchor when its process dies.
//!
//! [`SessionRegistry`] implements [`nowcast_traits::Sessions`]. A process is
//! not a session: the anchor declares its session first, then again on each
//! change (`/clear`, resume), and a change only replaces the session. The
//! subscriptions belong to the anchor and stay. They go when the anchor
//! process dies: the registry watches each declared anchor through
//! [`nowcast_traits::Identity::exited`], forgets its session and removes the
//! anchor from the router, which signals the flows this leaves without a
//! subscriber. An anchor with no client kind, a process anchored on itself,
//! is watched the same way: the registry only sees its id.

mod registry;

pub use registry::SessionRegistry;
