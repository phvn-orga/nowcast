//! Contract section 5, client kinds: their declarations, checked on a
//! [`DiscoverySubject`], and the attachment rule, checked on an
//! [`AttachmentSubject`]. The fixtures are in `fixtures/clients/`:
//! `claude-code.toml` is the embedded declaration, `user/` a user directory.

use std::path::{Path, PathBuf};

use nowcast_traits::{ClientKind, Discovery};
use nowcast_types::ClientKindName;

use crate::discovery::{DiscoverySubject, Layout};

/// `fixtures/clients/`.
pub fn dir() -> PathBuf {
    crate::fixtures().join("clients")
}

fn embedded() -> Vec<(String, String)> {
    let text = std::fs::read_to_string(dir().join("claude-code.toml")).unwrap();
    vec![("claude-code.toml".into(), text)]
}

fn kind(discovery: &impl Discovery, name: &str) -> Option<ClientKind> {
    discovery
        .client_kinds()
        .into_iter()
        .find(|k| k.name.as_str() == name)
}

/// Starts a discovery with the embedded declaration and, if `user`, the user
/// fixtures.
async fn start<S: DiscoverySubject>(user: bool) -> (Layout, S::Discovery) {
    let layout = Layout::new();
    if user {
        for entry in std::fs::read_dir(dir().join("user")).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), layout.path("clients").join(entry.file_name())).unwrap();
        }
    }
    let discovery = S::start(layout.dirs(embedded())).await;
    (layout, discovery)
}

/// The client kinds directory is `$XDG_CONFIG_HOME/nowcast/clients/`, by
/// default `~/.config/nowcast/clients/`.
pub async fn clients_dir_follows_xdg<S: DiscoverySubject>() {
    let home = Path::new("/home/user");
    assert_eq!(
        S::clients_dir(Some(Path::new("/xdg")), home),
        Path::new("/xdg/nowcast/clients")
    );
    assert_eq!(
        S::clients_dir(None, home),
        Path::new("/home/user/.config/nowcast/clients")
    );
}

/// An embedded declaration is recognized with no configuration, every field read.
pub async fn embedded_kinds_need_no_configuration<S: DiscoverySubject>() {
    let (_layout, discovery) = start::<S>(false).await;
    assert_eq!(
        discovery.client_kinds(),
        vec![ClientKind {
            name: ClientKindName::new("claude-code").unwrap(),
            description: "Claude Code sessions".into(),
            exe: vec!["*/claude/versions/*".into()],
        }]
    );
}

/// A user file with the name of an embedded declaration replaces it.
pub async fn a_user_file_replaces_the_embedded_one<S: DiscoverySubject>() {
    let (_layout, discovery) = start::<S>(true).await;
    let kinds = discovery.client_kinds();
    let claude: Vec<_> = kinds
        .iter()
        .filter(|k| k.name.as_str() == "claude-code")
        .collect();
    assert_eq!(claude.len(), 1, "one claude-code kind: {kinds:?}");
    assert_eq!(
        claude[0].exe,
        vec!["*/claude/versions/*", "*/claude-nightly/*"]
    );
}

/// A user file declares a kind no declaration embeds.
pub async fn a_user_file_adds_a_kind<S: DiscoverySubject>() {
    let (_layout, discovery) = start::<S>(true).await;
    let codex = kind(&discovery, "codex").expect("codex is declared");
    assert_eq!(codex.exe, vec!["*/bin/codex"]);
}

/// `name` equals the file name, and is `[a-z0-9-]+`: otherwise the file is
/// refused.
pub async fn a_bad_name_is_refused<S: DiscoverySubject>() {
    let (_layout, discovery) = start::<S>(true).await;
    assert!(kind(&discovery, "other").is_none(), "mismatched.toml");
    assert!(kind(&discovery, "mismatched").is_none(), "mismatched.toml");
    assert!(
        discovery
            .client_kinds()
            .iter()
            .all(|k| k.name.as_str() != "Bad"),
        "Bad.toml"
    );
}

/// Runs the declaration rules of contract section 5 against a discovery.
#[macro_export]
macro_rules! client_kind_suite {
    ($subject:ty) => {
        mod client_kind_suite {
            #[allow(unused_imports)]
            use super::*;
            $crate::__tests!(
                block_on,
                client_kind,
                $subject,
                [
                    clients_dir_follows_xdg,
                    embedded_kinds_need_no_configuration,
                    a_user_file_replaces_the_embedded_one,
                    a_user_file_adds_a_kind,
                    a_bad_name_is_refused,
                ]
            );
        }
    };
}

/// The attachment rule, apart from reading the process tree.
pub trait AttachmentSubject {
    /// `chain` holds executable paths: the connected process first, then its
    /// parent, and so on. Returns the anchor's index in `chain` and its kind,
    /// `None` when no process matched and the connected process anchors itself.
    fn attach(chain: &[PathBuf], kinds: &[ClientKind]) -> (usize, Option<ClientKindName>);
}

fn kinds() -> Vec<ClientKind> {
    let kind = |name: &str, exe: &str| ClientKind {
        name: ClientKindName::new(name).unwrap(),
        description: String::new(),
        exe: vec![exe.into()],
    };
    vec![
        kind("claude-code", "*/claude/versions/*"),
        kind("codex", "*/bin/codex"),
    ]
}

fn chain(paths: &[&str]) -> Vec<PathBuf> {
    paths.iter().map(PathBuf::from).collect()
}

fn named(n: &str) -> Option<ClientKindName> {
    Some(ClientKindName::new(n).unwrap())
}

const CLAUDE: &str = "/home/user/.local/share/claude/versions/2.1.0";

/// The nearest process whose executable matches, the connected one included,
/// becomes the anchor.
pub fn the_nearest_matching_ancestor_anchors<S: AttachmentSubject>() {
    let chain = chain(&["/usr/bin/curl", "/usr/bin/bash", CLAUDE, "/usr/bin/zsh"]);
    assert_eq!(S::attach(&chain, &kinds()), (2, named("claude-code")));
}

/// The nearest match wins whatever its kind.
pub fn the_nearest_match_wins_over_any_kind<S: AttachmentSubject>() {
    let chain = chain(&["/usr/bin/bash", "/usr/local/bin/codex", CLAUDE]);
    assert_eq!(S::attach(&chain, &kinds()), (1, named("codex")));
    let chain = self::chain(&["/usr/bin/bash", CLAUDE, "/usr/local/bin/codex"]);
    assert_eq!(S::attach(&chain, &kinds()), (1, named("claude-code")));
}

/// The connected process anchors itself when its own executable matches.
pub fn a_matching_process_anchors_itself<S: AttachmentSubject>() {
    let chain = chain(&[CLAUDE, "/usr/bin/zsh"]);
    assert_eq!(S::attach(&chain, &kinds()), (0, named("claude-code")));
}

/// With no matching ancestor, the connected process is the anchor, with no kind.
pub fn no_match_anchors_the_connected_process<S: AttachmentSubject>() {
    let chain = chain(&[
        "/usr/bin/nowcast",
        "/usr/bin/zsh",
        "/usr/lib/systemd/systemd",
    ]);
    assert_eq!(S::attach(&chain, &kinds()), (0, None));
}

/// Patterns are globs on the whole path: `*` also spans `/`, and several
/// versions match one pattern.
pub fn patterns_are_globs_on_the_whole_path<S: AttachmentSubject>() {
    for exe in [
        "/home/user/.local/share/claude/versions/2.1.0",
        "/opt/claude/versions/3.0.0-beta",
    ] {
        let chain = chain(&["/usr/bin/bash", exe]);
        assert_eq!(
            S::attach(&chain, &kinds()),
            (1, named("claude-code")),
            "{exe}"
        );
    }
    let chain = chain(&["/usr/bin/bash", "/home/user/claude/versions"]);
    assert_eq!(S::attach(&chain, &kinds()), (0, None), "no version segment");
}

/// Runs the attachment rule of contract section 5.
#[macro_export]
macro_rules! attachment_suite {
    ($subject:ty) => {
        mod attachment_suite {
            #[allow(unused_imports)]
            use super::*;
            $crate::__tests!(
                sync,
                client_kind,
                $subject,
                [
                    the_nearest_matching_ancestor_anchors,
                    the_nearest_match_wins_over_any_kind,
                    a_matching_process_anchors_itself,
                    no_match_anchors_the_connected_process,
                    patterns_are_globs_on_the_whole_path,
                ]
            );
        }
    };
}
