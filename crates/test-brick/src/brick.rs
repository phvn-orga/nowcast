//! What the test brick does, without the I/O: each message from the service
//! or command from the test gives the effects to carry out, in order.

use std::collections::BTreeSet;

use nowcast_codec::encode_brick;
use nowcast_test_brick::wire::{Command, Received, Reply};
use nowcast_test_brick::{CRASH_STATUS, INBOX_KIND, KIND, Mode, REFUSAL_CODE, TYPE};
use nowcast_types::{BrickMessage, FlowId, RequestId, ServiceMessage};
use serde::Deserialize;
use serde_json::json;

/// Something the I/O loop does.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    /// Writes this message to the service.
    Send(BrickMessage),
    /// Writes this reply to the test.
    Reply(Reply),
    /// Ends the process with this status.
    Exit(i32),
}

/// The params of an `item` subscription.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemParams {
    id: String,
}

pub struct Brick {
    mode: Mode,
    /// The flows subscribed and not released.
    flows: BTreeSet<FlowId>,
    /// The `req` of the last `publish` sent.
    last_req: u64,
    /// The `req` of the commanded `publish` awaiting the service's answer.
    awaiting: Option<RequestId>,
    refreshes: u64,
    received: Vec<Received>,
}

impl Brick {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            flows: BTreeSet::new(),
            last_req: 0,
            awaiting: None,
            refreshes: 0,
            received: Vec::new(),
        }
    }

    /// Whether a commanded `publish` awaits the service's answer. The next
    /// command waits for it, so that replies keep the order of commands.
    pub fn is_awaiting(&self) -> bool {
        self.awaiting.is_some()
    }

    pub fn on_service(&mut self, msg: ServiceMessage) -> Vec<Effect> {
        self.received.push(Received::from(&msg));
        match msg {
            ServiceMessage::Hello { .. } => match self.mode {
                Mode::NeverReady => vec![],
                Mode::Ready | Mode::Refusing => vec![Effect::Send(BrickMessage::Ready)],
            },
            ServiceMessage::Subscribe { req, kind, params } => {
                vec![Effect::Send(self.subscribe(req, &kind, &params))]
            }
            ServiceMessage::Release { flow } => {
                self.flows.remove(&flow);
                vec![]
            }
            ServiceMessage::Refresh { flow } if self.flows.contains(&flow) => {
                self.refreshes += 1;
                let detail = json!({ "id": flow.as_str(), "refresh": self.refreshes });
                vec![Effect::Send(BrickMessage::Publish {
                    req: self.next_req(),
                    flow,
                    data_type: TYPE.into(),
                    detail: detail.to_string().into_bytes(),
                })]
            }
            ServiceMessage::Refresh { .. } => vec![],
            ServiceMessage::Input { flow, detail, .. } if self.flows.contains(&flow) => {
                // The service checked the detail against the accepted schema.
                let input: serde_json::Value =
                    serde_json::from_slice(&detail).unwrap_or(serde_json::Value::Null);
                let detail = json!({ "id": flow.as_str(), "input": input });
                vec![Effect::Send(BrickMessage::Publish {
                    req: self.next_req(),
                    flow,
                    data_type: TYPE.into(),
                    detail: detail.to_string().into_bytes(),
                })]
            }
            ServiceMessage::Input { .. } => vec![],
            ServiceMessage::Shutdown => vec![Effect::Exit(0)],
            ServiceMessage::Ack { req } if self.awaiting == Some(req) => {
                self.awaiting = None;
                vec![Effect::Reply(Reply::Acked)]
            }
            ServiceMessage::Error { req, code } if self.awaiting == Some(req) => {
                self.awaiting = None;
                vec![Effect::Reply(Reply::Refused {
                    code: code.as_str().into(),
                })]
            }
            // The answer to a publish of a refresh.
            ServiceMessage::Ack { .. } | ServiceMessage::Error { .. } => vec![],
        }
    }

    /// Handles one line from the test.
    pub fn on_control(&mut self, line: &str) -> Vec<Effect> {
        let command = match serde_json::from_str::<Command>(line) {
            Ok(command) => command,
            Err(e) => return invalid(format!("unreadable command: {e}")),
        };
        match command {
            Command::Publish {
                flow,
                data_type,
                detail,
            } => match FlowId::new(flow) {
                Ok(flow) => {
                    let req = self.next_req();
                    let msg = BrickMessage::Publish {
                        req,
                        flow,
                        data_type,
                        detail,
                    };
                    if let Err(e) = encode_brick(&msg) {
                        return invalid(e.to_string());
                    }
                    self.awaiting = Some(req);
                    vec![Effect::Send(msg)]
                }
                Err(e) => invalid(e.to_string()),
            },
            Command::FlowError { flow, message } => match FlowId::new(flow) {
                Ok(flow) => {
                    let msg = BrickMessage::FlowError { flow, message };
                    if let Err(e) = encode_brick(&msg) {
                        return invalid(e.to_string());
                    }
                    vec![Effect::Send(msg), Effect::Reply(Reply::Done)]
                }
                Err(e) => invalid(e.to_string()),
            },
            Command::Received => vec![Effect::Reply(Reply::Received {
                messages: self.received.clone(),
            })],
            Command::Crash => vec![Effect::Exit(CRASH_STATUS)],
        }
    }

    /// The answer to `subscribe`.
    fn subscribe(&mut self, req: RequestId, kind: &str, params: &[u8]) -> BrickMessage {
        let refuse = |code: &str, message: String| BrickMessage::Error {
            req,
            code: code.into(),
            message,
        };
        if self.mode == Mode::Refusing {
            return refuse(REFUSAL_CODE, "this brick refuses every subscription".into());
        }
        if kind != KIND && kind != INBOX_KIND {
            return refuse("unknown_kind", format!("no subscription kind {kind:?}"));
        }
        let id = match serde_json::from_slice::<ItemParams>(params) {
            Ok(params) if kind == INBOX_KIND => format!("inbox/{}", params.id),
            Ok(params) => params.id,
            Err(e) => {
                return refuse(
                    "invalid_params",
                    format!("expected {{\"id\": string}}: {e}"),
                );
            }
        };
        match FlowId::new(id) {
            Ok(flow) => {
                self.flows.insert(flow.clone());
                BrickMessage::Subscribed { req, flow }
            }
            Err(e) => refuse("invalid_params", e.to_string()),
        }
    }

    fn next_req(&mut self) -> RequestId {
        self.last_req += 1;
        RequestId(self.last_req)
    }
}

fn invalid(message: String) -> Vec<Effect> {
    vec![Effect::Reply(Reply::Invalid { message })]
}

#[cfg(test)]
mod tests {
    use nowcast_codec::MAX_FRAME_LEN;
    use nowcast_types::{BrickName, PublishErrorCode};

    use super::*;

    fn flow(id: &str) -> FlowId {
        FlowId::new(id).unwrap()
    }

    fn subscribe(req: u64, kind: &str, params: &[u8]) -> ServiceMessage {
        ServiceMessage::Subscribe {
            req: RequestId(req),
            kind: kind.into(),
            params: params.to_vec(),
        }
    }

    fn error_code(effects: &[Effect]) -> &str {
        match effects {
            [Effect::Send(BrickMessage::Error { code, .. })] => code,
            other => panic!("expected one error, got {other:?}"),
        }
    }

    fn served(brick: &mut Brick, id: &str) {
        let params = serde_json::to_vec(&json!({ "id": id })).unwrap();
        assert_eq!(
            brick.on_service(subscribe(1, KIND, &params)),
            vec![Effect::Send(BrickMessage::Subscribed {
                req: RequestId(1),
                flow: flow(id)
            })]
        );
    }

    #[test]
    fn hello_is_answered_unless_never_ready() {
        let hello = ServiceMessage::Hello {
            name: BrickName::new("probe").unwrap(),
            protocol: 1,
        };
        for mode in [Mode::Ready, Mode::Refusing] {
            assert_eq!(
                Brick::new(mode).on_service(hello.clone()),
                vec![Effect::Send(BrickMessage::Ready)]
            );
        }
        assert_eq!(Brick::new(Mode::NeverReady).on_service(hello), vec![]);
    }

    #[test]
    fn the_flow_is_the_id_verbatim() {
        served(&mut Brick::new(Mode::Ready), "a/b#c d");
    }

    #[test]
    fn bad_subscriptions_are_refused() {
        let mut brick = Brick::new(Mode::Ready);
        let long = serde_json::to_vec(&json!({ "id": "x".repeat(257) })).unwrap();
        for (kind, params, code) in [
            ("other", &br#"{"id":"a"}"#[..], "unknown_kind"),
            (KIND, b"{not json", "invalid_params"),
            (KIND, br#"{"id":1}"#, "invalid_params"),
            (KIND, br#"{"id":"a","more":1}"#, "invalid_params"),
            (KIND, br#"{}"#, "invalid_params"),
            (KIND, &long, "invalid_params"),
        ] {
            assert_eq!(
                error_code(&brick.on_service(subscribe(1, kind, params))),
                code
            );
        }
    }

    #[test]
    fn refusing_refuses_every_subscription() {
        let mut brick = Brick::new(Mode::Refusing);
        let effects = brick.on_service(subscribe(1, KIND, br#"{"id":"a"}"#));
        assert_eq!(error_code(&effects), REFUSAL_CODE);
    }

    #[test]
    fn refresh_publishes_on_served_flows_only() {
        let mut brick = Brick::new(Mode::Ready);
        served(&mut brick, "a");
        let refresh = |id: &str| ServiceMessage::Refresh { flow: flow(id) };
        assert_eq!(
            brick.on_service(refresh("a")),
            vec![Effect::Send(BrickMessage::Publish {
                req: RequestId(1),
                flow: flow("a"),
                data_type: TYPE.into(),
                detail: br#"{"id":"a","refresh":1}"#.to_vec(),
            })]
        );
        assert_eq!(brick.on_service(refresh("never")), vec![]);
        brick.on_service(ServiceMessage::Release { flow: flow("a") });
        assert_eq!(brick.on_service(refresh("a")), vec![]);
    }

    #[test]
    fn a_commanded_publish_waits_for_its_own_answer() {
        let mut brick = Brick::new(Mode::Ready);
        served(&mut brick, "a");
        // A refresh publish takes req 1.
        brick.on_service(ServiceMessage::Refresh { flow: flow("a") });
        let effects = brick
            .on_control(r#"{"op":"publish","flow":"a","type":"probe.other/v1","detail":[123]}"#);
        assert_eq!(
            effects,
            vec![Effect::Send(BrickMessage::Publish {
                req: RequestId(2),
                flow: flow("a"),
                data_type: "probe.other/v1".into(),
                detail: b"{".to_vec(),
            })]
        );
        assert!(brick.is_awaiting());
        assert_eq!(
            brick.on_service(ServiceMessage::Ack { req: RequestId(1) }),
            vec![]
        );
        assert!(brick.is_awaiting());
        assert_eq!(
            brick.on_service(ServiceMessage::Error {
                req: RequestId(2),
                code: PublishErrorCode::UndeclaredType
            }),
            vec![Effect::Reply(Reply::Refused {
                code: "undeclared_type".into()
            })]
        );
        assert!(!brick.is_awaiting());
    }

    #[test]
    fn bad_commands_get_invalid() {
        let mut brick = Brick::new(Mode::Ready);
        let long = "x".repeat(257);
        for line in [
            "not json".to_owned(),
            r#"{"op":"nope"}"#.to_owned(),
            format!(r#"{{"op":"publish","flow":"{long}","type":"t","detail":[]}}"#),
            format!(r#"{{"op":"flow_error","flow":"{long}","message":"m"}}"#),
        ] {
            assert!(
                matches!(
                    brick.on_control(&line)[..],
                    [Effect::Reply(Reply::Invalid { .. })]
                ),
                "{line}"
            );
        }
        assert!(!brick.is_awaiting());
    }

    #[test]
    fn a_message_too_large_for_a_frame_gets_invalid() {
        let mut brick = Brick::new(Mode::Ready);
        let too_long = "x".repeat(MAX_FRAME_LEN);
        for command in [
            Command::FlowError {
                flow: "a".into(),
                message: too_long.clone(),
            },
            Command::Publish {
                flow: "a".into(),
                data_type: too_long,
                detail: vec![],
            },
        ] {
            let line = serde_json::to_string(&command).unwrap();
            assert!(matches!(
                brick.on_control(&line)[..],
                [Effect::Reply(Reply::Invalid { .. })]
            ));
        }
        assert!(!brick.is_awaiting());
    }

    #[test]
    fn received_lists_every_service_message() {
        let mut brick = Brick::new(Mode::Ready);
        served(&mut brick, "a");
        brick.on_service(ServiceMessage::Release { flow: flow("a") });
        let effects = brick.on_control(r#"{"op":"received"}"#);
        let [Effect::Reply(Reply::Received { messages })] = &effects[..] else {
            panic!("expected received, got {effects:?}");
        };
        assert_eq!(
            messages,
            &[
                Received::Subscribe {
                    req: 1,
                    kind: KIND.into(),
                    params: br#"{"id":"a"}"#.to_vec()
                },
                Received::Release { flow: "a".into() },
            ]
        );
    }

    #[test]
    fn shutdown_and_crash_exit() {
        let mut brick = Brick::new(Mode::Ready);
        assert_eq!(
            brick.on_service(ServiceMessage::Shutdown),
            vec![Effect::Exit(0)]
        );
        assert_eq!(
            brick.on_control(r#"{"op":"crash"}"#),
            vec![Effect::Exit(CRASH_STATUS)]
        );
    }
}
