//! A reference reading of the brick protocol frames (contract section 4), used
//! by the suites to judge what a subject writes and to feed it what a peer
//! would send. It is strict: a field of the wrong MessagePack type is an error.

use nowcast_types::{
    BrickMessage, BrickName, FlowId, PROTOCOL_VERSION, PublishErrorCode, RequestId, ServiceMessage,
};
use rmpv::Value;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Wraps a MessagePack value in a frame: its length as a little-endian u32,
/// then the value.
pub fn frame(value: &Value) -> Vec<u8> {
    let mut body = Vec::new();
    rmpv::encode::write_value(&mut body, value).expect("writing to a Vec");
    let mut frame = (body.len() as u32).to_le_bytes().to_vec();
    frame.extend(body);
    frame
}

/// Reads one frame back into its MessagePack value. The length must match the
/// bytes that follow exactly.
pub fn unframe(frame: &[u8]) -> Result<Value, String> {
    let (len, body) = frame
        .split_first_chunk::<4>()
        .ok_or("a frame starts with a 4-byte length")?;
    let len = u32::from_le_bytes(*len) as usize;
    if len != body.len() {
        return Err(format!(
            "the length says {len} bytes, {} follow",
            body.len()
        ));
    }
    let mut rest = body;
    let value = rmpv::decode::read_value(&mut rest).map_err(|e| e.to_string())?;
    if !rest.is_empty() {
        return Err("bytes left after the MessagePack value".into());
    }
    Ok(value)
}

/// Reads one frame from a stream, `None` at a clean end of stream.
pub async fn read_frame(reader: &mut (impl AsyncRead + Unpin)) -> Result<Option<Vec<u8>>, String> {
    let mut len = [0u8; 4];
    match reader.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.to_string()),
    }
    let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|e| format!("truncated frame: {e}"))?;
    let mut frame = len.to_vec();
    frame.extend(body);
    Ok(Some(frame))
}

fn s(v: &str) -> Value {
    Value::from(v)
}

fn map(t: &str, fields: Vec<(&str, Value)>) -> Value {
    let mut entries = vec![(s("v"), Value::from(PROTOCOL_VERSION)), (s("t"), s(t))];
    entries.extend(fields.into_iter().map(|(k, v)| (s(k), v)));
    Value::Map(entries)
}

/// The MessagePack value of a service message, keys in the contract's order.
pub fn service_value(msg: &ServiceMessage) -> Value {
    let t = msg.name();
    match msg {
        ServiceMessage::Hello { name, protocol } => map(
            t,
            vec![
                ("name", s(name.as_str())),
                ("protocol", Value::from(*protocol)),
            ],
        ),
        ServiceMessage::Subscribe { req, kind, params } => map(
            t,
            vec![
                ("req", Value::from(req.0)),
                ("kind", s(kind)),
                ("params", Value::Binary(params.clone())),
            ],
        ),
        ServiceMessage::Release { flow } | ServiceMessage::Refresh { flow } => {
            map(t, vec![("flow", s(flow.as_str()))])
        }
        ServiceMessage::Shutdown => map(t, vec![]),
        ServiceMessage::Ack { req } => map(t, vec![("req", Value::from(req.0))]),
        ServiceMessage::Error { req, code } => map(
            t,
            vec![("req", Value::from(req.0)), ("code", s(code.as_str()))],
        ),
    }
}

/// The MessagePack value of a brick message, keys in the contract's order.
pub fn brick_value(msg: &BrickMessage) -> Value {
    let t = msg.name();
    match msg {
        BrickMessage::Ready => map(t, vec![]),
        BrickMessage::Subscribed { req, flow } => map(
            t,
            vec![("req", Value::from(req.0)), ("flow", s(flow.as_str()))],
        ),
        BrickMessage::Error { req, code, message } => map(
            t,
            vec![
                ("req", Value::from(req.0)),
                ("code", s(code)),
                ("message", s(message)),
            ],
        ),
        BrickMessage::Publish {
            req,
            flow,
            data_type,
            detail,
        } => map(
            t,
            vec![
                ("req", Value::from(req.0)),
                ("flow", s(flow.as_str())),
                ("type", s(data_type)),
                ("detail", Value::Binary(detail.clone())),
            ],
        ),
        BrickMessage::FlowError { flow, message } => {
            map(t, vec![("flow", s(flow.as_str())), ("message", s(message))])
        }
    }
}

pub fn encode_service(msg: &ServiceMessage) -> Vec<u8> {
    frame(&service_value(msg))
}

pub fn encode_brick(msg: &BrickMessage) -> Vec<u8> {
    frame(&brick_value(msg))
}

/// The fields of a decoded message, checked for `v` and read by name.
struct Fields(Vec<(Value, Value)>);

impl Fields {
    fn read(value: Value) -> Result<(String, Self), String> {
        let Value::Map(entries) = value else {
            return Err("a message is a MessagePack map".into());
        };
        let fields = Self(entries);
        let v = fields.u64("v")?;
        if v != u64::from(PROTOCOL_VERSION) {
            return Err(format!("v is {v}, expected {PROTOCOL_VERSION}"));
        }
        let t = fields.str("t")?;
        Ok((t, fields))
    }

    fn get(&self, key: &str) -> Result<&Value, String> {
        self.0
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v)
            .ok_or_else(|| format!("missing key {key:?}"))
    }

    fn str(&self, key: &str) -> Result<String, String> {
        match self.get(key)? {
            Value::String(s) => s
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{key:?} is not valid UTF-8")),
            other => Err(format!("{key:?} must be a string, found {other}")),
        }
    }

    fn u64(&self, key: &str) -> Result<u64, String> {
        self.get(key)?
            .as_u64()
            .ok_or_else(|| format!("{key:?} must be an unsigned integer"))
    }

    fn bin(&self, key: &str) -> Result<Vec<u8>, String> {
        match self.get(key)? {
            Value::Binary(b) => Ok(b.clone()),
            other => Err(format!("{key:?} must be bin, found {other}")),
        }
    }

    fn req(&self) -> Result<RequestId, String> {
        self.u64("req").map(RequestId)
    }

    fn flow(&self) -> Result<FlowId, String> {
        FlowId::new(self.str("flow")?).map_err(|e| e.to_string())
    }
}

pub fn decode_service(frame: &[u8]) -> Result<ServiceMessage, String> {
    let (t, f) = Fields::read(unframe(frame)?)?;
    Ok(match t.as_str() {
        "hello" => ServiceMessage::Hello {
            name: BrickName::new(f.str("name")?).map_err(|e| e.to_string())?,
            protocol: u32::try_from(f.u64("protocol")?).map_err(|e| e.to_string())?,
        },
        "subscribe" => ServiceMessage::Subscribe {
            req: f.req()?,
            kind: f.str("kind")?,
            params: f.bin("params")?,
        },
        "release" => ServiceMessage::Release { flow: f.flow()? },
        "refresh" => ServiceMessage::Refresh { flow: f.flow()? },
        "shutdown" => ServiceMessage::Shutdown,
        "ack" => ServiceMessage::Ack { req: f.req()? },
        "error" => ServiceMessage::Error {
            req: f.req()?,
            code: f
                .str("code")?
                .parse::<PublishErrorCode>()
                .map_err(|e| e.to_string())?,
        },
        other => return Err(format!("unknown service message {other:?}")),
    })
}

pub fn decode_brick(frame: &[u8]) -> Result<BrickMessage, String> {
    let (t, f) = Fields::read(unframe(frame)?)?;
    Ok(match t.as_str() {
        "ready" => BrickMessage::Ready,
        "subscribed" => BrickMessage::Subscribed {
            req: f.req()?,
            flow: f.flow()?,
        },
        "error" => BrickMessage::Error {
            req: f.req()?,
            code: f.str("code")?,
            message: f.str("message")?,
        },
        "publish" => BrickMessage::Publish {
            req: f.req()?,
            flow: f.flow()?,
            data_type: f.str("type")?,
            detail: f.bin("detail")?,
        },
        "flow-error" => BrickMessage::FlowError {
            flow: f.flow()?,
            message: f.str("message")?,
        },
        other => return Err(format!("unknown brick message {other:?}")),
    })
}
