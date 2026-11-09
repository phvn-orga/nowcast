//! The forge brick, native to nowcast: a branch's PR and the CI of its head,
//! per pipeline and job, read from GitHub's public API. The `nowcast` binary
//! runs it as `nowcast brick forge`; its declaration and schemas are
//! [`FILES`].
//!
//! - `pr {remote, branch}`: the branch's PR, open first, and the CI of its
//!   head, or of the branch's head without an open PR.
//!
//! Each flow is read at its own pace ([`pace`]): fast while a CI is expected
//! or running, or for a short while after a `refresh`, every minute
//! otherwise. Requests are conditional ([`http`]), so a reading that changes
//! nothing costs no rate limit. The token comes from `gh`
//! ([`github::token`]); [`github::API_VAR`] and [`github::TOKEN_VAR`] replace
//! the API's address and the token, for tests.

mod ci;
mod github;
mod http;
mod pace;
mod remote;
#[cfg(test)]
mod testing;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use jiff::Timestamp;
use nowcast_codec::{read_service, write_brick};
use nowcast_types::{BrickMessage, FlowId, RequestId, ServiceMessage};
use serde_json::{Value, json};
use tokio::io::{Stdout, stdin, stdout};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::time::Instant;

use crate::github::Reading;
use crate::http::{Failure, Http};
use crate::pace::Pace;
use crate::remote::Repo;

/// The brick's name, as its declaration gives it.
pub const NAME: &str = "forge";

/// The declaration and its schemas, by path relative to the declaration's
/// directory.
pub const FILES: &[(&str, &str)] = &[
    (
        "brick.toml",
        include_str!(concat!(env!("OUT_DIR"), "/brick.toml")),
    ),
    (
        "schemas/params-pr.json",
        include_str!("../schemas/params-pr.json"),
    ),
    ("schemas/pr-v1.json", include_str!("../schemas/pr-v1.json")),
];

const PR_TYPE: &str = "forge.pr/v1";

/// Runs the brick on standard input and output until `shutdown` or the end
/// of its input. Standard error is its log.
pub fn run() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            log(format_args!("cannot start: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let status = runtime.block_on(serve());
    // Standard input is read on a blocking thread that may sit in a read.
    runtime.shutdown_background();
    status
}

fn log(message: impl std::fmt::Display) {
    eprintln!("{NAME}: {message}");
}

/// A `pr` flow.
struct Flow {
    repo: Repo,
    branch: String,
    pace: Pace,
    next: Instant,
    in_flight: bool,
    /// A `refresh` came during a reading: read again once it ends.
    again: bool,
    /// Publish after the next reading, even if nothing changed: a `refresh`
    /// is answered by a publication.
    force: bool,
    /// Tells a reading of this flow from one of an earlier flow of the same
    /// id, released since.
    generation: u64,
    /// The last successful reading.
    reading: Option<Reading>,
    status: &'static str,
    as_of: Option<Timestamp>,
    /// The detail last published, without its `as_of`.
    published: Option<Value>,
}

/// What a reading gave.
enum Outcome {
    Read(Reading),
    NoCredentials,
    Failed(Failure),
}

async fn serve() -> ExitCode {
    let api = std::env::var(github::API_VAR).unwrap_or_else(|_| github::API.to_owned());
    let http = match Http::new() {
        Ok(http) => http,
        Err(e) => {
            log(format_args!("cannot build the HTTP client: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let tokens: Arc<Mutex<HashMap<String, String>>> = Arc::default();

    let (from_service_tx, mut from_service) = unbounded_channel();
    tokio::spawn(async move {
        let mut input = stdin();
        loop {
            match read_service(&mut input).await {
                Ok(Some(msg)) => {
                    if from_service_tx.send(msg).is_err() {
                        return;
                    }
                }
                Ok(None) => return,
                Err(e) => {
                    log(format_args!("unreadable input: {e}"));
                    return;
                }
            }
        }
    });
    let mut output = stdout();
    let (done_tx, mut done) = unbounded_channel::<(FlowId, u64, Outcome)>();
    let mut flows: HashMap<FlowId, Flow> = HashMap::new();
    let (mut generation, mut last_req) = (0u64, 0u64);

    loop {
        let due = flows
            .values()
            .filter(|f| !f.in_flight)
            .map(|f| f.next)
            .min();
        tokio::select! {
            msg = from_service.recv() => {
                let Some(msg) = msg else { return ExitCode::SUCCESS };
                let now = Instant::now();
                let out = match msg {
                    ServiceMessage::Shutdown => return ExitCode::SUCCESS,
                    ServiceMessage::Hello { .. } => vec![BrickMessage::Ready],
                    ServiceMessage::Subscribe { req, kind, params } => {
                        generation += 1;
                        vec![subscribe(&mut flows, req, &kind, &params, now, generation)]
                    }
                    ServiceMessage::Release { flow } => {
                        flows.remove(&flow);
                        vec![]
                    }
                    ServiceMessage::Refresh { flow } => {
                        if let Some(f) = flows.get_mut(&flow) {
                            f.pace.refreshed(now);
                            f.force = true;
                            if f.in_flight {
                                f.again = true;
                            } else {
                                f.next = now;
                            }
                        }
                        vec![]
                    }
                    ServiceMessage::Error { req, code } => {
                        log(format_args!("publication {req} refused: {code:?}"));
                        vec![]
                    }
                    ServiceMessage::Input { .. } | ServiceMessage::Ack { .. } => vec![],
                };
                if !send(&mut output, out).await {
                    return ExitCode::FAILURE;
                }
            }
            () = sleep_until(due) => {
                let now = Instant::now();
                for (id, flow) in flows.iter_mut().filter(|(_, f)| !f.in_flight && f.next <= now) {
                    flow.in_flight = true;
                    spawn_reading(id.clone(), flow, &http, &api, &tokens, done_tx.clone());
                }
            }
            Some((id, generation, outcome)) = done.recv() => {
                let Some(flow) = flows.get_mut(&id).filter(|f| f.generation == generation) else { continue };
                let out = finish(&id, flow, outcome, Instant::now(), Timestamp::now(), &mut last_req);
                if !send(&mut output, out).await {
                    return ExitCode::FAILURE;
                }
            }
        }
    }
}

fn subscribe(
    flows: &mut HashMap<FlowId, Flow>,
    req: RequestId,
    kind: &str,
    params: &[u8],
    now: Instant,
    generation: u64,
) -> BrickMessage {
    let refuse = |code: &str, message: String| BrickMessage::Error {
        req,
        code: code.to_owned(),
        message,
    };
    if kind != "pr" {
        return refuse("unknown_kind", format!("no subscription kind {kind:?}"));
    }
    let params: Value = serde_json::from_slice(params).unwrap_or_default();
    let (Some(remote), Some(branch)) = (params["remote"].as_str(), params["branch"].as_str())
    else {
        return refuse("invalid_params", "remote and branch are required".into());
    };
    let Some(repo) = remote::recognise(remote, remote::ssh_hostname) else {
        return refuse(
            "unsupported_remote",
            format!("{remote:?} is not a repository on a forge the brick reads"),
        );
    };
    let flow = match FlowId::new(format!("{}#{branch}", repo.id())) {
        Ok(flow) => flow,
        Err(e) => return refuse("key_too_long", e.to_string()),
    };
    flows.entry(flow.clone()).or_insert_with(|| Flow {
        repo,
        branch: branch.to_owned(),
        pace: Pace::default(),
        next: now,
        in_flight: false,
        again: false,
        force: false,
        generation,
        reading: None,
        status: "pending",
        as_of: None,
        published: None,
    });
    BrickMessage::Subscribed { req, flow }
}

fn spawn_reading(
    id: FlowId,
    flow: &Flow,
    http: &Http,
    api: &str,
    tokens: &Arc<Mutex<HashMap<String, String>>>,
    done: UnboundedSender<(FlowId, u64, Outcome)>,
) {
    let Repo::GitHub { owner, name } = flow.repo.clone();
    let (branch, generation) = (flow.branch.clone(), flow.generation);
    let (http, api, tokens) = (http.clone(), api.to_owned(), tokens.clone());
    tokio::spawn(async move {
        let kept = tokens.lock().unwrap().get(&owner).cloned();
        let token = match kept {
            Some(token) => Some(token),
            None => github::token(&owner).await,
        };
        let outcome = match token {
            None => Outcome::NoCredentials,
            Some(token) => {
                tokens.lock().unwrap().insert(owner.clone(), token.clone());
                match github::read(&http, &api, &token, &owner, &name, &branch).await {
                    Ok(reading) => Outcome::Read(reading),
                    Err(failure) => {
                        if failure == Failure::Unauthorized {
                            tokens.lock().unwrap().remove(&owner);
                        }
                        Outcome::Failed(failure)
                    }
                }
            }
        };
        let _ = done.send((id, generation, outcome));
    });
}

/// Takes a reading in, plans the next one, and publishes when the detail
/// changed or a `refresh` asked for it.
fn finish(
    id: &FlowId,
    flow: &mut Flow,
    outcome: Outcome,
    now: Instant,
    at: Timestamp,
    last_req: &mut u64,
) -> Vec<BrickMessage> {
    flow.in_flight = false;
    let mut wait = None;
    match outcome {
        Outcome::Read(reading) => {
            flow.pace.saw(
                now,
                reading.head_sha.as_deref(),
                reading
                    .pr
                    .as_ref()
                    .filter(|p| p.state == "open")
                    .map(|p| p.number),
            );
            flow.reading = Some(reading);
            flow.status = "ok";
            flow.as_of = Some(at);
        }
        Outcome::NoCredentials => flow.status = "no_credentials",
        Outcome::Failed(failure) => {
            flow.status = match &failure {
                Failure::Unauthorized => "unauthorized",
                Failure::NotFound => "not_found",
                Failure::Limited(retry) => {
                    wait = Some(retry.unwrap_or(pace::GITHUB.normal));
                    "rate_limited"
                }
                Failure::Unreadable(why) => {
                    log(format_args!("{}: {why}", id.as_str()));
                    "unreadable"
                }
                Failure::Unreachable(why) => {
                    log(format_args!("{}: unreachable: {why}", id.as_str()));
                    "unreachable"
                }
            };
        }
    }
    let ci = flow.reading.as_ref().and_then(|r| r.ci.as_ref());
    let pipelines = ci.map_or(0, |c| c.pipelines.len());
    let running = ci.is_some_and(|c| c.is_active());
    let mut delay = flow.pace.next(&pace::GITHUB, now, pipelines, running);
    if let Some(wait) = wait {
        delay = delay.max(wait);
    }
    flow.next = if std::mem::take(&mut flow.again) {
        now
    } else {
        now + delay
    };

    let detail = detail(flow, flow.pace.expects(now, pipelines));
    let force = std::mem::take(&mut flow.force);
    if !force && flow.published.as_ref() == Some(&detail) {
        return vec![];
    }
    flow.published = Some(detail.clone());
    let mut detail = detail;
    detail["source"]["as_of"] = json!(flow.as_of.map(|t| t.to_string()));
    *last_req += 1;
    vec![BrickMessage::Publish {
        req: RequestId(*last_req),
        flow: id.clone(),
        data_type: PR_TYPE.to_owned(),
        detail: detail.to_string().into_bytes(),
    }]
}

/// `forge.pr/v1`, without its `as_of`.
fn detail(flow: &Flow, expected: bool) -> Value {
    let reading = flow.reading.as_ref();
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
        "repo": flow.repo.id(),
        "branch": flow.branch,
        "head_sha": reading.and_then(|r| r.head_sha.as_deref()),
        "pr": pr,
        "ci": reading.and_then(|r| r.ci.as_ref()).map(|c| c.to_json(expected)),
        "source": { "status": flow.status, "as_of": null },
    })
}

/// Writes the brick's messages. `false` once the service is gone.
async fn send(output: &mut Stdout, messages: Vec<BrickMessage>) -> bool {
    for msg in messages {
        if let Err(e) = write_brick(output, &msg).await {
            log(format_args!("cannot write to the service: {e}"));
            return false;
        }
    }
    true
}

async fn sleep_until(due: Option<Instant>) {
    match due {
        Some(due) => tokio::time::sleep_until(due).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::ci::{Ci, State};
    use crate::github::Pr;

    fn subscribed(flows: &mut HashMap<FlowId, Flow>, remote: &str) -> BrickMessage {
        let params = json!({ "remote": remote, "branch": "feat" }).to_string();
        subscribe(
            flows,
            RequestId(1),
            "pr",
            params.as_bytes(),
            Instant::now(),
            1,
        )
    }

    #[test]
    fn two_spellings_share_a_flow_and_others_are_refused() {
        let mut flows = HashMap::new();
        let a = subscribed(&mut flows, "git@github.com:O/R.git");
        let b = subscribed(&mut flows, "https://github.com/o/r");
        let flow = FlowId::new("github.com/o/r#feat").unwrap();
        assert_eq!(
            a,
            BrickMessage::Subscribed {
                req: RequestId(1),
                flow: flow.clone()
            }
        );
        assert_eq!(a, b);
        assert_eq!(flows.len(), 1);
        assert!(matches!(subscribed(&mut flows, "https://example.com/o/r"),
            BrickMessage::Error { code, .. } if code == "unsupported_remote"));
        assert!(
            matches!(subscribe(&mut flows, RequestId(2), "issues", b"{}", Instant::now(), 2),
            BrickMessage::Error { code, .. } if code == "unknown_kind")
        );
    }

    fn reading(sha: &str, state: State) -> Outcome {
        Outcome::Read(Reading {
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
            ci: Some(Ci {
                pipelines: vec![crate::ci::tests::pipeline(state, vec![])],
                missing_required: vec![],
            }),
        })
    }

    fn published(out: &[BrickMessage]) -> Option<Value> {
        match out {
            [BrickMessage::Publish { detail, .. }] => Some(serde_json::from_slice(detail).unwrap()),
            [] => None,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_reading_publishes_only_what_changed_unless_refreshed() {
        let mut flows = HashMap::new();
        subscribed(&mut flows, "https://github.com/o/r");
        let id = FlowId::new("github.com/o/r#feat").unwrap();
        let flow = flows.get_mut(&id).unwrap();
        let (now, mut req) = (Instant::now(), 0);
        let first = published(&finish(
            &id,
            flow,
            reading("a", State::Running),
            now,
            Timestamp::now(),
            &mut req,
        ))
        .unwrap();
        assert_eq!(first["pr"]["number"], 7);
        assert_eq!(first["ci"]["state"], "running");
        assert_eq!(first["source"]["status"], "ok");
        assert!(first["source"]["as_of"].is_string());
        assert_eq!(flow.next, now + pace::GITHUB.running);
        let later = Timestamp::now() + jiff::SignedDuration::from_secs(5);
        assert_eq!(
            published(&finish(
                &id,
                flow,
                reading("a", State::Running),
                now,
                later,
                &mut req
            )),
            None,
            "only as_of moved"
        );
        flow.force = true;
        assert!(
            published(&finish(
                &id,
                flow,
                reading("a", State::Running),
                now,
                later,
                &mut req
            ))
            .is_some()
        );
        let failed = published(&finish(
            &id,
            flow,
            Outcome::Failed(Failure::Unreachable("x".into())),
            now,
            later,
            &mut req,
        ))
        .unwrap();
        assert_eq!(failed["source"]["status"], "unreachable");
        assert_eq!(failed["pr"]["number"], 7, "the last reading stays");
    }

    #[test]
    fn a_new_head_without_pipeline_reads_fast_and_rolls_up_running() {
        let mut flows = HashMap::new();
        subscribed(&mut flows, "https://github.com/o/r");
        let id = FlowId::new("github.com/o/r#feat").unwrap();
        let flow = flows.get_mut(&id).unwrap();
        let (now, mut req) = (Instant::now(), 0);
        finish(
            &id,
            flow,
            reading("a", State::Pass),
            now,
            Timestamp::now(),
            &mut req,
        );
        let mut pushed = reading("b", State::Pass);
        if let Outcome::Read(r) = &mut pushed {
            r.ci = Some(Ci::default());
        }
        let detail =
            published(&finish(&id, flow, pushed, now, Timestamp::now(), &mut req)).unwrap();
        assert_eq!(
            detail["ci"]["state"], "running",
            "a CI is expected on the new head"
        );
        assert_eq!(flow.next, now + pace::GITHUB.expected_first);
    }

    #[test]
    fn a_rate_limit_waits_as_asked() {
        let mut flows = HashMap::new();
        subscribed(&mut flows, "https://github.com/o/r");
        let id = FlowId::new("github.com/o/r#feat").unwrap();
        let flow = flows.get_mut(&id).unwrap();
        let (now, mut req) = (Instant::now(), 0);
        let limited = Outcome::Failed(Failure::Limited(Some(Duration::from_secs(300))));
        let detail =
            published(&finish(&id, flow, limited, now, Timestamp::now(), &mut req)).unwrap();
        assert_eq!(detail["source"]["status"], "rate_limited");
        assert_eq!(detail["pr"], Value::Null);
        assert_eq!(flow.next, now + Duration::from_secs(300));
    }
}
