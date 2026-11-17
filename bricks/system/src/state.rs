//! What the brick answers the service, without I/O: the `machine` flow and
//! one `process/<pid>` flow per anchor, each published when its detail
//! changes and after every `refresh`.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use nowcast_types::{BrickMessage, FlowId, RequestId, ServiceMessage};
use serde_json::Value;

pub const MACHINE_TYPE: &str = "system.machine/v1";
pub const PROCESS_TYPE: &str = "system.process/v1";
const MACHINE: &str = "machine";
const PROCESS: &str = "process";

/// What a flow measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    Machine,
    /// The tree of this pid, the anchor of the flow's subscribers.
    Process(u32),
}

impl Target {
    fn flow(self) -> FlowId {
        let id = match self {
            Self::Machine => MACHINE.to_owned(),
            Self::Process(pid) => format!("{PROCESS}/{pid}"),
        };
        FlowId::new(id).expect("a short flow id")
    }

    /// The target whose flow is exactly `flow`: `process/077` names none.
    fn parse(flow: &FlowId) -> Option<Self> {
        let target = match flow.as_str() {
            MACHINE => Self::Machine,
            id => Self::Process(id.strip_prefix(PROCESS)?.strip_prefix('/')?.parse().ok()?),
        };
        (target.flow() == *flow).then_some(target)
    }

    fn data_type(self) -> &'static str {
        match self {
            Self::Machine => MACHINE_TYPE,
            Self::Process(_) => PROCESS_TYPE,
        }
    }
}

/// What the shell does with a flow's sampler after a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sampling {
    Unchanged,
    /// The flow opened: start sampling it.
    Start(Target),
    /// The flow was released: stop.
    Stop(Target),
    /// A `refresh`: sample it now.
    Now(Target),
}

/// A flow being served.
#[derive(Default)]
struct Served {
    /// The last detail published, as sent.
    last: Option<Vec<u8>>,
    /// The next sample publishes even unchanged.
    forced: bool,
}

#[derive(Default)]
pub struct State {
    flows: HashMap<Target, Served>,
    last_req: u64,
}

impl State {
    pub fn on_service(&mut self, msg: ServiceMessage) -> (Vec<BrickMessage>, Sampling) {
        match msg {
            ServiceMessage::Hello { .. } => (vec![BrickMessage::Ready], Sampling::Unchanged),
            ServiceMessage::Subscribe {
                req, kind, anchor, ..
            } => {
                let target = match kind.as_str() {
                    MACHINE => Target::Machine,
                    PROCESS => Target::Process(anchor),
                    _ => {
                        let error = BrickMessage::Error {
                            req,
                            code: "unknown_kind".into(),
                            message: format!("no subscription kind {kind:?}"),
                        };
                        return (vec![error], Sampling::Unchanged);
                    }
                };
                let subscribed = BrickMessage::Subscribed {
                    req,
                    flow: target.flow(),
                };
                let sampling = match self.flows.entry(target) {
                    Entry::Occupied(_) => Sampling::Unchanged,
                    Entry::Vacant(vacant) => {
                        vacant.insert(Served::default());
                        Sampling::Start(target)
                    }
                };
                (vec![subscribed], sampling)
            }
            ServiceMessage::Release { flow } => match Target::parse(&flow) {
                Some(target) if self.flows.remove(&target).is_some() => {
                    (vec![], Sampling::Stop(target))
                }
                _ => (vec![], Sampling::Unchanged),
            },
            ServiceMessage::Refresh { flow } => {
                match Target::parse(&flow).and_then(|t| Some((t, self.flows.get_mut(&t)?))) {
                    Some((target, served)) => {
                        served.forced = true;
                        (vec![], Sampling::Now(target))
                    }
                    None => (vec![], Sampling::Unchanged),
                }
            }
            // The brick declares no refresh scope and accepts no input.
            _ => (vec![], Sampling::Unchanged),
        }
    }

    /// A sample of `target`: published when it differs from the last one,
    /// or after a `refresh`.
    pub fn on_sample(&mut self, target: Target, detail: &Value) -> Vec<BrickMessage> {
        let Some(served) = self.flows.get_mut(&target) else {
            return vec![];
        };
        let bytes = serde_json::to_vec(detail).expect("a JSON value serialises");
        let forced = std::mem::take(&mut served.forced);
        if !forced && served.last.as_ref() == Some(&bytes) {
            return vec![];
        }
        served.last = Some(bytes.clone());
        self.last_req += 1;
        vec![BrickMessage::Publish {
            req: RequestId(self.last_req),
            flow: target.flow(),
            data_type: target.data_type().into(),
            detail: bytes,
        }]
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn subscribe(state: &mut State, kind: &str) -> (Vec<BrickMessage>, Sampling) {
        subscribe_from(state, kind, 4242)
    }

    fn subscribe_from(state: &mut State, kind: &str, anchor: u32) -> (Vec<BrickMessage>, Sampling) {
        state.on_service(ServiceMessage::Subscribe {
            req: RequestId(1),
            kind: kind.into(),
            params: b"{}".to_vec(),
            anchor,
        })
    }

    fn published(messages: &[BrickMessage], target: Target) -> Vec<Value> {
        messages
            .iter()
            .map(|m| match m {
                BrickMessage::Publish {
                    detail,
                    data_type,
                    flow,
                    ..
                } => {
                    assert_eq!(data_type, target.data_type());
                    assert_eq!(flow, &target.flow());
                    serde_json::from_slice(detail).unwrap()
                }
                other => panic!("expected publish, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn the_machine_flow_starts_sampling_once() {
        let mut state = State::default();
        let (out, sampling) = subscribe(&mut state, "machine");
        assert_eq!(
            out,
            [BrickMessage::Subscribed {
                req: RequestId(1),
                flow: FlowId::new("machine").unwrap()
            }]
        );
        assert_eq!(sampling, Sampling::Start(Target::Machine));
        assert_eq!(subscribe(&mut state, "machine").1, Sampling::Unchanged);
    }

    #[test]
    fn a_process_flow_is_the_anchors() {
        let mut state = State::default();
        let (out, sampling) = subscribe_from(&mut state, "process", 77);
        assert_eq!(
            out,
            [BrickMessage::Subscribed {
                req: RequestId(1),
                flow: FlowId::new("process/77").unwrap()
            }]
        );
        assert_eq!(sampling, Sampling::Start(Target::Process(77)));
        // The same anchor shares its flow; another gets its own.
        assert_eq!(
            subscribe_from(&mut state, "process", 77).1,
            Sampling::Unchanged
        );
        assert_eq!(
            subscribe_from(&mut state, "process", 78).1,
            Sampling::Start(Target::Process(78))
        );
    }

    #[test]
    fn an_unknown_kind_is_refused() {
        let mut state = State::default();
        let (out, sampling) = subscribe(&mut state, "processes");
        assert!(matches!(&out[..], [BrickMessage::Error { code, .. }] if code == "unknown_kind"));
        assert_eq!(sampling, Sampling::Unchanged);
    }

    #[test]
    fn a_sample_publishes_only_what_changed_unless_refreshed() {
        let mut state = State::default();
        subscribe(&mut state, "machine");
        let m = Target::Machine;
        let a = json!({ "cpu": { "usage": 3 } });
        let b = json!({ "cpu": { "usage": 4 } });
        assert_eq!(
            published(&state.on_sample(m, &a), m),
            std::slice::from_ref(&a)
        );
        assert!(state.on_sample(m, &a).is_empty());
        assert_eq!(
            published(&state.on_sample(m, &b), m),
            std::slice::from_ref(&b)
        );
        let (out, sampling) = state.on_service(ServiceMessage::Refresh { flow: m.flow() });
        assert!(out.is_empty());
        assert_eq!(sampling, Sampling::Now(m));
        assert_eq!(
            published(&state.on_sample(m, &b), m),
            std::slice::from_ref(&b)
        );
        assert!(state.on_sample(m, &b).is_empty());
    }

    #[test]
    fn flows_publish_apart() {
        let mut state = State::default();
        subscribe(&mut state, "machine");
        subscribe_from(&mut state, "process", 77);
        let (m, p) = (Target::Machine, Target::Process(77));
        let a = json!({ "cpu": { "usage": 3 } });
        assert_eq!(
            published(&state.on_sample(m, &a), m),
            std::slice::from_ref(&a)
        );
        assert_eq!(
            published(&state.on_sample(p, &a), p),
            std::slice::from_ref(&a)
        );
        assert!(
            state.on_sample(Target::Process(78), &a).is_empty(),
            "never subscribed"
        );
    }

    #[test]
    fn a_release_stops_and_the_next_flow_publishes_afresh() {
        let mut state = State::default();
        subscribe_from(&mut state, "process", 77);
        let p = Target::Process(77);
        let a = json!({ "cpu": {} });
        state.on_sample(p, &a);
        let (_, sampling) = state.on_service(ServiceMessage::Release { flow: p.flow() });
        assert_eq!(sampling, Sampling::Stop(p));
        assert!(state.on_sample(p, &a).is_empty());
        assert_eq!(
            subscribe_from(&mut state, "process", 77).1,
            Sampling::Start(p)
        );
        assert_eq!(published(&state.on_sample(p, &a), p), [a]);
    }

    #[test]
    fn flows_never_served_are_ignored() {
        let mut state = State::default();
        // `process/077` and `process/+77` are not `process/77`'s flow, served.
        subscribe_from(&mut state, "process", 77);
        for flow in [
            "machine",
            "process/78",
            "process/077",
            "process/+77",
            "process/x",
            "process",
            "other",
        ] {
            let flow = FlowId::new(flow).unwrap();
            assert_eq!(
                state.on_service(ServiceMessage::Release { flow: flow.clone() }),
                (vec![], Sampling::Unchanged)
            );
            assert_eq!(
                state.on_service(ServiceMessage::Refresh { flow }),
                (vec![], Sampling::Unchanged)
            );
        }
    }

    #[test]
    fn hello_is_answered_with_ready() {
        let mut state = State::default();
        let (out, _) = state.on_service(ServiceMessage::Hello {
            name: nowcast_types::BrickName::new("system").unwrap(),
            protocol: 1,
        });
        assert_eq!(out, [BrickMessage::Ready]);
    }
}
