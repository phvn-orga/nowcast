//! Azure DevOps through its REST API: a branch's PR and the CI of its head,
//! mapped onto the neutral model.
//!
//! The CI of an open PR is that of its policies: a build policy designates
//! the build Azure DevOps shows on the PR (`context.buildId`), and requires
//! all of its jobs when it is blocking; a status policy requires one PR
//! status. The PR's other builds count when they tested its merge commit
//! (`lastMergeCommit`), the branch's builds when they tested its head, and
//! its statuses form one `external` pipeline. Without an open PR, the CI is
//! that of the builds of the branch's head.
//! Azure DevOps has no merge queue: `queue` is always `None`.

use std::collections::HashSet;

use serde_json::Value;

use super::{Client, Names, VERSION, timeline};
use crate::model::{Ci, Job, Pipeline, Pr, State, Trigger, summary};
use crate::provider::BranchReading;
use crate::tools::http::{Failure, encode};

/// The type of a build policy.
const BUILD_POLICY: &str = "0609b952-1397-4640-95ec-e00a01b2c241";
/// The type of a status policy.
const STATUS_POLICY: &str = "cbdc66da-9728-4af8-aada-9a5a32e4a226";
/// The PRs one listing reads: a full one may leave the branch's out.
const PAGE: usize = 100;
/// The builds of a ref one listing reads, the latest queued first.
const BUILDS: usize = 50;

/// What a branch remembers between readings: the latest iteration of its
/// open PR, and the head it was read for.
#[derive(Default)]
pub struct Memory {
    iteration: Option<(u64, String, u64)>,
}

/// Reads the PR of `branch` and the CI of its head.
pub async fn read(
    client: &Client<'_>,
    names: &Names,
    branch: &str,
    memory: &mut Memory,
) -> Result<BranchReading, Failure> {
    let found = find_pr(client, names, branch).await?;
    let (head_sha, ci) = match found.as_ref().filter(|(pr, _)| pr.is_open()) {
        Some((pr, raw)) => {
            let ci = pr_ci(client, names, branch, pr, raw, memory).await?;
            (Some(pr.head_sha.clone()), Some(ci))
        }
        None => {
            memory.iteration = None;
            let head = head(client, names, branch).await?;
            let ci = match &head {
                Some(sha) => Some(branch_ci(client, names, branch, sha).await?),
                None => None,
            };
            (head, ci)
        }
    };
    Ok(BranchReading {
        head_sha,
        pr: found.map(|(pr, _)| pr),
        ci,
    })
}

/// The branch's PR, active first, else its latest, with what the API gave.
/// The active PRs are one listing per repository, shared by every branch.
async fn find_pr(
    client: &Client<'_>,
    names: &Names,
    branch: &str,
) -> Result<Option<(Pr, Value)>, Failure> {
    let source = format!("refs/heads/{branch}");
    let pulls = format!("{}/pullrequests?searchCriteria.", names.repo_api());
    let active = client
        .get_fresh(&format!("{pulls}status=active&$top={PAGE}&{VERSION}"))
        .await?;
    let mut found = own(&active, &source).into_iter().next();
    let own_source = format!("{pulls}sourceRefName={}", encode(&source));
    if found.is_none() && list(&active).len() >= PAGE {
        let mine = client
            .get(&format!(
                "{own_source}&searchCriteria.status=active&{VERSION}"
            ))
            .await?;
        found = own(&mine, &source).into_iter().next();
    }
    if found.is_none() {
        let all = client
            .get(&format!(
                "{own_source}&searchCriteria.status=all&$top={PAGE}&{VERSION}"
            ))
            .await?;
        found = own(&all, &source)
            .into_iter()
            .max_by_key(|p| p["pullRequestId"].as_u64());
    }
    Ok(found.and_then(|raw| Some((pr_of(&raw, names)?, raw))))
}

/// The `value` list of an answer, empty without one.
fn list(answer: &Value) -> &[Value] {
    answer["value"].as_array().map_or(&[], Vec::as_slice)
}

/// The PRs whose source is `source` in this very repository: a fork's
/// branch of the same name is another branch.
fn own(pulls: &Value, source: &str) -> Vec<Value> {
    list(pulls)
        .iter()
        .filter(|p| p["sourceRefName"] == source && p["forkSource"].is_null())
        .cloned()
        .collect()
}

/// A PR of the API: `active`, `completed` and `abandoned` are `open`,
/// `merged` and `closed`; its head is the last commit of its source.
fn pr_of(p: &Value, names: &Names) -> Option<Pr> {
    let state = match p["status"].as_str()? {
        "active" => "open",
        "completed" => "merged",
        _ => "closed",
    };
    let number = p["pullRequestId"].as_u64()?;
    let base = p["targetRefName"].as_str().unwrap_or_default();
    let auto_merge = p["autoCompleteSetBy"].as_object().map(|by| {
        let who = by.get("uniqueName").and_then(Value::as_str);
        let strategy = p["completionOptions"]["mergeStrategy"].as_str();
        (who.map(str::to_lowercase), strategy.map(merge_method))
    });
    Some(Pr {
        number,
        state,
        draft: p["isDraft"].as_bool().unwrap_or(false),
        base: base.strip_prefix("refs/heads/").unwrap_or(base).to_owned(),
        head_sha: p["lastMergeSourceCommit"]["commitId"].as_str()?.to_owned(),
        url: format!(
            "{}/_git/{}/pullrequest/{number}",
            names.web(),
            encode(&names.repo)
        ),
        auto_merge,
        queue: None,
        dequeued: None,
    })
}

/// A merge strategy as the contract names it, else the forge's value.
fn merge_method(strategy: &str) -> String {
    match strategy {
        "noFastForward" => "merge",
        "squash" => "squash",
        "rebase" => "rebase",
        other => other,
    }
    .to_owned()
}

/// The head of `branch`; `None` when it is not on the forge. The filter
/// matches by prefix: the very name is picked.
async fn head(client: &Client<'_>, names: &Names, branch: &str) -> Result<Option<String>, Failure> {
    let filter = encode(&format!("heads/{branch}"));
    let path = format!("{}/refs?filter={filter}&{VERSION}", names.repo_api());
    let refs = client.get(&path).await?;
    let name = format!("refs/heads/{branch}");
    Ok(list(&refs)
        .iter()
        .find(|r| r["name"] == name.as_str())
        .and_then(|r| r["objectId"].as_str())
        .map(str::to_owned))
}

/// The path listing the builds of `git_ref`, the latest queued first.
fn builds_path(names: &Names, git_ref: &str) -> String {
    format!(
        "{}/build/builds?branchName={}&queryOrder=queueTimeDescending&$top={BUILDS}&{VERSION}",
        names.project_api(),
        encode(git_ref)
    )
}

/// The path listing the builds of `branch`.
fn branch_builds_path(names: &Names, branch: &str) -> String {
    builds_path(names, &format!("refs/heads/{branch}"))
}

/// The CI of `sha`, the head of `branch` without an open PR: its builds.
/// Nothing is required: the requirement comes from a PR's policies.
async fn branch_ci(
    client: &Client<'_>,
    names: &Names,
    branch: &str,
    sha: &str,
) -> Result<Ci, Failure> {
    let builds = client.get(&branch_builds_path(names, branch)).await?;
    let mut pipelines = Vec::new();
    for build in latest(&builds, sha, &HashSet::new()) {
        pipelines.push(build_pipeline(client, names, &build, None).await?);
    }
    Ok(Ci {
        sha: sha.to_owned(),
        pipelines,
        ..Ci::default()
    })
}

/// The builds that tested `sha`, the latest of each definition and trigger,
/// those of the definitions in `left_out` aside. `individualCI` and
/// `batchedCI` are both a push, so one pipeline; a reason without a neutral
/// trigger keeps its own.
fn latest(builds: &Value, sha: &str, left_out: &HashSet<u64>) -> Vec<Value> {
    let mut tested: Vec<&Value> = list(builds)
        .iter()
        .filter(|b| b["sourceVersion"] == sha)
        .filter(|b| {
            b["definition"]["id"]
                .as_u64()
                .is_none_or(|d| !left_out.contains(&d))
        })
        .collect();
    tested.sort_by_key(|b| std::cmp::Reverse(b["id"].as_u64()));
    let mut kept = HashSet::new();
    tested
        .into_iter()
        .filter(|b| {
            let reason = b["reason"].as_str().unwrap_or_default();
            let key = match trigger(reason) {
                Trigger::Other => reason,
                neutral => neutral.as_str(),
            };
            kept.insert((b["definition"]["id"].as_u64(), key))
        })
        .cloned()
        .collect()
}

/// A build with its jobs, each `required` as given.
async fn build_pipeline(
    client: &Client<'_>,
    names: &Names,
    build: &Value,
    required: Option<bool>,
) -> Result<Pipeline, Failure> {
    let (mut jobs, attempt) = timeline::jobs(client, names, build).await?;
    for job in &mut jobs {
        job.required = required;
    }
    Ok(pipeline_of(build, jobs, attempt, required))
}

/// A build as a pipeline. A running build whose active jobs all wait for a
/// person waits too: an approval may last for days.
fn pipeline_of(build: &Value, jobs: Vec<Job>, attempt: u64, required: Option<bool>) -> Pipeline {
    let (mut state, warning) = build_state(build["status"].as_str(), build["result"].as_str());
    let active = jobs.iter().filter(|j| j.state.is_active());
    if state == State::Running
        && active.clone().count() > 0
        && active.clone().all(|j| j.state == State::Waiting)
    {
        state = State::Waiting;
    }
    let reason = build["reason"].as_str().unwrap_or_default();
    let text = |v: &Value| v.as_str().map(str::to_owned);
    Pipeline {
        id: build["id"]
            .as_u64()
            .map(|i| i.to_string())
            .unwrap_or_default(),
        name: text(&build["definition"]["name"]).unwrap_or_default(),
        definition: build["definition"]["id"].as_u64().map(|d| d.to_string()),
        trigger: trigger(reason),
        raw_trigger: reason.to_owned(),
        attempt,
        sha: text(&build["sourceVersion"]),
        git_ref: text(&build["sourceBranch"]),
        state,
        warning,
        required,
        started_at: text(&build["startTime"]),
        finished_at: text(&build["finishTime"]),
        url: text(&build["_links"]["web"]["href"]),
        jobs,
    }
}

/// A build's state: its `status` while it is not over, then its `result`.
fn build_state(status: Option<&str>, result: Option<&str>) -> (State, bool) {
    match status {
        Some("notStarted" | "postponed") => (State::Queued, false),
        Some("inProgress" | "cancelling") => (State::Running, false),
        Some("completed") => match result {
            Some("succeeded") => (State::Pass, false),
            Some("partiallySucceeded") => (State::Pass, true),
            Some("failed") => (State::Fail, false),
            Some("canceled") => (State::Cancelled, false),
            _ => (State::Unknown, false),
        },
        _ => (State::Unknown, false),
    }
}

/// What started a build, from its `reason`.
fn trigger(reason: &str) -> Trigger {
    match reason {
        "individualCI" | "batchedCI" => Trigger::Push,
        "pullRequest" => Trigger::PullRequest,
        "manual" => Trigger::Manual,
        "schedule" => Trigger::Schedule,
        "resourceTrigger" => Trigger::Downstream,
        _ => Trigger::Other,
    }
}

/// A build policy as one PR evaluates it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BuildPolicy {
    evaluation: String,
    definition: Option<u64>,
    name: String,
    /// The build the evaluation designates, the one the PR shows.
    build: Option<u64>,
    blocking: bool,
    /// The evaluation's state: an expired one waits for a person.
    state: State,
}

/// A status policy: the status it requires, `<genre>/<name>`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StatusPolicy {
    context: String,
    blocking: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Policies {
    builds: Vec<BuildPolicy>,
    statuses: Vec<StatusPolicy>,
}

/// The enabled build and status policies of a PR's evaluations; the others
/// (reviewers, comments, merge strategy) say whether the PR can merge, not
/// whether its CI passes.
fn policies(evaluations: &Value) -> Policies {
    let mut policies = Policies::default();
    for e in list(evaluations) {
        let config = &e["configuration"];
        if config["isEnabled"] == false || config["isDeleted"] == true {
            continue;
        }
        let blocking = config["isBlocking"].as_bool().unwrap_or(false);
        let settings = &config["settings"];
        match config["type"]["id"].as_str() {
            Some(BUILD_POLICY) => policies.builds.push(build_policy(e, blocking)),
            Some(STATUS_POLICY) => {
                let name = settings["statusName"].as_str().unwrap_or_default();
                policies.statuses.push(StatusPolicy {
                    context: context_name(settings["statusGenre"].as_str(), name),
                    // A policy whose path filter does not match requires
                    // nothing.
                    blocking: blocking && e["status"] != "notApplicable",
                });
            }
            _ => {}
        }
    }
    policies
}

fn build_policy(e: &Value, blocking: bool) -> BuildPolicy {
    let settings = &e["configuration"]["settings"];
    let context = &e["context"];
    let definition = settings["buildDefinitionId"].as_u64();
    let name = [&settings["displayName"], &context["buildDefinitionName"]]
        .into_iter()
        .find_map(|n| n.as_str().filter(|n| !n.is_empty()))
        .map_or_else(
            || format!("build {}", definition.unwrap_or_default()),
            str::to_owned,
        );
    let waits = context["isExpired"] == true || settings["manualQueueOnly"] == true;
    BuildPolicy {
        evaluation: e["evaluationId"].as_str().unwrap_or_default().to_owned(),
        definition,
        name,
        build: context["buildId"].as_u64(),
        blocking,
        state: evaluation_state(e["status"].as_str(), waits),
    }
}

/// A policy evaluation's state. A `queued` one that waits for a person
/// (expired, or queued by hand only) is `waiting`: nothing runs until
/// someone queues the build.
fn evaluation_state(status: Option<&str>, waits: bool) -> State {
    match status {
        Some("queued") if waits => State::Waiting,
        Some("queued") => State::Queued,
        Some("running") => State::Running,
        Some("approved") => State::Pass,
        Some("rejected" | "broken") => State::Fail,
        Some("notApplicable") => State::Skipped,
        _ => State::Unknown,
    }
}

/// The CI of an open PR: its build policies, those whose path filter does
/// not match included, its other builds on its merge commit, the builds of
/// `branch` on its head, and its statuses. With an open PR, the policies say
/// what is required: every other job is not.
async fn pr_ci(
    client: &Client<'_>,
    names: &Names,
    branch: &str,
    pr: &Pr,
    raw: &Value,
    memory: &mut Memory,
) -> Result<Ci, Failure> {
    let project = raw["repository"]["project"]["id"]
        .as_str()
        .unwrap_or(&names.project);
    let artifact = format!("vstfs:///CodeReview/CodeReviewId/{project}/{}", pr.number);
    let merge_ref = format!("refs/pull/{}/merge", pr.number);
    let evaluations = format!(
        "{}/policy/evaluations?artifactId={}&includeNotApplicable=true&api-version=7.1-preview.1",
        names.project_api(),
        encode(&artifact)
    );
    let builds = builds_path(names, &merge_ref);
    let branch_builds = branch_builds_path(names, branch);
    let statuses = format!(
        "{}/pullRequests/{}/statuses?{VERSION}",
        names.repo_api(),
        pr.number
    );
    let (evaluations, builds, branch_builds, statuses) = tokio::try_join!(
        client.get(&evaluations),
        client.get(&builds),
        client.get(&branch_builds),
        client.get(&statuses),
    )?;
    let policies = policies(&evaluations);
    let mut pipelines = Vec::new();
    for policy in &policies.builds {
        let pipeline = policy_pipeline(client, names, &builds, policy, &merge_ref).await?;
        pipelines.push(pipeline);
    }
    // A policy whose path filter does not match designates no build: its
    // definition's builds still count.
    let covered = policies
        .builds
        .iter()
        .filter(|p| p.state != State::Skipped)
        .filter_map(|p| p.definition)
        .collect();
    let mut others = match raw["lastMergeCommit"]["commitId"].as_str() {
        Some(merge) => latest(&builds, merge, &covered),
        None => Vec::new(),
    };
    others.extend(latest(&branch_builds, &pr.head_sha, &HashSet::new()));
    for build in others {
        let published = |p: &Pipeline| {
            build["id"]
                .as_u64()
                .is_some_and(|id| p.id == id.to_string())
        };
        if !pipelines.iter().any(published) {
            pipelines.push(build_pipeline(client, names, &build, Some(false)).await?);
        }
    }
    let iteration = latest_iteration(client, names, pr, &statuses, memory).await?;
    let built = pipelines.iter().map(|p| p.id.clone()).collect();
    let (external, missing) = statuses_pipeline(&statuses, iteration, &built, &policies.statuses);
    pipelines.extend(external);
    Ok(Ci {
        sha: pr.head_sha.clone(),
        pipelines,
        missing_required: missing,
        expected_until: None,
    })
}

/// The pipeline of a build policy: the build its evaluation designates,
/// every job required when the policy blocks; without a build, expired, or
/// not applicable, a pipeline without job at the evaluation's state, which
/// carries the requirement. A queued evaluation still naming a completed
/// build names the previous one: the next has not shown up yet.
async fn policy_pipeline(
    client: &Client<'_>,
    names: &Names,
    builds: &Value,
    policy: &BuildPolicy,
    merge_ref: &str,
) -> Result<Pipeline, Failure> {
    let required = Some(policy.blocking);
    let designates = !matches!(policy.state, State::Waiting | State::Skipped);
    if let Some(id) = policy.build.filter(|_| designates) {
        let listed = list(builds).iter().find(|b| b["id"] == id).cloned();
        let build = match listed {
            Some(build) => Some(build),
            None => {
                let path = format!("{}/build/builds/{id}?{VERSION}", names.project_api());
                match client.get(&path).await {
                    Ok(build) => Some((*build).clone()),
                    Err(Failure::NotFound) => None,
                    Err(e) => return Err(e),
                }
            }
        };
        let stale = |b: &Value| policy.state == State::Queued && b["status"] == "completed";
        if let Some(build) = build.filter(|b| !stale(b)) {
            return build_pipeline(client, names, &build, required).await;
        }
    }
    Ok(Pipeline {
        id: policy.build.map_or_else(
            || format!("policy-{}", policy.evaluation),
            |b| b.to_string(),
        ),
        name: policy.name.clone(),
        definition: policy.definition.map(|d| d.to_string()),
        trigger: Trigger::PullRequest,
        raw_trigger: "policy".into(),
        attempt: 1,
        sha: None,
        git_ref: Some(merge_ref.to_owned()),
        state: policy.state,
        warning: false,
        required,
        started_at: None,
        finished_at: None,
        url: policy
            .build
            .map(|b| format!("{}/_build/results?buildId={b}", names.web())),
        jobs: Vec::new(),
    })
}

/// The PR's latest iteration, when a status names one: read once per head.
async fn latest_iteration(
    client: &Client<'_>,
    names: &Names,
    pr: &Pr,
    statuses: &Value,
    memory: &mut Memory,
) -> Result<Option<u64>, Failure> {
    if !list(statuses).iter().any(|s| s["iterationId"].is_u64()) {
        return Ok(None);
    }
    if let Some((number, head, iteration)) = &memory.iteration
        && *number == pr.number
        && *head == pr.head_sha
    {
        return Ok(Some(*iteration));
    }
    let path = format!(
        "{}/pullRequests/{}/iterations?{VERSION}",
        names.repo_api(),
        pr.number
    );
    let iterations = client.get(&path).await?;
    let latest = list(&iterations)
        .iter()
        .filter_map(|i| i["id"].as_u64())
        .max();
    memory.iteration = latest.map(|i| (pr.number, pr.head_sha.clone(), i));
    Ok(latest)
}

/// `<genre>/<name>`, or the name alone without a genre.
fn context_name(genre: Option<&str>, name: &str) -> String {
    match genre.filter(|g| !g.is_empty()) {
        Some(genre) => format!("{genre}/{name}"),
        None => name.to_owned(),
    }
}

/// The PR's statuses as one `external` pipeline, a job per context: the
/// latest of each, on the latest `iteration` or on the whole PR. A status a
/// build of the PR posts itself (its `targetUrl` names a build in `built`)
/// is that build's, already published. Each job is required when a
/// blocking status policy names it; such a policy that nothing answered is
/// missing.
fn statuses_pipeline(
    statuses: &Value,
    iteration: Option<u64>,
    built: &HashSet<String>,
    policies: &[StatusPolicy],
) -> (Option<Pipeline>, Vec<String>) {
    let mut latest: Vec<(String, &Value)> = Vec::new();
    for status in list(statuses) {
        let on = status["iterationId"].as_u64();
        if on.is_some_and(|on| Some(on) != iteration) || from_build(status, built) {
            continue;
        }
        let context = &status["context"];
        let name = context_name(
            context["genre"].as_str(),
            context["name"].as_str().unwrap_or_default(),
        );
        match latest.iter_mut().find(|(n, _)| *n == name) {
            Some(kept) if kept.1["id"].as_u64() < status["id"].as_u64() => kept.1 = status,
            Some(_) => {}
            None => latest.push((name, status)),
        }
    }
    let requires = |name: &str| {
        policies
            .iter()
            .any(|p| p.blocking && p.context.eq_ignore_ascii_case(name))
    };
    let jobs: Vec<Job> = latest
        .iter()
        .map(|(name, status)| status_job(name, status, requires(name)))
        .collect();
    let missing = policies
        .iter()
        .filter(|p| p.blocking && !jobs.iter().any(|j| j.name.eq_ignore_ascii_case(&p.context)))
        .map(|p| p.context.clone())
        .collect();
    let pipeline = (!jobs.is_empty()).then(|| Pipeline {
        id: "statuses".into(),
        name: "statuses".into(),
        definition: None,
        trigger: Trigger::External,
        raw_trigger: "status".into(),
        attempt: 1,
        sha: None,
        git_ref: None,
        state: summary(&jobs),
        warning: false,
        required: None,
        started_at: None,
        finished_at: None,
        url: None,
        jobs,
    });
    (pipeline, missing)
}

/// Whether a build in `built` posted `status`: its `targetUrl` names it.
fn from_build(status: &Value, built: &HashSet<String>) -> bool {
    let Some(url) = status["targetUrl"].as_str() else {
        return false;
    };
    url.split_once("buildId=").is_some_and(|(_, rest)| {
        let id: String = rest.chars().take_while(char::is_ascii_digit).collect();
        built.contains(&id)
    })
}

fn status_job(name: &str, status: &Value, required: bool) -> Job {
    let state = match status["state"].as_str() {
        Some("pending") => State::Running,
        Some("succeeded") => State::Pass,
        Some("failed" | "error") => State::Fail,
        Some("notApplicable") => State::Skipped,
        Some("notSet") => State::Queued,
        _ => State::Unknown,
    };
    let text = |v: &Value| v.as_str().map(str::to_owned);
    Job {
        id: status["id"]
            .as_u64()
            .map(|i| i.to_string())
            .unwrap_or_default(),
        name: name.to_owned(),
        stage: None,
        state,
        warning: false,
        required: Some(required),
        started_at: text(&status["creationDate"]),
        finished_at: (!state.is_active())
            .then(|| text(&status["updatedDate"]))
            .flatten(),
        url: text(&status["targetUrl"]),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::azure::{Azure, Context};
    use crate::testing::Server;
    use crate::tools::http::Http;

    fn names() -> Names {
        Names {
            org: "o".into(),
            project: "p".into(),
            repo: "r".into(),
        }
    }

    fn raw_pr(status: &str) -> Value {
        json!({ "pullRequestId": 7, "status": status, "isDraft": true,
            "sourceRefName": "refs/heads/feat", "targetRefName": "refs/heads/main",
            "lastMergeSourceCommit": { "commitId": "abc" },
            "lastMergeCommit": { "commitId": "m1" },
            "repository": { "project": { "id": "pid" } } })
    }

    #[test]
    fn a_pr_maps_its_state_draft_and_auto_complete() {
        for (status, state) in [
            ("active", "open"),
            ("completed", "merged"),
            ("abandoned", "closed"),
        ] {
            assert_eq!(pr_of(&raw_pr(status), &names()).unwrap().state, state);
        }
        let mut raw = raw_pr("active");
        raw["autoCompleteSetBy"] = json!({ "uniqueName": "Me@Corp.com" });
        raw["completionOptions"] = json!({ "mergeStrategy": "noFastForward" });
        let pr = pr_of(&raw, &names()).unwrap();
        assert!(pr.draft);
        assert_eq!((pr.base.as_str(), pr.head_sha.as_str()), ("main", "abc"));
        assert_eq!(pr.url, "https://dev.azure.com/o/p/_git/r/pullrequest/7");
        assert_eq!(
            pr.auto_merge,
            Some((Some("me@corp.com".into()), Some("merge".into())))
        );
        assert_eq!(pr.queue, None, "no merge queue on Azure DevOps");
    }

    #[test]
    fn a_build_reason_maps_to_a_trigger() {
        for (reason, trigger_) in [
            ("individualCI", Trigger::Push),
            ("batchedCI", Trigger::Push),
            ("pullRequest", Trigger::PullRequest),
            ("manual", Trigger::Manual),
            ("schedule", Trigger::Schedule),
            ("resourceTrigger", Trigger::Downstream),
            ("buildCompletion", Trigger::Other),
        ] {
            assert_eq!(trigger(reason), trigger_, "{reason}");
        }
    }

    #[test]
    fn one_pipeline_per_definition_and_trigger() {
        let build = |id: u64, definition: u64, reason: &str| {
            json!({ "id": id, "sourceVersion": "abc", "reason": reason,
                    "definition": { "id": definition } })
        };
        let builds = json!({ "value": [
            build(1, 10, "individualCI"),
            build(2, 10, "batchedCI"),
            build(3, 10, "pullRequest"),
            build(4, 10, "buildCompletion"),
            build(5, 10, "checkInShelveset"),
            build(6, 10, "checkInShelveset"),
            build(7, 11, "individualCI"),
        ] });
        let ids: Vec<_> = latest(&builds, "abc", &HashSet::new())
            .iter()
            .map(|b| b["id"].as_u64().unwrap())
            .collect();
        assert_eq!(
            ids,
            [7, 6, 4, 3, 2],
            "both pushes are one, each other reason its own"
        );
    }

    fn evaluation(id: &str, status: &str, context: Value, blocking: bool) -> Value {
        json!({ "evaluationId": id, "status": status, "context": context,
            "configuration": { "isEnabled": true, "isBlocking": blocking,
                "type": { "id": BUILD_POLICY },
                "settings": { "buildDefinitionId": 12, "displayName": "" } } })
    }

    /// An expired evaluation, or one queued by hand only, waits for a
    /// person; a queued one expects its build.
    #[test]
    fn an_expired_policy_waits_for_a_person() {
        let evaluations = json!({ "value": [
            evaluation("e1", "queued", json!({ "isExpired": true, "buildId": 5,
                "buildDefinitionName": "CI" }), true),
            evaluation("e2", "queued", json!({ "isExpired": false }), false),
            evaluation("e3", "notApplicable", json!({}), true),
            { "configuration": { "isEnabled": true, "type": { "id": "fa4e907d-c16b-4a4c-9dfa-4906e5d171dd" } },
              "status": "rejected" },
        ]});
        let policies = policies(&evaluations);
        let states: Vec<_> = policies.builds.iter().map(|p| p.state).collect();
        assert_eq!(states, [State::Waiting, State::Queued, State::Skipped]);
        assert_eq!(policies.builds[0].name, "CI");
        assert_eq!(policies.builds[1].name, "build 12");
        assert!(policies.statuses.is_empty(), "a reviewer policy is no CI");
    }

    fn status(id: u64, name: &str, state: &str, iteration: Option<u64>) -> Value {
        json!({ "id": id, "state": state, "iterationId": iteration,
            "context": { "genre": "ci", "name": name }, "targetUrl": "https://x" })
    }

    /// Statuses pile up: the latest of each context, on the latest
    /// iteration, forms a job; a build's own status is left out.
    #[test]
    fn the_latest_status_of_each_context_counts() {
        let mut coverage = status(9, "coverage", "succeeded", Some(2));
        coverage["targetUrl"] =
            json!("https://dev.azure.com/o/p/_build/results?buildId=41&view=cc");
        let statuses = json!({ "value": [
            status(1, "scan", "failed", Some(2)),
            status(2, "scan", "succeeded", Some(2)),
            status(3, "lint", "succeeded", Some(1)),
            status(4, "deploy", "pending", None),
            coverage,
        ]});
        let built = HashSet::from(["41".to_owned()]);
        let policies = [
            StatusPolicy {
                context: "ci/scan".into(),
                blocking: true,
            },
            StatusPolicy {
                context: "ci/e2e".into(),
                blocking: true,
            },
            StatusPolicy {
                context: "ci/deploy".into(),
                blocking: false,
            },
        ];
        let (pipeline, missing) = statuses_pipeline(&statuses, Some(2), &built, &policies);
        let pipeline = pipeline.unwrap();
        assert_eq!(pipeline.trigger, Trigger::External);
        let jobs: Vec<_> = pipeline
            .jobs
            .iter()
            .map(|j| (j.name.as_str(), j.state, j.required))
            .collect();
        assert_eq!(
            jobs,
            [
                ("ci/scan", State::Pass, Some(true)),
                ("ci/deploy", State::Running, Some(false)),
            ]
        );
        assert_eq!(missing, ["ci/e2e"]);
    }

    /// A forge where `feat` has the active PR 7, whose blocking build
    /// policy designates build 40: two stages, `build` with the jobs `a`
    /// and `b`, `deploy` waiting for an approval. Build 41 of another
    /// definition tested the merge commit; build 39, an older merge commit.
    /// Build 42 of the branch tested its head; build 43, an older head. A
    /// blocking build policy and a blocking status policy do not apply.
    async fn forge() -> Server {
        Server::start(|request| {
            let line = request.lines().next().unwrap_or_default().to_owned();
            let body = if line.contains("/pullrequests?searchcriteria.status=active") {
                json!({ "value": [raw_pr("active")] })
            } else if line.contains("/policy/evaluations?") {
                let mut skipped = evaluation("e2", "notApplicable", json!({ "buildId": 38 }), true);
                skipped["configuration"]["settings"]["buildDefinitionId"] = json!(15);
                json!({ "value": [
                    evaluation("e1", "running", json!({ "buildId": 40 }), true),
                    skipped,
                    { "evaluationId": "e3", "status": "notApplicable",
                      "configuration": { "isEnabled": true, "isBlocking": true,
                        "type": { "id": STATUS_POLICY },
                        "settings": { "statusGenre": "ci", "statusName": "scan" } } },
                ]})
            } else if line.contains("/build/builds?branchname=refs%2fheads%2ffeat") {
                let build = |id: u64, sha: &str| {
                    json!({ "id": id, "status": "notStarted", "reason": "individualCI",
                        "definition": { "id": 12 }, "sourceVersion": sha,
                        "sourceBranch": "refs/heads/feat" })
                };
                json!({ "value": [build(43, "old"), build(42, "abc")] })
            } else if line.contains("/build/builds?branchname=refs%2fpull%2f7%2fmerge") {
                let build = |id: u64, definition: u64, sha: &str| {
                    json!({ "id": id, "status": "inProgress", "reason": "pullRequest",
                        "definition": { "id": definition, "name": format!("def {definition}") },
                        "sourceVersion": sha, "sourceBranch": "refs/pull/7/merge",
                        "_links": { "web": { "href": format!("w/{id}") } } })
                };
                json!({ "value": [build(41, 13, "m1"), build(40, 12, "m1"), build(39, 14, "m0")] })
            } else if line.contains("/build/builds/40/timeline") {
                let record = |id: &str,
                              kind: &str,
                              parent: Option<&str>,
                              name: &str,
                              state: &str,
                              order: u64| {
                    json!({ "id": id, "parentId": parent, "type": kind, "name": name,
                        "state": state, "result": null, "order": order, "attempt": 1 })
                };
                json!({ "changeId": 9, "records": [
                    record("s1", "Stage", None, "build", "inProgress", 1),
                    record("s2", "Stage", None, "deploy", "pending", 2),
                    record("p2", "Phase", Some("s1"), "b", "inProgress", 2),
                    record("p1", "Phase", Some("s1"), "a", "inProgress", 1),
                    record("p3", "Phase", Some("s2"), "ship", "pending", 1),
                    record("j1", "Job", Some("p1"), "a", "inProgress", 1),
                    record("f", "Job", None, "Finalize build", "pending", 9),
                    record("c1", "Checkpoint", Some("s2"), "Checkpoint", "inProgress", 1),
                    record("c2", "Checkpoint.Approval", Some("c1"), "Approval", "inProgress", 1),
                ]})
            } else if line.contains("/build/builds/41/timeline") {
                json!({ "changeId": 1, "records": [
                    { "id": "q", "type": "Phase", "name": "__default", "state": "completed",
                      "result": "succeeded", "order": 1, "attempt": 2 },
                ]})
            } else if line.contains("/statuses?") {
                json!({ "value": [] })
            } else {
                return (404, vec![], String::new());
            };
            (200, vec![], body.to_string())
        })
        .await
    }

    fn context(server: &Server) -> Context {
        Azure::new(
            Http::new().unwrap(),
            server.base(),
            crate::credentials::testing::none(),
        )
        .context
    }

    async fn read_feat(context: &Context) -> BranchReading {
        let client = Client {
            context,
            token: "t".into(),
        };
        read(&client, &names(), "feat", &mut Memory::default())
            .await
            .unwrap()
    }

    /// The build of a blocking policy has its Phases as jobs, under their
    /// stage, all required; another build on the merge commit, and one of
    /// the branch on its head, are not required; one on an older merge
    /// commit or head is left out. A policy that does not apply is skipped
    /// and requires nothing.
    #[tokio::test]
    async fn a_blocking_policy_requires_every_job_of_its_build() {
        let server = forge().await;
        let reading = read_feat(&context(&server)).await;
        assert_eq!(reading.head_sha.as_deref(), Some("abc"));
        let ci = reading.ci.unwrap();
        assert_eq!(ci.sha, "abc", "the PR's head");
        let ids: Vec<_> = ci.pipelines.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["40", "38", "41", "42"]);
        let skipped = &ci.pipelines[1];
        assert_eq!(
            (skipped.state, skipped.required),
            (State::Skipped, Some(true))
        );
        assert!(skipped.jobs.is_empty(), "no build read");
        let pushed = &ci.pipelines[3];
        assert_eq!(
            (pushed.trigger, pushed.required),
            (Trigger::Push, Some(false))
        );
        assert!(
            ci.missing_required.is_empty(),
            "a status policy that does not apply"
        );
        let policy = &ci.pipelines[0];
        assert_eq!(policy.git_ref.as_deref(), Some("refs/pull/7/merge"));
        assert_eq!(policy.sha.as_deref(), Some("m1"));
        let jobs: Vec<_> = policy
            .jobs
            .iter()
            .map(|j| (j.stage.as_deref(), j.name.as_str(), j.state, j.required))
            .collect();
        assert_eq!(
            jobs,
            [
                (Some("build"), "a", State::Running, Some(true)),
                (Some("build"), "b", State::Running, Some(true)),
                (Some("deploy"), "ship", State::Waiting, Some(true)),
            ]
        );
        let other = &ci.pipelines[2];
        assert_eq!(other.jobs[0].stage, None, "the default stage is none");
        assert_eq!((other.jobs[0].required, other.attempt), (Some(false), 2));
        let requests = server.requests();
        assert!(requests.iter().any(|r| r.contains(
            "artifactid=vstfs%3a%2f%2f%2fcodereview%2fcodereviewid%2fpid%2f7&includenotapplicable=true&api-version=7.1-preview.1"
        )));
        assert!(
            !requests.iter().any(|r| r.contains("/iterations")),
            "no status"
        );
        assert!(!requests.iter().any(|r| r.contains("/builds/38")));
    }

    /// A queued evaluation that still names a completed build waits for
    /// the next one: no job, a CI expected.
    #[tokio::test]
    async fn a_queued_policy_naming_a_completed_build_expects_the_next() {
        let server = Server::start(|request| {
            let line = request.lines().next().unwrap_or_default().to_owned();
            let body = if line.contains("/pullrequests?searchcriteria.status=active") {
                json!({ "value": [raw_pr("active")] })
            } else if line.contains("/policy/evaluations?") {
                json!({ "value": [evaluation("e1", "queued", json!({ "buildId": 40 }), true)] })
            } else if line.contains("/build/builds/40?") {
                json!({ "id": 40, "status": "completed", "result": "succeeded",
                    "definition": { "id": 12 }, "sourceVersion": "m0" })
            } else if line.contains("/statuses?") || line.contains("/build/builds?") {
                json!({ "value": [] })
            } else {
                return (404, vec![], String::new());
            };
            (200, vec![], body.to_string())
        })
        .await;
        let ci = read_feat(&context(&server)).await.ci.unwrap();
        let policy = &ci.pipelines[0];
        assert_eq!(policy.state, State::Queued);
        assert!(policy.jobs.is_empty());
        assert!(!server.requests().iter().any(|r| r.contains("/timeline")));
    }

    /// A completed build's timeline is read again only when its status
    /// changes; a running one is read from its last change.
    #[tokio::test]
    async fn a_timeline_is_read_from_its_last_change() {
        let server = forge().await;
        let context = context(&server);
        read_feat(&context).await;
        read_feat(&context).await;
        let timelines: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|r| r.contains("/timeline"))
            .map(|r| r.lines().next().unwrap().to_owned())
            .collect();
        assert_eq!(timelines.len(), 4);
        assert!(timelines[2].contains("/40/timeline?api-version=7.1&changeid=9"));
        assert!(timelines[3].contains("/41/timeline?api-version=7.1&changeid=1"));
    }

    /// A branch without a PR: its head, then the builds that tested it.
    #[tokio::test]
    async fn a_branch_without_pr_reads_the_builds_of_its_head() {
        let server = Server::start(|request| {
            let line = request.lines().next().unwrap_or_default().to_owned();
            let body = if line.contains("/pullrequests?") {
                json!({ "value": [] })
            } else if line.contains("/refs?filter=heads%2ffeat") {
                json!({ "value": [{ "name": "refs/heads/feat-2", "objectId": "x" },
                    { "name": "refs/heads/feat", "objectId": "abc" }] })
            } else if line.contains("/build/builds?branchname=refs%2fheads%2ffeat") {
                json!({ "value": [
                    { "id": 3, "status": "notStarted", "reason": "individualCI",
                      "definition": { "id": 1 }, "sourceVersion": "abc" },
                    { "id": 2, "status": "completed", "result": "failed", "reason": "individualCI",
                      "definition": { "id": 1 }, "sourceVersion": "abc" },
                ]})
            } else {
                return (404, vec![], String::new());
            };
            (200, vec![], body.to_string())
        })
        .await;
        let reading = read_feat(&context(&server)).await;
        assert_eq!(reading.pr, None);
        let ci = reading.ci.unwrap();
        assert_eq!(ci.sha, "abc");
        assert_eq!(ci.pipelines.len(), 1, "the latest of a definition");
        let build = &ci.pipelines[0];
        assert_eq!((build.state, build.trigger), (State::Queued, Trigger::Push));
        assert_eq!(build.required, None);
        assert!(!server.requests().iter().any(|r| r.contains("/timeline")));
    }

    /// An expired policy publishes its build as a pipeline without job,
    /// waiting, which carries the requirement.
    #[tokio::test]
    async fn an_expired_policy_is_a_waiting_pipeline_without_job() {
        let server = Server::start(|request| {
            let line = request.lines().next().unwrap_or_default().to_owned();
            let body = if line.contains("/pullrequests?searchcriteria.status=active") {
                json!({ "value": [raw_pr("active")] })
            } else if line.contains("/policy/evaluations?") {
                json!({ "value": [evaluation("e1", "queued",
                    json!({ "buildId": 40, "isExpired": true }), true)] })
            } else if line.contains("/statuses?") || line.contains("/build/builds?") {
                json!({ "value": [] })
            } else {
                return (404, vec![], String::new());
            };
            (200, vec![], body.to_string())
        })
        .await;
        let ci = read_feat(&context(&server)).await.ci.unwrap();
        let policy = &ci.pipelines[0];
        assert_eq!((policy.id.as_str(), policy.state), ("40", State::Waiting));
        assert!(policy.jobs.is_empty());
        assert_eq!(policy.required, Some(true));
        assert_eq!(
            ci.synthesis(tokio::time::Instant::now())["required_state"],
            "waiting"
        );
    }

    /// A PR off the active listing is the branch's latest.
    #[tokio::test]
    async fn a_pr_off_the_active_listing_is_the_branch_s_latest() {
        let server = Server::start(|request| {
            let line = request.lines().next().unwrap_or_default().to_owned();
            let body = if line.contains("searchcriteria.status=active") {
                json!({ "value": [] })
            } else if line.contains("sourcerefname=refs%2fheads%2ffeat&searchcriteria.status=all") {
                let mut old = raw_pr("abandoned");
                old["pullRequestId"] = json!(3);
                json!({ "value": [old, raw_pr("completed")] })
            } else if line.contains("/refs?") {
                json!({ "value": [] })
            } else {
                return (404, vec![], String::new());
            };
            (200, vec![], body.to_string())
        })
        .await;
        let reading = read_feat(&context(&server)).await;
        let pr = reading.pr.unwrap();
        assert_eq!((pr.number, pr.state), (7, "merged"));
        assert_eq!((reading.head_sha, reading.ci), (None, None));
    }
}
