//! The control format: JSON objects, one per line (`\n`), over the control
//! socket. The test sends a [`Command`], the brick answers each one with one
//! [`Reply`], in order, and reads the next command only once it has answered
//! the previous one. Byte strings (a detail, subscription params) are arrays
//! of numbers, so that any bytes travel, invalid JSON included.
//!
//! ```text
//! test  → {"op":"publish","flow":"a","type":"probe.item/v1","detail":[123,125]}
//! brick → {"reply":"acked"}
//! test  → {"op":"received"}
//! brick → {"reply":"received","messages":[{"t":"hello","name":"probe","protocol":1}, ...]}
//! ```

use nowcast_types::{BrickName, FlowId, PublishErrorCode, RequestId, ServiceMessage};
use serde::{Deserialize, Serialize};

/// What a test asks the brick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    /// Sends `publish` with these fields as is, whatever the flow, the type
    /// or the detail. Answered with [`Reply::Acked`] or [`Reply::Refused`]
    /// once the service answers, or with [`Reply::Invalid`] when the message
    /// would not fit in a frame.
    Publish {
        flow: String,
        #[serde(rename = "type")]
        data_type: String,
        detail: Vec<u8>,
    },
    /// Sends `flow-error`. Answered with [`Reply::Done`] once it is written.
    FlowError { flow: String, message: String },
    /// Asks for every message the service sent so far, in order. Answered
    /// with [`Reply::Received`].
    Received,
    /// Exits at once with status [`crate::CRASH_STATUS`]. Never answered: the
    /// connection ends with the process.
    Crash,
}

/// What the brick answers a [`Command`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    /// The service answered the `publish` with `ack`.
    Acked,
    /// The service answered the `publish` with `error` and this code.
    Refused {
        code: String,
    },
    Done,
    Received {
        messages: Vec<Received>,
    },
    /// The command could not be read or carried out.
    Invalid {
        message: String,
    },
}

/// A message the service sent, in the protocol's terms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "kebab-case")]
pub enum Received {
    Hello {
        name: String,
        protocol: u32,
    },
    Subscribe {
        req: u64,
        kind: String,
        params: Vec<u8>,
    },
    Release {
        flow: String,
    },
    Refresh {
        flow: String,
    },
    Input {
        flow: String,
        #[serde(rename = "type")]
        data_type: String,
        detail: Vec<u8>,
    },
    Shutdown,
    Ack {
        req: u64,
    },
    Error {
        req: u64,
        code: String,
    },
}

impl From<&ServiceMessage> for Received {
    fn from(msg: &ServiceMessage) -> Self {
        match msg {
            ServiceMessage::Hello { name, protocol } => Self::Hello {
                name: name.as_str().to_owned(),
                protocol: *protocol,
            },
            ServiceMessage::Subscribe { req, kind, params } => Self::Subscribe {
                req: req.0,
                kind: kind.clone(),
                params: params.clone(),
            },
            ServiceMessage::Release { flow } => Self::Release {
                flow: flow.as_str().to_owned(),
            },
            ServiceMessage::Refresh { flow } => Self::Refresh {
                flow: flow.as_str().to_owned(),
            },
            ServiceMessage::Input {
                flow,
                data_type,
                detail,
            } => Self::Input {
                flow: flow.as_str().to_owned(),
                data_type: data_type.clone(),
                detail: detail.clone(),
            },
            ServiceMessage::Shutdown => Self::Shutdown,
            ServiceMessage::Ack { req } => Self::Ack { req: req.0 },
            ServiceMessage::Error { req, code } => Self::Error {
                req: req.0,
                code: code.as_str().to_owned(),
            },
        }
    }
}

impl TryFrom<Received> for ServiceMessage {
    type Error = String;

    fn try_from(msg: Received) -> Result<Self, String> {
        let flow = |flow: String| FlowId::new(flow).map_err(|e| e.to_string());
        Ok(match msg {
            Received::Hello { name, protocol } => Self::Hello {
                name: BrickName::new(name).map_err(|e| e.to_string())?,
                protocol,
            },
            Received::Subscribe { req, kind, params } => Self::Subscribe {
                req: RequestId(req),
                kind,
                params,
            },
            Received::Release { flow: id } => Self::Release { flow: flow(id)? },
            Received::Refresh { flow: id } => Self::Refresh { flow: flow(id)? },
            Received::Input {
                flow: id,
                data_type,
                detail,
            } => Self::Input {
                flow: flow(id)?,
                data_type,
                detail,
            },
            Received::Shutdown => Self::Shutdown,
            Received::Ack { req } => Self::Ack {
                req: RequestId(req),
            },
            Received::Error { req, code } => Self::Error {
                req: RequestId(req),
                code: code
                    .parse::<PublishErrorCode>()
                    .map_err(|e| e.to_string())?,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn commands_read_as_documented() {
        let publish: Command = serde_json::from_value(json!({
            "op": "publish", "flow": "a", "type": "probe.item/v1", "detail": [123, 125]
        }))
        .unwrap();
        assert_eq!(
            publish,
            Command::Publish {
                flow: "a".into(),
                data_type: "probe.item/v1".into(),
                detail: b"{}".to_vec(),
            }
        );
        let crash: Command = serde_json::from_value(json!({ "op": "crash" })).unwrap();
        assert_eq!(crash, Command::Crash);
        assert!(serde_json::from_value::<Command>(json!({ "op": "nope" })).is_err());
    }

    #[test]
    fn replies_write_as_documented() {
        assert_eq!(
            serde_json::to_value(Reply::Refused {
                code: "invalid_json".into()
            })
            .unwrap(),
            json!({ "reply": "refused", "code": "invalid_json" })
        );
        assert_eq!(
            serde_json::to_value(Received::from(&ServiceMessage::Refresh {
                flow: FlowId::new("a").unwrap()
            }))
            .unwrap(),
            json!({ "t": "refresh", "flow": "a" })
        );
    }

    #[test]
    fn service_messages_round_trip() {
        let messages = [
            ServiceMessage::Hello {
                name: BrickName::new("probe").unwrap(),
                protocol: 1,
            },
            ServiceMessage::Subscribe {
                req: RequestId(3),
                kind: "item".into(),
                params: br#"{"id":"a"}"#.to_vec(),
            },
            ServiceMessage::Release {
                flow: FlowId::new("a").unwrap(),
            },
            ServiceMessage::Refresh {
                flow: FlowId::new("a/b#c d").unwrap(),
            },
            ServiceMessage::Shutdown,
            ServiceMessage::Ack { req: RequestId(7) },
            ServiceMessage::Error {
                req: RequestId(8),
                code: PublishErrorCode::UndeclaredType,
            },
        ];
        for msg in messages {
            let line = serde_json::to_string(&Received::from(&msg)).unwrap();
            let back: Received = serde_json::from_str(&line).unwrap();
            assert_eq!(ServiceMessage::try_from(back), Ok(msg));
        }
    }

    #[test]
    fn an_unknown_code_does_not_convert() {
        let received = Received::Error {
            req: 1,
            code: "nope".into(),
        };
        assert!(ServiceMessage::try_from(received).is_err());
    }
}
