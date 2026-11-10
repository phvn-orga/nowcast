//! The neutral model every provider maps its forge onto: a PR, the pipelines
//! of a commit with their jobs, and their rollup. It is the output contract:
//! `forge.pr/v1` and `forge.pipelines/v1` are built from it, whatever the
//! forge.

use serde_json::{Value, json};
use tokio::time::Instant;

/// The state of a pipeline or a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Queued,
    /// A person must act: an approval, a manual job.
    Waiting,
    Running,
    Pass,
    Fail,
    Cancelled,
    Skipped,
    Unknown,
}

impl State {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Waiting => "waiting",
            Self::Running => "running",
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
            Self::Unknown => "unknown",
        }
    }

    /// Not over yet.
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Waiting | Self::Running)
    }
}

/// What started a pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Push,
    PullRequest,
    MergeQueue,
    Manual,
    Schedule,
    Downstream,
    External,
    Other,
}

impl Trigger {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::PullRequest => "pull_request",
            Self::MergeQueue => "merge_queue",
            Self::Manual => "manual",
            Self::Schedule => "schedule",
            Self::Downstream => "downstream",
            Self::External => "external",
            Self::Other => "other",
        }
    }
}

/// A pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pr {
    pub number: u64,
    /// `open`, `merged` or `closed`.
    pub state: &'static str,
    pub draft: bool,
    pub base: String,
    pub head_sha: String,
    pub url: String,
    /// Who asked for the PR to merge by itself, and the merge method.
    pub auto_merge: Option<(Option<String>, Option<String>)>,
}

impl Pr {
    pub fn is_open(&self) -> bool {
        self.state == "open"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub stage: Option<String>,
    pub state: State,
    pub warning: bool,
    /// Whether the job must pass for the PR to merge; `None` when the forge
    /// does not say.
    pub required: Option<bool>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipeline {
    pub id: String,
    pub name: String,
    pub definition: Option<String>,
    pub trigger: Trigger,
    pub raw_trigger: String,
    pub attempt: u64,
    /// The commit the pipeline tested.
    pub sha: Option<String>,
    pub git_ref: Option<String>,
    pub state: State,
    pub warning: bool,
    /// Carries the requirement only while the pipeline has no job to carry
    /// it: a required pipeline waiting for its build.
    pub required: Option<bool>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub url: Option<String>,
    pub jobs: Vec<Job>,
}

impl Pipeline {
    /// The pipeline's own requirement, which counts only without jobs.
    fn own_required(&self) -> Option<bool> {
        if self.jobs.is_empty() {
            self.required
        } else {
            None
        }
    }
}

/// The CI of one commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ci {
    /// The commit read.
    pub sha: String,
    pub pipelines: Vec<Pipeline>,
    /// Required checks that nothing reported on the commit.
    pub missing_required: Vec<String>,
    /// Until when the provider expects pipelines that have not shown up
    /// yet: before it, the rollup cannot be `none`, `pass` or `missing`. A
    /// deadline rather than a flag, so the expectation expires even when the
    /// readings after it fail.
    pub expected_until: Option<Instant>,
}

impl Ci {
    /// Whether a pipeline or a job is queued or running. One that waits for
    /// a person may wait for days: it does not count.
    pub fn is_running(&self) -> bool {
        self.elements()
            .any(|(state, _)| matches!(state, State::Queued | State::Running))
    }

    /// Every pipeline and every job, with whether it is required.
    fn elements(&self) -> impl Iterator<Item = (State, Option<bool>)> + '_ {
        self.pipelines.iter().flat_map(|p| {
            std::iter::once((p.state, p.own_required()))
                .chain(p.jobs.iter().map(|j| (j.state, j.required)))
        })
    }

    /// Whether pipelines are still expected at `now`.
    fn expected(&self, now: Instant) -> bool {
        self.expected_until.is_some_and(|until| now < until)
    }

    fn state(&self, now: Instant) -> &'static str {
        rollup(self.elements().map(|(s, _)| s), false, self.expected(now))
    }

    /// The rollup of the required items; `None` when nothing says what is.
    /// A required check still missing while the CI is expected or runs is
    /// `running`: it may come yet, from a job that `needs` a running one.
    fn required_state(&self, now: Instant) -> Option<&'static str> {
        let missing = !self.missing_required.is_empty();
        let any = self.elements().any(|(_, r)| r.is_some()) || missing;
        any.then(|| {
            rollup(
                self.elements()
                    .filter(|(_, r)| *r == Some(true))
                    .map(|(s, _)| s),
                missing,
                self.expected(now) || (missing && self.is_running()),
            )
        })
    }

    /// The rollup `forge.pr/v1` carries, as of `now`.
    pub fn synthesis(&self, now: Instant) -> Value {
        json!({
            "state": self.state(now),
            "required_state": self.required_state(now),
            "sha": self.sha,
        })
    }

    /// The detail `forge.pipelines/v1` carries as of `now`, without what
    /// names the branch and the source.
    pub fn detail(&self, now: Instant) -> Value {
        let counted = || {
            self.pipelines
                .iter()
                .flat_map(|p| &p.jobs)
                .filter(|j| j.state != State::Skipped)
        };
        json!({
            "state": self.state(now),
            "required_state": self.required_state(now),
            "missing_required": self.missing_required,
            "jobs": {
                "done": counted().filter(|j| !j.state.is_active()).count(),
                "total": counted().count(),
            },
            "pipelines": self.pipelines.iter().map(pipeline_json).collect::<Vec<_>>(),
        })
    }
}

/// Sums up states, in this order: `fail` if one failed or was cancelled;
/// `running` if one runs or waits its turn, or a CI is expected; `missing`
/// when a required one never reported (`missing` is only asked for the
/// required rollup); `waiting` if one waits for a person; `pass` if one
/// passed; `none` otherwise. Skipped and unknown states count for nothing.
fn rollup(states: impl Iterator<Item = State>, missing: bool, expected: bool) -> &'static str {
    let (mut fail, mut running, mut waiting, mut pass) = (false, expected, false, false);
    for state in states {
        match state {
            State::Fail | State::Cancelled => fail = true,
            State::Running | State::Queued => running = true,
            State::Waiting => waiting = true,
            State::Pass => pass = true,
            State::Skipped | State::Unknown => {}
        }
    }
    if fail {
        "fail"
    } else if running {
        "running"
    } else if missing {
        "missing"
    } else if waiting {
        "waiting"
    } else if pass {
        "pass"
    } else {
        "none"
    }
}

fn pipeline_json(p: &Pipeline) -> Value {
    json!({
        "id": p.id,
        "name": p.name,
        "definition": p.definition,
        "trigger": p.trigger.as_str(),
        "raw_trigger": p.raw_trigger,
        "attempt": p.attempt,
        "sha": p.sha,
        "ref": p.git_ref,
        "state": p.state.as_str(),
        "warning": p.warning,
        "required": p.own_required(),
        "started_at": p.started_at,
        "finished_at": p.finished_at,
        "url": p.url,
        "jobs": p.jobs.iter().map(|j| json!({
            "id": j.id,
            "name": j.name,
            "stage": j.stage,
            "state": j.state.as_str(),
            "warning": j.warning,
            "required": j.required,
            "started_at": j.started_at,
            "finished_at": j.finished_at,
            "url": j.url,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Duration;

    use super::*;

    pub fn job(name: &str, state: State, required: Option<bool>) -> Job {
        Job {
            id: name.into(),
            name: name.into(),
            stage: None,
            state,
            warning: false,
            required,
            started_at: None,
            finished_at: None,
            url: None,
        }
    }

    pub fn pipeline(state: State, jobs: Vec<Job>) -> Pipeline {
        Pipeline {
            id: "1".into(),
            name: "CI".into(),
            definition: None,
            trigger: Trigger::Push,
            raw_trigger: "push".into(),
            attempt: 1,
            sha: None,
            git_ref: None,
            state,
            warning: false,
            required: None,
            started_at: None,
            finished_at: None,
            url: None,
            jobs,
        }
    }

    pub fn ci(pipelines: Vec<Pipeline>) -> Ci {
        Ci {
            sha: "a".into(),
            pipelines,
            ..Ci::default()
        }
    }

    #[test]
    fn a_ci_waiting_for_a_person_is_not_running() {
        let now = Instant::now();
        let waiting = ci(vec![pipeline(
            State::Waiting,
            vec![job("deploy", State::Waiting, None)],
        )]);
        assert!(!waiting.is_running());
        assert_eq!(waiting.synthesis(now)["state"], "waiting");
        let queued = ci(vec![pipeline(
            State::Running,
            vec![job("a", State::Queued, None)],
        )]);
        assert!(queued.is_running());
    }

    #[test]
    fn a_failed_job_fails_a_running_pipeline() {
        let now = Instant::now();
        let ci = ci(vec![pipeline(
            State::Running,
            vec![job("a", State::Fail, None), job("b", State::Running, None)],
        )]);
        let detail = ci.detail(now);
        assert_eq!(detail["state"], "fail");
        assert_eq!(
            detail["required_state"],
            Value::Null,
            "nothing says what is required"
        );
        assert_eq!(detail["jobs"], json!({ "done": 1, "total": 2 }));
        assert_eq!(
            ci.synthesis(now),
            json!({ "state": "fail", "required_state": null, "sha": "a" })
        );
    }

    #[test]
    fn nothing_is_none_unless_a_ci_is_expected() {
        let now = Instant::now();
        let mut nothing = ci(vec![]);
        assert_eq!(nothing.synthesis(now)["state"], "none");
        nothing.expected_until = Some(now + Duration::from_secs(1));
        assert_eq!(nothing.synthesis(now)["state"], "running");
        assert_eq!(
            nothing.synthesis(now + Duration::from_secs(1))["state"],
            "none",
            "the expectation expired"
        );
    }

    #[test]
    fn skipped_jobs_count_for_nothing() {
        let now = Instant::now();
        let ci = ci(vec![pipeline(
            State::Pass,
            vec![job("a", State::Pass, None), job("b", State::Skipped, None)],
        )]);
        let detail = ci.detail(now);
        assert_eq!(detail["state"], "pass");
        assert_eq!(detail["jobs"], json!({ "done": 1, "total": 1 }));
    }

    #[test]
    fn the_required_rollup_sees_only_required_jobs_and_what_is_missing() {
        let now = Instant::now();
        let mut ci = ci(vec![pipeline(
            State::Fail,
            vec![
                job("lint", State::Fail, Some(false)),
                job("check", State::Pass, Some(true)),
            ],
        )]);
        ci.missing_required = vec!["e2e".into()];
        assert_eq!(ci.detail(now)["state"], "fail");
        assert_eq!(ci.detail(now)["required_state"], "missing");
        ci.expected_until = Some(now + Duration::from_secs(1));
        assert_eq!(
            ci.detail(now)["required_state"],
            "running",
            "it may still come"
        );
        ci.expected_until = None;
        ci.missing_required.clear();
        assert_eq!(ci.detail(now)["required_state"], "pass");
    }

    /// A required check may `need` a job that still runs: while anything
    /// runs, a missing one is not a blocker yet.
    #[test]
    fn a_missing_required_check_is_running_while_the_ci_runs() {
        let now = Instant::now();
        let mut ci = ci(vec![pipeline(
            State::Running,
            vec![
                job("build", State::Running, Some(false)),
                job("check", State::Pass, Some(true)),
            ],
        )]);
        ci.missing_required = vec!["e2e".into()];
        assert_eq!(ci.detail(now)["required_state"], "running");
        ci.missing_required.clear();
        assert_eq!(
            ci.detail(now)["required_state"],
            "pass",
            "a non-required job running holds back nothing required"
        );
        ci.missing_required = vec!["e2e".into()];
        ci.pipelines[0].state = State::Pass;
        ci.pipelines[0].jobs[0].state = State::Pass;
        assert_eq!(ci.detail(now)["required_state"], "missing", "all is over");
    }

    /// A required pipeline that waits for its build has no job to carry the
    /// requirement: it carries it. Once a job exists, only jobs do.
    #[test]
    fn a_pipeline_carries_the_requirement_only_without_jobs() {
        let now = Instant::now();
        let mut waiting = pipeline(State::Queued, vec![]);
        waiting.required = Some(true);
        let mut with_jobs = pipeline(State::Pass, vec![job("a", State::Pass, Some(false))]);
        with_jobs.required = Some(true);
        let ci = ci(vec![waiting, with_jobs]);
        let detail = ci.detail(now);
        assert_eq!(detail["required_state"], "running");
        assert_eq!(detail["pipelines"][0]["required"], true);
        assert_eq!(detail["pipelines"][1]["required"], Value::Null);
    }
}
