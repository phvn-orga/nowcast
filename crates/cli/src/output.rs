//! What the commands print on stdout: readable lines by default, one JSON
//! value per line with `--json`.

use std::io::{self, Write};

use serde_json::{Value, json};

/// `get`: the flow's last event.
///
/// Readable: `<flow> <type> <time>`, then the detail as indented JSON.
/// JSON: the CloudEvents event as the service sent it, `null` without value.
pub fn last_value(out: &mut impl Write, json: bool, event: Option<&Value>) -> io::Result<()> {
    match (event, json) {
        (None, true) => writeln!(out, "null")?,
        (None, false) => {}
        (Some(event), true) => writeln!(out, "{event}")?,
        (Some(event), false) => {
            writeln!(
                out,
                "{} {} {}",
                text(event, "subject"),
                text(event, "type"),
                text(event, "time")
            )?;
            let detail = event.get("data").unwrap_or(&Value::Null);
            let pretty = serde_json::to_string_pretty(detail).map_err(io::Error::other)?;
            writeln!(out, "{pretty}")?;
        }
    }
    out.flush()
}

/// `subscribe`: what the service answered.
///
/// Readable: `subscription <id>`, `flow <flow>`, and `last <detail>` when the
/// flow already has a value. JSON: the answer as is.
pub fn subscribed(out: &mut impl Write, json: bool, body: &Value) -> io::Result<()> {
    if json {
        writeln!(out, "{body}")?;
    } else {
        writeln!(out, "subscription {}", text(body, "subscription"))?;
        writeln!(out, "flow {}", text(body, "flow"))?;
        if let Some(last) = body.get("last").filter(|last| !last.is_null()) {
            writeln!(out, "last {}", last.get("data").unwrap_or(&Value::Null))?;
        }
    }
    out.flush()
}

/// `watch`: one line per event of the stream.
///
/// Readable: `update <flow> <detail>`, `flow-error <flow> <message>`,
/// `flow-ended <flow>`. JSON: `{"event": <name>, "data": <data>}`.
pub fn stream_event(out: &mut impl Write, json: bool, name: &str, data: &Value) -> io::Result<()> {
    if json {
        writeln!(out, "{}", json!({ "event": name, "data": data }))?;
    } else {
        match name {
            "update" => writeln!(
                out,
                "update {} {}",
                text(data, "subject"),
                data.get("data").unwrap_or(&Value::Null)
            )?,
            "flow-error" => writeln!(
                out,
                "flow-error {} {}",
                text(data, "flow"),
                text(data, "message")
            )?,
            _ => writeln!(out, "{name} {}", text(data, "flow"))?,
        }
    }
    out.flush()
}

/// A string field, `-` when absent.
fn text<'a>(value: &'a Value, field: &str) -> &'a str {
    value.get(field).and_then(Value::as_str).unwrap_or("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> Value {
        json!({
            "specversion": "1.0",
            "id": "42",
            "source": "nowcast://bricks/forge",
            "type": "forge.pr/v1",
            "subject": "forge/github.com/org/repo#main",
            "time": "2026-11-05T13:30:00Z",
            "dataschema": "nowcast://bricks/forge/types/forge.pr/v1",
            "data": { "number": 123, "state": "open" }
        })
    }

    fn printed(write: impl FnOnce(&mut Vec<u8>) -> io::Result<()>) -> String {
        let mut out = Vec::new();
        write(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_readable_last_value() {
        let text = printed(|out| last_value(out, false, Some(&event())));
        assert_eq!(
            text,
            "forge/github.com/org/repo#main forge.pr/v1 2026-11-05T13:30:00Z\n\
             {\n  \"number\": 123,\n  \"state\": \"open\"\n}\n"
        );
    }

    #[test]
    fn a_json_last_value_is_the_event() {
        let text = printed(|out| last_value(out, true, Some(&event())));
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), event());
        assert_eq!(text.lines().count(), 1);
    }

    #[test]
    fn no_value() {
        assert_eq!(printed(|out| last_value(out, true, None)), "null\n");
        assert_eq!(printed(|out| last_value(out, false, None)), "");
    }

    #[test]
    fn a_readable_subscription() {
        let body = json!({ "subscription": "s-1", "flow": "forge/x", "last": null });
        assert_eq!(
            printed(|out| subscribed(out, false, &body)),
            "subscription s-1\nflow forge/x\n"
        );
        let body = json!({ "subscription": "s-1", "flow": "forge/x", "last": event() });
        assert_eq!(
            printed(|out| subscribed(out, false, &body)),
            "subscription s-1\nflow forge/x\nlast {\"number\":123,\"state\":\"open\"}\n"
        );
    }

    #[test]
    fn readable_stream_events() {
        assert_eq!(
            printed(|out| stream_event(out, false, "update", &event())),
            "update forge/github.com/org/repo#main {\"number\":123,\"state\":\"open\"}\n"
        );
        let error = json!({ "flow": "forge/x", "message": "source down" });
        assert_eq!(
            printed(|out| stream_event(out, false, "flow-error", &error)),
            "flow-error forge/x source down\n"
        );
        let ended = json!({ "flow": "forge/x" });
        assert_eq!(
            printed(|out| stream_event(out, false, "flow-ended", &ended)),
            "flow-ended forge/x\n"
        );
    }

    #[test]
    fn json_stream_events_keep_their_name() {
        let ended = json!({ "flow": "forge/x" });
        let text = printed(|out| stream_event(out, true, "flow-ended", &ended));
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!({ "event": "flow-ended", "data": { "flow": "forge/x" } })
        );
    }

    #[test]
    fn missing_fields_show_as_dashes() {
        assert_eq!(
            printed(|out| stream_event(out, false, "flow-error", &json!({}))),
            "flow-error - -\n"
        );
    }
}
