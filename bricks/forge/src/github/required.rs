//! The checks a base requires before a PR merges into it: the union of its
//! branch protection's (GraphQL, the only API that shows them without admin
//! rights) and of the `required_status_checks` rules of its rulesets (REST,
//! conditional). Read once per base for every branch followed, then at most
//! once per [`RHYTHM`]'s normal pace. A source that fails keeps what it last
//! gave: the branch's reading never fails for it.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::time::Instant;

use super::RHYTHM;
use super::read::{HEADERS, encode};
use crate::model::Ci;
use crate::tools::http::{Failure, Http};

const PROTECTION: &str = "query($owner: String!, $name: String!, $ref: String!) { \
    repository(owner: $owner, name: $name) { ref(qualifiedName: $ref) { \
    refUpdateRule { requiredStatusCheckContexts } } } }";

/// A base of a repository, and how to read it.
pub struct Base<'a> {
    pub http: &'a Http,
    pub api: &'a str,
    pub token: &'a str,
    pub owner: &'a str,
    pub name: &'a str,
    pub base: &'a str,
}

/// What each source of one base last gave, and when to read them again.
#[derive(Default)]
struct Entry {
    /// When to read the sources again; `None` reads them at once.
    next: Option<Instant>,
    protection: Option<Vec<String>>,
    rulesets: Option<Vec<String>>,
}

impl Entry {
    /// The union of what the sources gave; `None` when none ever answered.
    fn names(&self) -> Option<BTreeSet<String>> {
        if self.protection.is_none() && self.rulesets.is_none() {
            return None;
        }
        let all = self.protection.iter().chain(&self.rulesets).flatten();
        Some(all.cloned().collect())
    }
}

/// The required check names of each base, shared by every branch followed.
/// One lock per base: two branches on one base read it once.
#[derive(Clone, Default)]
pub struct Required(Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<Entry>>>>>);

impl Required {
    /// The names `base` requires, read again when due; `None` when unknown.
    pub async fn names(&self, base: &Base<'_>) -> Option<BTreeSet<String>> {
        // Owner and name come in lowercase; a branch name keeps its case.
        let key = format!("{}/{}#{}", base.owner, base.name, base.base);
        let entry = self.0.lock().unwrap().entry(key).or_default().clone();
        let mut entry = entry.lock().await;
        let now = Instant::now();
        if entry.next.is_none_or(|next| now >= next) {
            let (protection, rulesets) = tokio::join!(protection(base), rulesets(base));
            keep(&mut entry.protection, protection, base, "branch protection");
            keep(&mut entry.rulesets, rulesets, base, "rulesets");
            // Set once read: a reading cancelled midway leaves the next
            // caller to read again.
            entry.next = Some(now + RHYTHM.normal);
        }
        entry.names()
    }
}

/// Keeps what a source gave, or logs why it gave nothing.
fn keep(
    slot: &mut Option<Vec<String>>,
    read: Result<Vec<String>, Failure>,
    base: &Base,
    what: &str,
) {
    match read {
        Ok(names) => *slot = Some(names),
        Err(failure) => crate::log(format_args!(
            "github.com/{}/{}@{}: {what} unread: {failure:?}",
            base.owner, base.name, base.base
        )),
    }
}

/// The checks of the base's branch protection; none without protection.
async fn protection(base: &Base<'_>) -> Result<Vec<String>, Failure> {
    let variables = json!({
        "owner": base.owner,
        "name": base.name,
        "ref": format!("refs/heads/{}", base.base),
    });
    let answer = graphql(base, PROTECTION, variables).await?;
    let contexts =
        &answer["data"]["repository"]["ref"]["refUpdateRule"]["requiredStatusCheckContexts"];
    Ok(strings(contexts.as_array().into_iter().flatten()))
}

/// Answers `query` with `variables` on the base's repository; GraphQL
/// errors are a failure.
pub async fn graphql(base: &Base<'_>, query: &str, variables: Value) -> Result<Value, Failure> {
    let body = json!({ "query": query, "variables": variables });
    let url = format!("{}/graphql", base.api);
    let answer = base.http.post(&url, base.token, HEADERS, &body).await?;
    if let Some(errors) = answer.get("errors") {
        return Err(Failure::Unreadable(format!("GraphQL errors: {errors}")));
    }
    Ok(answer)
}

/// The checks the `required_status_checks` rules of the base's rulesets
/// require.
async fn rulesets(base: &Base<'_>) -> Result<Vec<String>, Failure> {
    let url = format!(
        "{}/repos/{}/{}/rules/branches/{}?per_page=100",
        base.api,
        base.owner,
        base.name,
        encode(base.base)
    );
    let rules = base.http.get(&url, base.token, HEADERS).await?;
    let checks = rules
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["type"] == "required_status_checks")
        .filter_map(|r| r["parameters"]["required_status_checks"].as_array())
        .flatten()
        .map(|c| &c["context"]);
    Ok(strings(checks))
}

fn strings<'a>(values: impl Iterator<Item = &'a Value>) -> Vec<String> {
    values
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// Marks each job of `ci` required or not by its name, and lists the
/// `names` no job carries. Empty `names` say nothing: the base has no rule.
pub fn mark(ci: &mut Ci, names: &BTreeSet<String>) {
    if names.is_empty() {
        return;
    }
    let jobs = || ci.pipelines.iter().flat_map(|p| &p.jobs);
    let seen: BTreeSet<&str> = jobs().map(|j| j.name.as_str()).collect();
    ci.missing_required = names
        .iter()
        .filter(|n| !seen.contains(n.as_str()))
        .cloned()
        .collect();
    for job in ci.pipelines.iter_mut().flat_map(|p| &mut p.jobs) {
        job.required = Some(names.contains(&job.name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::State;
    use crate::model::tests::{ci, job, pipeline};
    use crate::testing::Server;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|&n| n.to_owned()).collect()
    }

    /// Branch protection requires `check`, a ruleset `e2e` and the commit
    /// status `deploy`, unless the handler is told otherwise.
    fn forge(
        graphql: (u16, &'static str),
    ) -> impl Fn(&str) -> (u16, Vec<(&'static str, &'static str)>, String) {
        move |request: &str| {
            let line = request.lines().next().unwrap_or_default();
            if line.starts_with("post /graphql") {
                (graphql.0, vec![], graphql.1.to_owned())
            } else if line.contains("/repos/o/r/rules/branches/main") {
                let rules = json!([
                    { "type": "deletion" },
                    { "type": "required_status_checks", "parameters": {
                        "required_status_checks": [{ "context": "e2e" }, { "context": "deploy" }] } },
                ]);
                (200, vec![("ETag", "\"r1\"")], rules.to_string())
            } else {
                (404, vec![], String::new())
            }
        }
    }

    const PROTECTED: &str = r#"{"data":{"repository":{"ref":{"refUpdateRule":
        {"requiredStatusCheckContexts":["check"]}}}}}"#;

    async fn names(server: &Server, required: &Required) -> Option<BTreeSet<String>> {
        let (http, api) = (Http::new().unwrap(), server.base());
        let base = Base {
            http: &http,
            api: &api,
            token: "t",
            owner: "o",
            name: "r",
            base: "main",
        };
        required.names(&base).await
    }

    #[tokio::test]
    async fn the_names_are_the_union_of_protection_and_rulesets() {
        let server = Server::start(forge((200, PROTECTED))).await;
        let required = Required::default();
        assert_eq!(
            names(&server, &required).await,
            Some(set(&["check", "deploy", "e2e"]))
        );
        let requests = server.requests();
        let query = requests.iter().find(|r| r.starts_with("post")).unwrap();
        assert!(query.contains("refupdaterule { requiredstatuscheckcontexts }"));
        assert!(query.contains(r#""ref":"refs/heads/main""#));
        assert!(
            requests
                .iter()
                .any(|r| r.starts_with("get /repos/o/r/rules/branches/main?per_page=100 "))
        );
        assert_eq!(names(&server, &required).await.unwrap().len(), 3);
        assert_eq!(server.requests().len(), 2, "read once per normal pace");
    }

    #[tokio::test]
    async fn a_failing_graphql_keeps_the_rulesets_or_the_names_it_gave() {
        let server = Server::start(forge((200, r#"{"errors":[{"message":"no"}]}"#))).await;
        let required = Required::default();
        assert_eq!(
            names(&server, &required).await,
            Some(set(&["deploy", "e2e"]))
        );

        let server = Server::start(forge((200, PROTECTED))).await;
        let required = Required::default();
        names(&server, &required).await;
        let failing = Server::start(|_: &str| (502, vec![], String::new())).await;
        for entry in required.0.lock().unwrap().values() {
            entry.try_lock().unwrap().next = None;
        }
        assert_eq!(
            names(&failing, &required).await,
            Some(set(&["check", "deploy", "e2e"])),
            "both sources keep what they gave"
        );
        assert_eq!(failing.requests().len(), 2, "both were asked again");
    }

    #[tokio::test]
    async fn sources_that_never_answered_say_nothing() {
        let failing = Server::start(|_: &str| (500, vec![], String::new())).await;
        assert_eq!(names(&failing, &Required::default()).await, None);
    }

    #[test]
    fn jobs_are_marked_by_name_and_the_absent_ones_listed() {
        let mut ci = ci(vec![pipeline(
            State::Running,
            vec![
                job("check", State::Running, None),
                job("lint", State::Pass, None),
            ],
        )]);
        mark(&mut ci, &set(&["check", "e2e"]));
        let required: Vec<_> = ci.pipelines[0].jobs.iter().map(|j| j.required).collect();
        assert_eq!(required, [Some(true), Some(false)]);
        assert_eq!(ci.missing_required, ["e2e"]);
    }

    #[test]
    fn no_rule_says_nothing() {
        let mut ci = ci(vec![pipeline(
            State::Pass,
            vec![job("check", State::Pass, None)],
        )]);
        mark(&mut ci, &BTreeSet::new());
        assert_eq!(ci.pipelines[0].jobs[0].required, None);
        assert!(ci.missing_required.is_empty());
    }
}
