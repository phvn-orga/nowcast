//! Flows, subscriptions, fan-out and last value.
//!
//! [`FlowRouter`] implements [`nowcast_traits::Router`]: it attaches
//! subscriptions to anchors, fans each publication out to the anchors
//! subscribed to its flow, keeps the last value of each flow, and signals a
//! flow that loses its last subscriber. It never starts or stops a brick and
//! does no IO: the supervisor and the HTTP server drive it.
//!
//! Subscription ids read `s-<n>`, with `n` a hexadecimal counter, so an id is
//! never issued twice while the service runs. Event ids come from one counter
//! shared by every flow, started from the clock (microseconds since 1970) so
//! that they keep increasing across a restart of the service.

mod router;
mod state;

pub use router::FlowRouter;
