//! Change detection on Azure DevOps, by project, repository and kind. Azure
//! DevOps gives no `ETag` and counts every request against its budget, so
//! each detector compares what it reads with what it read before. Four
//! detectors, shared by every target of the provider that depends on them,
//! and running only while one does:
//!
//! - PRs, one per project: the project's active PRs, compared PR by PR on
//!   their votes, last merge source and merge commits, draft flag,
//!   auto-complete, status and title (the list has no update date). A PR
//!   that leaves the list was completed or abandoned;
//! - builds, one per project: the active builds and the 20 last completed,
//!   compared on `id`, `status`, `result` and `lastChangedDate`;
//! - pushes, one per repository: each push since the last one seen, with
//!   the refs it updated, `refs/pull/<n>/merge` included;
//! - work items, one per project: each revision since the server's
//!   watermark, compared on state, assignee, type and title; WIQL on
//!   `System.ChangedDate` when the revisions feed is refused.
//!
//! What moved wakes the targets it touches ([`Moved::touches`]), which then
//! read in detail. The first answer only sets what the next ones are
//! compared with. Statuses and policy evaluations of a PR are in none of
//! these lists: a PR target keeps reading them at its own pace.
//!
//! A detector polls every [`PERIOD`](crate::tools::throttle::PERIOD), about
//! 3 TSTU per 5-minute window for a project at the costs measured (the
//! budget is 200), slower when its answers keep changing ([`Throttle`]), at
//! [`SLOWEST`] at least while the budget left is under half of it, and not
//! before a `Retry-After`. A target's `refresh` has the detectors it depends
//! on poll at once and opens their fast window: they poll at the provider's
//! window pace for [`WINDOW_FOR`].

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::Hash;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use reqwest::header::HeaderMap;
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;
use tokio::time::Instant;

use super::{BUDGET, Context, HEADERS, Names, RHYTHM, VERSION, decode, limited, token};
use crate::tools::http::{self, encode};
use crate::tools::pace::WINDOW_FOR;
use crate::tools::throttle::{SLOWEST, Throttle};

/// How long a `refresh` waits for its detectors before the target reads
/// anyway, on a slow or unreachable forge.
const ASKED_FOR: Duration = Duration::from_secs(5);
/// The active PRs of a project one poll reads.
const PULLS: usize = 500;
/// The completed builds of a project one poll reads.
const BUILDS: usize = 20;
/// The fields a work item revision is read with: what the contract shows.
const FIELDS: &str =
    "System.Id,System.State,System.AssignedTo,System.WorkItemType,System.Title,System.ChangedDate";
/// The batches of revisions one poll reads at most; the rest waits for the
/// next poll.
const PAGES: usize = 5;
/// The work items whose last revision is kept at most; past it, they are
/// forgotten, and the next revision of each wakes its targets.
const KNOWN_MAX: usize = 10_000;
/// The work items a WIQL poll lists at most.
const CHANGED_MAX: usize = 200;
/// The share of the budget under which a detector polls at [`SLOWEST`].
const LOW_BUDGET: f64 = 0.5;

/// What a detector polls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Pulls,
    Builds,
    Pushes,
    WorkItems,
}

/// The detectors a branch's target, or a PR's, depends on.
pub const BRANCH: &[Kind] = &[Kind::Pulls, Kind::Builds, Kind::Pushes];
/// The detector the targets of a project's work items depend on.
pub const TRACKER: &[Kind] = &[Kind::WorkItems];

impl Kind {
    /// The kind, for the log.
    const fn name(self) -> &'static str {
        match self {
            Self::Pulls => "PRs",
            Self::Builds => "builds",
            Self::Pushes => "pushes",
            Self::WorkItems => "work items",
        }
    }
}

/// A detector of a project: its kind, and the repository of a pushes
/// detector, empty for the others, which serve the whole project.
type Key = (Kind, String);

fn key(kind: Kind, interest: &Interest) -> Key {
    let repo = if kind == Kind::Pushes {
        interest.repo.clone()
    } else {
        String::new()
    };
    (kind, repo)
}

/// What a target is about, for a detector to tell whether an item touches
/// it. A target updates it from its readings ([`Watcher::learn`]).
#[derive(Debug, Clone, Default)]
pub struct Interest {
    /// The detectors the target depends on.
    pub kinds: &'static [Kind],
    /// The repository of the target, in lowercase as in the identity.
    pub repo: String,
    /// The branch it reads: a branch's, or the source branch of a PR.
    pub branch: Option<String>,
    /// The PR it reads: a PR's own, or a branch's latest.
    pub pr: Option<u64>,
    /// The commit whose CI it reads.
    pub sha: Option<String>,
    /// The work item it reads.
    pub issue: Option<u64>,
    /// Whether it reads the project's open work items.
    pub tracker: bool,
}

/// An item a detector saw move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Moved {
    /// A PR of `repo`, with its source branch when it is not a fork's.
    Pull {
        repo: String,
        number: u64,
        branch: Option<String>,
    },
    /// A build of `repo`: the branch or the PR it built, and its commit.
    Build {
        repo: String,
        branch: Option<String>,
        pr: Option<u64>,
        sha: Option<String>,
    },
    /// A ref a push updated in `repo`: a branch, or a PR's merge ref.
    Ref {
        repo: String,
        branch: Option<String>,
        pr: Option<u64>,
    },
    /// A work item.
    WorkItem(u64),
}

impl Moved {
    /// Whether this move touches a target of `interest`. A PR touches its
    /// own target and that of its source branch; a build, the target of its
    /// branch, of its PR, or reading its commit; a ref, the target of its
    /// branch or of its PR, whose branch target knows it; a work item, its
    /// own target and the project's open work items.
    pub fn touches(&self, interest: &Interest) -> bool {
        let same = |a: &Option<String>, b: &Option<String>| a.is_some() && a == b;
        let pr = |n: &Option<u64>| n.is_some() && *n == interest.pr;
        match self {
            Self::Pull {
                repo,
                number,
                branch,
            } => {
                *repo == interest.repo
                    && (interest.pr == Some(*number) || same(branch, &interest.branch))
            }
            Self::Build {
                repo,
                branch,
                pr: number,
                sha,
            } => {
                *repo == interest.repo
                    && (same(branch, &interest.branch) || pr(number) || same(sha, &interest.sha))
            }
            Self::Ref {
                repo,
                branch,
                pr: number,
            } => *repo == interest.repo && (same(branch, &interest.branch) || pr(number)),
            Self::WorkItem(id) => interest.tracker || interest.issue == Some(*id),
        }
    }
}

/// The `value` list of an answer, empty without one.
fn items(answer: &Value) -> &[Value] {
    answer["value"].as_array().map_or(&[], Vec::as_slice)
}

/// The branch or the PR a ref names: `refs/heads/<branch>`, or
/// `refs/pull/<n>/merge`.
fn target_of(git_ref: &str) -> (Option<String>, Option<u64>) {
    if let Some(branch) = git_ref.strip_prefix("refs/heads/") {
        return (Some(branch.to_owned()), None);
    }
    let pr = git_ref
        .strip_prefix("refs/pull/")
        .and_then(|rest| rest.strip_suffix("/merge"))
        .and_then(|n| n.parse().ok());
    (None, pr)
}

/// A repository's name as the identity writes it.
fn repo_of(item: &Value) -> String {
    item["repository"]["name"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase()
}

/// The items of `after` new or changed since `before`, and those of
/// `before` gone from `after` that `gone` says moved by leaving.
fn compare<'a, K: Eq + Hash, S: PartialEq>(
    before: &'a [Value],
    after: &'a [Value],
    key: impl Fn(&Value) -> Option<K>,
    signature: impl Fn(&Value) -> S,
    gone: impl Fn(&Value) -> bool,
) -> Vec<&'a Value> {
    let known: HashMap<K, S> = before
        .iter()
        .filter_map(|item| Some((key(item)?, signature(item))))
        .collect();
    let mut moved: Vec<&Value> = after
        .iter()
        .filter(|item| key(item).is_some_and(|k| known.get(&k) != Some(&signature(item))))
        .collect();
    let now: Vec<K> = after.iter().filter_map(&key).collect();
    moved.extend(
        before
            .iter()
            .filter(|item| key(item).is_some_and(|k| !now.contains(&k)) && gone(item)),
    );
    moved
}

/// What moved from the active PRs `before` to the active PRs `after` of a
/// project: a PR new to the list, gone from it, or whose compared fields
/// changed.
pub fn moved_pulls(before: &Value, after: &Value) -> Vec<Moved> {
    let key = |p: &Value| Some((repo_of(p), p["pullRequestId"].as_u64()?));
    let signature = |p: &Value| {
        let votes: Vec<Value> = p["reviewers"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .map(|r| json!([r["id"], r["vote"]]))
            .collect();
        [
            p["status"].clone(),
            p["title"].clone(),
            p["isDraft"].clone(),
            p["lastMergeSourceCommit"]["commitId"].clone(),
            p["lastMergeCommit"]["commitId"].clone(),
            p["autoCompleteSetBy"]["id"].clone(),
            Value::Array(votes),
        ]
    };
    compare(items(before), items(after), key, signature, |_| true)
        .into_iter()
        .filter_map(|p| {
            let source = p["sourceRefName"].as_str().unwrap_or_default();
            Some(Moved::Pull {
                repo: repo_of(p),
                number: p["pullRequestId"].as_u64()?,
                branch: p["forkSource"]
                    .is_null()
                    .then(|| source.strip_prefix("refs/heads/").map(str::to_owned))
                    .flatten(),
            })
        })
        .collect()
}

/// What moved from the builds `before` to the builds `after` of a project:
/// a build new to the lists, whose compared fields changed, or that left
/// them before it was seen completed.
pub fn moved_builds(before: &Value, after: &Value) -> Vec<Moved> {
    let key = |b: &Value| b["id"].as_u64();
    let signature = |b: &Value| {
        [
            b["status"].clone(),
            b["result"].clone(),
            b["lastChangedDate"].clone(),
        ]
    };
    let active = |b: &Value| b["status"] != "completed";
    compare(items(before), items(after), key, signature, active)
        .into_iter()
        .map(|b| {
            let (branch, pr) = target_of(b["sourceBranch"].as_str().unwrap_or_default());
            let triggered = b["triggerInfo"]["pr.number"]
                .as_str()
                .and_then(|n| n.parse().ok());
            Moved::Build {
                repo: repo_of(b),
                branch,
                pr: pr.or(triggered),
                sha: b["sourceVersion"].as_str().map(str::to_owned),
            }
        })
        .collect()
}

/// The refs the pushes of `repo` after push `after` updated, every push
/// when `after` is `None`.
pub fn moved_refs(pushes: &Value, after: Option<u64>, repo: &str) -> Vec<Moved> {
    items(pushes)
        .iter()
        .filter(|p| {
            p["pushId"]
                .as_u64()
                .is_some_and(|id| after.is_none_or(|last| id > last))
        })
        .flat_map(|p| p["refUpdates"].as_array().map_or(&[][..], Vec::as_slice))
        .map(|update| {
            let (branch, pr) = target_of(update["name"].as_str().unwrap_or_default());
            Moved::Ref {
                repo: repo.to_owned(),
                branch,
                pr,
            }
        })
        .collect()
}

/// What a work item revision shows of the contract.
fn revision_signature(fields: &Value) -> [Value; 4] {
    [
        fields["System.State"].clone(),
        fields["System.AssignedTo"].clone(),
        fields["System.WorkItemType"].clone(),
        fields["System.Title"].clone(),
    ]
}

/// The watermark of a batch of revisions: its `continuationToken`, else the
/// one its `nextLink` carries.
fn watermark_of(batch: &Value) -> Option<String> {
    if let Some(token) = batch["continuationToken"].as_str() {
        return Some(token.to_owned());
    }
    let link = batch["nextLink"].as_str()?;
    let query = link.split_once('?')?.1;
    let token = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("continuationToken="))?;
    decode(token)
}

/// The time now, as Azure DevOps reads a date and time.
fn now() -> String {
    jiff::Timestamp::now()
        .strftime("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// Whether an answer says the budget left is under [`LOW_BUDGET`] of it:
/// Azure DevOps sends `X-RateLimit-Remaining` only as it runs out.
fn budget_low(headers: &HeaderMap) -> bool {
    let number = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<f64>().ok())
    };
    number("x-ratelimit-remaining").is_some_and(|remaining| {
        remaining < number("x-ratelimit-limit").unwrap_or(BUDGET) * LOW_BUDGET
    })
}

/// The projects detected, shared by every target of the provider.
#[derive(Clone, Default)]
pub struct Detection {
    projects: Arc<Mutex<HashMap<String, Weak<Project>>>>,
}

impl Detection {
    /// Registers a target of the project of `names` about `interest`: the
    /// detectors it depends on run while it stays registered.
    pub fn watch(&self, context: &Context, names: &Names, interest: Interest) -> Detected {
        let key = names.tracker();
        let project = {
            let mut projects = self.projects.lock().unwrap();
            projects.retain(|_, project| project.strong_count() > 0);
            if let Some(project) = projects.get(&key).and_then(Weak::upgrade) {
                project
            } else {
                let project = Arc::new(Project {
                    context: context.clone(),
                    names: Names {
                        repo: String::new(),
                        ..names.clone()
                    },
                    state: Mutex::default(),
                });
                projects.insert(key, Arc::downgrade(&project));
                project
            }
        };
        let (wake, wakes) = unbounded_channel();
        let id = project.add(interest, wake);
        Detected {
            watcher: Watcher { project, id },
            wakes,
        }
    }

    /// The projects detected and their detectors running, for tests.
    #[cfg(test)]
    pub fn running(&self) -> Vec<(String, usize)> {
        let projects = self.projects.lock().unwrap();
        let mut running: Vec<(String, usize)> = projects
            .iter()
            .filter_map(|(key, project)| {
                let project = project.upgrade()?;
                let count = project.state.lock().unwrap().detectors.len();
                Some((key.clone(), count))
            })
            .collect();
        running.sort();
        running
    }
}

/// One project detected, held by the targets registered on it.
pub struct Project {
    context: Context,
    /// The organisation and the project, without a repository.
    names: Names,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    last_id: u64,
    /// Each target registered, and where to wake it.
    watchers: HashMap<u64, (Interest, UnboundedSender<()>)>,
    detectors: HashMap<Key, Running>,
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

impl Project {
    /// Registers a target, starting the detectors it needs that no other
    /// target started.
    fn add(self: &Arc<Self>, interest: Interest, wake: UnboundedSender<()>) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.last_id += 1;
        let id = state.last_id;
        for kind in interest.kinds {
            let key = key(*kind, &interest);
            if let Entry::Vacant(vacant) = state.detectors.entry(key) {
                let (asks, asked) = unbounded_channel();
                let detector = Detector::new(self, *kind, &vacant.key().1);
                let task = tokio::spawn(detector.run(asked)).abort_handle();
                vacant.insert(Running { task, asks });
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
        detectors.retain(|k, _| {
            watchers
                .values()
                .any(|(i, _)| i.kinds.iter().any(|kind| key(*kind, i) == *k))
        });
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
    project: Arc<Project>,
    id: u64,
}

impl Watcher {
    /// Updates what the target is about.
    pub fn learn(&self, update: impl FnOnce(&mut Interest)) {
        let mut state = self.project.state.lock().unwrap();
        if let Some((interest, _)) = state.watchers.get_mut(&self.id) {
            update(interest);
        }
    }

    /// Has the detectors the target depends on poll now, opening their fast
    /// window, and returns once they did, could not, or [`ASKED_FOR`] passed.
    pub async fn poll_now(&self) {
        let answers: Vec<oneshot::Receiver<()>> = {
            let state = self.project.state.lock().unwrap();
            let Some((interest, _)) = state.watchers.get(&self.id) else {
                return;
            };
            interest
                .kinds
                .iter()
                .filter_map(|kind| state.detectors.get(&key(*kind, interest)))
                .map(|running| {
                    let (answer, answered) = oneshot::channel();
                    let _ = running.asks.send(answer);
                    answered
                })
                .collect()
        };
        let all = async {
            for answered in answers {
                let _ = answered.await;
            }
        };
        let _ = tokio::time::timeout(ASKED_FOR, all).await;
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.project.remove(self.id);
    }
}

/// A target's registration, and what its detectors send it.
pub struct Detected {
    pub watcher: Watcher,
    wakes: UnboundedReceiver<()>,
}

impl Detected {
    /// Waits until `deadline`, for a `refresh`, or for a detector to say the
    /// target moved; what piled up counts as one. A `refresh` calls
    /// `refreshed`, and has the target's detectors poll first: what they
    /// wake is the reading that follows. A rate limit (`limited`) holds the
    /// wait whatever comes. `false` once the core is gone.
    pub async fn wait(
        &mut self,
        deadline: Instant,
        refreshes: &mut UnboundedReceiver<()>,
        limited: bool,
        mut refreshed_now: impl FnMut(),
    ) -> bool {
        let mut refreshed = false;
        loop {
            // A pending `refresh` wins over a wake or the deadline, for its
            // detectors to poll before the reading.
            tokio::select! {
                biased;
                refresh = refreshes.recv() => {
                    if refresh.is_none() {
                        return false;
                    }
                    while refreshes.try_recv().is_ok() {}
                    refreshed = true;
                    refreshed_now();
                    if !limited {
                        break;
                    }
                }
                Some(()) = self.wakes.recv() => {
                    if !limited {
                        break;
                    }
                }
                () = tokio::time::sleep_until(deadline) => break,
            }
        }
        if refreshed {
            self.watcher.poll_now().await;
        }
        while self.wakes.try_recv().is_ok() {}
        true
    }
}

/// Where a pushes detector stands: the last push seen, and its date, which
/// the next poll starts from.
#[derive(Default)]
struct Pushes {
    started: bool,
    last: Option<u64>,
    since: Option<String>,
}

/// Where a work items detector stands.
#[derive(Default)]
struct Revisions {
    /// Whether the revisions feed answered once.
    started: bool,
    /// The server's watermark, the next batch's start.
    watermark: Option<String>,
    /// Without a watermark, when the last poll started.
    since: Option<String>,
    /// The last revision of each work item seen.
    known: HashMap<u64, [Value; 4]>,
    /// Whether the feed was refused, WIQL taking its place.
    wiql: bool,
    /// The time of WIQL's last answer (`asOf`), the next query's start.
    as_of: Option<String>,
}

/// One detector of a project.
struct Detector {
    project: Weak<Project>,
    kind: Kind,
    context: Context,
    /// The project, and the repository of a pushes detector.
    names: Names,
    /// PRs and builds: the last lists read.
    last: Option<Value>,
    pushes: Pushes,
    revisions: Revisions,
    throttle: Throttle,
    /// Until when a `refresh` keeps the pace fast.
    window_until: Option<Instant>,
    /// Whether the last poll's answers said the budget runs low.
    low: bool,
    /// Whether the cost of a poll was logged, once per detector.
    costed: bool,
    /// Whether the last poll failed: a lasting failure is logged once.
    failing: bool,
}

impl Detector {
    fn new(project: &Arc<Project>, kind: Kind, repo: &str) -> Self {
        Self {
            project: Arc::downgrade(project),
            kind,
            context: project.context.clone(),
            names: Names {
                repo: repo.to_owned(),
                ..project.names.clone()
            },
            last: None,
            pushes: Pushes::default(),
            revisions: Revisions::default(),
            throttle: Throttle::default(),
            window_until: None,
            low: false,
            costed: false,
            failing: false,
        }
    }

    /// Polls, wakes what moved, waits for its period or to be asked, and
    /// again. Those who asked are answered once the poll that follows is
    /// done; while it cannot poll (a rate limit, no token), at once.
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

    /// The window pace after a `refresh`, else the throttle's; at
    /// [`SLOWEST`] at least while the budget runs low.
    fn period(&self, now: Instant) -> Duration {
        let period = if self.window_until.is_some_and(|until| now < until) {
            RHYTHM.window
        } else {
            self.throttle.period()
        };
        if self.low {
            period.max(SLOWEST)
        } else {
            period
        }
    }

    /// Where the detector reads, for the log.
    fn at(&self) -> String {
        let at = if self.kind == Kind::Pushes {
            self.names.id()
        } else {
            self.names.tracker()
        };
        format!("{at} {} detector", self.kind.name())
    }

    /// One poll, and how long to wait when it cannot poll: what a rate
    /// limit asks, or the normal pace without a token.
    async fn poll(&mut self) -> Option<Duration> {
        let Some(token) = token(&self.context).await else {
            return Some(RHYTHM.normal);
        };
        self.low = false;
        let polled = match self.kind {
            Kind::Pulls => self.poll_pulls(&token).await,
            Kind::Builds => self.poll_builds(&token).await,
            Kind::Pushes => self.poll_pushes(&token).await,
            Kind::WorkItems => self.poll_work_items(&token).await,
        };
        match polled {
            Ok(moved) => {
                self.failing = false;
                // The first answer only sets what the next are compared with.
                if let Some(moved) = moved {
                    self.throttle.answered(!moved.is_empty());
                    if !moved.is_empty()
                        && let Some(project) = self.project.upgrade()
                    {
                        project.wake(&moved);
                    }
                }
                None
            }
            Err(failure) => self.failed(failure).await,
        }
    }

    /// GETs `path` of the API, noting what its answer says of the budget.
    async fn get(&mut self, token: &str, path: &str) -> Result<Arc<Value>, http::Failure> {
        let url = format!("{}{path}", self.context.api);
        let (body, headers) = self.context.http.get_answer(&url, token, HEADERS).await?;
        self.measured(&headers);
        limited(&headers)?;
        Ok(body)
    }

    /// POSTs `body` to `path` of the API, as [`Detector::get`].
    async fn post(
        &mut self,
        token: &str,
        path: &str,
        body: &Value,
    ) -> Result<Value, http::Failure> {
        let url = format!("{}{path}", self.context.api);
        let (body, headers) = self
            .context
            .http
            .post_answer(&url, token, HEADERS, body)
            .await?;
        self.measured(&headers);
        limited(&headers)?;
        Ok(body)
    }

    /// Notes whether the budget runs low, and logs once what a call costs
    /// (`X-RateLimit-Cost`, in TSTU), which Azure DevOps does not send on
    /// every answer.
    fn measured(&mut self, headers: &HeaderMap) {
        self.low |= budget_low(headers);
        if std::mem::replace(&mut self.costed, true) {
            return;
        }
        let cost = headers
            .get("x-ratelimit-cost")
            .and_then(|v| v.to_str().ok());
        match cost {
            Some(cost) => crate::log(format_args!("{}: {cost} TSTU per call", self.at())),
            None => crate::log(format_args!("{}: no cost on its answers", self.at())),
        }
    }

    /// The PRs of the project that moved since the last poll.
    async fn poll_pulls(&mut self, token: &str) -> Result<Option<Vec<Moved>>, http::Failure> {
        let path = format!(
            "{}/git/pullrequests?searchCriteria.status=active&$top={PULLS}&{VERSION}",
            self.names.project_api()
        );
        let body = Arc::unwrap_or_clone(self.get(token, &path).await?);
        Ok(self.compared(body, moved_pulls))
    }

    /// The builds of the project that moved since the last poll: every
    /// active build, and the last completed, in two calls. Unlike a list
    /// that pages, a long build cannot leave the first.
    async fn poll_builds(&mut self, token: &str) -> Result<Option<Vec<Moved>>, http::Failure> {
        let api = self.names.project_api();
        let active = format!("{api}/build/builds?statusFilter=inProgress,notStarted&{VERSION}");
        let active = self.get(token, &active).await?;
        let done = format!(
            "{api}/build/builds?queryOrder=finishTimeDescending&statusFilter=completed&$top={BUILDS}&{VERSION}"
        );
        let done = self.get(token, &done).await?;
        let all: Vec<Value> = items(&active).iter().chain(items(&done)).cloned().collect();
        Ok(self.compared(json!({ "value": all }), moved_builds))
    }

    /// What `moved` says moved from the last lists to `body`, kept for the
    /// next poll; `None` for the first.
    fn compared(
        &mut self,
        body: Value,
        moved: fn(&Value, &Value) -> Vec<Moved>,
    ) -> Option<Vec<Moved>> {
        let found = self.last.as_ref().map(|before| moved(before, &body));
        self.last = Some(body);
        found
    }

    /// The refs the repository's pushes updated since the last push seen.
    /// The first poll reads the last push only; the next read from its
    /// date, the server's, and keep the pushes after it.
    // https://learn.microsoft.com/en-us/rest/api/azure/devops/git/pushes/list?view=azure-devops-rest-7.1
    async fn poll_pushes(&mut self, token: &str) -> Result<Option<Vec<Moved>>, http::Failure> {
        let repo_api = self.names.repo_api();
        let path = match &self.pushes.since {
            Some(since) => format!(
                "{repo_api}/pushes?searchCriteria.fromDate={}&searchCriteria.includeRefUpdates=true&{VERSION}",
                encode(since)
            ),
            None => {
                format!("{repo_api}/pushes?$top=1&searchCriteria.includeRefUpdates=true&{VERSION}")
            }
        };
        let body = self.get(token, &path).await?;
        let started = std::mem::replace(&mut self.pushes.started, true);
        let moved = moved_refs(&body, self.pushes.last, &self.names.repo);
        for push in items(&body) {
            if let Some(id) = push["pushId"].as_u64()
                && self.pushes.last.is_none_or(|last| id > last)
            {
                self.pushes.last = Some(id);
                self.pushes.since = push["date"].as_str().map(str::to_owned);
            }
        }
        Ok(started.then_some(moved))
    }

    /// The work items of the project whose revisions since the last poll
    /// changed what the contract shows. The feed's watermark is the
    /// server's; the first poll starts from now. A feed refused (`404`, or
    /// `403` while WIQL answers) gives way to WIQL for good.
    // https://learn.microsoft.com/en-us/rest/api/azure/devops/wit/reporting-work-item-revisions/read-reporting-revisions-get?view=azure-devops-rest-7.1
    async fn poll_work_items(&mut self, token: &str) -> Result<Option<Vec<Moved>>, http::Failure> {
        if self.revisions.wiql {
            return self.poll_wiql(token).await;
        }
        let api = self.names.project_api();
        let first = !self.revisions.started;
        let mut moved = Vec::new();
        for page in 0..PAGES {
            let start = match &self.revisions.watermark {
                Some(watermark) => format!("continuationToken={}", encode(watermark)),
                None => {
                    let since = self.revisions.since.clone().unwrap_or_else(now);
                    format!("startDateTime={}", encode(&since))
                }
            };
            let path =
                format!("{api}/wit/reporting/workitemrevisions?{start}&fields={FIELDS}&{VERSION}");
            let started_at = now();
            let batch = match self.get(token, &path).await {
                Ok(batch) => batch,
                Err(failure @ (http::Failure::NotFound | http::Failure::Unauthorized))
                    if first && page == 0 =>
                {
                    return self.refused(token, failure).await;
                }
                Err(failure) => return Err(failure),
            };
            self.revisions.started = true;
            let values = batch["values"].as_array().map_or(&[][..], Vec::as_slice);
            for revision in values {
                let fields = &revision["fields"];
                let Some(id) = revision["id"]
                    .as_u64()
                    .or_else(|| fields["System.Id"].as_u64())
                else {
                    continue;
                };
                let signature = revision_signature(fields);
                let known = &mut self.revisions.known;
                if known.get(&id) != Some(&signature) {
                    if known.len() >= KNOWN_MAX {
                        known.clear();
                    }
                    known.insert(id, signature);
                    moved.push(Moved::WorkItem(id));
                }
            }
            match watermark_of(&batch) {
                Some(watermark) => self.revisions.watermark = Some(watermark),
                None => self.revisions.since = Some(started_at),
            }
            if batch["isLastBatch"].as_bool() != Some(false) {
                break;
            }
        }
        Ok((!first).then_some(moved))
    }

    /// The revisions feed refused with `failure`: WIQL takes its place for
    /// good when it answers, or when the feed is not there (`404`). A `403`
    /// WIQL shares is the token's, not the feed's.
    async fn refused(
        &mut self,
        token: &str,
        failure: http::Failure,
    ) -> Result<Option<Vec<Moved>>, http::Failure> {
        let polled = self.poll_wiql(token).await;
        if polled.is_ok() || failure == http::Failure::NotFound {
            self.revisions.wiql = true;
            crate::log(format_args!(
                "{}: the revisions feed answered {failure:?}, WIQL takes its place",
                self.at()
            ));
        }
        polled
    }

    /// The work items of the project changed since WIQL's last answer.
    /// `timePrecision` lets the date carry a time (a `400` without it).
    // https://learn.microsoft.com/en-us/rest/api/azure/devops/wit/wiql/query-by-wiql?view=azure-devops-rest-7.1
    async fn poll_wiql(&mut self, token: &str) -> Result<Option<Vec<Moved>>, http::Failure> {
        let first = self.revisions.as_of.is_none();
        let since = self.revisions.as_of.clone().unwrap_or_else(now);
        let query = format!(
            "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project \
             AND [System.ChangedDate] > '{since}' ORDER BY [System.ChangedDate] DESC"
        );
        let path = format!(
            "{}/wit/wiql?timePrecision=true&$top={CHANGED_MAX}&{VERSION}",
            self.names.project_api()
        );
        let answer = self.post(token, &path, &json!({ "query": query })).await?;
        self.revisions.as_of = Some(answer["asOf"].as_str().map_or(since, str::to_owned));
        let moved = answer["workItems"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter_map(|item| item["id"].as_u64().map(Moved::WorkItem))
            .collect();
        Ok((!first).then_some(moved))
    }

    /// A failed poll, and how long a rate limit asks to wait. A refused
    /// token is forgotten, as by a target.
    async fn failed(&mut self, failure: http::Failure) -> Option<Duration> {
        if failure == http::Failure::Unauthorized {
            *self.context.token.lock().await = None;
        }
        if !std::mem::replace(&mut self.failing, true) {
            let why = match &failure {
                http::Failure::Unreachable(why) => format!("unreachable: {why}"),
                http::Failure::Unreadable(why) => why.clone(),
                other => format!("{other:?}"),
            };
            crate::log(format_args!("{}: {why}", self.at()));
        }
        match failure {
            http::Failure::Limited(wait) => Some(wait.unwrap_or(RHYTHM.normal)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::Stamp;
    use crate::testing::Server;
    use crate::tools::throttle::PERIOD;

    fn pull(repo: &str, number: u64, branch: &str, vote: i64) -> Value {
        json!({ "repository": { "name": repo }, "pullRequestId": number, "status": "active",
            "title": "t", "isDraft": false, "sourceRefName": format!("refs/heads/{branch}"),
            "lastMergeSourceCommit": { "commitId": "a" }, "lastMergeCommit": { "commitId": "m" },
            "reviewers": [{ "id": "me", "vote": vote }] })
    }

    fn build(id: u64, repo: &str, git_ref: &str, status: &str, sha: &str) -> Value {
        json!({ "id": id, "status": status, "result": null, "lastChangedDate": "t",
            "repository": { "name": repo }, "sourceBranch": git_ref, "sourceVersion": sha })
    }

    fn push(id: u64, refs: &[&str]) -> Value {
        let refs: Vec<Value> = refs.iter().map(|r| json!({ "name": r })).collect();
        json!({ "pushId": id, "date": format!("2026-11-13T10:00:{id:02}Z"), "refUpdates": refs })
    }

    fn revision(id: u64, state: &str) -> Value {
        json!({ "id": id, "rev": 2, "fields": { "System.Id": id, "System.State": state,
            "System.Title": "t", "System.WorkItemType": "Bug", "System.ChangedDate": "t" } })
    }

    fn list(items: Vec<Value>) -> Value {
        json!({ "value": items })
    }

    #[test]
    fn what_moved_is_what_is_new_changed_or_gone() {
        let before = list(vec![pull("R", 7, "feat", 0), pull("r", 8, "old", 0)]);
        let mut forked = pull("r", 9, "feat", 0);
        forked["forkSource"] = json!({ "name": "refs/heads/feat" });
        let after = list(vec![pull("r", 7, "feat", 10), forked]);
        assert_eq!(
            moved_pulls(&before, &after),
            [
                Moved::Pull {
                    repo: "r".into(),
                    number: 7,
                    branch: Some("feat".into())
                },
                Moved::Pull {
                    repo: "r".into(),
                    number: 9,
                    branch: None
                },
                Moved::Pull {
                    repo: "r".into(),
                    number: 8,
                    branch: Some("old".into())
                },
            ],
            "a vote, a fork's PR, a PR completed"
        );
        assert!(moved_pulls(&after, &after).is_empty());
        let before = list(vec![
            build(1, "r", "refs/heads/feat", "inProgress", "a"),
            build(2, "r", "refs/heads/main", "completed", "b"),
            build(3, "r", "refs/heads/gone", "notStarted", "c"),
        ]);
        let mut triggered = build(4, "r", "refs/heads/x", "notStarted", "d");
        triggered["triggerInfo"] = json!({ "pr.number": "12" });
        let after = list(vec![
            build(1, "r", "refs/heads/feat", "completed", "a"),
            build(5, "r", "refs/pull/9/merge", "notStarted", "m"),
            triggered,
        ]);
        let target = |b: Option<&str>, pr, sha: &str| Moved::Build {
            repo: "r".into(),
            branch: b.map(str::to_owned),
            pr,
            sha: Some(sha.into()),
        };
        assert_eq!(
            moved_builds(&before, &after),
            [
                target(Some("feat"), None, "a"),
                target(None, Some(9), "m"),
                target(Some("x"), Some(12), "d"),
                target(Some("gone"), None, "c"),
            ],
            "a completed build out of the lists moved nothing"
        );
        let pushes = list(vec![
            push(12, &["refs/heads/feat", "refs/pull/7/merge"]),
            push(11, &["refs/tags/v1"]),
            push(10, &["refs/heads/old"]),
        ]);
        let ref_of = |branch: Option<&str>, pr| Moved::Ref {
            repo: "r".into(),
            branch: branch.map(str::to_owned),
            pr,
        };
        assert_eq!(
            moved_refs(&pushes, Some(10), "r"),
            [
                ref_of(Some("feat"), None),
                ref_of(None, Some(7)),
                ref_of(None, None)
            ]
        );
        assert_eq!(
            watermark_of(&json!({ "nextLink": "https://h/x?continuationToken=3%3B2%3B1&a=b" })),
            Some("3;2;1".into())
        );
        assert_eq!(
            watermark_of(&json!({ "continuationToken": "w", "nextLink": "h?continuationToken=x" })),
            Some("w".into())
        );
    }

    /// A forge whose every answer is `bodies[version]`.
    async fn lists(version: Arc<Mutex<usize>>, bodies: Vec<Value>) -> Server {
        Server::start(move |_| {
            let v = *version.lock().unwrap();
            (200, vec![], bodies[v].to_string())
        })
        .await
    }

    async fn context(server: &Server) -> Context {
        let azure = super::super::Azure::new(
            http::Http::new().unwrap(),
            server.base(),
            crate::credentials::testing::none(),
        );
        *azure.context.token.lock().await = Some(("t".into(), None, Stamp::default()));
        azure.context
    }

    /// A project with a target per interest, and no detector running: the
    /// test polls them itself.
    async fn project(
        server: &Server,
        interests: Vec<Interest>,
    ) -> (Arc<Project>, Vec<UnboundedReceiver<()>>) {
        let project = Arc::new(Project {
            context: context(server).await,
            names: Names {
                org: "o".into(),
                project: "p".into(),
                repo: String::new(),
            },
            state: Mutex::default(),
        });
        let mut wakes = Vec::new();
        let mut state = project.state.lock().unwrap();
        for (id, interest) in interests.into_iter().enumerate() {
            let (wake, woken) = unbounded_channel();
            state.watchers.insert(id as u64, (interest, wake));
            wakes.push(woken);
        }
        drop(state);
        (project, wakes)
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
        let branch = |repo: &str, branch: &str, pr, sha: &str| Interest {
            kinds: BRANCH,
            repo: repo.into(),
            branch: Some(branch.into()),
            pr,
            sha: Some(sha.into()),
            ..Interest::default()
        };
        vec![
            // 0: the branch `feat` of `r`, at `a`, with PR 7.
            branch("r", "feat", Some(7), "a"),
            // 1: the branch `other` of `r`, without PR.
            branch("r", "other", None, "z"),
            // 2: the branch `feat` of `s`, with PR 9.
            branch("s", "feat", Some(9), "f"),
            // 3: the open work items.
            Interest {
                kinds: TRACKER,
                tracker: true,
                ..Interest::default()
            },
            // 4: work item 3.
            Interest {
                kinds: TRACKER,
                issue: Some(3),
                ..Interest::default()
            },
        ]
    }

    /// The first answer wakes nothing; a change wakes only the targets it
    /// touches; the same content again wakes nothing.
    #[tokio::test]
    async fn a_change_wakes_only_the_targets_touched_and_the_same_content_nothing() {
        let batch = |values: Vec<Value>, token: &str| json!({ "values": values, "continuationToken": token, "isLastBatch": true });
        let cases = [
            (
                Kind::Pulls,
                list(vec![pull("r", 7, "feat", 0)]),
                list(vec![pull("r", 7, "feat", 10)]),
                vec![0],
            ),
            (
                Kind::Pulls,
                list(vec![pull("r", 7, "feat", 0)]),
                list(vec![]),
                vec![0],
            ),
            (
                Kind::Pulls,
                list(vec![]),
                list(vec![pull("s", 12, "feat", 0)]),
                vec![2],
            ),
            (
                Kind::Builds,
                list(vec![build(1, "r", "refs/heads/feat", "inProgress", "a")]),
                list(vec![build(1, "r", "refs/heads/feat", "completed", "a")]),
                vec![0],
            ),
            (
                Kind::Builds,
                list(vec![]),
                list(vec![build(2, "s", "refs/pull/9/merge", "notStarted", "m")]),
                vec![2],
            ),
            (
                Kind::Builds,
                list(vec![]),
                list(vec![build(3, "r", "refs/heads/main", "notStarted", "z")]),
                vec![1],
            ),
            (
                Kind::Pushes,
                list(vec![push(10, &[])]),
                list(vec![push(11, &["refs/heads/other"]), push(10, &[])]),
                vec![1],
            ),
            (
                Kind::Pushes,
                list(vec![push(10, &[])]),
                list(vec![push(11, &["refs/pull/7/merge"]), push(10, &[])]),
                vec![0],
            ),
            (
                Kind::WorkItems,
                batch(vec![], "w1"),
                batch(vec![revision(3, "Active")], "w2"),
                vec![3, 4],
            ),
        ];
        for (kind, before, after, expected) in cases {
            let version = Arc::new(Mutex::new(0));
            let server = lists(version.clone(), vec![before, after]).await;
            let (project, mut wakes) = project(&server, interests()).await;
            let mut detector = Detector::new(&project, kind, "r");
            assert_eq!(detector.poll().await, None);
            assert!(woken(&mut wakes).is_empty(), "{kind:?}: the first answer");
            assert_eq!(detector.poll().await, None);
            assert!(woken(&mut wakes).is_empty(), "{kind:?}: nothing changed");
            *version.lock().unwrap() = 1;
            detector.poll().await;
            assert_eq!(woken(&mut wakes), expected, "{kind:?}");
            detector.poll().await;
            assert!(woken(&mut wakes).is_empty(), "{kind:?}: the same after");
            assert_eq!(detector.throttle.recent(), [false, true, false]);
        }
    }

    /// Pushes are read from the date of the last one seen, the server's.
    #[tokio::test]
    async fn pushes_are_read_from_the_last_seen() {
        let version = Arc::new(Mutex::new(0));
        let bodies = vec![
            list(vec![push(10, &[])]),
            list(vec![push(11, &["refs/heads/feat"]), push(10, &[])]),
        ];
        let server = lists(version.clone(), bodies).await;
        let (project, _) = project(&server, vec![]).await;
        let mut detector = Detector::new(&project, Kind::Pushes, "r");
        detector.poll().await;
        *version.lock().unwrap() = 1;
        detector.poll().await;
        detector.poll().await;
        let requests = server.requests();
        let paths: Vec<&str> = requests
            .iter()
            .map(|r| r.lines().next().unwrap_or_default())
            .collect();
        assert!(paths[0].contains(
            "/o/p/_apis/git/repositories/r/pushes?$top=1&searchcriteria.includerefupdates=true&"
        ));
        assert!(paths[1].contains("pushes?searchcriteria.fromdate=2026-11-13t10%3a00%3a10z&"));
        assert!(paths[1].contains("&searchcriteria.includerefupdates=true&"));
        assert!(paths[2].contains("searchcriteria.fromdate=2026-11-13t10%3a00%3a11z&"));
    }

    /// The revisions feed starts from now, reads its batches up to the last,
    /// and goes on from the server's watermark.
    #[tokio::test]
    async fn the_revisions_watermark_advances() {
        let server = Server::start(|request| {
            let body = if request.contains("startdatetime=") {
                json!({ "values": [], "continuationToken": "w1", "isLastBatch": false })
            } else if request.contains("continuationtoken=w1&") {
                json!({ "values": [revision(5, "New")], "continuationToken": "w2",
                    "isLastBatch": true })
            } else if request.contains("continuationtoken=w2&") {
                json!({ "values": [revision(3, "Active")], "continuationToken": "w3",
                    "isLastBatch": true })
            } else {
                json!({ "values": [], "continuationToken": "w3", "isLastBatch": true })
            };
            (200, vec![], body.to_string())
        })
        .await;
        let (project, mut wakes) = project(&server, interests()).await;
        let mut detector = Detector::new(&project, Kind::WorkItems, "");
        detector.poll().await;
        assert!(
            woken(&mut wakes).is_empty(),
            "the first batches set the start"
        );
        assert_eq!(server.requests().len(), 2, "both batches of the first poll");
        detector.poll().await;
        assert_eq!(woken(&mut wakes), [3, 4]);
        detector.poll().await;
        let requests = server.requests();
        assert!(requests[3].contains("continuationtoken=w3&"));
        let fields = "&fields=system.id,system.state,system.assignedto,system.workitemtype,\
                      system.title,system.changeddate&";
        assert!(requests.iter().all(|r| {
            r.starts_with("get /o/p/_apis/wit/reporting/workitemrevisions?") && r.contains(fields)
        }));
    }

    /// A feed that is not there gives way to WIQL on the changed date, from
    /// the time of its last answer.
    #[tokio::test]
    async fn wiql_takes_over_from_a_feed_not_found() {
        let server = Server::start(|request| {
            if request.contains("/workitemrevisions?") {
                return (404, vec![], String::new());
            }
            let body = if request.contains("> '2026-11-13t10:00:00.5z'") {
                json!({ "asOf": "2026-11-13T10:00:10.5Z", "workItems": [{ "id": 3 }] })
            } else {
                json!({ "asOf": "2026-11-13T10:00:00.5Z", "workItems": [{ "id": 4 }] })
            };
            (200, vec![], body.to_string())
        })
        .await;
        let (project, mut wakes) = project(&server, interests()).await;
        let mut detector = Detector::new(&project, Kind::WorkItems, "");
        assert_eq!(detector.poll().await, None);
        assert!(detector.revisions.wiql);
        assert!(
            woken(&mut wakes).is_empty(),
            "WIQL's first answer sets the start"
        );
        detector.poll().await;
        assert_eq!(woken(&mut wakes), [3, 4]);
        detector.poll().await;
        let requests = server.requests();
        let feed = requests
            .iter()
            .filter(|r| r.contains("/workitemrevisions?"))
            .count();
        assert_eq!(feed, 1, "the feed is asked no more");
        let last = requests.last().unwrap();
        assert!(last.starts_with("post /o/p/_apis/wit/wiql?timeprecision=true&"));
        assert!(last.contains("[system.changeddate] > '2026-11-13t10:00:10.5z'"));
    }

    /// A feed refused along with WIQL is the token's refusal: the feed is
    /// kept.
    #[tokio::test]
    async fn a_token_refused_everywhere_keeps_the_feed() {
        let server = Server::start(|_| (403, vec![], String::new())).await;
        let (project, _) = project(&server, vec![]).await;
        let mut detector = Detector::new(&project, Kind::WorkItems, "");
        assert_eq!(detector.poll().await, None);
        assert!(!detector.revisions.wiql);
        assert!(detector.failing);
        assert!(project.context.token.lock().await.is_none());
    }

    /// A budget under half its size slows the detector to the slowest pace,
    /// window or not; a `Retry-After` holds it; a budget recovered lets it
    /// speed back up.
    #[tokio::test]
    async fn a_low_budget_slows_the_detector_down() {
        let answer = Arc::new(Mutex::new(0));
        let given = answer.clone();
        let server = Server::start(move |_| {
            let headers = match *given.lock().unwrap() {
                0 => vec![
                    ("X-RateLimit-Remaining", "80"),
                    ("X-RateLimit-Limit", "200"),
                ],
                1 => vec![("Retry-After", "30")],
                _ => vec![],
            };
            (200, headers, list(vec![]).to_string())
        })
        .await;
        let (project, _) = project(&server, vec![]).await;
        let mut detector = Detector::new(&project, Kind::Pulls, "");
        let now = Instant::now();
        assert_eq!(detector.poll().await, None);
        assert_eq!(detector.period(now), SLOWEST);
        detector.window_until = Some(now + WINDOW_FOR);
        assert_eq!(detector.period(now), SLOWEST, "the budget wins");
        *answer.lock().unwrap() = 1;
        assert_eq!(detector.poll().await, Some(Duration::from_secs(30)));
        *answer.lock().unwrap() = 2;
        assert_eq!(detector.poll().await, None);
        detector.window_until = None;
        assert_eq!(detector.period(now), PERIOD);
    }
}
