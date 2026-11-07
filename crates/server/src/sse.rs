//! The session's event stream, as Server-Sent Events.

use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use hyper::body::{Body, Frame};
use nowcast_types::{EventId, StreamEvent};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{Instant, Interval, MissedTickBehavior, interval_at};

use crate::event::{CloudEvent, FlowError, FlowSignal, to_line};

/// How often an idle stream sends a comment. Writing is how a closed client
/// is noticed, and its stream released.
pub(crate) const KEEP_ALIVE: Duration = Duration::from_secs(15);

const KEEP_ALIVE_FRAME: &[u8] = b": keep-alive\n\n";

/// Reads `Last-Event-ID`. A value that is not an event id is ignored: the
/// client then receives every last value again, which it can recognize by
/// their ids, rather than miss one.
pub(crate) fn last_event_id(header: Option<&[u8]>) -> Option<EventId> {
    let value = std::str::from_utf8(header?).ok()?;
    value.trim().parse().ok().map(EventId)
}

/// One SSE block. Only `update` carries an `id`: the signals are not replayed.
/// `None` for an event that cannot be written, which is logged and skipped.
pub(crate) fn encode(event: &StreamEvent) -> Option<Bytes> {
    let (id, data) = match event {
        StreamEvent::Update(update) => match CloudEvent::new(update) {
            Ok(cloud) => (Some(update.id), to_line(&cloud)),
            Err(e) => {
                tracing::warn!("skipping an update on the stream: {e}");
                return None;
            }
        },
        StreamEvent::FlowError { flow, message } => (
            None,
            to_line(&FlowError {
                flow: flow.to_string(),
                message,
            }),
        ),
        StreamEvent::FlowEnded { flow } | StreamEvent::FlowGap { flow } => {
            (None, to_line(&FlowSignal::new(flow)))
        }
    };
    let mut block = format!("event: {}\n", event.name());
    if let Some(id) = id {
        block.push_str(&format!("id: {id}\n"));
    }
    // `data` is one line by construction; split anyway, as SSE requires.
    for line in data.split('\n') {
        block.push_str("data: ");
        block.push_str(line);
        block.push('\n');
    }
    block.push('\n');
    Some(Bytes::from(block))
}

/// A response body that never ends on its own: it closes when the router
/// drops the stream (the anchor is gone) or when the client disconnects.
pub(crate) struct SseBody {
    events: UnboundedReceiver<StreamEvent>,
    keep_alive: Interval,
}

impl SseBody {
    pub fn new(events: UnboundedReceiver<StreamEvent>) -> Self {
        let mut keep_alive = interval_at(Instant::now() + KEEP_ALIVE, KEEP_ALIVE);
        keep_alive.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Self { events, keep_alive }
    }
}

impl Body for SseBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        loop {
            match self.events.poll_recv(cx) {
                Poll::Ready(Some(event)) => match encode(&event) {
                    Some(block) => {
                        self.keep_alive.reset();
                        return Poll::Ready(Some(Ok(Frame::data(block))));
                    }
                    None => continue,
                },
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => break,
            }
        }
        match self.keep_alive.poll_tick(cx) {
            Poll::Ready(_) => {
                Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(KEEP_ALIVE_FRAME)))))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use http_body_util::BodyExt;
    use nowcast_types::{Event, QualifiedFlowId};
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;

    fn flow() -> QualifiedFlowId {
        QualifiedFlowId::parse("probe/a").unwrap()
    }

    fn update(data: &[u8]) -> StreamEvent {
        StreamEvent::Update(Event {
            id: EventId(7),
            flow: flow(),
            data_type: "probe.item/v1".into(),
            time: SystemTime::UNIX_EPOCH,
            data: data.to_vec(),
        })
    }

    fn text(event: &StreamEvent) -> String {
        String::from_utf8(encode(event).unwrap().to_vec()).unwrap()
    }

    #[test]
    fn an_update_carries_its_id() {
        assert_eq!(
            text(&update(b"{\"n\":1}")),
            "event: update\nid: 7\ndata: {\"specversion\":\"1.0\",\"id\":\"7\",\
             \"source\":\"nowcast://bricks/probe\",\"type\":\"probe.item/v1\",\
             \"subject\":\"probe/a\",\"time\":\"1970-01-01T00:00:00Z\",\
             \"dataschema\":\"nowcast://bricks/probe/types/probe.item/v1\",\
             \"data\":{\"n\":1}}\n\n"
        );
    }

    #[test]
    fn signals_carry_no_id() {
        let error = StreamEvent::FlowError {
            flow: flow(),
            message: "down".into(),
        };
        assert_eq!(
            text(&error),
            "event: flow-error\ndata: {\"flow\":\"probe/a\",\"message\":\"down\"}\n\n"
        );
        let ended = StreamEvent::FlowEnded { flow: flow() };
        assert_eq!(
            text(&ended),
            "event: flow-ended\ndata: {\"flow\":\"probe/a\"}\n\n"
        );
        let gap = StreamEvent::FlowGap { flow: flow() };
        assert_eq!(
            text(&gap),
            "event: flow-gap\ndata: {\"flow\":\"probe/a\"}\n\n"
        );
    }

    #[test]
    fn an_unwritable_update_is_skipped() {
        assert_eq!(encode(&update(b"not json")), None);
    }

    #[test]
    fn last_event_id_is_a_number_or_nothing() {
        assert_eq!(last_event_id(Some(b"42")), Some(EventId(42)));
        assert_eq!(last_event_id(Some(b" 42 ")), Some(EventId(42)));
        assert_eq!(last_event_id(None), None);
        for bad in [
            &b""[..],
            b"x",
            b"-1",
            b"4.2",
            b"\xff",
            b"99999999999999999999",
        ] {
            assert_eq!(last_event_id(Some(bad)), None);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_body_skips_bad_updates_then_ends_with_the_stream() {
        let (tx, rx) = unbounded_channel();
        let mut body = SseBody::new(rx);
        tx.send(update(b"not json")).unwrap();
        tx.send(StreamEvent::FlowEnded { flow: flow() }).unwrap();
        drop(tx);
        let frame = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert!(frame.starts_with(b"event: flow-ended\n"));
        assert!(body.frame().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_body_sends_keep_alives() {
        let (tx, rx) = unbounded_channel();
        let mut body = SseBody::new(rx);
        let started = Instant::now();
        let frame = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(&frame[..], KEEP_ALIVE_FRAME);
        assert_eq!(started.elapsed(), KEEP_ALIVE);
        drop(tx);
    }
}
