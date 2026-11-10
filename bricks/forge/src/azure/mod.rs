//! The Azure DevOps provider: recognises Azure DevOps remotes, the old
//! `visualstudio.com` addresses included, and follows a branch through the
//! REST API ([`read`], [`timeline`]) at the pace of [`RHYTHM`]. Work items
//! belong to the project, not to the repository: the tracker is the project,
//! whose open work items, and one work item, are followed at the normal pace
//! ([`issues`]). Azure DevOps gives no `ETag` and counts every request
//! against a budget: the core publishes only what changed, and a reading
//! whose answer asks to slow down is a rate limit ([`slow_down`]). The token
//! comes from `az` ([`token`]); [`API_VAR`] and [`TOKEN_VAR`] replace the
//! API's address and the token, for tests.

mod issues;
mod read;
mod timeline;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, RETRY_AFTER};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

use crate::model::State;
use crate::provider::{BranchReading, Failure, Provider, Reading, Repo, Updates, Watch};
use crate::tools::http::{self, Http, encode};
use crate::tools::pace::{Pace, Rhythm, wait_or_refresh};
use crate::tools::remote;
use issues::Tracker;

/// Azure DevOps' REST API, the organisation first in each path.
pub const API: &str = "https://dev.azure.com";
/// Replaces [`API`], for tests.
pub const API_VAR: &str = "NOWCAST_FORGE_AZURE_API";
/// A token to use instead of asking `az`, for tests.
pub const TOKEN_VAR: &str = "NOWCAST_FORGE_AZURE_TOKEN";
/// Where people open what the API reads.
const WEB: &str = "https://dev.azure.com";
/// Azure DevOps' application id, the resource its tokens are asked for.
const RESOURCE: &str = "499b84ac-1321-427f-aa17-267ca6975798";
/// The API version of every request but the policy evaluations'.
const VERSION: &str = "api-version=7.1";

/// Every reading is counted: slower than GitHub's, where an unchanged
/// reading is free.
pub const RHYTHM: Rhythm = Rhythm {
    expected_first: Duration::from_secs(5),
    expected_after: Duration::from_secs(10),
    running: Duration::from_secs(10),
    window: Duration::from_secs(5),
    normal: Duration::from_secs(120),
};

const HEADERS: &[(&str, &str)] = &[("accept", "application/json")];

/// How long before its expiry a token is asked for again.
const RENEW_BEFORE: Duration = Duration::from_secs(120);
/// How long a token `az` gives without its expiry lasts: at least 5 minutes.
const LIFETIME: Duration = Duration::from_secs(300);
/// How long a repository's active PRs, read for one branch, serve the
/// others.
const FRESH_FOR: Duration = Duration::from_secs(3);
/// The budget of a 5-minute window, in TSTU, when an answer does not say.
const BUDGET: f64 = 200.0;

/// The token every reading shares, and when to ask for a new one: `None`
/// for a token that does not expire. `None` until asked.
type Token = Arc<tokio::sync::Mutex<Option<(String, Option<Instant>)>>>;

/// Answers read for one branch that serve the others for a while: each
/// path's answer, and when it was read.
type Fresh = Arc<Mutex<HashMap<String, (Instant, Arc<Value>)>>>;

/// What every target followed shares.
#[derive(Clone)]
struct Context {
    http: Http,
    api: String,
    token: Token,
    fresh: Fresh,
    timelines: timeline::Timelines,
    accounts: issues::Accounts,
    kinds: issues::Kinds,
}

pub struct Azure {
    context: Context,
}

impl Azure {
    /// Reads `api`, through `http`.
    pub fn new(http: Http, api: String) -> Self {
        Self {
            context: Context {
                http,
                api,
                token: Token::default(),
                fresh: Fresh::default(),
                timelines: timeline::Timelines::default(),
                accounts: issues::Accounts::default(),
                kinds: issues::Kinds::default(),
            },
        }
    }

    /// Follows the open work items of the project of `repo`, or its work
    /// item `number`.
    fn follow_tracker(&self, repo: &Repo, number: Option<u64>, updates: Updates) -> Watch {
        let tracker = Tracker {
            context: self.context.clone(),
            names: Names::of(repo),
            number,
        };
        Watch::spawn(move |refreshes| tracker.follow(updates, refreshes))
    }
}

impl Provider for Azure {
    fn recognise(&self, remote: &str) -> Option<Repo> {
        let names = recognise(remote, remote::ssh_hostname)?;
        Some(Repo {
            id: names.id(),
            tracker: names.tracker(),
            remote: remote.to_owned(),
        })
    }

    fn follow(&self, repo: &Repo, branch: &str, updates: Updates) -> Watch {
        let branch = Branch {
            context: self.context.clone(),
            names: Names::of(repo),
            branch: branch.to_owned(),
            memory: read::Memory::default(),
        };
        Watch::spawn(move |refreshes| branch.follow(updates, refreshes))
    }

    fn follow_issues(&self, repo: &Repo, updates: Updates) -> Watch {
        self.follow_tracker(repo, None, updates)
    }

    fn follow_issue(&self, repo: &Repo, number: u64, updates: Updates) -> Watch {
        self.follow_tracker(repo, Some(number), updates)
    }
}

/// The organisation, project and repository of a remote, decoded and in
/// lowercase: Azure DevOps ignores their case.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Names {
    org: String,
    project: String,
    repo: String,
}

impl Names {
    /// The names the identity of `repo` holds.
    fn of(repo: &Repo) -> Self {
        let mut parts = repo.id.splitn(4, '/').skip(1);
        let mut next = || parts.next().unwrap_or_default().to_owned();
        Self {
            org: next(),
            project: next(),
            repo: next(),
        }
    }

    /// `dev.azure.com/<org>/<project>/<repo>`.
    fn id(&self) -> String {
        format!("{}/{}", self.tracker(), self.repo)
    }

    /// The project, which holds the work items: `dev.azure.com/<org>/<project>`.
    fn tracker(&self) -> String {
        format!("dev.azure.com/{}/{}", self.org, self.project)
    }

    /// The path of the project's API, its names encoded again.
    fn project_api(&self) -> String {
        format!("/{}/{}/_apis", encode(&self.org), encode(&self.project))
    }

    /// The path of the repository's API.
    fn repo_api(&self) -> String {
        format!(
            "{}/git/repositories/{}",
            self.project_api(),
            encode(&self.repo)
        )
    }

    /// The project's web address.
    fn web(&self) -> String {
        format!("{WEB}/{}/{}", encode(&self.org), encode(&self.project))
    }
}

/// The names of an Azure DevOps repository: `https://[<org>@]dev.azure.com/
/// <org>/<project>/_git/<repo>`, `git@ssh.dev.azure.com:v3/<org>/<project>/
/// <repo>`, and the old `https://<org>.visualstudio.com/[DefaultCollection/]
/// <project>/_git/<repo>` and `<org>@vs-ssh.visualstudio.com:v3/<org>/
/// <project>/<repo>`. A repository named as its project may leave the
/// project out (`<org>/_git/<repo>`). Names may be percent-encoded. On an
/// SSH remote, an unknown host is taken as an SSH alias and handed to
/// `resolve`, which gives its real host name.
fn recognise(remote: &str, resolve: impl Fn(&str) -> Option<String>) -> Option<Names> {
    let (host, path) = remote::split(remote)?;
    let host = if is_azure(&host) {
        host
    } else if remote::is_ssh(remote) {
        resolve(&host)?.to_ascii_lowercase()
    } else {
        return None;
    };
    let segments: Vec<&str> = path.split('/').collect();
    let (org, project, repo) = match (host.as_str(), segments.as_slice()) {
        ("ssh.dev.azure.com" | "vs-ssh.visualstudio.com", ["v3", org, project, repo]) => {
            (*org, *project, *repo)
        }
        ("ssh.dev.azure.com" | "vs-ssh.visualstudio.com", _) => return None,
        ("dev.azure.com", [org, rest @ ..]) => {
            let (project, repo) = git_path(rest)?;
            (*org, project, repo)
        }
        (host, rest) => {
            let org = host.strip_suffix(".visualstudio.com")?;
            let rest = match rest {
                [collection, rest @ ..] if collection.eq_ignore_ascii_case("DefaultCollection") => {
                    rest
                }
                rest => rest,
            };
            let (project, repo) = git_path(rest)?;
            (org, project, repo)
        }
    };
    Some(Names {
        org: name(org)?,
        project: name(project)?,
        repo: name(repo)?,
    })
}

fn is_azure(host: &str) -> bool {
    host == "dev.azure.com" || host == "ssh.dev.azure.com" || host.ends_with(".visualstudio.com")
}

/// The project and repository of `<project>/_git/<repo>`, or of
/// `_git/<repo>` for a repository named as its project.
fn git_path<'a>(rest: &[&'a str]) -> Option<(&'a str, &'a str)> {
    match rest {
        [project, "_git", repo] => Some((project, repo)),
        ["_git", repo] => Some((repo, repo)),
        _ => None,
    }
}

/// A name of a remote's path, decoded and in lowercase.
fn name(segment: &str) -> Option<String> {
    let decoded = decode(segment)?.to_lowercase();
    (!decoded.is_empty() && !decoded.contains('/')).then_some(decoded)
}

/// Decodes `%XX` sequences; `None` when they do not make UTF-8.
fn decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// The token, kept until shortly before it expires, then asked for again;
/// `None` without `az` or without a login.
async fn token(kept: &Token) -> Option<String> {
    let mut kept = kept.lock().await;
    if let Some((token, renew)) = kept.as_ref()
        && renew.is_none_or(|at| Instant::now() < at)
    {
        return Some(token.clone());
    }
    *kept = ask().await;
    kept.as_ref().map(|(token, _)| token.clone())
}

/// A token from [`TOKEN_VAR`], which does not expire, or from `az`.
async fn ask() -> Option<(String, Option<Instant>)> {
    if let Ok(token) = std::env::var(TOKEN_VAR) {
        return Some((token, None));
    }
    let output = tokio::process::Command::new("az")
        .args(["account", "get-access-token", "--resource", RESOURCE])
        .args(["--output", "json"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let (token, lasts) = parse_token(&output.stdout, unix_now())?;
    Some((token, Some(Instant::now() + lasts)))
}

/// The token in `az`'s answer, and how long until it is asked for again:
/// shortly before `expires_on` (POSIX seconds; `expiresOn` is a local time
/// without its zone), or before [`LIFETIME`] when the answer does not say.
fn parse_token(output: &[u8], now: u64) -> Option<(String, Duration)> {
    let answer: Value = serde_json::from_slice(output).ok()?;
    let token = answer["accessToken"].as_str().filter(|t| !t.is_empty())?;
    let expires = &answer["expires_on"];
    let expires = expires
        .as_u64()
        .or_else(|| expires.as_str().and_then(|s| s.parse().ok()));
    let lasts = expires.map_or(LIFETIME, |at| Duration::from_secs(at.saturating_sub(now)));
    Some((token.to_owned(), lasts.saturating_sub(RENEW_BEFORE)))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A reading's way to the API, with the token it reads with.
struct Client<'a> {
    context: &'a Context,
    token: String,
}

impl Client<'_> {
    /// GETs `path` of the API.
    async fn get(&self, path: &str) -> Result<Arc<Value>, http::Failure> {
        let url = format!("{}{path}", self.context.api);
        let (body, headers) = self
            .context
            .http
            .get_answer(&url, &self.token, HEADERS)
            .await?;
        limited(&headers)?;
        Ok(body)
    }

    /// GETs `path`, or gives the answer another target read less than
    /// [`FRESH_FOR`] ago: what every branch of a repository shares.
    async fn get_fresh(&self, path: &str) -> Result<Arc<Value>, http::Failure> {
        let kept = self.context.fresh.lock().unwrap().get(path).cloned();
        if let Some((at, body)) = kept
            && at.elapsed() < FRESH_FOR
        {
            return Ok(body);
        }
        let body = self.get(path).await?;
        let mut fresh = self.context.fresh.lock().unwrap();
        fresh.retain(|_, (at, _)| at.elapsed() < FRESH_FOR);
        fresh.insert(path.to_owned(), (Instant::now(), body.clone()));
        Ok(body)
    }

    /// POSTs `body` to `path` of the API.
    async fn post(&self, path: &str, body: &Value) -> Result<Value, http::Failure> {
        let url = format!("{}{path}", self.context.api);
        let (body, headers) = self
            .context
            .http
            .post_answer(&url, &self.token, HEADERS, body)
            .await?;
        limited(&headers)?;
        Ok(body)
    }
}

/// A successful answer that asks to slow down is a rate limit.
fn limited(headers: &HeaderMap) -> Result<(), http::Failure> {
    match slow_down(headers, unix_now()) {
        Some(wait) => Err(http::Failure::Limited(Some(wait))),
        None => Ok(()),
    }
}

/// How long a successful answer asks to slow down for, `now` in POSIX
/// seconds: its `Retry-After`, or, with less than a tenth of the budget
/// left (`X-RateLimit-Remaining` of `X-RateLimit-Limit`), until
/// `X-RateLimit-Reset`, else the normal pace. `None` when it does not ask.
fn slow_down(headers: &HeaderMap, now: u64) -> Option<Duration> {
    let number = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<f64>().ok())
    };
    if let Some(secs) = number(RETRY_AFTER.as_str()) {
        return Some(Duration::from_secs_f64(secs.max(0.0)));
    }
    let remaining = number("x-ratelimit-remaining")?;
    if remaining * 10.0 >= number("x-ratelimit-limit").unwrap_or(BUDGET) {
        return None;
    }
    Some(
        number("x-ratelimit-reset")
            .map(|reset| Duration::from_secs((reset as u64).saturating_sub(now)))
            .filter(|wait| !wait.is_zero())
            .unwrap_or(RHYTHM.normal),
    )
}

/// What `failure`, in a reading of `at`, says in `source.status`, and how
/// long a rate limit asks to wait. A refused token is forgotten: the next
/// reading asks `az` again.
async fn failed(
    context: &Context,
    at: &str,
    failure: http::Failure,
) -> (Failure, Option<Duration>) {
    match failure {
        http::Failure::Unauthorized => {
            *context.token.lock().await = None;
            (Failure::Unauthorized, None)
        }
        http::Failure::NotFound => (Failure::NotFound, None),
        http::Failure::Limited(wait) => (Failure::RateLimited, Some(wait.unwrap_or(RHYTHM.normal))),
        http::Failure::Unreadable(why) => {
            crate::log(format_args!("{at}: {why}"));
            (Failure::Unreadable, None)
        }
        http::Failure::Unreachable(why) => {
            crate::log(format_args!("{at}: unreachable: {why}"));
            (Failure::Unreachable, None)
        }
    }
}

/// One branch followed.
struct Branch {
    context: Context,
    names: Names,
    branch: String,
    memory: read::Memory,
}

impl Branch {
    /// Reads, sends what it read, waits as the pace says or for a
    /// `refresh`, and again, until the core is gone. A rate limit is waited
    /// out whatever the refreshes.
    async fn follow(mut self, updates: Updates, mut refreshes: UnboundedReceiver<()>) {
        let mut pace = Pace::default();
        loop {
            let (outcome, wait) = self.read_once().await;
            let now = Instant::now();
            let (outcome, delay) = match outcome {
                Ok(mut reading) => {
                    let delay = paced(&mut pace, &mut reading, now);
                    (Ok(reading), delay)
                }
                // A failure says nothing new about the CI.
                Err(failure) => (
                    Err(failure),
                    wait.map_or(RHYTHM.normal, |w| w.max(RHYTHM.normal)),
                ),
            };
            if !updates.send(outcome.map(|r| Reading::Branch(Box::new(r)))) {
                return;
            }
            let refreshed = || pace.refreshed(Instant::now());
            if !wait_or_refresh(now + delay, &mut refreshes, wait.is_some(), refreshed).await {
                return;
            }
        }
    }

    /// One reading, and how long a rate limit asks to wait.
    async fn read_once(&mut self) -> (Result<BranchReading, Failure>, Option<Duration>) {
        let Some(token) = token(&self.context.token).await else {
            return (Err(Failure::NoCredentials), None);
        };
        let client = Client {
            context: &self.context,
            token,
        };
        match read::read(&client, &self.names, &self.branch, &mut self.memory).await {
            Ok(reading) => (Ok(reading), None),
            Err(failure) => {
                let at = format!("{}#{}", self.names.id(), self.branch);
                let (failure, wait) = failed(&self.context, &at, failure).await;
                (Err(failure), wait)
            }
        }
    }
}

/// Sets until when `reading` expects a CI, and gives how long to wait
/// after it. A pipeline without a job that waits its turn (a build policy
/// waiting for its build, a build not started) has not shown up yet: a CI
/// is expected. On an open PR, the policies say whether a build comes, so
/// nothing more is guessed; a branch without one expects a CI for a while
/// after a new head.
fn paced(pace: &mut Pace, reading: &mut BranchReading, now: Instant) -> Duration {
    let open = reading
        .pr
        .as_ref()
        .filter(|p| p.is_open())
        .map(|p| p.number);
    pace.saw(now, reading.head_sha.as_deref(), open);
    let Some(ci) = reading.ci.as_mut() else {
        return pace.next(&RHYTHM, now, 0, false);
    };
    let total = ci.pipelines.len();
    let shown = ci
        .pipelines
        .iter()
        .filter(|p| !(p.jobs.is_empty() && p.state == State::Queued))
        .count();
    let seen = if shown < total {
        0
    } else if open.is_some() {
        shown.max(1)
    } else {
        shown
    };
    ci.expected_until = pace.expected_until(seen);
    pace.next(&RHYTHM, now, seen, ci.is_running())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;
    use crate::model::tests::pipeline;
    use crate::model::{Ci, Pr};
    use crate::testing::Server;

    fn none(_: &str) -> Option<String> {
        None
    }

    fn names(org: &str, project: &str, repo: &str) -> Option<Names> {
        Some(Names {
            org: org.into(),
            project: project.into(),
            repo: repo.into(),
        })
    }

    #[test]
    fn every_spelling_of_an_azure_remote_names_one_repo() {
        let repo = names("fabrikam", "my project", "web app");
        for remote in [
            "https://fabrikam@dev.azure.com/fabrikam/My%20Project/_git/Web%20App",
            "https://dev.azure.com/Fabrikam/my%20project/_git/web%20app/",
            "git@ssh.dev.azure.com:v3/fabrikam/My%20Project/Web%20App",
            "ssh://git@ssh.dev.azure.com/v3/fabrikam/My%20Project/Web%20App",
            "https://fabrikam.visualstudio.com/My%20Project/_git/Web%20App",
            "https://fabrikam.visualstudio.com/DefaultCollection/My%20Project/_git/Web%20App",
            "fabrikam@vs-ssh.visualstudio.com:v3/fabrikam/My%20Project/Web%20App",
        ] {
            assert_eq!(recognise(remote, none), repo, "{remote}");
        }
        let named = recognise("https://dev.azure.com/o/_git/Proj", none);
        assert_eq!(named, names("o", "proj", "proj"), "named as its project");
        let id = names("fabrikam", "my project", "web app").unwrap();
        assert_eq!(id.id(), "dev.azure.com/fabrikam/my project/web app");
        assert_eq!(id.tracker(), "dev.azure.com/fabrikam/my project");
        assert_eq!(
            id.repo_api(),
            "/fabrikam/my%20project/_apis/git/repositories/web%20app"
        );
    }

    #[test]
    fn an_ssh_alias_is_resolved_and_the_rest_refused() {
        let resolve = |alias: &str| (alias == "azure").then(|| "ssh.dev.azure.com".to_owned());
        assert_eq!(
            recognise("git@azure:v3/o/p/r", resolve),
            names("o", "p", "r")
        );
        for remote in [
            "",
            "https://github.com/o/r",
            "https://dev.azure.com/o/p/r",
            "https://dev.azure.com/o/p/_git/r/extra",
            "git@ssh.dev.azure.com:v2/o/p/r",
            "https://dev.azure.com/o/p/_git/a%2Fb",
        ] {
            assert_eq!(recognise(remote, none), None, "{remote}");
        }
    }

    #[test]
    fn the_token_of_az_is_renewed_before_it_expires() {
        let answer = json!({ "accessToken": "t", "expiresOn": "2026-11-10 12:00:00.000000",
            "expires_on": 1_003_600, "tokenType": "Bearer" });
        let parsed = parse_token(answer.to_string().as_bytes(), 1_000_000);
        assert_eq!(parsed, Some(("t".into(), Duration::from_secs(3600 - 120))));
        let as_text = json!({ "accessToken": "t", "expires_on": "1000600" });
        assert_eq!(
            parse_token(as_text.to_string().as_bytes(), 1_000_000)
                .unwrap()
                .1,
            Duration::from_secs(480)
        );
        let bare = json!({ "accessToken": "t" });
        assert_eq!(
            parse_token(bare.to_string().as_bytes(), 0).unwrap().1,
            LIFETIME - RENEW_BEFORE
        );
        assert_eq!(parse_token(b"{\"accessToken\":\"\"}", 0), None);
        assert_eq!(parse_token(b"not json", 0), None);
    }

    #[test]
    fn an_answer_asks_to_slow_down_on_retry_after_or_a_low_budget() {
        let headers = |pairs: &[(&'static str, &str)]| {
            let mut map = HeaderMap::new();
            for (name, value) in pairs {
                map.insert(*name, value.parse().unwrap());
            }
            map
        };
        assert_eq!(slow_down(&headers(&[]), 0), None);
        assert_eq!(
            slow_down(&headers(&[("retry-after", "30")]), 0),
            Some(Duration::from_secs(30))
        );
        let plenty = headers(&[
            ("x-ratelimit-remaining", "150"),
            ("x-ratelimit-limit", "200"),
        ]);
        assert_eq!(slow_down(&plenty, 0), None);
        let low = headers(&[
            ("x-ratelimit-remaining", "12.5"),
            ("x-ratelimit-limit", "200"),
            ("x-ratelimit-reset", "1090"),
        ]);
        assert_eq!(slow_down(&low, 1000), Some(Duration::from_secs(90)));
        let low = headers(&[("x-ratelimit-remaining", "3")]);
        assert_eq!(slow_down(&low, 0), Some(RHYTHM.normal));
    }

    fn open_pr() -> Pr {
        Pr {
            number: 7,
            state: "open",
            draft: false,
            base: "main".into(),
            head_sha: "a".into(),
            url: "u".into(),
            auto_merge: None,
            queue: None,
            dequeued: None,
        }
    }

    /// On an open PR, a policy waiting for its build expects a CI, and
    /// nothing else does; a branch without a PR expects one after a push.
    #[test]
    fn the_policies_of_a_pr_say_whether_a_ci_is_expected() {
        let t0 = Instant::now();
        let reading = |head: &str, pr: bool, pipelines| BranchReading {
            head_sha: Some(head.into()),
            pr: pr.then(open_pr),
            ci: Some(Ci {
                sha: head.into(),
                pipelines,
                ..Ci::default()
            }),
        };
        let mut pace = Pace::default();
        paced(&mut pace, &mut reading("a", true, vec![]), t0);
        let mut pushed = reading("b", true, vec![]);
        assert_eq!(paced(&mut pace, &mut pushed, t0), RHYTHM.normal);
        assert_eq!(pushed.ci.unwrap().expected_until, None, "no policy");
        let mut waiting = reading("c", true, vec![pipeline(State::Queued, vec![])]);
        assert_eq!(paced(&mut pace, &mut waiting, t0), RHYTHM.expected_first);
        let mut pace = Pace::default();
        paced(&mut pace, &mut reading("a", false, vec![]), t0);
        let mut pushed = reading("b", false, vec![]);
        assert_eq!(paced(&mut pace, &mut pushed, t0), RHYTHM.expected_first);
        assert!(pushed.ci.unwrap().expected_until.is_some());
    }

    /// No token is `no_credentials`; a refused one is forgotten; a budget
    /// almost spent is a rate limit, waited out.
    #[tokio::test]
    async fn failures_become_the_source_status() {
        let server = Server::start(|request| {
            if request.contains("/denied/") {
                (401, vec![], String::new())
            } else {
                let headers = vec![("X-RateLimit-Remaining", "1"), ("Retry-After", "60")];
                (200, headers, json!({ "value": [] }).to_string())
            }
        })
        .await;
        let azure = Azure::new(Http::new().unwrap(), server.base());
        let branch = |org: &str| Branch {
            context: azure.context.clone(),
            names: names(org, "p", "r").unwrap(),
            branch: "feat".into(),
            memory: read::Memory::default(),
        };
        *azure.context.token.lock().await = Some(("t".into(), None));
        assert_eq!(
            branch("denied").read_once().await,
            (Err(Failure::Unauthorized), None)
        );
        assert!(azure.context.token.lock().await.is_none());
        *azure.context.token.lock().await = Some(("t".into(), None));
        assert_eq!(
            branch("busy").read_once().await,
            (Err(Failure::RateLimited), Some(Duration::from_secs(60)))
        );
        let expired = Some(("t".into(), Some(Instant::now())));
        *azure.context.token.lock().await = expired;
        if std::env::var(TOKEN_VAR).is_err() && which_az().is_none() {
            assert_eq!(
                branch("busy").read_once().await,
                (Err(Failure::NoCredentials), None),
                "an expired token is asked again, from no az"
            );
        }
    }

    fn which_az() -> Option<()> {
        std::process::Command::new("az")
            .arg("--version")
            .output()
            .ok()
            .map(|_| ())
    }

    /// The provider follows a branch: its first reading reaches the core.
    #[tokio::test]
    async fn a_followed_branch_sends_its_readings() {
        let server = Server::start(|request| {
            let line = request.lines().next().unwrap_or_default().to_owned();
            let body = if line.contains("/refs?") {
                json!({ "value": [{ "name": "refs/heads/feat", "objectId": "abc" }] })
            } else {
                json!({ "value": [] })
            };
            (200, vec![], body.to_string())
        })
        .await;
        let azure = Azure::new(Http::new().unwrap(), server.base());
        *azure.context.token.lock().await = Some(("t".into(), None));
        let repo = azure.recognise("git@ssh.dev.azure.com:v3/o/p/r").unwrap();
        assert_eq!(repo.id, "dev.azure.com/o/p/r");
        let (tx, mut rx) = unbounded_channel();
        let _watch = azure.follow(&repo, "feat", Updates::new(3, tx));
        let (target, outcome) = rx.recv().await.unwrap();
        assert_eq!(target, 3);
        let Ok(Reading::Branch(reading)) = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(reading.head_sha.as_deref(), Some("abc"));
        assert_eq!(reading.ci.unwrap().sha, "abc");
    }
}
