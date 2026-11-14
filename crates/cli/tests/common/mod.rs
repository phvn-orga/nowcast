//! A fake nowcast service on a Unix socket, following the client API of
//! contract section 3, and helpers to run the `nowcast` binary against it.
//!
//! The fake knows one brick, `probe`, with one kind, `item`, whose params are
//! `{"id": <string>}` and whose flow is `probe/<id>`. It tells anchors apart
//! by the peer's pid, or puts every connection on one shared anchor to stand
//! for an agent whose hooks run the tool.

#![allow(dead_code)]

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader, Lines};
use tokio::net::UnixListener;
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

pub const BRICK: &str = "probe";
pub const KIND: &str = "item";
pub const TYPE: &str = "probe.item/v1";
const WAIT: Duration = Duration::from_secs(10);

/// How the service attaches connections to anchor processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchoring {
    /// Each process is its own anchor: the tool launched from a plain shell.
    PerProcess,
    /// Every connection belongs to one anchor: the tool run by an agent's hooks.
    Shared,
}

/// How the probe brick answers a subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Ready,
    /// Never ready: `503 brick_unavailable`.
    Unavailable,
    /// Answers `subscribe` with an error: `422 subscription_refused`.
    Refusing,
}

/// A request the fake received, and its status.
#[derive(Debug, Clone)]
pub struct Logged {
    pub pid: i32,
    pub method: String,
    /// As sent, still percent-encoded.
    pub path: String,
    pub query: Option<String>,
    pub body: Option<Value>,
    pub accept: Option<String>,
    pub last_event_id: Option<String>,
    pub status: u16,
}

/// A canned answer that replaces the fake's own for one path.
#[derive(Debug, Clone)]
struct Canned {
    path: String,
    status: u16,
    content_type: &'static str,
    body: String,
}

#[derive(Default)]
struct Anchor {
    session: Option<String>,
    /// Subscription id to flow.
    subscriptions: HashMap<String, String>,
    streams: Vec<UnboundedSender<Bytes>>,
}

impl Anchor {
    fn subscribed(&self, flow: &str) -> bool {
        self.subscriptions.values().any(|f| f == flow)
    }

    fn send(&mut self, frame: &str) {
        self.streams
            .retain(|s| s.send(Bytes::from(frame.to_owned())).is_ok());
    }
}

struct State {
    anchoring: Anchoring,
    probe: Probe,
    anchors: HashMap<i32, Anchor>,
    /// Flow to its last event.
    last: HashMap<String, Value>,
    /// Flow to every event it published, oldest first.
    history: HashMap<String, Vec<Value>>,
    next_event: u64,
    next_subscription: u64,
    log: Vec<Logged>,
    canned: Vec<Canned>,
}

impl State {
    fn anchor(&mut self, pid: i32) -> &mut Anchor {
        let key = match self.anchoring {
            Anchoring::PerProcess => pid,
            Anchoring::Shared => 0,
        };
        self.anchors.entry(key).or_default()
    }
}

pub struct Fake {
    dir: TempDir,
    socket: PathBuf,
    state: Arc<Mutex<State>>,
    server: Mutex<Option<JoinHandle<()>>>,
}

impl Fake {
    /// A running service.
    pub async fn start(anchoring: Anchoring) -> Self {
        let fake = Self::absent(anchoring);
        fake.listen();
        fake
    }

    /// A service whose socket does not exist yet: call [`Fake::listen`].
    pub fn absent(anchoring: Anchoring) -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let socket = dir.path().join("nowcast").join("nowcast.sock");
        Self {
            dir,
            socket,
            state: Arc::new(Mutex::new(State {
                anchoring,
                probe: Probe::Ready,
                anchors: HashMap::new(),
                last: HashMap::new(),
                history: HashMap::new(),
                next_event: 1,
                next_subscription: 1,
                log: Vec::new(),
                canned: Vec::new(),
            })),
            server: Mutex::new(None),
        }
    }

    pub fn listen(&self) {
        std::fs::create_dir_all(self.socket.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&self.socket).expect("the socket binds");
        let state = self.state.clone();
        let server = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let pid = stream.peer_cred().ok().and_then(|c| c.pid()).unwrap_or(-1);
                let state = state.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request| handle(state.clone(), pid, request));
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        *self.server.lock().unwrap() = Some(server);
    }

    /// The directory to give as `XDG_RUNTIME_DIR`: the socket is
    /// `nowcast/nowcast.sock` in it.
    pub fn runtime_dir(&self) -> &Path {
        self.dir.path()
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn set_probe(&self, probe: Probe) {
        self.state.lock().unwrap().probe = probe;
    }

    /// Answers every request on `path` with this, before any other check,
    /// in place of an answer canned before for it.
    pub fn can(&self, path: &str, status: u16, content_type: &'static str, body: &str) {
        let mut state = self.state.lock().unwrap();
        state.canned.retain(|c| c.path != path);
        state.canned.push(Canned {
            path: path.into(),
            status,
            content_type,
            body: body.into(),
        });
    }

    /// Subscribes the shared anchor to `probe/<id>`, as an agent's own hook
    /// or mod would. Returns the flow.
    pub fn subscribe_shared(&self, id: &str) -> String {
        let mut state = self.state.lock().unwrap();
        assert_eq!(state.anchoring, Anchoring::Shared);
        let flow = format!("{BRICK}/{id}");
        let subscription = format!("s-{}", state.next_subscription);
        state.next_subscription += 1;
        state
            .anchor(0)
            .subscriptions
            .insert(subscription, flow.clone());
        flow
    }

    /// The probe publishes `data` on `flow`. Returns the event.
    pub fn publish(&self, flow: &str, data: Value) -> Value {
        let mut state = self.state.lock().unwrap();
        let id = state.next_event;
        state.next_event += 1;
        let event = json!({
            "specversion": "1.0",
            "id": id.to_string(),
            "source": format!("nowcast://bricks/{BRICK}"),
            "type": TYPE,
            "subject": flow,
            "time": "2026-11-06T10:00:00Z",
            "dataschema": format!("nowcast://bricks/{BRICK}/types/{TYPE}"),
            "data": data,
        });
        state.last.insert(flow.to_owned(), event.clone());
        state
            .history
            .entry(flow.to_owned())
            .or_default()
            .push(event.clone());
        let frame = update_frame(&event);
        for anchor in state.anchors.values_mut() {
            if anchor.subscribed(flow) {
                anchor.send(&frame);
            }
        }
        event
    }

    pub fn flow_error(&self, flow: &str, message: &str) {
        let data = json!({ "flow": flow, "message": message });
        self.signal(flow, &format!("event: flow-error\ndata: {data}\n\n"));
    }

    /// Some events of the flow are gone.
    pub fn flow_gap(&self, flow: &str) {
        let data = json!({ "flow": flow });
        self.signal(flow, &format!("event: flow-gap\ndata: {data}\n\n"));
    }

    /// The flow ends: its subscriptions are gone.
    pub fn end_flow(&self, flow: &str) {
        let data = json!({ "flow": flow });
        self.signal(flow, &format!("event: flow-ended\ndata: {data}\n\n"));
        let mut state = self.state.lock().unwrap();
        for anchor in state.anchors.values_mut() {
            anchor.subscriptions.retain(|_, f| f != flow);
        }
    }

    /// Sends raw bytes on every open stream.
    pub fn send_raw(&self, frame: &str) {
        let mut state = self.state.lock().unwrap();
        for anchor in state.anchors.values_mut() {
            anchor.send(frame);
        }
    }

    fn signal(&self, flow: &str, frame: &str) {
        let mut state = self.state.lock().unwrap();
        for anchor in state.anchors.values_mut() {
            if anchor.subscribed(flow) {
                anchor.send(frame);
            }
        }
    }

    /// Ends every open stream; sessions and subscriptions stay.
    pub fn close_streams(&self) {
        let mut state = self.state.lock().unwrap();
        for anchor in state.anchors.values_mut() {
            anchor.streams.clear();
        }
    }

    /// Forgets everything, as a restarted service would, and ends the streams.
    pub fn restart(&self) {
        let mut state = self.state.lock().unwrap();
        state.anchors.clear();
        state.last.clear();
    }

    pub fn open_streams(&self) -> usize {
        let state = self.state.lock().unwrap();
        state
            .anchors
            .values()
            .flat_map(|a| &a.streams)
            .filter(|s| !s.is_closed())
            .count()
    }

    /// Waits until `n` streams are open.
    pub async fn wait_streams(&self, n: usize) {
        timeout(WAIT, async {
            while self.open_streams() < n {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{n} open streams, log: {:#?}", self.log()));
    }

    pub fn log(&self) -> Vec<Logged> {
        self.state.lock().unwrap().log.clone()
    }

    /// Every process declared its session before any other call, and no call
    /// was answered `428`.
    #[track_caller]
    pub fn assert_sessions_first(&self) {
        let log = self.log();
        assert!(!log.is_empty(), "no request reached the service");
        let mut seen = Vec::new();
        for entry in &log {
            assert_ne!(
                entry.status, 428,
                "a call came before the session: {log:#?}"
            );
            if !seen.contains(&entry.pid) {
                assert_eq!(
                    (entry.method.as_str(), entry.path.as_str()),
                    ("PUT", "/v1/session"),
                    "the first call of process {}: {log:#?}",
                    entry.pid
                );
                seen.push(entry.pid);
            }
        }
    }
}

fn update_frame(event: &Value) -> String {
    format!(
        "event: update\nid: {}\ndata: {event}\n\n",
        event["id"].as_str().unwrap()
    )
}

type Reply = Response<BoxBody<Bytes, Infallible>>;

fn reply(status: u16, content_type: &str, body: String) -> Reply {
    let mut response = Response::builder().status(status);
    if !body.is_empty() {
        response = response.header("content-type", content_type);
    }
    response.body(Full::new(Bytes::from(body)).boxed()).unwrap()
}

fn json_reply(status: u16, body: Value) -> Reply {
    reply(status, "application/json", body.to_string())
}

fn empty(status: u16) -> Reply {
    reply(status, "", String::new())
}

fn error(status: u16, code: &str, message: &str) -> Reply {
    json_reply(
        status,
        json!({ "error": { "code": code, "message": message } }),
    )
}

async fn handle(
    state: Arc<Mutex<State>>,
    pid: i32,
    request: Request<Incoming>,
) -> Result<Reply, Infallible> {
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    let query = request.uri().query().map(str::to_owned);
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let accept = header("accept");
    let last_event_id = header("last-event-id");
    let bytes = request.into_body().collect().await.unwrap().to_bytes();
    let body: Option<Value> = serde_json::from_slice(&bytes).ok();

    let mut state = state.lock().unwrap();
    let response = route(
        &mut state,
        pid,
        &method,
        &path,
        query.as_deref(),
        body.as_ref(),
        last_event_id.as_deref(),
    );
    state.log.push(Logged {
        pid,
        method,
        path,
        query,
        body,
        accept,
        last_event_id,
        status: response.status().as_u16(),
    });
    Ok(response)
}

fn route(
    state: &mut State,
    pid: i32,
    method: &str,
    path: &str,
    query: Option<&str>,
    body: Option<&Value>,
    last_event_id: Option<&str>,
) -> Reply {
    if let Some(canned) = state.canned.iter().find(|c| c.path == path) {
        return reply(canned.status, canned.content_type, canned.body.clone());
    }
    if (method, path) == ("PUT", "/v1/session") {
        return match body.and_then(|b| b["id"].as_str()) {
            Some(id) if !id.is_empty() => {
                // `"replace": false` declares only for an anchor without one.
                let replace = body.and_then(|b| b["replace"].as_bool()).unwrap_or(true);
                let session = &mut state.anchor(pid).session;
                if replace || session.is_none() {
                    *session = Some(id.to_owned());
                }
                empty(204)
            }
            _ => error(400, "invalid_request", "{\"id\": <string>} expected"),
        };
    }
    if state.anchor(pid).session.is_none() {
        return error(428, "session_required", "Declare the session first.");
    }
    if let Some(flow) = path.strip_prefix("/v1/flows/") {
        // Refresh and input are both a POST answered `202`.
        let (flow, refresh) = match flow
            .strip_suffix("/refresh")
            .or_else(|| flow.strip_suffix("/input"))
        {
            Some(flow) => (flow, true),
            None => (flow, false),
        };
        if method != if refresh { "POST" } else { "GET" } {
            return error(405, "method_not_allowed", "");
        }
        let flow = percent_decode(flow);
        if !state.anchor(pid).subscribed(&flow) {
            return error(404, "unknown_flow", "not subscribed to this flow");
        }
        if refresh {
            return empty(202);
        }
        if let Some(after) = query.and_then(|q| q.strip_prefix("after=")) {
            let after: u64 = after.parse().unwrap();
            let events: Vec<&Value> = state
                .history
                .get(&flow)
                .into_iter()
                .flatten()
                .filter(|e| e["id"].as_str().unwrap().parse::<u64>().unwrap() > after)
                .collect();
            return json_reply(200, json!({ "gap": false, "events": events }));
        }
        return match state.last.get(&flow) {
            Some(event) => json_reply(200, event.clone()),
            None => empty(204),
        };
    }
    match (method, path) {
        ("POST", "/v1/subscriptions") => subscribe(state, pid, body),
        ("GET", "/v1/stream") => stream(state, pid, last_event_id),
        _ => error(404, "not_found", "no such route"),
    }
}

fn subscribe(state: &mut State, pid: i32, body: Option<&Value>) -> Reply {
    let Some(body) = body else {
        return error(400, "invalid_request", "a JSON body");
    };
    if body["brick"] != BRICK {
        return error(404, "unknown_brick", "no such brick");
    }
    if body["kind"] != KIND {
        return error(404, "unknown_kind", "no such kind");
    }
    let id = match body["params"].as_object() {
        Some(params) if params.len() == 1 => params.get("id").and_then(Value::as_str),
        _ => None,
    };
    let Some(id) = id else {
        return error(400, "invalid_params", "params must be {\"id\": <string>}");
    };
    match state.probe {
        Probe::Unavailable => {
            return error(503, "brick_unavailable", "probe did not answer ready");
        }
        Probe::Refusing => {
            return error(422, "subscription_refused", "no_such_item: nothing here");
        }
        Probe::Ready => {}
    }
    let flow = format!("{BRICK}/{id}");
    let subscription = format!("s-{}", state.next_subscription);
    state.next_subscription += 1;
    state
        .anchor(pid)
        .subscriptions
        .insert(subscription.clone(), flow.clone());
    let last = state.last.get(&flow).cloned().unwrap_or(Value::Null);
    json_reply(
        201,
        json!({ "subscription": subscription, "flow": flow, "last": last }),
    )
}

fn stream(state: &mut State, pid: i32, last_event_id: Option<&str>) -> Reply {
    let after: u64 = last_event_id.and_then(|id| id.parse().ok()).unwrap_or(0);
    let (sender, receiver) = unbounded_channel();
    let flows: Vec<String> = state.anchor(pid).subscriptions.values().cloned().collect();
    for flow in flows {
        if let Some(event) = state.last.get(&flow) {
            let id: u64 = event["id"].as_str().unwrap().parse().unwrap();
            if id > after {
                let _ = sender.send(Bytes::from(update_frame(event)));
            }
        }
    }
    state.anchor(pid).streams.push(sender);
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(ChannelBody(receiver).boxed())
        .unwrap()
}

/// A response body fed by a channel; it ends when the sender is dropped.
struct ChannelBody(UnboundedReceiver<Bytes>);

impl Body for ChannelBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.0
            .poll_recv(cx)
            .map(|chunk| chunk.map(|b| Ok(Frame::data(b))))
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = s
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap()
}

/// The `nowcast` binary, aimed at the fake's socket through `NOWCAST_SOCKET`,
/// with no session given.
pub fn nowcast(fake: &Fake) -> Command {
    let mut command = bare();
    command.env("NOWCAST_SOCKET", fake.socket());
    command
}

/// The `nowcast` binary, with none of its environment variables set, and a
/// configuration directory that does not exist.
pub fn bare() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nowcast"));
    command
        .env("XDG_CONFIG_HOME", "/nonexistent/nowcast-tests")
        .env_remove("NOWCAST_SOCKET")
        .env_remove("NOWCAST_SESSION")
        .env_remove("XDG_RUNTIME_DIR")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    command
}

/// Runs a command to its end, `stdin` written to its standard input.
pub async fn run_with_stdin(command: &mut Command, args: &[&str], stdin: &str) -> Output {
    use tokio::io::AsyncWriteExt;

    let mut child = command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the command runs");
    let mut pipe = child.stdin.take().unwrap();
    pipe.write_all(stdin.as_bytes()).await.unwrap();
    drop(pipe);
    timeout(WAIT, child.wait_with_output())
        .await
        .expect("the command ends")
        .expect("the command runs")
}

/// Runs a command to its end.
pub async fn run(command: &mut Command, args: &[&str]) -> Output {
    timeout(WAIT, command.args(args).output())
        .await
        .expect("the command ends")
        .expect("the command runs")
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// A long-running `nowcast watch`, read line by line.
pub struct Watching {
    child: Child,
    lines: Lines<BufReader<ChildStdout>>,
}

impl Watching {
    pub fn spawn(command: &mut Command, args: &[&str]) -> Self {
        let mut child = command
            .arg("watch")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("watch starts");
        let lines = BufReader::new(child.stdout.take().unwrap()).lines();
        Self { child, lines }
    }

    pub fn pid(&self) -> i32 {
        self.child.id().unwrap() as i32
    }

    pub async fn next_line(&mut self) -> String {
        timeout(WAIT, self.lines.next_line())
            .await
            .expect("a line in time")
            .expect("stdout readable")
            .expect("watch still running")
    }

    /// Whether a line comes within `within`.
    pub async fn line_within(&mut self, within: Duration) -> Option<String> {
        timeout(within, self.lines.next_line())
            .await
            .ok()
            .and_then(|line| line.ok().flatten())
    }

    /// Waits for the end of a watch expected to stop by itself.
    pub async fn wait(mut self) -> (Option<i32>, String) {
        let status = timeout(WAIT, self.child.wait())
            .await
            .expect("watch stops")
            .unwrap();
        (status.code(), self.stderr().await)
    }

    /// Stops the watch and returns what it wrote on stderr.
    pub async fn stop(mut self) -> String {
        self.child.kill().await.unwrap();
        self.stderr().await
    }

    async fn stderr(&mut self) -> String {
        let mut err = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            stderr.read_to_string(&mut err).await.unwrap();
        }
        err
    }
}
