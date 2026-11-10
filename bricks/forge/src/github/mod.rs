//! The GitHub provider: recognises GitHub remotes and follows a branch
//! through the REST API ([`read`]), at the pace of [`RHYTHM`], with the
//! checks the base of its open PR requires ([`required`]) and the PR's
//! entry in the base's merge queue ([`queue`]). A reading
//! that changes nothing costs no rate limit (`304`), hence a fast pace while
//! a CI is expected or runs. A repository's open issues, and one issue, are
//! followed at the normal pace ([`issues`]). The token comes from the
//! source the user chose, `gh` or a token of theirs ([`token`]);
//! [`API_VAR`] and [`TOKEN_VAR`] replace the API's address and the token,
//! for tests.

mod issues;
mod queue;
mod read;
mod required;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

use crate::credentials::{Credentials, Forge, Source, Stamp};
use crate::model::{Ci, Dequeued, QueueEntry};
use crate::provider::{BranchReading, Failure, Provider, Reading, Repo, Updates, Watch};
use crate::tools::http::{self, Http};
use crate::tools::pace::{EXPECTED_FOR, Pace, Rhythm, wait_or_refresh};
use crate::tools::remote;
use issues::{Accounts, Tracker};
use queue::Queues;
use required::{Base, Required};

/// GitHub's REST API.
pub const API: &str = "https://api.github.com";
/// Replaces [`API`], for tests.
pub const API_VAR: &str = "NOWCAST_FORGE_GITHUB_API";
/// A token to use instead of the source chosen, for tests.
pub const TOKEN_VAR: &str = "NOWCAST_FORGE_GITHUB_TOKEN";

/// A reading that changes nothing is free: fast while something moves.
pub const RHYTHM: Rhythm = Rhythm {
    expected_first: Duration::from_secs(2),
    expected_after: Duration::from_secs(5),
    running: Duration::from_secs(5),
    window: Duration::from_secs(2),
    normal: Duration::from_secs(60),
};

/// The `Authorization` header by login, shared by every branch followed.
type Tokens = Arc<Mutex<Kept>>;

/// The tokens obtained, and the configuration's stamp when they were.
#[derive(Default)]
struct Kept {
    stamp: Stamp,
    by_owner: HashMap<String, String>,
}

pub struct GitHub {
    http: Http,
    api: String,
    credentials: Credentials,
    tokens: Tokens,
    accounts: Accounts,
    required: Required,
    queues: Queues,
}

impl GitHub {
    /// Reads `api`, through `http`, whose cache it shares with every branch,
    /// with the token `credentials` lead to.
    pub fn new(http: Http, api: String, credentials: Credentials) -> Self {
        Self {
            http,
            api,
            credentials,
            tokens: Tokens::default(),
            accounts: Accounts::default(),
            required: Required::default(),
            queues: Queues::default(),
        }
    }

    /// Follows the open issues of `repo`, or its issue `number`.
    fn follow_tracker(&self, repo: &Repo, number: Option<u64>, updates: Updates) -> Watch {
        let (owner, name) = owner_and_name(repo);
        let tracker = Tracker {
            http: self.http.clone(),
            api: self.api.clone(),
            credentials: self.credentials.clone(),
            tokens: self.tokens.clone(),
            accounts: self.accounts.clone(),
            owner,
            name,
            number,
        };
        Watch::spawn(move |refreshes| tracker.follow(updates, refreshes))
    }
}

/// The owner as the remote writes it, `gh` matching logins
/// case-sensitively, and the repository's name, the identity's.
fn owner_and_name(repo: &Repo) -> (String, String) {
    let (_, path) = remote::split(&repo.remote).unwrap_or_default();
    let owner = path.split('/').next().unwrap_or_default().to_owned();
    let name = repo.id.rsplit('/').next().unwrap_or_default().to_owned();
    (owner, name)
}

impl Provider for GitHub {
    fn recognise(&self, remote: &str) -> Option<Repo> {
        let (owner, name) = recognise(remote, remote::ssh_hostname)?;
        let id = format!("github.com/{owner}/{name}");
        // A repository holds its own issues.
        Some(Repo {
            tracker: id.clone(),
            id,
            remote: remote.to_owned(),
        })
    }

    fn follow(&self, repo: &Repo, branch: &str, updates: Updates) -> Watch {
        let (owner, name) = owner_and_name(repo);
        let branch = Branch {
            http: self.http.clone(),
            api: self.api.clone(),
            credentials: self.credentials.clone(),
            tokens: self.tokens.clone(),
            accounts: self.accounts.clone(),
            required: self.required.clone(),
            queues: self.queues.clone(),
            memory: Memory::default(),
            owner,
            name,
            branch: branch.to_owned(),
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

/// One branch followed.
struct Branch {
    http: Http,
    api: String,
    credentials: Credentials,
    tokens: Tokens,
    accounts: Accounts,
    required: Required,
    queues: Queues,
    memory: Memory,
    owner: String,
    name: String,
    branch: String,
}

impl Branch {
    /// Reads, sends what it read, waits as the pace says or for a
    /// `refresh`, and again, until the core is gone. A `refresh` that comes
    /// during a reading is answered by the next one. A rate limit is waited
    /// out whatever the refreshes.
    async fn follow(mut self, updates: Updates, mut refreshes: UnboundedReceiver<()>) {
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
                    // A PR in the merge queue moves as its queue CI runs.
                    let queued = reading.pr.as_ref().is_some_and(|p| p.queue.is_some());
                    let delay = match &mut reading.ci {
                        Some(ci) => {
                            ci.expected_until = pace.expected_until(ci.pipelines.len());
                            let running = ci.is_running() || queued;
                            pace.next(&RHYTHM, now, ci.pipelines.len(), running)
                        }
                        None => pace.next(&RHYTHM, now, 0, queued),
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
        let Some(token) =
            owner_token(&self.tokens, &self.accounts, &self.credentials, &self.owner).await
        else {
            return (Err(Failure::NoCredentials), None);
        };
        let read = read::read(
            &self.http,
            &self.api,
            &token,
            &self.owner,
            &self.name,
            &self.branch,
        )
        .await;
        let failure = match read {
            Ok(mut reading) => {
                let names = self.mark_required(&token, &mut reading).await;
                self.read_queue(&token, &mut reading, names.as_ref()).await;
                return (Ok(reading), None);
            }
            Err(failure) => failure,
        };
        let at = format!("github.com/{}/{}#{}", self.owner, self.name, self.branch);
        let (failure, wait) = failed(&self.tokens, &self.owner, &at, failure);
        (Err(failure), wait)
    }
}

/// The token that reads `owner`'s repositories, kept in `tokens` once its
/// source gave it, until the configuration changes; `None` without one.
/// Until then, each reading reads the configuration again. A new
/// configuration drops every token kept, and the accounts they read as.
async fn owner_token(
    tokens: &Tokens,
    accounts: &Accounts,
    credentials: &Credentials,
    owner: &str,
) -> Option<String> {
    let stamp = credentials.stamp();
    {
        let mut kept = tokens.lock().unwrap();
        if kept.stamp != stamp {
            kept.stamp = stamp;
            kept.by_owner.clear();
            accounts.lock().unwrap().clear();
        }
        if let Some(token) = kept.by_owner.get(owner) {
            return Some(token.clone());
        }
    }
    let token = token(credentials, owner).await?;
    let mut kept = tokens.lock().unwrap();
    // Not kept when the configuration changed while it was read.
    if kept.stamp == stamp {
        kept.by_owner.insert(owner.to_owned(), token.clone());
    }
    Some(token)
}

/// What `failure`, in a reading of `at` with `owner`'s token, says in
/// `source.status`, and how long a rate limit asks to wait. A refused token
/// is forgotten: the next reading asks its source again.
fn failed(
    tokens: &Tokens,
    owner: &str,
    at: &str,
    failure: http::Failure,
) -> (Failure, Option<Duration>) {
    match failure {
        http::Failure::Unauthorized => {
            tokens.lock().unwrap().by_owner.remove(owner);
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

/// What a branch remembers of its PR's passage through the merge queue.
#[derive(Default)]
struct Memory {
    /// The PR, its head and when it entered the queue, as the last reading
    /// found them in the queue.
    queued: Option<(u64, String, String)>,
    /// Why that PR left the queue with that head.
    dequeued: Option<(u64, String, Dequeued)>,
    /// The commit the queue tests, and since when: its CI is expected for a
    /// while before it shows up.
    tested: Option<(String, Instant)>,
    /// The last CI read for that commit, kept when a reading of it fails.
    queue_ci: Option<Ci>,
    /// Whether a failure to read that CI was logged: a lasting one is
    /// logged once, until it reads again.
    queue_ci_unread: bool,
}

/// Whether RFC 3339 time `a` comes before `b`; `false` when either does not
/// parse.
fn before(a: &str, b: &str) -> bool {
    match (a.parse::<jiff::Timestamp>(), b.parse::<jiff::Timestamp>()) {
        (Ok(a), Ok(b)) => a < b,
        _ => false,
    }
}

impl Branch {
    /// Marks the jobs the base of the open PR requires, and gives the
    /// names it requires. Without an open PR, nothing is required: the
    /// requirement comes from the base's rules.
    async fn mark_required(
        &self,
        token: &str,
        reading: &mut BranchReading,
    ) -> Option<BTreeSet<String>> {
        let (Some(pr), Some(ci)) = (
            reading.pr.as_ref().filter(|p| p.is_open()),
            reading.ci.as_mut(),
        ) else {
            return None;
        };
        let base = Base {
            http: &self.http,
            api: &self.api,
            token,
            owner: &self.owner,
            name: &self.name,
            base: &pr.base,
        };
        let names = self.required.names(&base).await;
        if let Some(names) = &names {
            required::mark(ci, names);
        }
        names
    }

    /// Reads the open PR's entry in its base's merge queue, with the CI of
    /// the commit the queue tests, marked from the base's `names` and
    /// expected for a while after that commit shows up. A PR that left the
    /// queue unmerged gets why, read once it is read without failure, kept
    /// while its head is the one that was queued. Nothing here fails the
    /// branch's reading: the queue is secondary to the PR and its own CI. A
    /// queue CI that cannot be read keeps the last one read for that commit,
    /// or none.
    async fn read_queue(
        &mut self,
        token: &str,
        reading: &mut BranchReading,
        names: Option<&BTreeSet<String>>,
    ) {
        let Some(pr) = reading.pr.as_mut().filter(|p| p.is_open()) else {
            self.memory = Memory::default();
            return;
        };
        let pr_base = pr.base.clone();
        let base = Base {
            http: &self.http,
            api: &self.api,
            token,
            owner: &self.owner,
            name: &self.name,
            base: &pr_base,
        };
        // `None`: the base never answered, nothing is known yet.
        match self.queues.entry(&base, pr.number).await {
            Some(Some(entry)) => {
                self.memory.queued =
                    Some((pr.number, pr.head_sha.clone(), entry.enqueued_at.clone()));
                self.memory.dequeued = None;
                let ci = match &entry.sha {
                    Some(sha) => queue_ci(&mut self.memory, &base, sha, names).await,
                    None => None,
                };
                pr.queue = Some(QueueEntry {
                    state: entry.state,
                    position: entry.position,
                    enqueued_at: entry.enqueued_at,
                    estimated_merge_at: entry.estimated_merge_at,
                    sha: entry.sha,
                    ci,
                });
            }
            Some(None) => {
                self.memory.tested = None;
                self.memory.queue_ci = None;
                self.memory.queue_ci_unread = false;
                let left = self.memory.queued.take();
                if let Some((number, head, enqueued_at)) = left
                    && number == pr.number
                    && head == pr.head_sha
                {
                    match queue::removal(&base, number).await {
                        // A removal older than the entry is an earlier one.
                        Ok(why) => {
                            self.memory.dequeued = why
                                .filter(|d| !before(&d.at, &enqueued_at))
                                .map(|d| (number, head, d));
                        }
                        Err(failure) => {
                            crate::log(format_args!(
                                "github.com/{}/{}#{number}: merge queue removal unread: {failure:?}",
                                self.owner, self.name
                            ));
                            // The next reading asks again.
                            self.memory.queued = Some((number, head, enqueued_at));
                        }
                    }
                }
            }
            None => {}
        }
        pr.dequeued = self
            .memory
            .dequeued
            .as_ref()
            .filter(|(number, head, _)| *number == pr.number && *head == pr.head_sha)
            .map(|(_, _, why)| why.clone());
    }
}

/// The CI of `sha`, the commit the queue of `base` tests, marked from the
/// base's `names` and expected for a while after that commit shows up. A
/// reading that fails gives the last CI `memory` holds for that commit, or
/// none; it is logged once per commit, until a reading succeeds.
async fn queue_ci(
    memory: &mut Memory,
    base: &Base<'_>,
    sha: &str,
    names: Option<&BTreeSet<String>>,
) -> Option<Ci> {
    let since = match &memory.tested {
        Some((tested, since)) if tested == sha => *since,
        _ => {
            memory.queue_ci = None;
            memory.queue_ci_unread = false;
            Instant::now()
        }
    };
    memory.tested = Some((sha.to_owned(), since));
    let read = read::ci(
        base.http, base.api, base.token, base.owner, base.name, sha, None,
    );
    match read.await {
        Ok(mut ci) => {
            if ci.pipelines.is_empty() {
                ci.expected_until = Some(since + EXPECTED_FOR);
            }
            if let Some(names) = names {
                required::mark(&mut ci, names);
            }
            memory.queue_ci = Some(ci);
            memory.queue_ci_unread = false;
        }
        Err(failure) => {
            if !std::mem::replace(&mut memory.queue_ci_unread, true) {
                crate::log(format_args!(
                    "github.com/{}/{}@{sha}: merge queue CI unread: {failure:?}",
                    base.owner, base.name
                ));
            }
        }
    }
    memory.queue_ci.clone()
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

/// The `Authorization` header for `owner`'s repositories, from the source
/// the user chose, and no other. From `gh`, the account it holds for the
/// owner, or else its active one: an organization is not an account. From
/// the keyring, the user's token. `None` when nothing is chosen, or the
/// source gives no token.
async fn token(credentials: &Credentials, owner: &str) -> Option<String> {
    let token = match std::env::var(TOKEN_VAR) {
        Ok(token) => token,
        Err(_) => match credentials.source(Forge::GitHub)? {
            Source::Cli => match gh_token(Some(owner)).await {
                Some(token) => token,
                None => gh_token(None).await?,
            },
            Source::Token => credentials.token(Forge::GitHub).await?,
        },
    };
    Some(format!("Bearer {token}"))
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
    use crate::credentials::testing;
    use crate::model::Pr;
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
            credentials: crate::credentials::testing::none(),
            tokens: Tokens::default(),
            accounts: Accounts::default(),
            required: Required::default(),
            queues: Queues::default(),
            memory: Memory::default(),
            owner: owner.into(),
            name: "r".into(),
            branch: "feat".into(),
        }
    }

    /// Only the source chosen is read: nothing chosen, or no token kept,
    /// is `no_credentials`; the user's token goes as a bearer; after a
    /// refusal, the configuration is read again.
    #[tokio::test]
    async fn the_token_comes_from_the_source_chosen() {
        if std::env::var(TOKEN_VAR).is_ok() {
            return;
        }
        let server = Server::start(|request| {
            if request.contains("authorization: bearer good") {
                (404, vec![], String::new())
            } else {
                (401, vec![], String::new())
            }
        })
        .await;
        let fake = testing::Fake::new();
        let mut branch = branch(&server, "o");
        branch.credentials = fake.credentials.clone();
        let no_credentials = (Err(Failure::NoCredentials), None);
        assert_eq!(branch.read_once().await, no_credentials);
        fake.choose(Forge::GitHub, Some(Source::Token), None);
        assert_eq!(branch.read_once().await, no_credentials);
        fake.choose(Forge::GitHub, Some(Source::Token), Some("bad"));
        assert_eq!(branch.read_once().await, (Err(Failure::Unauthorized), None));
        fake.choose(Forge::GitHub, Some(Source::Token), Some("good"));
        assert_eq!(branch.read_once().await, (Err(Failure::NotFound), None));
        assert_eq!(server.requests().len(), 2, "nothing sent without a token");
    }

    /// A token that serves is kept while the configuration stays; a new
    /// configuration reaches the next reading: a new token, another source,
    /// no source.
    #[tokio::test]
    async fn a_new_configuration_drops_the_tokens_kept() {
        if std::env::var(TOKEN_VAR).is_ok() {
            return;
        }
        let server = Server::start(|request| {
            if request.contains("authorization: bearer good") {
                (404, vec![], String::new())
            } else {
                (401, vec![], String::new())
            }
        })
        .await;
        let fake = testing::Fake::new();
        let mut branch = branch(&server, "o");
        branch.credentials = fake.credentials.clone();
        let not_found = (Err(Failure::NotFound), None);
        fake.choose(Forge::GitHub, Some(Source::Token), Some("good"));
        assert_eq!(branch.read_once().await, not_found);
        crate::credentials::Secrets::delete(&*fake.secrets, Forge::GitHub)
            .await
            .unwrap();
        assert_eq!(branch.read_once().await, not_found, "kept");
        branch
            .accounts
            .lock()
            .unwrap()
            .insert("Bearer good".into(), "me".into());
        fake.choose(Forge::GitHub, Some(Source::Token), Some("new"));
        assert_eq!(
            branch.read_once().await,
            (Err(Failure::Unauthorized), None),
            "a new token"
        );
        assert!(branch.accounts.lock().unwrap().is_empty());
        // What `gh` would have given, without running it.
        fake.choose(Forge::GitHub, Some(Source::Cli), None);
        let stamp = fake.credentials.stamp();
        *branch.tokens.lock().unwrap() = Kept {
            stamp,
            by_owner: HashMap::from([("o".into(), "Bearer gh".into())]),
        };
        fake.choose(Forge::GitHub, Some(Source::Token), Some("good"));
        assert_eq!(branch.read_once().await, not_found, "another source");
        fake.choose(Forge::GitHub, None, None);
        assert_eq!(
            branch.read_once().await,
            (Err(Failure::NoCredentials), None),
            "no source"
        );
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
        let mut denied = branch(&server, "denied");
        denied
            .tokens
            .lock()
            .unwrap()
            .by_owner
            .insert("denied".into(), "old".into());
        assert_eq!(denied.read_once().await, (Err(Failure::Unauthorized), None));
        assert!(denied.tokens.lock().unwrap().by_owner.is_empty());
        let mut limited = branch(&server, "busy");
        limited
            .tokens
            .lock()
            .unwrap()
            .by_owner
            .insert("busy".into(), "t".into());
        assert_eq!(
            limited.read_once().await,
            (Err(Failure::RateLimited), Some(Duration::from_secs(300)))
        );
    }

    /// A forge where `feat` has the open PR 7 on `main` when `pr` says so.
    /// Its head has a workflow run with `check` and `lint`, and the commit
    /// status `deploy`. `main` requires `check` by protection, and `e2e` and
    /// `deploy` by a ruleset.
    async fn forge(pr: bool) -> Server {
        Server::start(move |request| {
            let line = request.lines().next().unwrap_or_default().to_owned();
            let body = if line.starts_with("post /graphql") {
                json!({ "data": { "repository": { "ref": { "refUpdateRule": {
                    "requiredStatusCheckContexts": ["check"] } } } } })
            } else if line.contains("/rules/branches/main") {
                json!([{ "type": "required_status_checks", "parameters": {
                    "required_status_checks": [{ "context": "e2e" }, { "context": "deploy" }] } }])
            } else if line.contains("/pulls?state=open") && pr {
                json!([{ "number": 7, "state": "open", "draft": false, "html_url": "u",
                    "merged_at": null, "base": { "ref": "main" },
                    "head": { "ref": "feat", "sha": "abc", "repo": { "full_name": "o/r" } } }])
            } else if line.contains("/pulls?") {
                json!([])
            } else if line.contains("/branches/feat") {
                json!({ "commit": { "sha": "abc" } })
            } else if line.contains("/actions/runs") {
                json!({ "workflow_runs": [{ "id": 1, "name": "CI", "path": "ci.yml",
                    "event": "pull_request", "check_suite_id": 10, "status": "in_progress",
                    "created_at": "t" }] })
            } else if line.contains("/check-runs") {
                let check = |id: u64, name: &str| {
                    json!({ "id": id, "name": name,
                    "status": "in_progress", "check_suite": { "id": 10 },
                    "app": { "slug": "github-actions" } })
                };
                json!({ "check_runs": [check(1, "check"), check(2, "lint")] })
            } else if line.contains("/status") {
                json!({ "statuses": [{ "id": 3, "context": "deploy", "state": "success" }] })
            } else {
                return (404, vec![], String::new());
            };
            (200, vec![], body.to_string())
        })
        .await
    }

    fn required(reading: &BranchReading) -> Vec<(String, Option<bool>)> {
        let ci = reading.ci.as_ref().unwrap();
        ci.pipelines
            .iter()
            .flat_map(|p| &p.jobs)
            .map(|j| (j.name.clone(), j.required))
            .collect()
    }

    /// The jobs of an open PR are marked from the union of the base's
    /// protection and rulesets; a required check nothing reported is listed.
    #[tokio::test]
    async fn the_jobs_of_an_open_pr_say_whether_they_are_required() {
        let server = forge(true).await;
        let mut branch = branch(&server, "o");
        branch
            .tokens
            .lock()
            .unwrap()
            .by_owner
            .insert("o".into(), "t".into());
        let (Ok(reading), None) = branch.read_once().await else {
            panic!("the reading failed");
        };
        assert_eq!(
            required(&reading),
            [
                ("check".to_owned(), Some(true)),
                ("lint".to_owned(), Some(false)),
                ("deploy".to_owned(), Some(true)),
            ]
        );
        assert_eq!(reading.ci.unwrap().missing_required, ["e2e"]);
    }

    /// Without an open PR, nothing is required, and the rules are not read.
    #[tokio::test]
    async fn without_an_open_pr_nothing_is_required() {
        let server = forge(false).await;
        let mut branch = branch(&server, "o");
        branch
            .tokens
            .lock()
            .unwrap()
            .by_owner
            .insert("o".into(), "t".into());
        let (Ok(reading), None) = branch.read_once().await else {
            panic!("the reading failed");
        };
        assert!(required(&reading).iter().all(|(_, r)| r.is_none()));
        assert!(reading.ci.unwrap().missing_required.is_empty());
        assert!(
            !server
                .requests()
                .iter()
                .any(|r| r.starts_with("post") || r.contains("/rules/"))
        );
    }

    /// What the queue forge shows: whether `main` has a merge queue, whether
    /// PR 7 is in it, PR 7's head, and whether GraphQL fails.
    struct Queue {
        exists: bool,
        queued: bool,
        head: &'static str,
        broken: bool,
        /// Whether the commit the queue tests has its CI yet.
        queue_ci: bool,
        /// Whether reading that CI answers `404`.
        queue_ci_lost: bool,
        /// When PR 7 left the queue; `None` fails the query.
        removed_at: Option<&'static str>,
    }

    /// A forge where `feat` has the open PR 7 on `main`, which requires
    /// `check`. The queue tests PR 7 on the commit `q1`, whose `merge_group`
    /// run has `check` running; PR 7 left the queue because its CI failed.
    async fn queue_forge(queue: Arc<Mutex<Queue>>) -> Server {
        Server::start(move |request| {
            let q = queue.lock().unwrap();
            let line = request.lines().next().unwrap_or_default().to_owned();
            let body = if line.starts_with("post /graphql") && q.broken {
                return (502, vec![], String::new());
            } else if line.starts_with("post /graphql") && request.contains("mergequeue(branch") {
                let nodes = if q.queued {
                    json!([{ "position": 2, "state": "AWAITING_CHECKS",
                        "enqueuedAt": "2026-11-09T07:40:00Z", "estimatedTimeToMerge": 720,
                        "headCommit": { "oid": "q1" }, "pullRequest": { "number": 7 } }])
                } else {
                    json!([])
                };
                let queue = q.exists.then(|| json!({ "entries": { "nodes": nodes } }));
                json!({ "data": { "repository": { "mergeQueue": queue } } })
            } else if line.starts_with("post /graphql") && request.contains("timelineitems") {
                let Some(at) = q.removed_at else {
                    return (502, vec![], String::new());
                };
                json!({ "data": { "repository": { "pullRequest": { "timelineItems": { "nodes": [
                    { "reason": "CI failed", "createdAt": at }] } } } } })
            } else if line.starts_with("post /graphql") {
                json!({ "data": { "repository": { "ref": { "refUpdateRule": {
                    "requiredStatusCheckContexts": ["check"] } } } } })
            } else if line.contains("/rules/branches/main") {
                json!([])
            } else if line.contains("/pulls?state=open") {
                json!([{ "number": 7, "state": "open", "draft": false, "html_url": "u",
                    "merged_at": null, "base": { "ref": "main" },
                    "head": { "ref": "feat", "sha": q.head, "repo": { "full_name": "o/r" } } }])
            } else if line.contains("q1") && q.queue_ci_lost {
                return (404, vec![], String::new());
            } else if line.contains("/actions/runs?head_sha=q1") && q.queue_ci {
                json!({ "workflow_runs": [{ "id": 2, "name": "CI", "path": "ci.yml",
                    "event": "merge_group", "check_suite_id": 20, "status": "in_progress",
                    "head_branch": "gh-readonly-queue/main/pr-7-abc", "created_at": "t" }] })
            } else if line.contains("/commits/q1/check-runs") && q.queue_ci {
                json!({ "check_runs": [{ "id": 1, "name": "check", "status": "in_progress",
                    "check_suite": { "id": 20 }, "app": { "slug": "github-actions" } }] })
            } else if line.contains("/actions/runs") {
                json!({ "workflow_runs": [] })
            } else if line.contains("/check-runs") {
                json!({ "check_runs": [] })
            } else if line.contains("/status") {
                json!({ "statuses": [] })
            } else {
                return (404, vec![], String::new());
            };
            (200, vec![], body.to_string())
        })
        .await
    }

    fn queue(exists: bool, queued: bool) -> Arc<Mutex<Queue>> {
        Arc::new(Mutex::new(Queue {
            exists,
            queued,
            head: "abc",
            broken: false,
            queue_ci: true,
            queue_ci_lost: false,
            removed_at: Some("2026-11-09T07:45:00Z"),
        }))
    }

    async fn read_pr(branch: &mut Branch) -> Pr {
        let (Ok(reading), None) = branch.read_once().await else {
            panic!("the reading failed");
        };
        reading.pr.unwrap()
    }

    fn asked(server: &Server, what: &str) -> usize {
        server
            .requests()
            .iter()
            .filter(|r| r.contains(what))
            .count()
    }

    /// A PR in the queue carries its entry and the CI of the commit the
    /// queue tests, marked from the base's rules; the base is read again
    /// every 10 s while the PR is in it.
    #[tokio::test]
    async fn a_pr_in_the_queue_carries_its_entry_and_the_queue_ci() {
        let server = queue_forge(queue(true, true)).await;
        let mut branch = branch(&server, "o");
        branch
            .tokens
            .lock()
            .unwrap()
            .by_owner
            .insert("o".into(), "t".into());
        let pr = read_pr(&mut branch).await;
        let mut entry = pr.queue.unwrap().detail(Instant::now());
        assert!(entry["estimated_merge_at"].is_string());
        entry["estimated_merge_at"] = json!(null);
        assert_eq!(
            entry,
            json!({ "state": "awaiting_checks", "position": 2,
                "enqueued_at": "2026-11-09T07:40:00Z", "estimated_merge_at": null, "sha": "q1",
                "ci": { "state": "running", "required_state": "running", "sha": "q1" } })
        );
        assert_eq!(pr.dequeued, None);
        let query = server
            .requests()
            .into_iter()
            .find(|r| r.contains("mergequeue(branch"))
            .unwrap();
        assert!(query.contains(r#""branch":"main""#));
        read_pr(&mut branch).await;
        assert_eq!(asked(&server, "mergequeue(branch"), 1, "not before 10 s");
        branch.queues.age(queue::FAST);
        read_pr(&mut branch).await;
        assert_eq!(asked(&server, "mergequeue(branch"), 2);
    }

    /// A base without a queue gives no entry, and is asked again only at
    /// the normal pace.
    #[tokio::test]
    async fn a_base_without_a_queue_is_asked_at_the_normal_pace() {
        let server = queue_forge(queue(false, false)).await;
        let mut branch = branch(&server, "o");
        branch
            .tokens
            .lock()
            .unwrap()
            .by_owner
            .insert("o".into(), "t".into());
        let pr = read_pr(&mut branch).await;
        assert_eq!((pr.queue, pr.dequeued), (None, None));
        read_pr(&mut branch).await;
        branch.queues.age(queue::FAST);
        read_pr(&mut branch).await;
        assert_eq!(asked(&server, "mergequeue(branch"), 1);
        branch.queues.age(RHYTHM.normal);
        read_pr(&mut branch).await;
        assert_eq!(asked(&server, "mergequeue(branch"), 2);
    }

    /// A queue GraphQL cannot read fails nothing: the branch is read.
    #[tokio::test]
    async fn a_failing_queue_query_keeps_the_reading() {
        let world = queue(true, true);
        world.lock().unwrap().broken = true;
        let server = queue_forge(world).await;
        let mut branch = branch(&server, "o");
        branch
            .tokens
            .lock()
            .unwrap()
            .by_owner
            .insert("o".into(), "t".into());
        assert_eq!(read_pr(&mut branch).await.queue, None);
    }

    /// A PR that left the queue unmerged says why, asked once, until it
    /// gets a new head.
    #[tokio::test]
    async fn a_pr_out_of_the_queue_says_why_until_a_new_head() {
        let world = queue(true, true);
        let server = queue_forge(world.clone()).await;
        let mut branch = branch(&server, "o");
        branch
            .tokens
            .lock()
            .unwrap()
            .by_owner
            .insert("o".into(), "t".into());
        read_pr(&mut branch).await;
        world.lock().unwrap().queued = false;
        branch.queues.age(queue::FAST);
        let pr = read_pr(&mut branch).await;
        assert_eq!(pr.queue, None);
        let why = Dequeued {
            reason: Some("CI failed".into()),
            at: "2026-11-09T07:45:00Z".into(),
        };
        assert_eq!(pr.dequeued, Some(why.clone()));
        let removal = server
            .requests()
            .into_iter()
            .find(|r| r.contains("timelineitems"))
            .unwrap();
        assert!(removal.contains("itemtypes: [removed_from_merge_queue_event]"));
        assert!(removal.contains(r#""number":7"#));
        branch.queues.age(RHYTHM.normal);
        assert_eq!(read_pr(&mut branch).await.dequeued, Some(why));
        assert_eq!(asked(&server, "timelineitems"), 1, "asked once");
        world.lock().unwrap().head = "def";
        assert_eq!(read_pr(&mut branch).await.dequeued, None, "a new head");
    }

    /// A queue CI that cannot be read fails nothing, `404` included: the
    /// PR and its own CI are read, and the queue CI last read is kept, or
    /// none.
    #[tokio::test]
    async fn a_failing_queue_ci_keeps_the_reading_and_the_last_queue_ci() {
        let world = queue(true, true);
        world.lock().unwrap().queue_ci_lost = true;
        let server = queue_forge(world.clone()).await;
        let mut branch = branch(&server, "o");
        branch
            .tokens
            .lock()
            .unwrap()
            .by_owner
            .insert("o".into(), "t".into());
        let (Ok(reading), None) = branch.read_once().await else {
            panic!("the reading failed");
        };
        assert_eq!(reading.ci.unwrap().sha, "abc", "the PR's own CI");
        let entry = reading.pr.unwrap().queue.unwrap();
        assert_eq!(entry.sha.as_deref(), Some("q1"));
        assert_eq!(entry.ci, None, "never read");
        assert!(branch.memory.queue_ci_unread, "logged");
        world.lock().unwrap().queue_ci_lost = false;
        let read = read_pr(&mut branch).await.queue.unwrap().ci;
        assert_eq!(read.as_ref().unwrap().pipelines.len(), 1);
        assert!(!branch.memory.queue_ci_unread, "a failure would log again");
        world.lock().unwrap().queue_ci_lost = true;
        assert_eq!(read_pr(&mut branch).await.queue.unwrap().ci, read, "kept");
    }

    /// The commit the queue tests, before its CI shows up, expects one: a
    /// required check is not missing yet.
    #[tokio::test]
    async fn the_queue_ci_is_expected_before_it_shows_up() {
        let world = queue(true, true);
        world.lock().unwrap().queue_ci = false;
        let server = queue_forge(world).await;
        let mut branch = branch(&server, "o");
        branch
            .tokens
            .lock()
            .unwrap()
            .by_owner
            .insert("o".into(), "t".into());
        let entry = read_pr(&mut branch).await.queue.unwrap();
        assert_eq!(
            entry.detail(Instant::now())["ci"],
            json!({ "state": "running", "required_state": "running", "sha": "q1" })
        );
        let past = Instant::now() + EXPECTED_FOR;
        assert_eq!(entry.detail(past)["ci"]["required_state"], "missing");
    }

    /// Why a PR left the queue is asked again when the query failed, and
    /// an earlier removal than the PR's last entry says nothing.
    #[tokio::test]
    async fn a_removal_is_asked_again_after_a_failure_and_must_be_the_last() {
        let world = queue(true, true);
        world.lock().unwrap().removed_at = None;
        let server = queue_forge(world.clone()).await;
        let mut branch = branch(&server, "o");
        branch
            .tokens
            .lock()
            .unwrap()
            .by_owner
            .insert("o".into(), "t".into());
        read_pr(&mut branch).await;
        world.lock().unwrap().queued = false;
        branch.queues.age(queue::FAST);
        assert_eq!(read_pr(&mut branch).await.dequeued, None);
        world.lock().unwrap().removed_at = Some("2026-11-09T07:30:00Z");
        assert_eq!(read_pr(&mut branch).await.dequeued, None, "an earlier one");
        assert_eq!(asked(&server, "timelineitems"), 2);
        read_pr(&mut branch).await;
        assert_eq!(asked(&server, "timelineitems"), 2, "answered: not again");
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
        let github = GitHub::new(Http::new().unwrap(), server.base(), testing::none());
        github
            .tokens
            .lock()
            .unwrap()
            .by_owner
            .insert("o".into(), "t".into());
        let repo = github.recognise("git@github.com:o/r.git").unwrap();
        assert_eq!(repo.id, "github.com/o/r");
        let (tx, mut rx) = unbounded_channel();
        let _watch = github.follow(&repo, "feat", Updates::new(3, tx));
        let (target, outcome) = rx.recv().await.unwrap();
        assert_eq!(target, 3);
        let Ok(Reading::Branch(reading)) = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(reading.pr.unwrap().number, 7);
        let ci = reading.ci.unwrap();
        assert_eq!(ci.sha, "abc");
        assert_eq!(ci.expected_until, None, "the first reading expects nothing");
    }
}
