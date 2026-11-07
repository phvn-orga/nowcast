//! Contract section 4, the frames: a little-endian u32 length followed by a
//! MessagePack map carrying `v`, `t` and the message's fields.
//!
//! The subject is a codec. It is judged against [`crate::reference`]: what it
//! encodes must read back strictly, and it must decode what a peer would send.

use nowcast_types::{
    BrickMessage, BrickName, FlowId, PROTOCOL_VERSION, PublishErrorCode, RequestId, ServiceMessage,
};
use rmpv::Value;

use crate::reference;

/// A codec of the brick protocol. Each frame includes its length prefix.
pub trait CodecSubject {
    fn encode_service(msg: &ServiceMessage) -> Vec<u8>;
    fn decode_service(frame: &[u8]) -> Result<ServiceMessage, String>;
    fn encode_brick(msg: &BrickMessage) -> Vec<u8>;
    fn decode_brick(frame: &[u8]) -> Result<BrickMessage, String>;
}

/// Above `u32::MAX`, to catch a `req` narrowed on the way.
const BIG_REQ: RequestId = RequestId(u64::MAX - 1);

fn flow() -> FlowId {
    FlowId::new("github.com/org/repo#main").unwrap()
}

/// One of each service message.
pub fn service_messages() -> Vec<ServiceMessage> {
    vec![
        ServiceMessage::Hello {
            name: BrickName::new("forge").unwrap(),
            protocol: PROTOCOL_VERSION,
        },
        ServiceMessage::Subscribe {
            req: BIG_REQ,
            kind: "pr".into(),
            params: br#"{"remote":"git@github.com:org/repo.git","branch":"main"}"#.to_vec(),
        },
        ServiceMessage::Release { flow: flow() },
        ServiceMessage::Refresh { flow: flow() },
        ServiceMessage::Input {
            flow: flow(),
            data_type: "forge.push/v1".into(),
            detail: br#"{"branch":"main"}"#.to_vec(),
        },
        ServiceMessage::Shutdown,
        ServiceMessage::Ack { req: RequestId(7) },
        ServiceMessage::Error {
            req: RequestId(8),
            code: PublishErrorCode::UndeclaredType,
        },
    ]
}

/// One of each brick message.
pub fn brick_messages() -> Vec<BrickMessage> {
    vec![
        BrickMessage::Ready,
        BrickMessage::Subscribed {
            req: RequestId(1),
            flow: flow(),
        },
        BrickMessage::Error {
            req: RequestId(2),
            code: "no_such_repo".into(),
            message: "The remote does not exist.".into(),
        },
        BrickMessage::Publish {
            req: BIG_REQ,
            flow: flow(),
            data_type: "forge.pr/v1".into(),
            detail: br#"{"number":123,"state":"open"}"#.to_vec(),
        },
        BrickMessage::FlowError {
            flow: flow(),
            message: "GitHub answers 502.".into(),
        },
    ]
}

fn reversed(value: Value) -> Value {
    match value {
        Value::Map(mut entries) => {
            entries.reverse();
            Value::Map(entries)
        }
        other => other,
    }
}

fn check_frame(frame: &[u8]) {
    let (len, body) = frame
        .split_first_chunk::<4>()
        .expect("a frame starts with a 4-byte length");
    assert_eq!(
        u32::from_le_bytes(*len) as usize,
        body.len(),
        "the little-endian u32 prefix must count the bytes that follow"
    );
    let value = reference::unframe(frame).expect("the body is one MessagePack value");
    assert!(
        value.is_map(),
        "the body is a MessagePack map, found {value}"
    );
}

/// Every frame is a little-endian u32 length, then a MessagePack map.
pub fn frames_are_length_prefixed_maps<S: CodecSubject>() {
    for msg in service_messages() {
        check_frame(&S::encode_service(&msg));
    }
    for msg in brick_messages() {
        check_frame(&S::encode_brick(&msg));
    }
}

/// Service messages carry `v`, `t`, `req` and the fields of the contract's
/// table, `params` as `bin`.
pub fn encodes_service_messages<S: CodecSubject>() {
    for msg in service_messages() {
        let frame = S::encode_service(&msg);
        assert_eq!(
            reference::decode_service(&frame),
            Ok(msg.clone()),
            "{} does not read back as the contract spells it",
            msg.name()
        );
    }
}

/// Brick messages carry `v`, `t`, `req` and the fields of the contract's table,
/// `detail` as `bin`.
pub fn encodes_brick_messages<S: CodecSubject>() {
    for msg in brick_messages() {
        let frame = S::encode_brick(&msg);
        assert_eq!(
            reference::decode_brick(&frame),
            Ok(msg.clone()),
            "{} does not read back as the contract spells it",
            msg.name()
        );
    }
}

/// The codec reads every service message a service sends.
pub fn decodes_service_messages<S: CodecSubject>() {
    for msg in service_messages() {
        assert_eq!(
            S::decode_service(&reference::encode_service(&msg)),
            Ok(msg.clone())
        );
    }
}

/// The codec reads every brick message a brick sends.
pub fn decodes_brick_messages<S: CodecSubject>() {
    for msg in brick_messages() {
        assert_eq!(
            S::decode_brick(&reference::encode_brick(&msg)),
            Ok(msg.clone())
        );
    }
}

/// A message is a map: the order of its keys carries no meaning.
pub fn decoding_ignores_key_order<S: CodecSubject>() {
    for msg in service_messages() {
        let frame = reference::frame(&reversed(reference::service_value(&msg)));
        assert_eq!(S::decode_service(&frame), Ok(msg.clone()));
    }
    for msg in brick_messages() {
        let frame = reference::frame(&reversed(reference::brick_value(&msg)));
        assert_eq!(S::decode_brick(&frame), Ok(msg.clone()));
    }
}

/// A frame whose length does not match its body is refused.
pub fn refuses_a_wrong_length<S: CodecSubject>() {
    let mut frame = reference::encode_brick(&BrickMessage::Ready);
    frame.push(0xc0);
    assert!(S::decode_brick(&frame).is_err());
    let mut frame = reference::encode_service(&ServiceMessage::Shutdown);
    frame.pop();
    assert!(S::decode_service(&frame).is_err());
}

/// `params` and `detail` are `bin`: the same bytes sent as a string are refused.
pub fn refuses_json_sent_as_a_string<S: CodecSubject>() {
    let as_string = |value: Value, key: &str| match value {
        Value::Map(entries) => Value::Map(
            entries
                .into_iter()
                .map(|(k, v)| match (k.as_str() == Some(key), v) {
                    (true, Value::Binary(b)) => {
                        (k, Value::from(String::from_utf8(b).unwrap().as_str()))
                    }
                    (_, v) => (k, v),
                })
                .collect(),
        ),
        other => other,
    };
    let subscribe = &service_messages()[1];
    let frame = reference::frame(&as_string(reference::service_value(subscribe), "params"));
    assert!(
        S::decode_service(&frame).is_err(),
        "params sent as a string"
    );
    let publish = &brick_messages()[3];
    let frame = reference::frame(&as_string(reference::brick_value(publish), "detail"));
    assert!(S::decode_brick(&frame).is_err(), "detail sent as a string");
}

/// Runs the frame rules of contract section 4 against a codec.
#[macro_export]
macro_rules! codec_suite {
    ($subject:ty) => {
        mod codec_suite {
            #[allow(unused_imports)]
            use super::*;
            $crate::__tests!(
                sync,
                codec,
                $subject,
                [
                    frames_are_length_prefixed_maps,
                    encodes_service_messages,
                    encodes_brick_messages,
                    decodes_service_messages,
                    decodes_brick_messages,
                    decoding_ignores_key_order,
                    refuses_a_wrong_length,
                    refuses_json_sent_as_a_string,
                ]
            );
        }
    };
}
