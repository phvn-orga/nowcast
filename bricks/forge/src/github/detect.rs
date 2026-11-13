//! Change detection on GitHub, by repository and kind: three detectors per
//! repository, polling one list each in conditional requests (`304` costs
//! nothing), shared by every target of that repository the provider
//! follows, and running only while one of them depends on it:
//!
//! - runs: `actions/runs?per_page=20`;
//! - PRs: `pulls?state=all&sort=updated&direction=desc&per_page=20`;
//! - issues: `issues?state=all&sort=updated&direction=desc&per_page=20`.
//!
//! On a `200`, a detector compares the list with its last one: an item new
//! to the list, or whose `updated_at`, state or head changed, has moved, and
//! wakes the targets it touches ([`Moved::touches`]), which then read in
//! detail. The first answer only sets what the next ones are compared with.
//!
//! A detector polls every [`PERIOD`](crate::tools::throttle::PERIOD),
//! slower on a busy repository, as the shared registry does
//! ([`crate::tools::detect`]): a `200` counts as a change for its throttle.
//! A target's `refresh` opens the fast window of the detectors it depends
//! on: a change made on the forge takes from 5 to 24 s to show in these
//! lists.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use super::issues::Accounts;
use super::read::{HEADERS, is_repo};
use super::{RHYTHM, Tokens, owner_token};
use crate::credentials::Credentials;
use crate::tools::detect::{self, Polled};
use crate::tools::http::Http;
use crate::tools::pace::Rhythm;

/// What a detector polls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Runs,
    Pulls,
    Issues,
}

/// The detectors a branch's target, or a PR's, depends on.
pub const BRANCH: &[Kind] = &[Kind::Runs, Kind::Pulls];
/// The detector the targets of a tracker depend on.
pub const TRACKER: &[Kind] = &[Kind::Issues];

impl Kind {
    /// The kind, for the log.
    const fn name(self) -> &'static str {
        match self {
            Self::Runs => "runs",
            Self::Pulls => "PRs",
            Self::Issues => "issues",
        }
    }

    /// The list polled, under the repository's path. Its parameters never
    /// change, for its `ETag` to keep serving.
    const fn path(self) -> &'static str {
        match self {
            Self::Runs => "actions/runs?per_page=20",
            Self::Pulls => "pulls?state=all&sort=updated&direction=desc&per_page=20",
            Self::Issues => "issues?state=all&sort=updated&direction=desc&per_page=20",
        }
    }

    /// The items of an answer.
    fn items(self, body: &Value) -> &[Value] {
        let list = match self {
            Self::Runs => &body["workflow_runs"],
            Self::Pulls | Self::Issues => body,
        };
        list.as_array().map_or(&[], Vec::as_slice)
    }

    /// What tells an item from the others: a run's id, a PR's or an issue's
    /// number.
    fn key(self, item: &Value) -> Option<u64> {
        match self {
            Self::Runs => item["id"].as_u64(),
            Self::Pulls | Self::Issues => item["number"].as_u64(),
        }
    }

    /// What changes when an item moves.
    fn signature(self, item: &Value) -> [&Value; 4] {
        match self {
            Self::Runs => [
                &item["updated_at"],
                &item["status"],
                &item["conclusion"],
                &item["head_sha"],
            ],
            Self::Pulls => [
                &item["updated_at"],
                &item["state"],
                &item["head"]["sha"],
                &Value::Null,
            ],
            Self::Issues => [
                &item["updated_at"],
                &item["state"],
                &Value::Null,
                &Value::Null,
            ],
        }
    }
}

/// What a target is about, for a detector to tell whether an item touches
/// it. A target updates it from its readings ([`Watcher::learn`]).
#[derive(Debug, Clone, Default)]
pub struct Interest {
    /// The detectors the target depends on.
    pub kinds: &'static [Kind],
    /// The branch of this repository it reads: a branch's, or the head
    /// branch of a PR from this repository.
    pub branch: Option<String>,
    /// The PR it reads: a PR's own, or a branch's latest.
    pub pr: Option<u64>,
    /// The commit whose CI it reads.
    pub sha: Option<String>,
    /// The issue it reads.
    pub issue: Option<u64>,
    /// Whether it reads the tracker's open issues.
    pub tracker: bool,
}

/// An item a detector saw move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Moved {
    /// A workflow run: its branch, when the run is of this repository, its
    /// commit, and the PRs it belongs to.
    Run {
        branch: Option<String>,
        sha: Option<String>,
        prs: Vec<u64>,
    },
    /// A PR, with its head branch when that branch is of this repository.
    Pull { number: u64, branch: Option<String> },
    /// An issue.
    Issue(u64),
    /// A PR, as the issues list mixes them in.
    PullInIssues(u64),
}

impl Moved {
    /// Whether this move touches a target of `interest`. A run touches the
    /// target of its branch, any target reading its commit, and those of
    /// the PRs it belongs to; a PR, its own targets and that of its head
    /// branch; an issue, its own target and the tracker's open issues.
    pub fn touches(&self, interest: &Interest) -> bool {
        let same = |a: &Option<String>, b: &Option<String>| a.is_some() && a == b;
        match self {
            Self::Run { branch, sha, prs } => {
                same(branch, &interest.branch)
                    || same(sha, &interest.sha)
                    || interest.pr.is_some_and(|n| prs.contains(&n))
            }
            Self::Pull { number, branch } => {
                interest.pr == Some(*number) || same(branch, &interest.branch)
            }
            Self::Issue(number) => interest.tracker || interest.issue == Some(*number),
            Self::PullInIssues(number) => interest.pr == Some(*number),
        }
    }
}

/// What moved from list `before` to list `after` of a detector of `kind`
/// in repository `repo` (`<owner>/<name>`).
pub fn moved(kind: Kind, before: &Value, after: &Value, repo: &str) -> Vec<Moved> {
    let known: HashMap<u64, [&Value; 4]> = kind
        .items(before)
        .iter()
        .filter_map(|item| Some((kind.key(item)?, kind.signature(item))))
        .collect();
    kind.items(after)
        .iter()
        .filter(|item| {
            kind.key(item)
                .is_some_and(|key| known.get(&key) != Some(&kind.signature(item)))
        })
        .filter_map(|item| describe(kind, item, repo))
        .collect()
}

/// The move of `item`, from a list of `kind` in `repo`.
fn describe(kind: Kind, item: &Value, repo: &str) -> Option<Moved> {
    let own = |r: &Value| is_repo(r, repo);
    let text = |v: &Value| v.as_str().map(str::to_owned);
    Some(match kind {
        Kind::Runs => Moved::Run {
            branch: own(&item["head_repository"])
                .then(|| text(&item["head_branch"]))
                .flatten(),
            sha: text(&item["head_sha"]),
            prs: item["pull_requests"]
                .as_array()
                .map(|a| a.iter().filter_map(|p| p["number"].as_u64()).collect())
                .unwrap_or_default(),
        },
        Kind::Pulls => Moved::Pull {
            number: item["number"].as_u64()?,
            branch: own(&item["head"]["repo"])
                .then(|| text(&item["head"]["ref"]))
                .flatten(),
        },
        Kind::Issues if item.get("pull_request").is_some() => {
            Moved::PullInIssues(item["number"].as_u64()?)
        }
        Kind::Issues => Moved::Issue(item["number"].as_u64()?),
    })
}

/// What a detector needs to read.
#[derive(Clone)]
pub struct Access {
    pub http: Http,
    pub api: String,
    pub credentials: Credentials,
    pub tokens: Tokens,
    pub accounts: Accounts,
}

/// The repositories detected, shared by every target of the provider.
pub type Detection = detect::Detection<Repository>;
/// A target's registration, and what its detectors send it.
pub type Detected = detect::Detected<Repository>;

/// Registers a target of `owner/name` about `interest`: the detectors it
/// depends on run while it stays registered.
pub fn watch(
    detection: &Detection,
    access: &Access,
    owner: &str,
    name: &str,
    interest: Interest,
) -> Detected {
    let key = format!("{owner}/{name}").to_ascii_lowercase();
    let repository = || Repository {
        access: access.clone(),
        owner: owner.to_owned(),
        name: name.to_owned(),
    };
    detection.watch(key, repository, interest)
}

/// One repository detected: what its detectors share.
pub struct Repository {
    access: Access,
    owner: String,
    name: String,
}

impl detect::Scope for Repository {
    type Interest = Interest;
    type Key = Kind;
    type Moved = Moved;
    type Detector = RepoDetector;

    const RHYTHM: Rhythm = RHYTHM;

    fn keys(interest: &Interest) -> Vec<Kind> {
        interest.kinds.to_vec()
    }

    fn touches(moved: &Moved, interest: &Interest) -> bool {
        moved.touches(interest)
    }

    fn detector(&self, kind: &Kind) -> RepoDetector {
        RepoDetector {
            kind: *kind,
            access: self.access.clone(),
            owner: self.owner.clone(),
            name: self.name.clone(),
            last: None,
        }
    }
}

/// One detector of a repository: the list it polls, and the last read.
pub struct RepoDetector {
    kind: Kind,
    access: Access,
    owner: String,
    name: String,
    last: Option<Arc<Value>>,
}

impl detect::Detector for RepoDetector {
    type Moved = Moved;

    /// A `304` changed nothing; a `200` compares the list with the last
    /// one. Without a token, the detector waits: asking its source again
    /// sooner (`gh auth token`) would not bring one.
    async fn poll(&mut self) -> Polled<Moved> {
        let a = &self.access;
        let Some(token) = owner_token(&a.tokens, &a.accounts, &a.credentials, &self.owner).await
        else {
            return Polled::NoToken;
        };
        let url = format!(
            "{}/repos/{}/{}/{}",
            a.api,
            self.owner,
            self.name,
            self.kind.path()
        );
        let (body, changed) = match a.http.get_changed(&url, &token, HEADERS).await {
            Ok(answer) => answer,
            Err(failure) => return Polled::Failed(failure),
        };
        if !changed {
            return Polled::Answered {
                changed: false,
                moved: Vec::new(),
            };
        }
        let polled = match &self.last {
            Some(before) => {
                let repo = format!("{}/{}", self.owner, self.name);
                Polled::Answered {
                    changed: true,
                    moved: moved(self.kind, before, &body, &repo),
                }
            }
            None => Polled::First,
        };
        self.last = Some(body);
        polled
    }

    async fn drop_token(&mut self) {
        let mut tokens = self.access.tokens.lock().unwrap();
        tokens.by_owner.remove(&self.owner);
    }

    fn at(&self) -> String {
        format!(
            "github.com/{}/{} {} detector",
            self.owner,
            self.name,
            self.kind.name()
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use std::sync::Mutex;
    use std::time::Duration;

    use tokio::sync::mpsc::UnboundedReceiver;
    use tokio::time::Instant;

    use super::*;
    use crate::testing::Server;
    use crate::tools::detect::testing::{watched, woken};
    use crate::tools::detect::{Poller, Watched};
    use crate::tools::pace::WINDOW_FOR;

    fn run(id: u64, updated: &str, branch: &str, repo: &str, sha: &str, prs: &[u64]) -> Value {
        let prs: Vec<Value> = prs.iter().map(|n| json!({ "number": n })).collect();
        json!({ "id": id, "updated_at": updated, "status": "completed", "conclusion": "success",
            "head_branch": branch, "head_sha": sha, "head_repository": { "full_name": repo },
            "pull_requests": prs })
    }

    fn pull(number: u64, updated: &str, branch: &str, repo: &str) -> Value {
        json!({ "number": number, "updated_at": updated, "state": "open",
            "head": { "ref": branch, "sha": "s", "repo": { "full_name": repo } } })
    }

    fn issue(number: u64, updated: &str, pull: bool) -> Value {
        let mut issue = json!({ "number": number, "updated_at": updated, "state": "open" });
        if pull {
            issue["pull_request"] = json!({ "url": "p" });
        }
        issue
    }

    #[test]
    fn what_moved_is_what_is_new_or_changed() {
        let before = json!({ "workflow_runs": [
            run(1, "t1", "feat", "o/r", "a", &[]),
            run(2, "t1", "main", "o/r", "b", &[]),
            run(3, "t1", "old", "o/r", "c", &[]),
        ] });
        let mut done = run(2, "t1", "main", "o/r", "b", &[]);
        done["conclusion"] = json!("failure");
        let after = json!({ "workflow_runs": [
            run(4, "t2", "feat", "fork/r", "d", &[7]),
            run(1, "t1", "feat", "o/r", "a", &[]),
            done,
        ] });
        assert_eq!(
            moved(Kind::Runs, &before, &after, "O/R"),
            [
                Moved::Run {
                    branch: None,
                    sha: Some("d".into()),
                    prs: vec![7]
                },
                Moved::Run {
                    branch: Some("main".into()),
                    sha: Some("b".into()),
                    prs: vec![]
                },
            ],
            "a fork's branch is no branch of this repository; a run gone moved nothing"
        );
        let before = json!([pull(5, "t1", "feat", "o/r")]);
        let after = json!([
            pull(6, "t1", "feat", "fork/r"),
            pull(5, "t2", "feat", "o/r")
        ]);
        assert_eq!(
            moved(Kind::Pulls, &before, &after, "o/r"),
            [
                Moved::Pull {
                    number: 6,
                    branch: None
                },
                Moved::Pull {
                    number: 5,
                    branch: Some("feat".into())
                },
            ]
        );
        let before = json!([issue(1, "t1", false), issue(2, "t1", true)]);
        let after = json!([
            issue(2, "t2", true),
            issue(3, "t1", false),
            issue(1, "t1", false)
        ]);
        assert_eq!(
            moved(Kind::Issues, &before, &after, "o/r"),
            [Moved::PullInIssues(2), Moved::Issue(3)]
        );
    }

    /// A forge whose lists are `bodies[version]`, under an `ETag` naming
    /// the version: asked with that `ETag`, it answers `304`.
    async fn lists(version: Arc<Mutex<usize>>, bodies: Vec<Value>) -> Server {
        Server::start(move |request| {
            let v = *version.lock().unwrap();
            if request.contains(&format!("if-none-match: \"v{v}\"")) {
                return (304, vec![], String::new());
            }
            let etag: &'static str = Box::leak(format!("\"v{v}\"").into_boxed_str());
            (200, vec![("ETag", etag)], bodies[v].to_string())
        })
        .await
    }

    fn access(server: &Server) -> Access {
        let tokens = Tokens::default();
        tokens
            .lock()
            .unwrap()
            .by_owner
            .insert("o".into(), "t".into());
        Access {
            http: Http::new().unwrap(),
            api: server.base(),
            credentials: crate::credentials::testing::none(),
            tokens,
            accounts: Accounts::default(),
        }
    }

    /// A repository with a target per interest, and no detector running:
    /// the test polls them itself.
    fn repository(
        server: &Server,
        interests: Vec<Interest>,
    ) -> (Arc<Watched<Repository>>, Vec<UnboundedReceiver<()>>) {
        let repository = Repository {
            access: access(server),
            owner: "o".into(),
            name: "r".into(),
        };
        watched(repository, interests)
    }

    fn interests() -> Vec<Interest> {
        vec![
            // 0: the branch `feat`, at `a`, with PR 7.
            Interest {
                kinds: BRANCH,
                branch: Some("feat".into()),
                pr: Some(7),
                sha: Some("a".into()),
                ..Interest::default()
            },
            // 1: the branch `other`, without PR.
            Interest {
                kinds: BRANCH,
                branch: Some("other".into()),
                sha: Some("z".into()),
                ..Interest::default()
            },
            // 2: PR 9 from a fork, at `f`.
            Interest {
                kinds: BRANCH,
                pr: Some(9),
                sha: Some("f".into()),
                ..Interest::default()
            },
            // 3: the open issues.
            Interest {
                kinds: TRACKER,
                tracker: true,
                ..Interest::default()
            },
            // 4: issue 3.
            Interest {
                kinds: TRACKER,
                issue: Some(3),
                ..Interest::default()
            },
        ]
    }

    /// The first answer wakes nothing; a `200` wakes only the targets what
    /// moved touches; a `304` wakes nothing.
    #[tokio::test]
    async fn a_200_wakes_only_the_targets_touched_and_a_304_nothing() {
        let cases = [
            (
                Kind::Runs,
                json!({ "workflow_runs": [] }),
                json!({ "workflow_runs": [run(1, "t", "feat", "o/r", "x", &[])] }),
                vec![0],
            ),
            (
                Kind::Runs,
                json!({ "workflow_runs": [] }),
                json!({ "workflow_runs": [run(1, "t", "fork-branch", "fork/r", "f", &[])] }),
                vec![2],
            ),
            (
                Kind::Runs,
                json!({ "workflow_runs": [] }),
                json!({ "workflow_runs": [run(1, "t", "main", "o/r", "m", &[7])] }),
                vec![0],
            ),
            (
                Kind::Pulls,
                json!([]),
                json!([pull(9, "t", "other", "fork/r")]),
                vec![2],
            ),
            (
                Kind::Pulls,
                json!([]),
                json!([pull(12, "t", "other", "o/r")]),
                vec![1],
            ),
            (
                Kind::Issues,
                json!([issue(3, "t1", false)]),
                json!([issue(3, "t2", false)]),
                vec![3, 4],
            ),
            (
                Kind::Issues,
                json!([]),
                json!([issue(7, "t", true), issue(5, "t", false)]),
                vec![0, 3],
            ),
        ];
        for (kind, before, after, expected) in cases {
            let version = Arc::new(Mutex::new(0));
            let server = lists(version.clone(), vec![before, after]).await;
            let (repo, mut wakes) = repository(&server, interests());
            let mut detector = Poller::new(&repo, &kind);
            assert_eq!(detector.poll().await, None);
            assert!(woken(&mut wakes).is_empty(), "{kind:?}: the first answer");
            assert_eq!(detector.poll().await, None);
            assert!(woken(&mut wakes).is_empty(), "{kind:?}: a 304");
            *version.lock().unwrap() = 1;
            detector.poll().await;
            assert_eq!(woken(&mut wakes), expected, "{kind:?}");
            detector.poll().await;
            assert!(woken(&mut wakes).is_empty(), "{kind:?}: a 304 after");
            let path = format!("/repos/o/r/{} ", kind.path());
            assert!(server.requests().iter().all(|r| r.contains(&path)));
            assert_eq!(detector.throttle.recent(), [false, true, false]);
        }
    }

    /// A rate limit is waited out and logged once; a refused token is
    /// forgotten, and without one the detector waits for the normal pace.
    #[tokio::test]
    async fn a_failing_detector_waits_out_a_rate_limit() {
        let server = Server::start(|request| {
            if request.contains("/actions/") {
                (403, vec![("Retry-After", "300")], String::new())
            } else {
                (401, vec![], String::new())
            }
        })
        .await;
        let (repo, _) = repository(&server, vec![]);
        let mut runs = Poller::new(&repo, &Kind::Runs);
        assert_eq!(runs.poll().await, Some(Duration::from_secs(300)));
        assert!(runs.failing);
        let mut pulls = Poller::new(&repo, &Kind::Pulls);
        assert_eq!(pulls.poll().await, None);
        assert!(repo.scope.access.tokens.lock().unwrap().by_owner.is_empty());
        if std::env::var(super::super::TOKEN_VAR).is_err() {
            assert_eq!(pulls.poll().await, Some(RHYTHM.normal));
        }
    }

    /// A failed poll has the detector wait the normal pace, window open or
    /// not; only a `401` drops the token, once until a poll answers.
    #[tokio::test]
    async fn a_failed_poll_waits_the_normal_pace_and_only_a_401_drops_the_token() {
        let server = Server::start(|request| {
            let status = if request.contains("/actions/") {
                403
            } else if request.contains("/pulls?") {
                404
            } else {
                401
            };
            (status, vec![], String::new())
        })
        .await;
        let (repo, _) = repository(&server, vec![]);
        let token = || {
            repo.scope
                .access
                .tokens
                .lock()
                .unwrap()
                .by_owner
                .get("o")
                .cloned()
        };
        let now = Instant::now();
        for kind in [Kind::Runs, Kind::Pulls] {
            let mut detector = Poller::new(&repo, &kind);
            detector.window_until = Some(now + WINDOW_FOR);
            assert_eq!(detector.period(now), RHYTHM.window);
            assert_eq!(detector.poll().await, None);
            assert_eq!(detector.period(now), RHYTHM.normal, "{kind:?}");
            assert_eq!(token().as_deref(), Some("t"), "{kind:?} keeps the token");
        }
        let mut issues = Poller::new(&repo, &Kind::Issues);
        assert_eq!(issues.poll().await, None);
        assert_eq!(issues.period(now), RHYTHM.normal);
        assert_eq!(token(), None, "a 401 drops the token");
        repo.scope
            .access
            .tokens
            .lock()
            .unwrap()
            .by_owner
            .insert("o".into(), "t2".into());
        assert_eq!(issues.poll().await, None);
        assert_eq!(token().as_deref(), Some("t2"), "the next 401 keeps it");
    }
}
