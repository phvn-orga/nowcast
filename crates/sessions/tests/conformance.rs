//! The registry passes the session rules of contract section 3.

use std::sync::{Arc, LazyLock};

use nowcast_conformance::sessions::SessionsSubject;
use nowcast_sessions::SessionRegistry;
use nowcast_traits::fake::{FakeIdentity, FakeRouter};
use tokio::runtime::Runtime;

/// The suite runs without a runtime; the registry's task needs one to start.
static RUNTIME: LazyLock<Runtime> =
    LazyLock::new(|| Runtime::new().expect("a runtime for the registry's task"));

struct Subject;

impl SessionsSubject for Subject {
    type Sessions = SessionRegistry;
    fn new() -> SessionRegistry {
        let _runtime = RUNTIME.enter();
        SessionRegistry::new(Arc::new(FakeIdentity::new()), Arc::new(FakeRouter::new()))
    }
}

nowcast_conformance::sessions_suite!(Subject);
