//! The jobs of a build, from its timeline. The unit of job is the `Phase`
//! record, grouped under its `Stage`: a job waiting or skipped exists only as
//! a `Phase`; an internal `Job` without one ("Finalize build") and the tasks
//! are not kept. A timeline is read from its last change (`changeId`), and a
//! completed build's is read again only when its status changes: a rerun of
//! jobs or of a stage keeps the build's id. Kept per build, for every
//! branch followed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::{Client, Names, VERSION};
use crate::model::{Job, State};
use crate::tools::http::Failure;

/// Timelines kept at most; past it, they are read again in full.
const KEPT_MAX: usize = 1024;
/// The stage of a pipeline that declares none.
const DEFAULT_STAGE: &str = "__default";

/// The timeline of each build, by its project and id.
pub type Timelines = Arc<Mutex<HashMap<String, Timeline>>>;

/// What a timeline's last reading gave.
#[derive(Debug, Clone, Default)]
pub struct Timeline {
    /// The build's status and result when it was read.
    version: String,
    change_id: Option<i64>,
    /// The records kept, by id: stages, phases and approvals.
    records: HashMap<String, Value>,
}

/// The jobs of `build`, and its attempt: the highest of its jobs'. A build
/// not started yet has none.
pub async fn jobs(
    client: &Client<'_>,
    names: &Names,
    build: &Value,
) -> Result<(Vec<Job>, u64), Failure> {
    let status = build["status"].as_str().unwrap_or_default();
    let Some(id) = build["id"].as_u64() else {
        return Ok((Vec::new(), 1));
    };
    if matches!(status, "notStarted" | "postponed" | "none" | "") {
        return Ok((Vec::new(), 1));
    }
    let key = format!("{}/{id}", names.project_api());
    let version = format!("{status}/{}", build["result"].as_str().unwrap_or_default());
    let kept = client.context.timelines.lock().unwrap().get(&key).cloned();
    let timeline = match kept {
        Some(kept) if kept.version == version && status == "completed" => kept,
        kept => {
            let kept = kept.unwrap_or_default();
            let mut path = format!(
                "{}/build/builds/{id}/timeline?{VERSION}",
                names.project_api()
            );
            if let Some(change) = kept.change_id {
                path.push_str(&format!("&changeId={change}"));
            }
            let read = match client.get(&path).await {
                Ok(read) => read,
                Err(Failure::NotFound) => return Ok((Vec::new(), 1)),
                Err(e) => return Err(e),
            };
            let timeline = merge(kept, &read, version);
            let mut timelines = client.context.timelines.lock().unwrap();
            if timelines.len() >= KEPT_MAX {
                timelines.clear();
            }
            timelines.insert(key, timeline.clone());
            timeline
        }
    };
    Ok(jobs_of(
        &timeline.records,
        build["_links"]["web"]["href"].as_str(),
    ))
}

/// `kept` updated with the records `read` changed. A timeline that is not
/// there yet (`null`) changes nothing.
fn merge(mut kept: Timeline, read: &Value, version: String) -> Timeline {
    kept.version = version;
    if let Some(change) = read["changeId"].as_i64() {
        kept.change_id = Some(change);
    }
    let records = read["records"].as_array().into_iter().flatten();
    for record in records.filter(|r| is_kept(r)) {
        if let Some(id) = record["id"].as_str() {
            kept.records.insert(id.to_owned(), record.clone());
        }
    }
    kept
}

fn is_kept(record: &Value) -> bool {
    matches!(
        record["type"].as_str(),
        Some("Stage" | "Phase" | "Checkpoint.Approval")
    )
}

/// The `Phase` records as jobs, in the order of their stages then their
/// own, each pointing at the build's `url`. While an approval is pending,
/// the jobs that have not started wait for that person.
fn jobs_of(records: &HashMap<String, Value>, url: Option<&str>) -> (Vec<Job>, u64) {
    let approval = records
        .values()
        .any(|r| r["type"] == "Checkpoint.Approval" && r["state"] == "inProgress");
    let stage = |phase: &Value| phase["parentId"].as_str().and_then(|id| records.get(id));
    let mut phases: Vec<&Value> = records.values().filter(|r| r["type"] == "Phase").collect();
    phases.sort_by_key(|p| {
        let stage_order = stage(p).and_then(|s| s["order"].as_u64());
        (stage_order, p["order"].as_u64())
    });
    let attempt = phases
        .iter()
        .filter_map(|p| p["attempt"].as_u64())
        .max()
        .unwrap_or(1);
    let jobs = phases
        .into_iter()
        .map(|phase| {
            let stage_name = stage(phase)
                .and_then(|s| s["name"].as_str())
                .filter(|n| *n != DEFAULT_STAGE);
            job(phase, stage_name, approval, url)
        })
        .collect();
    (jobs, attempt)
}

fn job(phase: &Value, stage: Option<&str>, approval: bool, url: Option<&str>) -> Job {
    let (mut state, warning) = record_state(phase["state"].as_str(), phase["result"].as_str());
    if approval && state == State::Queued {
        state = State::Waiting;
    }
    let text = |v: &Value| v.as_str().map(str::to_owned);
    Job {
        id: text(&phase["id"]).unwrap_or_default(),
        name: text(&phase["name"]).unwrap_or_default(),
        stage: stage.map(str::to_owned),
        state,
        warning,
        required: None,
        started_at: text(&phase["startTime"]),
        finished_at: text(&phase["finishTime"]),
        url: url.map(str::to_owned),
    }
}

/// A timeline record's state: its `state` while it is not over, then its
/// `result`.
fn record_state(state: Option<&str>, result: Option<&str>) -> (State, bool) {
    match state {
        Some("pending") => (State::Queued, false),
        Some("inProgress") => (State::Running, false),
        Some("completed") => match result {
            Some("succeeded") => (State::Pass, false),
            Some("succeededWithIssues") => (State::Pass, true),
            Some("failed") => (State::Fail, false),
            Some("canceled") => (State::Cancelled, false),
            Some("skipped") => (State::Skipped, false),
            _ => (State::Unknown, false),
        },
        _ => (State::Unknown, false),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_record_maps_its_state_and_result() {
        assert_eq!(record_state(Some("pending"), None), (State::Queued, false));
        assert_eq!(
            record_state(Some("completed"), Some("succeededWithIssues")),
            (State::Pass, true)
        );
        assert_eq!(
            record_state(Some("completed"), Some("skipped")),
            (State::Skipped, false)
        );
        assert_eq!(
            record_state(Some("completed"), Some("abandoned")),
            (State::Unknown, false)
        );
    }

    /// A change of the timeline replaces the records it names and keeps
    /// the others; tasks are not kept.
    #[test]
    fn a_change_is_merged_into_what_was_kept() {
        let first = json!({ "changeId": 3, "records": [
            { "id": "p", "type": "Phase", "name": "a", "state": "inProgress" },
            { "id": "t", "type": "Task", "name": "step" },
        ]});
        let kept = merge(Timeline::default(), &first, "inProgress/".into());
        assert_eq!(kept.records.len(), 1);
        let change = json!({ "changeId": 5, "records": [
            { "id": "p", "type": "Phase", "name": "a", "state": "completed", "result": "succeeded" },
        ]});
        let kept = merge(kept, &change, "completed/succeeded".into());
        assert_eq!(kept.change_id, Some(5));
        let (jobs, attempt) = jobs_of(&kept.records, None);
        assert_eq!((jobs[0].state, attempt), (State::Pass, 1));
        let kept = merge(kept, &Value::Null, "completed/succeeded".into());
        assert_eq!(kept.records.len(), 1, "no timeline yet changes nothing");
    }
}
