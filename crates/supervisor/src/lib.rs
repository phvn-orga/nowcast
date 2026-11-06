//! Starting, stopping and restarting bricks.
//!
//! [`BrickSupervisor`] implements [`nowcast_traits::Supervisor`]: it is the
//! only module that starts or stops a brick, on the router's signals.
//! [`ProcessLauncher`] implements [`nowcast_traits::BrickLauncher`] with child
//! processes and the brick codec.
//!
//! # A brick's life
//!
//! - **Launch** at the first subscription, from the declaration's command. The
//!   service sends `hello` and waits for `ready`. Without it in time, the brick
//!   is stopped and every subscription waiting on it fails with
//!   [`nowcast_traits::BrickUnavailable`]. Subscriptions arriving meanwhile
//!   wait for the same launch.
//! - **Subscriptions**: each one is a `subscribe`, answered with the flow,
//!   prefixed with the brick's name, or with `error`. On the answer, the
//!   supervisor subscribes the anchor in the router, in the same step as it
//!   handles the brick's next messages: a value published right after
//!   `subscribed` is the subscription's `last` or reaches the anchor's
//!   streams. A client gone by then gets no subscription, and one made but
//!   never read is undone. A brick that does not answer in time makes the
//!   subscription fail as unavailable.
//! - **A flow left while subscribed again**: every release (a flow that loses
//!   its last subscriber, an answer for a client gone or for a request no
//!   longer in flight) waits while a `subscribe` is in flight, and happens
//!   only if the flow is still without a subscriber, so that the brick never
//!   gets `release` after a `subscribe` it took for that flow.
//! - **Publications**: each `publish` is answered with `ack`, or with `error`
//!   for an unknown or released flow (`unknown_flow`), a type missing from the
//!   declaration (`undeclared_type`) or a detail that is not JSON
//!   (`invalid_json`). Only an acknowledged value reaches the router.
//!   `flow-error` is relayed to the flow's clients, a refresh to the brick.
//! - **Stop**: when a flow loses its last subscriber (the router's empty
//!   flows), the brick gets `release` and the router forgets the flow and
//!   its last value, so that a later subscriber starts afresh. Once no flow is live and nothing is in
//!   flight, the grace period starts; a subscription within it keeps the
//!   brick. At its end the brick gets `shutdown`, then `SIGTERM`, then
//!   `SIGKILL`, each after a delay, until it exits. A subscription arriving
//!   while it stops relaunches it once it has exited.
//! - **Crash**: a brick exiting unasked fails the subscriptions in flight,
//!   and each of its live flows gets `flow-error`. While live flows remain, it
//!   is relaunched after a backoff and subscribed again to each of them, which
//!   gives back the same ids since flow ids are deterministic. A subscription
//!   during the backoff fails as unavailable. After too many crashes in a row
//!   the supervisor gives up: the flows get `flow-ended`, and the next
//!   subscription starts afresh. A flow the relaunched brick refuses, or
//!   answers with another id, ends the same way.
//! - **Removal**: a brick that leaves discovery is handled as if each of its
//!   flows had lost its last subscriber, except that the flows end for their
//!   clients (`flow-ended`) and the subscriptions waiting fail as for an
//!   unknown brick. A changed declaration applies from the next launch.
//!
//! # Delays
//!
//! [`Config::default`] holds these values; each is a field of [`Config`].
//!
//! | Delay | Default | From the contract |
//! |---|---|---|
//! | `hello` to `ready` | 5 s | yes ([`nowcast_types::READY_TIMEOUT`]) |
//! | `subscribe` to its answer | 10 s | no |
//! | Grace period with no live flow | 30 s | yes |
//! | `shutdown` to `SIGTERM` | 5 s | no |
//! | `SIGTERM` to `SIGKILL` | 5 s | no |
//! | First wait before a relaunch | 1 s, doubled at each crash in a row | no |
//! | Longest wait before a relaunch | 30 s | no |
//! | Run after `ready` that resets the crash count | 60 s | no |
//! | Relaunches in a row before giving up | 8 (about 2 min of attempts) | no |
//!
//! Counting subscribers is the router's job: the supervisor asks it, and acts
//! on its empty flows.

mod config;
mod launcher;
mod state;
mod supervisor;

pub use config::Config;
pub use launcher::ProcessLauncher;
pub use supervisor::BrickSupervisor;
