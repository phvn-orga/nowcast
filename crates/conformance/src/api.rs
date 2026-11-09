//! Contract section 3, the client API over HTTP on the service's Unix socket.
//!
//! The subject starts a service that declares the [`crate::probe`], and
//! drives the probe through an [`ApiHarness`]. Every request opens its own
//! connection, as hook-based clients do.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use nowcast_types::QualifiedFlowId;
use serde_json::{Value, json};
use tokio::net::UnixStream;
use tokio::time::{sleep, timeout};

use crate::probe;

/// How the probe answers `hello`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeMode {
    Ready,
    /// Never answers, so subscribing fails with `503`.
    NeverReady,
    /// Answers every `subscribe` with `error`, code [`REFUSAL_CODE`].
    Refusing,
}

/// The code the probe refuses subscriptions with, in [`ProbeMode::Refusing`].
pub const REFUSAL_CODE: &str = "no_such_item";

/// A running service and control over its probe.
pub trait ApiHarness {
    fn socket(&self) -> PathBuf;

    /// The probe publishes `detail` on `flow`. Returns once the service holds
    /// the value.
    fn publish(&self, flow: &QualifiedFlowId, detail: &[u8]) -> impl Future<Output = ()>;

    /// The probe reports `flow-error` on `flow`.
    fn flow_error(&self, flow: &QualifiedFlowId, message: &str) -> impl Future<Output = ()>;

    /// The flows the probe was asked to refresh, in order.
    fn refreshes(&self) -> impl Future<Output = Vec<QualifiedFlowId>>;

    /// The inputs the probe received, in order: flow, type and detail.
    fn inputs(&self) -> impl Future<Output = Vec<(QualifiedFlowId, String, Value)>>;
}

/// Starts a fresh service: no session, no subscription.
pub trait ApiSubject {
    type Harness: ApiHarness;
    fn start(probe: ProbeMode) -> impl Future<Output = Self::Harness>;
}

const WAIT: Duration = Duration::from_secs(5);

/// Percent-encodes everything but unreserved characters, for a flow id in a
/// path.
pub fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

struct Response {
    status: u16,
    content_type: Option<String>,
    body: Bytes,
}

impl Response {
    /// Bodies are JSON.
    fn json(&self) -> Value {
        let content_type = self.content_type.as_deref().unwrap_or_default();
        assert!(
            content_type.starts_with("application/json"),
            "content type {content_type:?}"
        );
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "the body is not JSON ({e}): {:?}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    #[track_caller]
    fn status(&self, expected: u16) -> &Self {
        assert_eq!(
            self.status,
            expected,
            "body: {:?}",
            String::from_utf8_lossy(&self.body)
        );
        self
    }

    /// The error has the contract's one shape, with this status and code.
    #[track_caller]
    fn error(&self, status: u16, code: &str) {
        self.status(status);
        let body = self.json();
        let error = body["error"].as_object().expect("{\"error\": {...}}");
        assert_eq!(error["code"], code);
        assert!(error["message"].is_string(), "the error has a message");
        assert_eq!(error.len(), 2, "code and message: {body}");
        assert_eq!(body.as_object().unwrap().len(), 1, "only error: {body}");
    }
}

struct Client {
    socket: PathBuf,
}

impl Client {
    fn of(harness: &impl ApiHarness) -> Self {
        Self {
            socket: harness.socket(),
        }
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<Bytes>,
        headers: &[(&str, &str)],
    ) -> hyper::Response<Incoming> {
        let stream = UnixStream::connect(&self.socket)
            .await
            .expect("the service listens");
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .expect("an HTTP/1.1 connection");
        tokio::spawn(conn);
        let mut request = hyper::Request::builder()
            .method(method)
            .uri(path)
            .header("host", "nowcast");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let body = match body {
            Some(bytes) => {
                request = request.header("content-type", "application/json");
                bytes
            }
            None => Bytes::new(),
        };
        sender
            .send_request(request.body(Full::new(body)).unwrap())
            .await
            .expect("a response")
    }

    async fn request(&self, method: &str, path: &str, body: Option<Value>) -> Response {
        let body = body.map(|json| Bytes::from(serde_json::to_vec(&json).unwrap()));
        self.request_raw(method, path, body).await
    }

    /// A request whose body is sent as is, JSON or not.
    async fn request_raw(&self, method: &str, path: &str, body: Option<Bytes>) -> Response {
        let response = self.send(method, path, body, &[]).await;
        let status = response.status().as_u16();
        let content_type = content_type(&response);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Response {
            status,
            content_type,
            body,
        }
    }

    async fn declare_session(&self) {
        self.request("PUT", "/v1/session", Some(json!({ "id": "session-1" })))
            .await
            .status(204);
    }

    async fn subscribe(&self, id: &str) -> Subscribed {
        self.subscribe_to(probe::KIND, id).await
    }

    async fn subscribe_to(&self, kind: &str, id: &str) -> Subscribed {
        let response = self
            .request(
                "POST",
                "/v1/subscriptions",
                Some(json!({ "brick": probe::NAME, "kind": kind, "params": { "id": id } })),
            )
            .await;
        response.status(201);
        let body = response.json();
        let flow = body["flow"].as_str().expect("a flow").to_owned();
        Subscribed {
            subscription: body["subscription"]
                .as_str()
                .expect("a subscription")
                .to_owned(),
            qualified: QualifiedFlowId::parse(&flow).expect("a <brick>/<flow> id"),
            flow,
            last: body["last"].clone(),
        }
    }

    async fn flow(&self, flow: &str) -> Response {
        self.request("GET", &format!("/v1/flows/{}", encode(flow)), None)
            .await
    }

    async fn stream(&self, last_event_id: Option<&str>) -> Sse {
        let mut headers = vec![("accept", "text/event-stream")];
        if let Some(id) = last_event_id {
            headers.push(("last-event-id", id));
        }
        let response = self.send("GET", "/v1/stream", None, &headers).await;
        assert_eq!(response.status().as_u16(), 200);
        let content_type = content_type(&response).unwrap_or_default();
        assert!(
            content_type.starts_with("text/event-stream"),
            "content type {content_type:?}"
        );
        Sse {
            body: response.into_body(),
            buffer: Vec::new(),
        }
    }
}

fn content_type(response: &hyper::Response<Incoming>) -> Option<String> {
    response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

struct Subscribed {
    subscription: String,
    flow: String,
    qualified: QualifiedFlowId,
    last: Value,
}

#[derive(Debug)]
struct SseEvent {
    event: String,
    id: Option<String>,
    data: String,
}

struct Sse {
    body: Incoming,
    /// Raw bytes, so that a character split across two chunks stays whole.
    buffer: Vec<u8>,
}

/// Drops the `\r` of every `\r\n`.
fn crlf_to_lf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for (i, &b) in bytes.iter().enumerate() {
        if !(b == b'\r' && bytes.get(i + 1) == Some(&b'\n')) {
            out.push(b);
        }
    }
    out
}

impl Sse {
    /// The next event within `within`, `None` if none comes.
    async fn next_within(&mut self, within: Duration) -> Option<SseEvent> {
        timeout(within, async {
            loop {
                let normalized = crlf_to_lf(&self.buffer);
                if let Some(end) = normalized.windows(2).position(|w| w == b"\n\n") {
                    let block = std::str::from_utf8(&normalized[..end]).expect("SSE is UTF-8");
                    let event = parse_event(block);
                    self.buffer = normalized[end + 2..].to_vec();
                    if let Some(event) = event {
                        return event;
                    }
                    continue;
                }
                let frame = self
                    .body
                    .frame()
                    .await
                    .expect("the stream stays open")
                    .expect("a readable stream");
                if let Ok(data) = frame.into_data() {
                    self.buffer.extend_from_slice(&data);
                }
            }
        })
        .await
        .ok()
    }

    async fn next(&mut self) -> SseEvent {
        self.next_within(WAIT).await.expect("an event")
    }
}

/// Reads one SSE block; comments only (keep-alives) give `None`.
fn parse_event(block: &str) -> Option<SseEvent> {
    let mut event = None;
    let mut id = None;
    let mut data = Vec::new();
    for line in block.lines() {
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => event = Some(value.to_owned()),
            "id" => id = Some(value.to_owned()),
            "data" => data.push(value),
            _ => {}
        }
    }
    if event.is_none() && data.is_empty() {
        return None;
    }
    Some(SseEvent {
        event: event.unwrap_or_else(|| "message".into()),
        id,
        data: data.join("\n"),
    })
}

/// `YYYY-MM-DDTHH:MM:SS`, optional fraction, then `Z` or `±HH:MM`.
fn is_rfc3339(time: &str) -> bool {
    let b = time.as_bytes();
    let digits = |r: std::ops::Range<usize>| {
        r.into_iter()
            .all(|i| b.get(i).is_some_and(u8::is_ascii_digit))
    };
    if !(digits(0..4)
        && b.get(4) == Some(&b'-')
        && digits(5..7)
        && b.get(7) == Some(&b'-')
        && digits(8..10)
        && matches!(b.get(10), Some(b'T' | b't'))
        && digits(11..13)
        && b.get(13) == Some(&b':')
        && digits(14..16)
        && b.get(16) == Some(&b':')
        && digits(17..19))
    {
        return false;
    }
    let mut rest = &time[19..];
    if let Some(fraction) = rest.strip_prefix('.') {
        let len = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if len == 0 {
            return false;
        }
        rest = &fraction[len..];
    }
    match rest.as_bytes() {
        [b'Z' | b'z'] => true,
        [b'+' | b'-', h1, h2, b':', m1, m2] => [h1, h2, m1, m2].iter().all(|c| c.is_ascii_digit()),
        _ => false,
    }
}

/// The event has every attribute of the contract, its detail as published.
#[track_caller]
fn assert_cloudevent(event: &Value, flow: &str, detail: &Value) {
    assert_eq!(event["specversion"], "1.0");
    assert!(event["id"].is_string(), "id is a string: {event}");
    assert_eq!(event["source"], format!("nowcast://bricks/{}", probe::NAME));
    assert_eq!(event["type"], probe::TYPE);
    assert_eq!(event["subject"], flow);
    let time = event["time"].as_str().expect("a time");
    assert!(is_rfc3339(time), "time {time:?} is RFC 3339");
    assert_eq!(
        event["dataschema"],
        format!("nowcast://bricks/{}/types/{}", probe::NAME, probe::TYPE)
    );
    assert_eq!(&event["data"], detail);
}

/// Sets up a client with a declared session.
async fn ready<S: ApiSubject>() -> (S::Harness, Client) {
    let harness = S::start(ProbeMode::Ready).await;
    let client = Client::of(&harness);
    client.declare_session().await;
    (harness, client)
}

async fn publish(harness: &impl ApiHarness, flow: &QualifiedFlowId, detail: &Value) {
    harness
        .publish(flow, &serde_json::to_vec(detail).unwrap())
        .await;
}

/// `PUT /v1/session` answers `204`.
pub async fn declaring_the_session_answers_204<S: ApiSubject>() {
    ready::<S>().await;
}

/// Any request before the session is declared answers `428`
/// `session_required`, with the contract's error shape.
pub async fn requests_before_the_session_are_refused<S: ApiSubject>() {
    let harness = S::start(ProbeMode::Ready).await;
    let client = Client::of(&harness);
    let subscribe = json!({ "brick": probe::NAME, "kind": probe::KIND, "params": { "id": "a" } });
    for (method, path, body) in [
        ("POST", "/v1/subscriptions", Some(subscribe)),
        ("DELETE", "/v1/subscriptions/s-1", None),
        ("GET", "/v1/flows/probe%2Fa", None),
        ("POST", "/v1/flows/probe%2Fa/refresh", None),
        ("POST", "/v1/flows/probe%2Fa/input", Some(json!({}))),
        ("GET", "/v1/stream", None),
    ] {
        client
            .request(method, path, body)
            .await
            .error(428, "session_required");
    }
}

/// `POST /v1/subscriptions` answers `201` with the subscription, the flow
/// prefixed with the brick's name, and `last: null` on a new flow.
pub async fn subscribing_answers_201<S: ApiSubject>() {
    let (_harness, client) = ready::<S>().await;
    let subscribed = client.subscribe("a").await;
    assert!(!subscribed.subscription.is_empty());
    assert_eq!(subscribed.qualified.brick(), &probe::name());
    assert_eq!(subscribed.last, Value::Null);
}

/// `last` carries the flow's last value, as an event.
pub async fn subscribing_returns_the_last_value<S: ApiSubject>() {
    let (harness, client) = ready::<S>().await;
    let first = client.subscribe("a").await;
    let detail = json!({ "n": 1 });
    publish(&harness, &first.qualified, &detail).await;
    let second = client.subscribe("a").await;
    assert_cloudevent(&second.last, &first.flow, &detail);
}

/// Params that do not match the kind's schema answer `400` `invalid_params`.
pub async fn invalid_params_answer_400<S: ApiSubject>() {
    let (_harness, client) = ready::<S>().await;
    for params in [
        json!({}),
        json!({ "id": 3 }),
        json!({ "id": "a", "extra": 1 }),
    ] {
        client
            .request(
                "POST",
                "/v1/subscriptions",
                Some(json!({ "brick": probe::NAME, "kind": probe::KIND, "params": params })),
            )
            .await
            .error(400, "invalid_params");
    }
}

/// An unknown brick or kind answers `404` `unknown_brick` or `unknown_kind`.
pub async fn unknown_bricks_and_kinds_answer_404<S: ApiSubject>() {
    let (_harness, client) = ready::<S>().await;
    let params = json!({ "id": "a" });
    client
        .request(
            "POST",
            "/v1/subscriptions",
            Some(json!({ "brick": "nope", "kind": probe::KIND, "params": params })),
        )
        .await
        .error(404, "unknown_brick");
    client
        .request(
            "POST",
            "/v1/subscriptions",
            Some(json!({ "brick": probe::NAME, "kind": "nope", "params": params })),
        )
        .await
        .error(404, "unknown_kind");
}

/// A brick that does not answer `ready` in time answers `503`
/// `brick_unavailable`.
pub async fn an_unavailable_brick_answers_503<S: ApiSubject>() {
    let harness = S::start(ProbeMode::NeverReady).await;
    let client = Client::of(&harness);
    client.declare_session().await;
    client
        .request(
            "POST",
            "/v1/subscriptions",
            Some(json!({ "brick": probe::NAME, "kind": probe::KIND, "params": { "id": "a" } })),
        )
        .await
        .error(503, "brick_unavailable");
}

/// A brick answering `subscribe` with `error` answers `422`
/// `subscription_refused`, the brick's code in the message.
pub async fn a_refused_subscription_answers_422<S: ApiSubject>() {
    let harness = S::start(ProbeMode::Refusing).await;
    let client = Client::of(&harness);
    client.declare_session().await;
    let response = client
        .request(
            "POST",
            "/v1/subscriptions",
            Some(json!({ "brick": probe::NAME, "kind": probe::KIND, "params": { "id": "a" } })),
        )
        .await;
    response.error(422, "subscription_refused");
    let message = response.json()["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        message.starts_with(&format!("{REFUSAL_CODE}: ")),
        "the message starts with the brick's code: {message:?}"
    );
}

/// Two subscribers of one flow get two subscriptions: deleting one does not cut
/// the other. The flow is shared.
pub async fn subscriptions_are_per_subscriber<S: ApiSubject>() {
    let (_harness, client) = ready::<S>().await;
    let first = client.subscribe("a").await;
    let second = client.subscribe("a").await;
    assert_ne!(first.subscription, second.subscription);
    assert_eq!(first.flow, second.flow);
    client
        .request(
            "DELETE",
            &format!("/v1/subscriptions/{}", first.subscription),
            None,
        )
        .await
        .status(204);
    client.flow(&first.flow).await.status(204);
}

/// `DELETE /v1/subscriptions/{subscription}` answers `204`; the flow is then no
/// longer readable.
pub async fn unsubscribing_answers_204<S: ApiSubject>() {
    let (_harness, client) = ready::<S>().await;
    let subscribed = client.subscribe("a").await;
    client
        .request(
            "DELETE",
            &format!("/v1/subscriptions/{}", subscribed.subscription),
            None,
        )
        .await
        .status(204);
    client
        .flow(&subscribed.flow)
        .await
        .error(404, "unknown_flow");
}

/// `POST /v1/flows/{flow}/refresh` answers `202` and reaches the brick.
pub async fn refresh_answers_202_and_reaches_the_brick<S: ApiSubject>() {
    let (harness, client) = ready::<S>().await;
    let subscribed = client.subscribe("a").await;
    client
        .request(
            "POST",
            &format!("/v1/flows/{}/refresh", encode(&subscribed.flow)),
            None,
        )
        .await
        .status(202);
    let reached = timeout(WAIT, async {
        while !harness.refreshes().await.contains(&subscribed.qualified) {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(reached.is_ok(), "the brick never got the refresh");
}

/// `POST /v1/bricks/{brick}/refresh` answers `202` and refreshes every flow
/// of the brick, or of the kind given, without a subscription to them; an
/// unknown brick or kind answers `404`.
pub async fn brick_refresh_reaches_every_flow_of_the_brick<S: ApiSubject>() {
    let (harness, client) = ready::<S>().await;
    let item = client.subscribe("a").await;
    let inbox = client.subscribe_to(probe::INBOX_KIND, "b").await;
    let path = format!("/v1/bricks/{}/refresh", probe::NAME);
    client
        .request("POST", &path, Some(json!({ "kind": probe::KIND })))
        .await
        .status(202);
    wait_for_refresh(&harness, &item.qualified).await;
    assert!(
        !harness.refreshes().await.contains(&inbox.qualified),
        "only the kind asked for"
    );
    client.request("POST", &path, None).await.status(202);
    wait_for_refresh(&harness, &inbox.qualified).await;

    let unknown = client
        .request("POST", "/v1/bricks/nope/refresh", None)
        .await;
    assert_eq!(unknown.status(404).json()["error"]["code"], "unknown_brick");
    let unknown = client
        .request("POST", &path, Some(json!({ "kind": "nope" })))
        .await;
    assert_eq!(unknown.status(404).json()["error"]["code"], "unknown_kind");
}

async fn wait_for_refresh(harness: &impl ApiHarness, flow: &QualifiedFlowId) {
    let reached = timeout(WAIT, async {
        while !harness.refreshes().await.contains(flow) {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(reached.is_ok(), "the brick never got the refresh of {flow}");
}

/// `POST /v1/flows/{flow}/input` answers `202` and relays the detail to the
/// brick, under the type the flow's kind accepts.
pub async fn input_answers_202_and_reaches_the_brick<S: ApiSubject>() {
    let (harness, client) = ready::<S>().await;
    let subscribed = client.subscribe_to(probe::INBOX_KIND, "a").await;
    let path = format!("/v1/flows/{}/input", encode(&subscribed.flow));
    for n in 1..=2 {
        client
            .request("POST", &path, Some(json!({ "n": n })))
            .await
            .status(202);
    }
    let expected = |n: i64| {
        (
            subscribed.qualified.clone(),
            probe::INPUT_TYPE.to_owned(),
            json!({ "n": n }),
        )
    };
    let reached = timeout(WAIT, async {
        while harness.inputs().await.len() < 2 {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(reached.is_ok(), "the brick never got both inputs");
    assert_eq!(harness.inputs().await, vec![expected(1), expected(2)]);
}

/// An input takes a subscription to the flow (`404` `unknown_flow`), a kind
/// that accepts one (`405` `input_not_accepted`), and JSON that matches the
/// accepted type's schema (`400` `invalid_input`). None reaches the brick.
pub async fn invalid_inputs_are_refused<S: ApiSubject>() {
    let (harness, client) = ready::<S>().await;
    let inbox = client.subscribe_to(probe::INBOX_KIND, "a").await;
    let item = client.subscribe("b").await;
    let path = |flow: &str| format!("/v1/flows/{}/input", encode(flow));
    client
        .request("POST", &path("probe/inbox/never"), Some(json!({})))
        .await
        .error(404, "unknown_flow");
    client
        .request("POST", &path(&item.flow), Some(json!({})))
        .await
        .error(405, "input_not_accepted");
    client
        .request("POST", &path(&inbox.flow), Some(json!(42)))
        .await
        .error(400, "invalid_input");
    client
        .request_raw(
            "POST",
            &path(&inbox.flow),
            Some(Bytes::from_static(b"{nope")),
        )
        .await
        .error(400, "invalid_input");
    assert_eq!(harness.inputs().await, vec![]);
}

/// `GET /v1/flows/{flow}` answers `204` before the first value, then `200`
/// with the last event.
pub async fn reading_a_flow_gives_its_last_event<S: ApiSubject>() {
    let (harness, client) = ready::<S>().await;
    let subscribed = client.subscribe("a").await;
    client.flow(&subscribed.flow).await.status(204);
    let detail = json!({ "n": 2 });
    publish(&harness, &subscribed.qualified, &detail).await;
    let response = client.flow(&subscribed.flow).await;
    response.status(200);
    assert_cloudevent(&response.json(), &subscribed.flow, &detail);
}

/// Reading a flow the anchor is not subscribed to answers `404`
/// `unknown_flow`.
pub async fn reading_an_unsubscribed_flow_answers_404<S: ApiSubject>() {
    let (_harness, client) = ready::<S>().await;
    client.flow("probe/never").await.error(404, "unknown_flow");
}

/// A flow id goes percent-encoded in a path, `/` included.
pub async fn flow_ids_are_percent_encoded<S: ApiSubject>() {
    let (_harness, client) = ready::<S>().await;
    let subscribed = client.subscribe("a/b#c d").await;
    assert!(
        subscribed.flow.contains('/'),
        "a qualified flow id holds a /"
    );
    client.flow(&subscribed.flow).await.status(204);
}

/// Subscriptions live with the anchor process, not the connection: a later
/// connection still reads the flow.
pub async fn subscriptions_outlive_the_connection<S: ApiSubject>() {
    let (harness, client) = ready::<S>().await;
    let subscribed = client.subscribe("a").await;
    let other = Client::of(&harness);
    other.flow(&subscribed.flow).await.status(204);
}

/// `GET /v1/stream` is a `text/event-stream`; an `update` carries the event as
/// CloudEvents JSON, its SSE `id` the event's `id`.
pub async fn updates_are_cloudevents<S: ApiSubject>() {
    let (harness, client) = ready::<S>().await;
    let subscribed = client.subscribe("a").await;
    let mut stream = client.stream(None).await;
    let detail = json!({ "number": 123, "state": "open", "checks": [{ "name": "build" }] });
    publish(&harness, &subscribed.qualified, &detail).await;
    let event = stream.next().await;
    assert_eq!(event.event, "update");
    let data: Value = serde_json::from_str(&event.data).unwrap();
    assert_cloudevent(&data, &subscribed.flow, &detail);
    assert_eq!(event.id.as_deref(), data["id"].as_str());
}

/// The stream carries flows subscribed after it opened.
pub async fn the_stream_includes_later_subscriptions<S: ApiSubject>() {
    let (harness, client) = ready::<S>().await;
    let mut stream = client.stream(None).await;
    let subscribed = client.subscribe("later").await;
    publish(&harness, &subscribed.qualified, &json!({})).await;
    let event = stream.next().await;
    assert_eq!(event.event, "update");
    let data: Value = serde_json::from_str(&event.data).unwrap();
    assert_eq!(data["subject"], subscribed.flow);
}

/// A brick unable to serve a flow shows as `flow-error`, with no SSE `id` and
/// `{"flow", "message"}` as data.
pub async fn flow_errors_reach_the_stream<S: ApiSubject>() {
    let (harness, client) = ready::<S>().await;
    let subscribed = client.subscribe("a").await;
    let mut stream = client.stream(None).await;
    harness
        .flow_error(&subscribed.qualified, "source down")
        .await;
    let event = stream.next().await;
    assert_eq!(event.event, "flow-error");
    assert_eq!(event.id, None, "a signal carries no id");
    let data: Value = serde_json::from_str(&event.data).unwrap();
    assert_eq!(
        data,
        json!({ "flow": subscribed.flow, "message": "source down" })
    );
}

/// A deleted subscription shows as `flow-ended`, with no SSE `id` and
/// `{"flow"}` as data.
pub async fn unsubscribing_ends_the_flow_on_the_stream<S: ApiSubject>() {
    let (_harness, client) = ready::<S>().await;
    let subscribed = client.subscribe("a").await;
    let mut stream = client.stream(None).await;
    client
        .request(
            "DELETE",
            &format!("/v1/subscriptions/{}", subscribed.subscription),
            None,
        )
        .await
        .status(204);
    let event = stream.next().await;
    assert_eq!(event.event, "flow-ended");
    assert_eq!(event.id, None, "a signal carries no id");
    let data: Value = serde_json::from_str(&event.data).unwrap();
    assert_eq!(data, json!({ "flow": subscribed.flow }));
}

/// A stream opened without `Last-Event-ID` first sends the last value of each
/// subscribed flow: a value published between subscribing and opening the
/// stream is not lost.
pub async fn a_fresh_stream_sends_every_last_value<S: ApiSubject>() {
    let (harness, client) = ready::<S>().await;
    let a = client.subscribe("a").await;
    publish(&harness, &a.qualified, &json!({ "v": 1 })).await;
    publish(&harness, &a.qualified, &json!({ "v": 2 })).await;
    let mut stream = client.stream(None).await;
    let event = stream.next().await;
    assert_eq!(event.event, "update");
    let data: Value = serde_json::from_str(&event.data).unwrap();
    assert_eq!(event.id.as_deref(), data["id"].as_str());
    assert_eq!(
        (&data["subject"], &data["data"]),
        (&json!(a.flow), &json!({ "v": 2 }))
    );
    if let Some(extra) = stream.next_within(Duration::from_millis(300)).await {
        panic!("only the last value comes, got {extra:?}");
    }
}

/// On reconnection with `Last-Event-ID`, every kept event newer than it comes
/// again, in increasing id order across flows, and no older one.
pub async fn reconnecting_replays_the_history<S: ApiSubject>() {
    let (harness, client) = ready::<S>().await;
    let a = client.subscribe("a").await;
    let b = client.subscribe("b").await;
    publish(&harness, &a.qualified, &json!({ "v": 1 })).await;
    let seen = client.flow(&a.flow).await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    publish(&harness, &a.qualified, &json!({ "v": 2 })).await;
    publish(&harness, &b.qualified, &json!({ "v": 1 })).await;
    publish(&harness, &a.qualified, &json!({ "v": 3 })).await;
    let mut stream = client.stream(Some(&seen)).await;
    let mut replayed = Vec::new();
    for _ in 0..3 {
        let event = stream.next().await;
        assert_eq!(event.event, "update");
        let data: Value = serde_json::from_str(&event.data).unwrap();
        replayed.push((
            data["subject"].as_str().unwrap().to_owned(),
            data["data"].clone(),
        ));
    }
    assert_eq!(
        replayed,
        vec![
            (a.flow.clone(), json!({ "v": 2 })),
            (b.flow, json!({ "v": 1 })),
            (a.flow, json!({ "v": 3 })),
        ]
    );
    if let Some(extra) = stream.next_within(Duration::from_millis(300)).await {
        panic!("only the events after the id come again, got {extra:?}");
    }
}

/// A gap shows as `flow-gap` before the flow's replay, with no SSE `id` and
/// `{"flow"}` as data. `Last-Event-ID: 0` predates every run of the service,
/// so it always has one.
pub async fn a_gap_reaches_the_stream<S: ApiSubject>() {
    let (harness, client) = ready::<S>().await;
    let a = client.subscribe("a").await;
    publish(&harness, &a.qualified, &json!({ "v": 1 })).await;
    let mut stream = client.stream(Some("0")).await;
    let gap = stream.next().await;
    assert_eq!(gap.event, "flow-gap");
    assert_eq!(gap.id, None, "a signal carries no id");
    let data: Value = serde_json::from_str(&gap.data).unwrap();
    assert_eq!(data, json!({ "flow": a.flow }));
    let event = stream.next().await;
    assert_eq!(event.event, "update");
    let data: Value = serde_json::from_str(&event.data).unwrap();
    assert_eq!(data["data"], json!({ "v": 1 }));
}

/// `GET /v1/flows/{flow}?after=<id>` answers `200` with the kept events after
/// the id as CloudEvents and whether some are gone; `400` `invalid_request`
/// when the id is not one.
pub async fn reading_a_flow_after_an_id_gives_its_history<S: ApiSubject>() {
    let (harness, client) = ready::<S>().await;
    let a = client.subscribe("a").await;
    let path = |after: &str| format!("/v1/flows/{}?after={after}", encode(&a.flow));
    let empty = client.request("GET", &path("0"), None).await;
    empty.status(200);
    assert_eq!(empty.json(), json!({ "gap": true, "events": [] }));
    for v in 1..=3 {
        publish(&harness, &a.qualified, &json!({ "v": v })).await;
    }
    let all = client.request("GET", &path("0"), None).await.json();
    let first = all["events"][0]["id"].as_str().unwrap().to_owned();
    let after = client.request("GET", &path(&first), None).await;
    after.status(200);
    let body = after.json();
    assert_eq!(body["gap"], json!(false));
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_cloudevent(&events[0], &a.flow, &json!({ "v": 2 }));
    assert_cloudevent(&events[1], &a.flow, &json!({ "v": 3 }));
    client
        .request("GET", &path("x"), None)
        .await
        .error(400, "invalid_request");
    client
        .request("GET", "/v1/flows/probe%2Fnever?after=0", None)
        .await
        .error(404, "unknown_flow");
}

/// Runs the HTTP rules of contract section 3 against a service.
#[macro_export]
macro_rules! api_suite {
    ($subject:ty) => {
        mod api_suite {
            #[allow(unused_imports)]
            use super::*;
            $crate::__tests!(
                block_on,
                api,
                $subject,
                [
                    declaring_the_session_answers_204,
                    requests_before_the_session_are_refused,
                    subscribing_answers_201,
                    subscribing_returns_the_last_value,
                    invalid_params_answer_400,
                    unknown_bricks_and_kinds_answer_404,
                    an_unavailable_brick_answers_503,
                    a_refused_subscription_answers_422,
                    subscriptions_are_per_subscriber,
                    unsubscribing_answers_204,
                    refresh_answers_202_and_reaches_the_brick,
                    brick_refresh_reaches_every_flow_of_the_brick,
                    input_answers_202_and_reaches_the_brick,
                    invalid_inputs_are_refused,
                    reading_a_flow_gives_its_last_event,
                    reading_an_unsubscribed_flow_answers_404,
                    flow_ids_are_percent_encoded,
                    subscriptions_outlive_the_connection,
                    updates_are_cloudevents,
                    the_stream_includes_later_subscriptions,
                    flow_errors_reach_the_stream,
                    unsubscribing_ends_the_flow_on_the_stream,
                    a_fresh_stream_sends_every_last_value,
                    reconnecting_replays_the_history,
                    a_gap_reaches_the_stream,
                    reading_a_flow_after_an_id_gives_its_history,
                ]
            );
        }
    };
}
