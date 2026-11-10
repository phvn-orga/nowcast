//! The core of the brick: a registry of providers, the flows, and their
//! publication. It knows no forge and no pace.
//!
//! - `pr {remote, branch}` publishes `forge.pr/v1`, flow
//!   `pr/<repo>#<branch>`;
//! - `pipelines {remote, branch}` publishes `forge.pipelines/v1`, flow
//!   `pipelines/<repo>#<branch>`.
//!
//! Both kinds of one branch share a target: the provider follows the branch
//! once and each reading serves both flows. A flow is published when its
//! detail changed, and after a `refresh` in any case.

use std::collections::HashMap;

use jiff::Timestamp;
use nowcast_types::{BrickMessage, FlowId, RequestId};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Instant;

use crate::model::Ci;
use crate::provider::{BranchReading, Failure, Outcome, Provider, Repo, Updates, Watch};

pub const PR_TYPE: &str = "forge.pr/v1";
pub const PIPELINES_TYPE: &str = "forge.pipelines/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Pr,
    Pipelines,
}

impl Kind {
    fn parse(kind: &str) -> Option<Self> {
        match kind {
            "pr" => Some(Self::Pr),
            "pipelines" => Some(Self::Pipelines),
            _ => None,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Pr => "pr",
            Self::Pipelines => "pipelines",
        }
    }

    const fn data_type(self) -> &'static str {
        match self {
            Self::Pr => PR_TYPE,
            Self::Pipelines => PIPELINES_TYPE,
        }
    }
}

/// A branch of a repository: `(repo id, branch)`.
type TargetKey = (String, String);

/// A branch a provider follows for one or more flows.
struct Target {
    /// Tells this target's readings from those of an earlier target of the
    /// same key, released since.
    id: u64,
    repo: Repo,
    branch: String,
    watch: Watch,
    /// The flows this target serves; it is released with the last one.
    flows: Vec<FlowId>,
    /// The last successful reading.
    reading: Option<BranchReading>,
    /// `None` until the first reading, then how the last one went.
    status: Option<Result<(), Failure>>,
    as_of: Option<Timestamp>,
}

impl Target {
    /// `source.status` in the details.
    fn source_status(&self) -> &'static str {
        match self.status {
            None => "pending",
            Some(Ok(())) => "ok",
            Some(Err(failure)) => failure.as_str(),
        }
    }
}

struct Flow {
    kind: Kind,
    target: TargetKey,
    /// Publish after the next reading even if nothing changed.
    force: bool,
    /// The detail last published, without its `as_of`.
    published: Option<Value>,
}

pub struct Core {
    providers: Vec<Box<dyn Provider>>,
    updates: UnboundedSender<(u64, Outcome)>,
    flows: HashMap<FlowId, Flow>,
    targets: HashMap<TargetKey, Target>,
    by_id: HashMap<u64, TargetKey>,
    last_target: u64,
    last_req: u64,
}

impl Core {
    /// `updates` receives what the providers read, for [`Core::update`].
    pub fn new(
        providers: Vec<Box<dyn Provider>>,
        updates: UnboundedSender<(u64, Outcome)>,
    ) -> Self {
        Self {
            providers,
            updates,
            flows: HashMap::new(),
            targets: HashMap::new(),
            by_id: HashMap::new(),
            last_target: 0,
            last_req: 0,
        }
    }

    /// Answers a `subscribe`, then publishes the flow at once when its
    /// branch was already read for another flow.
    pub fn subscribe(&mut self, req: RequestId, kind: &str, params: &[u8]) -> Vec<BrickMessage> {
        let refuse = |code: &str, message: String| {
            vec![BrickMessage::Error {
                req,
                code: code.to_owned(),
                message,
            }]
        };
        let Some(kind) = Kind::parse(kind) else {
            return refuse("unknown_kind", format!("no subscription kind {kind:?}"));
        };
        let params: Value = serde_json::from_slice(params).unwrap_or_default();
        let (Some(remote), Some(branch)) = (params["remote"].as_str(), params["branch"].as_str())
        else {
            return refuse("invalid_params", "remote and branch are required".into());
        };
        let Some((index, repo)) = self
            .providers
            .iter()
            .enumerate()
            .find_map(|(i, p)| p.recognise(remote).map(|r| (i, r)))
        else {
            return refuse(
                "unsupported_remote",
                format!("{remote:?} is not a repository on a forge the brick reads"),
            );
        };
        let flow = match FlowId::new(format!("{}/{}#{branch}", kind.name(), repo.id)) {
            Ok(flow) => flow,
            Err(e) => return refuse("key_too_long", e.to_string()),
        };
        let mut out = vec![BrickMessage::Subscribed {
            req,
            flow: flow.clone(),
        }];
        if self.flows.contains_key(&flow) {
            return out;
        }
        let key = (repo.id.clone(), branch.to_owned());
        if !self.targets.contains_key(&key) {
            self.last_target += 1;
            let id = self.last_target;
            let updates = Updates::new(id, self.updates.clone());
            let watch = self.providers[index].follow(&repo, branch, updates);
            self.by_id.insert(id, key.clone());
            self.targets.insert(
                key.clone(),
                Target {
                    id,
                    repo,
                    branch: branch.to_owned(),
                    watch,
                    flows: Vec::new(),
                    reading: None,
                    status: None,
                    as_of: None,
                },
            );
        }
        let target = self.targets.get_mut(&key).expect("just inserted");
        target.flows.push(flow.clone());
        let mut new = Flow {
            kind,
            target: key,
            force: false,
            published: None,
        };
        if target.status.is_some() {
            out.extend(publish(
                &mut self.last_req,
                &flow,
                &mut new,
                target,
                Instant::now(),
            ));
        }
        self.flows.insert(flow, new);
        out
    }

    /// The last subscriber of flow `id` left; its branch stops being followed
    /// when no other flow needs it.
    pub fn release(&mut self, id: &FlowId) {
        let Some(flow) = self.flows.remove(id) else {
            return;
        };
        if let Some(target) = self.targets.get_mut(&flow.target) {
            target.flows.retain(|f| f != id);
            if target.flows.is_empty()
                && let Some(target) = self.targets.remove(&flow.target)
            {
                self.by_id.remove(&target.id);
            }
        }
    }

    /// Asks the provider for a reading now; the flow is published after it.
    pub fn refresh(&mut self, flow: &FlowId) {
        let Some(flow) = self.flows.get_mut(flow) else {
            return;
        };
        flow.force = true;
        if let Some(target) = self.targets.get(&flow.target) {
            target.watch.refresh();
        }
    }

    /// Takes in what a provider read for target `id` at `at`, and publishes
    /// the flows whose detail changed or that a `refresh` asked for, as of
    /// `now`. A failure keeps the last reading.
    pub fn update(
        &mut self,
        id: u64,
        outcome: Outcome,
        at: Timestamp,
        now: Instant,
    ) -> Vec<BrickMessage> {
        let Some(key) = self.by_id.get(&id) else {
            return vec![];
        };
        let Some(target) = self.targets.get_mut(key) else {
            return vec![];
        };
        match outcome {
            Ok(reading) => {
                target.reading = Some(reading);
                target.status = Some(Ok(()));
                target.as_of = Some(at);
            }
            Err(failure) => target.status = Some(Err(failure)),
        }
        let target = &*target;
        let mut out = Vec::new();
        for flow_id in &target.flows {
            if let Some(flow) = self.flows.get_mut(flow_id) {
                out.extend(publish(&mut self.last_req, flow_id, flow, target, now));
            }
        }
        out
    }
}

/// Publishes `flow` when its detail as of `now` changed or a `refresh`
/// asked for it.
fn publish(
    last_req: &mut u64,
    id: &FlowId,
    flow: &mut Flow,
    target: &Target,
    now: Instant,
) -> Option<BrickMessage> {
    let detail = match flow.kind {
        Kind::Pr => pr_detail(target, now),
        Kind::Pipelines => pipelines_detail(target, now),
    };
    let force = std::mem::take(&mut flow.force);
    if !force && flow.published.as_ref() == Some(&detail) {
        return None;
    }
    flow.published = Some(detail.clone());
    let mut detail = detail;
    detail["source"]["as_of"] = json!(target.as_of.map(|t| t.to_string()));
    *last_req += 1;
    Some(BrickMessage::Publish {
        req: RequestId(*last_req),
        flow: id.clone(),
        data_type: flow.kind.data_type().to_owned(),
        detail: detail.to_string().into_bytes(),
    })
}

/// `forge.pr/v1` as of `now`, without its `as_of`.
fn pr_detail(target: &Target, now: Instant) -> Value {
    let reading = target.reading.as_ref();
    let pr = reading.and_then(|r| r.pr.as_ref()).map(|p| {
        json!({
            "number": p.number,
            "state": p.state,
            "draft": p.draft,
            "base": p.base,
            "head_sha": p.head_sha,
            "url": p.url,
            "auto_merge": p.auto_merge.as_ref().map(|(by, method)| json!({ "enabled_by": by, "method": method })),
            "queue": null,
            "dequeued": null,
        })
    });
    json!({
        "repo": target.repo.id,
        "branch": target.branch,
        "head_sha": reading.and_then(|r| r.head_sha.as_deref()),
        "pr": pr,
        "ci": reading.and_then(|r| r.ci.as_ref()).map(|c| c.synthesis(now)),
        "source": { "status": target.source_status(), "as_of": null },
    })
}

/// `forge.pipelines/v1` as of `now`, without its `as_of`.
fn pipelines_detail(target: &Target, now: Instant) -> Value {
    let reading = target.reading.as_ref();
    let ci = reading.and_then(|r| r.ci.as_ref());
    let mut detail = ci.map_or_else(|| Ci::default().detail(now), |c| c.detail(now));
    detail["repo"] = json!(target.repo.id);
    detail["branch"] = json!(target.branch);
    detail["sha"] = json!(ci.map(|c| &c.sha));
    detail["pr"] = json!(
        reading
            .and_then(|r| r.pr.as_ref())
            .filter(|p| p.is_open())
            .map(|p| p.number)
    );
    detail["source"] = json!({ "status": target.source_status(), "as_of": null });
    detail
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    use super::*;
    use crate::model::tests::{ci, job, pipeline};
    use crate::model::{Pr, State};

    /// Recognises `https://forge/<repo>`; records what it follows and each
    /// `refresh` it receives.
    #[derive(Default, Clone)]
    struct Fake {
        followed: Arc<Mutex<Vec<(String, String)>>>,
        refreshes: Arc<Mutex<usize>>,
    }

    impl Provider for Fake {
        fn recognise(&self, remote: &str) -> Option<Repo> {
            let name = remote.strip_prefix("https://forge/")?;
            Some(Repo {
                id: format!("forge/{}", name.to_lowercase()),
                remote: remote.to_owned(),
            })
        }

        fn follow(&self, repo: &Repo, branch: &str, _updates: Updates) -> Watch {
            self.followed
                .lock()
                .unwrap()
                .push((repo.id.clone(), branch.to_owned()));
            let refreshes = self.refreshes.clone();
            Watch::spawn(move |mut rx: UnboundedReceiver<()>| async move {
                while rx.recv().await.is_some() {
                    *refreshes.lock().unwrap() += 1;
                }
            })
        }
    }

    fn core() -> (Core, Fake) {
        let fake = Fake::default();
        let (tx, _rx) = unbounded_channel();
        (Core::new(vec![Box::new(fake.clone())], tx), fake)
    }

    fn params(remote: &str) -> Vec<u8> {
        json!({ "remote": remote, "branch": "feat" })
            .to_string()
            .into_bytes()
    }

    fn reading(sha: &str, state: State) -> Outcome {
        let mut ci = ci(vec![pipeline(state, vec![job("check", state, Some(true))])]);
        ci.sha = sha.into();
        Ok(BranchReading {
            head_sha: Some(sha.into()),
            pr: Some(Pr {
                number: 7,
                state: "open",
                draft: false,
                base: "main".into(),
                head_sha: sha.into(),
                url: "u".into(),
                auto_merge: None,
            }),
            ci: Some(ci),
        })
    }

    fn published(out: &[BrickMessage]) -> Vec<(String, Value)> {
        out.iter()
            .filter_map(|m| match m {
                BrickMessage::Publish { flow, detail, .. } => Some((
                    flow.as_str().to_owned(),
                    serde_json::from_slice(detail).unwrap(),
                )),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn both_kinds_of_a_branch_share_one_following() {
        let (mut core, fake) = core();
        let pr = core.subscribe(RequestId(1), "pr", &params("https://forge/O/R"));
        let again = core.subscribe(RequestId(2), "pr", &params("https://forge/o/r"));
        let pipelines = core.subscribe(RequestId(3), "pipelines", &params("https://forge/o/r"));
        let flow = |m: &BrickMessage| match m {
            BrickMessage::Subscribed { flow, .. } => flow.as_str().to_owned(),
            other => panic!("{other:?}"),
        };
        assert_eq!(flow(&pr[0]), "pr/forge/o/r#feat");
        assert_eq!(flow(&again[0]), "pr/forge/o/r#feat");
        assert_eq!(flow(&pipelines[0]), "pipelines/forge/o/r#feat");
        assert_eq!(
            *fake.followed.lock().unwrap(),
            [("forge/o/r".to_owned(), "feat".to_owned())]
        );
    }

    #[tokio::test]
    async fn what_no_provider_recognises_is_refused() {
        let (mut core, _) = core();
        let code = |out: Vec<BrickMessage>| match &out[..] {
            [BrickMessage::Error { code, .. }] => code.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            code(core.subscribe(RequestId(1), "pr", &params("https://elsewhere/o/r"))),
            "unsupported_remote"
        );
        assert_eq!(
            code(core.subscribe(RequestId(2), "issues", &params("https://forge/o/r"))),
            "unknown_kind"
        );
        assert_eq!(
            code(core.subscribe(RequestId(3), "pr", br#"{"remote":"https://forge/o/r"}"#)),
            "invalid_params"
        );
    }

    #[tokio::test]
    async fn a_reading_serves_both_flows_and_publishes_only_what_changed() {
        let (mut core, _) = core();
        core.subscribe(RequestId(1), "pr", &params("https://forge/o/r"));
        core.subscribe(RequestId(2), "pipelines", &params("https://forge/o/r"));
        let first = published(&core.update(
            1,
            reading("a", State::Running),
            Timestamp::now(),
            Instant::now(),
        ));
        assert_eq!(first.len(), 2);
        let pr = &first.iter().find(|(f, _)| f.starts_with("pr/")).unwrap().1;
        assert_eq!(pr["pr"]["number"], 7);
        assert_eq!(
            pr["ci"],
            json!({ "state": "running", "required_state": "running", "sha": "a" })
        );
        assert!(pr["source"]["as_of"].is_string());
        let pipelines = &first
            .iter()
            .find(|(f, _)| f.starts_with("pipelines/"))
            .unwrap()
            .1;
        assert_eq!(pipelines["sha"], "a");
        assert_eq!(pipelines["pr"], 7);
        assert_eq!(pipelines["pipelines"][0]["jobs"][0]["required"], true);
        assert_eq!(pipelines["source"]["status"], "ok");

        let later = Timestamp::now() + jiff::SignedDuration::from_secs(5);
        assert!(
            core.update(1, reading("a", State::Running), later, Instant::now())
                .is_empty(),
            "only as_of moved"
        );
    }

    #[tokio::test]
    async fn a_refresh_reaches_the_provider_and_forces_a_publication() {
        let (mut core, fake) = core();
        core.subscribe(RequestId(1), "pr", &params("https://forge/o/r"));
        core.update(
            1,
            reading("a", State::Pass),
            Timestamp::now(),
            Instant::now(),
        );
        let flow = FlowId::new("pr/forge/o/r#feat").unwrap();
        core.refresh(&flow);
        tokio::task::yield_now().await;
        assert_eq!(*fake.refreshes.lock().unwrap(), 1);
        let out = core.update(
            1,
            reading("a", State::Pass),
            Timestamp::now(),
            Instant::now(),
        );
        assert_eq!(published(&out).len(), 1, "unchanged, but refreshed");
    }

    #[tokio::test]
    async fn a_failure_keeps_the_last_reading() {
        let (mut core, _) = core();
        core.subscribe(RequestId(1), "pr", &params("https://forge/o/r"));
        core.update(
            1,
            reading("a", State::Pass),
            Timestamp::now(),
            Instant::now(),
        );
        let out = published(&core.update(
            1,
            Err(Failure::Unreachable),
            Timestamp::now(),
            Instant::now(),
        ));
        assert_eq!(out[0].1["source"]["status"], "unreachable");
        assert_eq!(out[0].1["pr"]["number"], 7, "the last reading stays");
    }

    #[tokio::test]
    async fn an_expected_ci_expires_even_when_the_readings_fail() {
        let (mut core, _) = core();
        core.subscribe(RequestId(1), "pr", &params("https://forge/o/r"));
        let t0 = Instant::now();
        let expected = Ok(BranchReading {
            head_sha: Some("a".into()),
            pr: None,
            ci: Some(Ci {
                sha: "a".into(),
                expected_until: Some(t0 + Duration::from_secs(120)),
                ..Ci::default()
            }),
        });
        let out = published(&core.update(1, expected, Timestamp::now(), t0));
        assert_eq!(out[0].1["ci"]["state"], "running");
        let later = t0 + Duration::from_secs(121);
        let out = published(&core.update(1, Err(Failure::Unreachable), Timestamp::now(), later));
        assert_eq!(out[0].1["source"]["status"], "unreachable");
        assert_eq!(out[0].1["ci"]["state"], "none", "no CI came for this push");
    }

    #[tokio::test]
    async fn a_flow_added_to_a_read_branch_is_published_at_once() {
        let (mut core, _) = core();
        core.subscribe(RequestId(1), "pr", &params("https://forge/o/r"));
        assert!(
            published(&core.subscribe(RequestId(2), "pipelines", &params("https://forge/o/r")))
                .is_empty(),
            "nothing read yet"
        );
        core.release(&FlowId::new("pipelines/forge/o/r#feat").unwrap());
        core.update(
            1,
            reading("a", State::Pass),
            Timestamp::now(),
            Instant::now(),
        );
        let out = core.subscribe(RequestId(3), "pipelines", &params("https://forge/o/r"));
        assert!(matches!(out[0], BrickMessage::Subscribed { .. }));
        assert_eq!(published(&out)[0].1["state"], "pass");
    }

    #[tokio::test]
    async fn the_last_flow_released_stops_the_following() {
        let (mut core, fake) = core();
        core.subscribe(RequestId(1), "pr", &params("https://forge/o/r"));
        core.subscribe(RequestId(2), "pipelines", &params("https://forge/o/r"));
        core.release(&FlowId::new("pr/forge/o/r#feat").unwrap());
        assert!(
            !published(&core.update(
                1,
                reading("a", State::Pass),
                Timestamp::now(),
                Instant::now()
            ))
            .is_empty(),
            "the pipelines flow still needs the branch"
        );
        core.release(&FlowId::new("pipelines/forge/o/r#feat").unwrap());
        assert!(
            core.update(
                1,
                reading("b", State::Pass),
                Timestamp::now(),
                Instant::now()
            )
            .is_empty()
        );
        core.subscribe(RequestId(3), "pr", &params("https://forge/o/r"));
        assert_eq!(fake.followed.lock().unwrap().len(), 2, "followed anew");
        assert!(
            core.update(
                1,
                reading("c", State::Pass),
                Timestamp::now(),
                Instant::now()
            )
            .is_empty(),
            "a reading of the released target is ignored"
        );
    }

    #[tokio::test]
    async fn without_commit_the_pipelines_are_empty() {
        let (mut core, _) = core();
        core.subscribe(RequestId(1), "pipelines", &params("https://forge/o/r"));
        let out = published(&core.update(
            1,
            Ok(BranchReading::default()),
            Timestamp::now(),
            Instant::now(),
        ));
        let detail = &out[0].1;
        assert_eq!(detail["sha"], Value::Null);
        assert_eq!(detail["pr"], Value::Null);
        assert_eq!(detail["state"], "none");
        assert_eq!(detail["pipelines"], json!([]));
    }
}
