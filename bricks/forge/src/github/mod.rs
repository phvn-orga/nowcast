//! The GitHub provider: recognises GitHub remotes and follows a branch
//! through the REST API ([`read`]), at the pace of [`RHYTHM`]. A reading
//! that changes nothing costs no rate limit (`304`), hence a fast pace while
//! a CI is expected or runs. The token comes from `gh` ([`token`]);
//! [`API_VAR`] and [`TOKEN_VAR`] replace the API's address and the token,
//! for tests.

mod read;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

use crate::provider::{Failure, Outcome, Provider, Repo, Updates, Watch};
use crate::tools::http::{self, Http};
use crate::tools::pace::{Pace, Rhythm};
use crate::tools::remote;

/// GitHub's REST API.
pub const API: &str = "https://api.github.com";
/// Replaces [`API`], for tests.
pub const API_VAR: &str = "NOWCAST_FORGE_GITHUB_API";
/// A token to use instead of asking `gh`, for tests.
pub const TOKEN_VAR: &str = "NOWCAST_FORGE_GITHUB_TOKEN";

/// A reading that changes nothing is free: fast while something moves.
pub const RHYTHM: Rhythm = Rhythm {
    expected_first: Duration::from_secs(2),
    expected_after: Duration::from_secs(5),
    running: Duration::from_secs(5),
    window: Duration::from_secs(2),
    normal: Duration::from_secs(60),
};

/// Tokens by login, shared by every branch followed.
type Tokens = Arc<Mutex<HashMap<String, String>>>;

pub struct GitHub {
    http: Http,
    api: String,
    tokens: Tokens,
}

impl GitHub {
    /// Reads `api`, through `http`, whose cache it shares with every branch.
    pub fn new(http: Http, api: String) -> Self {
        Self {
            http,
            api,
            tokens: Tokens::default(),
        }
    }
}

impl Provider for GitHub {
    fn recognise(&self, remote: &str) -> Option<Repo> {
        let (owner, name) = recognise(remote, remote::ssh_hostname)?;
        Some(Repo {
            id: format!("github.com/{owner}/{name}"),
            remote: remote.to_owned(),
        })
    }

    fn follow(&self, repo: &Repo, branch: &str, updates: Updates) -> Watch {
        // The owner as the remote writes it: `gh` matches logins
        // case-sensitively. The repository's name is the identity's.
        let (_, path) = remote::split(&repo.remote).unwrap_or_default();
        let owner = path.split('/').next().unwrap_or_default().to_owned();
        let name = repo.id.rsplit('/').next().unwrap_or_default().to_owned();
        let branch = Branch {
            http: self.http.clone(),
            api: self.api.clone(),
            tokens: self.tokens.clone(),
            owner,
            name,
            branch: branch.to_owned(),
        };
        Watch::spawn(move |refreshes| branch.follow(updates, refreshes))
    }
}

/// One branch followed.
struct Branch {
    http: Http,
    api: String,
    tokens: Tokens,
    owner: String,
    name: String,
    branch: String,
}

impl Branch {
    /// Reads, sends what it read, waits as the pace says or for a
    /// `refresh`, and again, until the core is gone. A `refresh` that comes
    /// during a reading is answered by the next one. A rate limit is waited
    /// out whatever the refreshes.
    async fn follow(self, updates: Updates, mut refreshes: UnboundedReceiver<()>) {
        let mut pace = Pace::default();
        loop {
            let (outcome, wait) = self.read_once().await;
            let now = Instant::now();
            let (outcome, delay) = match outcome {
                Ok(mut reading) => {
                    pace.saw(
                        now,
                        reading.head_sha.as_deref(),
                        reading
                            .pr
                            .as_ref()
                            .filter(|p| p.is_open())
                            .map(|p| p.number),
                    );
                    let delay = match &mut reading.ci {
                        Some(ci) => {
                            ci.expected_until = pace.expected_until(ci.pipelines.len());
                            pace.next(&RHYTHM, now, ci.pipelines.len(), ci.is_running())
                        }
                        None => pace.next(&RHYTHM, now, 0, false),
                    };
                    (Ok(reading), delay)
                }
                // A failure says nothing new about the CI: no faster than
                // the normal pace until a reading succeeds again.
                Err(failure) => (
                    Err(failure),
                    wait.map_or(RHYTHM.normal, |w| w.max(RHYTHM.normal)),
                ),
            };
            if !updates.send(outcome) {
                return;
            }
            let deadline = now + delay;
            loop {
                tokio::select! {
                    () = tokio::time::sleep_until(deadline) => break,
                    refresh = refreshes.recv() => {
                        if refresh.is_none() {
                            return;
                        }
                        while refreshes.try_recv().is_ok() {}
                        pace.refreshed(Instant::now());
                        if wait.is_none() {
                            break;
                        }
                    }
                }
            }
        }
    }

    /// One reading, and how long a rate limit asks to wait.
    async fn read_once(&self) -> (Outcome, Option<Duration>) {
        let kept = self.tokens.lock().unwrap().get(&self.owner).cloned();
        let token = match kept {
            Some(token) => Some(token),
            None => token(&self.owner).await,
        };
        let Some(token) = token else {
            return (Err(Failure::NoCredentials), None);
        };
        self.tokens
            .lock()
            .unwrap()
            .insert(self.owner.clone(), token.clone());
        let failure = match read::read(
            &self.http,
            &self.api,
            &token,
            &self.owner,
            &self.name,
            &self.branch,
        )
        .await
        {
            Ok(reading) => return (Ok(reading), None),
            Err(failure) => failure,
        };
        let at = || format!("github.com/{}/{}#{}", self.owner, self.name, self.branch);
        match failure {
            http::Failure::Unauthorized => {
                // The next reading asks `gh` again.
                self.tokens.lock().unwrap().remove(&self.owner);
                (Err(Failure::Unauthorized), None)
            }
            http::Failure::NotFound => (Err(Failure::NotFound), None),
            http::Failure::Limited(wait) => (
                Err(Failure::RateLimited),
                Some(wait.unwrap_or(RHYTHM.normal)),
            ),
            http::Failure::Unreadable(why) => {
                crate::log(format_args!("{}: {why}", at()));
                (Err(Failure::Unreadable), None)
            }
            http::Failure::Unreachable(why) => {
                crate::log(format_args!("{}: unreachable: {why}", at()));
                (Err(Failure::Unreachable), None)
            }
        }
    }
}

/// The owner and name of a GitHub repository, in lowercase: GitHub ignores
/// their case. On an SSH remote, a host that is not GitHub's is taken as an
/// SSH alias and handed to `resolve`, which gives its real host name.
fn recognise(remote: &str, resolve: impl Fn(&str) -> Option<String>) -> Option<(String, String)> {
    let (host, path) = remote::split(remote)?;
    let host = if is_github(&host) {
        host
    } else if remote::is_ssh(remote) {
        resolve(&host)?.to_ascii_lowercase()
    } else {
        return None;
    };
    if !is_github(&host) {
        return None;
    }
    let (owner, name) = path.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some((owner.to_ascii_lowercase(), name.to_ascii_lowercase()))
}

fn is_github(host: &str) -> bool {
    host == "github.com" || host == "www.github.com"
}

/// The account `gh` holds for the repository's owner, or else its active
/// one: an organization is not an account. `None` without `gh` or without a
/// login.
async fn token(owner: &str) -> Option<String> {
    if let Ok(token) = std::env::var(TOKEN_VAR) {
        return Some(token);
    }
    match gh_token(Some(owner)).await {
        Some(token) => Some(token),
        None => gh_token(None).await,
    }
}

async fn gh_token(user: Option<&str>) -> Option<String> {
    let mut command = tokio::process::Command::new("gh");
    command.args(["auth", "token", "--hostname", "github.com"]);
    if let Some(user) = user {
        command.args(["--user", user]);
    }
    let output = command
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !token.is_empty()).then_some(token)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;
    use crate::testing::Server;

    fn none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn every_spelling_of_a_github_remote_names_one_repo() {
        let repo = Some(("phvn-orga".to_owned(), "nowcast".to_owned()));
        for remote in [
            "https://github.com/phvn-orga/nowcast.git",
            "https://github.com/phvn-orga/nowcast",
            "https://user@github.com/Phvn-Orga/NowCast/",
            "git@github.com:phvn-orga/nowcast.git",
            "ssh://git@github.com:22/phvn-orga/nowcast.git",
            "  git@github.com:phvn-orga/nowcast  ",
        ] {
            assert_eq!(recognise(remote, none), repo, "{remote}");
        }
    }

    #[test]
    fn an_ssh_alias_is_resolved() {
        let resolve = |alias: &str| (alias == "github-pro").then(|| "GitHub.com".to_owned());
        assert_eq!(
            recognise("git@github-pro:org/repo.git", resolve),
            Some(("org".into(), "repo".into()))
        );
        assert_eq!(recognise("git@gitlab.com:org/repo.git", resolve), None);
        let never = |_: &str| -> Option<String> { panic!("an HTTPS host is no SSH alias") };
        assert_eq!(recognise("https://github-pro/org/repo.git", never), None);
    }

    #[test]
    fn what_is_not_a_github_repository_is_refused() {
        for remote in [
            "",
            "not a remote",
            "/srv/git/repo.git",
            "https://github.com/only-owner",
            "https://github.com/org/repo/tree/main",
            "https://gitlab.com/org/repo.git",
        ] {
            assert_eq!(recognise(remote, none), None, "{remote}");
        }
    }

    fn branch(server: &Server, owner: &str) -> Branch {
        Branch {
            http: Http::new().unwrap(),
            api: server.base(),
            tokens: Tokens::default(),
            owner: owner.into(),
            name: "r".into(),
            branch: "feat".into(),
        }
    }

    /// A refused token is asked for again; a rate limit is waited out.
    #[tokio::test]
    async fn failures_become_the_source_status() {
        let server = Server::start(|request| {
            if request.contains("/repos/denied/") {
                (401, vec![], String::new())
            } else {
                (403, vec![("Retry-After", "300")], String::new())
            }
        })
        .await;
        let denied = branch(&server, "denied");
        denied
            .tokens
            .lock()
            .unwrap()
            .insert("denied".into(), "old".into());
        assert_eq!(denied.read_once().await, (Err(Failure::Unauthorized), None));
        assert!(denied.tokens.lock().unwrap().is_empty());
        let limited = branch(&server, "busy");
        limited
            .tokens
            .lock()
            .unwrap()
            .insert("busy".into(), "t".into());
        assert_eq!(
            limited.read_once().await,
            (Err(Failure::RateLimited), Some(Duration::from_secs(300)))
        );
    }

    /// The provider follows a branch: its first reading reaches the core.
    #[tokio::test]
    async fn a_followed_branch_sends_its_readings() {
        let server = Server::start(|request| {
            let line = request.lines().next().unwrap_or_default().to_owned();
            if line.contains("/pulls?state=open") {
                let pr = json!([{ "number": 7, "state": "open", "draft": false, "html_url": "u",
                    "merged_at": null, "base": { "ref": "main" },
                    "head": { "ref": "feat", "sha": "abc", "repo": { "full_name": "o/r" } } }]);
                (200, vec![], pr.to_string())
            } else if line.contains("/check-runs") {
                (200, vec![], json!({ "check_runs": [] }).to_string())
            } else if line.contains("/actions/runs") {
                (200, vec![], json!({ "workflow_runs": [] }).to_string())
            } else {
                (200, vec![], json!({ "statuses": [] }).to_string())
            }
        })
        .await;
        let github = GitHub::new(Http::new().unwrap(), server.base());
        github.tokens.lock().unwrap().insert("o".into(), "t".into());
        let repo = github.recognise("git@github.com:o/r.git").unwrap();
        assert_eq!(repo.id, "github.com/o/r");
        let (tx, mut rx) = unbounded_channel();
        let _watch = github.follow(&repo, "feat", Updates::new(3, tx));
        let (target, outcome) = rx.recv().await.unwrap();
        assert_eq!(target, 3);
        let reading = outcome.unwrap();
        assert_eq!(reading.pr.unwrap().number, 7);
        let ci = reading.ci.unwrap();
        assert_eq!(ci.sha, "abc");
        assert_eq!(ci.expected_until, None, "the first reading expects nothing");
    }
}
