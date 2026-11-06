//! The contract's conformance suites for sections 1, 2 and 5.

use std::path::{Path, PathBuf};

use nowcast_conformance::discovery::{Dirs, DiscoverySubject};
use nowcast_conformance::manifest::ManifestSubject;
use nowcast_discovery::{DiscoveryConfig, FsDiscovery, bricks_dir, clients_dir, load_manifest};
use nowcast_traits::BrickDeclaration;

struct Subject;

impl ManifestSubject for Subject {
    fn load(path: &Path) -> Result<BrickDeclaration, String> {
        load_manifest(path).map_err(|e| e.to_string())
    }
}

impl DiscoverySubject for Subject {
    type Discovery = FsDiscovery;

    async fn start(dirs: Dirs) -> FsDiscovery {
        FsDiscovery::start(DiscoveryConfig {
            bricks: dirs.bricks,
            clients: dirs.clients,
            embedded_clients: dirs.embedded_clients,
        })
        .expect("the discovery starts")
    }

    fn bricks_dir(xdg_config_home: Option<&Path>, home: &Path) -> PathBuf {
        bricks_dir(xdg_config_home, home)
    }

    fn clients_dir(xdg_config_home: Option<&Path>, home: &Path) -> PathBuf {
        clients_dir(xdg_config_home, home)
    }
}

nowcast_conformance::manifest_suite!(Subject);
nowcast_conformance::discovery_suite!(Subject);
nowcast_conformance::client_kind_suite!(Subject);
