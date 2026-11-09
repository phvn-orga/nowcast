//! The CI of a commit, neutral across forges: its pipelines, each with its
//! jobs, and a rollup. Each forge maps its own vocabulary onto [`State`] and
//! [`Trigger`].

use serde_json::{Value, json};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub stage: Option<String>,
    pub state: State,
    pub warning: bool,
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
    /// The commit the pipeline ran on.
    pub sha: Option<String>,
    pub git_ref: Option<String>,
    pub state: State,
    pub warning: bool,
    pub required: Option<bool>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub url: Option<String>,
    pub jobs: Vec<Job>,
}

/// The CI of one commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ci {
    pub pipelines: Vec<Pipeline>,
    /// Required checks that nothing reported on the commit.
    pub missing_required: Vec<String>,
}

impl Ci {
    /// Whether a pipeline or a job is not over yet.
    pub fn is_active(&self) -> bool {
        self.elements().any(|(state, _)| state.is_active())
    }

    /// Every pipeline and every job, with whether it is required.
    fn elements(&self) -> impl Iterator<Item = (State, Option<bool>)> + '_ {
        self.pipelines.iter().flat_map(|p| {
            std::iter::once((p.state, p.required))
                .chain(p.jobs.iter().map(|j| (j.state, j.required)))
        })
    }

    /// The detail's `ci`. `expected`: a CI is due on the commit and has not
    /// shown up yet, so the rollup cannot be `none` or `pass`.
    pub fn to_json(&self, expected: bool) -> Value {
        let any_required =
            self.elements().any(|(_, r)| r.is_some()) || !self.missing_required.is_empty();
        let required_state = any_required.then(|| {
            rollup(
                self.elements()
                    .filter(|(_, r)| *r == Some(true))
                    .map(|(s, _)| s),
                !self.missing_required.is_empty(),
                expected,
            )
        });
        let counted = || {
            self.pipelines
                .iter()
                .flat_map(|p| &p.jobs)
                .filter(|j| j.state != State::Skipped)
        };
        json!({
            "state": rollup(self.elements().map(|(s, _)| s), false, expected),
            "required_state": required_state,
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
        "required": p.required,
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

    #[test]
    fn a_failed_job_fails_a_running_pipeline() {
        let ci = Ci {
            pipelines: vec![pipeline(
                State::Running,
                vec![job("a", State::Fail, None), job("b", State::Running, None)],
            )],
            missing_required: vec![],
        };
        let json = ci.to_json(false);
        assert_eq!(json["state"], "fail");
        assert_eq!(
            json["required_state"],
            Value::Null,
            "nothing says what is required"
        );
        assert_eq!(json["jobs"], json!({ "done": 1, "total": 2 }));
    }

    #[test]
    fn nothing_is_none_unless_a_ci_is_expected() {
        assert_eq!(Ci::default().to_json(false)["state"], "none");
        assert_eq!(Ci::default().to_json(true)["state"], "running");
    }

    #[test]
    fn skipped_jobs_count_for_nothing() {
        let ci = Ci {
            pipelines: vec![pipeline(
                State::Pass,
                vec![job("a", State::Pass, None), job("b", State::Skipped, None)],
            )],
            missing_required: vec![],
        };
        let json = ci.to_json(false);
        assert_eq!(json["state"], "pass");
        assert_eq!(json["jobs"], json!({ "done": 1, "total": 1 }));
    }

    #[test]
    fn the_required_rollup_sees_only_required_items_and_what_is_missing() {
        let mut ci = Ci {
            pipelines: vec![pipeline(
                State::Fail,
                vec![
                    job("lint", State::Fail, Some(false)),
                    job("check", State::Pass, Some(true)),
                ],
            )],
            missing_required: vec!["e2e".into()],
        };
        let json = ci.to_json(false);
        assert_eq!(json["state"], "fail");
        assert_eq!(json["required_state"], "missing");
        assert_eq!(
            ci.to_json(true)["required_state"],
            "running",
            "it may still come"
        );
        ci.missing_required.clear();
        assert_eq!(ci.to_json(false)["required_state"], "pass");
    }
}
