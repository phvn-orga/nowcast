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
//! A detector polls every [`PERIOD`], slower on a busy repository
//! ([`Throttle`]). A target's `refresh` has the detectors it depends on poll
//! at once and opens their fast window: they poll at the provider's window
//! pace for [`WINDOW_FOR`], a change seen on the forge after an action
//! taking from 5 to 24 s to show in these lists.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;
use tokio::time::Instant;

use super::issues::Accounts;
use super::read::HEADERS;
use super::{RHYTHM, Tokens, owner_token};
use crate::credentials::Credentials;
use crate::tools::http::{self, Http};
use crate::tools::pace::WINDOW_FOR;

/// How often a detector polls a calm repository.
pub const PERIOD: Duration = Duration::from_secs(10);
/// How often, at the slowest, a detector polls a busy one.
pub const SLOWEST: Duration = Duration::from_secs(60);
/// The polls the slowdown rule looks back on.
const LOOKBACK: usize = 6;

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
    let own = |r: &Value| {
        r["full_name"]
            .as_str()
            .is_some_and(|f| f.eq_ignore_ascii_case(repo))
    };
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

/// How often a detector polls. It starts at [`PERIOD`]. Once it has
/// [`LOOKBACK`] answers at a period: when more than half were a `200`,
/// the period doubles, up to [`SLOWEST`]; when one at most was, it halves,
/// down to [`PERIOD`]. A change of period starts the count again, so that
/// each period is judged on answers at that period; otherwise the oldest
/// answer leaves the count.
#[derive(Debug)]
pub struct Throttle {
    period: Duration,
    /// Whether each answer since the period was set was a `200`.
    recent: Vec<bool>,
}

impl Default for Throttle {
    fn default() -> Self {
        Self {
            period: PERIOD,
            recent: Vec::new(),
        }
    }
}

impl Throttle {
    /// An answer, `changed` for a `200`.
    pub fn answered(&mut self, changed: bool) {
        self.recent.push(changed);
        if self.recent.len() < LOOKBACK {
            return;
        }
        let changes = self.recent.iter().filter(|c| **c).count();
        let period = if changes > LOOKBACK / 2 {
            (self.period * 2).min(SLOWEST)
        } else if changes <= 1 {
            (self.period / 2).max(PERIOD)
        } else {
            self.period
        };
        if period == self.period {
            self.recent.remove(0);
        } else {
            self.period = period;
            self.recent.clear();
        }
    }

    pub const fn period(&self) -> Duration {
        self.period
    }
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
#[derive(Clone, Default)]
pub struct Detection {
    repos: Arc<Mutex<HashMap<String, Weak<Repository>>>>,
}

impl Detection {
    /// Registers a target of `owner/name` about `interest`: the detectors it
    /// depends on run while it stays registered.
    pub fn watch(&self, access: &Access, owner: &str, name: &str, interest: Interest) -> Detected {
        let key = format!("{owner}/{name}").to_ascii_lowercase();
        let repo = {
            let mut repos = self.repos.lock().unwrap();
            repos.retain(|_, repo| repo.strong_count() > 0);
            if let Some(repo) = repos.get(&key).and_then(Weak::upgrade) {
                repo
            } else {
                let repo = Arc::new(Repository {
                    access: access.clone(),
                    owner: owner.to_owned(),
                    name: name.to_owned(),
                    state: Mutex::default(),
                });
                repos.insert(key, Arc::downgrade(&repo));
                repo
            }
        };
        let (wake, wakes) = unbounded_channel();
        let id = repo.add(interest, wake);
        Detected {
            watcher: Watcher { repo, id },
            wakes,
        }
    }

    /// The repositories detected and their detectors running, for tests.
    #[cfg(test)]
    pub fn running(&self) -> Vec<(String, usize)> {
        let repos = self.repos.lock().unwrap();
        let mut running: Vec<(String, usize)> = repos
            .iter()
            .filter_map(|(key, repo)| {
                let repo = repo.upgrade()?;
                let count = repo.state.lock().unwrap().detectors.len();
                Some((key.clone(), count))
            })
            .collect();
        running.sort();
        running
    }
}

/// One repository detected, held by the targets registered on it.
pub struct Repository {
    access: Access,
    owner: String,
    name: String,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    last_id: u64,
    /// Each target registered, and where to wake it.
    watchers: HashMap<u64, (Interest, UnboundedSender<()>)>,
    detectors: HashMap<Kind, Running>,
}

/// A detector's task, stopped when dropped.
struct Running {
    task: AbortHandle,
    /// Asks for a poll now, answered once done.
    asks: UnboundedSender<oneshot::Sender<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Repository {
    /// Registers a target, starting the detectors it needs that no other
    /// target started.
    fn add(self: &Arc<Self>, interest: Interest, wake: UnboundedSender<()>) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.last_id += 1;
        let id = state.last_id;
        for kind in interest.kinds {
            if !state.detectors.contains_key(kind) {
                let (asks, asked) = unbounded_channel();
                let detector = Detector::new(self, *kind);
                let task = tokio::spawn(detector.run(asked)).abort_handle();
                state.detectors.insert(*kind, Running { task, asks });
            }
        }
        state.watchers.insert(id, (interest, wake));
        id
    }

    /// Unregisters target `id`, stopping the detectors no target needs.
    fn remove(&self, id: u64) {
        let mut state = self.state.lock().unwrap();
        state.watchers.remove(&id);
        let State {
            watchers,
            detectors,
            ..
        } = &mut *state;
        detectors.retain(|kind, _| watchers.values().any(|(i, _)| i.kinds.contains(kind)));
    }

    /// Wakes the targets one of `moved` touches.
    fn wake(&self, moved: &[Moved]) {
        let state = self.state.lock().unwrap();
        for (interest, wake) in state.watchers.values() {
            if moved.iter().any(|m| m.touches(interest)) {
                let _ = wake.send(());
            }
        }
    }
}

/// A target's registration; dropping it unregisters the target.
pub struct Watcher {
    repo: Arc<Repository>,
    id: u64,
}

impl Watcher {
    /// Updates what the target is about.
    pub fn learn(&self, update: impl FnOnce(&mut Interest)) {
        let mut state = self.repo.state.lock().unwrap();
        if let Some((interest, _)) = state.watchers.get_mut(&self.id) {
            update(interest);
        }
    }

    /// Has the detectors the target depends on poll now, opening their fast
    /// window, and returns once they did, or could not.
    pub async fn poll_now(&self) {
        let answers: Vec<oneshot::Receiver<()>> = {
            let state = self.repo.state.lock().unwrap();
            let kinds = state
                .watchers
                .get(&self.id)
                .map_or(&[][..], |(i, _)| i.kinds);
            kinds
                .iter()
                .filter_map(|kind| state.detectors.get(kind))
                .map(|running| {
                    let (answer, answered) = oneshot::channel();
                    let _ = running.asks.send(answer);
                    answered
                })
                .collect()
        };
        for answered in answers {
            let _ = answered.await;
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.repo.remove(self.id);
    }
}

/// A target's registration, and what its detectors send it.
pub struct Detected {
    pub watcher: Watcher,
    wakes: UnboundedReceiver<()>,
}

impl Detected {
    /// Waits until `deadline`, for a `refresh`, or for a detector to say the
    /// target moved; what piled up counts as one. A `refresh` has the
    /// target's detectors poll first: what they wake is the reading that
    /// follows. A rate limit (`limited`) holds the wait whatever comes.
    /// `false` once the core is gone.
    pub async fn wait(
        &mut self,
        deadline: Instant,
        refreshes: &mut UnboundedReceiver<()>,
        limited: bool,
    ) -> bool {
        let mut refreshed = false;
        loop {
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => break,
                refresh = refreshes.recv() => {
                    if refresh.is_none() {
                        return false;
                    }
                    while refreshes.try_recv().is_ok() {}
                    refreshed = true;
                    if !limited {
                        break;
                    }
                }
                Some(()) = self.wakes.recv() => {
                    if !limited {
                        break;
                    }
                }
            }
        }
        if refreshed {
            self.watcher.poll_now().await;
        }
        while self.wakes.try_recv().is_ok() {}
        true
    }
}

/// One detector of a repository.
struct Detector {
    repo: Weak<Repository>,
    kind: Kind,
    access: Access,
    owner: String,
    name: String,
    /// The last list read.
    last: Option<Arc<Value>>,
    throttle: Throttle,
    /// Until when a `refresh` keeps the pace fast.
    window_until: Option<Instant>,
    /// Whether the last poll failed: a lasting failure is logged once.
    failing: bool,
}

impl Detector {
    fn new(repo: &Arc<Repository>, kind: Kind) -> Self {
        Self {
            repo: Arc::downgrade(repo),
            kind,
            access: repo.access.clone(),
            owner: repo.owner.clone(),
            name: repo.name.clone(),
            last: None,
            throttle: Throttle::default(),
            window_until: None,
            failing: false,
        }
    }

    /// Polls, wakes what moved, waits for its period or to be asked, and
    /// again. Those who asked are answered once the poll that follows is
    /// done; during a rate limit, at once, without a poll.
    async fn run(mut self, mut asked: UnboundedReceiver<oneshot::Sender<()>>) {
        let mut waiting: Vec<oneshot::Sender<()>> = Vec::new();
        loop {
            let limited = self.poll().await;
            for answer in waiting.drain(..) {
                let _ = answer.send(());
            }
            let now = Instant::now();
            let deadline = now + limited.unwrap_or_else(|| self.period(now));
            loop {
                tokio::select! {
                    () = tokio::time::sleep_until(deadline) => break,
                    answer = asked.recv() => {
                        let Some(answer) = answer else {
                            return;
                        };
                        self.window_until = Some(Instant::now() + WINDOW_FOR);
                        if limited.is_some() {
                            let _ = answer.send(());
                            continue;
                        }
                        waiting.push(answer);
                        while let Ok(answer) = asked.try_recv() {
                            waiting.push(answer);
                        }
                        break;
                    }
                }
            }
        }
    }

    /// The window pace after a `refresh`, else the throttle's.
    fn period(&self, now: Instant) -> Duration {
        if self.window_until.is_some_and(|until| now < until) {
            RHYTHM.window
        } else {
            self.throttle.period()
        }
    }

    /// One poll, and how long a rate limit asks to wait. Without a token,
    /// nothing is polled.
    async fn poll(&mut self) -> Option<Duration> {
        let a = &self.access;
        let token = owner_token(&a.tokens, &a.accounts, &a.credentials, &self.owner).await?;
        let url = format!(
            "{}/repos/{}/{}/{}",
            a.api,
            self.owner,
            self.name,
            self.kind.path()
        );
        match a.http.get_changed(&url, &token, HEADERS).await {
            Ok((body, changed)) => {
                self.failing = false;
                if changed {
                    self.saw(body);
                } else {
                    self.throttle.answered(false);
                }
                None
            }
            Err(failure) => self.failed(failure),
        }
    }

    /// A new list: what moved since the last one wakes what it touches.
    fn saw(&mut self, body: Arc<Value>) {
        if let Some(before) = &self.last {
            self.throttle.answered(true);
            let repo = format!("{}/{}", self.owner, self.name);
            let moved = moved(self.kind, before, &body, &repo);
            if !moved.is_empty()
                && let Some(repo) = self.repo.upgrade()
            {
                repo.wake(&moved);
            }
        }
        self.last = Some(body);
    }

    /// A failed poll, and how long a rate limit asks to wait. A refused
    /// token is forgotten, as by a target.
    fn failed(&mut self, failure: http::Failure) -> Option<Duration> {
        if failure == http::Failure::Unauthorized {
            let mut tokens = self.access.tokens.lock().unwrap();
            tokens.by_owner.remove(&self.owner);
        }
        if !std::mem::replace(&mut self.failing, true) {
            let why = match &failure {
                http::Failure::Unreachable(why) => format!("unreachable: {why}"),
                http::Failure::Unreadable(why) => why.clone(),
                other => format!("{other:?}"),
            };
            crate::log(format_args!(
                "github.com/{}/{} {} detector: {why}",
                self.owner,
                self.name,
                self.kind.name()
            ));
        }
        match failure {
            http::Failure::Limited(wait) => Some(wait.unwrap_or(RHYTHM.normal)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::testing::Server;

    #[test]
    fn a_busy_detector_slows_down_and_speeds_back_up_when_it_settles() {
        let mut throttle = Throttle::default();
        let feed = |throttle: &mut Throttle, answers: &[bool]| {
            for changed in answers {
                throttle.answered(*changed);
            }
            throttle.period()
        };
        let busy = [true, true, false, true, true, false];
        assert_eq!(feed(&mut throttle, &busy[..5]), PERIOD, "not enough yet");
        assert_eq!(feed(&mut throttle, &busy[5..]), PERIOD * 2, "4 of 6");
        assert_eq!(feed(&mut throttle, &busy), PERIOD * 4);
        assert_eq!(feed(&mut throttle, &busy), SLOWEST);
        assert_eq!(feed(&mut throttle, &busy), SLOWEST, "no slower");
        let half = [true, false, true, false, true, false];
        assert_eq!(feed(&mut throttle, &half), SLOWEST, "half is not most");
        let calm = [false, false, true, false, false, false];
        assert_eq!(feed(&mut throttle, &calm), SLOWEST / 2);
        assert_eq!(feed(&mut throttle, &calm), SLOWEST / 4);
        assert_eq!(feed(&mut throttle, &calm), PERIOD);
        assert_eq!(feed(&mut throttle, &calm), PERIOD, "no faster");
    }

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
    ) -> (Arc<Repository>, Vec<UnboundedReceiver<()>>) {
        let repo = Arc::new(Repository {
            access: access(server),
            owner: "o".into(),
            name: "r".into(),
            state: Mutex::default(),
        });
        let mut wakes = Vec::new();
        let mut state = repo.state.lock().unwrap();
        for (id, interest) in interests.into_iter().enumerate() {
            let (wake, woken) = unbounded_channel();
            state.watchers.insert(id as u64, (interest, wake));
            wakes.push(woken);
        }
        drop(state);
        (repo, wakes)
    }

    /// Which targets were woken since the last call.
    fn woken(wakes: &mut [UnboundedReceiver<()>]) -> Vec<usize> {
        let mut woken = Vec::new();
        for (i, wake) in wakes.iter_mut().enumerate() {
            if wake.try_recv().is_ok() {
                woken.push(i);
            }
            while wake.try_recv().is_ok() {}
        }
        woken
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
            let mut detector = Detector::new(&repo, kind);
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
            assert_eq!(detector.throttle.recent, [false, true, false]);
        }
    }

    /// A rate limit is waited out and logged once; a refused token is
    /// forgotten.
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
        let mut runs = Detector::new(&repo, Kind::Runs);
        assert_eq!(runs.poll().await, Some(Duration::from_secs(300)));
        assert!(runs.failing);
        let mut pulls = Detector::new(&repo, Kind::Pulls);
        assert_eq!(pulls.poll().await, None);
        assert!(repo.access.tokens.lock().unwrap().by_owner.is_empty());
    }
}
