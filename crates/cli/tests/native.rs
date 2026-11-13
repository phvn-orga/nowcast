//! The native bricks the binary carries: `nowcast brick <name>` against the
//! brick side of the protocol, `nowcast bricks enable|disable`, which looks
//! a brick's program up in the service's `PATH`, and the configuration
//! command a brick declares, which `enable` and `bricks configure` run.

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

    fn scope() -> Option<Vec<u8>> {
        Some(br#"{"remote":"git@github.com:o/r.git","types":["pr"]}"#.to_vec())
    }
}

mod forge {
    use super::*;
    nowcast_conformance::brick_suite!(Forge);
}

const WAIT: std::time::Duration = std::time::Duration::from_secs(10);

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
    let output = run(&mut bare(), &["brick", "claude-code", "configure"]).await;
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

/// A brick declared in `root/brick/`, whose configuration command writes
/// its arguments, and whether its stdin is a terminal, into `configured`
/// beside the declaration, then exits with `exit`.
fn configured_brick(root: &Path, exit: u8) -> std::path::PathBuf {
    let dir = root.join("brick");
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("configure");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ -t 0 ]; then tty=tty; else tty=none; fi\n\
             echo \"$tty $*\" > configured\nexit {exit}\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let declaration = dir.join("brick.toml");
    std::fs::write(
        &declaration,
        r#"
manifest_version = 1
name = "configured"
version = "0.1.0"
description = "A brick with a configuration."
protocol = 1

[run]
command = ["./brick"]

[configure]
command = ["./configure", "a"]
"#,
    )
    .unwrap();
    declaration
}

/// What the configuration command wrote, `None` when it did not run.
fn configured(root: &Path) -> Option<String> {
    std::fs::read_to_string(root.join("brick/configured")).ok()
}

fn configured_link(root: &Path) -> std::path::PathBuf {
    root.join("config/nowcast/bricks/configured.toml")
}

/// A pseudo-terminal: its controlling side, kept open while the command
/// runs, and the terminal to give the command. Both close on exec: only the
/// command's standard streams hold the terminal.
fn terminal() -> (rustix::fd::OwnedFd, rustix::fd::OwnedFd) {
    use rustix::fs::{Mode, OFlags};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let name = ptsname(&master, Vec::new()).unwrap();
    let slave = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    (master, slave)
}

/// Which of a command's standard streams are a terminal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tty {
    None,
    Stdin,
    /// Stdin and stderr, as the configuration prompts need.
    Both,
}

/// `nowcast bricks <args>` with `root`'s directories, its streams in a
/// terminal as `tty` says. A stderr in the terminal is read back from it
/// into the output, its line endings as written.
async fn bricks_in(root: &Path, tty: Tty, args: &[&str]) -> std::process::Output {
    let mut command = bricks(&root.join("config"), &root.join("data"));
    let master = match tty {
        Tty::None => None,
        Tty::Stdin | Tty::Both => {
            let (master, slave) = terminal();
            if tty == Tty::Both {
                command.stderr(slave.try_clone().unwrap());
            }
            command.stdin(slave);
            Some(master)
        }
    };
    // Not `run`: `output` would pipe stderr whatever was set.
    let pipe = std::process::Stdio::piped;
    command.args(args).stdout(pipe());
    if tty != Tty::Both {
        command.stderr(pipe());
    }
    let child = command.spawn().unwrap();
    let mut output = tokio::time::timeout(WAIT, child.wait_with_output())
        .await
        .expect("the command ends")
        .unwrap();
    // The command holds the parent's copy of the terminal: once dropped,
    // the terminal reads what was written, then ends.
    drop(command);
    if let (Some(master), Tty::Both) = (master, tty) {
        let read = tokio::task::spawn_blocking(move || {
            let mut written = Vec::new();
            let mut buf = [0; 4096];
            while let Ok(n @ 1..) = rustix::io::read(&master, &mut buf) {
                written.extend_from_slice(&buf[..n]);
            }
            written
        });
        let written = tokio::time::timeout(WAIT, read)
            .await
            .expect("the terminal ends")
            .unwrap();
        output.stderr = String::from_utf8(written)
            .unwrap()
            .replace("\r\n", "\n")
            .into();
    }
    output
}

#[tokio::test]
async fn enable_runs_the_configuration_in_the_terminal_first() {
    let root = tempfile::tempdir().unwrap();
    let declaration = configured_brick(root.path(), 0);
    let output = bricks_in(
        root.path(),
        Tty::Both,
        &["enable", declaration.to_str().unwrap()],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(configured(root.path()).as_deref(), Some("tty a\n"));
    assert!(configured_link(root.path()).is_symlink());
}

#[tokio::test]
async fn a_failed_configuration_refuses_the_brick_unless_forced() {
    let root = tempfile::tempdir().unwrap();
    let declaration = configured_brick(root.path(), 3);
    let path = declaration.to_str().unwrap();
    let output = bricks_in(root.path(), Tty::Both, &["enable", path]).await;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let message = stderr(&output);
    assert!(
        message.contains("configure\" failed (exit status: 3)"),
        "{message}"
    );
    assert!(message.contains("--force"), "{message}");
    assert!(std::fs::symlink_metadata(configured_link(root.path())).is_err());

    let forced = bricks_in(root.path(), Tty::Both, &["enable", "--force", path]).await;
    assert_eq!(forced.status.code(), Some(0), "{forced:?}");
    assert!(
        stderr(&forced).contains("enabled all the same"),
        "{forced:?}"
    );
    assert!(configured_link(root.path()).is_symlink());
}

/// Without a terminal on stdin, or on stderr where the prompts draw.
#[tokio::test]
async fn without_a_terminal_the_brick_is_enabled_unconfigured() {
    for tty in [Tty::None, Tty::Stdin] {
        let root = tempfile::tempdir().unwrap();
        let declaration = configured_brick(root.path(), 0);
        let output = bricks_in(root.path(), tty, &["enable", declaration.to_str().unwrap()]).await;
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert_eq!(configured(root.path()), None);
        let message = stderr(&output);
        assert!(
            message.contains("configured enabled without its configuration"),
            "{message}"
        );
        assert!(
            message.contains("`nowcast bricks configure configured`"),
            "{message}"
        );
        assert!(configured_link(root.path()).is_symlink());
    }
}

#[tokio::test]
async fn bricks_configure_runs_the_configuration_again() {
    let root = tempfile::tempdir().unwrap();
    let declaration = configured_brick(root.path(), 0);
    let enabled = bricks_in(
        root.path(),
        Tty::None,
        &["enable", declaration.to_str().unwrap()],
    )
    .await;
    assert_eq!(enabled.status.code(), Some(0), "{enabled:?}");
    let output = bricks_in(root.path(), Tty::Stdin, &["configure", "configured"]).await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(configured(root.path()).as_deref(), Some("tty a\n"));

    configured_brick(root.path(), 5);
    let failed = bricks_in(root.path(), Tty::Stdin, &["configure", "configured"]).await;
    assert_eq!(failed.status.code(), Some(1), "{failed:?}");
    assert!(stderr(&failed).contains("exit status: 5"), "{failed:?}");
}

#[tokio::test]
async fn a_brick_without_configuration_says_so() {
    let root = tempfile::tempdir().unwrap();
    let output = bricks_in(root.path(), Tty::Stdin, &["configure", "claude-code"]).await;
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        stderr(&output).contains("claude-code has nothing to configure"),
        "{output:?}"
    );
    let output = bricks_in(root.path(), Tty::Stdin, &["configure", "nope"]).await;
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}
