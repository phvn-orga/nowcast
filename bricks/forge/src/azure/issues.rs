//! A project's work items on Azure DevOps: its open work items, the most
//! recently created first, and one work item. Both are read at the normal
//! pace of [`RHYTHM`], and at once after a `refresh`.
//!
//! WIQL cannot filter on state categories, so the states of each work item
//! type are read (each has a fixed category) and the query excludes, type
//! by type, the states of the `Completed` and `Removed` categories. The
//! types that are not tickets (tests, code reviews, feedback) are excluded
//! by their category. `@Me` tells the reader's work items from the others;
//! the account that reads comes from the connection data, asked once per
//! token.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

use super::{Client, Context, Names, RHYTHM, VERSION, failed, token};
use crate::model::{Issue, Issues};
use crate::provider::{Failure, Outcome, Reading, Updates};
use crate::tools::http::{self, encode};
use crate::tools::pace::wait_or_refresh;

/// The work items listed at most.
const MAX: usize = 200;
/// The state categories of a work item that is no longer open.
const CLOSED: [&str; 2] = ["Completed", "Removed"];
/// The categories of the types that are not tickets.
const NOT_TICKETS: [&str; 5] = [
    "Microsoft.HiddenCategory",
    "Microsoft.TestCaseCategory",
    "Microsoft.TestPlanCategory",
    "Microsoft.TestSuiteCategory",
    "Microsoft.SharedStepCategory",
];
/// How long the states of a project's types serve: they rarely change.
const KINDS_FOR: Duration = Duration::from_secs(30 * 60);

/// The account each token reads as, shared by every tracker followed.
pub type Accounts = Arc<Mutex<HashMap<String, String>>>;
/// The types of each project, and when they were read.
pub type Kinds = Arc<Mutex<HashMap<String, (Instant, Arc<Vec<Kind>>)>>>;

/// A work item type, and the states in which its work items are no longer
/// open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kind {
    name: String,
    closed: Vec<String>,
}

/// A project's open work items followed, or its work item `number`.
pub struct Tracker {
    pub context: Context,
    pub names: Names,
    pub number: Option<u64>,
}

impl Tracker {
    /// Reads, sends what it read, waits for the normal pace or a `refresh`,
    /// and again, until the core is gone. A rate limit is waited out
    /// whatever the refreshes.
    pub async fn follow(self, updates: Updates, mut refreshes: UnboundedReceiver<()>) {
        loop {
            let (outcome, wait) = self.read_once().await;
            if !updates.send(outcome) {
                return;
            }
            let delay = wait.map_or(RHYTHM.normal, |w| w.max(RHYTHM.normal));
            let deadline = Instant::now() + delay;
            if !wait_or_refresh(deadline, &mut refreshes, wait.is_some(), || {}).await {
                return;
            }
        }
    }

    /// One reading, and how long a rate limit asks to wait.
    async fn read_once(&self) -> (Outcome, Option<Duration>) {
        let Some(token) = token(&self.context).await else {
            return (Err(Failure::NoCredentials), None);
        };
        let client = Client {
            context: &self.context,
            token,
        };
        let read = match self.number {
            None => self.open(&client).await.map(Reading::Issues),
            Some(number) => self.one(&client, number).await.map(Reading::Issue),
        };
        match read {
            Ok(reading) => (Ok(reading), None),
            Err(failure) => {
                if failure == http::Failure::Unauthorized {
                    // A refused token reads as no account any more.
                    self.context.accounts.lock().unwrap().remove(&client.token);
                }
                let (failure, wait) = failed(&self.context, &self.at(), failure).await;
                (Err(failure), wait)
            }
        }
    }

    /// Where the readings go, for the log.
    fn at(&self) -> String {
        match self.number {
            None => format!("{} work items", self.names.tracker()),
            Some(number) => format!("{} work item #{number}", self.names.tracker()),
        }
    }

    /// The open work items, the reader's apart: at most [`MAX`], the most
    /// recently created, `truncated` when there are more.
    async fn open(&self, client: &Client<'_>) -> Result<Issues, http::Failure> {
        let account = self.account(client).await?;
        let kinds = self.kinds(client).await?;
        let path = format!(
            "{}/wit/wiql?$top={}&{VERSION}",
            self.names.project_api(),
            MAX + 1
        );
        let (all, mine) = (
            json!({ "query": wiql(&kinds, false) }),
            json!({ "query": wiql(&kinds, true) }),
        );
        let (all, mine) = tokio::try_join!(client.post(&path, &all), client.post(&path, &mine))?;
        let mut ids = ids_of(&all)?;
        let truncated = ids.len() > MAX;
        ids.truncate(MAX);
        let mine: HashSet<u64> = ids_of(&mine)?.into_iter().collect();
        let fields = self.fields(client, &ids).await?;
        let mut issues = Issues {
            account,
            truncated,
            ..Issues::default()
        };
        for id in ids {
            let Some(fields) = fields.get(&id) else {
                continue;
            };
            let issue = issue_of(id, fields, true, &self.names);
            if mine.contains(&id) {
                issues.mine.push(issue);
            } else {
                issues.others.push(issue);
            }
        }
        Ok(issues)
    }

    /// The title and type of the work items `ids`, by id; one deleted since
    /// the query is left out.
    async fn fields(
        &self,
        client: &Client<'_>,
        ids: &[u64],
    ) -> Result<HashMap<u64, Value>, http::Failure> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids: Vec<String> = ids.iter().map(u64::to_string).collect();
        let path = format!(
            "{}/wit/workitems?ids={}&fields=System.Title,System.WorkItemType&errorPolicy=omit&{VERSION}",
            self.names.project_api(),
            ids.join(",")
        );
        let items = client.get(&path).await?;
        let items = items["value"]
            .as_array()
            .ok_or_else(|| http::Failure::Unreadable("the work items are not a list".into()))?;
        Ok(items
            .iter()
            .filter_map(|i| Some((i["id"].as_u64()?, i["fields"].clone())))
            .collect())
    }

    /// Work item `number`, open unless its state is in a closed category of
    /// its type. One of another project is not found here.
    async fn one(&self, client: &Client<'_>, number: u64) -> Result<Issue, http::Failure> {
        let path = format!(
            "{}/wit/workitems/{number}?fields=System.Title,System.WorkItemType,System.State,System.TeamProject&{VERSION}",
            self.names.project_api()
        );
        let item = client.get(&path).await?;
        let fields = &item["fields"];
        let project = fields["System.TeamProject"].as_str().unwrap_or_default();
        if project.to_lowercase() != self.names.project {
            return Err(http::Failure::NotFound);
        }
        let kinds = self.kinds(client).await?;
        let open = is_open(
            &kinds,
            fields["System.WorkItemType"].as_str().unwrap_or_default(),
            fields["System.State"].as_str().unwrap_or_default(),
        );
        Ok(issue_of(number, fields, open, &self.names))
    }

    /// The account the token reads as, in lowercase, asked once.
    async fn account(&self, client: &Client<'_>) -> Result<String, http::Failure> {
        let kept = self
            .context
            .accounts
            .lock()
            .unwrap()
            .get(&client.token)
            .cloned();
        if let Some(account) = kept {
            return Ok(account);
        }
        let path = format!("/{}/_apis/connectionData", encode(&self.names.org));
        let data = client.get(&path).await?;
        let user = &data["authenticatedUser"];
        let account = user["properties"]["Account"]["$value"]
            .as_str()
            .or_else(|| user["providerDisplayName"].as_str())
            .ok_or_else(|| http::Failure::Unreadable("no authenticated user".into()))?
            .to_lowercase();
        let mut accounts = self.context.accounts.lock().unwrap();
        // Every target shares one token: the accounts of the tokens it
        // replaced, renewed every hour from `az`, serve no more.
        accounts.clear();
        accounts.insert(client.token.clone(), account.clone());
        Ok(account)
    }

    /// The project's work item types with their closed states, read again
    /// after [`KINDS_FOR`].
    async fn kinds(&self, client: &Client<'_>) -> Result<Arc<Vec<Kind>>, http::Failure> {
        let api = self.names.project_api();
        let kept = self.context.kinds.lock().unwrap().get(&api).cloned();
        if let Some((at, kinds)) = kept
            && at.elapsed() < KINDS_FOR
        {
            return Ok(kinds);
        }
        let types = client
            .get(&format!("{api}/wit/workitemtypes?{VERSION}"))
            .await?;
        let mut kinds = Vec::new();
        for name in list(&types).iter().filter_map(|t| t["name"].as_str()) {
            let path = format!("{api}/wit/workitemtypes/{}/states?{VERSION}", encode(name));
            let states = client.get(&path).await?;
            kinds.push(Kind {
                name: name.to_owned(),
                closed: closed_states(&states),
            });
        }
        let kinds = Arc::new(kinds);
        self.context
            .kinds
            .lock()
            .unwrap()
            .insert(api, (Instant::now(), kinds.clone()));
        Ok(kinds)
    }
}

/// The `value` list of an answer, empty without one.
fn list(answer: &Value) -> &[Value] {
    answer["value"].as_array().map_or(&[], Vec::as_slice)
}

/// The states of a type whose category is a closed one.
fn closed_states(states: &Value) -> Vec<String> {
    list(states)
        .iter()
        .filter(|s| s["category"].as_str().is_some_and(|c| CLOSED.contains(&c)))
        .filter_map(|s| s["name"].as_str().map(str::to_owned))
        .collect()
}

/// Whether a work item of type `kind` in `state` is open; one of a type
/// not read is.
fn is_open(kinds: &[Kind], kind: &str, state: &str) -> bool {
    kinds
        .iter()
        .find(|k| k.name.eq_ignore_ascii_case(kind))
        .is_none_or(|k| !k.closed.iter().any(|s| s.eq_ignore_ascii_case(state)))
}

/// The WIQL of the project's open tickets, the reader's alone when `mine`,
/// the most recently created first.
fn wiql(kinds: &[Kind], mine: bool) -> String {
    let mut query =
        String::from("SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project");
    for category in NOT_TICKETS {
        let _ = write!(
            query,
            " AND [System.WorkItemType] NOT IN GROUP '{category}'"
        );
    }
    let open: Vec<String> = kinds.iter().map(open_clause).collect();
    if !open.is_empty() {
        let _ = write!(query, " AND ({})", open.join(" OR "));
    }
    if mine {
        query.push_str(" AND [System.AssignedTo] = @Me");
    }
    query.push_str(" ORDER BY [System.CreatedDate] DESC");
    query
}

/// The open work items of one type: those not in its closed states.
fn open_clause(kind: &Kind) -> String {
    let of_type = format!("[System.WorkItemType] = {}", quote(&kind.name));
    if kind.closed.is_empty() {
        return of_type;
    }
    let closed: Vec<String> = kind.closed.iter().map(|s| quote(s)).collect();
    format!(
        "({of_type} AND [System.State] NOT IN ({}))",
        closed.join(", ")
    )
}

/// A WIQL string literal.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The ids a WIQL answer lists, in its order.
fn ids_of(answer: &Value) -> Result<Vec<u64>, http::Failure> {
    let items = answer["workItems"]
        .as_array()
        .ok_or_else(|| http::Failure::Unreadable("the query listed no work items".into()))?;
    Ok(items.iter().filter_map(|i| i["id"].as_u64()).collect())
}

fn issue_of(id: u64, fields: &Value, open: bool, names: &Names) -> Issue {
    Issue {
        number: id,
        title: fields["System.Title"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        kind: fields["System.WorkItemType"].as_str().map(str::to_owned),
        url: format!("{}/_workitems/edit/{id}", names.web()),
        open,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::azure::Azure;
    use crate::testing::Server;
    use crate::tools::http::Http;

    fn kind(name: &str, closed: &[&str]) -> Kind {
        Kind {
            name: name.into(),
            closed: closed.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// The query excludes, type by type, the states of the closed
    /// categories, and the types that are not tickets by their category.
    #[test]
    fn the_wiql_excludes_closed_states_per_type_and_test_types() {
        let kinds = [
            kind("Bug", &["Closed", "Removed"]),
            kind("Customer's Ask", &["Done"]),
            kind("Epic", &[]),
        ];
        assert_eq!(
            wiql(&kinds, true),
            "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project \
             AND [System.WorkItemType] NOT IN GROUP 'Microsoft.HiddenCategory' \
             AND [System.WorkItemType] NOT IN GROUP 'Microsoft.TestCaseCategory' \
             AND [System.WorkItemType] NOT IN GROUP 'Microsoft.TestPlanCategory' \
             AND [System.WorkItemType] NOT IN GROUP 'Microsoft.TestSuiteCategory' \
             AND [System.WorkItemType] NOT IN GROUP 'Microsoft.SharedStepCategory' \
             AND (([System.WorkItemType] = 'Bug' AND [System.State] NOT IN ('Closed', 'Removed')) \
             OR ([System.WorkItemType] = 'Customer''s Ask' AND [System.State] NOT IN ('Done')) \
             OR [System.WorkItemType] = 'Epic') \
             AND [System.AssignedTo] = @Me ORDER BY [System.CreatedDate] DESC"
        );
        assert!(!wiql(&kinds, false).contains("@Me"));
    }

    /// A project with the types `Bug` and `Task`; three open work items,
    /// the newest first, `3` assigned to the reader; `9` deleted since.
    async fn project() -> Server {
        Server::start(|request| {
            let line = request.lines().next().unwrap_or_default().to_owned();
            let body = if line.contains("/o/_apis/connectiondata") {
                json!({ "authenticatedUser": { "providerDisplayName": "Me",
                    "properties": { "Account": { "$type": "System.String", "$value": "Me@Corp.com" } } } })
            } else if line.contains("/wit/workitemtypes/bug/states") {
                json!({ "value": [{ "name": "New", "category": "Proposed" },
                    { "name": "Resolved", "category": "Resolved" },
                    { "name": "Closed", "category": "Completed" }] })
            } else if line.contains("/wit/workitemtypes/task/states") {
                json!({ "value": [{ "name": "To Do", "category": "Proposed" },
                    { "name": "Done", "category": "Completed" },
                    { "name": "Removed", "category": "Removed" }] })
            } else if line.contains("/wit/workitemtypes?") {
                json!({ "value": [{ "name": "Bug" }, { "name": "Task" }] })
            } else if line.starts_with("post") && line.contains("/wit/wiql?$top=201") {
                let ids: &[u64] = if request.contains("@me") { &[3] } else { &[3, 9, 2, 1] };
                let items: Vec<Value> = ids.iter().map(|id| json!({ "id": id })).collect();
                json!({ "workItems": items })
            } else if line.contains("/wit/workitems?ids=3,9,2,1&") {
                let item = |id: u64, kind: &str| json!({ "id": id, "fields": {
                    "System.Title": format!("item {id}"), "System.WorkItemType": kind } });
                json!({ "value": [item(1, "Task"), item(2, "Bug"), item(3, "Bug")] })
            } else if line.contains("/wit/workitems/5?") {
                json!({ "id": 5, "fields": { "System.Title": "five", "System.WorkItemType": "Task",
                    "System.State": "Done", "System.TeamProject": "P" } })
            } else if line.contains("/wit/workitems/6?") {
                json!({ "id": 6, "fields": { "System.Title": "six", "System.WorkItemType": "Bug",
                    "System.State": "Resolved", "System.TeamProject": "P" } })
            } else if line.contains("/wit/workitems/8?") {
                json!({ "id": 8, "fields": { "System.TeamProject": "Other" } })
            } else {
                return (404, vec![], String::new());
            };
            (200, vec![], body.to_string())
        })
        .await
    }

    async fn tracker(server: &Server, number: Option<u64>) -> Tracker {
        let azure = Azure::new(
            Http::new().unwrap(),
            server.base(),
            crate::credentials::testing::none(),
        );
        *azure.context.token.lock().await =
            Some(("t".into(), None, crate::credentials::Stamp::default()));
        Tracker {
            context: azure.context,
            names: Names {
                org: "o".into(),
                project: "p".into(),
                repo: "r".into(),
            },
            number,
        }
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

    /// The reader's work items are told from the others by `@Me`, in the
    /// order of the query; the account and the types are asked once.
    #[tokio::test]
    async fn the_open_work_items_split_between_mine_and_others() {
        let server = project().await;
        let tracker = tracker(&server, None).await;
        let replaced = ("old".to_owned(), "me@corp.com".to_owned());
        tracker.context.accounts.lock().unwrap().extend([replaced]);
        let Reading::Issues(issues) = read(&tracker).await else {
            panic!("not issues");
        };
        assert_eq!(issues.account, "me@corp.com");
        assert_eq!(numbers(&issues.mine), [3]);
        assert_eq!(numbers(&issues.others), [2, 1], "9 is gone");
        assert_eq!(issues.others[0].kind.as_deref(), Some("Bug"));
        assert_eq!(
            issues.others[0].url,
            "https://dev.azure.com/o/p/_workitems/edit/2"
        );
        assert!(!issues.truncated);
        read(&tracker).await;
        let requests = server.requests();
        let asked = |what: &str| requests.iter().filter(|r| r.contains(what)).count();
        assert_eq!(asked("connectiondata"), 1);
        let accounts = tracker.context.accounts.lock().unwrap();
        assert!(
            !accounts.contains_key("old"),
            "a replaced token is forgotten"
        );
        assert_eq!(asked("/wit/workitemtypes?"), 1);
        let query = requests.iter().find(|r| r.contains("/wit/wiql")).unwrap();
        assert!(query.contains(
            "([system.workitemtype] = 'task' and [system.state] not in ('done', 'removed'))"
        ));
    }

    /// A work item is open unless its state is in a closed category; one
    /// missing, or of another project, is not found.
    #[tokio::test]
    async fn a_work_item_is_open_closed_or_not_found() {
        let server = project().await;
        let Reading::Issue(done) = read(&tracker(&server, Some(5)).await).await else {
            panic!("not an issue");
        };
        assert_eq!(
            (done.number, done.open, done.kind.as_deref()),
            (5, false, Some("Task"))
        );
        let Reading::Issue(resolved) = read(&tracker(&server, Some(6)).await).await else {
            panic!("not an issue");
        };
        assert!(resolved.open, "resolved is not completed");
        for number in [7, 8] {
            assert_eq!(
                tracker(&server, Some(number)).await.read_once().await,
                (Err(Failure::NotFound), None),
                "#{number}"
            );
        }
    }

    /// More than 200 open work items: the 200 newest, `truncated`.
    #[tokio::test]
    async fn past_200_work_items_the_list_is_truncated() {
        let server = Server::start(|request| {
            let line = request.lines().next().unwrap_or_default().to_owned();
            let body = if line.contains("connectiondata") {
                json!({ "authenticatedUser": { "providerDisplayName": "Me" } })
            } else if line.contains("/wit/workitemtypes?") {
                json!({ "value": [] })
            } else if line.contains("/wit/wiql") {
                let ids: Vec<Value> = (1..=201).rev().map(|id| json!({ "id": id })).collect();
                json!({ "workItems": if request.contains("@me") { vec![] } else { ids } })
            } else if line.contains("/wit/workitems?") {
                let items: Vec<Value> = (2..=201)
                    .map(|id| json!({ "id": id, "fields": { "System.Title": "t" } }))
                    .collect();
                json!({ "value": items })
            } else {
                return (404, vec![], String::new());
            };
            (200, vec![], body.to_string())
        })
        .await;
        let Reading::Issues(issues) = read(&tracker(&server, None).await).await else {
            panic!("not issues");
        };
        assert!(issues.truncated);
        assert_eq!(issues.others.len(), 200);
        assert_eq!(
            (issues.account.as_str(), issues.others[0].number),
            ("me", 201)
        );
        let fields = server
            .requests()
            .into_iter()
            .find(|r| r.contains("/wit/workitems?"))
            .unwrap();
        assert!(!fields.contains(",1&"), "the 201st is not asked");
    }
}
