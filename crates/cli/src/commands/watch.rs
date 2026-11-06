//! `nowcast watch`: holds the session's event stream until interrupted.
//!
//! Each connection declares the session, makes sure the subscriptions asked
//! with `--subscribe` still exist, then opens the stream, resuming after the
//! last event seen. When the service goes away, the tool waits and connects
//! again, with a growing delay.

use std::io::Write;
use std::time::Duration;

use nowcast_types::ApiErrorCode;
use serde_json::Value;

use crate::client::{Client, Session};
use crate::error::CliError;
use crate::output;
use crate::sse::SseEvent;

/// The first delay before connecting again, doubled at each failure up to
/// [`MAX_RETRY`], and reset once a stream opens.
const FIRST_RETRY: Duration = Duration::from_millis(200);
const MAX_RETRY: Duration = Duration::from_secs(5);

pub async fn watch(
    client: &Client,
    subscribe: &[String],
    json: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<(), CliError> {
    let wanted = subscribe
        .chunks(3)
        .map(|chunk| match chunk {
            [brick, kind, params] => Ok(Wanted {
                brick: brick.clone(),
                kind: kind.clone(),
                params: super::parse_params(params)?,
                flow: None,
            }),
            _ => Err(CliError::Usage(
                "--subscribe takes BRICK KIND PARAMS".into(),
            )),
        })
        .collect::<Result<_, _>>()?;
    Watcher {
        client,
        json,
        wanted,
        last_event_id: None,
        retry: FIRST_RETRY,
        down: false,
        out,
        err,
    }
    .run()
    .await
}

/// A subscription asked with `--subscribe`, and its flow once made.
struct Wanted {
    brick: String,
    kind: String,
    params: Value,
    flow: Option<String>,
}

struct Watcher<'a, O, E> {
    client: &'a Client,
    json: bool,
    wanted: Vec<Wanted>,
    /// The SSE id of the last `update` seen, sent as `Last-Event-ID`.
    last_event_id: Option<String>,
    retry: Duration,
    /// The last connection failed: say so once, and say when it is back.
    down: bool,
    out: &'a mut O,
    err: &'a mut E,
}

impl<O: Write, E: Write> Watcher<'_, O, E> {
    async fn run(mut self) -> Result<(), CliError> {
        loop {
            let reason = match self.connection().await {
                Ok(()) => "the service closed the stream".to_owned(),
                Err(error) if is_transient(&error) => error.to_string(),
                Err(error) => return Err(error),
            };
            if !self.down {
                self.down = true;
                let _ = writeln!(self.err, "nowcast: {reason}; retrying");
            }
            tokio::time::sleep(self.retry).await;
            self.retry = (self.retry * 2).min(MAX_RETRY);
        }
    }

    /// One connection, until the stream ends.
    async fn connection(&mut self) -> Result<(), CliError> {
        let session = self.client.declare().await?;
        self.ensure_subscriptions(&session).await?;
        let mut stream = session.stream(self.last_event_id.as_deref()).await?;
        if std::mem::take(&mut self.down) {
            let _ = writeln!(self.err, "nowcast: connected");
        }
        self.retry = FIRST_RETRY;
        while let Some(event) = stream.next().await? {
            self.show(event)?;
        }
        Ok(())
    }

    /// Subscribes what is not subscribed yet, and again what the service lost,
    /// a restart for instance. The last values then all come again: the id the
    /// stream would resume from belongs to the lost state.
    async fn ensure_subscriptions(&mut self, session: &Session<'_>) -> Result<(), CliError> {
        let mut lost = false;
        for wanted in &mut self.wanted {
            if let Some(flow) = &wanted.flow {
                match session.last(flow).await {
                    Ok(_) => continue,
                    Err(error) if has_code(&error, ApiErrorCode::UnknownFlow) => lost = true,
                    Err(error) => return Err(error),
                }
            }
            let subscribed = session
                .subscribe(&wanted.brick, &wanted.kind, &wanted.params)
                .await?;
            let _ = writeln!(self.err, "nowcast: subscribed to {}", subscribed.flow);
            wanted.flow = Some(subscribed.flow);
        }
        if lost {
            self.last_event_id = None;
        }
        Ok(())
    }

    fn show(&mut self, event: SseEvent) -> Result<(), CliError> {
        let name = event.event.as_str();
        if !matches!(name, "update" | "flow-error" | "flow-ended") {
            return Ok(()); // An event a later contract may add.
        }
        if name == "update"
            && let Some(id) = &event.id
        {
            self.last_event_id = Some(id.clone());
        }
        let data: Value = match serde_json::from_str(&event.data) {
            Ok(data) => data,
            Err(e) => {
                let _ = writeln!(
                    self.err,
                    "nowcast: skipped a {name} whose data is not JSON: {e}"
                );
                return Ok(());
            }
        };
        if name == "flow-ended"
            && let Some(flow) = data.get("flow").and_then(Value::as_str)
        {
            // The flow is gone on purpose: do not subscribe it again.
            self.wanted.retain(|w| w.flow.as_deref() != Some(flow));
        }
        output::stream_event(self.out, self.json, name, &data)?;
        Ok(())
    }
}

/// Worth connecting again: the service is away, restarted and forgot the
/// session, or the brick is not ready yet.
fn is_transient(error: &CliError) -> bool {
    matches!(error, CliError::Unreachable { .. })
        || has_code(error, ApiErrorCode::SessionRequired)
        || has_code(error, ApiErrorCode::BrickUnavailable)
}

fn has_code(error: &CliError, expected: ApiErrorCode) -> bool {
    matches!(error, CliError::Api { code, .. } if code == expected.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(code: &str) -> CliError {
        CliError::Api {
            code: code.into(),
            message: String::new(),
        }
    }

    #[test]
    fn transient_errors() {
        assert!(is_transient(&CliError::Unreachable {
            socket: "/x".into(),
            reason: String::new()
        }));
        assert!(is_transient(&api("session_required")));
        assert!(is_transient(&api("brick_unavailable")));
        for code in [
            "invalid_params",
            "unknown_brick",
            "unknown_kind",
            "subscription_refused",
        ] {
            assert!(!is_transient(&api(code)), "{code}");
        }
        assert!(!is_transient(&CliError::Protocol(String::new())));
    }
}
