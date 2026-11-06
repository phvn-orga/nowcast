//! Reusable conformance suites, one per contract of `docs/design/03-contrats.md`
//! (design branch).
//!
//! Each suite is a macro that a module calls from its own tests with a subject:
//! a type implementing the suite's `*Subject` trait. It expands to one `#[test]`
//! per rule of the contract, inside a module named after the suite. Nothing
//! runs until a module calls it:
//!
//! ```text
//! struct Subject;
//! impl nowcast_conformance::router::RouterSubject for Subject { ... }
//! nowcast_conformance::router_suite!(Subject);
//! ```
//!
//! | Contract section | Suite | Subject |
//! |---|---|---|
//! | 1. Brick declaration file | [`manifest_suite!`] | [`manifest::ManifestSubject`] |
//! | 2. Brick discovery | [`discovery_suite!`] | [`discovery::DiscoverySubject`] |
//! | 3. Client API, router rules | [`router_suite!`] | [`router::RouterSubject`] |
//! | 3. Client API, sessions | [`sessions_suite!`] | [`sessions::SessionsSubject`] |
//! | 3. Client API, over HTTP | [`api_suite!`] | [`api::ApiSubject`] |
//! | 4. Brick protocol, frames | [`codec_suite!`] | [`codec::CodecSubject`] |
//! | 4. Brick protocol, brick side | [`brick_suite!`] | [`brick::BrickSubject`] |
//! | 4. Brick protocol, service side | [`supervisor_suite!`] | [`supervisor::SupervisorSubject`] |
//! | 5. Client kind declaration | [`client_kind_suite!`] | [`discovery::DiscoverySubject`] |
//! | 5. Client kind attachment rule | [`attachment_suite!`] | [`client_kind::AttachmentSubject`] |
//!
//! Each test names the rule it checks in its doc comment. When the contract
//! changes, the suites change with it.

use std::future::Future;
use std::path::PathBuf;

pub mod api;
pub mod brick;
pub mod client_kind;
pub mod codec;
pub mod discovery;
pub mod manifest;
pub mod probe;
pub mod reference;
pub mod router;
pub mod sessions;
pub mod supervisor;

/// The fixtures shipped with this crate: declaration files and schemas. The
/// path is canonical, so that it compares equal to a path with links resolved.
pub fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .canonicalize()
        .expect("the fixtures directory exists")
}

/// Runs an async test on a fresh runtime.
#[doc(hidden)]
pub fn block_on<F: Future>(test: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a test runtime")
        .block_on(test)
}

/// Runs an async test on a runtime whose clock is paused: sleeps advance it
/// instantly once every task is idle.
#[doc(hidden)]
pub fn block_on_paused<F: Future>(test: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("a test runtime")
        .block_on(test)
}

/// Expands to one `#[test]` per listed rule function of a suite module.
#[doc(hidden)]
#[macro_export]
macro_rules! __tests {
    (sync, $module:ident, $subject:ty, [$($test:ident),+ $(,)?]) => {
        $(
            #[test]
            fn $test() {
                $crate::$module::$test::<$subject>()
            }
        )+
    };
    ($runner:ident, $module:ident, $subject:ty, [$($test:ident),+ $(,)?]) => {
        $(
            #[test]
            fn $test() {
                $crate::$runner($crate::$module::$test::<$subject>())
            }
        )+
    };
}
