//! Contract section 2, brick discovery, checked on a [`Discovery`] started on
//! temporary directories. Stopping a removed brick and ending its flows is the
//! supervisor's part, in [`crate::supervisor`].

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nowcast_traits::{Discovery, DiscoveryChange};
use nowcast_types::BrickName;
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio::time::timeout;

/// Where a discovery under test reads its declarations.
pub struct Dirs {
    /// Holds links to brick declaration files.
    pub bricks: PathBuf,
    /// Holds the user's client kind declarations.
    pub clients: PathBuf,
    /// The client kind declarations compiled in, as (file name, contents).
    pub embedded_clients: Vec<(String, String)>,
}

/// Starts a discovery, and tells where it looks by default.
pub trait DiscoverySubject {
    type Discovery: Discovery;

    /// Reads the directories and returns once it follows the bricks directory.
    fn start(dirs: Dirs) -> impl Future<Output = Self::Discovery>;

    /// The bricks directory for these values of `$XDG_CONFIG_HOME` and `$HOME`.
    fn bricks_dir(xdg_config_home: Option<&Path>, home: &Path) -> PathBuf;

    /// The client kinds directory for these values of `$XDG_CONFIG_HOME` and
    /// `$HOME`.
    fn clients_dir(xdg_config_home: Option<&Path>, home: &Path) -> PathBuf;
}

/// A temporary layout: a bricks directory, an empty clients directory, and a
/// copy of the manifest fixtures standing for the bricks' own repositories.
pub(crate) struct Layout {
    _tmp: TempDir,
    /// Canonical, so that it compares equal to paths with links resolved.
    root: PathBuf,
}

fn copy_files(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

impl Layout {
    pub(crate) fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for dir in ["bricks", "clients", "repo/schemas"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let fixtures = crate::manifest::dir();
        copy_files(&fixtures, &root.join("repo"));
        copy_files(&fixtures.join("schemas"), &root.join("repo/schemas"));
        Self { _tmp: tmp, root }
    }

    pub(crate) fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    pub(crate) fn dirs(&self, embedded_clients: Vec<(String, String)>) -> Dirs {
        Dirs {
            bricks: self.path("bricks"),
            clients: self.path("clients"),
            embedded_clients,
        }
    }

    /// Activates a fixture: links `bricks/<link>` to `repo/<fixture>`.
    fn activate(&self, fixture: &str, link: &str) {
        std::os::unix::fs::symlink(
            self.path("repo").join(fixture),
            self.path("bricks").join(link),
        )
        .unwrap();
    }
}

fn name(n: &str) -> BrickName {
    BrickName::new(n).unwrap()
}

fn names(discovery: &impl Discovery) -> Vec<String> {
    let mut names: Vec<_> = discovery
        .bricks()
        .iter()
        .map(|b| b.name.to_string())
        .collect();
    names.sort();
    names
}

async fn wait_for(changes: &mut broadcast::Receiver<DiscoveryChange>, expected: DiscoveryChange) {
    let found = timeout(Duration::from_secs(5), async {
        loop {
            match changes.recv().await {
                Ok(change) if change == expected => return,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => panic!("changes closed"),
            }
        }
    })
    .await;
    assert!(found.is_ok(), "no {expected:?} within 5 s");
}

/// The bricks directory is `$XDG_CONFIG_HOME/nowcast/bricks/`, by default
/// `~/.config/nowcast/bricks/`.
pub async fn bricks_dir_follows_xdg<S: DiscoverySubject>() {
    let home = Path::new("/home/user");
    assert_eq!(
        S::bricks_dir(Some(Path::new("/xdg")), home),
        Path::new("/xdg/nowcast/bricks")
    );
    assert_eq!(
        S::bricks_dir(None, home),
        Path::new("/home/user/.config/nowcast/bricks")
    );
}

/// A link to a declaration file in the bricks directory activates the brick.
pub async fn a_link_activates_a_brick<S: DiscoverySubject>() {
    let layout = Layout::new();
    layout.activate("forge.toml", "forge.toml");
    let discovery = S::start(layout.dirs(vec![])).await;
    assert_eq!(names(&discovery), vec!["forge"]);
    assert!(discovery.brick(&name("forge")).is_some());
    assert!(discovery.brick(&name("minimal")).is_none());
}

/// Relative paths resolve from the file's real location, link resolved: the
/// brick stays in its repository.
pub async fn paths_resolve_from_the_real_file<S: DiscoverySubject>() {
    let layout = Layout::new();
    layout.activate("forge.toml", "forge.toml");
    let discovery = S::start(layout.dirs(vec![])).await;
    let forge = discovery.brick(&name("forge")).unwrap();
    assert_eq!(
        Path::new(&forge.command[0]),
        layout.path("repo/bin/nowcast-forge")
    );
    assert_eq!(forge.dir, layout.path("repo"));
    let expected: serde_json::Value = serde_json::from_slice(
        &std::fs::read(layout.path("repo/schemas/subscription-pr.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(forge.subscriptions["pr"].schema, expected);
}

/// The directory is followed after startup: a new link adds the brick.
pub async fn a_new_link_is_followed<S: DiscoverySubject>() {
    let layout = Layout::new();
    let discovery = S::start(layout.dirs(vec![])).await;
    let mut changes = discovery.changes();
    assert!(names(&discovery).is_empty());
    layout.activate("forge.toml", "forge.toml");
    wait_for(&mut changes, DiscoveryChange::BrickAdded(name("forge"))).await;
    assert_eq!(names(&discovery), vec!["forge"]);
}

/// Removing the link deactivates the brick.
pub async fn removing_the_link_deactivates<S: DiscoverySubject>() {
    let layout = Layout::new();
    layout.activate("forge.toml", "forge.toml");
    let discovery = S::start(layout.dirs(vec![])).await;
    let mut changes = discovery.changes();
    std::fs::remove_file(layout.path("bricks/forge.toml")).unwrap();
    wait_for(&mut changes, DiscoveryChange::BrickRemoved(name("forge"))).await;
    assert!(discovery.brick(&name("forge")).is_none());
}

/// An invalid file is left out; the valid ones still load.
pub async fn an_invalid_file_is_ignored<S: DiscoverySubject>() {
    let layout = Layout::new();
    layout.activate("forge.toml", "forge.toml");
    layout.activate("invalid-undeclared-type.toml", "broken.toml");
    let discovery = S::start(layout.dirs(vec![])).await;
    assert_eq!(names(&discovery), vec!["forge"]);
}

/// Two files declaring the same `name` are both refused: neither wins.
pub async fn a_shared_name_refuses_both<S: DiscoverySubject>() {
    let layout = Layout::new();
    let copy = layout.path("repo/forge-copy.toml");
    std::fs::copy(layout.path("repo/forge.toml"), &copy).unwrap();
    layout.activate("forge.toml", "forge.toml");
    layout.activate("forge-copy.toml", "forge-copy.toml");
    layout.activate("minimal.toml", "minimal.toml");
    let discovery = S::start(layout.dirs(vec![])).await;
    assert_eq!(names(&discovery), vec!["minimal"]);
}

/// Runs the rules of contract section 2 against a discovery.
#[macro_export]
macro_rules! discovery_suite {
    ($subject:ty) => {
        mod discovery_suite {
            #[allow(unused_imports)]
            use super::*;
            $crate::__tests!(
                block_on,
                discovery,
                $subject,
                [
                    bricks_dir_follows_xdg,
                    a_link_activates_a_brick,
                    paths_resolve_from_the_real_file,
                    a_new_link_is_followed,
                    removing_the_link_deactivates,
                    an_invalid_file_is_ignored,
                    a_shared_name_refuses_both,
                ]
            );
        }
    };
}
