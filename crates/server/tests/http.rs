//! The server against the fakes of every module: the client API conformance
//! suite, then the cases the contract leaves to the server.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use nowcast_conformance::api::{ApiHarness, ApiSubject, ProbeMode, REFUSAL_CODE, encode};
use nowcast_conformance::{block_on, probe};
use nowcast_server::{MAX_BODY, Server};
use nowcast_traits::fake::{FakeDiscovery, FakeIdentity, FakeRouter, FakeSessions, FakeSupervisor};
use nowcast_traits::{
    Anchor, AnchorId, Identity, ResolveError, Resolved, Router, Supervisor, UnknownFlow,
    UnknownSubscription,
};
use nowcast_types::QualifiedFlowId;
use nowcast_types::{BrickName, Event, EventId, StreamEvent, SubscriptionId};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;
use tokio::time::timeout;

/// A server on a fresh socket, its modules at hand.
struct Harness<I: Identity = FakeIdentity> {
    router: Arc<FakeRouter>,
    supervisor: Arc<FakeSupervisor>,
    discovery: Arc<FakeDiscovery>,
    identity: Arc<I>,
    socket: PathBuf,
    task: JoinHandle<()>,
    _dir: TempDir,
}

impl<I: Identity> Drop for Harness<I> {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn start_with<I: Identity>(identity: I, mode: ProbeMode) -> Harness<I> {
    let router = Arc::new(FakeRouter::new());
    let supervisor = Arc::new(FakeSupervisor::new(router.clone()));
    let discovery = Arc::new(FakeDiscovery::new());
    let identity = Arc::new(identity);
    discovery.insert(probe::declaration(vec!["probe".into()], PathBuf::from("/")));
    match mode {
        ProbeMode::Ready => {}
        ProbeMode::NeverReady => supervisor.set_unavailable(probe::name()),
        ProbeMode::Refusing => supervisor.set_refusal(probe::name(), REFUSAL_CODE, "no such item"),
    }
    let dir = tempfile::tempdir().expect("a temporary directory");
    let socket = dir.path().join("nowcast.sock");
    let listener = UnixListener::bind(&socket).expect("a socket");
    let server = Server {
        router: Arc::clone(&router),
        supervisor: Arc::clone(&supervisor),
        discovery: Arc::clone(&discovery),
        identity: Arc::clone(&identity),
        sessions: Arc::new(FakeSessions::new()),
    };
    let task = tokio::spawn(server.serve(listener));
    Harness {
        router,
        supervisor,
        discovery,
        identity,
        socket,
        task,
        _dir: dir,
    }
}

fn start(mode: ProbeMode) -> Harness {
    start_with(FakeIdentity::new(), mode)
}

impl<I: Identity> ApiHarness for Harness<I> {
    fn socket(&self) -> PathBuf {
        self.socket.clone()
    }

    async fn publish(&self, flow: &QualifiedFlowId, detail: &[u8]) {
        self.router
            .publish(flow, probe::TYPE.into(), detail.to_vec())
            .expect("the flow is subscribed");
    }

    async fn flow_error(&self, flow: &QualifiedFlowId, message: &str) {
        self.router.flow_error(flow, message.into());
    }

    async fn refreshes(&self) -> Vec<QualifiedFlowId> {
        self.supervisor.refreshes()
    }
}

struct Fakes;

impl ApiSubject for Fakes {
    type Harness = Harness;

    async fn start(probe: ProbeMode) -> Harness {
        start(probe)
    }
}

nowcast_conformance::api_suite!(Fakes);

// What follows covers what the suite does not: the server's own answers.

struct Response {
    status: u16,
    headers: hyper::HeaderMap,
    body: Bytes,
}

impl Response {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("a JSON body")
    }

    #[track_caller]
    fn error(&self, status: u16, code: &str) {
        assert_eq!(self.status, status, "{:?}", self.body);
        assert_eq!(self.headers["content-type"], "application/json");
        let body = self.json();
        assert_eq!(body["error"]["code"], code, "{body}");
        assert!(body["error"]["message"].is_string());
    }
}

async fn send(
    socket: &PathBuf,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: impl Into<Bytes>,
) -> hyper::Response<Incoming> {
    let stream = UnixStream::connect(socket)
        .await
        .expect("the server listens");
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
    sender
        .send_request(request.body(Full::new(body.into())).unwrap())
        .await
        .expect("a response")
}

async fn request(
    harness: &Harness<impl Identity>,
    method: &str,
    path: &str,
    body: &str,
) -> Response {
    let response = send(&harness.socket, method, path, &[], body.to_owned()).await;
    Response {
        status: response.status().as_u16(),
        headers: response.headers().clone(),
        body: response.into_body().collect().await.unwrap().to_bytes(),
    }
}

async fn declare(harness: &Harness<impl Identity>) {
    let response = request(harness, "PUT", "/v1/session", r#"{"id":"s"}"#).await;
    assert_eq!(response.status, 204);
}

async fn subscribe(harness: &Harness<impl Identity>, id: &str) -> Value {
    let body = json!({ "brick": probe::NAME, "kind": probe::KIND, "params": { "id": id } });
    let response = request(harness, "POST", "/v1/subscriptions", &body.to_string()).await;
    assert_eq!(response.status, 201, "{:?}", response.body);
    assert_eq!(response.headers["content-type"], "application/json");
    response.json()
}

fn qualified(subscribed: &Value) -> QualifiedFlowId {
    QualifiedFlowId::parse(subscribed["flow"].as_str().unwrap()).unwrap()
}

/// The `update` events of a stream, read until `count` came or a pause.
async fn updates(response: hyper::Response<Incoming>, count: usize) -> Vec<Value> {
    let mut body = response.into_body();
    let mut text = String::new();
    let mut found = Vec::new();
    while found.len() < count {
        let Ok(Some(Ok(frame))) = timeout(Duration::from_millis(300), body.frame()).await else {
            break;
        };
        text.push_str(std::str::from_utf8(&frame.into_data().unwrap()).unwrap());
        while let Some(end) = text.find("\n\n") {
            let block: String = text.drain(..end + 2).collect();
            if let Some(data) = block.lines().find_map(|l| l.strip_prefix("data: ")) {
                found.push(serde_json::from_str(data).unwrap());
            }
        }
    }
    found
}

fn run<F: Future>(test: F) -> F::Output {
    block_on(test)
}

#[test]
fn unknown_routes_answer_404_not_found() {
    run(async {
        let harness = start(ProbeMode::Ready);
        for path in ["/", "/v2/session", "/v1/nope", "/v1/flows/probe/a"] {
            request(&harness, "GET", path, "")
                .await
                .error(404, "not_found");
        }
    });
}

#[test]
fn a_wrong_method_answers_405_with_allow() {
    run(async {
        let harness = start(ProbeMode::Ready);
        let response = request(&harness, "GET", "/v1/session", "").await;
        response.error(405, "method_not_allowed");
        assert_eq!(response.headers["allow"], "PUT");
    });
}

#[test]
fn a_malformed_session_answers_400_invalid_request() {
    run(async {
        let harness = start(ProbeMode::Ready);
        for body in ["", "not json", "{}", r#"{"id":3}"#, r#"{"id":""}"#] {
            request(&harness, "PUT", "/v1/session", body)
                .await
                .error(400, "invalid_request");
        }
        // Nothing was declared.
        request(&harness, "GET", "/v1/stream", "")
            .await
            .error(428, "session_required");
    });
}

#[test]
fn a_malformed_subscription_answers_400_invalid_request() {
    run(async {
        let harness = start(ProbeMode::Ready);
        declare(&harness).await;
        for body in [
            "[]",
            r#"{"kind":"item","params":{"id":"a"}}"#,
            r#"{"brick":"probe","params":{"id":"a"}}"#,
            r#"{"brick":"probe","kind":"item"}"#,
            r#"{"brick":1,"kind":"item","params":{"id":"a"}}"#,
        ] {
            request(&harness, "POST", "/v1/subscriptions", body)
                .await
                .error(400, "invalid_request");
        }
    });
}

#[test]
fn an_oversized_body_answers_413() {
    run(async {
        let harness = start(ProbeMode::Ready);
        let body = format!(r#"{{"id":"{}"}}"#, "x".repeat(MAX_BODY));
        request(&harness, "PUT", "/v1/session", &body)
            .await
            .error(413, "invalid_request");
    });
}

#[test]
fn a_brick_name_that_cannot_exist_is_unknown() {
    run(async {
        let harness = start(ProbeMode::Ready);
        declare(&harness).await;
        let body = json!({ "brick": "Not A Name", "kind": "item", "params": {} });
        request(&harness, "POST", "/v1/subscriptions", &body.to_string())
            .await
            .error(404, "unknown_brick");
    });
}

#[test]
fn a_removed_brick_is_unknown() {
    run(async {
        let harness = start(ProbeMode::Ready);
        declare(&harness).await;
        harness.discovery.remove(&probe::name());
        let body = json!({ "brick": probe::NAME, "kind": probe::KIND, "params": { "id": "a" } });
        request(&harness, "POST", "/v1/subscriptions", &body.to_string())
            .await
            .error(404, "unknown_brick");
    });
}

#[test]
fn invalid_params_say_what_is_wrong_and_reach_no_brick() {
    run(async {
        let harness = start(ProbeMode::Ready);
        declare(&harness).await;
        let body = json!({ "brick": probe::NAME, "kind": probe::KIND, "params": { "id": 3 } });
        let response = request(&harness, "POST", "/v1/subscriptions", &body.to_string()).await;
        response.error(400, "invalid_params");
        let message = response.json()["error"]["message"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(message.contains("/id"), "{message}");
        assert_eq!(harness.router.subscribers(&probe_flow("a")), 0);
    });
}

fn probe_flow(id: &str) -> QualifiedFlowId {
    let flow = format!("{}/{}:{}", probe::NAME, probe::KIND, json!({ "id": id }));
    QualifiedFlowId::parse(&flow).unwrap()
}

#[test]
fn deleting_an_unknown_subscription_answers_404() {
    run(async {
        let harness = start(ProbeMode::Ready);
        declare(&harness).await;
        request(&harness, "DELETE", "/v1/subscriptions/s-404", "")
            .await
            .error(404, "unknown_subscription");
        // Deleting twice: the second time it is gone.
        let subscribed = subscribe(&harness, "a").await;
        let path = format!(
            "/v1/subscriptions/{}",
            subscribed["subscription"].as_str().unwrap()
        );
        assert_eq!(request(&harness, "DELETE", &path, "").await.status, 204);
        request(&harness, "DELETE", &path, "")
            .await
            .error(404, "unknown_subscription");
    });
}

#[test]
fn another_anchors_subscription_is_unknown() {
    run(async {
        let harness = start(ProbeMode::Ready);
        declare(&harness).await;
        let subscribed = subscribe(&harness, "a").await;
        harness.identity.set_anchor(Anchor {
            id: AnchorId(2),
            kind: None,
        });
        declare(&harness).await;
        let path = format!(
            "/v1/subscriptions/{}",
            subscribed["subscription"].as_str().unwrap()
        );
        request(&harness, "DELETE", &path, "")
            .await
            .error(404, "unknown_subscription");
        let flow = format!("/v1/flows/{}", encode(subscribed["flow"].as_str().unwrap()));
        request(&harness, "GET", &flow, "")
            .await
            .error(404, "unknown_flow");
        request(&harness, "POST", &format!("{flow}/refresh"), "")
            .await
            .error(404, "unknown_flow");
        assert!(harness.supervisor.refreshes().is_empty());
    });
}

#[test]
fn the_session_is_per_anchor() {
    run(async {
        let harness = start(ProbeMode::Ready);
        declare(&harness).await;
        harness.identity.set_anchor(Anchor {
            id: AnchorId(2),
            kind: None,
        });
        request(&harness, "GET", "/v1/stream", "")
            .await
            .error(428, "session_required");
    });
}

#[test]
fn refreshing_an_unsubscribed_flow_answers_404() {
    run(async {
        let harness = start(ProbeMode::Ready);
        declare(&harness).await;
        request(&harness, "POST", "/v1/flows/probe%2Fnever/refresh", "")
            .await
            .error(404, "unknown_flow");
        assert!(harness.supervisor.refreshes().is_empty());
    });
}

#[test]
fn flow_ids_that_cannot_exist_are_unknown() {
    run(async {
        let harness = start(ProbeMode::Ready);
        declare(&harness).await;
        let too_long = format!("/v1/flows/probe%2F{}", "x".repeat(300));
        for path in [
            "/v1/flows/no-separator",
            "/v1/flows/Bad%2Fa",
            "/v1/flows/",
            too_long.as_str(),
        ] {
            request(&harness, "GET", path, "")
                .await
                .error(404, "unknown_flow");
        }
    });
}

#[test]
fn a_malformed_escape_answers_400() {
    run(async {
        let harness = start(ProbeMode::Ready);
        declare(&harness).await;
        for path in [
            "/v1/flows/probe%2",
            "/v1/flows/probe%zz/refresh",
            "/v1/subscriptions/%FF",
        ] {
            let method = if path.ends_with("refresh") {
                "POST"
            } else if path.contains("subscriptions") {
                "DELETE"
            } else {
                "GET"
            };
            request(&harness, method, path, "")
                .await
                .error(400, "invalid_request");
        }
    });
}

/// The acceptance rule of the issue: after a reconnection with
/// `Last-Event-ID`, only the flows with a newer value come again.
#[test]
fn reconnecting_skips_flows_with_no_newer_value() {
    run(async {
        let harness = start(ProbeMode::Ready);
        declare(&harness).await;
        let a = subscribe(&harness, "a").await;
        let b = subscribe(&harness, "b").await;
        harness.publish(&qualified(&a), b"{}").await;
        let seen = harness
            .router
            .last(AnchorId(1), &qualified(&a))
            .unwrap()
            .unwrap()
            .id;
        harness.publish(&qualified(&b), b"{}").await;
        let response = send(
            &harness.socket,
            "GET",
            "/v1/stream",
            &[("last-event-id", &seen.to_string())],
            "",
        )
        .await;
        let replayed = updates(response, 2).await;
        let subjects: Vec<_> = replayed.iter().map(|e| e["subject"].clone()).collect();
        assert_eq!(subjects, vec![b["flow"].clone()]);
    });
}

#[test]
fn a_missing_session_wins_over_a_malformed_body() {
    run(async {
        let harness = start(ProbeMode::Ready);
        for (method, path, body) in [
            ("POST", "/v1/subscriptions", "not json"),
            ("DELETE", "/v1/subscriptions/%FF", ""),
            ("GET", "/v1/flows/probe%2", ""),
        ] {
            request(&harness, method, path, body)
                .await
                .error(428, "session_required");
        }
    });
}

#[test]
fn a_last_event_id_past_every_value_replays_nothing() {
    run(async {
        let harness = start(ProbeMode::Ready);
        declare(&harness).await;
        let a = subscribe(&harness, "a").await;
        harness.publish(&qualified(&a), b"{}").await;
        let response = send(
            &harness.socket,
            "GET",
            "/v1/stream",
            &[("last-event-id", "1000")],
            "",
        )
        .await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["cache-control"], "no-cache");
        assert!(updates(response, 1).await.is_empty());
    });
}

#[test]
fn an_unreadable_last_event_id_replays_everything() {
    run(async {
        let harness = start(ProbeMode::Ready);
        declare(&harness).await;
        let a = subscribe(&harness, "a").await;
        let b = subscribe(&harness, "b").await;
        harness.publish(&qualified(&a), b"{}").await;
        harness.publish(&qualified(&b), b"{}").await;
        let response = send(
            &harness.socket,
            "GET",
            "/v1/stream",
            &[("last-event-id", "abc")],
            "",
        )
        .await;
        let replayed = updates(response, 3).await;
        let subjects: Vec<_> = replayed.iter().map(|e| e["subject"].clone()).collect();
        assert_eq!(subjects, vec![a["flow"].clone(), b["flow"].clone()]);
    });
}

#[test]
fn the_stream_ends_when_the_router_drops_the_anchor() {
    run(async {
        let harness = start(ProbeMode::Ready);
        declare(&harness).await;
        let response = send(&harness.socket, "GET", "/v1/stream", &[], "").await;
        harness.router.remove_anchor(AnchorId(1));
        let ended = timeout(Duration::from_secs(5), response.into_body().collect()).await;
        assert!(ended.is_ok(), "the response ends");
    });
}

#[test]
fn several_requests_share_one_connection() {
    run(async {
        let harness = start(ProbeMode::Ready);
        let stream = UnixStream::connect(&harness.socket).await.unwrap();
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .unwrap();
        tokio::spawn(conn);
        for (method, path, body, status) in [
            ("PUT", "/v1/session", r#"{"id":"s"}"#, 204),
            ("GET", "/v1/flows/probe%2Fnever", "", 404),
            ("PUT", "/v1/session", r#"{"id":"t"}"#, 204),
        ] {
            sender.ready().await.unwrap();
            let request = hyper::Request::builder()
                .method(method)
                .uri(path)
                .body(Full::new(Bytes::from(body)))
                .unwrap();
            let response = sender.send_request(request).await.unwrap();
            assert_eq!(response.status().as_u16(), status);
            response.into_body().collect().await.unwrap();
        }
    });
}

/// Fails to identify every connection.
struct Blind;

impl Identity for Blind {
    fn identify(&self, _socket: std::os::fd::BorrowedFd<'_>) -> std::io::Result<Anchor> {
        Err(std::io::Error::other("no peer pidfd"))
    }

    fn exited(&self, _anchor: AnchorId) -> impl Future<Output = ()> + Send {
        std::future::pending()
    }
}

#[test]
fn an_unidentified_connection_answers_500() {
    run(async {
        let harness = start_with(Blind, ProbeMode::Ready);
        let response = request(&harness, "PUT", "/v1/session", r#"{"id":"s"}"#).await;
        response.error(500, "internal_error");
        let message = response.json()["error"]["message"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(message.contains("no peer pidfd"), "{message}");
        // A wrong route is still told apart.
        request(&harness, "GET", "/nope", "")
            .await
            .error(404, "not_found");
    });
}

#[test]
fn an_undecodable_last_value_reads_as_500_and_subscribes_with_null() {
    run(async {
        let harness = start(ProbeMode::Ready);
        declare(&harness).await;
        let a = subscribe(&harness, "a").await;
        harness
            .router
            .publish(&qualified(&a), probe::TYPE.into(), b"not json".to_vec())
            .unwrap();
        let path = format!("/v1/flows/{}", encode(a["flow"].as_str().unwrap()));
        request(&harness, "GET", &path, "")
            .await
            .error(500, "internal_error");
        let again = subscribe(&harness, "a").await;
        assert_eq!(again["last"], Value::Null);
    });
}

#[test]
fn dropping_the_server_closes_open_streams() {
    run(async {
        let harness = start(ProbeMode::Ready);
        declare(&harness).await;
        let response = send(&harness.socket, "GET", "/v1/stream", &[], "").await;
        assert_eq!(response.status(), 200);
        harness.task.abort();
        let ended = timeout(Duration::from_secs(5), response.into_body().collect()).await;
        assert!(ended.is_ok(), "the stream ends with the server");
    });
}

/// Lets the anchor die while its brick resolves: forgets its session and
/// removes it from the router, in the registry's order, then subscribes it
/// as the supervisor would, recreating it in the router.
struct DyingAnchor {
    inner: FakeSupervisor,
    router: Arc<FakeRouter>,
    sessions: Arc<FakeSessions>,
}

impl Supervisor for DyingAnchor {
    fn resolve(
        &self,
        anchor: AnchorId,
        brick: &BrickName,
        kind: &str,
        params: &[u8],
    ) -> impl Future<Output = Result<Resolved, ResolveError>> + Send {
        self.sessions.forget(anchor);
        self.router.remove_anchor(anchor);
        self.inner.resolve(anchor, brick, kind, params)
    }

    fn refresh(&self, flow: &QualifiedFlowId) -> Result<(), UnknownFlow> {
        self.inner.refresh(flow)
    }
}

#[test]
fn an_anchor_dying_while_its_brick_resolves_keeps_no_subscription() {
    run(async {
        let router = Arc::new(FakeRouter::new());
        let sessions = Arc::new(FakeSessions::new());
        let discovery = Arc::new(FakeDiscovery::new());
        discovery.insert(probe::declaration(vec!["probe".into()], PathBuf::from("/")));
        let supervisor = DyingAnchor {
            inner: FakeSupervisor::new(router.clone()),
            router: router.clone(),
            sessions: sessions.clone(),
        };
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("nowcast.sock");
        let server = Server {
            router: router.clone(),
            supervisor: Arc::new(supervisor),
            discovery,
            identity: Arc::new(FakeIdentity::new()),
            sessions,
        };
        let task = tokio::spawn(server.serve(UnixListener::bind(&socket).unwrap()));
        let declared = send(&socket, "PUT", "/v1/session", &[], r#"{"id":"s"}"#).await;
        assert_eq!(declared.status(), 204);
        let body = json!({ "brick": probe::NAME, "kind": probe::KIND, "params": { "id": "a" } });
        let response = send(&socket, "POST", "/v1/subscriptions", &[], body.to_string()).await;
        let response = Response {
            status: response.status().as_u16(),
            headers: response.headers().clone(),
            body: response.into_body().collect().await.unwrap().to_bytes(),
        };
        response.error(428, "session_required");
        assert_eq!(router.subscribers(&probe_flow("a")), 0);
        task.abort();
    });
}

/// Lets the anchor die while the server opens its stream: forgets its session
/// and removes it, in the registry's order, just before the stream is made,
/// which recreates the anchor in the router.
struct DyingOnStream {
    inner: FakeRouter,
    sessions: Arc<FakeSessions>,
}

impl Router for DyingOnStream {
    fn subscribe(
        &self,
        anchor: AnchorId,
        flow: QualifiedFlowId,
    ) -> (SubscriptionId, Option<Event>) {
        self.inner.subscribe(anchor, flow)
    }

    fn unsubscribe(
        &self,
        anchor: AnchorId,
        subscription: &SubscriptionId,
    ) -> Result<(), UnknownSubscription> {
        self.inner.unsubscribe(anchor, subscription)
    }

    fn publish(
        &self,
        flow: &QualifiedFlowId,
        data_type: String,
        data: Vec<u8>,
    ) -> Result<Event, UnknownFlow> {
        self.inner.publish(flow, data_type, data)
    }

    fn last(&self, anchor: AnchorId, flow: &QualifiedFlowId) -> Result<Option<Event>, UnknownFlow> {
        self.inner.last(anchor, flow)
    }

    fn flow_error(&self, flow: &QualifiedFlowId, message: String) {
        self.inner.flow_error(flow, message);
    }

    fn end_flow(&self, flow: &QualifiedFlowId) {
        self.inner.end_flow(flow);
    }

    fn stream(&self, anchor: AnchorId, since: Option<EventId>) -> UnboundedReceiver<StreamEvent> {
        self.sessions.forget(anchor);
        self.inner.remove_anchor(anchor);
        self.inner.stream(anchor, since)
    }

    fn subscribers(&self, flow: &QualifiedFlowId) -> usize {
        self.inner.subscribers(flow)
    }

    fn empty_flows(&self) -> UnboundedReceiver<QualifiedFlowId> {
        self.inner.empty_flows()
    }

    fn has_subscriptions(&self) -> tokio::sync::watch::Receiver<bool> {
        self.inner.has_subscriptions()
    }

    fn remove_anchor(&self, anchor: AnchorId) {
        self.inner.remove_anchor(anchor);
    }
}

#[test]
fn an_anchor_dying_while_its_stream_opens_gets_no_stream() {
    run(async {
        let sessions = Arc::new(FakeSessions::new());
        let router = Arc::new(DyingOnStream {
            inner: FakeRouter::new(),
            sessions: sessions.clone(),
        });
        let discovery = Arc::new(FakeDiscovery::new());
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("nowcast.sock");
        let server = Server {
            router: router.clone(),
            supervisor: Arc::new(FakeSupervisor::new(router.clone())),
            discovery,
            identity: Arc::new(FakeIdentity::new()),
            sessions,
        };
        let task = tokio::spawn(server.serve(UnixListener::bind(&socket).unwrap()));
        let declared = send(&socket, "PUT", "/v1/session", &[], r#"{"id":"s"}"#).await;
        assert_eq!(declared.status(), 204);
        let response = send(&socket, "GET", "/v1/stream", &[], "").await;
        let response = Response {
            status: response.status().as_u16(),
            headers: response.headers().clone(),
            body: timeout(Duration::from_secs(5), response.into_body().collect())
                .await
                .expect("the response ends")
                .unwrap()
                .to_bytes(),
        };
        response.error(428, "session_required");
        task.abort();
    });
}
