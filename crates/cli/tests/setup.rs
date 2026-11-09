//! `nowcast setup` and `nowcast setup --remove`, against a `systemctl` that
//! records its calls, with no terminal to ask in.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::{bare, run, stderr};

struct Home {
    root: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        let home = Self {
            root: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(home.bin()).unwrap();
        home.systemctl("exit 0");
        home
    }

    fn bin(&self) -> PathBuf {
        self.root.path().join("bin")
    }

    fn units(&self) -> PathBuf {
        self.root.path().join("config/systemd/user")
    }

    fn brick_link(&self, name: &str) -> PathBuf {
        self.root
            .path()
            .join(format!("config/nowcast/bricks/{name}.toml"))
    }

    /// Installs a `systemctl` that logs its arguments, then runs `then`.
    fn systemctl(&self, then: &str) {
        let log = self.root.path().join("systemctl.log");
        let script = format!("#!/bin/sh\necho \"$*\" >> '{}'\n{then}\n", log.display());
        let path = self.bin().join("systemctl");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The calls to `systemctl` since the last one, and forgets them.
    fn calls(&self) -> Vec<String> {
        let log = self.root.path().join("systemctl.log");
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        let _ = std::fs::remove_file(&log);
        text.lines().map(str::to_owned).collect()
    }

    async fn setup(&self, args: &[&str]) -> std::process::Output {
        let path = format!(
            "{}:{}",
            self.bin().display(),
            std::env::var("PATH").unwrap()
        );
        let mut command = bare();
        command
            .env("HOME", self.root.path())
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_DATA_HOME", self.root.path().join("data"))
            .env("XDG_RUNTIME_DIR", self.root.path().join("run"))
            .env("PATH", path);
        let mut all = vec!["setup"];
        all.extend(args);
        run(&mut command, &all).await
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

const ENABLE: [&str; 2] = ["--user daemon-reload", "--user enable --now nowcast.socket"];

#[tokio::test]
async fn writes_the_units_and_enables_the_socket() {
    let home = Home::new();
    let output = home.setup(&[]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(home.calls(), ENABLE);

    let socket = home.units().join("nowcast.socket");
    assert_eq!(read(&socket), nowcast_daemon::SOCKET_UNIT);
    let service = read(&home.units().join("nowcast.service"));
    let exec = format!("ExecStart={} daemon", env!("CARGO_BIN_EXE_nowcast"));
    assert!(service.lines().any(|l| l == exec), "{service}");
    let report = stderr(&output);
    assert!(report.contains("No brick enabled"), "{report}");
    assert!(!home.brick_link("claude-code").exists());

    // Again: the units are left as they are.
    let output = home.setup(&[]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(home.calls(), ENABLE);
    assert!(stderr(&output).contains("nowcast.service up to date"));
}

#[tokio::test]
async fn rewrites_a_unit_that_differs_or_is_a_link() {
    let home = Home::new();
    std::fs::create_dir_all(home.units()).unwrap();
    let old = home.root.path().join("old.socket");
    std::fs::write(&old, "old").unwrap();
    std::os::unix::fs::symlink(&old, home.units().join("nowcast.socket")).unwrap();
    std::fs::write(home.units().join("nowcast.service"), "old").unwrap();

    let output = home.setup(&[]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let socket = home.units().join("nowcast.socket");
    assert!(!std::fs::symlink_metadata(&socket).unwrap().is_symlink());
    assert_eq!(read(&socket), nowcast_daemon::SOCKET_UNIT);
    assert_ne!(read(&home.units().join("nowcast.service")), "old");
    assert_eq!(read(&old), "old", "the link's target is left");
    let report = stderr(&output);
    assert!(report.contains("nowcast.socket rewritten"), "{report}");
    assert!(report.contains("nowcast.service rewritten"), "{report}");
}

#[tokio::test]
async fn enables_the_native_bricks_named() {
    let home = Home::new();
    let output = home.setup(&["claude-code"]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(home.brick_link("claude-code").is_symlink());
    assert!(stderr(&output).contains("Brick claude-code enabled"));
}

#[tokio::test]
async fn refuses_a_brick_not_native_before_changing_anything() {
    let home = Home::new();
    let output = home.setup(&["forge"]).await;
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("no native brick \"forge\""));
    assert!(home.calls().is_empty());
    assert!(!home.units().exists());
}

#[tokio::test]
async fn a_systemctl_failure_is_reported() {
    let home = Home::new();
    home.systemctl("echo 'Failed to connect to bus' >&2; exit 1");
    let output = home.setup(&[]).await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let message = stderr(&output);
    assert!(
        message.contains("systemctl --user daemon-reload failed"),
        "{message}"
    );
    assert!(message.contains("Failed to connect to bus"), "{message}");
}

#[tokio::test]
async fn remove_stops_the_service_and_removes_its_units() {
    let home = Home::new();
    let output = home.setup(&["claude-code"]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    // As `systemctl enable` would.
    let wants = home.units().join("sockets.target.wants");
    std::fs::create_dir_all(&wants).unwrap();
    std::os::unix::fs::symlink("../nowcast.socket", wants.join("nowcast.socket")).unwrap();
    home.calls();

    let output = home.setup(&["--remove"]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        home.calls(),
        [
            "--user stop nowcast.socket nowcast.service",
            "--user daemon-reload"
        ]
    );
    for name in [
        "nowcast.socket",
        "nowcast.service",
        "sockets.target.wants/nowcast.socket",
    ] {
        assert!(
            std::fs::symlink_metadata(home.units().join(name)).is_err(),
            "{name}"
        );
    }
    assert!(home.brick_link("claude-code").is_symlink(), "bricks stay");

    // Nothing left: nothing to do.
    let output = home.setup(&["--remove"]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(home.calls().is_empty());
    assert!(stderr(&output).contains("nothing to remove"));
}

#[tokio::test]
async fn remove_goes_on_when_the_service_does_not_stop() {
    let home = Home::new();
    std::fs::create_dir_all(home.units()).unwrap();
    std::os::unix::fs::symlink("/nonexistent", home.units().join("nowcast.socket")).unwrap();
    home.systemctl("case \"$*\" in *stop*) echo 'Unit not loaded' >&2; exit 5;; esac");

    let output = home.setup(&["--remove"]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stderr(&output).contains("Unit not loaded"));
    assert!(std::fs::symlink_metadata(home.units().join("nowcast.socket")).is_err());
}
