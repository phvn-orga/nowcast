//! Encoding and decoding of whole frames.

use nowcast_codec::{
    CodecError, MAX_FRAME_LEN, PREFIX_LEN, decode_brick, decode_service, encode_brick,
    encode_service, unframe,
};
use nowcast_conformance::codec::{brick_messages, service_messages};
use nowcast_conformance::reference;
use nowcast_types::{
    BrickMessage, BrickName, FLOW_ID_MAX_LEN, FlowId, PROTOCOL_VERSION, PublishErrorCode,
    RequestId, ServiceMessage,
};
use rmpv::Value;

fn flow(id: &str) -> FlowId {
    FlowId::new(id).unwrap()
}

/// The suite's messages, plus the edges of each field.
fn all_service_messages() -> Vec<ServiceMessage> {
    let mut msgs = service_messages();
    msgs.extend([
        ServiceMessage::Subscribe {
            req: RequestId(0),
            kind: String::new(),
            params: Vec::new(),
        },
        ServiceMessage::Subscribe {
            req: RequestId(u64::MAX),
            kind: "pr".into(),
            // The codec carries the bytes, it does not check they are JSON.
            params: vec![0xff, 0x00, b'{'],
        },
        ServiceMessage::Hello {
            name: BrickName::new("a-0").unwrap(),
            protocol: u32::MAX,
        },
        ServiceMessage::Release {
            flow: flow(&"é".repeat(FLOW_ID_MAX_LEN / 2)),
        },
        ServiceMessage::Refresh { flow: flow("") },
        ServiceMessage::Input {
            flow: flow("a"),
            data_type: String::new(),
            // The codec carries the bytes, it does not check they are JSON.
            detail: vec![0xff],
        },
        ServiceMessage::Ack {
            req: RequestId(u64::MAX),
        },
    ]);
    msgs.extend(
        PublishErrorCode::ALL
            .iter()
            .map(|&code| ServiceMessage::Error {
                req: RequestId(3),
                code,
            }),
    );
    msgs
}

fn all_brick_messages() -> Vec<BrickMessage> {
    let mut msgs = brick_messages();
    msgs.extend([
        BrickMessage::Subscribed {
            req: RequestId(u64::MAX),
            flow: flow(&"x".repeat(FLOW_ID_MAX_LEN)),
        },
        BrickMessage::Error {
            req: RequestId(0),
            code: String::new(),
            message: "« unicode » ✓".into(),
        },
        BrickMessage::Publish {
            req: RequestId(0),
            flow: flow("f"),
            data_type: "t/v1".into(),
            detail: Vec::new(),
        },
        BrickMessage::Publish {
            req: RequestId(1),
            flow: flow("f"),
            data_type: "t/v1".into(),
            detail: vec![b'1'; 1024 * 1024],
        },
        BrickMessage::FlowError {
            flow: flow("f"),
            message: String::new(),
        },
    ]);
    msgs
}

/// The body of a frame as a list of `(key, value)`.
fn entries(frame: &[u8]) -> Vec<(String, Value)> {
    let Value::Map(entries) = reference::unframe(frame).unwrap() else {
        panic!("a frame holds a map");
    };
    entries
        .into_iter()
        .map(|(k, v)| (k.as_str().unwrap().to_owned(), v))
        .collect()
}

fn get<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn service_req(msg: &ServiceMessage) -> Option<u64> {
    match msg {
        ServiceMessage::Subscribe { req, .. }
        | ServiceMessage::Ack { req }
        | ServiceMessage::Error { req, .. } => Some(req.0),
        _ => None,
    }
}

fn brick_req(msg: &BrickMessage) -> Option<u64> {
    match msg {
        BrickMessage::Subscribed { req, .. }
        | BrickMessage::Error { req, .. }
        | BrickMessage::Publish { req, .. } => Some(req.0),
        _ => None,
    }
}

fn assert_envelope(frame: &[u8], t: &str, req: Option<u64>) {
    let entries = entries(frame);
    assert_eq!(
        get(&entries, "v"),
        Some(&Value::from(PROTOCOL_VERSION)),
        "{t}: v"
    );
    assert_eq!(get(&entries, "t"), Some(&Value::from(t)), "{t}: t");
    assert_eq!(
        get(&entries, "req"),
        req.map(Value::from).as_ref(),
        "{t}: req"
    );
}

#[test]
fn every_service_message_round_trips_with_v_t_and_req() {
    for msg in all_service_messages() {
        let frame = encode_service(&msg).unwrap();
        assert_envelope(&frame, msg.name(), service_req(&msg));
        assert_eq!(decode_service(&frame).unwrap(), msg);
    }
}

#[test]
fn every_brick_message_round_trips_with_v_t_and_req() {
    for msg in all_brick_messages() {
        let frame = encode_brick(&msg).unwrap();
        assert_envelope(&frame, msg.name(), brick_req(&msg));
        assert_eq!(decode_brick(&frame).unwrap(), msg);
    }
}

#[test]
fn encoding_matches_the_reference_byte_for_byte() {
    for msg in all_service_messages() {
        assert_eq!(
            encode_service(&msg).unwrap(),
            reference::encode_service(&msg)
        );
    }
    for msg in all_brick_messages() {
        assert_eq!(encode_brick(&msg).unwrap(), reference::encode_brick(&msg));
    }
}

#[test]
fn unframe_returns_the_body() {
    let frame = encode_brick(&BrickMessage::Ready).unwrap();
    assert_eq!(unframe(&frame).unwrap(), &frame[PREFIX_LEN..]);
}

#[test]
fn every_truncation_of_a_frame_is_refused() {
    let frames = all_service_messages()
        .iter()
        .map(|m| encode_service(m).unwrap())
        .chain(
            all_brick_messages()
                .iter()
                .map(|m| encode_brick(m).unwrap()),
        )
        .filter(|frame| frame.len() < 4096)
        .collect::<Vec<_>>();
    for frame in frames {
        for len in 0..frame.len() {
            let cut = &frame[..len];
            assert!(decode_service(cut).is_err(), "{len} bytes of {frame:?}");
            assert!(decode_brick(cut).is_err(), "{len} bytes of {frame:?}");
        }
    }
}

#[test]
fn a_frame_shorter_than_its_prefix_is_truncated() {
    assert!(matches!(
        decode_brick(&[1, 0]),
        Err(CodecError::Truncated {
            expected: 4,
            received: 2
        })
    ));
}

#[test]
fn a_length_that_does_not_match_the_body_is_refused() {
    let mut frame = encode_brick(&BrickMessage::Ready).unwrap();
    frame.push(0);
    assert!(matches!(
        decode_brick(&frame),
        Err(CodecError::LengthMismatch { .. })
    ));
}

#[test]
fn a_non_map_payload_is_refused() {
    let payloads = [
        Value::Nil,
        Value::from(1),
        Value::from("ready"),
        Value::Binary(vec![1, 2]),
        Value::Array(vec![Value::from("v"), Value::from(1)]),
    ];
    for payload in payloads {
        let frame = reference::frame(&payload);
        assert!(
            matches!(decode_brick(&frame), Err(CodecError::Malformed(_))),
            "{payload}"
        );
        assert!(
            matches!(decode_service(&frame), Err(CodecError::Malformed(_))),
            "{payload}"
        );
    }
}

fn raw(body: &[u8]) -> Vec<u8> {
    let mut frame = u32::try_from(body.len()).unwrap().to_le_bytes().to_vec();
    frame.extend_from_slice(body);
    frame
}

#[test]
fn invalid_message_pack_is_refused() {
    // 0xc1 is a reserved marker; 0x81 announces a one-entry map with none.
    for body in [&[][..], &[0xc1], &[0x81], &[0xdf, 0xff, 0xff, 0xff, 0xff]] {
        assert!(
            matches!(decode_brick(&raw(body)), Err(CodecError::Malformed(_))),
            "{body:?}"
        );
    }
}

#[test]
fn bytes_after_the_map_are_refused() {
    let mut body = encode_brick(&BrickMessage::Ready).unwrap()[PREFIX_LEN..].to_vec();
    body.push(0xc0);
    assert!(matches!(
        decode_brick(&raw(&body)),
        Err(CodecError::Malformed(_))
    ));
}

#[test]
fn deep_nesting_is_refused_without_overflowing_the_stack() {
    // An unknown key holding 100 000 nested one-element arrays.
    let mut body = vec![0x83, 0xa1, b'v', 0x01, 0xa1, b't', 0xa5];
    body.extend_from_slice(b"ready");
    body.extend_from_slice(&[0xa1, b'x']);
    body.extend(std::iter::repeat_n(0x91, 100_000));
    body.push(0xc0);
    assert!(matches!(
        decode_brick(&raw(&body)),
        Err(CodecError::Malformed(_))
    ));
}

#[test]
fn an_oversized_length_is_refused() {
    let len = u32::try_from(MAX_FRAME_LEN + 1).unwrap();
    let frame = len.to_le_bytes();
    assert!(matches!(
        decode_brick(&frame),
        Err(CodecError::TooLarge { len, max: MAX_FRAME_LEN }) if len == MAX_FRAME_LEN + 1
    ));
    assert!(matches!(
        decode_service(&u32::MAX.to_le_bytes()),
        Err(CodecError::TooLarge { .. })
    ));
}

#[test]
fn an_oversized_message_is_not_encoded() {
    let publish = BrickMessage::Publish {
        req: RequestId(1),
        flow: flow("f"),
        data_type: "t/v1".into(),
        detail: vec![0; MAX_FRAME_LEN],
    };
    assert!(matches!(
        encode_brick(&publish),
        Err(CodecError::TooLarge {
            max: MAX_FRAME_LEN,
            ..
        })
    ));
    let subscribe = ServiceMessage::Subscribe {
        req: RequestId(1),
        kind: "pr".into(),
        params: vec![0; MAX_FRAME_LEN],
    };
    assert!(matches!(
        encode_service(&subscribe),
        Err(CodecError::TooLarge { .. })
    ));
}

/// The value of a message with one key replaced, added, or removed (`None`).
fn with(value: Value, key: &str, new: Option<Value>) -> Value {
    let Value::Map(mut entries) = value else {
        unreachable!()
    };
    let at = entries.iter().position(|(k, _)| k.as_str() == Some(key));
    match (at, new) {
        (Some(i), Some(new)) => entries[i].1 = new,
        (Some(i), None) => {
            entries.remove(i);
        }
        (None, Some(new)) => entries.push((Value::from(key), new)),
        (None, None) => {}
    }
    Value::Map(entries)
}

fn brick_frame(msg: &BrickMessage, key: &str, new: Option<Value>) -> Vec<u8> {
    reference::frame(&with(reference::brick_value(msg), key, new))
}

fn service_frame(msg: &ServiceMessage, key: &str, new: Option<Value>) -> Vec<u8> {
    reference::frame(&with(reference::service_value(msg), key, new))
}

/// The service's `error` among the conformance messages.
fn error_message() -> ServiceMessage {
    service_messages()
        .into_iter()
        .find(|m| matches!(m, ServiceMessage::Error { .. }))
        .unwrap()
}

fn publish() -> BrickMessage {
    brick_messages().remove(3)
}

fn subscribe() -> ServiceMessage {
    service_messages().remove(1)
}

#[test]
fn another_protocol_version_is_refused() {
    let frame = brick_frame(&BrickMessage::Ready, "v", Some(Value::from(2)));
    assert!(matches!(
        decode_brick(&frame),
        Err(CodecError::UnsupportedVersion(2))
    ));
    let frame = service_frame(&ServiceMessage::Shutdown, "v", Some(Value::from(0)));
    assert!(matches!(
        decode_service(&frame),
        Err(CodecError::UnsupportedVersion(0))
    ));
}

#[test]
fn a_missing_or_mistyped_version_is_refused() {
    for v in [
        None,
        Some(Value::from("1")),
        Some(Value::from(-1)),
        Some(Value::F64(1.0)),
    ] {
        let frame = brick_frame(&BrickMessage::Ready, "v", v.clone());
        assert!(
            matches!(decode_brick(&frame), Err(CodecError::Malformed(_))),
            "{v:?}"
        );
    }
}

#[test]
fn an_unknown_type_is_refused() {
    let frame = brick_frame(&BrickMessage::Ready, "t", Some(Value::from("readyy")));
    assert!(matches!(
        decode_brick(&frame),
        Err(CodecError::UnknownType(t)) if t == "readyy"
    ));
}

#[test]
fn a_long_unknown_type_is_clipped_in_the_error() {
    let t = "x".repeat(10_000);
    let frame = brick_frame(&BrickMessage::Ready, "t", Some(Value::from(t.as_str())));
    let Err(CodecError::UnknownType(reported)) = decode_brick(&frame) else {
        panic!("an unknown type");
    };
    assert!(reported.len() < 100, "{} bytes", reported.len());
    assert!(t.starts_with(reported.trim_end_matches('.')));
}

#[test]
fn a_message_of_the_other_direction_is_refused() {
    let ready = encode_brick(&BrickMessage::Ready).unwrap();
    assert!(matches!(
        decode_service(&ready),
        Err(CodecError::UnknownType(t)) if t == "ready"
    ));
    let hello = encode_service(&service_messages()[0]).unwrap();
    assert!(matches!(
        decode_brick(&hello),
        Err(CodecError::UnknownType(t)) if t == "hello"
    ));
}

#[test]
fn a_missing_type_is_refused() {
    let frame = brick_frame(&BrickMessage::Ready, "t", None);
    assert!(matches!(
        decode_brick(&frame),
        Err(CodecError::Malformed(_))
    ));
}

#[test]
fn every_missing_field_is_refused() {
    for msg in all_brick_messages() {
        for (key, _) in entries(&encode_brick(&msg).unwrap()) {
            let frame = brick_frame(&msg, &key, None);
            assert!(
                decode_brick(&frame).is_err(),
                "{} without {key}",
                msg.name()
            );
        }
    }
    for msg in all_service_messages() {
        for (key, _) in entries(&encode_service(&msg).unwrap()) {
            let frame = service_frame(&msg, &key, None);
            assert!(
                decode_service(&frame).is_err(),
                "{} without {key}",
                msg.name()
            );
        }
    }
}

#[test]
fn fields_of_the_wrong_type_are_refused() {
    let brick = |key: &str, value: Value| {
        decode_brick(&brick_frame(&publish(), key, Some(value))).map(|_| ())
    };
    let service = |key: &str, value: Value| {
        decode_service(&service_frame(&subscribe(), key, Some(value))).map(|_| ())
    };
    let cases = [
        ("negative req", brick("req", Value::from(-1))),
        ("req as a string", brick("req", Value::from("7"))),
        ("req as a float", brick("req", Value::F64(7.0))),
        ("flow as bin", brick("flow", Value::Binary(b"f".to_vec()))),
        ("type as nil", brick("type", Value::Nil)),
        ("detail as a string", brick("detail", Value::from("{}"))),
        ("detail as an array", brick("detail", Value::Array(vec![]))),
        ("params as a string", service("params", Value::from("{}"))),
        ("params as a map", service("params", Value::Map(vec![]))),
        ("kind as an integer", service("kind", Value::from(1))),
    ];
    for (case, decoded) in cases {
        assert!(
            matches!(decoded, Err(CodecError::Malformed(_))),
            "{case}: {decoded:?}"
        );
    }
}

#[test]
fn field_values_outside_the_contract_are_refused() {
    let long_flow = Value::from("x".repeat(FLOW_ID_MAX_LEN + 1).as_str());
    let hello = service_messages().remove(0);
    let error = error_message();
    let cases = [
        decode_brick(&brick_frame(&publish(), "flow", Some(long_flow.clone()))).map(|_| ()),
        decode_service(&service_frame(
            &ServiceMessage::Release { flow: flow("f") },
            "flow",
            Some(long_flow),
        ))
        .map(|_| ()),
        decode_service(&service_frame(&hello, "name", Some(Value::from("Forge")))).map(|_| ()),
        decode_service(&service_frame(
            &hello,
            "protocol",
            Some(Value::from(u64::from(u32::MAX) + 1)),
        ))
        .map(|_| ()),
        decode_service(&service_frame(&error, "code", Some(Value::from("nope")))).map(|_| ()),
    ];
    for (i, decoded) in cases.into_iter().enumerate() {
        assert!(
            matches!(decoded, Err(CodecError::Malformed(_))),
            "case {i}: {decoded:?}"
        );
    }
}

#[test]
fn unknown_keys_are_ignored() {
    let frame = brick_frame(
        &publish(),
        "later",
        Some(Value::Array(vec![Value::from(1)])),
    );
    assert_eq!(decode_brick(&frame).unwrap(), publish());
}

#[test]
fn a_duplicate_key_is_refused() {
    let Value::Map(mut entries) = reference::brick_value(&publish()) else {
        unreachable!()
    };
    entries.push((Value::from("flow"), Value::from("other")));
    let frame = reference::frame(&Value::Map(entries));
    assert!(matches!(
        decode_brick(&frame),
        Err(CodecError::Malformed(_))
    ));
}

#[test]
fn a_non_string_key_is_refused() {
    let Value::Map(mut entries) = reference::brick_value(&BrickMessage::Ready) else {
        unreachable!()
    };
    entries.push((Value::from(1), Value::from(1)));
    let frame = reference::frame(&Value::Map(entries));
    assert!(matches!(
        decode_brick(&frame),
        Err(CodecError::Malformed(_))
    ));
}

#[test]
fn a_string_that_is_not_utf8_is_refused() {
    // {"v": 1, "t": "flow-error", "flow": <str ff>, "message": ""}
    let mut body = vec![0x84, 0xa1, b'v', 0x01, 0xa1, b't', 0xaa];
    body.extend_from_slice(b"flow-error");
    body.extend_from_slice(&[0xa4, b'f', b'l', b'o', b'w', 0xa1, 0xff]);
    body.extend_from_slice(&[0xa7, b'm', b'e', b's', b's', b'a', b'g', b'e', 0xa0]);
    assert!(matches!(
        decode_brick(&raw(&body)),
        Err(CodecError::Malformed(_))
    ));
}

#[test]
fn integers_in_any_width_are_read() {
    // A peer may write `v` as a u64 and `req` as a positive i64.
    // {"v": u64 1, "t": "ack", "req": i64 7}
    let mut body = vec![0x83, 0xa1, b'v', 0xcf, 0, 0, 0, 0, 0, 0, 0, 1];
    body.extend_from_slice(&[0xa1, b't', 0xa3, b'a', b'c', b'k']);
    body.extend_from_slice(&[0xa3, b'r', b'e', b'q', 0xd3, 0, 0, 0, 0, 0, 0, 0, 7]);
    assert_eq!(
        decode_service(&raw(&body)).unwrap(),
        ServiceMessage::Ack { req: RequestId(7) }
    );
}

/// A small deterministic generator, so that the test needs no extra crate.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next()).unwrap() % n
    }
}

#[test]
fn corrupted_frames_never_panic() {
    let frames: Vec<Vec<u8>> = service_messages()
        .iter()
        .map(|m| encode_service(m).unwrap())
        .chain(brick_messages().iter().map(|m| encode_brick(m).unwrap()))
        .collect();
    let mut rng = Lcg(42);
    for _ in 0..20_000 {
        let mut frame = frames[rng.below(frames.len())].clone();
        for _ in 0..=rng.below(4) {
            let at = rng.below(frame.len());
            frame[at] = u8::try_from(rng.next() & 0xff).unwrap();
        }
        // Either answer is fine; reaching the next line is the test.
        let _ = decode_service(&frame);
        let _ = decode_brick(&frame);
    }
}

#[test]
fn a_map_with_many_keys_decodes_in_linear_time() {
    // 200 000 distinct unknown keys: under 2 MB, far below the frame limit. A
    // duplicate check comparing every key with every other takes minutes here.
    const KEYS: u32 = 200_000;
    let mut body = vec![0xdf];
    body.extend_from_slice(&(KEYS + 2).to_be_bytes());
    body.extend_from_slice(&[0xa1, b'v', 0x01, 0xa1, b't', 0xa5]);
    body.extend_from_slice(b"ready");
    for i in 0..KEYS {
        body.push(0xa8);
        body.extend_from_slice(format!("{i:08x}").as_bytes());
        body.push(0xc0);
    }
    let frame = raw(&body);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(decode_brick(&frame).map(|_| ())));
    let decoded = rx
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("decoding finishes within 20 s");
    assert!(decoded.is_ok(), "{decoded:?}");
}

#[test]
fn an_unknown_error_code_is_clipped_in_the_error() {
    let error = error_message();
    let code = "x".repeat(10_000);
    let frame = service_frame(&error, "code", Some(Value::from(code.as_str())));
    let reported = decode_service(&frame).unwrap_err().to_string();
    assert!(reported.len() < 200, "{} bytes", reported.len());
}
