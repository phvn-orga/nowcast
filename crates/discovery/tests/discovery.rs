//! Following the directories, beyond the conformance suites.

use std::path::{Path, PathBuf};
use std::time::Duration;

use nowcast_discovery::{DiscoveryConfig, FsDiscovery};
use nowcast_traits::{Discovery, DiscoveryChange};
use nowcast_types::BrickName;
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio::time::timeout;

/// A bricks directory, a clients directory, and the manifest fixtures copied
/// into `repo/` as a brick repository.
struct Layout {
    _tmp: TempDir,
    root: PathBuf,
}

impl Layout {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let fixtures = nowcast_conformance::manifest::dir();
        for dir in ["repo/schemas", "bricks", "clients"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        copy(&fixtures, &root.join("repo"));
        copy(&fixtures.join("schemas"), &root.join("repo/schemas"));
        Self { _tmp: tmp, root }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn config(&self, embedded_clients: &[(&str, &str)]) -> DiscoveryConfig {
        DiscoveryConfig {
            bricks: self.path("bricks"),
            clients: self.path("clients"),
            embedded_clients: embedded_clients
                .iter()
                .map(|&(f, t)| (f.to_owned(), t.to_owned()))
                .collect(),
        }
    }

    fn link(&self, fixture: &str, link: &str) {
        std::os::unix::fs::symlink(
            self.path("repo").join(fixture),
            self.path("bricks").join(link),
        )
        .unwrap();
    }

    /// Replaces `bricks/<link>` atomically with a link to `fixture`.
    fn retarget(&self, fixture: &str, link: &str) {
        let tmp = self.path("bricks/.tmp-link");
        std::os::unix::fs::symlink(self.path("repo").join(fixture), &tmp).unwrap();
        std::fs::rename(&tmp, self.path("bricks").join(link)).unwrap();
    }
}

fn copy(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

fn name(n: &str) -> BrickName {
    BrickName::new(n).unwrap()
}

fn names(discovery: &FsDiscovery) -> Vec<String> {
    discovery
        .bricks()
        .iter()
        .map(|b| b.name.to_string())
        .collect()
}

async fn next(changes: &mut broadcast::Receiver<DiscoveryChange>) -> DiscoveryChange {
    timeout(Duration::from_secs(5), changes.recv())
        .await
        .expect("a change within 5 s")
        .expect("the changes channel is open")
}

/// Polls until `done` holds, for at most 5 s.
async fn eventually(mut done: impl FnMut() -> bool) {
    let polled = timeout(Duration::from_secs(5), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(polled.is_ok(), "not reached within 5 s");
}

const CLAUDE: &str = r#"
name = "claude-code"
description = "Claude Code sessions"
[anchor]
exe = ["*/claude/versions/*"]
"#;

#[tokio::test]
async fn retargeting_a_link_is_a_change() {
    let layout = Layout::new();
    layout.link("forge.toml", "forge.toml");
    let discovery = FsDiscovery::start(layout.config(&[])).unwrap();
    let mut changes = discovery.changes();
    std::fs::write(
        layout.path("repo/forge2.toml"),
        std::fs::read_to_string(layout.path("repo/forge.toml"))
            .unwrap()
            .replace("0.1.0", "0.2.0"),
    )
    .unwrap();
    layout.retarget("forge2.toml", "forge.toml");
    assert_eq!(
        next(&mut changes).await,
        DiscoveryChange::BrickChanged(name("forge"))
    );
    assert_eq!(discovery.brick(&name("forge")).unwrap().version, "0.2.0");
}

#[tokio::test]
async fn a_link_turned_invalid_removes_the_brick() {
    let layout = Layout::new();
    layout.link("forge.toml", "forge.toml");
    let discovery = FsDiscovery::start(layout.config(&[])).unwrap();
    let mut changes = discovery.changes();
    layout.retarget("invalid-undeclared-type.toml", "forge.toml");
    assert_eq!(
        next(&mut changes).await,
        DiscoveryChange::BrickRemoved(name("forge"))
    );
    assert!(names(&discovery).is_empty());
}

#[tokio::test]
async fn an_unrelated_entry_announces_nothing() {
    let layout = Layout::new();
    layout.link("forge.toml", "forge.toml");
    let discovery = FsDiscovery::start(layout.config(&[])).unwrap();
    let mut changes = discovery.changes();
    std::fs::write(layout.path("bricks/README"), "not a declaration").unwrap();
    std::os::unix::fs::symlink(
        layout.path("repo/missing.toml"),
        layout.path("bricks/gone.toml"),
    )
    .unwrap();
    layout.link("minimal.toml", "minimal.toml");
    // The first change announced is the valid link added last.
    assert_eq!(
        next(&mut changes).await,
        DiscoveryChange::BrickAdded(name("minimal"))
    );
    assert_eq!(names(&discovery), ["forge", "minimal"]);
}

#[tokio::test]
async fn a_plain_file_activates_a_brick_too() {
    let layout = Layout::new();
    std::fs::write(
        layout.path("bricks/bare.toml"),
        "manifest_version = 1\nname = \"bare\"\nversion = \"0.1.0\"\n\
         description = \"A brick.\"\nprotocol = 1\nrun = { command = [\"b\"] }\n",
    )
    .unwrap();
    let discovery = FsDiscovery::start(layout.config(&[])).unwrap();
    let bare = discovery.brick(&name("bare")).expect("a plain file loads");
    assert_eq!(bare.dir, layout.path("bricks"));
}

#[tokio::test]
async fn missing_directories_are_created() {
    let tmp = tempfile::tempdir().unwrap();
    let config = DiscoveryConfig {
        bricks: tmp.path().join("nowcast/bricks"),
        clients: tmp.path().join("nowcast/clients"),
        embedded_clients: vec![],
    };
    let discovery = FsDiscovery::start(config.clone()).unwrap();
    assert!(config.bricks.is_dir() && config.clients.is_dir());
    assert!(discovery.bricks().is_empty());
}

#[tokio::test]
async fn the_clients_directory_is_followed() {
    let layout = Layout::new();
    let discovery = FsDiscovery::start(layout.config(&[("claude-code.toml", CLAUDE)])).unwrap();
    std::fs::write(
        layout.path("clients/claude-code.toml"),
        CLAUDE.replace("*/claude/versions/*", "*/claude-beta/*"),
    )
    .unwrap();
    eventually(|| discovery.client_kinds()[0].exe == ["*/claude-beta/*"]).await;
    std::fs::remove_file(layout.path("clients/claude-code.toml")).unwrap();
    eventually(|| discovery.client_kinds()[0].exe == ["*/claude/versions/*"]).await;
}

#[tokio::test]
async fn an_empty_pattern_list_switches_an_embedded_kind_off() {
    let layout = Layout::new();
    std::fs::write(
        layout.path("clients/claude-code.toml"),
        "name = \"claude-code\"\ndescription = \"Off\"\n[anchor]\nexe = []\n",
    )
    .unwrap();
    let discovery = FsDiscovery::start(layout.config(&[("claude-code.toml", CLAUDE)])).unwrap();
    let kinds = discovery.client_kinds();
    assert_eq!(kinds.len(), 1);
    assert!(kinds[0].exe.is_empty());
}

#[tokio::test]
async fn a_kind_embedded_twice_is_refused() {
    let layout = Layout::new();
    let codex = "name = \"codex\"\ndescription = \"Codex\"\n[anchor]\nexe = [\"*/codex\"]\n";
    let discovery = FsDiscovery::start(layout.config(&[
        ("claude-code.toml", CLAUDE),
        ("codex.toml", codex),
        ("claude-code.toml", CLAUDE),
    ]))
    .unwrap();
    let kinds: Vec<_> = discovery
        .client_kinds()
        .iter()
        .map(|k| k.name.to_string())
        .collect();
    assert_eq!(kinds, ["codex"]);
}

#[tokio::test]
async fn embedded_kinds_come_first() {
    let layout = Layout::new();
    let aaa = "name = \"aaa\"\ndescription = \"A\"\n[anchor]\nexe = [\"*/aaa\"]\n";
    std::fs::write(layout.path("clients/aaa.toml"), aaa).unwrap();
    let discovery = FsDiscovery::start(layout.config(&[("claude-code.toml", CLAUDE)])).unwrap();
    let kinds: Vec<_> = discovery
        .client_kinds()
        .iter()
        .map(|k| k.name.to_string())
        .collect();
    assert_eq!(kinds, ["claude-code", "aaa"]);
}

#[test]
fn every_embedded_declaration_is_valid() {
    for (file, text) in nowcast_discovery::EMBEDDED_CLIENTS {
        nowcast_discovery::parse_client_kind(file, text)
            .unwrap_or_else(|e| panic!("embedded {file} is refused: {e}"));
    }
}
