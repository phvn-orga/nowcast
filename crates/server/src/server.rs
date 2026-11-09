//! Accepts connections, identifies their anchor, and answers the routes.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::os::fd::AsFd;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::header::{ALLOW, CACHE_CONTROL, CONTENT_TYPE, HeaderMap, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use nowcast_traits::{
    AnchorId, BrickState, Discovery, Identity, InputError, Router, Sessions, Supervisor,
};
use nowcast_types::{
    ApiError, ApiErrorCode, BrickName, EventId, QualifiedFlowId, SessionId, SubscriptionId,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::error::HttpError;
use crate::event::{CloudEvent, to_line};
use crate::params;
use crate::route::{self, Route, decode};
use crate::sse::{self, SseBody};
use crate::validators::Validators;

/// The largest request body read, in bytes. Requests carry a session id or a
/// subscription, far below it.
pub const MAX_BODY: usize = 64 * 1024;

/// The pause after a failed `accept` (for example, out of file descriptors),
/// so that the loop does not spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

type Body = BoxBody<Bytes, Infallible>;

/// The client API, served over HTTP/1.1 on a Unix socket.
///
/// Each connection is attached to its anchor process once, when accepted.
/// Every request on it then acts for that anchor: two connections from the
/// same anchor share its session, subscriptions and stream.
pub struct Server<R, Su, D, I, Se> {
    pub router: Arc<R>,
    pub supervisor: Arc<Su>,
    pub discovery: Arc<D>,
    pub identity: Arc<I>,
    pub sessions: Arc<Se>,
    /// When the service started, which `GET /v1/service` tells.
    pub started_at: SystemTime,
    /// The compiled schemas of accepted types: [`Validators::default`].
    pub validators: Validators,
}

#[derive(Deserialize)]
struct DeclareSession {
    id: String,
}

#[derive(Deserialize)]
struct Subscribe {
    brick: String,
    kind: String,
    params: Value,
}

/// The body of `POST /v1/bricks/{brick}/refresh`, which may be empty.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RefreshBrick {
    kind: Option<String>,
}

#[derive(serde::Serialize)]
struct HistoryRead<'a> {
    gap: bool,
    events: Vec<CloudEvent<'a>>,
}

#[derive(serde::Serialize)]
struct Subscribed<'a> {
    subscription: &'a str,
    flow: String,
    last: Option<CloudEvent<'a>>,
}

impl<R, Su, D, I, Se> Server<R, Su, D, I, Se>
where
    R: Router,
    Su: Supervisor,
    D: Discovery,
    I: Identity,
    Se: Sessions,
{
    /// Serves `listener` until the returned future is dropped, which closes
    /// every open connection with it, event streams included. A failed
    /// `accept` is logged and retried; a failed connection only ends itself.
    pub async fn serve(self, listener: UnixListener) {
        self.run(listener, None).await;
    }

    /// Serves `listener` as [`Server::serve`] does, and returns once the
    /// service has been idle for `idle` without a break: no open connection,
    /// and no subscription in the router ([`Router::has_subscriptions`]).
    ///
    /// Any connection, an event stream with no subscription included, stops
    /// the count, and the next idle moment starts it over. A connection
    /// already waiting when the delay ends goes first: it is accepted and
    /// keeps the server running. When this returns, every
    /// connection is closed and the listener is dropped, so that a connection
    /// arriving later waits in the socket's queue for whoever listens next.
    pub async fn serve_until_idle(self, listener: UnixListener, idle: Duration) {
        self.run(listener, Some(idle)).await;
    }

    async fn run(self, listener: UnixListener, idle: Option<Duration>) {
        let mut subscribed = self.router.has_subscriptions();
        let server = Arc::new(self);
        // Owned here, so that dropping this future aborts every connection.
        let mut connections = JoinSet::new();
        // When the service stops being used for long enough, if it is idle.
        let mut deadline: Option<Instant> = None;
        loop {
            // Nothing makes a subscription without a connection, so with no
            // connection open the router's answer can only go from `true` to
            // `false`, which `changed` reports.
            let used = !connections.is_empty() || *subscribed.borrow_and_update();
            deadline = match (used, idle) {
                // A delay too long for the clock never ends.
                (false, Some(idle)) => deadline.or_else(|| Instant::now().checked_add(idle)),
                _ => None,
            };
            // Biased: a connection waiting to be accepted goes before the end
            // of the delay, even when both are ready at once.
            tokio::select! {
                biased;
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        connections.spawn(Arc::clone(&server).connection(stream));
                    }
                    Err(e) => {
                        tracing::warn!("accepting a connection failed: {e}");
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                    }
                },
                // Reaps finished connections, so the set does not grow.
                Some(ended) = connections.join_next() => {
                    if let Err(e) = ended {
                        tracing::warn!("a connection task failed: {e}");
                    }
                }
                // Only the router going away closes it, which the server's
                // own handle prevents.
                Ok(()) = subscribed.changed() => {}
                () = sleep_until_some(deadline) => {
                    tracing::info!(
                        "no connection and no subscription for {:?}, stopping",
                        idle.unwrap_or_default()
                    );
                    return;
                }
            }
        }
    }

    async fn connection(self: Arc<Self>, stream: UnixStream) {
        // A connection whose peer cannot be identified still gets answers:
        // a `500` on every request says why, where closing would not.
        let anchor = self
            .identity
            .identify(stream.as_fd())
            .map(|anchor| anchor.id)
            .map_err(|e| {
                tracing::warn!("cannot identify a connected process: {e}");
                e.to_string()
            });
        let service = service_fn(move |request| {
            let server = Arc::clone(&self);
            let anchor = anchor.clone();
            async move { Ok::<_, Infallible>(server.respond(anchor, request).await) }
        });
        let served = http1::Builder::new()
            .timer(TokioTimer::new())
            .serve_connection(TokioIo::new(stream), service)
            .await;
        if let Err(e) = served {
            tracing::debug!("connection ended: {e}");
        }
    }

    async fn respond(
        &self,
        anchor: Result<AnchorId, String>,
        request: Request<Incoming>,
    ) -> Response<Body> {
        self.dispatch(anchor, request)
            .await
            .unwrap_or_else(|error| error_response(&error))
    }

    async fn dispatch(
        &self,
        anchor: Result<AnchorId, String>,
        request: Request<Incoming>,
    ) -> Result<Response<Body>, HttpError> {
        let (parts, body) = request.into_parts();
        let route = Route::parse(&parts.method, parts.uri.path())?;
        let anchor = anchor.map_err(|e| {
            HttpError::internal(format!("cannot identify the connected process: {e}"))
        })?;
        // What the service says of itself belongs to no session.
        if !matches!(route, Route::Session | Route::Service | Route::Status)
            && self.sessions.current(anchor).is_none()
        {
            return Err(HttpError::session_required());
        }
        match route {
            Route::Service => Ok(json(StatusCode::OK, service(self.started_at)?.to_string())),
            Route::Status => self.status(),
            Route::Session => {
                let DeclareSession { id } = read_json(body).await?;
                if id.is_empty() {
                    return Err(HttpError::invalid_request("the session id is empty"));
                }
                self.sessions.declare(anchor, SessionId::new(id));
                Ok(empty(StatusCode::NO_CONTENT))
            }
            Route::Subscribe => self.subscribe(anchor, read_json(body).await?).await,
            Route::Unsubscribe(id) => {
                let id = decode(id)?;
                self.router
                    .unsubscribe(anchor, &SubscriptionId::new(id.as_str()))
                    .map_err(|_| HttpError::unknown_subscription(&id))?;
                Ok(empty(StatusCode::NO_CONTENT))
            }
            Route::Flow(flow) => match route::after(parts.uri.query())? {
                None => self.last(anchor, &flow_id(flow)?),
                Some(after) => self.history(anchor, &flow_id(flow)?, after),
            },
            Route::Refresh(flow) => {
                let flow = flow_id(flow)?;
                // Like reading it, refreshing a flow takes a subscription.
                if !self.router.is_subscribed(anchor, &flow) {
                    return Err(HttpError::unknown_flow(&flow.to_string()));
                }
                self.supervisor
                    .refresh(&flow)
                    .map_err(|_| HttpError::unknown_flow(&flow.to_string()))?;
                Ok(empty(StatusCode::ACCEPTED))
            }
            Route::Input(flow) => self.input(anchor, &flow_id(flow)?, body).await,
            Route::BrickRefresh(brick) => self.refresh_brick(brick, body).await,
            Route::Stream => self.stream(anchor, &parts.headers),
        }
    }

    /// The whole state, read from each module in turn: the bricks enabled or
    /// still running, every declared session with its subscriptions, and the
    /// flows with a subscriber.
    fn status(&self) -> Result<Response<Body>, HttpError> {
        let mut bricks: BTreeMap<String, Value> = self
            .discovery
            .bricks()
            .iter()
            .map(|brick| {
                let stopped = serde_json::json!({
                    "name": brick.name.as_str(),
                    "state": "stopped",
                    "pid": null,
                    "crashes": 0,
                });
                (brick.name.to_string(), stopped)
            })
            .collect();
        for brick in self.supervisor.bricks() {
            let state = match brick.state {
                BrickState::Starting => "starting",
                BrickState::Running => "running",
                BrickState::Stopping => "stopping",
                BrickState::Waiting => "waiting",
                BrickState::GivenUp => "given_up",
            };
            let summary = serde_json::json!({
                "name": brick.name.as_str(),
                "state": state,
                "pid": brick.pid,
                "crashes": brick.crashes,
            });
            bricks.insert(brick.name.to_string(), summary);
        }
        let sessions: Vec<Value> = self
            .sessions
            .declared()
            .into_iter()
            .map(|(anchor, session)| {
                let subscriptions: Vec<Value> = self
                    .router
                    .subscriptions(anchor)
                    .into_iter()
                    .map(|(id, flow)| {
                        serde_json::json!({ "id": id.as_str(), "flow": flow.to_string() })
                    })
                    .collect();
                serde_json::json!({ "id": session.as_str(), "subscriptions": subscriptions })
            })
            .collect();
        let mut flows = Vec::new();
        for flow in self.router.flows() {
            if flow.subscribers == 0 {
                continue;
            }
            let last_event = match flow.last {
                Some((id, time)) => {
                    serde_json::json!({ "id": id.to_string(), "time": timestamp(time)? })
                }
                None => Value::Null,
            };
            flows.push(serde_json::json!({
                "id": flow.flow.to_string(),
                "subscribers": flow.subscribers,
                "last_event": last_event,
            }));
        }
        let body = serde_json::json!({
            "service": service(self.started_at)?,
            "bricks": bricks.into_values().collect::<Vec<_>>(),
            "sessions": sessions,
            "flows": flows,
        });
        Ok(json(StatusCode::OK, body.to_string()))
    }

    /// Relays a subscriber's input to the flow's brick, once checked against
    /// the type the flow's subscription kind accepts.
    async fn input(
        &self,
        anchor: AnchorId,
        flow: &QualifiedFlowId,
        body: Incoming,
    ) -> Result<Response<Body>, HttpError> {
        let unknown = || HttpError::unknown_flow(&flow.to_string());
        // Like reading it, sending to a flow takes a subscription.
        if !self.router.is_subscribed(anchor, flow) {
            return Err(unknown());
        }
        let kind = self.supervisor.kind(flow).ok_or_else(unknown)?;
        let declaration = self.discovery.brick(flow.brick()).ok_or_else(unknown)?;
        let not_accepted = || {
            HttpError::api(
                ApiErrorCode::InputNotAccepted,
                format!("kind {kind:?} of brick {} accepts no input", flow.brick()),
            )
        };
        let accepts = declaration
            .subscriptions
            .get(&kind)
            .and_then(|k| k.accepts.clone())
            .ok_or_else(not_accepted)?;
        // Discovery refuses a declaration whose accepted type is undeclared.
        let schema = &declaration
            .types
            .get(&accepts)
            .ok_or_else(not_accepted)?
            .schema;
        let validator = self.validators.get(&declaration, &accepts, schema)?;
        let bytes = read_body(body).await?;
        let input: Value = serde_json::from_slice(&bytes).map_err(|e| {
            HttpError::api(
                ApiErrorCode::InvalidInput,
                format!("the input is not JSON: {e}"),
            )
        })?;
        params::validate_input(&validator, &input, &accepts)?;
        self.supervisor
            .input(flow, &accepts, bytes.to_vec())
            .map_err(|e| match e {
                InputError::UnknownFlow(_) => unknown(),
                InputError::Unavailable(e) => ApiError::from(e).into(),
            })?;
        Ok(empty(StatusCode::ACCEPTED))
    }

    /// Refreshes every live flow of a brick, or of one of its kinds, whoever
    /// subscribed to it. A brick without flows gets nothing, and a stopped
    /// one is not launched for it.
    async fn refresh_brick(
        &self,
        brick: &str,
        body: Incoming,
    ) -> Result<Response<Body>, HttpError> {
        let name = decode(brick)?;
        let unknown_brick =
            || HttpError::api(ApiErrorCode::UnknownBrick, format!("no brick {name:?}"));
        let brick = BrickName::new(name.as_str()).map_err(|_| unknown_brick())?;
        let declaration = self.discovery.brick(&brick).ok_or_else(unknown_brick)?;
        let body = read_body(body).await?;
        let RefreshBrick { kind } = if body.iter().all(u8::is_ascii_whitespace) {
            RefreshBrick::default()
        } else {
            serde_json::from_slice(&body).map_err(|e| {
                HttpError::invalid_request(format!("the body is not the expected JSON: {e}"))
            })?
        };
        if let Some(kind) = &kind
            && !declaration.subscriptions.contains_key(kind)
        {
            return Err(HttpError::api(
                ApiErrorCode::UnknownKind,
                format!("brick {brick} has no kind {kind:?}"),
            ));
        }
        for flow in self.router.flows() {
            if flow.subscribers == 0 || flow.flow.brick() != &brick {
                continue;
            }
            if kind.is_some() && self.supervisor.kind(&flow.flow) != kind {
                continue;
            }
            // A flow that ended meanwhile has nothing to refresh.
            let _ = self.supervisor.refresh(&flow.flow);
        }
        Ok(empty(StatusCode::ACCEPTED))
    }

    async fn subscribe(
        &self,
        anchor: AnchorId,
        request: Subscribe,
    ) -> Result<Response<Body>, HttpError> {
        let unknown_brick = || {
            HttpError::api(
                ApiErrorCode::UnknownBrick,
                format!("no brick {:?}", request.brick),
            )
        };
        // A name that is not a valid brick name cannot name a brick.
        let brick = BrickName::new(request.brick.as_str()).map_err(|_| unknown_brick())?;
        let declaration = self.discovery.brick(&brick).ok_or_else(unknown_brick)?;
        let kind = declaration
            .subscriptions
            .get(&request.kind)
            .ok_or_else(|| {
                HttpError::api(
                    ApiErrorCode::UnknownKind,
                    format!("brick {brick} has no kind {:?}", request.kind),
                )
            })?;
        params::validate(&kind.schema, &request.params, &request.kind)?;
        let params = serde_json::to_vec(&request.params).expect("a JSON value always serializes");
        // The supervisor makes the router subscription itself, in order with
        // what the brick publishes.
        let resolved = self
            .supervisor
            .resolve(anchor, &brick, &request.kind, &params)
            .await
            .map_err(ApiError::from)?;
        // The anchor may have died while its brick was resolving. Its session
        // is forgotten before the router drops the anchor, so a subscription
        // made after that drop is seen here and undone; one made before it is
        // already gone with the anchor.
        if self.sessions.current(anchor).is_none() {
            let _ = self.router.unsubscribe(anchor, &resolved.subscription);
            return Err(HttpError::session_required());
        }
        let last = resolved.last.as_ref().and_then(|event| {
            CloudEvent::new(event)
                .inspect_err(|e| tracing::warn!("answering `last: null`: {e}"))
                .ok()
        });
        let body = to_line(&Subscribed {
            subscription: resolved.subscription.as_str(),
            flow: resolved.flow.to_string(),
            last,
        });
        Ok(json(StatusCode::CREATED, body))
    }

    fn last(&self, anchor: AnchorId, flow: &QualifiedFlowId) -> Result<Response<Body>, HttpError> {
        let last = self
            .router
            .last(anchor, flow)
            .map_err(|_| HttpError::unknown_flow(&flow.to_string()))?;
        let Some(event) = last else {
            return Ok(empty(StatusCode::NO_CONTENT));
        };
        let event = CloudEvent::new(&event).map_err(|e| {
            tracing::warn!("cannot answer the last value of {flow}: {e}");
            HttpError::internal(e.to_string())
        })?;
        Ok(json(StatusCode::OK, to_line(&event)))
    }

    fn history(
        &self,
        anchor: AnchorId,
        flow: &QualifiedFlowId,
        after: EventId,
    ) -> Result<Response<Body>, HttpError> {
        let history = self
            .router
            .history(anchor, flow, after)
            .map_err(|_| HttpError::unknown_flow(&flow.to_string()))?;
        // Like the stream, an event that cannot be written is skipped.
        let events = history
            .events
            .iter()
            .filter_map(|event| {
                CloudEvent::new(event)
                    .inspect_err(|e| tracing::warn!("skipping an event of {flow}'s history: {e}"))
                    .ok()
            })
            .collect();
        let body = to_line(&HistoryRead {
            gap: history.gap,
            events,
        });
        Ok(json(StatusCode::OK, body))
    }

    fn stream(&self, anchor: AnchorId, headers: &HeaderMap) -> Result<Response<Body>, HttpError> {
        let since = sse::last_event_id(headers.get("last-event-id").map(HeaderValue::as_bytes));
        let events = self.router.stream(anchor, since);
        // Opening a stream recreates the anchor in the router. A session gone
        // since the first check means the anchor died meanwhile (nothing else
        // forgets a session), maybe already removed from the router: remove
        // it again, which closes this stream.
        if self.sessions.current(anchor).is_none() {
            self.router.remove_anchor(anchor);
            return Err(HttpError::session_required());
        }
        let mut response = Response::new(SseBody::new(events).boxed());
        let headers = response.headers_mut();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        Ok(response)
    }
}

/// Sleeps until `deadline`, or forever without one.
async fn sleep_until_some(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// A flow id from a path segment. One that does not even parse cannot be
/// subscribed to, hence `unknown_flow`.
fn flow_id(segment: &str) -> Result<QualifiedFlowId, HttpError> {
    let flow = decode(segment)?;
    QualifiedFlowId::parse(&flow).map_err(|_| HttpError::unknown_flow(&flow))
}

async fn read_json<T: DeserializeOwned>(body: Incoming) -> Result<T, HttpError> {
    let bytes = read_body(body).await?;
    serde_json::from_slice(&bytes)
        .map_err(|e| HttpError::invalid_request(format!("the body is not the expected JSON: {e}")))
}

/// The whole body, up to [`MAX_BODY`] bytes.
async fn read_body(body: Incoming) -> Result<Bytes, HttpError> {
    Ok(Limited::new(body, MAX_BODY)
        .collect()
        .await
        .map_err(|e| {
            if e.is::<LengthLimitError>() {
                HttpError::payload_too_large(MAX_BODY)
            } else {
                HttpError::invalid_request(format!("cannot read the body: {e}"))
            }
        })?
        .to_bytes())
}

/// What the service says of itself: the version of the binary running it,
/// when it started, and its own `PATH`, the one bricks are looked up in
/// (`null` when unset or not UTF-8).
fn service(started_at: SystemTime) -> Result<Value, HttpError> {
    let path = std::env::var("PATH").ok();
    Ok(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "started_at": timestamp(started_at)?,
        "path": path,
    }))
}

/// `time` in RFC 3339, UTC.
fn timestamp(time: SystemTime) -> Result<String, HttpError> {
    jiff::Timestamp::try_from(time)
        .map(|t| t.to_string())
        .map_err(|e| HttpError::internal(format!("a time out of range: {e}")))
}

fn empty(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(Empty::new().boxed());
    *response.status_mut() = status;
    response
}

fn json(status: StatusCode, body: String) -> Response<Body> {
    let mut response = Response::new(Full::new(Bytes::from(body)).boxed());
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

fn error_response(error: &HttpError) -> Response<Body> {
    let mut response = Response::new(Full::new(Bytes::from(error.body())).boxed());
    *response.status_mut() = error.status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Some(allow) = error.allow {
        headers.insert(ALLOW, HeaderValue::from_static(allow));
    }
    response
}
