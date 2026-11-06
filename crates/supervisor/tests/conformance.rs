//! The supervisor passes the service-side rules of contract section 4.

use std::sync::Arc;

use nowcast_conformance::supervisor::SupervisorSubject;
use nowcast_supervisor::{BrickSupervisor, Config};
use nowcast_traits::fake::{FakeDiscovery, FakeLauncher, FakeRouter};

struct Subject;

impl SupervisorSubject for Subject {
    type Supervisor = BrickSupervisor<FakeDiscovery, FakeLauncher, FakeRouter>;
    fn start(
        discovery: Arc<FakeDiscovery>,
        launcher: Arc<FakeLauncher>,
        router: Arc<FakeRouter>,
    ) -> Self::Supervisor {
        BrickSupervisor::start(discovery, launcher, router, Config::default())
    }
}

nowcast_conformance::supervisor_suite!(Subject);
