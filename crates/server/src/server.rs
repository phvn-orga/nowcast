//! Accepts connections, identifies their anchor, and answers the routes.

use std::convert::Infallible;
use std::os::fd::AsFd;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::header::{ALLOW, CACHE_CONTROL, CONTENT_TYPE, HeaderMap, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use nowcast_traits::{AnchorId, Discovery, Identity, Router, Sessions, Supervisor};
use nowcast_types::{
    ApiError, ApiErrorCode, BrickName, QualifiedFlowId, SessionId, SubscriptionId,
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
use crate::route::{Route, decode};
use crate::sse::{self, SseBody};

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
        if route != Route::Session && self.sessions.current(anchor).is_none() {
            return Err(HttpError::session_required());
        }
        match route {
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
            Route::Flow(flow) => self.last(anchor, &flow_id(flow)?),
            Route::Refresh(flow) => {
                let flow = flow_id(flow)?;
                // Like reading it, refreshing a flow takes a subscription.
                self.router
                    .last(anchor, &flow)
                    .map_err(|_| HttpError::unknown_flow(&flow.to_string()))?;
                self.supervisor
                    .refresh(&flow)
                    .map_err(|_| HttpError::unknown_flow(&flow.to_string()))?;
                Ok(empty(StatusCode::ACCEPTED))
            }
            Route::Stream => self.stream(anchor, &parts.headers),
        }
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
    let bytes = Limited::new(body, MAX_BODY)
        .collect()
        .await
        .map_err(|e| {
            if e.is::<LengthLimitError>() {
                HttpError::payload_too_large(MAX_BODY)
            } else {
                HttpError::invalid_request(format!("cannot read the body: {e}"))
            }
        })?
        .to_bytes();
    serde_json::from_slice(&bytes)
        .map_err(|e| HttpError::invalid_request(format!("the body is not the expected JSON: {e}")))
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
