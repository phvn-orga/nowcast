//! The core of the brick: a registry of providers, the flows, and their
//! publication. It knows no forge and no pace.
//!
//! - `pr {remote, branch}` publishes `forge.pr/v1`, flow
//!   `pr/<repo>#<branch>`;
//! - `pipelines {remote, branch}` publishes `forge.pipelines/v1`, flow
//!   `pipelines/<repo>#<branch>`;
//! - `issues {remote}` publishes `forge.issues/v1`, flow `issues/<tracker>`;
//! - `issue {remote, number}` publishes `forge.issue/v1`, flow
//!   `issue/<tracker>#<number>`.
//!
//! A target is what a provider follows: a branch, a tracker's open issues,
//! or one issue. Both kinds of one branch share a target: the provider
//! follows the branch once and each reading serves both flows. A flow is
//! published when its detail changed, and after a `refresh` in any case.

use std::collections::HashMap;

use jiff::Timestamp;
use nowcast_types::{BrickMessage, FlowId, RequestId};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Instant;

use crate::model::{Ci, Issue, Issues};
use crate::provider::{BranchReading, Failure, Outcome, Provider, Reading, Repo, Updates, Watch};

pub const PR_TYPE: &str = "forge.pr/v1";
pub const PIPELINES_TYPE: &str = "forge.pipelines/v1";
pub const ISSUES_TYPE: &str = "forge.issues/v1";
pub const ISSUE_TYPE: &str = "forge.issue/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Pr,
    Pipelines,
    Issues,
    Issue,
}

impl Kind {
    fn parse(kind: &str) -> Option<Self> {
        match kind {
            "pr" => Some(Self::Pr),
            "pipelines" => Some(Self::Pipelines),
            "issues" => Some(Self::Issues),
            "issue" => Some(Self::Issue),
            _ => None,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Pr => "pr",
            Self::Pipelines => "pipelines",
            Self::Issues => "issues",
            Self::Issue => "issue",
        }
    }

    const fn data_type(self) -> &'static str {
        match self {
            Self::Pr => PR_TYPE,
            Self::Pipelines => PIPELINES_TYPE,
            Self::Issues => ISSUES_TYPE,
            Self::Issue => ISSUE_TYPE,
        }
    }

    /// The subject `params` name for this kind, or why they do not.
    fn subject(self, params: &Value) -> Result<Subject, &'static str> {
        match self {
            Self::Pr | Self::Pipelines => params["branch"]
                .as_str()
                .map(|b| Subject::Branch(b.to_owned()))
                .ok_or("remote and branch are required"),
            Self::Issues => Ok(Subject::Issues),
            Self::Issue => params["number"]
                .as_u64()
                .filter(|n| *n > 0)
                .map(Subject::Issue)
                .ok_or("remote and a positive number are required"),
        }
    }
}

/// What a target is about, within its repository or tracker.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Subject {
    Branch(String),
    /// The tracker's open issues.
    Issues,
    Issue(u64),
}

impl Subject {
    /// What the subject belongs to: the repository for a branch, the
    /// tracker for issues.
    fn scope(&self, repo: &Repo) -> String {
        match self {
            Self::Branch(_) => repo.id.clone(),
            Self::Issues | Self::Issue(_) => repo.tracker.clone(),
        }
    }

    /// The identifier of `kind`'s flow on this subject, in `scope`.
    fn flow(&self, kind: Kind, scope: &str) -> String {
        match self {
            Self::Branch(branch) => format!("{}/{scope}#{branch}", kind.name()),
            Self::Issues => format!("{}/{scope}", kind.name()),
            Self::Issue(number) => format!("{}/{scope}#{number}", kind.name()),
        }
    }
}

/// A subject in its scope: `(repo id or tracker, subject)`.
type TargetKey = (String, Subject);

/// What a provider follows for one or more flows.
struct Target {
    /// Tells this target's readings from those of an earlier target of the
    /// same key, released since.
    id: u64,
    repo: Repo,
    subject: Subject,
    watch: Watch,
    /// The flows this target serves; it is released with the last one.
    flows: Vec<FlowId>,
    /// The last successful reading.
    reading: Option<Reading>,
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

    /// `source` in the details, without its `as_of`.
    fn source(&self) -> Value {
        json!({ "status": self.source_status(), "as_of": null })
    }

    fn branch(&self) -> Option<&BranchReading> {
        match &self.reading {
            Some(Reading::Branch(reading)) => Some(reading.as_ref()),
            _ => None,
        }
    }

    fn issues(&self) -> Option<&Issues> {
        match &self.reading {
            Some(Reading::Issues(issues)) => Some(issues),
            _ => None,
        }
    }

    fn issue(&self) -> Option<&Issue> {
        match &self.reading {
            Some(Reading::Issue(issue)) => Some(issue),
            _ => None,
        }
    }

    /// The branch's name, for a branch's target.
    fn branch_name(&self) -> &str {
        match &self.subject {
            Subject::Branch(branch) => branch,
            _ => "",
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
    /// target was already read for another flow.
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
        let (remote, subject) = match (params["remote"].as_str(), kind.subject(&params)) {
            (Some(remote), Ok(subject)) => (remote, subject),
            (_, Err(why)) => return refuse("invalid_params", why.into()),
            (None, Ok(_)) => return refuse("invalid_params", "remote is required".into()),
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
        let key = (subject.scope(&repo), subject);
        let flow = match FlowId::new(key.1.flow(kind, &key.0)) {
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
        if !self.targets.contains_key(&key) {
            self.follow(index, repo, key.clone());
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

    /// Has provider `index` follow the subject of `key` in `repo`.
    fn follow(&mut self, index: usize, repo: Repo, key: TargetKey) {
        self.last_target += 1;
        let id = self.last_target;
        let updates = Updates::new(id, self.updates.clone());
        let provider = &self.providers[index];
        let watch = match &key.1 {
            Subject::Branch(branch) => provider.follow(&repo, branch, updates),
            Subject::Issues => provider.follow_issues(&repo, updates),
            Subject::Issue(number) => provider.follow_issue(&repo, *number, updates),
        };
        self.by_id.insert(id, key.clone());
        let subject = key.1.clone();
        self.targets.insert(
            key,
            Target {
                id,
                repo,
                subject,
                watch,
                flows: Vec::new(),
                reading: None,
                status: None,
                as_of: None,
            },
        );
    }

    /// The last subscriber of flow `id` left; its target stops being
    /// followed when no other flow needs it.
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
        Kind::Issues => issues_detail(target),
        Kind::Issue => issue_detail(target),
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
    let reading = target.branch();
    let pr = reading.and_then(|r| r.pr.as_ref()).map(|p| {
        json!({
            "number": p.number,
            "state": p.state,
            "draft": p.draft,
            "base": p.base,
            "head_sha": p.head_sha,
            "url": p.url,
            "auto_merge": p.auto_merge.as_ref().map(|(by, method)| json!({ "enabled_by": by, "method": method })),
            "queue": p.queue.as_ref().map(|q| q.detail(now)),
            "dequeued": p.dequeued.as_ref().map(|d| json!({ "reason": d.reason, "at": d.at })),
        })
    });
    json!({
        "repo": target.repo.id,
        "branch": target.branch_name(),
        "head_sha": reading.and_then(|r| r.head_sha.as_deref()),
        "pr": pr,
        "ci": reading.and_then(|r| r.ci.as_ref()).map(|c| c.synthesis(now)),
        "source": target.source(),
    })
}

/// `forge.pipelines/v1` as of `now`, without its `as_of`.
fn pipelines_detail(target: &Target, now: Instant) -> Value {
    let reading = target.branch();
    let ci = reading.and_then(|r| r.ci.as_ref());
    let mut detail = ci.map_or_else(|| Ci::default().detail(now), |c| c.detail(now));
    detail["repo"] = json!(target.repo.id);
    detail["branch"] = json!(target.branch_name());
    detail["sha"] = json!(ci.map(|c| &c.sha));
    detail["pr"] = json!(
        reading
            .and_then(|r| r.pr.as_ref())
            .filter(|p| p.is_open())
            .map(|p| p.number)
    );
    detail["source"] = target.source();
    detail
}

/// `forge.issues/v1`, without its `as_of`: empty lists until a reading.
fn issues_detail(target: &Target) -> Value {
    let issues = target.issues();
    let list = |pick: fn(&Issues) -> &Vec<Issue>| -> Vec<Value> {
        issues.map_or_else(Vec::new, |i| pick(i).iter().map(Issue::summary).collect())
    };
    json!({
        "tracker": target.repo.tracker,
        "account": issues.map(|i| &i.account),
        "mine": list(|i| &i.mine),
        "others": list(|i| &i.others),
        "truncated": issues.is_some_and(|i| i.truncated),
        "source": target.source(),
    })
}

/// `forge.issue/v1`, without its `as_of`: the number is the key's, the
/// other fields `null` until a reading finds the issue.
fn issue_detail(target: &Target) -> Value {
    let issue = target.issue();
    let number = match target.subject {
        Subject::Issue(number) => Some(number),
        _ => None,
    };
    json!({
        "tracker": target.repo.tracker,
        "number": number,
        "title": issue.map(|i| &i.title),
        "type": issue.and_then(|i| i.kind.as_ref()),
        "state": issue.map(Issue::state),
        "url": issue.map(|i| &i.url),
        "source": target.source(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    use super::*;
    use crate::model::tests::{ci, job, pipeline};
    use crate::model::{Dequeued, Pr, QueueEntry, State};

    /// Recognises `https://forge/<org>/<repo>`, whose issues the
    /// organisation holds, as on a forge where several repositories share a
    /// tracker; records what it follows and each `refresh` it receives.
    #[derive(Default, Clone)]
    struct Fake {
        followed: Arc<Mutex<Vec<(String, String)>>>,
        refreshes: Arc<Mutex<usize>>,
    }

    impl Fake {
        fn watch(&self, scope: &str, what: String) -> Watch {
            self.followed.lock().unwrap().push((scope.to_owned(), what));
            let refreshes = self.refreshes.clone();
            Watch::spawn(move |mut rx: UnboundedReceiver<()>| async move {
                while rx.recv().await.is_some() {
                    *refreshes.lock().unwrap() += 1;
                }
            })
        }
    }

    impl Provider for Fake {
        fn recognise(&self, remote: &str) -> Option<Repo> {
            let name = remote.strip_prefix("https://forge/")?.to_lowercase();
            let org = name.split('/').next()?;
            Some(Repo {
                id: format!("forge/{name}"),
                tracker: format!("forge/{org}"),
                remote: remote.to_owned(),
            })
        }

        fn follow(&self, repo: &Repo, branch: &str, _updates: Updates) -> Watch {
            self.watch(&repo.id, branch.to_owned())
        }

        fn follow_issues(&self, repo: &Repo, _updates: Updates) -> Watch {
            self.watch(&repo.tracker, "issues".to_owned())
        }

        fn follow_issue(&self, repo: &Repo, number: u64, _updates: Updates) -> Watch {
            self.watch(&repo.tracker, format!("#{number}"))
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
        Ok(Reading::Branch(Box::new(BranchReading {
            head_sha: Some(sha.into()),
            pr: Some(Pr {
                number: 7,
                state: "open",
                draft: false,
                base: "main".into(),
                head_sha: sha.into(),
                url: "u".into(),
                auto_merge: None,
                queue: None,
                dequeued: None,
            }),
            ci: Some(ci),
        })))
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
            code(core.subscribe(RequestId(2), "labels", &params("https://forge/o/r"))),
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

    /// The PR's entry in the merge queue carries the rollup of its CI.
    #[tokio::test]
    async fn the_pr_publishes_its_queue_entry_and_why_it_left() {
        let (mut core, _) = core();
        core.subscribe(RequestId(1), "pr", &params("https://forge/o/r"));
        let Ok(Reading::Branch(mut read)) = reading("a", State::Pass) else {
            unreachable!()
        };
        let mut queue_ci = ci(vec![pipeline(State::Running, vec![])]);
        queue_ci.sha = "q".into();
        let pr = read.pr.as_mut().unwrap();
        pr.queue = Some(QueueEntry {
            state: "queued".into(),
            position: 1,
            enqueued_at: "t0".into(),
            estimated_merge_at: None,
            sha: Some("q".into()),
            ci: Some(queue_ci),
        });
        pr.dequeued = Some(Dequeued {
            reason: Some("conflict".into()),
            at: "t1".into(),
        });
        let out = published(&core.update(
            1,
            Ok(Reading::Branch(read)),
            Timestamp::now(),
            Instant::now(),
        ));
        let pr = &out[0].1["pr"];
        assert_eq!(
            pr["queue"],
            json!({ "state": "queued", "position": 1, "enqueued_at": "t0",
                "estimated_merge_at": null, "sha": "q",
                "ci": { "state": "running", "required_state": null, "sha": "q" } })
        );
        assert_eq!(pr["dequeued"], json!({ "reason": "conflict", "at": "t1" }));
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
        let expected = Ok(Reading::Branch(Box::new(BranchReading {
            head_sha: Some("a".into()),
            pr: None,
            ci: Some(Ci {
                sha: "a".into(),
                expected_until: Some(t0 + Duration::from_secs(120)),
                ..Ci::default()
            }),
        })));
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
            Ok(Reading::Branch(Box::default())),
            Timestamp::now(),
            Instant::now(),
        ));
        let detail = &out[0].1;
        assert_eq!(detail["sha"], Value::Null);
        assert_eq!(detail["pr"], Value::Null);
        assert_eq!(detail["state"], "none");
        assert_eq!(detail["pipelines"], json!([]));
    }

    fn issue(number: u64, open: bool) -> Issue {
        Issue {
            number,
            title: format!("issue {number}"),
            kind: Some("Bug".into()),
            url: format!("u/{number}"),
            open,
        }
    }

    fn issues_params(remote: &str) -> Vec<u8> {
        json!({ "remote": remote }).to_string().into_bytes()
    }

    fn issue_params(remote: &str, number: u64) -> Vec<u8> {
        json!({ "remote": remote, "number": number })
            .to_string()
            .into_bytes()
    }

    /// Issues belong to the tracker: two repositories it holds share one
    /// following of its open issues, and of one issue.
    #[tokio::test]
    async fn the_repositories_of_one_tracker_share_its_issues() {
        let (mut core, fake) = core();
        let flow = |out: Vec<BrickMessage>| match &out[..] {
            [BrickMessage::Subscribed { flow, .. }] => flow.as_str().to_owned(),
            other => panic!("{other:?}"),
        };
        let issues = core.subscribe(RequestId(1), "issues", &issues_params("https://forge/O/a"));
        let again = core.subscribe(RequestId(2), "issues", &issues_params("https://forge/o/b"));
        let one = core.subscribe(
            RequestId(3),
            "issue",
            &issue_params("https://forge/o/a", 52),
        );
        let same = core.subscribe(
            RequestId(4),
            "issue",
            &issue_params("https://forge/o/b", 52),
        );
        assert_eq!(flow(issues), "issues/forge/o");
        assert_eq!(flow(again), "issues/forge/o");
        assert_eq!(flow(one), "issue/forge/o#52");
        assert_eq!(flow(same), "issue/forge/o#52");
        assert_eq!(
            *fake.followed.lock().unwrap(),
            [
                ("forge/o".to_owned(), "issues".to_owned()),
                ("forge/o".to_owned(), "#52".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn an_issue_needs_a_positive_number() {
        let (mut core, _) = core();
        for params in [
            br#"{"remote":"https://forge/o/r"}"#.as_slice(),
            br#"{"remote":"https://forge/o/r","number":0}"#,
            br#"{"remote":"https://forge/o/r","number":"52"}"#,
        ] {
            match &core.subscribe(RequestId(1), "issue", params)[..] {
                [BrickMessage::Error { code, .. }] => assert_eq!(code, "invalid_params"),
                other => panic!("{other:?}"),
            }
        }
        assert!(matches!(
            &core.subscribe(RequestId(2), "issues", b"{}")[..],
            [BrickMessage::Error { code, .. }] if code == "invalid_params"
        ));
    }

    /// The open issues are published split between the reader's and the
    /// others, again only when they change or after a `refresh`.
    #[tokio::test]
    async fn the_open_issues_publish_on_change_and_after_a_refresh() {
        let (mut core, fake) = core();
        core.subscribe(RequestId(1), "issues", &issues_params("https://forge/o/r"));
        let pending = core.subscribe(RequestId(2), "issues", &issues_params("https://forge/o/r"));
        assert!(published(&pending).is_empty(), "nothing read yet");
        let read = || {
            Ok(Reading::Issues(Issues {
                account: "me".into(),
                mine: vec![issue(52, true)],
                others: vec![issue(51, true)],
                truncated: true,
            }))
        };
        let out = published(&core.update(1, read(), Timestamp::now(), Instant::now()));
        let (flow, detail) = &out[0];
        assert_eq!(flow, "issues/forge/o");
        assert!(detail["source"]["as_of"].is_string());
        let mut detail = detail.clone();
        detail["source"]["as_of"] = Value::Null;
        assert_eq!(
            detail,
            json!({ "tracker": "forge/o", "account": "me",
                "mine": [{ "number": 52, "title": "issue 52", "type": "Bug", "url": "u/52" }],
                "others": [{ "number": 51, "title": "issue 51", "type": "Bug", "url": "u/51" }],
                "truncated": true, "source": { "status": "ok", "as_of": null } })
        );
        assert!(
            core.update(1, read(), Timestamp::now(), Instant::now())
                .is_empty()
        );
        core.refresh(&FlowId::new("issues/forge/o").unwrap());
        tokio::task::yield_now().await;
        assert_eq!(*fake.refreshes.lock().unwrap(), 1);
        let out = core.update(1, read(), Timestamp::now(), Instant::now());
        assert_eq!(published(&out).len(), 1, "unchanged, but refreshed");
    }

    /// An issue that is not found has null fields but its key's; once
    /// read, a failure keeps it.
    #[tokio::test]
    async fn an_issue_publishes_its_state_and_keeps_it_on_failure() {
        let (mut core, _) = core();
        core.subscribe(RequestId(1), "issue", &issue_params("https://forge/o/r", 7));
        let out =
            published(&core.update(1, Err(Failure::NotFound), Timestamp::now(), Instant::now()));
        assert_eq!(
            out[0].1,
            json!({ "tracker": "forge/o", "number": 7, "title": null, "type": null,
                "state": null, "url": null, "source": { "status": "not_found", "as_of": null } })
        );
        let read = Ok(Reading::Issue(issue(7, false)));
        let out = published(&core.update(1, read, Timestamp::now(), Instant::now()));
        let detail = &out[0].1;
        assert_eq!(detail["state"], "closed");
        assert_eq!(detail["title"], "issue 7");
        assert_eq!(detail["type"], "Bug");
        assert_eq!(detail["source"]["status"], "ok");
        let out = published(&core.update(
            1,
            Err(Failure::Unreachable),
            Timestamp::now(),
            Instant::now(),
        ));
        assert_eq!(out[0].1["source"]["status"], "unreachable");
        assert_eq!(out[0].1["state"], "closed", "the last reading stays");
    }

    #[tokio::test]
    async fn the_last_issue_flow_released_stops_its_following() {
        let (mut core, fake) = core();
        core.subscribe(RequestId(1), "issue", &issue_params("https://forge/o/r", 7));
        core.release(&FlowId::new("issue/forge/o#7").unwrap());
        let read = Ok(Reading::Issue(issue(7, true)));
        assert!(
            core.update(1, read, Timestamp::now(), Instant::now())
                .is_empty()
        );
        core.subscribe(RequestId(2), "issue", &issue_params("https://forge/o/r", 7));
        assert_eq!(fake.followed.lock().unwrap().len(), 2, "followed anew");
    }
}
