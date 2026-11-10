//! `nowcast reload` against a fake service found as systemd holds it, under
//! units `nowcast setup` wrote, with a `systemctl` that records its calls.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::time::Duration;

use common::{Anchoring, Fake, bare, nowcast, run, stderr, stdout};
use serde_json::{Value, json};

const INSTALLED: &str = env!("CARGO_PKG_VERSION");
const RESTART: &str = "--user restart nowcast.service";

struct Home {
    root: tempfile::TempDir,
}

impl Home {
    /// A home where systemd says the socket and the service are active, and
    /// `restart` returns at once.
    fn new() -> Self {
        let home = Self {
            root: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(home.path("bin")).unwrap();
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{log}'\ncase \"$*\" in\n\
             *is-active*) cat '{states}' ;;\n\
             *restart*) while [ ! -f '{go}' ]; do sleep 0.02; done ;;\nesac\n",
            log = home.path("systemctl.log").display(),
            states = home.path("states").display(),
            go = home.path("go").display(),
        );
        let systemctl = home.path("bin/systemctl");
        std::fs::write(&systemctl, script).unwrap();
        std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755)).unwrap();
        home.states("active\nactive\n");
        home.let_restart_return();
        home
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// What `systemctl is-active nowcast.socket nowcast.service` prints.
    fn states(&self, states: &str) {
        std::fs::write(self.path("states"), states).unwrap();
    }

    fn hold_restart(&self) {
        let _ = std::fs::remove_file(self.path("go"));
    }

    fn let_restart_return(&self) {
        std::fs::write(self.path("go"), "").unwrap();
    }

    /// Waits for `nowcast` to call `systemctl restart`, failing the test
    /// rather than hanging when it never does.
    async fn restart_called(&self) {
        let called = async {
            while !self.calls().iter().any(|c| c == RESTART) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), called)
            .await
            .expect("systemctl restart is called");
    }

    fn calls(&self) -> Vec<String> {
        let text = std::fs::read_to_string(self.path("systemctl.log")).unwrap_or_default();
        text.lines().map(str::to_owned).collect()
    }

    /// Writes the units as `nowcast setup` does, and the link enabling the
    /// socket, which the recording `systemctl` does not make.
    async fn set_up(&self, fake: &Fake) {
        let output = run(&mut self.command(fake), &["setup"]).await;
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        let wants = self.path("config/systemd/user/sockets.target.wants");
        std::fs::create_dir_all(&wants).unwrap();
        std::os::unix::fs::symlink("../nowcast.socket", wants.join("nowcast.socket")).unwrap();
        std::fs::remove_file(self.path("systemctl.log")).unwrap();
    }

    /// `nowcast` finding the fake's socket through `XDG_RUNTIME_DIR`.
    fn command(&self, fake: &Fake) -> tokio::process::Command {
        let path = format!(
            "{}:{}",
            self.path("bin").display(),
            std::env::var("PATH").unwrap()
        );
        let mut command = bare();
        command
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_RUNTIME_DIR", fake.runtime_dir())
            .env("PATH", path);
        command
    }

    async fn reload(&self, fake: &Fake, args: &[&str]) -> Output {
        let mut all = vec!["reload"];
        all.extend(args);
        run(&mut self.command(fake), &all).await
    }
}

fn service(version: &str) -> String {
    json!({ "version": version, "started_at": "2026-11-10T11:00:00Z", "path": null }).to_string()
}

#[tokio::test]
async fn restarts_the_service_and_prints_the_version_before_and_after() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let home = Home::new();
    home.set_up(&fake).await;
    fake.can("/v1/service", 200, "application/json", &service("0.0.1"));

    // The new version answers only once systemd has restarted the service.
    home.hold_restart();
    let child = home
        .command(&fake)
        .arg("reload")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    home.restart_called().await;
    fake.can("/v1/service", 200, "application/json", &service(INSTALLED));
    home.let_restart_return();
    let output = child.wait_with_output().await.unwrap();

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!("service restarted: 0.0.1 → {INSTALLED}\n")
    );
    let paths: Vec<_> = fake.log().into_iter().map(|r| r.path).collect();
    assert_eq!(paths, ["/v1/service", "/v1/service"], "no session declared");
    assert_eq!(home.calls().last().map(String::as_str), Some(RESTART));
}

#[tokio::test]
async fn prints_json_and_restarts_at_the_same_version() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let home = Home::new();
    home.set_up(&fake).await;
    fake.can("/v1/service", 200, "application/json", &service(INSTALLED));

    let output = home.reload(&fake, &["--json"]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let printed: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(printed, json!({ "before": INSTALLED, "after": INSTALLED }));
    assert!(home.calls().iter().any(|c| c == RESTART));
}

#[tokio::test]
async fn a_service_that_does_not_answer_is_restarted_all_the_same() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let home = Home::new();
    home.set_up(&fake).await;
    fake.can("/v1/service", 500, "text/plain", "broken");

    home.hold_restart();
    let child = home
        .command(&fake)
        .args(["reload", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    home.restart_called().await;
    fake.can("/v1/service", 200, "application/json", &service(INSTALLED));
    home.let_restart_return();
    let output = child.wait_with_output().await.unwrap();

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let printed: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(printed, json!({ "before": null, "after": INSTALLED }));
}

#[tokio::test]
async fn a_stopped_service_is_left_stopped() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let home = Home::new();
    home.set_up(&fake).await;
    home.states("active\ninactive\n");

    let output = home.reload(&fake, &[]).await;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "service stopped: the next connection starts the installed version\n"
    );
    let output = home.reload(&fake, &["--json"]).await;
    let printed: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(printed, json!({ "before": null, "after": null }));

    assert!(fake.log().is_empty(), "no request reaches the socket");
    assert!(!home.calls().iter().any(|c| c == RESTART));
}

#[tokio::test]
async fn refuses_without_the_units_of_setup() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let home = Home::new();

    let output = home.reload(&fake, &[]).await;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        stderr(&output).contains("run nowcast setup"),
        "{}",
        stderr(&output)
    );
    assert!(home.calls().is_empty());
    assert!(fake.log().is_empty());
}

#[tokio::test]
async fn refuses_outdated_units_before_restarting() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let home = Home::new();
    home.set_up(&fake).await;
    std::fs::write(home.path("config/systemd/user/nowcast.service"), "old").unwrap();

    let output = home.reload(&fake, &[]).await;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        stderr(&output).contains("outdated: run nowcast setup, then nowcast reload"),
        "{}",
        stderr(&output)
    );
    assert!(home.calls().is_empty());
}

#[tokio::test]
async fn refuses_a_socket_given() {
    let fake = Fake::start(Anchoring::PerProcess).await;
    let output = run(&mut nowcast(&fake), &["reload"]).await;
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        stderr(&output).contains("NOWCAST_SOCKET"),
        "{}",
        stderr(&output)
    );
    assert!(fake.log().is_empty());
}
