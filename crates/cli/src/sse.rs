//! An incremental reader of Server-Sent Events, fed with the stream's chunks
//! as they come.

/// One event of the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field, `message` when absent.
    pub event: String,
    /// The `id:` field of this event, if it has one. Unlike a browser, the id
    /// is not carried over to the next events: the service only puts one on
    /// `update`.
    pub id: Option<String>,
    /// The `data:` lines, joined with `\n`.
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseParser {
    line: Vec<u8>,
    /// The last byte was a `\r`: a `\n` right after it ends the same line.
    after_cr: bool,
    event: Option<String>,
    id: Option<String>,
    data: Option<String>,
}

impl SseParser {
    /// Reads a chunk and appends the events it completes. Lines end with
    /// `\n`, `\r\n` or `\r`, possibly split across chunks.
    pub fn feed(&mut self, chunk: &[u8], events: &mut Vec<SseEvent>) {
        for &byte in chunk {
            if std::mem::take(&mut self.after_cr) && byte == b'\n' {
                continue;
            }
            match byte {
                b'\n' => self.end_line(events),
                b'\r' => {
                    self.end_line(events);
                    self.after_cr = true;
                }
                _ => self.line.push(byte),
            }
        }
    }

    fn end_line(&mut self, events: &mut Vec<SseEvent>) {
        let line = std::mem::take(&mut self.line);
        if line.is_empty() {
            events.extend(self.dispatch());
            return;
        }
        let line = String::from_utf8_lossy(&line);
        if line.starts_with(':') {
            return; // A comment, such as a keep-alive.
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (&*line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_owned()),
            },
            "id" if !value.contains('\0') => self.id = Some(value.to_owned()),
            _ => {}
        }
    }

    /// A blank line ends the event; one without data is dropped.
    fn dispatch(&mut self) -> Option<SseEvent> {
        let event = self.event.take();
        let id = self.id.take();
        let data = self.data.take()?;
        Some(SseEvent {
            event: event
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| "message".into()),
            id,
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut parser = SseParser::default();
        let mut events = Vec::new();
        for chunk in chunks {
            parser.feed(chunk, &mut events);
        }
        events
    }

    fn event(event: &str, id: Option<&str>, data: &str) -> SseEvent {
        SseEvent {
            event: event.into(),
            id: id.map(Into::into),
            data: data.into(),
        }
    }

    #[test]
    fn reads_an_update() {
        let events = parse(&[b"event: update\nid: 42\ndata: {\"a\":1}\n\n"]);
        assert_eq!(events, [event("update", Some("42"), "{\"a\":1}")]);
    }

    #[test]
    fn signals_have_no_id() {
        let events = parse(&[b"event: update\nid: 1\ndata: x\n\nevent: flow-ended\ndata: y\n\n"]);
        assert_eq!(
            events,
            [
                event("update", Some("1"), "x"),
                event("flow-ended", None, "y")
            ]
        );
    }

    #[test]
    fn every_line_ending_works() {
        let events = parse(&[b"data: a\r\n\r\ndata: b\r\rdata: c\n\n"]);
        assert_eq!(
            events,
            [
                event("message", None, "a"),
                event("message", None, "b"),
                event("message", None, "c")
            ]
        );
    }

    #[test]
    fn chunks_split_anywhere() {
        let whole: &[u8] = "event: update\r\nid: 7\r\ndata: {\"é\":1}\r\n\r\n".as_bytes();
        let expected = [event("update", Some("7"), "{\"é\":1}")];
        for at in 0..whole.len() {
            let (a, b) = whole.split_at(at);
            assert_eq!(parse(&[a, b]), expected, "split at {at}");
        }
        let bytes: Vec<&[u8]> = whole.chunks(1).collect();
        assert_eq!(parse(&bytes), expected);
    }

    #[test]
    fn data_lines_join_with_newlines() {
        let events = parse(&[b"data: a\ndata:b\ndata\n\n"]);
        assert_eq!(events, [event("message", None, "a\nb\n")]);
    }

    #[test]
    fn comments_and_unknown_fields_are_ignored() {
        let events = parse(&[b": keep-alive\n\nretry: 10\nfoo: bar\ndata: x\n\n"]);
        assert_eq!(events, [event("message", None, "x")]);
    }

    #[test]
    fn an_event_without_data_is_dropped() {
        let events = parse(&[b"event: update\nid: 3\n\ndata: x\n\n"]);
        assert_eq!(events, [event("message", None, "x")]);
    }

    #[test]
    fn an_unfinished_event_waits() {
        assert!(parse(&[b"event: update\ndata: x\n"]).is_empty());
    }

    #[test]
    fn an_id_with_nul_is_ignored() {
        let events = parse(&[b"id: a\0b\ndata: x\n\n"]);
        assert_eq!(events, [event("message", None, "x")]);
    }
}
