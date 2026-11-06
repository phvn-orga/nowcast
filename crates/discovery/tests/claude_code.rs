//! The Claude Code declaration the repository ships,
//! `clients/claude-code/claude-code.toml`: compiled in, and anchoring a
//! Claude Code session on the Claude Code process wherever it is installed.

use std::path::Path;

use nowcast_discovery::{EMBEDDED_CLIENTS, parse_client_kind};
use nowcast_identity::KindMatcher;
use nowcast_traits::ClientKind;

fn embedded() -> Vec<ClientKind> {
    EMBEDDED_CLIENTS
        .iter()
        .map(|(file, text)| parse_client_kind(file, text).unwrap())
        .collect()
}

/// Where the Claude Code executable lies, by way of installing it.
const INSTALLS: &[&str] = &[
    // The native installer.
    "/home/user/.local/share/claude/versions/2.1.319",
    // npm, globally with a user prefix, globally, and in a project.
    "/home/user/.npm-global/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe",
    "/usr/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe",
    "/home/user/project/node_modules/@anthropic-ai/claude-code/bin/claude.exe",
    // npm, through the package's `cli-wrapper.cjs` fallback.
    "/usr/lib/node_modules/@anthropic-ai/claude-code-linux-x64/claude",
    "/home/user/project/node_modules/@anthropic-ai/claude-code-linux-arm64-musl/claude",
    // The apt, dnf and apk packages.
    "/usr/bin/claude",
];

/// The anchor `chain` gets, as the index of the matching process and its kind.
fn anchor(matcher: &KindMatcher, chain: &[&str]) -> (usize, Option<String>) {
    let ((index, _), kind) = matcher
        .attach(chain.iter().enumerate(), |(_, exe)| Some(Path::new(exe)))
        .unwrap();
    (index, kind.map(|k| k.to_string()))
}

#[test]
fn the_claude_code_declaration_is_embedded() {
    let names: Vec<_> = embedded().iter().map(|k| k.name.to_string()).collect();
    assert_eq!(names, ["claude-code"]);
}

/// A mod's request leaves from the Claude Code process itself, and the
/// stream from `nowcast watch`, its child: both anchor on Claude Code.
#[test]
fn a_mod_request_and_the_stream_anchor_on_claude_code() {
    let matcher = KindMatcher::new(&embedded());
    let claude_code = Some("claude-code".to_owned());
    for exe in INSTALLS {
        let request = [exe, "/usr/bin/zsh", "/usr/lib/systemd/systemd"];
        let stream = [
            "/home/user/.cargo/bin/nowcast",
            exe,
            "/usr/bin/zsh",
            "/usr/lib/systemd/systemd",
        ];
        assert_eq!(
            anchor(&matcher, &request),
            (0, claude_code.clone()),
            "{exe}"
        );
        assert_eq!(anchor(&matcher, &stream), (1, claude_code.clone()), "{exe}");
    }
}

#[test]
fn other_programs_are_not_claude_code() {
    let matcher = KindMatcher::new(&embedded());
    for exe in [
        "/usr/bin/node",
        "/usr/bin/bash",
        "/home/user/.cargo/bin/nowcast",
        "/home/user/.local/bin/claude-helper",
        "/home/user/claude/versions",
        "/usr/lib/node_modules/@anthropic-ai/claude-code/cli-wrapper.cjs",
    ] {
        assert_eq!(anchor(&matcher, &[exe, "/usr/bin/zsh"]), (0, None), "{exe}");
    }
}
