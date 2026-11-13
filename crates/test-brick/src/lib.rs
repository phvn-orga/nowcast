//! The test brick: a real brick process that speaks the brick protocol
//! (contract section 4) and publishes only when a test tells it to. It is the
//! real-process version of the conformance probe (`nowcast-conformance`,
//! module `probe`): name `probe`, one subscription kind `item` with params
//! `{"id": <string>}`, one published type `probe.item/v1`.
//!
//! # As a brick
//!
//! - `hello` is answered with `ready`, unless the mode says otherwise.
//! - `subscribe` of kind `item` maps to the flow named by `id`, verbatim: the
//!   same params give the same flow, across restarts. Unreadable params, a flow
//!   id over 256 bytes or another kind are answered with `error`, codes
//!   `invalid_params` and `unknown_kind`. `subscribe` of kind `inbox`, with
//!   the same params, maps to the flow `inbox/<id>`.
//! - `input` on a flow it serves publishes `{"id": <flow>, "input": <detail>}`.
//! - `refresh` of a flow it serves publishes `{"id": <flow>, "refresh": <n>}`,
//!   `n` counting the refreshes of this process from 1. A refresh of a flow it
//!   does not serve is ignored. Nothing is published otherwise, so a new flow
//!   has no value until a test publishes one.
//! - `refresh-scope` publishes nothing: a test reads the scope among what
//!   the service sent.
//! - `release` forgets the flow; `shutdown` or the end of its input ends the
//!   process with status 0.
//!
//! # Configuration
//!
//! Two environment variables, set in the service's environment or in the
//! `[run] env` of the declaration:
//!
//! - [`MODE_VAR`]: the [`Mode`], `ready` by default.
//! - [`CONTROL_VAR`]: the path of a Unix socket the test listens on with
//!   [`Control`]. Unset, the brick runs without control.
//!
//! # Control
//!
//! At start, the brick connects to the control socket, so each brick process
//! the service launches shows up as one [`ControlledBrick`], and its exit as
//! the end of that connection. Through it a test publishes anything (valid
//! JSON, invalid JSON, an undeclared type, an unknown flow) and gets the
//! service's answer, reports `flow-error`, reads what the service sent, or
//! makes the brick crash. The format is in [`wire`]; [`Control`] and
//! [`ControlledBrick`] are a client for it.
//!
//! [`manifest`] is the declaration file shipped with the crate. Its command
//! is the bare program name `nowcast-test-brick`, looked up in the service's
//! `PATH`. [`binary`] gives the path of the built program.
//!
//! # From a test
//!
//! A harness binds a [`Control`] in a temporary directory and launches the
//! brick with [`CONTROL_VAR`] set to its path: through the `env` of a
//! declaration built with the probe's (`probe::declaration`), or through the
//! service's own environment when the service loads [`manifest`] from a
//! bricks directory. Each [`Control::accept`] is then one launch of the
//! brick, and [`ControlledBrick::exited`] its end.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

mod control;
pub mod wire;

pub use control::{Control, ControlledBrick};

/// The brick's name, the conformance probe's.
pub const NAME: &str = "probe";
/// Its subscription kind without input.
pub const KIND: &str = "item";
/// Its subscription kind that accepts an input, with the same params. Its
/// flow is `inbox/<id>`.
pub const INBOX_KIND: &str = "inbox";
/// The type it publishes.
pub const TYPE: &str = "probe.item/v1";
/// The type `inbox` accepts: any JSON object.
pub const INPUT_TYPE: &str = "probe.input/v1";
/// The `error` code of every `subscribe` in [`Mode::Refusing`], the API
/// suite's `REFUSAL_CODE`.
pub const REFUSAL_CODE: &str = "no_such_item";

/// The environment variable holding the [`Mode`].
pub const MODE_VAR: &str = "NOWCAST_TEST_BRICK_MODE";
/// The environment variable holding the path of the control socket.
pub const CONTROL_VAR: &str = "NOWCAST_TEST_BRICK_CONTROL";

/// The exit status of a crash on demand.
pub const CRASH_STATUS: i32 = 101;
/// The exit status when the configuration is invalid or the control socket
/// cannot be reached.
pub const CONFIG_STATUS: i32 = 2;

/// The declaration file of the test brick, with its schemas next to it.
pub fn manifest() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/brick/probe.toml"))
}

/// Where Cargo puts the `nowcast-test-brick` binary for the running test: the
/// profile directory, parent of the `deps` directory holding the test
/// executable. It is the directory to put on the service's `PATH` for
/// [`manifest`], or the command of a declaration.
///
/// Cargo builds the binary only when it builds this package's own tests
/// (`cargo test --workspace` does) or on `cargo build -p nowcast-test-brick`:
/// a test of another package must make sure it exists and is up to date.
/// `None` when the running executable has no such parent.
pub fn binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let profile = exe.parent()?.parent()?;
    Some(profile.join(format!(
        "nowcast-test-brick{}",
        std::env::consts::EXE_SUFFIX
    )))
}

/// How the brick answers the service, fixed for the life of the process.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    /// Answers `hello` and serves every valid subscription.
    #[default]
    Ready,
    /// Never answers `hello`, so the service gives up on it.
    NeverReady,
    /// Answers every `subscribe` with `error`, code [`REFUSAL_CODE`].
    Refusing,
}

impl Mode {
    /// The value of [`MODE_VAR`] for this mode.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::NeverReady => "never-ready",
            Self::Refusing => "refusing",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A [`MODE_VAR`] value naming no mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownMode(pub String);

impl fmt::Display for UnknownMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown mode {:?}, expected ready, never-ready or refusing",
            self.0
        )
    }
}

impl std::error::Error for UnknownMode {}

impl FromStr for Mode {
    type Err = UnknownMode;

    fn from_str(s: &str) -> Result<Self, UnknownMode> {
        [Self::Ready, Self::NeverReady, Self::Refusing]
            .into_iter()
            .find(|mode| mode.as_str() == s)
            .ok_or_else(|| UnknownMode(s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_round_trip() {
        for mode in [Mode::Ready, Mode::NeverReady, Mode::Refusing] {
            assert_eq!(mode.as_str().parse(), Ok(mode));
        }
        assert_eq!("".parse::<Mode>(), Err(UnknownMode(String::new())));
        assert_eq!("Ready".parse::<Mode>(), Err(UnknownMode("Ready".into())));
    }

    #[test]
    fn the_manifest_ships_with_the_crate() {
        assert!(manifest().is_file());
    }
}
