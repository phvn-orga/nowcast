//! What the brick answers the service, without I/O: one flow, `machine`,
//! published when its detail changes and after every `refresh`.

use nowcast_types::{BrickMessage, FlowId, RequestId, ServiceMessage};
use serde_json::Value;

pub const MACHINE_TYPE: &str = "system.machine/v1";
const MACHINE: &str = "machine";

/// What the shell does with the sampler after a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sampling {
    Unchanged,
    /// The flow opened: start sampling.
    Start,
    /// The flow was released: stop.
    Stop,
    /// A `refresh`: sample now.
    Now,
}

#[derive(Default)]
pub struct State {
    subscribed: bool,
    /// The last detail published, as sent.
    last: Option<Vec<u8>>,
    /// The next sample publishes even unchanged.
    forced: bool,
    last_req: u64,
}

impl State {
    pub fn on_service(&mut self, msg: ServiceMessage) -> (Vec<BrickMessage>, Sampling) {
        match msg {
            ServiceMessage::Hello { .. } => (vec![BrickMessage::Ready], Sampling::Unchanged),
            ServiceMessage::Subscribe { req, kind, .. } => {
                if kind != MACHINE {
                    let error = BrickMessage::Error {
                        req,
                        code: "unknown_kind".into(),
                        message: format!("no subscription kind {kind:?}"),
                    };
                    return (vec![error], Sampling::Unchanged);
                }
                let subscribed = BrickMessage::Subscribed {
                    req,
                    flow: machine(),
                };
                let sampling = if self.subscribed {
                    Sampling::Unchanged
                } else {
                    self.subscribed = true;
                    Sampling::Start
                };
                (vec![subscribed], sampling)
            }
            ServiceMessage::Release { flow } if flow.as_str() == MACHINE && self.subscribed => {
                self.subscribed = false;
                self.last = None;
                self.forced = false;
                (vec![], Sampling::Stop)
            }
            ServiceMessage::Refresh { flow } if flow.as_str() == MACHINE && self.subscribed => {
                self.forced = true;
                (vec![], Sampling::Now)
            }
            // The brick declares no refresh scope and accepts no input.
            _ => (vec![], Sampling::Unchanged),
        }
    }

    /// A sample: published when it differs from the last one, or after a
    /// `refresh`.
    pub fn on_sample(&mut self, detail: &Value) -> Vec<BrickMessage> {
        if !self.subscribed {
            return vec![];
        }
        let bytes = serde_json::to_vec(detail).expect("a JSON value serialises");
        let forced = std::mem::take(&mut self.forced);
        if !forced && self.last.as_ref() == Some(&bytes) {
            return vec![];
        }
        self.last = Some(bytes.clone());
        self.last_req += 1;
        vec![BrickMessage::Publish {
            req: RequestId(self.last_req),
            flow: machine(),
            data_type: MACHINE_TYPE.into(),
            detail: bytes,
        }]
    }
}

fn machine() -> FlowId {
    FlowId::new(MACHINE).expect("a short flow id")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn subscribe(state: &mut State, kind: &str) -> (Vec<BrickMessage>, Sampling) {
        state.on_service(ServiceMessage::Subscribe {
            req: RequestId(1),
            kind: kind.into(),
            params: b"{}".to_vec(),
        })
    }

    fn published(messages: &[BrickMessage]) -> Vec<Value> {
        messages
            .iter()
            .map(|m| match m {
                BrickMessage::Publish {
                    detail,
                    data_type,
                    flow,
                    ..
                } => {
                    assert_eq!(data_type, MACHINE_TYPE);
                    assert_eq!(flow.as_str(), MACHINE);
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
                flow: machine()
            }]
        );
        assert_eq!(sampling, Sampling::Start);
        assert_eq!(subscribe(&mut state, "machine").1, Sampling::Unchanged);
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
        let a = json!({ "cpu": { "usage": 3 } });
        let b = json!({ "cpu": { "usage": 4 } });
        assert_eq!(published(&state.on_sample(&a)), std::slice::from_ref(&a));
        assert!(state.on_sample(&a).is_empty());
        assert_eq!(published(&state.on_sample(&b)), std::slice::from_ref(&b));
        let (out, sampling) = state.on_service(ServiceMessage::Refresh { flow: machine() });
        assert!(out.is_empty());
        assert_eq!(sampling, Sampling::Now);
        assert_eq!(published(&state.on_sample(&b)), std::slice::from_ref(&b));
        assert!(state.on_sample(&b).is_empty());
    }

    #[test]
    fn a_release_stops_and_the_next_flow_publishes_afresh() {
        let mut state = State::default();
        subscribe(&mut state, "machine");
        let a = json!({ "cpu": {} });
        state.on_sample(&a);
        let (_, sampling) = state.on_service(ServiceMessage::Release { flow: machine() });
        assert_eq!(sampling, Sampling::Stop);
        assert!(state.on_sample(&a).is_empty());
        assert_eq!(subscribe(&mut state, "machine").1, Sampling::Start);
        assert_eq!(published(&state.on_sample(&a)), [a]);
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
