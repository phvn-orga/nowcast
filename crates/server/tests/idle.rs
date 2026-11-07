//! `Server::serve_until_idle`: when the server counts as used, and the races
//! around the end of the idle delay. The clock is real, so the delay is short.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use nowcast_conformance::probe;
use nowcast_server::Server;
use nowcast_traits::fake::{FakeDiscovery, FakeIdentity, FakeRouter, FakeSessions, FakeSupervisor};
use nowcast_traits::{AnchorId, Router};
use nowcast_types::QualifiedFlowId;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};

const IDLE: Duration = Duration::from_millis(300);
/// Long enough for anything that must happen.
const WAIT: Duration = Duration::from_secs(10);

/// The anchor `FakeIdentity` gives every connection.
const A: AnchorId = AnchorId(1);

struct Harness {
    router: Arc<FakeRouter>,
    socket: PathBuf,
    task: JoinHandle<()>,
    _dir: TempDir,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn listener() -> (UnixListener, PathBuf, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("nowcast.sock");
    (UnixListener::bind(&socket).unwrap(), socket, dir)
}

/// Serves `listener` until idle for `idle`.
fn serve(listener: UnixListener, socket: PathBuf, dir: TempDir, idle: Duration) -> Harness {
    let router = Arc::new(FakeRouter::new());
    let discovery = Arc::new(FakeDiscovery::new());
    discovery.insert(probe::declaration(vec!["probe".into()], PathBuf::from("/")));
    let server = Server {
        router: Arc::clone(&router),
        supervisor: Arc::new(FakeSupervisor::new(router.clone())),
        discovery,
        identity: Arc::new(FakeIdentity::new()),
        sessions: Arc::new(FakeSessions::new()),
        validators: Default::default(),
    };
    let task = tokio::spawn(server.serve_until_idle(listener, idle));
    Harness {
        router,
        socket,
        task,
        _dir: dir,
    }
}

fn start(idle: Duration) -> Harness {
    let (listener, socket, dir) = listener();
    serve(listener, socket, dir, idle)
}

impl Harness {
    /// Waits for `serve_until_idle` to return, and gives when it did.
    async fn returned(&mut self) -> Instant {
        timeout(WAIT, &mut self.task)
            .await
            .expect("the server returns")
            .unwrap();
        Instant::now()
    }

    /// Checks that the server is still serving after several idle delays.
    async fn keeps_serving(&mut self) {
        sleep(IDLE * 3).await;
        assert!(!self.task.is_finished(), "the server returned");
        // And it answers.
        let (status, _) = request(&self.socket, "PUT", "/v1/session", r#"{"id":"s"}"#).await;
        assert_eq!(status, 204);
    }
}

/// Sends one request on a connection of its own, reads the whole answer,
/// then closes the connection.
async fn request(socket: &Path, method: &str, path: &str, body: &str) -> (u16, Bytes) {
    let stream = UnixStream::connect(socket)
        .await
        .expect("the server listens");
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    let conn = tokio::spawn(conn);
    let request = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("host", "nowcast")
        .body(Full::new(Bytes::from(body.to_owned())))
        .unwrap();
    let response: hyper::Response<Incoming> = sender.send_request(request).await.unwrap();
    let status = response.status().as_u16();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    drop(sender);
    // The connection is closed once this returns.
    timeout(WAIT, conn).await.unwrap().unwrap().unwrap();
    (status, body)
}

/// Declares a session and subscribes anchor `A` to the probe's flow `id`, on
/// connections closed since.
async fn subscribe(socket: &Path, id: &str) -> QualifiedFlowId {
    let (status, _) = request(socket, "PUT", "/v1/session", r#"{"id":"s"}"#).await;
    assert_eq!(status, 204);
    let body = json!({ "brick": probe::NAME, "kind": probe::KIND, "params": { "id": id } });
    let (status, body) = request(socket, "POST", "/v1/subscriptions", &body.to_string()).await;
    assert_eq!(status, 201, "{body:?}");
    let body: Value = serde_json::from_slice(&body).unwrap();
    QualifiedFlowId::parse(body["flow"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn an_unused_server_returns_after_the_delay_and_stops_listening() {
    let began = Instant::now();
    let mut harness = start(IDLE);
    let returned = harness.returned().await;
    assert!(returned - began >= IDLE, "returned before the delay");
    assert!(
        UnixStream::connect(&harness.socket).await.is_err(),
        "still listening"
    );
}

#[tokio::test]
async fn an_open_connection_keeps_the_server_running() {
    let mut harness = start(IDLE);
    // Connected, and not a single byte sent.
    let connection = UnixStream::connect(&harness.socket).await.unwrap();
    harness.keeps_serving().await;
    drop(connection);
    let closed = Instant::now();
    let returned = harness.returned().await;
    assert!(returned - closed >= IDLE, "the delay starts at the close");
}

#[tokio::test]
async fn a_stream_with_no_subscription_keeps_the_server_running() {
    let mut harness = start(IDLE);
    let (status, _) = request(&harness.socket, "PUT", "/v1/session", r#"{"id":"s"}"#).await;
    assert_eq!(status, 204);
    let mut stream = UnixStream::connect(&harness.socket).await.unwrap();
    stream
        .write_all(b"GET /v1/stream HTTP/1.1\r\nHost: nowcast\r\n\r\n")
        .await
        .unwrap();
    let mut head = [0; 12];
    stream.read_exact(&mut head).await.unwrap();
    assert_eq!(&head, b"HTTP/1.1 200");
    assert!(!*harness.router.has_subscriptions().borrow());

    harness.keeps_serving().await;
    drop(stream);
    harness.returned().await;
}

#[tokio::test]
async fn a_subscription_keeps_the_server_running_until_its_anchor_dies() {
    let mut harness = start(IDLE);
    subscribe(&harness.socket, "a").await;
    // No connection is left: only the subscription holds the server.
    harness.keeps_serving().await;
    harness.router.remove_anchor(A);
    let removed = Instant::now();
    let returned = harness.returned().await;
    assert!(
        returned - removed >= IDLE,
        "the delay starts at the removal"
    );
}

#[tokio::test]
async fn a_subscription_keeps_the_server_running_until_it_is_deleted() {
    let mut harness = start(IDLE);
    let (status, _) = request(&harness.socket, "PUT", "/v1/session", r#"{"id":"s"}"#).await;
    assert_eq!(status, 204);
    let body = json!({ "brick": probe::NAME, "kind": probe::KIND, "params": { "id": "a" } });
    let (status, body) = request(
        &harness.socket,
        "POST",
        "/v1/subscriptions",
        &body.to_string(),
    )
    .await;
    assert_eq!(status, 201, "{body:?}");
    let body: Value = serde_json::from_slice(&body).unwrap();
    harness.keeps_serving().await;

    let id = body["subscription"].as_str().unwrap();
    let path = format!("/v1/subscriptions/{id}");
    let (status, _) = request(&harness.socket, "DELETE", &path, "").await;
    assert_eq!(status, 204);
    let deleted = Instant::now();
    let returned = harness.returned().await;
    assert!(
        returned - deleted >= IDLE,
        "the delay starts at the deletion"
    );
}

#[tokio::test]
async fn a_flow_ended_with_its_subscribers_lets_the_server_return() {
    let mut harness = start(IDLE);
    let flow = subscribe(&harness.socket, "a").await;
    harness.keeps_serving().await;
    // What the supervisor does when it gives up on a brick.
    harness.router.end_flow(&flow);
    harness.returned().await;
}

#[tokio::test]
async fn a_connection_during_the_delay_starts_it_over() {
    let mut harness = start(IDLE);
    sleep(IDLE * 2 / 3).await;
    let (status, _) = request(&harness.socket, "PUT", "/v1/session", r#"{"id":"s"}"#).await;
    assert_eq!(status, 204);
    let closed = Instant::now();
    let returned = harness.returned().await;
    assert!(returned - closed >= IDLE, "the delay was not started over");
}

/// A client connecting as the delay ends: its connection and the end of the
/// delay are both ready when the server next runs. The connection is served,
/// not dropped with the listener. The runtime has one thread, held while the
/// delay runs out; without the rule, the race is lost one time in two, so it
/// is run several times.
#[tokio::test]
async fn a_connection_waiting_when_the_delay_ends_is_served() {
    let idle = Duration::from_millis(60);
    for _ in 0..12 {
        let mut harness = start(idle);
        // The server waits for the end of the delay.
        sleep(idle / 3).await;
        let mut client = std::os::unix::net::UnixStream::connect(&harness.socket).unwrap();
        // Holds the only thread past the end of the delay.
        std::thread::sleep(idle);
        let answer = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            client
                .write_all(
                    b"PUT /v1/session HTTP/1.1\r\nHost: nowcast\r\nContent-Length: 10\r\n\
                      Connection: close\r\n\r\n{\"id\":\"s\"}",
                )
                .unwrap();
            let mut answer = String::new();
            client.read_to_string(&mut answer).unwrap();
            answer
        });
        let answer = timeout(WAIT, answer).await.unwrap().unwrap();
        assert!(answer.starts_with("HTTP/1.1 204"), "{answer:?}");
        harness.returned().await;
    }
}

#[tokio::test]
async fn serve_runs_whatever_the_idle_time() {
    let (listener, socket, _dir) = listener();
    let router = Arc::new(FakeRouter::new());
    let server = Server {
        router: Arc::clone(&router),
        supervisor: Arc::new(FakeSupervisor::new(router.clone())),
        discovery: Arc::new(FakeDiscovery::new()),
        identity: Arc::new(FakeIdentity::new()),
        sessions: Arc::new(FakeSessions::new()),
        validators: Default::default(),
    };
    let task = tokio::spawn(server.serve(listener));
    sleep(IDLE * 2).await;
    assert!(!task.is_finished());
    let (status, _) = request(&socket, "PUT", "/v1/session", r#"{"id":"s"}"#).await;
    assert_eq!(status, 204);
    task.abort();
}
