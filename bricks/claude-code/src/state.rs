//! What the brick holds and publishes, without I/O: each message from the
//! service, each measure and each reading gives the messages to send.

use std::collections::{BTreeMap, BTreeSet};

use jiff::{SignedDuration, Timestamp};
use nowcast_types::{BrickMessage, FlowId, RequestId, ServiceMessage};
use serde_json::{Map, Value, json};

use crate::quotas::{ApiStatus, Quotas, Reading, Source, windows_of};

pub const QUOTAS_TYPE: &str = "claude-code.quotas/v1";
pub const SESSION_TYPE: &str = "claude-code.session/v1";
pub const SESSIONS_TYPE: &str = "claude-code.sessions/v1";

const QUOTAS: &str = "quotas";
const SESSIONS: &str = "sessions";
const SESSION_PREFIX: &str = "session/";
const SESSIONS_PREFIX: &str = "sessions/";

/// The cache lifetime assumed when the session has not said: a Claude
/// subscription's.
const DEFAULT_TTL: SignedDuration = SignedDuration::from_hours(1);

/// What a flow id stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Flow {
    Quotas,
    /// A session's own flow, where it sends its measures.
    Session(String),
    Sessions,
    /// The reading of one session.
    OneSession(String),
}

impl Flow {
    fn parse(flow: &FlowId) -> Option<Self> {
        let id = flow.as_str();
        match id {
            QUOTAS => Some(Self::Quotas),
            SESSIONS => Some(Self::Sessions),
            _ => id
                .strip_prefix(SESSION_PREFIX)
                .map(|s| Self::Session(s.to_owned()))
                .or_else(|| {
                    id.strip_prefix(SESSIONS_PREFIX)
                        .map(|s| Self::OneSession(s.to_owned()))
                }),
        }
    }

    fn id(&self) -> String {
        match self {
            Self::Quotas => QUOTAS.into(),
            Self::Sessions => SESSIONS.into(),
            Self::Session(s) => format!("{SESSION_PREFIX}{s}"),
            Self::OneSession(s) => format!("{SESSIONS_PREFIX}{s}"),
        }
    }
}

#[derive(Default)]
pub struct State {
    /// The flows subscribed and not released.
    flows: BTreeSet<String>,
    /// Running sessions that sent a measure, by id: their entry.
    sessions: BTreeMap<String, Map<String, Value>>,
    /// Sessions that ended while someone still reads them alone.
    ended: BTreeMap<String, Map<String, Value>>,
    quotas: Quotas,
    last_req: u64,
}

impl State {
    /// Whether the quotas flow has a subscriber: the usage API is read only
    /// then.
    pub fn wants_quotas(&self) -> bool {
        self.flows.contains(QUOTAS)
    }

    pub fn quotas(&self) -> &Quotas {
        &self.quotas
    }

    /// Answers a message from the service. `now` dates what it changes.
    pub fn on_service(&mut self, msg: ServiceMessage, now: Timestamp) -> Vec<BrickMessage> {
        match msg {
            ServiceMessage::Hello { .. } => vec![BrickMessage::Ready],
            ServiceMessage::Subscribe { req, kind, params } => self.subscribe(req, &kind, &params),
            ServiceMessage::Release { flow } => self.release(&flow, now),
            ServiceMessage::Refresh { flow } => match Flow::parse(&flow) {
                // The usage API is read again by the shell.
                Some(Flow::Quotas) => vec![],
                Some(flow) => self.republish(&flow),
                None => vec![],
            },
            ServiceMessage::Input { flow, detail, .. } => match Flow::parse(&flow) {
                Some(Flow::Session(id)) if self.flows.contains(flow.as_str()) => {
                    match serde_json::from_slice::<Value>(&detail) {
                        Ok(Value::Object(measure)) => self.measure(&id, &measure, now),
                        _ => vec![],
                    }
                }
                _ => vec![],
            },
            ServiceMessage::Shutdown
            | ServiceMessage::Ack { .. }
            | ServiceMessage::Error { .. } => {
                vec![]
            }
        }
    }

    /// The usage API answered `answer`, read at `as_of`.
    pub fn on_answer(
        &mut self,
        answer: &Value,
        as_of: Timestamp,
        now: Timestamp,
    ) -> Vec<BrickMessage> {
        let Some(windows) = windows_of(answer) else {
            return self.on_status(ApiStatus::Unreadable, now);
        };
        let mut changed = self.quotas.set_status(ApiStatus::Ok, as_of);
        for (name, percent, resets_at) in windows {
            let reading = Reading {
                percent,
                resets_at,
                as_of,
                source: Source::UsageApi,
            };
            changed |= self.quotas.offer(&name, reading);
        }
        self.quotas_if(changed)
    }

    /// Claude Code's own copy of an answer, fetched at `fetched`: its windows
    /// count where they are fresher, the API's status is left as is.
    pub fn on_copy(&mut self, answer: &Value, fetched: Timestamp) -> Vec<BrickMessage> {
        let Some(windows) = windows_of(answer) else {
            return vec![];
        };
        let mut changed = false;
        for (name, percent, resets_at) in windows {
            let reading = Reading {
                percent,
                resets_at,
                as_of: fetched,
                source: Source::UsageApi,
            };
            changed |= self.quotas.offer(&name, reading);
        }
        self.quotas_if(changed)
    }

    /// The usage API could not be read, for this reason.
    pub fn on_status(&mut self, status: ApiStatus, now: Timestamp) -> Vec<BrickMessage> {
        let changed = self.quotas.set_status(status, now);
        self.quotas_if(changed)
    }

    fn subscribe(&mut self, req: RequestId, kind: &str, params: &[u8]) -> Vec<BrickMessage> {
        let params: Value = serde_json::from_slice(params).unwrap_or(Value::Null);
        let id = params.get("id").and_then(Value::as_str).map(str::to_owned);
        let flow = match (kind, id) {
            ("quotas", _) => Flow::Quotas,
            ("session", Some(id)) => Flow::Session(id),
            ("sessions", None) => Flow::Sessions,
            ("sessions", Some(id)) => Flow::OneSession(id),
            _ => {
                return vec![BrickMessage::Error {
                    req,
                    code: "invalid_params".into(),
                    message: format!("no flow for kind {kind:?} with {params}"),
                }];
            }
        };
        let id = match FlowId::new(flow.id()) {
            Ok(id) => id,
            Err(e) => {
                return vec![BrickMessage::Error {
                    req,
                    code: "invalid_params".into(),
                    message: e.to_string(),
                }];
            }
        };
        self.flows.insert(flow.id());
        let mut out = vec![BrickMessage::Subscribed { req, flow: id }];
        out.extend(self.republish(&flow));
        out
    }

    fn release(&mut self, flow: &FlowId, now: Timestamp) -> Vec<BrickMessage> {
        self.flows.remove(flow.as_str());
        match Flow::parse(flow) {
            Some(Flow::Session(id)) => {
                let Some(mut entry) = self.sessions.remove(&id) else {
                    return vec![];
                };
                entry.insert("ended_at".into(), now.to_string().into());
                let mut out = self.publish_list();
                if self.reads_one(&id) {
                    out.extend(self.publish(
                        &Flow::OneSession(id.clone()),
                        SESSION_TYPE,
                        Value::Object(entry.clone()),
                    ));
                    self.ended.insert(id, entry);
                }
                out
            }
            Some(Flow::OneSession(id)) => {
                self.ended.remove(&id);
                vec![]
            }
            _ => vec![],
        }
    }

    /// A session sent a measure: its entry takes every field given.
    fn measure(
        &mut self,
        id: &str,
        measure: &Map<String, Value>,
        now: Timestamp,
    ) -> Vec<BrickMessage> {
        let entry = self
            .sessions
            .entry(id.to_owned())
            .or_insert_with(|| empty_entry(id));
        for key in [
            "root",
            "model",
            "effort",
            "context",
            "tokens",
            "last_request_at",
            "cache_ttl",
            "cost_usd",
        ] {
            if let Some(value) = measure.get(key) {
                entry.insert(key.into(), value.clone());
            }
        }
        entry.insert("updated_at".into(), now.to_string().into());
        let expires = cache_expires_at(entry);
        entry.insert(
            "cache_expires_at".into(),
            expires.map_or(Value::Null, |t| t.to_string().into()),
        );

        let mut changed = false;
        for limit in measure
            .get("rate_limits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let (Some(name), Some(percent)) = (
                limit.get("name").and_then(Value::as_str),
                limit.get("percent").and_then(Value::as_f64),
            ) else {
                continue;
            };
            let reading = Reading {
                percent,
                resets_at: limit
                    .get("resets_at")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                as_of: now,
                source: Source::Session,
            };
            changed |= self.quotas.offer(name, reading);
        }

        let entry = Value::Object(self.sessions[id].clone());
        let mut out = self.publish(&Flow::Session(id.to_owned()), SESSION_TYPE, entry.clone());
        if self.reads_one(id) {
            out.extend(self.publish(&Flow::OneSession(id.to_owned()), SESSION_TYPE, entry));
        }
        out.extend(self.publish_list());
        out.extend(self.quotas_if(changed));
        out
    }

    /// Publishes what a flow holds now, if it holds something.
    fn republish(&mut self, flow: &Flow) -> Vec<BrickMessage> {
        match flow {
            Flow::Quotas => {
                let detail = self.quotas.detail();
                self.publish(flow, QUOTAS_TYPE, detail)
            }
            Flow::Sessions => self.publish_list(),
            Flow::Session(id) => match self.sessions.get(id) {
                Some(entry) => {
                    let entry = Value::Object(entry.clone());
                    self.publish(flow, SESSION_TYPE, entry)
                }
                None => vec![],
            },
            Flow::OneSession(id) => match self.sessions.get(id).or_else(|| self.ended.get(id)) {
                Some(entry) => {
                    let entry = Value::Object(entry.clone());
                    self.publish(flow, SESSION_TYPE, entry)
                }
                None => vec![],
            },
        }
    }

    fn publish_list(&mut self) -> Vec<BrickMessage> {
        let sessions: Vec<Value> = self.sessions.values().cloned().map(Value::Object).collect();
        self.publish(
            &Flow::Sessions,
            SESSIONS_TYPE,
            json!({ "sessions": sessions }),
        )
    }

    fn quotas_if(&mut self, changed: bool) -> Vec<BrickMessage> {
        if !changed {
            return vec![];
        }
        let detail = self.quotas.detail();
        self.publish(&Flow::Quotas, QUOTAS_TYPE, detail)
    }

    fn reads_one(&self, id: &str) -> bool {
        self.flows.contains(&Flow::OneSession(id.to_owned()).id())
    }

    /// A `publish` on `flow`, if it is subscribed.
    fn publish(&mut self, flow: &Flow, data_type: &str, detail: Value) -> Vec<BrickMessage> {
        let id = flow.id();
        if !self.flows.contains(&id) {
            return vec![];
        }
        let Ok(flow) = FlowId::new(id) else {
            return vec![];
        };
        self.last_req += 1;
        vec![BrickMessage::Publish {
            req: RequestId(self.last_req),
            flow,
            data_type: data_type.into(),
            detail: detail.to_string().into_bytes(),
        }]
    }
}

fn empty_entry(id: &str) -> Map<String, Value> {
    let mut entry = Map::new();
    entry.insert("session".into(), id.into());
    for key in [
        "updated_at",
        "ended_at",
        "root",
        "model",
        "effort",
        "context",
        "tokens",
        "last_request_at",
        "cache_ttl",
        "cache_expires_at",
        "cost_usd",
    ] {
        entry.insert(key.into(), Value::Null);
    }
    entry
}

/// `last_request_at` plus `cache_ttl`, one hour when it is unknown.
fn cache_expires_at(entry: &Map<String, Value>) -> Option<Timestamp> {
    let last: Timestamp = entry.get("last_request_at")?.as_str()?.parse().ok()?;
    let ttl = match entry.get("cache_ttl").and_then(Value::as_str) {
        Some("5m") => SignedDuration::from_mins(5),
        _ => DEFAULT_TTL,
    };
    last.checked_add(ttl).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: i64) -> Timestamp {
        Timestamp::from_second(1_800_000_000 + s).unwrap()
    }

    fn flow(id: &str) -> FlowId {
        FlowId::new(id).unwrap()
    }

    fn subscribe(state: &mut State, kind: &str, params: Value) -> Vec<BrickMessage> {
        state.on_service(
            ServiceMessage::Subscribe {
                req: RequestId(1),
                kind: kind.into(),
                params: params.to_string().into_bytes(),
            },
            at(0),
        )
    }

    fn input(state: &mut State, id: &str, measure: Value, s: i64) -> Vec<BrickMessage> {
        state.on_service(
            ServiceMessage::Input {
                flow: flow(&format!("session/{id}")),
                data_type: "claude-code.measure/v1".into(),
                detail: measure.to_string().into_bytes(),
            },
            at(s),
        )
    }

    /// The publications among `out`, as (flow, detail).
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

    #[test]
    fn subscriptions_map_to_their_flows() {
        let mut state = State::default();
        for (kind, params, expected) in [
            ("quotas", json!({}), "quotas"),
            ("session", json!({ "id": "s1" }), "session/s1"),
            ("sessions", json!({}), "sessions"),
            ("sessions", json!({ "id": "s1" }), "sessions/s1"),
        ] {
            let out = subscribe(&mut state, kind, params);
            assert_eq!(
                out[0],
                BrickMessage::Subscribed {
                    req: RequestId(1),
                    flow: flow(expected)
                },
                "{kind}"
            );
        }
        assert!(state.wants_quotas());
        let refused = subscribe(&mut state, "session", json!({}));
        assert!(
            matches!(&refused[..], [BrickMessage::Error { code, .. }] if code == "invalid_params")
        );
    }

    #[test]
    fn a_measure_reaches_the_session_its_readers_and_the_list() {
        let mut state = State::default();
        subscribe(&mut state, "session", json!({ "id": "s1" }));
        subscribe(&mut state, "sessions", json!({ "id": "s1" }));
        let list = subscribe(&mut state, "sessions", json!({}));
        assert_eq!(
            published(&list),
            vec![("sessions".into(), json!({ "sessions": [] }))]
        );

        let out = input(
            &mut state,
            "s1",
            json!({
                "model": "claude-opus-5-5",
                "context": { "tokens": 1000, "window": 200000, "percent": 1 },
                "tokens": { "input": 1, "output": 2, "cache_creation": 3, "cache_read": 4 },
                "last_request_at": "2026-11-07T15:31:10Z",
                "cache_ttl": "5m",
            }),
            5,
        );
        let out = published(&out);
        let flows: Vec<&str> = out.iter().map(|(f, _)| f.as_str()).collect();
        assert_eq!(flows, ["session/s1", "sessions/s1", "sessions"]);
        let entry = &out[0].1;
        assert_eq!(entry["session"], "s1");
        assert_eq!(entry["model"], "claude-opus-5-5");
        assert_eq!(entry["tokens"]["cache_read"], 4);
        assert_eq!(entry["updated_at"], at(5).to_string());
        assert_eq!(entry["ended_at"], Value::Null);
        assert_eq!(entry["cache_expires_at"], "2026-11-07T15:36:10Z");
        assert_eq!(entry["effort"], Value::Null);
        assert_eq!(out[2].1, json!({ "sessions": [entry] }));

        // A later measure keeps what it does not repeat.
        let out = published(&input(&mut state, "s1", json!({ "effort": "high" }), 6));
        assert_eq!(out[0].1["model"], "claude-opus-5-5");
        assert_eq!(out[0].1["effort"], "high");
    }

    #[test]
    fn the_cache_lasts_an_hour_unless_told() {
        let mut state = State::default();
        subscribe(&mut state, "session", json!({ "id": "s1" }));
        let out = published(&input(
            &mut state,
            "s1",
            json!({ "last_request_at": "2026-11-07T15:31:10Z" }),
            1,
        ));
        assert_eq!(out[0].1["cache_expires_at"], "2026-11-07T16:31:10Z");
    }

    #[test]
    fn the_end_of_a_session_leaves_the_list_and_tells_its_reader() {
        let mut state = State::default();
        subscribe(&mut state, "session", json!({ "id": "s1" }));
        subscribe(&mut state, "sessions", json!({ "id": "s1" }));
        subscribe(&mut state, "sessions", json!({}));
        input(&mut state, "s1", json!({ "model": "m" }), 1);
        let out = published(&state.on_service(
            ServiceMessage::Release {
                flow: flow("session/s1"),
            },
            at(9),
        ));
        assert_eq!(out[0], ("sessions".into(), json!({ "sessions": [] })));
        assert_eq!(out[1].0, "sessions/s1");
        assert_eq!(out[1].1["ended_at"], at(9).to_string());
        // A reader that comes later still gets the ended entry.
        let again = published(&subscribe(&mut state, "sessions", json!({ "id": "s1" })));
        assert_eq!(again[0].1["ended_at"], at(9).to_string());
    }

    #[test]
    fn a_reader_of_an_unknown_session_gets_nothing_yet() {
        let mut state = State::default();
        let out = subscribe(&mut state, "sessions", json!({ "id": "nobody" }));
        assert_eq!(published(&out), vec![]);
    }

    #[test]
    fn an_input_on_a_flow_not_held_is_ignored() {
        let mut state = State::default();
        assert_eq!(input(&mut state, "s1", json!({ "model": "m" }), 1), vec![]);
    }

    #[test]
    fn session_readings_update_the_quotas() {
        let mut state = State::default();
        let first = published(&subscribe(&mut state, "quotas", json!({})));
        assert_eq!(first[0].1["usage_api"]["status"], "pending");
        subscribe(&mut state, "session", json!({ "id": "s1" }));
        let limits = json!({ "rate_limits": [{ "name": "five_hour", "percent": 23.5, "resets_at": "2026-11-07T18:00:00Z" }] });
        let out = published(&input(&mut state, "s1", limits.clone(), 3));
        let quotas = &out.last().unwrap().1;
        assert_eq!(out.last().unwrap().0, "quotas");
        assert_eq!(
            quotas["windows"],
            json!([{ "name": "five_hour", "percent": 23.5, "resets_at": "2026-11-07T18:00:00Z", "as_of": at(3).to_string(), "source": "session" }])
        );
        // The same reading again publishes the session, not the quotas.
        let again = published(&input(&mut state, "s1", limits, 4));
        assert!(again.iter().all(|(f, _)| f != "quotas"));
    }

    #[test]
    fn usage_api_answers_and_failures_reach_the_quotas() {
        let mut state = State::default();
        subscribe(&mut state, "quotas", json!({}));
        let answer = json!({ "five_hour": { "utilization": 13, "resets_at": null } });
        let out = published(&state.on_answer(&answer, at(1), at(1)));
        assert_eq!(
            out[0].1["usage_api"],
            json!({ "status": "ok", "as_of": at(1).to_string() })
        );
        assert_eq!(out[0].1["windows"][0]["source"], "usage-api");
        let out = published(&state.on_status(ApiStatus::TokenExpired, at(2)));
        assert_eq!(out[0].1["usage_api"]["status"], "token_expired");
        assert_eq!(state.on_status(ApiStatus::TokenExpired, at(3)), vec![]);
        let out = published(&state.on_answer(&json!("nope"), at(4), at(4)));
        assert_eq!(out[0].1["usage_api"]["status"], "unreadable");
    }
}
