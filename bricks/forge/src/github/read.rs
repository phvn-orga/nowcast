//! GitHub through its REST API: a branch's PR and the CI of a commit, mapped
//! onto the neutral model. Every request is conditional (`tools::http`) and
//! keeps its parameters from one reading to the next, so that its `ETag`
//! keeps serving; the open PRs are one list per repository, shared by every
//! branch.

use serde_json::Value;

use crate::model::{Ci, Job, Pipeline, Pr, State, Trigger};
use crate::provider::BranchReading;
use crate::tools::http::{Failure, Http};

pub(super) const HEADERS: &[(&str, &str)] = &[
    ("accept", "application/vnd.github+json"),
    ("x-github-api-version", "2022-11-28"),
];

/// The open PRs one page lists: a full page may leave the branch's out.
const PAGE: usize = 100;

/// Reads the PR of `branch` in `owner/name` and the CI of its head.
pub async fn read(
    http: &Http,
    api: &str,
    token: &str,
    owner: &str,
    name: &str,
    branch: &str,
) -> Result<BranchReading, Failure> {
    let repo = format!("{api}/repos/{owner}/{name}");
    let get = |url: String| async move { http.get(&url, token, HEADERS).await };

    let head = encode(&format!("{owner}:{branch}"));
    let open = get(format!("{repo}/pulls?state=open&per_page={PAGE}")).await?;
    let mut pr = open_pr(&open, owner, name, branch);
    if pr.is_none() && open.as_array().is_some_and(|a| a.len() >= PAGE) {
        let own = get(format!(
            "{repo}/pulls?head={head}&state=open&per_page={PAGE}"
        ))
        .await?;
        pr = open_pr(&own, owner, name, branch);
    }
    if pr.is_none() {
        let closed = get(format!("{repo}/pulls?head={head}&state=closed&per_page=1")).await?;
        pr = closed.as_array().and_then(|a| a.first()).and_then(pr_of);
    }
    let open_one = pr.as_ref().filter(|p| p.is_open());
    let head_sha = match open_one {
        Some(pr) => Some(pr.head_sha.clone()),
        None => match get(format!("{repo}/branches/{}", encode(branch))).await {
            Ok(b) => b["commit"]["sha"].as_str().map(str::to_owned),
            Err(Failure::NotFound) => None,
            Err(e) => return Err(e),
        },
    };
    let ci = match &head_sha {
        Some(sha) => Some(
            ci(
                http,
                api,
                token,
                owner,
                name,
                sha,
                open_one.map(|p| p.number),
            )
            .await?,
        ),
        None => None,
    };
    Ok(BranchReading { head_sha, pr, ci })
}

/// Reads the CI of commit `sha` in `owner/name`, which `pr`, when open,
/// is about.
pub async fn ci(
    http: &Http,
    api: &str,
    token: &str,
    owner: &str,
    name: &str,
    sha: &str,
    pr: Option<u64>,
) -> Result<Ci, Failure> {
    let repo = format!("{api}/repos/{owner}/{name}");
    let get = |url: String| async move { http.get(&url, token, HEADERS).await };
    let (runs, checks, statuses) = tokio::try_join!(
        get(format!("{repo}/actions/runs?head_sha={sha}&per_page=100")),
        get(format!("{repo}/commits/{sha}/check-runs?per_page=100")),
        get(format!("{repo}/commits/{sha}/status")),
    )?;
    Ok(Ci {
        sha: sha.to_owned(),
        pipelines: pipelines(&runs, &checks, &statuses, pr),
        missing_required: Vec::new(),
        expected_until: None,
    })
}

/// Percent-encodes what a path segment or a query value cannot hold as is.
pub(super) fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The open PR whose head is `branch` of this very repository: a branch of
/// the same name in a fork is another branch.
fn open_pr(pulls: &Value, owner: &str, name: &str, branch: &str) -> Option<Pr> {
    let full = format!("{owner}/{name}");
    pulls.as_array()?.iter().find_map(|p| {
        let same_repo = p["head"]["repo"]["full_name"]
            .as_str()
            .is_some_and(|f| f.eq_ignore_ascii_case(&full));
        (same_repo && p["head"]["ref"] == branch)
            .then(|| pr_of(p))
            .flatten()
    })
}

fn pr_of(p: &Value) -> Option<Pr> {
    let state = match (p["state"].as_str()?, p["merged_at"].is_string()) {
        ("open", _) => "open",
        (_, true) => "merged",
        _ => "closed",
    };
    let auto_merge = p["auto_merge"].as_object().map(|a| {
        (
            a.get("enabled_by")
                .and_then(|u| u["login"].as_str())
                .map(str::to_owned),
            a.get("merge_method")
                .and_then(Value::as_str)
                .map(str::to_owned),
        )
    });
    Some(Pr {
        number: p["number"].as_u64()?,
        state,
        draft: p["draft"].as_bool().unwrap_or(false),
        base: p["base"]["ref"].as_str().unwrap_or_default().to_owned(),
        head_sha: p["head"]["sha"].as_str()?.to_owned(),
        url: p["html_url"].as_str().unwrap_or_default().to_owned(),
        auto_merge,
        queue: None,
        dequeued: None,
    })
}

/// The pipelines of a commit: its workflow runs, each with its check runs;
/// check runs of apps other than GitHub Actions, one pipeline per app; and
/// the commit statuses, as one `external` pipeline. Of the runs of one
/// workflow for one event, only the latest counts.
fn pipelines(runs: &Value, checks: &Value, statuses: &Value, pr: Option<u64>) -> Vec<Pipeline> {
    let checks: Vec<&Value> = checks["check_runs"]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let mut out: Vec<Pipeline> = Vec::new();
    let mut kept = std::collections::HashSet::new();
    let mut suites_of_runs = std::collections::HashSet::new();
    let mut runs: Vec<&Value> = runs["workflow_runs"]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    // Latest first, whatever order the API used.
    runs.sort_by(|a, b| b["created_at"].as_str().cmp(&a["created_at"].as_str()));
    for run in runs {
        let suite = run["check_suite_id"].as_u64();
        suites_of_runs.extend(suite);
        let key = (
            run["path"].as_str().unwrap_or_default(),
            run["event"].as_str().unwrap_or_default(),
        );
        if !kept.insert(key) {
            continue;
        }
        let event = run["event"].as_str().unwrap_or_default();
        let branch = run["head_branch"].as_str();
        let git_ref = match (trigger(event), pr) {
            (Trigger::PullRequest, Some(n)) => Some(format!("refs/pull/{n}/merge")),
            _ => branch.map(|b| format!("refs/heads/{b}")),
        };
        let (state, warning) = run_state(run["status"].as_str(), run["conclusion"].as_str());
        out.push(Pipeline {
            id: run["id"]
                .as_u64()
                .map(|i| i.to_string())
                .unwrap_or_default(),
            name: run["name"].as_str().unwrap_or_default().to_owned(),
            definition: run["path"].as_str().map(str::to_owned),
            trigger: trigger(event),
            raw_trigger: event.to_owned(),
            attempt: run["run_attempt"].as_u64().unwrap_or(1),
            sha: run["head_sha"].as_str().map(str::to_owned),
            git_ref,
            state,
            warning,
            required: None,
            started_at: run["run_started_at"].as_str().map(str::to_owned),
            finished_at: (!state.is_active())
                .then(|| run["updated_at"].as_str().map(str::to_owned))
                .flatten(),
            url: run["html_url"].as_str().map(str::to_owned),
            jobs: checks
                .iter()
                .filter(|c| suite.is_some() && c["check_suite"]["id"].as_u64() == suite)
                .map(|c| job(c))
                .collect(),
        });
    }
    // Check runs no workflow run holds: another app's, one pipeline per app.
    // Those of an older run of GitHub Actions are left out with their run.
    let mut apps: Vec<Pipeline> = Vec::new();
    for c in &checks {
        let suite = c["check_suite"]["id"].as_u64();
        if suite.is_some_and(|s| suites_of_runs.contains(&s))
            || c["app"]["slug"] == "github-actions"
        {
            continue;
        }
        let app = c["app"]["name"].as_str().unwrap_or("checks").to_owned();
        let id = suite.map(|s| s.to_string()).unwrap_or_else(|| app.clone());
        match apps.iter_mut().find(|p| p.id == id) {
            Some(p) => p.jobs.push(job(c)),
            None => apps.push(Pipeline {
                id,
                name: app.clone(),
                definition: None,
                trigger: Trigger::External,
                raw_trigger: c["app"]["slug"].as_str().unwrap_or_default().to_owned(),
                attempt: 1,
                sha: c["head_sha"].as_str().map(str::to_owned),
                git_ref: None,
                state: State::Unknown,
                warning: false,
                required: None,
                started_at: None,
                finished_at: None,
                url: None,
                jobs: vec![job(c)],
            }),
        }
    }
    for p in &mut apps {
        p.state = summary(&p.jobs);
    }
    out.extend(apps);
    // A commit without any status answers `pending` with no status: that is
    // the absence of statuses, not one pending.
    let statuses: Vec<Job> = statuses["statuses"]
        .as_array()
        .map(|a| a.iter().map(status_job).collect())
        .unwrap_or_default();
    if !statuses.is_empty() {
        out.push(Pipeline {
            id: "statuses".into(),
            name: "statuses".into(),
            definition: None,
            trigger: Trigger::External,
            raw_trigger: "status".into(),
            attempt: 1,
            sha: None,
            git_ref: None,
            state: summary(&statuses),
            warning: false,
            required: None,
            started_at: None,
            finished_at: None,
            url: None,
            jobs: statuses,
        });
    }
    out
}

/// The state of a group of jobs that has none of its own.
fn summary(jobs: &[Job]) -> State {
    let has = |s: State| jobs.iter().any(|j| j.state == s);
    if has(State::Fail) {
        State::Fail
    } else if has(State::Cancelled) {
        State::Cancelled
    } else if has(State::Running) {
        State::Running
    } else if has(State::Queued) {
        State::Queued
    } else if has(State::Waiting) {
        State::Waiting
    } else if has(State::Pass) {
        State::Pass
    } else if !jobs.is_empty() && jobs.iter().all(|j| j.state == State::Skipped) {
        State::Skipped
    } else {
        State::Unknown
    }
}

fn job(c: &Value) -> Job {
    let (state, warning) = run_state(c["status"].as_str(), c["conclusion"].as_str());
    Job {
        id: c["id"].as_u64().map(|i| i.to_string()).unwrap_or_default(),
        name: c["name"].as_str().unwrap_or_default().to_owned(),
        stage: None,
        state,
        warning,
        required: None,
        started_at: c["started_at"].as_str().map(str::to_owned),
        finished_at: c["completed_at"].as_str().map(str::to_owned),
        url: c["html_url"].as_str().map(str::to_owned),
    }
}

fn status_job(s: &Value) -> Job {
    let state = match s["state"].as_str() {
        Some("success") => State::Pass,
        Some("failure" | "error") => State::Fail,
        Some("pending") => State::Running,
        _ => State::Unknown,
    };
    Job {
        id: s["id"].as_u64().map(|i| i.to_string()).unwrap_or_default(),
        name: s["context"].as_str().unwrap_or_default().to_owned(),
        stage: None,
        state,
        warning: false,
        required: None,
        started_at: s["created_at"].as_str().map(str::to_owned),
        finished_at: (!state.is_active())
            .then(|| s["updated_at"].as_str().map(str::to_owned))
            .flatten(),
        url: s["target_url"].as_str().map(str::to_owned),
    }
}

/// A workflow run or a check run: `status` while it is not over, then its
/// `conclusion`.
fn run_state(status: Option<&str>, conclusion: Option<&str>) -> (State, bool) {
    let state = match status {
        Some("queued" | "requested" | "pending") => State::Queued,
        Some("waiting") => State::Waiting,
        Some("in_progress") => State::Running,
        Some("completed") => match conclusion {
            Some("success") => State::Pass,
            Some("failure" | "timed_out" | "startup_failure") => State::Fail,
            Some("cancelled") => State::Cancelled,
            Some("skipped" | "neutral") => State::Skipped,
            Some("action_required") => State::Waiting,
            _ => State::Unknown,
        },
        _ => State::Unknown,
    };
    (state, false)
}

fn trigger(event: &str) -> Trigger {
    match event {
        "push" => Trigger::Push,
        "pull_request" | "pull_request_target" => Trigger::PullRequest,
        "merge_group" => Trigger::MergeQueue,
        "workflow_dispatch" => Trigger::Manual,
        "schedule" => Trigger::Schedule,
        "workflow_run" | "workflow_call" => Trigger::Downstream,
        _ => Trigger::Other,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::testing::Server;

    fn run(
        id: u64,
        path: &str,
        event: &str,
        created: &str,
        status: &str,
        conclusion: Option<&str>,
    ) -> Value {
        json!({
            "id": id, "name": path, "path": path, "event": event, "run_attempt": 1,
            "head_sha": "abc", "head_branch": "feat", "check_suite_id": id * 10,
            "status": status, "conclusion": conclusion, "created_at": created,
            "run_started_at": created, "updated_at": created, "html_url": format!("https://github.com/o/r/actions/runs/{id}"),
        })
    }

    fn check(
        id: u64,
        suite: u64,
        name: &str,
        app: &str,
        status: &str,
        conclusion: Option<&str>,
    ) -> Value {
        json!({
            "id": id, "name": name, "head_sha": "abc", "status": status, "conclusion": conclusion,
            "started_at": "2026-11-09T06:00:00Z", "completed_at": null, "html_url": "u",
            "check_suite": { "id": suite }, "app": { "slug": app, "name": app },
        })
    }

    #[test]
    fn runs_become_pipelines_with_their_jobs_latest_per_workflow_and_event() {
        let runs = json!({ "workflow_runs": [
            run(1, "ci.yml", "pull_request", "2026-11-09T06:00:00Z", "completed", Some("failure")),
            run(2, "ci.yml", "pull_request", "2026-11-09T07:00:00Z", "in_progress", None),
            run(3, "ci.yml", "push", "2026-11-09T06:30:00Z", "completed", Some("success")),
            run(4, "release.yml", "pull_request", "2026-11-09T06:30:00Z", "completed", Some("action_required")),
        ]});
        let checks = json!({ "check_runs": [
            check(11, 10, "old", "github-actions", "completed", Some("failure")),
            check(21, 20, "check", "github-actions", "in_progress", None),
            check(22, 20, "lint", "github-actions", "completed", Some("skipped")),
            check(31, 30, "check", "github-actions", "completed", Some("success")),
            check(91, 90, "scan", "sonarcloud", "completed", Some("success")),
        ]});
        let statuses = json!({ "state": "pending", "total_count": 0, "statuses": [] });
        let p = pipelines(&runs, &checks, &statuses, Some(7));
        let ids: Vec<&str> = p.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            ["2", "3", "4", "90"],
            "run 1 is an older run of ci.yml on pull_request"
        );
        assert_eq!(p[0].state, State::Running);
        assert_eq!(p[0].git_ref.as_deref(), Some("refs/pull/7/merge"));
        assert_eq!(
            p[0].jobs
                .iter()
                .map(|j| (j.name.as_str(), j.state))
                .collect::<Vec<_>>(),
            [("check", State::Running), ("lint", State::Skipped)]
        );
        assert_eq!(p[1].git_ref.as_deref(), Some("refs/heads/feat"));
        assert_eq!(
            p[2].state,
            State::Waiting,
            "a run waiting for approval has no job"
        );
        assert!(p[2].jobs.is_empty());
        assert_eq!(
            (p[3].name.as_str(), p[3].trigger, p[3].state),
            ("sonarcloud", Trigger::External, State::Pass)
        );
    }

    #[test]
    fn commit_statuses_form_one_external_pipeline() {
        let statuses = json!({ "state": "failure", "total_count": 2, "statuses": [
            { "id": 1, "context": "ci/jenkins", "state": "failure", "target_url": "j" },
            { "id": 2, "context": "deploy", "state": "pending" },
        ]});
        let p = pipelines(&json!({}), &json!({}), &statuses, None);
        assert_eq!(p.len(), 1);
        assert_eq!((p[0].trigger, p[0].state), (Trigger::External, State::Fail));
        assert_eq!(p[0].jobs[1].state, State::Running);
    }

    #[test]
    fn the_open_pr_is_the_branch_of_this_repository() {
        let pulls = json!([
            { "number": 1, "state": "open", "draft": false, "html_url": "u1", "merged_at": null,
              "head": { "ref": "feat", "sha": "f", "repo": { "full_name": "fork/r" } }, "base": { "ref": "main" } },
            { "number": 2, "state": "open", "draft": true, "html_url": "u2", "merged_at": null,
              "auto_merge": { "enabled_by": { "login": "me" }, "merge_method": "squash" },
              "head": { "ref": "feat", "sha": "s", "repo": { "full_name": "O/R" } }, "base": { "ref": "main" } },
        ]);
        let pr = open_pr(&pulls, "o", "r", "feat").unwrap();
        assert_eq!((pr.number, pr.draft, pr.head_sha.as_str()), (2, true, "s"));
        assert_eq!(
            pr.auto_merge,
            Some((Some("me".into()), Some("squash".into())))
        );
        assert_eq!(open_pr(&pulls, "o", "r", "other"), None);
        let merged =
            json!({ "number": 3, "state": "closed", "merged_at": "t", "head": { "sha": "x" } });
        assert_eq!(pr_of(&merged).unwrap().state, "merged");
    }

    /// A branch without a PR: its head is read, then the CI of that head.
    #[tokio::test]
    async fn a_branch_without_pr_reads_the_ci_of_its_head() {
        let server = Server::start(|request| {
            let line = request.lines().next().unwrap_or_default().to_owned();
            let body = if line.contains("/pulls?") {
                "[]".to_owned()
            } else if line.contains("/branches/feat%2fx") {
                json!({ "commit": { "sha": "abc" } }).to_string()
            } else if line.contains("/actions/runs?head_sha=abc") {
                json!({ "workflow_runs": [run(1, "ci.yml", "push", "t", "queued", None)] })
                    .to_string()
            } else if line.contains("/commits/abc/check-runs") {
                json!({ "check_runs": [] }).to_string()
            } else if line.contains("/commits/abc/status") {
                json!({ "statuses": [] }).to_string()
            } else {
                return (404, vec![], String::new());
            };
            (200, vec![], body)
        })
        .await;
        let http = Http::new().unwrap();
        let reading = read(&http, &server.base(), "t", "o", "r", "feat/x")
            .await
            .unwrap();
        assert_eq!(reading.head_sha.as_deref(), Some("abc"));
        assert_eq!(reading.pr, None);
        assert_eq!(reading.ci.unwrap().pipelines[0].state, State::Queued);
        assert!(server.requests()[0].contains("x-github-api-version: 2022-11-28"));
    }

    #[tokio::test]
    async fn a_branch_not_pushed_has_no_head_and_no_ci() {
        let server = Server::start(|request| {
            if request
                .lines()
                .next()
                .unwrap_or_default()
                .contains("/branches/")
            {
                (404, vec![], String::new())
            } else {
                (200, vec![], "[]".into())
            }
        })
        .await;
        let http = Http::new().unwrap();
        let reading = read(&http, &server.base(), "t", "o", "r", "feat")
            .await
            .unwrap();
        assert_eq!(reading, BranchReading::default());
    }

    /// The open PRs are one list shared by every branch; when a full page
    /// leaves the branch's PR out, the branch's own open PRs are asked.
    #[tokio::test]
    async fn a_full_page_of_open_prs_asks_for_the_branch_s_own() {
        let server = Server::start(|request| {
            let line = request.lines().next().unwrap_or_default().to_owned();
            let pr = |n: u64, branch: &str| {
                json!({ "number": n, "state": "open", "draft": false, "html_url": "u",
                    "merged_at": null, "base": { "ref": "main" },
                    "head": { "ref": branch, "sha": "abc", "repo": { "full_name": "o/r" } } })
            };
            let body = if line.contains("/pulls?state=open&per_page=100") {
                json!((0..100).map(|n| pr(n, "other")).collect::<Vec<_>>()).to_string()
            } else if line.contains("/pulls?head=o%3afeat&state=open") {
                json!([pr(500, "feat")]).to_string()
            } else if line.contains("/check-runs") {
                json!({ "check_runs": [] }).to_string()
            } else if line.contains("/actions/runs") {
                json!({ "workflow_runs": [] }).to_string()
            } else if line.contains("/status") {
                json!({ "statuses": [] }).to_string()
            } else {
                return (404, vec![], String::new());
            };
            (200, vec![], body)
        })
        .await;
        let http = Http::new().unwrap();
        let reading = read(&http, &server.base(), "t", "o", "r", "feat")
            .await
            .unwrap();
        assert_eq!(reading.pr.unwrap().number, 500);
        assert_eq!(reading.ci.unwrap().sha, "abc");
    }
}
