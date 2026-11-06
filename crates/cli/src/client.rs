//! The client API over HTTP/1.1 on the service's Unix socket.
//!
//! Every request opens its own connection: the service attaches requests to a
//! session by their anchor process, not by their connection.

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{ACCEPT, CONTENT_TYPE, HOST};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use nowcast_types::SessionId;
use serde_json::{Value, json};
use tokio::net::UnixStream;

use crate::error::CliError;
use crate::sse::{SseEvent, SseParser};

/// How long a request may wait for its whole answer, or a stream for its
/// headers. A healthy service answers well within it: the slowest call,
/// a subscription, waits at most 5 s for its brick (contract section 4).
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Client {
    socket: PathBuf,
    session: SessionId,
    timeout: Duration,
}

impl Client {
    pub fn new(socket: PathBuf, session: SessionId) -> Self {
        Self {
            socket,
            session,
            timeout: ANSWER_TIMEOUT,
        }
    }

    /// Fails with [`CliError::Unreachable`] when `future` takes longer than
    /// the answer timeout: a stuck service must not hold a hook forever.
    async fn within<T>(
        &self,
        future: impl Future<Output = Result<T, CliError>>,
    ) -> Result<T, CliError> {
        tokio::time::timeout(self.timeout, future)
            .await
            .unwrap_or_else(|_| {
                Err(self.unreachable(format!("no answer within {} s", self.timeout.as_secs_f32())))
            })
    }

    /// Declares the session. Every other call goes through the returned
    /// handle, so none can come before it.
    pub async fn declare(&self) -> Result<Session<'_>, CliError> {
        let body = json!({ "id": self.session.as_str() });
        let reply = self.call(Method::PUT, "/v1/session", Some(body)).await?;
        reply.expect(StatusCode::NO_CONTENT)?;
        Ok(Session { client: self })
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> Result<Response<Incoming>, CliError> {
        let stream = UnixStream::connect(&self.socket)
            .await
            .map_err(|e| self.unreachable(e))?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| self.unreachable(e))?;
        tokio::spawn(async move {
            // Errors surface on the request or its body.
            let _ = connection.await;
        });
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(HOST, "nowcast");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let body = match body {
            Some(json) => {
                request = request.header(CONTENT_TYPE, "application/json");
                Bytes::from(json.to_string())
            }
            None => Bytes::new(),
        };
        let request = request
            .body(Full::new(body))
            .map_err(|e| CliError::Usage(format!("cannot build the request: {e}")))?;
        sender
            .send_request(request)
            .await
            .map_err(|e| self.unreachable(e))
    }

    /// Sends a request and reads the whole answer.
    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Reply, CliError> {
        self.within(async {
            let response = self.send(method, path, body, &[]).await?;
            let status = response.status();
            let body = response
                .into_body()
                .collect()
                .await
                .map_err(|e| self.unreachable(e))?
                .to_bytes();
            Ok(Reply { status, body })
        })
        .await
    }

    fn unreachable(&self, reason: impl ToString) -> CliError {
        CliError::Unreachable {
            socket: self.socket.clone(),
            reason: reason.to_string(),
        }
    }
}

/// A client whose session is declared.
pub struct Session<'a> {
    client: &'a Client,
}

/// What `POST /v1/subscriptions` answered.
pub struct Subscribed {
    pub flow: String,
    /// The whole body: `subscription`, `flow` and `last`.
    pub body: Value,
}

impl Session<'_> {
    /// `POST /v1/subscriptions`.
    pub async fn subscribe(
        &self,
        brick: &str,
        kind: &str,
        params: &Value,
    ) -> Result<Subscribed, CliError> {
        let body = json!({ "brick": brick, "kind": kind, "params": params });
        let reply = self
            .client
            .call(Method::POST, "/v1/subscriptions", Some(body))
            .await?;
        let body = reply.expect(StatusCode::CREATED)?.json()?;
        let (Some(flow), Some(_)) = (
            body.get("flow").and_then(Value::as_str),
            body.get("subscription").and_then(Value::as_str),
        ) else {
            return Err(CliError::Protocol(format!(
                "a subscription without its id or flow: {body}"
            )));
        };
        Ok(Subscribed {
            flow: flow.to_owned(),
            body,
        })
    }

    /// `GET /v1/flows/{flow}`: the last event, `None` if the flow has none yet.
    pub async fn last(&self, flow: &str) -> Result<Option<Value>, CliError> {
        let path = format!("/v1/flows/{}", percent_encode(flow));
        let reply = self.client.call(Method::GET, &path, None).await?;
        match reply.status {
            StatusCode::OK => reply.json().map(Some),
            StatusCode::NO_CONTENT => Ok(None),
            _ => Err(reply.into_error()),
        }
    }

    /// `POST /v1/flows/{flow}/refresh`: the service relays it to the brick.
    pub async fn refresh(&self, flow: &str) -> Result<(), CliError> {
        let path = format!("/v1/flows/{}/refresh", percent_encode(flow));
        let reply = self.client.call(Method::POST, &path, None).await?;
        reply.expect(StatusCode::ACCEPTED)?;
        Ok(())
    }

    /// `GET /v1/stream`, resuming after `last_event_id` if given.
    pub async fn stream(&self, last_event_id: Option<&str>) -> Result<EventStream, CliError> {
        let mut headers = vec![(ACCEPT.as_str(), "text/event-stream")];
        if let Some(id) = last_event_id {
            headers.push(("last-event-id", id));
        }
        // Only the headers and an error body are bounded: an open stream
        // may stay quiet for as long as no flow publishes.
        let response = self
            .client
            .within(async {
                let response = self
                    .client
                    .send(Method::GET, "/v1/stream", None, &headers)
                    .await?;
                if response.status() == StatusCode::OK {
                    return Ok(response);
                }
                let status = response.status();
                let body = response
                    .into_body()
                    .collect()
                    .await
                    .map_err(|e| self.client.unreachable(e))?
                    .to_bytes();
                Err(Reply { status, body }.into_error())
            })
            .await?;
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if !content_type.starts_with("text/event-stream") {
            return Err(CliError::Protocol(format!(
                "the stream has content type {content_type:?}"
            )));
        }
        Ok(EventStream {
            socket: self.client.socket.clone(),
            body: response.into_body(),
            parser: SseParser::default(),
            ready: VecDeque::new(),
        })
    }
}

/// The session's open event stream.
pub struct EventStream {
    socket: PathBuf,
    body: Incoming,
    parser: SseParser,
    ready: VecDeque<SseEvent>,
}

impl EventStream {
    /// The next event, `None` once the service closes the stream.
    pub async fn next(&mut self) -> Result<Option<SseEvent>, CliError> {
        loop {
            if let Some(event) = self.ready.pop_front() {
                return Ok(Some(event));
            }
            let Some(frame) = self.body.frame().await else {
                return Ok(None);
            };
            let frame = frame.map_err(|e| CliError::Unreachable {
                socket: self.socket.clone(),
                reason: e.to_string(),
            })?;
            if let Ok(data) = frame.into_data() {
                let mut events = Vec::new();
                self.parser.feed(&data, &mut events);
                self.ready.extend(events);
            }
        }
    }
}

struct Reply {
    status: StatusCode,
    body: Bytes,
}

impl Reply {
    fn expect(self, status: StatusCode) -> Result<Self, CliError> {
        if self.status == status {
            Ok(self)
        } else {
            Err(self.into_error())
        }
    }

    fn json(&self) -> Result<Value, CliError> {
        serde_json::from_slice(&self.body).map_err(|e| {
            CliError::Protocol(format!(
                "the body is not JSON ({e}): {}",
                String::from_utf8_lossy(&self.body)
            ))
        })
    }

    /// An error answer in the contract's shape, `{"error": {"code", "message"}}`.
    fn into_error(self) -> CliError {
        let error = serde_json::from_slice::<Value>(&self.body)
            .ok()
            .and_then(|body| {
                let error = body.get("error")?;
                let code = error.get("code")?.as_str()?.to_owned();
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                Some((code, message))
            });
        match error {
            Some((code, message)) if !self.status.is_success() => CliError::Api { code, message },
            _ => CliError::Protocol(format!(
                "status {}, body {:?}",
                self.status.as_u16(),
                String::from_utf8_lossy(&self.body)
            )),
        }
    }
}

/// Percent-encodes everything but unreserved characters, so that a flow id,
/// which may hold `/`, stays one path segment.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(status: u16, body: &str) -> Reply {
        Reply {
            status: StatusCode::from_u16(status).unwrap(),
            body: Bytes::from(body.to_owned()),
        }
    }

    /// A service that accepts connections and never answers.
    fn stuck_service() -> (tempfile::TempDir, Client) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("nowcast.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });
        let mut client = Client::new(socket, SessionId::new("s"));
        client.timeout = Duration::from_millis(100);
        (dir, client)
    }

    #[tokio::test]
    async fn a_stuck_service_times_out() {
        let (_dir, client) = stuck_service();
        let error = tokio::time::timeout(Duration::from_secs(5), client.declare())
            .await
            .expect("the call gives up by itself")
            .err()
            .unwrap();
        assert!(
            matches!(error, CliError::Unreachable { ref reason, .. } if reason.contains("no answer")),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_stream_that_never_opens_times_out() {
        let (_dir, client) = stuck_service();
        let session = Session { client: &client };
        let error = tokio::time::timeout(Duration::from_secs(5), session.stream(None))
            .await
            .expect("the stream gives up by itself")
            .err()
            .unwrap();
        assert!(matches!(error, CliError::Unreachable { .. }), "{error:?}");
    }

    #[test]
    fn flow_ids_stay_one_segment() {
        assert_eq!(
            percent_encode("forge/github.com/org/repo#main"),
            "forge%2Fgithub.com%2Forg%2Frepo%23main"
        );
        assert_eq!(percent_encode("a b%é"), "a%20b%25%C3%A9");
        assert_eq!(percent_encode("Az09-._~"), "Az09-._~");
    }

    #[test]
    fn errors_in_the_contract_shape_keep_their_code() {
        let error = reply(
            404,
            r#"{"error":{"code":"unknown_flow","message":"not subscribed"}}"#,
        )
        .into_error();
        let CliError::Api { code, message } = error else {
            panic!("{error:?}");
        };
        assert_eq!((&*code, &*message), ("unknown_flow", "not subscribed"));
    }

    #[test]
    fn other_answers_are_protocol_errors() {
        for (status, body) in [
            (500, "boom"),
            (404, "{}"),
            (400, r#"{"error":{"code":3}}"#),
            (200, r#"{"error":{"code":"x","message":"y"}}"#),
        ] {
            let error = reply(status, body).into_error();
            assert!(
                matches!(error, CliError::Protocol(_)),
                "{status} {body}: {error:?}"
            );
        }
    }

    #[test]
    fn a_missing_message_is_tolerated() {
        let error = reply(400, r#"{"error":{"code":"invalid_params"}}"#).into_error();
        assert!(matches!(error, CliError::Api { ref code, .. } if code == "invalid_params"));
    }
}
