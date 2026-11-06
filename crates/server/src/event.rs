//! Events as JSON: CloudEvents for `update`, small objects for the signals.

use std::fmt;

use jiff::Timestamp;
use nowcast_types::{Event, QualifiedFlowId, SPEC_VERSION};
use serde::Serialize;
use serde_json::value::RawValue;

/// An event that cannot be written: its detail is not JSON, or its time is out
/// of range. The router only stores checked details, so this is a bug upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InvalidEvent {
    pub id: u64,
    pub reason: String,
}

impl fmt::Display for InvalidEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "event {} cannot be written: {}", self.id, self.reason)
    }
}

impl std::error::Error for InvalidEvent {}

/// An event in the CloudEvents JSON format, its `data` the brick's detail as
/// published.
#[derive(Debug, Serialize)]
pub(crate) struct CloudEvent<'a> {
    specversion: &'static str,
    id: String,
    source: String,
    #[serde(rename = "type")]
    data_type: &'a str,
    subject: String,
    time: String,
    dataschema: String,
    data: &'a RawValue,
}

impl<'a> CloudEvent<'a> {
    pub fn new(event: &'a Event) -> Result<Self, InvalidEvent> {
        let invalid = |reason: String| InvalidEvent {
            id: event.id.0,
            reason,
        };
        let data: &RawValue = serde_json::from_slice(&event.data)
            .map_err(|e| invalid(format!("the detail is not JSON: {e}")))?;
        let time = Timestamp::try_from(event.time)
            .map_err(|e| invalid(format!("the time is out of range: {e}")))?;
        Ok(Self {
            specversion: SPEC_VERSION,
            id: event.id.to_string(),
            source: event.source(),
            data_type: &event.data_type,
            subject: event.subject().to_string(),
            time: time.to_string(),
            dataschema: event.dataschema(),
            data,
        })
    }
}

/// `data` of a `flow-error`.
#[derive(Debug, Serialize)]
pub(crate) struct FlowError<'a> {
    pub flow: String,
    pub message: &'a str,
}

/// `data` of a `flow-ended`.
#[derive(Debug, Serialize)]
pub(crate) struct FlowEnded {
    pub flow: String,
}

impl FlowEnded {
    pub fn new(flow: &QualifiedFlowId) -> Self {
        Self {
            flow: flow.to_string(),
        }
    }
}

/// Serializes `value` on a single line, as an SSE `data` field needs.
///
/// A raw detail may be pretty-printed. A line break in valid JSON can only be
/// whitespace between tokens (strings escape theirs), so turning it into a
/// space changes no value.
pub(crate) fn to_line(value: &impl Serialize) -> String {
    let json = serde_json::to_string(value).expect("these values always serialize");
    if json.contains(['\n', '\r']) {
        json.replace(['\n', '\r'], " ")
    } else {
        json
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use nowcast_types::EventId;
    use serde_json::{Value, json};

    use super::*;

    fn event(data: &[u8]) -> Event {
        Event {
            id: EventId(42),
            flow: QualifiedFlowId::parse("forge/github.com/org/repo#main").unwrap(),
            data_type: "forge.pr/v1".into(),
            time: SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_971_800),
            data: data.to_vec(),
        }
    }

    #[test]
    fn an_event_has_every_attribute_of_the_contract() {
        let event = event(br#"{"number":123,"state":"open"}"#);
        let json = to_line(&CloudEvent::new(&event).unwrap());
        assert_eq!(
            json,
            r#"{"specversion":"1.0","id":"42","source":"nowcast://bricks/forge","type":"forge.pr/v1","subject":"forge/github.com/org/repo#main","time":"2026-11-06T13:30:00Z","dataschema":"nowcast://bricks/forge/types/forge.pr/v1","data":{"number":123,"state":"open"}}"#
        );
    }

    #[test]
    fn sub_second_times_keep_their_fraction() {
        let mut event = event(b"{}");
        event.time += Duration::from_millis(250);
        let json: Value =
            serde_json::from_str(&to_line(&CloudEvent::new(&event).unwrap())).unwrap();
        assert_eq!(json["time"], "2026-11-06T13:30:00.25Z");
    }

    #[test]
    fn the_detail_passes_through_untouched() {
        // Numbers a JSON value type would round or reformat.
        let detail =
            r#"{"big":123456789012345678901234567890,"f":1.10,"e":1E400,"u":"\u00e9","v":"é"}"#;
        let json = to_line(&CloudEvent::new(&event(detail.as_bytes())).unwrap());
        let start = json.find(r#""data":"#).unwrap() + 7;
        assert_eq!(&json[start..json.len() - 1], detail);
    }

    #[test]
    fn a_pretty_printed_detail_fits_on_one_line() {
        let detail = b"\n  {\r\n  \"a\": [1,\n 2],\n  \"s\": \"x\\ny\"\n}\n";
        let json = to_line(&CloudEvent::new(&event(detail)).unwrap());
        assert!(!json.contains(['\n', '\r']), "{json}");
        let value: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["data"], json!({ "a": [1, 2], "s": "x\ny" }));
    }

    #[test]
    fn a_detail_that_is_not_json_is_refused() {
        for detail in [&b""[..], b"{", b"{} {}", b"\xff"] {
            let error = CloudEvent::new(&event(detail)).unwrap_err();
            assert_eq!(error.id, 42);
        }
    }

    #[test]
    fn a_time_before_the_epoch_is_written() {
        let mut event = event(b"null");
        event.time = SystemTime::UNIX_EPOCH - Duration::from_secs(86_400);
        let json: Value =
            serde_json::from_str(&to_line(&CloudEvent::new(&event).unwrap())).unwrap();
        assert_eq!(json["time"], "1969-12-31T00:00:00Z");
    }

    #[test]
    fn signals_have_the_contract_shape() {
        let flow = QualifiedFlowId::parse("forge/a/b").unwrap();
        assert_eq!(
            to_line(&FlowError {
                flow: flow.to_string(),
                message: "line\nbreak"
            }),
            r#"{"flow":"forge/a/b","message":"line\nbreak"}"#
        );
        assert_eq!(to_line(&FlowEnded::new(&flow)), r#"{"flow":"forge/a/b"}"#);
    }
}
