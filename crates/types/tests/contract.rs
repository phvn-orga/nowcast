//! Every message and error code named in `docs/design/03-contrats.md` (design
//! branch) has a type or a variant. The lists below are copied from it: when the
//! contract changes, update them along with the types.

use std::time::{Duration, SystemTime};

use nowcast_types::{
    ApiErrorCode, BrickMessage, BrickName, Event, EventId, FlowId, PROTOCOL_VERSION,
    PublishErrorCode, QualifiedFlowId, READY_TIMEOUT, RequestId, SPEC_VERSION, ServiceMessage,
    StreamEvent,
};

fn names<T>(items: &[T], name: impl Fn(&T) -> &'static str) -> Vec<&'static str> {
    items.iter().map(name).collect()
}

fn flow() -> FlowId {
    FlowId::new("github.com/org/repo#main").unwrap()
}

fn qualified() -> QualifiedFlowId {
    QualifiedFlowId::new(BrickName::new("forge").unwrap(), flow())
}

fn event() -> Event {
    Event {
        id: EventId(42),
        flow: qualified(),
        data_type: "forge.pr/v1".into(),
        time: SystemTime::UNIX_EPOCH,
        data: b"{}".to_vec(),
    }
}

#[test]
fn service_to_brick_messages() {
    let all = [
        ServiceMessage::Hello {
            name: BrickName::new("forge").unwrap(),
            protocol: PROTOCOL_VERSION,
        },
        ServiceMessage::Subscribe {
            req: RequestId(1),
            kind: "pr".into(),
            params: b"{}".to_vec(),
        },
        ServiceMessage::Release { flow: flow() },
        ServiceMessage::Refresh { flow: flow() },
        ServiceMessage::Shutdown,
        ServiceMessage::Ack { req: RequestId(7) },
        ServiceMessage::Error {
            req: RequestId(7),
            code: PublishErrorCode::UnknownFlow,
        },
    ];
    assert_eq!(
        names(&all, ServiceMessage::name),
        [
            "hello",
            "subscribe",
            "release",
            "refresh",
            "shutdown",
            "ack",
            "error"
        ]
    );
}

#[test]
fn brick_to_service_messages() {
    let all = [
        BrickMessage::Ready,
        BrickMessage::Subscribed {
            req: RequestId(1),
            flow: flow(),
        },
        BrickMessage::Error {
            req: RequestId(1),
            code: "not_found".into(),
            message: "no such repository".into(),
        },
        BrickMessage::Publish {
            req: RequestId(7),
            flow: flow(),
            data_type: "forge.pr/v1".into(),
            detail: b"{}".to_vec(),
        },
        BrickMessage::FlowError {
            flow: flow(),
            message: "rate limited".into(),
        },
    ];
    assert_eq!(
        names(&all, BrickMessage::name),
        ["ready", "subscribed", "error", "publish", "flow-error"]
    );
}

#[test]
fn stream_events() {
    let all = [
        StreamEvent::Update(event()),
        StreamEvent::FlowError {
            flow: qualified(),
            message: "rate limited".into(),
        },
        StreamEvent::FlowEnded { flow: qualified() },
        StreamEvent::FlowGap { flow: qualified() },
    ];
    assert_eq!(
        names(&all, StreamEvent::name),
        ["update", "flow-error", "flow-ended", "flow-gap"]
    );
}

#[test]
fn api_error_codes() {
    assert_eq!(
        names(ApiErrorCode::ALL, |c| c.as_str()),
        [
            "session_required",
            "invalid_params",
            "unknown_brick",
            "unknown_kind",
            "unknown_flow",
            "brick_unavailable",
            "subscription_refused"
        ]
    );
    for code in ApiErrorCode::ALL {
        assert_eq!(code.as_str().parse(), Ok(*code));
    }
    assert!("nope".parse::<ApiErrorCode>().is_err());
}

#[test]
fn publish_error_codes() {
    assert_eq!(
        names(PublishErrorCode::ALL, |c| c.as_str()),
        ["invalid_json", "unknown_flow", "undeclared_type"]
    );
    for code in PublishErrorCode::ALL {
        assert_eq!(code.as_str().parse(), Ok(*code));
    }
    assert!("nope".parse::<PublishErrorCode>().is_err());
}

#[test]
fn event_attributes() {
    let event = event();
    assert_eq!(SPEC_VERSION, "1.0");
    assert_eq!(event.source(), "nowcast://bricks/forge");
    assert_eq!(
        event.subject().to_string(),
        "forge/github.com/org/repo#main"
    );
    assert_eq!(
        event.dataschema(),
        "nowcast://bricks/forge/types/forge.pr/v1"
    );
}

#[test]
fn protocol_constants() {
    assert_eq!(PROTOCOL_VERSION, 1);
    assert_eq!(READY_TIMEOUT, Duration::from_secs(5));
}
