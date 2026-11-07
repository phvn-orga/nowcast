//! The native bricks the binary carries: `nowcast brick <name>` against the
//! brick side of the protocol, and `nowcast bricks enable|disable`.

mod common;

use std::path::Path;

use common::{bare, run};
use nowcast_conformance::brick::BrickSubject;
use nowcast_types::BrickName;

struct ClaudeCode;

impl BrickSubject for ClaudeCode {
    fn command() -> std::process::Command {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_nowcast"));
        // No Claude Code files: the suite never subscribes to the quotas.
        command
            .args(["brick", "claude-code"])
            .env("HOME", "/nonexistent");
        command
    }

    fn name() -> BrickName {
        BrickName::new("claude-code").unwrap()
    }

    fn subscription() -> (String, Vec<u8>) {
        ("sessions".into(), b"{}".to_vec())
    }
}

nowcast_conformance::brick_suite!(ClaudeCode);

fn bricks(config: &Path, data: &Path) -> tokio::process::Command {
    let mut command = bare();
    command
        .env("XDG_CONFIG_HOME", config)
        .env("XDG_DATA_HOME", data)
        .arg("bricks");
    command
}

#[tokio::test]
async fn a_native_brick_is_enabled_by_name_and_disabled() {
    let root = tempfile::tempdir().unwrap();
    let (config, data) = (root.path().join("config"), root.path().join("data"));
    let enabled = run(&mut bricks(&config, &data), &["enable", "claude-code"]).await;
    assert_eq!(enabled.status.code(), Some(0), "{enabled:?}");
    let link = config.join("nowcast/bricks/claude-code.toml");
    let declaration = data.join("nowcast/bricks/claude-code/brick.toml");
    assert_eq!(std::fs::read_link(&link).unwrap(), declaration);
    assert!(
        data.join("nowcast/bricks/claude-code/schemas/measure-v1.json")
            .is_file()
    );

    let disabled = run(&mut bricks(&config, &data), &["disable", "claude-code"]).await;
    assert_eq!(disabled.status.code(), Some(0), "{disabled:?}");
    assert!(std::fs::symlink_metadata(&link).is_err());
    // Disabling again is no error.
    let again = run(&mut bricks(&config, &data), &["disable", "claude-code"]).await;
    assert_eq!(again.status.code(), Some(0), "{again:?}");
}

#[tokio::test]
async fn any_declaration_is_enabled_by_path() {
    let root = tempfile::tempdir().unwrap();
    let (config, data) = (root.path().join("config"), root.path().join("data"));
    let probe = nowcast_test_brick_manifest();
    let enabled = run(
        &mut bricks(&config, &data),
        &["enable", probe.to_str().unwrap()],
    )
    .await;
    assert_eq!(enabled.status.code(), Some(0), "{enabled:?}");
    let link = config.join("nowcast/bricks/probe.toml");
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        probe.canonicalize().unwrap()
    );
}

#[tokio::test]
async fn an_unknown_native_brick_exits_2() {
    let root = tempfile::tempdir().unwrap();
    let (config, data) = (root.path().join("config"), root.path().join("data"));
    let output = run(&mut bricks(&config, &data), &["enable", "nope"]).await;
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let output = run(&mut bare(), &["brick", "nope"]).await;
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}

/// The test brick's declaration, shipped with its crate.
fn nowcast_test_brick_manifest() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-brick/brick/probe.toml")
}
