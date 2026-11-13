//! A repository's issues on GitHub, through the REST API: its open issues,
//! the most recently created first, and one issue. Both are read when the
//! repository's issues detector says they moved, at once after a `refresh`,
//! and at the slow [`SAFETY`] pace for what detection could miss. The
//! issues API mixes pull requests in: they are removed. The account that
//! reads is `GET /user`, asked once per token.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

use super::detect::Detected;
use super::read::HEADERS;
use super::{RHYTHM, Tokens, failed, owner_token};
use crate::credentials::Credentials;
use crate::model::{Issue, Issues};
use crate::provider::{Failure, Outcome, Reading, Updates};
use crate::tools::http::{self, Http};

/// The issues one page lists; two pages at most are read.
const PAGE: usize = 100;
/// How often issues are read without a detector saying they moved: for
/// what it could miss, such as an issue that left its list of 20 between
/// two polls.
pub const SAFETY: Duration = Duration::from_secs(300);

/// The login each token reads as, shared by every tracker followed.
pub type Accounts = Arc<Mutex<HashMap<String, String>>>;

/// A repository's open issues followed, or its issue `number`.
pub struct Tracker {
    pub http: Http,
    pub api: String,
    pub credentials: Credentials,
    pub tokens: Tokens,
    pub accounts: Accounts,
    pub owner: String,
    pub name: String,
    pub number: Option<u64>,
}

impl Tracker {
    /// Reads, sends what it read, waits for the [`SAFETY`] pace, for the
    /// issues detector to say they moved, or for a `refresh`, and again,
    /// until the core is gone. A rate limit is waited out whatever comes.
    pub async fn follow(
        self,
        updates: Updates,
        mut refreshes: UnboundedReceiver<()>,
        mut detected: Detected,
    ) {
        loop {
            let (outcome, wait) = self.read_once().await;
            if !updates.send(outcome) {
                return;
            }
            let delay = wait.map_or(SAFETY, |w| w.max(RHYTHM.normal));
            let deadline = Instant::now() + delay;
            if !detected
                .wait(deadline, &mut refreshes, wait.is_some())
                .await
            {
                return;
            }
        }
    }

    /// One reading, and how long a rate limit asks to wait.
    async fn read_once(&self) -> (Outcome, Option<Duration>) {
        let Some(token) =
            owner_token(&self.tokens, &self.accounts, &self.credentials, &self.owner).await
        else {
            return (Err(Failure::NoCredentials), None);
        };
        let read = match self.number {
            None => self.open(&token).await.map(Reading::Issues),
            Some(number) => self.one(&token, number).await.map(Reading::Issue),
        };
        match read {
            Ok(reading) => (Ok(reading), None),
            Err(failure) => {
                if failure == http::Failure::Unauthorized {
                    // A refused token reads as no account any more.
                    self.accounts.lock().unwrap().remove(&token);
                }
                let (failure, wait) = failed(&self.tokens, &self.owner, &self.at(), failure);
                (Err(failure), wait)
            }
        }
    }

    /// Where the readings go, for the log.
    fn at(&self) -> String {
        let repo = format!("github.com/{}/{}", self.owner, self.name);
        match self.number {
            None => format!("{repo} issues"),
            Some(number) => format!("{repo} issue #{number}"),
        }
    }

    async fn get(&self, path: &str, token: &str) -> Result<Arc<Value>, http::Failure> {
        let url = format!("{}{path}", self.api);
        self.http.get(&url, token, HEADERS).await
    }

    /// The open issues, split between the account's and the others: two
    /// pages at most, `truncated` when the second is full. The parameters
    /// stay the same from one reading to the next, for the `ETag`.
    async fn open(&self, token: &str) -> Result<Issues, http::Failure> {
        let account = self.account(token).await?;
        let path = format!(
            "/repos/{}/{}/issues?state=open&sort=created&direction=desc&per_page={PAGE}",
            self.owner, self.name
        );
        let mut issues = Issues {
            account,
            ..Issues::default()
        };
        let first = self.get(&path, token).await?;
        if sort_into(&mut issues, &first)? {
            let second = self.get(&format!("{path}&page=2"), token).await?;
            issues.truncated = sort_into(&mut issues, &second)?;
        }
        Ok(issues)
    }

    /// The login `token` reads as, asked once.
    async fn account(&self, token: &str) -> Result<String, http::Failure> {
        if let Some(login) = self.accounts.lock().unwrap().get(token).cloned() {
            return Ok(login);
        }
        let user = self.get("/user", token).await?;
        let login = user["login"]
            .as_str()
            .ok_or_else(|| http::Failure::Unreadable("the user has no login".into()))?
            .to_owned();
        self.accounts
            .lock()
            .unwrap()
            .insert(token.to_owned(), login.clone());
        Ok(login)
    }

    /// Issue `number`: a pull request is no issue, hence not found.
    async fn one(&self, token: &str, number: u64) -> Result<Issue, http::Failure> {
        let path = format!("/repos/{}/{}/issues/{number}", self.owner, self.name);
        let item = self.get(&path, token).await?;
        if item.get("pull_request").is_some() {
            return Err(http::Failure::NotFound);
        }
        issue_of(&item).ok_or_else(|| http::Failure::Unreadable("an issue without number".into()))
    }
}

/// Sorts the issues of a `page` into `issues`, the pull requests removed
/// and an issue a shift between pages repeated kept once. `true` when the
/// page is full: there may be more.
fn sort_into(issues: &mut Issues, page: &Value) -> Result<bool, http::Failure> {
    let items = page
        .as_array()
        .ok_or_else(|| http::Failure::Unreadable("the issues are not a list".into()))?;
    for item in items.iter().filter(|i| i.get("pull_request").is_none()) {
        let Some(issue) = issue_of(item) else {
            continue;
        };
        if issues
            .mine
            .iter()
            .chain(&issues.others)
            .any(|i| i.number == issue.number)
        {
            continue;
        }
        if assigned(item, &issues.account) {
            issues.mine.push(issue);
        } else {
            issues.others.push(issue);
        }
    }
    Ok(items.len() >= PAGE)
}

/// Whether `login` is among the issue's assignees.
fn assigned(item: &Value, login: &str) -> bool {
    item["assignees"].as_array().is_some_and(|a| {
        a.iter().any(|u| {
            u["login"]
                .as_str()
                .is_some_and(|l| l.eq_ignore_ascii_case(login))
        })
    })
}

/// An issue of the REST API; `type` is the organisation's issue type, an
/// object whose `name` is kept, `null` without one.
fn issue_of(item: &Value) -> Option<Issue> {
    Some(Issue {
        number: item["number"].as_u64()?,
        title: item["title"].as_str().unwrap_or_default().to_owned(),
        kind: item["type"]["name"].as_str().map(str::to_owned),
        url: item["html_url"].as_str().unwrap_or_default().to_owned(),
        open: item["state"] == "open",
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;
    use crate::github::GitHub;
    use crate::provider::Provider;
    use crate::testing::Server;

    fn tracker(server: &Server, number: Option<u64>) -> Tracker {
        let tokens = Tokens::default();
        tokens
            .lock()
            .unwrap()
            .by_owner
            .insert("o".into(), "t".into());
        Tracker {
            http: Http::new().unwrap(),
            api: server.base(),
            credentials: crate::credentials::testing::none(),
            tokens,
            accounts: Accounts::default(),
            owner: "o".into(),
            name: "r".into(),
            number,
        }
    }

    fn item(number: u64, assignees: &[&str]) -> Value {
        let assignees: Vec<Value> = assignees.iter().map(|l| json!({ "login": l })).collect();
        json!({ "number": number, "title": format!("issue {number}"), "state": "open",
            "html_url": format!("u/{number}"), "type": null, "assignees": assignees })
    }

    fn pull(number: u64) -> Value {
        let mut pull = item(number, &["me"]);
        pull["pull_request"] = json!({ "url": "p" });
        pull
    }

    async fn read(tracker: &Tracker) -> Reading {
        match tracker.read_once().await {
            (Ok(reading), None) => reading,
            other => panic!("{other:?}"),
        }
    }

    fn numbers(issues: &[Issue]) -> Vec<u64> {
        issues.iter().map(|i| i.number).collect()
    }

    /// The account's issues are told from the others, the pull requests
    /// removed; the account is asked once.
    #[tokio::test]
    async fn the_open_issues_split_between_the_account_and_the_others() {
        let server = Server::start(|request| {
            let line = request.lines().next().unwrap_or_default();
            let body = if line.contains("/user") {
                json!({ "login": "Me" })
            } else if line
                .contains("/repos/o/r/issues?state=open&sort=created&direction=desc&per_page=100 ")
            {
                let mut bug = item(1, &[]);
                bug["type"] = json!({ "id": 7, "name": "Bug", "color": "red" });
                json!([
                    item(4, &["someone", "me"]),
                    pull(3),
                    item(2, &["someone"]),
                    bug
                ])
            } else {
                return (404, vec![], String::new());
            };
            (200, vec![], body.to_string())
        })
        .await;
        let tracker = tracker(&server, None);
        let Reading::Issues(issues) = read(&tracker).await else {
            panic!("not issues");
        };
        assert_eq!(issues.account, "Me");
        assert_eq!(numbers(&issues.mine), [4]);
        assert_eq!(numbers(&issues.others), [2, 1]);
        assert_eq!(issues.others[1].kind.as_deref(), Some("Bug"));
        assert_eq!(issues.others[0].kind, None);
        assert!(!issues.truncated);
        read(&tracker).await;
        let requests = server.requests();
        assert_eq!(requests.iter().filter(|r| r.contains("/user")).count(), 1);
        assert!(!requests.iter().any(|r| r.contains("page=2")));
    }

    /// Past a full first page the second is read, with the same
    /// parameters; a full second page says there are more.
    #[tokio::test]
    async fn the_open_issues_stop_at_two_pages() {
        for (second, truncated) in [(100, true), (40, false)] {
            let server = Server::start(move |request| {
                let line = request.lines().next().unwrap_or_default();
                let body = if line.contains("/user") {
                    json!({ "login": "me" })
                } else if line.contains("per_page=100&page=2 ") {
                    json!((0..second).map(|n| item(100 - n, &[])).collect::<Vec<_>>())
                } else if line.contains("per_page=100 ") {
                    let page: Vec<Value> = (0..100)
                        .map(|n| 200 - n)
                        .map(|n| if n % 10 == 0 { pull(n) } else { item(n, &[]) })
                        .collect();
                    json!(page)
                } else {
                    return (404, vec![], String::new());
                };
                (200, vec![], body.to_string())
            })
            .await;
            let Reading::Issues(issues) = read(&tracker(&server, None)).await else {
                panic!("not issues");
            };
            assert_eq!(issues.truncated, truncated);
            assert_eq!(issues.others.len(), 90 + second as usize);
            assert_eq!(issues.others[0].number, 199);
            let pages = server.requests();
            assert!(pages.iter().any(|r| r.starts_with(
                "get /repos/o/r/issues?state=open&sort=created&direction=desc&per_page=100&page=2 "
            )));
        }
    }

    /// An issue's state, and its type when the organisation gives one.
    #[tokio::test]
    async fn an_issue_is_open_or_closed() {
        let server = Server::start(|request| {
            let line = request.lines().next().unwrap_or_default();
            let mut issue = item(5, &[]);
            if line.contains("/issues/6 ") {
                issue["number"] = json!(6);
                issue["state"] = json!("closed");
                issue["type"] = json!({ "name": "Task" });
            }
            (200, vec![], issue.to_string())
        })
        .await;
        let Reading::Issue(open) = read(&tracker(&server, Some(5))).await else {
            panic!("not an issue");
        };
        assert_eq!((open.number, open.open, open.kind), (5, true, None));
        let Reading::Issue(closed) = read(&tracker(&server, Some(6))).await else {
            panic!("not an issue");
        };
        assert_eq!(closed.title, "issue 5");
        assert_eq!(
            (closed.number, closed.open, closed.kind.as_deref()),
            (6, false, Some("Task"))
        );
    }

    /// A pull request's number, a missing one or a deleted one is not
    /// found.
    #[tokio::test]
    async fn a_pull_request_or_a_missing_number_is_not_found() {
        let server = Server::start(|request| {
            let line = request.lines().next().unwrap_or_default();
            if line.contains("/issues/3 ") {
                (200, vec![], pull(3).to_string())
            } else if line.contains("/issues/8 ") {
                (410, vec![], String::new())
            } else {
                (404, vec![], String::new())
            }
        })
        .await;
        for number in [3, 9, 8] {
            assert_eq!(
                tracker(&server, Some(number)).read_once().await,
                (Err(Failure::NotFound), None),
                "#{number}"
            );
        }
    }

    /// A refused token is asked for again, its account forgotten; a rate
    /// limit is waited out.
    #[tokio::test]
    async fn failures_are_those_of_a_branch() {
        let server = Server::start(|request| {
            if request.contains("/issues/1 ") {
                (401, vec![], String::new())
            } else {
                (403, vec![("Retry-After", "300")], String::new())
            }
        })
        .await;
        let denied = tracker(&server, Some(1));
        denied
            .accounts
            .lock()
            .unwrap()
            .insert("t".into(), "me".into());
        assert_eq!(denied.read_once().await, (Err(Failure::Unauthorized), None));
        assert!(denied.tokens.lock().unwrap().by_owner.is_empty());
        assert!(denied.accounts.lock().unwrap().is_empty());
        assert_eq!(
            tracker(&server, Some(2)).read_once().await,
            (Err(Failure::RateLimited), Some(Duration::from_secs(300)))
        );
    }

    /// The provider follows an issue: its first reading reaches the core,
    /// and a `refresh` reads it again at once.
    #[tokio::test]
    async fn a_followed_issue_sends_its_readings() {
        let server = Server::start(|_| (200, vec![], item(52, &[]).to_string())).await;
        let credentials = crate::credentials::testing::none();
        let github = GitHub::new(Http::new().unwrap(), server.base(), credentials);
        let repo = github.recognise("git@github.com:O/R.git").unwrap();
        assert_eq!(repo.tracker, "github.com/o/r");
        github
            .tokens
            .lock()
            .unwrap()
            .by_owner
            .insert("O".into(), "t".into());
        let (tx, mut rx) = unbounded_channel();
        let watch = github.follow_issue(&repo, 52, Updates::new(4, tx));
        let (target, outcome) = rx.recv().await.unwrap();
        assert_eq!(target, 4);
        assert!(matches!(
            outcome,
            Ok(Reading::Issue(Issue { number: 52, .. }))
        ));
        watch.refresh();
        assert!(rx.recv().await.unwrap().1.is_ok());
        let read = |r: &&String| r.contains("/issues/52 ");
        assert_eq!(server.requests().iter().filter(read).count(), 2);
    }
}
