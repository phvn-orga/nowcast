//! The messages: a MessagePack map with `v`, `t`, `req` when the message has
//! one, and the fields of contract section 4.

use std::collections::HashMap;
use std::io;

use nowcast_types::{
    BrickMessage, BrickName, FlowId, PROTOCOL_VERSION, PublishErrorCode, RequestId, ServiceMessage,
};
use rmpv::ValueRef;

use crate::CodecError;
use crate::frame::{PREFIX_LEN, prefix, unframe};

/// How deep a decoded body may nest. Every message is a flat map, so this
/// only leaves room for fields a later minor version might add.
const MAX_DEPTH: usize = 16;

/// Encodes a service message into a whole frame, length prefix included.
pub fn encode_service(msg: &ServiceMessage) -> Result<Vec<u8>, CodecError> {
    let t = msg.name();
    match msg {
        ServiceMessage::Hello { name, protocol } => encode(
            t,
            &[
                ("name", name.as_str().into()),
                ("protocol", (*protocol).into()),
            ],
        ),
        ServiceMessage::Subscribe { req, kind, params } => encode(
            t,
            &[
                ("req", req.0.into()),
                ("kind", kind.as_str().into()),
                ("params", params.as_slice().into()),
            ],
        ),
        ServiceMessage::Release { flow } | ServiceMessage::Refresh { flow } => {
            encode(t, &[("flow", flow.as_str().into())])
        }
        ServiceMessage::Input {
            flow,
            data_type,
            detail,
        } => encode(
            t,
            &[
                ("flow", flow.as_str().into()),
                ("type", data_type.as_str().into()),
                ("detail", detail.as_slice().into()),
            ],
        ),
        ServiceMessage::Shutdown => encode(t, &[]),
        ServiceMessage::Ack { req } => encode(t, &[("req", req.0.into())]),
        ServiceMessage::Error { req, code } => {
            encode(t, &[("req", req.0.into()), ("code", code.as_str().into())])
        }
    }
}

/// Encodes a brick message into a whole frame, length prefix included.
pub fn encode_brick(msg: &BrickMessage) -> Result<Vec<u8>, CodecError> {
    let t = msg.name();
    match msg {
        BrickMessage::Ready => encode(t, &[]),
        BrickMessage::Subscribed { req, flow } => {
            encode(t, &[("req", req.0.into()), ("flow", flow.as_str().into())])
        }
        BrickMessage::Error { req, code, message } => encode(
            t,
            &[
                ("req", req.0.into()),
                ("code", code.as_str().into()),
                ("message", message.as_str().into()),
            ],
        ),
        BrickMessage::Publish {
            req,
            flow,
            data_type,
            detail,
        } => encode(
            t,
            &[
                ("req", req.0.into()),
                ("flow", flow.as_str().into()),
                ("type", data_type.as_str().into()),
                ("detail", detail.as_slice().into()),
            ],
        ),
        BrickMessage::FlowError { flow, message } => encode(
            t,
            &[
                ("flow", flow.as_str().into()),
                ("message", message.as_str().into()),
            ],
        ),
    }
}

/// Decodes a whole frame, length prefix included, into a service message.
pub fn decode_service(frame: &[u8]) -> Result<ServiceMessage, CodecError> {
    let fields = Fields::read(frame)?;
    Ok(match fields.t {
        "hello" => ServiceMessage::Hello {
            name: BrickName::new(fields.str("name")?)
                .map_err(|e| invalid("name", e.to_string()))?,
            protocol: u32::try_from(fields.u64("protocol")?)
                .map_err(|_| invalid("protocol", "does not fit a u32".into()))?,
        },
        "subscribe" => ServiceMessage::Subscribe {
            req: fields.req()?,
            kind: fields.str("kind")?.to_owned(),
            params: fields.bin("params")?.to_vec(),
        },
        "release" => ServiceMessage::Release {
            flow: fields.flow()?,
        },
        "refresh" => ServiceMessage::Refresh {
            flow: fields.flow()?,
        },
        "input" => ServiceMessage::Input {
            flow: fields.flow()?,
            data_type: fields.str("type")?.to_owned(),
            detail: fields.bin("detail")?.to_vec(),
        },
        "shutdown" => ServiceMessage::Shutdown,
        "ack" => ServiceMessage::Ack { req: fields.req()? },
        "error" => ServiceMessage::Error {
            req: fields.req()?,
            code: {
                let code = fields.str("code")?;
                code.parse::<PublishErrorCode>()
                    .map_err(|_| invalid("code", format!("is unknown: {:?}", clip(code))))?
            },
        },
        other => return Err(CodecError::UnknownType(clip(other))),
    })
}

/// Decodes a whole frame, length prefix included, into a brick message.
pub fn decode_brick(frame: &[u8]) -> Result<BrickMessage, CodecError> {
    let fields = Fields::read(frame)?;
    Ok(match fields.t {
        "ready" => BrickMessage::Ready,
        "subscribed" => BrickMessage::Subscribed {
            req: fields.req()?,
            flow: fields.flow()?,
        },
        "error" => BrickMessage::Error {
            req: fields.req()?,
            code: fields.str("code")?.to_owned(),
            message: fields.str("message")?.to_owned(),
        },
        "publish" => BrickMessage::Publish {
            req: fields.req()?,
            flow: fields.flow()?,
            data_type: fields.str("type")?.to_owned(),
            detail: fields.bin("detail")?.to_vec(),
        },
        "flow-error" => BrickMessage::FlowError {
            flow: fields.flow()?,
            message: fields.str("message")?.to_owned(),
        },
        other => return Err(CodecError::UnknownType(clip(other))),
    })
}

/// Writes the frame of a message: `v` and `t` first, then its fields.
fn encode(t: &str, fields: &[(&str, ValueRef<'_>)]) -> Result<Vec<u8>, CodecError> {
    let mut entries = Vec::with_capacity(2 + fields.len());
    entries.push((ValueRef::from("v"), ValueRef::from(PROTOCOL_VERSION)));
    entries.push((ValueRef::from("t"), ValueRef::from(t)));
    entries.extend(
        fields
            .iter()
            .map(|(key, value)| (ValueRef::from(*key), value.clone())),
    );
    let mut frame = vec![0u8; PREFIX_LEN];
    // Writing to a Vec cannot fail; the error is mapped rather than unwrapped.
    rmpv::encode::write_value_ref(&mut frame, &ValueRef::Map(entries)).map_err(io::Error::other)?;
    let prefix = prefix(frame.len() - PREFIX_LEN)?;
    frame[..PREFIX_LEN].copy_from_slice(&prefix);
    Ok(frame)
}

fn invalid(key: &str, reason: String) -> CodecError {
    CodecError::Malformed(format!("{key:?} {reason}"))
}

fn wrong_type(key: &str, expected: &str, found: &ValueRef<'_>) -> CodecError {
    invalid(key, format!("must be {expected}, found {}", kind(found)))
}

/// A peer's string cut to a length fit for an error message.
fn clip(s: &str) -> String {
    const MAX: usize = 64;
    match s.char_indices().nth(MAX) {
        Some((end, _)) => format!("{}...", &s[..end]),
        None => s.to_owned(),
    }
}

/// The MessagePack type of a value, for error messages: printing the value
/// itself could copy a whole frame into a log line.
fn kind(value: &ValueRef<'_>) -> &'static str {
    match value {
        ValueRef::Nil => "nil",
        ValueRef::Boolean(_) => "a boolean",
        ValueRef::Integer(n) if n.is_u64() => "an unsigned integer",
        ValueRef::Integer(_) => "a negative integer",
        ValueRef::F32(_) | ValueRef::F64(_) => "a float",
        ValueRef::String(_) => "a string",
        ValueRef::Binary(_) => "bin",
        ValueRef::Array(_) => "an array",
        ValueRef::Map(_) => "a map",
        ValueRef::Ext(..) => "an ext",
    }
}

/// The fields of a decoded message, its version checked and its type read.
/// Keyed by a hash map so that a map with many keys decodes in linear time.
struct Fields<'a> {
    t: &'a str,
    entries: HashMap<&'a str, ValueRef<'a>>,
}

impl<'a> Fields<'a> {
    fn read(frame: &'a [u8]) -> Result<Self, CodecError> {
        let mut body = unframe(frame)?;
        let value = rmpv::decode::read_value_ref_with_max_depth(&mut body, MAX_DEPTH)
            .map_err(|e| CodecError::Malformed(format!("invalid MessagePack: {e}")))?;
        if !body.is_empty() {
            return Err(CodecError::Malformed(format!(
                "{} bytes left after the MessagePack value",
                body.len()
            )));
        }
        let ValueRef::Map(map) = value else {
            return Err(CodecError::Malformed(format!(
                "a message is a MessagePack map, found {}",
                kind(&value)
            )));
        };
        let mut entries = HashMap::with_capacity(map.len());
        for (key, value) in map {
            let ValueRef::String(key) = key else {
                return Err(CodecError::Malformed(format!(
                    "message keys are strings, found {}",
                    kind(&key)
                )));
            };
            let key = key
                .into_str()
                .ok_or_else(|| CodecError::Malformed("a key is not valid UTF-8".into()))?;
            if entries.insert(key, value).is_some() {
                return Err(CodecError::Malformed(format!(
                    "duplicate key {:?}",
                    clip(key)
                )));
            }
        }
        let mut fields = Self { t: "", entries };
        let v = fields.u64("v")?;
        if v != u64::from(PROTOCOL_VERSION) {
            return Err(CodecError::UnsupportedVersion(v));
        }
        fields.t = fields.str("t")?;
        Ok(fields)
    }

    fn get(&self, key: &str) -> Result<&ValueRef<'a>, CodecError> {
        self.entries
            .get(key)
            .ok_or_else(|| CodecError::Malformed(format!("missing key {key:?}")))
    }

    fn str(&self, key: &str) -> Result<&'a str, CodecError> {
        match self.get(key)? {
            ValueRef::String(s) => s
                .into_str()
                .ok_or_else(|| invalid(key, "is not valid UTF-8".into())),
            other => Err(wrong_type(key, "a string", other)),
        }
    }

    fn u64(&self, key: &str) -> Result<u64, CodecError> {
        let value = self.get(key)?;
        match value {
            ValueRef::Integer(n) => n
                .as_u64()
                .ok_or_else(|| wrong_type(key, "an unsigned integer", value)),
            other => Err(wrong_type(key, "an unsigned integer", other)),
        }
    }

    fn bin(&self, key: &str) -> Result<&'a [u8], CodecError> {
        match self.get(key)? {
            ValueRef::Binary(bytes) => Ok(bytes),
            other => Err(wrong_type(key, "bin", other)),
        }
    }

    fn req(&self) -> Result<RequestId, CodecError> {
        self.u64("req").map(RequestId)
    }

    fn flow(&self) -> Result<FlowId, CodecError> {
        FlowId::new(self.str("flow")?).map_err(|e| invalid("flow", e.to_string()))
    }
}
