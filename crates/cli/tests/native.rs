//! The native bricks the binary carries: `nowcast brick <name>` against the
//! brick side of the protocol, and `nowcast bricks enable|disable`, which
//! looks a brick's program up in the service's `PATH`.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::{Anchoring, Fake, bare, nowcast, run, stderr};
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

struct Forge;

impl BrickSubject for Forge {
    fn command() -> std::process::Command {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_nowcast"));
        // Nothing listens there: every reading publishes `unreachable`.
        command
            .args(["brick", "forge"])
            .env("NOWCAST_FORGE_GITHUB_API", "http://127.0.0.1:9")
            .env("NOWCAST_FORGE_GITHUB_TOKEN", "t");
        command
    }

    fn name() -> BrickName {
        BrickName::new("forge").unwrap()
    }

    fn subscription() -> (String, Vec<u8>) {
        (
            "pr".into(),
            br#"{"remote":"https://github.com/o/r.git","branch":"main"}"#.to_vec(),
        )
    }
}

mod forge {
    use super::*;
    nowcast_conformance::brick_suite!(Forge);
}

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

/// `nowcast bricks` against `fake`, a service whose `PATH` is `path`.
fn bricks_with_path(fake: &Fake, path: &Path, root: &Path) -> tokio::process::Command {
    let body = serde_json::json!({ "path": path }).to_string();
    fake.can("/v1/service", 200, "application/json", &body);
    let mut command = nowcast(fake);
    command
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .arg("bricks");
    command
}

fn probe_link(root: &Path) -> std::path::PathBuf {
    root.join("config/nowcast/bricks/probe.toml")
}

#[tokio::test]
async fn a_program_missing_from_the_services_path_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let fake = Fake::start(Anchoring::PerProcess).await;
    let empty = root.path().join("bin");
    std::fs::create_dir(&empty).unwrap();
    let probe = nowcast_test_brick_manifest();
    let mut command = bricks_with_path(&fake, &empty, root.path());
    let output = run(&mut command, &["enable", probe.to_str().unwrap()]).await;
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let message = stderr(&output);
    assert!(
        message.contains(&format!(
            "program \"nowcast-test-brick\" not found in the service's PATH ({})",
            empty.display()
        )),
        "{message}"
    );
    assert!(message.contains("--force"), "{message}");
    assert!(std::fs::symlink_metadata(probe_link(root.path())).is_err());

    let mut command = bricks_with_path(&fake, &empty, root.path());
    let forced = run(
        &mut command,
        &["enable", "--force", probe.to_str().unwrap()],
    )
    .await;
    assert_eq!(forced.status.code(), Some(0), "{forced:?}");
    assert!(probe_link(root.path()).is_symlink());
}

#[tokio::test]
async fn a_program_on_the_services_path_is_enabled() {
    let root = tempfile::tempdir().unwrap();
    let fake = Fake::start(Anchoring::PerProcess).await;
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let program = bin.join("nowcast-test-brick");
    std::fs::write(&program, "").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let probe = nowcast_test_brick_manifest();
    let mut command = bricks_with_path(&fake, &bin, root.path());
    let output = run(&mut command, &["enable", probe.to_str().unwrap()]).await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(stderr(&output), "");
    assert!(probe_link(root.path()).is_symlink());
}

#[tokio::test]
async fn a_service_out_of_reach_leaves_the_program_unchecked() {
    let root = tempfile::tempdir().unwrap();
    let fake = Fake::absent(Anchoring::PerProcess);
    let probe = nowcast_test_brick_manifest();
    let mut command = nowcast(&fake);
    command
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_DATA_HOME", root.path().join("data"))
        .arg("bricks");
    let output = run(&mut command, &["enable", probe.to_str().unwrap()]).await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let message = stderr(&output);
    assert!(
        message.contains("\"nowcast-test-brick\" not looked for in the service's PATH"),
        "{message}"
    );
    assert!(probe_link(root.path()).is_symlink());
}

/// The test brick's declaration, shipped with its crate.
fn nowcast_test_brick_manifest() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-brick/brick/probe.toml")
}
