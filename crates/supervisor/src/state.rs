//! The supervisor's bookkeeping, kept free of locking and of IO so that every
//! transition is one plain method on [`State`], testable without processes.
//!
//! Each brick that runs, or that something waits on, has a [`Brick`] in an
//! explicit [`Phase`]. The state talks to a running brick through the handles
//! of its connection (an unbounded channel and the process's signals, which
//! never block) and to the router, whose calls never block either. What it
//! cannot do itself, launching a process and arming a timer, it asks of the
//! shell as an [`Effect`]; the shell reports back what happens to the process
//! and when a timer fires.
//!
//! Stale events are told apart by stamps drawn from one counter: each launch
//! has a generation, each phase an epoch, each `subscribe` a request id. A
//! message from an earlier process, or a timer armed for an earlier phase,
//! carries an old stamp and is ignored.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::mem;
use std::sync::Arc;
use std::time::Duration;

use nowcast_traits::{
    AnchorId, BrickDeclaration, BrickProcess, BrickUnavailable, InputError, ResolveError, Resolved,
    Router, UnknownFlow,
};
use nowcast_types::{
    BrickMessage, BrickName, FlowId, PROTOCOL_VERSION, PublishErrorCode, QualifiedFlowId,
    RequestId, ServiceMessage,
};
use serde::de::IgnoredAny;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

use crate::Config;

/// Where the answer to a `resolve` goes.
pub(crate) type Reply = oneshot::Sender<Result<Resolved, ResolveError>>;

/// What the shell carries out for the state.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    /// Launch the brick, then report the connection to [`State::launched`],
    /// or a failure to [`State::exited`], with this generation.
    Launch { brick: BrickName, generation: u64 },
    /// Call [`State::fired`] with `timer` once `after` has passed.
    Timer { after: Duration, timer: Timer },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Timer {
    pub(crate) brick: BrickName,
    pub(crate) kind: TimerKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimerKind {
    /// Ends the phase entered at this epoch: the `ready` timeout, the grace
    /// period, a step of the stop, or the backoff.
    Phase(u64),
    /// The `subscribe` with this request id has had its time to be answered.
    Answer(RequestId),
}

/// Where a brick is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// No process and nothing waiting: the brick is forgotten.
    Idle,
    /// Launched and sent `hello`, waiting for `ready`. Subscriptions queue.
    Starting,
    /// Ready, with live flows or requests in flight.
    Running,
    /// Ready, with nothing live: stopped when the grace period ends.
    Grace,
    /// Being stopped, the step saying what was sent last. Subscriptions
    /// queue, and relaunch the brick once it has exited.
    Stopping(Stop),
    /// Crashed with live flows, waiting to be relaunched.
    Backoff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    Shutdown,
    Terminated,
    Killed,
}

/// A running process: from its launch to its exit.
struct Process {
    declaration: Arc<BrickDeclaration>,
    to_brick: UnboundedSender<ServiceMessage>,
    signals: Box<dyn BrickProcess>,
}

/// What a flow was subscribed with, to subscribe it again after a restart.
struct Subscription {
    kind: String,
    params: Vec<u8>,
}

/// A `resolve` waiting for its flow, to subscribe `anchor` to it.
pub(crate) struct Waiter {
    anchor: AnchorId,
    kind: String,
    params: Vec<u8>,
    reply: Reply,
}

/// A `subscribe` sent and not yet answered.
enum Request {
    Client(Waiter),
    /// Sent again after a restart for this live flow.
    Resubscribe(FlowId),
}

/// The calls a brick's transitions need from outside it.
struct Ctx<'a> {
    config: &'a Config,
    stamps: &'a mut u64,
    effects: &'a mut Vec<Effect>,
    router: &'a dyn Router,
}

impl Ctx<'_> {
    fn stamp(&mut self) -> u64 {
        *self.stamps += 1;
        *self.stamps
    }
}

struct Brick {
    name: BrickName,
    phase: Phase,
    epoch: u64,
    generation: u64,
    process: Option<Process>,
    /// The flows the brick serves for someone: assigned by `subscribed` and
    /// not released since. They survive a crash.
    flows: BTreeMap<FlowId, Subscription>,
    /// Flows to release, held while a client `subscribe` is in flight: a
    /// `release` sent then could reach the brick after a `subscribe` it
    /// already took for that flow. Released once none is, and only if still
    /// without a subscriber. They may be flows the brick serves without the
    /// supervisor counting them live, from an answer nobody waits for.
    deferred: BTreeSet<FlowId>,
    /// Subscriptions waiting for the brick to be ready.
    queue: Vec<Waiter>,
    requests: HashMap<RequestId, Request>,
    /// Crashes in a row, the one being handled included.
    crashes: u32,
    ready_at: Option<Instant>,
    /// Why the last launch failed, until the next one: what subscriptions
    /// failing meanwhile tell the client.
    launch_error: Option<String>,
}

/// Every brick the supervisor looks after.
pub(crate) struct State {
    config: Config,
    bricks: HashMap<BrickName, Brick>,
    stamps: u64,
    effects: Vec<Effect>,
    /// Set by [`State::shutdown`], never cleared: no brick launches again.
    closing: bool,
}

impl State {
    pub(crate) fn new(config: Config) -> Self {
        Self {
            config,
            bricks: HashMap::new(),
            stamps: 0,
            effects: Vec::new(),
            closing: false,
        }
    }

    /// The effects asked for since the last call.
    pub(crate) fn take_effects(&mut self) -> Vec<Effect> {
        mem::take(&mut self.effects)
    }

    /// The bricks looked after.
    pub(crate) fn names(&self) -> Vec<BrickName> {
        self.bricks.keys().cloned().collect()
    }

    #[cfg(test)]
    fn phase(&self, brick: &BrickName) -> Phase {
        self.bricks.get(brick).map_or(Phase::Idle, |b| b.phase)
    }

    /// Runs `transition` on the brick, if known, then forgets it once idle.
    fn with(
        &mut self,
        brick: &BrickName,
        router: &dyn Router,
        transition: impl FnOnce(&mut Brick, &mut Ctx<'_>),
    ) {
        let Some(entry) = self.bricks.get_mut(brick) else {
            return;
        };
        let mut ctx = Ctx {
            config: &self.config,
            stamps: &mut self.stamps,
            effects: &mut self.effects,
            router,
        };
        transition(entry, &mut ctx);
        if entry.phase == Phase::Idle {
            self.bricks.remove(brick);
        }
    }

    /// Maps a subscription of a declared brick to its flow, launching the
    /// brick if needed, and subscribes `anchor` to it in the router once the
    /// brick has answered. The subscription, or the error, comes on the
    /// receiver.
    pub(crate) fn resolve(
        &mut self,
        anchor: AnchorId,
        brick: &BrickName,
        kind: &str,
        params: &[u8],
        router: &dyn Router,
    ) -> oneshot::Receiver<Result<Resolved, ResolveError>> {
        let (reply, answer) = oneshot::channel();
        if self.closing {
            let unavailable = BrickUnavailable::new(brick.clone());
            let _ = reply.send(Err(ResolveError::Unavailable(unavailable)));
            return answer;
        }
        let waiter = Waiter {
            anchor,
            kind: kind.to_owned(),
            params: params.to_vec(),
            reply,
        };
        self.bricks
            .entry(brick.clone())
            .or_insert_with(|| Brick::new(brick.clone()));
        self.with(brick, router, |b, ctx| b.resolve(waiter, ctx));
        answer
    }

    pub(crate) fn launched(
        &mut self,
        brick: &BrickName,
        generation: u64,
        declaration: Arc<BrickDeclaration>,
        to_brick: UnboundedSender<ServiceMessage>,
        signals: Box<dyn BrickProcess>,
        router: &dyn Router,
    ) {
        let process = Process {
            declaration,
            to_brick,
            signals,
        };
        self.with(brick, router, |b, _| b.launched(generation, process));
    }

    /// The brick's output closed: its process ended, or never started.
    pub(crate) fn exited(&mut self, brick: &BrickName, generation: u64, router: &dyn Router) {
        self.with(brick, router, |b, ctx| b.exited(generation, ctx));
    }

    /// The brick's process could not be started, for `error`: an exit whose
    /// cause the subscriptions it fails carry.
    pub(crate) fn launch_failed(
        &mut self,
        brick: &BrickName,
        generation: u64,
        error: String,
        router: &dyn Router,
    ) {
        self.with(brick, router, |b, ctx| {
            if generation == b.generation {
                b.launch_error = Some(error);
            }
            b.exited(generation, ctx);
        });
    }

    pub(crate) fn message(
        &mut self,
        brick: &BrickName,
        generation: u64,
        msg: BrickMessage,
        router: &dyn Router,
    ) {
        self.with(brick, router, |b, ctx| b.message(generation, msg, ctx));
    }

    pub(crate) fn fired(&mut self, timer: &Timer, router: &dyn Router) {
        self.with(&timer.brick, router, |b, ctx| b.fired(timer.kind, ctx));
    }

    /// The router signalled that `flow` lost its last subscriber.
    pub(crate) fn emptied(&mut self, flow: &QualifiedFlowId, router: &dyn Router) {
        self.with(flow.brick(), router, |b, ctx| b.emptied(flow, ctx));
    }

    /// Discovery no longer holds the brick.
    pub(crate) fn removed(&mut self, brick: &BrickName, router: &dyn Router) {
        self.with(brick, router, |b, ctx| b.removed(ctx));
    }

    /// The service is stopping: every brick stops for good. Subscriptions
    /// waiting fail as unavailable and the flows end. A brick with a process
    /// gets `shutdown`, then the signals, as at the end of a grace period; one
    /// already stopping goes on from its current step; one waiting to be
    /// relaunched is forgotten. Later subscriptions fail as unavailable.
    pub(crate) fn shutdown(&mut self, router: &dyn Router) {
        self.closing = true;
        for brick in self.names() {
            self.with(&brick, router, |b, ctx| b.shutdown(ctx));
        }
    }

    /// Whether [`State::shutdown`] was called and every process has exited
    /// since.
    pub(crate) fn is_drained(&self) -> bool {
        self.closing && self.bricks.is_empty()
    }

    pub(crate) fn refresh(&self, flow: &QualifiedFlowId) -> Result<(), UnknownFlow> {
        let brick = self
            .bricks
            .get(flow.brick())
            .filter(|b| b.flows.contains_key(flow.flow()))
            .ok_or_else(|| UnknownFlow(flow.clone()))?;
        if brick.is_ready() {
            brick.send(ServiceMessage::Refresh {
                flow: flow.flow().clone(),
            });
        } else {
            // The relaunched brick publishes again once subscribed again.
            debug!(brick = %brick.name, %flow, "refresh dropped: the brick is not running");
        }
        Ok(())
    }

    pub(crate) fn kind(&self, flow: &QualifiedFlowId) -> Option<String> {
        let brick = self.bricks.get(flow.brick())?;
        brick.flows.get(flow.flow()).map(|s| s.kind.clone())
    }

    pub(crate) fn input(
        &self,
        flow: &QualifiedFlowId,
        data_type: &str,
        detail: Vec<u8>,
    ) -> Result<(), InputError> {
        let brick = self
            .bricks
            .get(flow.brick())
            .filter(|b| b.flows.contains_key(flow.flow()))
            .ok_or_else(|| InputError::UnknownFlow(UnknownFlow(flow.clone())))?;
        if !brick.is_ready() {
            // Unlike a refresh, an input carries data the brick would lose.
            return Err(InputError::Unavailable(brick.why_unavailable()));
        }
        brick.send(ServiceMessage::Input {
            flow: flow.flow().clone(),
            data_type: data_type.to_owned(),
            detail,
        });
        Ok(())
    }
}

impl Waiter {
    fn fail(self, error: ResolveError) {
        // A dropped receiver is a client that went away: nothing to tell.
        let _ = self.reply.send(Err(error));
    }
}

impl Brick {
    fn new(name: BrickName) -> Self {
        Self {
            name,
            phase: Phase::Idle,
            epoch: 0,
            generation: 0,
            process: None,
            flows: BTreeMap::new(),
            deferred: BTreeSet::new(),
            queue: Vec::new(),
            requests: HashMap::new(),
            crashes: 0,
            ready_at: None,
            launch_error: None,
        }
    }

    fn qualify(&self, flow: &FlowId) -> QualifiedFlowId {
        QualifiedFlowId::new(self.name.clone(), flow.clone())
    }

    fn unavailable(&self) -> ResolveError {
        ResolveError::Unavailable(self.why_unavailable())
    }

    fn why_unavailable(&self) -> BrickUnavailable {
        BrickUnavailable {
            brick: self.name.clone(),
            cause: self.launch_error.clone(),
        }
    }

    /// Whether the process is ready and knows the live flows.
    fn is_ready(&self) -> bool {
        matches!(self.phase, Phase::Running | Phase::Grace)
    }

    fn has_client_requests(&self) -> bool {
        self.requests
            .values()
            .any(|r| matches!(r, Request::Client(_)))
    }

    /// Sends to the process, if one runs. A send fails only once the process
    /// is going away, which its exit reports.
    fn send(&self, msg: ServiceMessage) {
        if let Some(process) = &self.process {
            let _ = process.to_brick.send(msg);
        }
    }

    /// Changes phase and arms the phase's timer, if it has one. The new epoch
    /// makes the timers of earlier phases stale.
    fn enter(&mut self, phase: Phase, ctx: &mut Ctx<'_>) {
        self.phase = phase;
        self.epoch = ctx.stamp();
        let after = match phase {
            Phase::Starting => ctx.config.ready_timeout,
            Phase::Grace => ctx.config.grace,
            Phase::Stopping(Stop::Shutdown) => ctx.config.terminate_after,
            Phase::Stopping(Stop::Terminated) => ctx.config.kill_after,
            Phase::Backoff => ctx.config.backoff(self.crashes),
            Phase::Idle | Phase::Running | Phase::Stopping(Stop::Killed) => return,
        };
        self.arm(after, TimerKind::Phase(self.epoch), ctx);
    }

    fn arm(&self, after: Duration, kind: TimerKind, ctx: &mut Ctx<'_>) {
        ctx.effects.push(Effect::Timer {
            after,
            timer: Timer {
                brick: self.name.clone(),
                kind,
            },
        });
    }

    fn launch(&mut self, ctx: &mut Ctx<'_>) {
        self.launch_error = None;
        self.generation = ctx.stamp();
        self.enter(Phase::Starting, ctx);
        ctx.effects.push(Effect::Launch {
            brick: self.name.clone(),
            generation: self.generation,
        });
    }

    fn launched(&mut self, generation: u64, process: Process) {
        if generation != self.generation || self.phase != Phase::Starting {
            // Dropping the connection closes the brick's input.
            return;
        }
        self.process = Some(process);
        self.send(ServiceMessage::Hello {
            name: self.name.clone(),
            protocol: PROTOCOL_VERSION,
        });
    }

    fn stop(&mut self, ctx: &mut Ctx<'_>) {
        self.send(ServiceMessage::Shutdown);
        self.enter(Phase::Stopping(Stop::Shutdown), ctx);
    }

    fn subscribe(&mut self, kind: String, params: Vec<u8>, request: Request, ctx: &mut Ctx<'_>) {
        let req = RequestId(ctx.stamp());
        self.send(ServiceMessage::Subscribe { req, kind, params });
        self.requests.insert(req, request);
        self.arm(ctx.config.answer_timeout, TimerKind::Answer(req), ctx);
    }

    fn resolve(&mut self, waiter: Waiter, ctx: &mut Ctx<'_>) {
        match self.phase {
            Phase::Idle => {
                self.queue.push(waiter);
                self.launch(ctx);
            }
            Phase::Starting | Phase::Stopping(_) => self.queue.push(waiter),
            Phase::Running | Phase::Grace => {
                let (kind, params) = (waiter.kind.clone(), waiter.params.clone());
                self.subscribe(kind, params, Request::Client(waiter), ctx);
            }
            Phase::Backoff => waiter.fail(self.unavailable()),
        }
        self.settle(ctx);
    }

    /// Stops sending a flow to anyone. The brick hears of it only if it is
    /// ready: a relaunched brick that is not yet will simply not get it back.
    /// The router forgets the flow and its last value, so that a later
    /// subscriber starts afresh; a subscriber still there gets `flow-ended`.
    fn release(&mut self, flow: &FlowId, ctx: &mut Ctx<'_>) {
        self.flows.remove(flow);
        self.deferred.remove(flow);
        if self.is_ready() {
            self.send(ServiceMessage::Release { flow: flow.clone() });
        }
        ctx.router.end_flow(&self.qualify(flow));
    }

    /// Ends a flow the brick can no longer serve: its clients get
    /// `flow-error` with `message`, then `flow-ended`.
    fn lose(&mut self, flow: &FlowId, message: &str, ctx: &mut Ctx<'_>) {
        self.flows.remove(flow);
        self.deferred.remove(flow);
        let qualified = self.qualify(flow);
        ctx.router.flow_error(&qualified, message.to_owned());
        ctx.router.end_flow(&qualified);
    }

    /// Releases a flow nobody holds, or defers it while a client `subscribe`
    /// is in flight. Every release decision goes through here.
    fn let_go(&mut self, flow: &FlowId, ctx: &mut Ctx<'_>) {
        if self.has_client_requests() {
            self.deferred.insert(flow.clone());
        } else {
            self.release(flow, ctx);
        }
    }

    /// Releases the deferred flows still without a subscriber, once no client
    /// `subscribe` can hand them out any more.
    fn flush_deferred(&mut self, ctx: &mut Ctx<'_>) {
        if self.has_client_requests() {
            return;
        }
        for flow in mem::take(&mut self.deferred) {
            if ctx.router.subscribers(&self.qualify(&flow)) == 0 {
                self.release(&flow, ctx);
            }
        }
    }

    /// Moves between running and grace, or out of a backoff that has nothing
    /// left to relaunch for. Runs after every transition.
    fn settle(&mut self, ctx: &mut Ctx<'_>) {
        self.flush_deferred(ctx);
        let busy = !self.flows.is_empty() || !self.requests.is_empty() || !self.queue.is_empty();
        match self.phase {
            Phase::Running if !busy => self.enter(Phase::Grace, ctx),
            Phase::Grace if busy => self.enter(Phase::Running, ctx),
            Phase::Backoff if self.flows.is_empty() => self.enter(Phase::Idle, ctx),
            _ => {}
        }
    }

    fn exited(&mut self, generation: u64, ctx: &mut Ctx<'_>) {
        if generation != self.generation || matches!(self.phase, Phase::Idle | Phase::Backoff) {
            return;
        }
        self.process = None;
        match self.phase {
            Phase::Stopping(_) => {
                info!(brick = %self.name, "brick stopped");
                if !self.flows.is_empty() {
                    // Stopped for missing `ready` on a relaunch: still owed.
                    self.restart(ctx);
                } else if !self.queue.is_empty() {
                    self.crashes = 0;
                    self.launch(ctx);
                } else {
                    self.enter(Phase::Idle, ctx);
                }
            }
            _ => {
                warn!(brick = %self.name, "brick exited unexpectedly");
                let unavailable = self.unavailable();
                for request in mem::take(&mut self.requests).into_values() {
                    if let Request::Client(waiter) = request {
                        waiter.fail(unavailable.clone());
                    }
                }
                self.flush_deferred(ctx);
                for flow in self.flows.keys() {
                    ctx.router.flow_error(
                        &self.qualify(flow),
                        "the brick stopped unexpectedly, relaunching it".into(),
                    );
                }
                self.restart(ctx);
            }
        }
        self.settle(ctx);
    }

    /// After a crash: relaunches the brick, after a backoff, while it has
    /// live flows, and gives up on them after too many crashes in a row.
    /// Subscriptions waiting fail: the brick is unavailable for now.
    fn restart(&mut self, ctx: &mut Ctx<'_>) {
        let unavailable = self.unavailable();
        for waiter in self.queue.drain(..) {
            waiter.fail(unavailable.clone());
        }
        if self.flows.is_empty() {
            self.enter(Phase::Idle, ctx);
            return;
        }
        if self
            .ready_at
            .take()
            .is_some_and(|ready| ready.elapsed() >= ctx.config.stable_after)
        {
            self.crashes = 0;
        }
        self.crashes += 1;
        if self.crashes > ctx.config.max_restarts {
            error!(
                brick = %self.name,
                crashes = self.crashes,
                "giving up on the brick: its flows end"
            );
            for flow in mem::take(&mut self.flows).into_keys() {
                self.lose(&flow, "the brick keeps crashing, giving up on it", ctx);
            }
            self.enter(Phase::Idle, ctx);
        } else {
            self.enter(Phase::Backoff, ctx);
        }
    }

    fn fired(&mut self, kind: TimerKind, ctx: &mut Ctx<'_>) {
        match kind {
            TimerKind::Answer(req) => {
                let Some(request) = self.requests.remove(&req) else {
                    return;
                };
                warn!(brick = %self.name, req = req.0, "brick did not answer subscribe in time");
                match request {
                    Request::Client(waiter) => waiter.fail(self.unavailable()),
                    Request::Resubscribe(flow) if self.flows.contains_key(&flow) => {
                        let message = "the relaunched brick did not take the flow back";
                        self.lose(&flow, message, ctx);
                    }
                    // Released meanwhile: nobody is left to tell.
                    Request::Resubscribe(_) => {}
                }
            }
            TimerKind::Phase(epoch) if epoch == self.epoch => match self.phase {
                Phase::Starting => {
                    warn!(brick = %self.name, "brick did not answer hello in time, stopping it");
                    let unavailable = self.unavailable();
                    for waiter in self.queue.drain(..) {
                        waiter.fail(unavailable.clone());
                    }
                    self.stop(ctx);
                }
                Phase::Grace => self.stop(ctx),
                Phase::Stopping(Stop::Shutdown) => {
                    if let Some(process) = &mut self.process {
                        process.signals.terminate();
                    }
                    self.enter(Phase::Stopping(Stop::Terminated), ctx);
                }
                Phase::Stopping(Stop::Terminated) => {
                    if let Some(process) = &mut self.process {
                        process.signals.kill();
                    }
                    self.enter(Phase::Stopping(Stop::Killed), ctx);
                }
                Phase::Backoff => self.launch(ctx),
                Phase::Idle | Phase::Running | Phase::Stopping(Stop::Killed) => {}
            },
            TimerKind::Phase(_) => return,
        }
        self.settle(ctx);
    }

    fn message(&mut self, generation: u64, msg: BrickMessage, ctx: &mut Ctx<'_>) {
        if generation != self.generation || self.process.is_none() {
            return;
        }
        match msg {
            BrickMessage::Ready => self.ready(ctx),
            BrickMessage::Subscribed { req, flow } => self.subscribed(req, flow, ctx),
            BrickMessage::Error { req, code, message } => self.refused(req, code, message, ctx),
            BrickMessage::Publish {
                req,
                flow,
                data_type,
                detail,
            } => {
                let answer = match self.publish(&flow, data_type, detail, ctx) {
                    Ok(()) => ServiceMessage::Ack { req },
                    Err(code) => {
                        debug!(brick = %self.name, %flow, req = req.0, code = code.as_str(), "publish refused");
                        ServiceMessage::Error { req, code }
                    }
                };
                self.send(answer);
            }
            BrickMessage::FlowError { flow, message } => {
                if self.flows.contains_key(&flow) {
                    ctx.router.flow_error(&self.qualify(&flow), message);
                } else {
                    debug!(brick = %self.name, %flow, "flow-error on a flow it does not serve");
                }
            }
        }
        self.settle(ctx);
    }

    fn ready(&mut self, ctx: &mut Ctx<'_>) {
        if self.phase != Phase::Starting {
            warn!(brick = %self.name, "unexpected ready, ignored");
            return;
        }
        info!(brick = %self.name, "brick ready");
        self.ready_at = Some(Instant::now());
        self.enter(Phase::Running, ctx);
        let live: Vec<(FlowId, String, Vec<u8>)> = self
            .flows
            .iter()
            .map(|(flow, s)| (flow.clone(), s.kind.clone(), s.params.clone()))
            .collect();
        for (flow, kind, params) in live {
            self.subscribe(kind, params, Request::Resubscribe(flow), ctx);
        }
        for waiter in mem::take(&mut self.queue) {
            if waiter.reply.is_closed() {
                continue;
            }
            let (kind, params) = (waiter.kind.clone(), waiter.params.clone());
            self.subscribe(kind, params, Request::Client(waiter), ctx);
        }
    }

    fn subscribed(&mut self, req: RequestId, flow: FlowId, ctx: &mut Ctx<'_>) {
        match self.requests.remove(&req) {
            None => {
                warn!(brick = %self.name, req = req.0, %flow, "subscribed answers no subscribe in flight");
                // The brick now serves the flow: tell it nobody wants it.
                if !self.flows.contains_key(&flow) {
                    self.let_go(&flow, ctx);
                }
            }
            Some(Request::Client(Waiter {
                anchor,
                kind,
                params,
                reply,
            })) => {
                if reply.is_closed() {
                    // The client went away: no subscription, and a flow
                    // nobody else holds goes back to the brick.
                    if !self.flows.contains_key(&flow) {
                        self.let_go(&flow, ctx);
                    }
                    return;
                }
                self.flows
                    .entry(flow.clone())
                    .or_insert(Subscription { kind, params });
                // Under the lock, so ordered with every publication and
                // every release decision on the flow.
                let qualified = self.qualify(&flow);
                let (subscription, last) = ctx.router.subscribe(anchor, qualified.clone());
                let resolved = Resolved {
                    subscription,
                    flow: qualified,
                    last,
                };
                if let Err(Ok(resolved)) = reply.send(Ok(resolved)) {
                    // Gone since the check: the router's empty-flow signal
                    // then releases the flow.
                    let _ = ctx.router.unsubscribe(anchor, &resolved.subscription);
                }
            }
            Some(Request::Resubscribe(live)) => {
                if flow != live {
                    warn!(brick = %self.name, %live, %flow, "relaunched brick assigned another flow id");
                    if self.flows.contains_key(&live) {
                        self.lose(&live, "the relaunched brick changed the flow's id", ctx);
                    }
                    if !self.flows.contains_key(&flow) {
                        self.let_go(&flow, ctx);
                    }
                }
            }
        }
    }

    fn refused(&mut self, req: RequestId, code: String, message: String, ctx: &mut Ctx<'_>) {
        match self.requests.remove(&req) {
            None => {
                warn!(brick = %self.name, req = req.0, code, "error answers no subscribe in flight");
            }
            Some(Request::Client(waiter)) => waiter.fail(ResolveError::Refused { code, message }),
            Some(Request::Resubscribe(flow)) => {
                if self.flows.contains_key(&flow) {
                    let message =
                        format!("the relaunched brick refused the flow: {code}: {message}");
                    self.lose(&flow, &message, ctx);
                }
            }
        }
    }

    /// Checks a `publish` and routes it. Invalid JSON never reaches the
    /// router.
    fn publish(
        &self,
        flow: &FlowId,
        data_type: String,
        detail: Vec<u8>,
        ctx: &mut Ctx<'_>,
    ) -> Result<(), PublishErrorCode> {
        if !self.flows.contains_key(flow) {
            return Err(PublishErrorCode::UnknownFlow);
        }
        let declared = self
            .process
            .as_ref()
            .is_some_and(|p| p.declaration.types.contains_key(&data_type));
        if !declared {
            return Err(PublishErrorCode::UndeclaredType);
        }
        if serde_json::from_slice::<IgnoredAny>(&detail).is_err() {
            return Err(PublishErrorCode::InvalidJson);
        }
        if ctx
            .router
            .publish(&self.qualify(flow), data_type, detail)
            .is_err()
        {
            // A live flow always has a router entry, made with its first
            // subscription and kept until the supervisor ends it.
            debug!(brick = %self.name, %flow, "the router does not know the flow");
        }
        Ok(())
    }

    fn emptied(&mut self, flow: &QualifiedFlowId, ctx: &mut Ctx<'_>) {
        let id = flow.flow();
        // A subscriber may have come back since the signal.
        if !self.flows.contains_key(id) || ctx.router.subscribers(flow) > 0 {
            return;
        }
        self.let_go(id, ctx);
        self.settle(ctx);
    }

    /// The service stops: see [`State::shutdown`]. Clearing the flows and the
    /// queue is what keeps the exit from relaunching the brick.
    fn shutdown(&mut self, ctx: &mut Ctx<'_>) {
        let unavailable = self.unavailable();
        for waiter in self.queue.drain(..) {
            waiter.fail(unavailable.clone());
        }
        for request in mem::take(&mut self.requests).into_values() {
            if let Request::Client(waiter) = request {
                waiter.fail(unavailable.clone());
            }
        }
        let flows = mem::take(&mut self.flows).into_keys();
        for flow in flows.chain(mem::take(&mut self.deferred)) {
            ctx.router.end_flow(&self.qualify(&flow));
        }
        match self.phase {
            Phase::Starting | Phase::Running | Phase::Grace => {
                info!(brick = %self.name, "the service stops, stopping the brick");
                self.stop(ctx);
            }
            Phase::Backoff => self.enter(Phase::Idle, ctx),
            Phase::Idle | Phase::Stopping(_) => {}
        }
    }

    /// The brick left discovery: as if each flow lost its last subscriber,
    /// except that the flows end for their clients. Subscriptions waiting
    /// fail as for an unknown brick.
    fn removed(&mut self, ctx: &mut Ctx<'_>) {
        let unknown = ResolveError::UnknownBrick(self.name.clone());
        for waiter in self.queue.drain(..) {
            waiter.fail(unknown.clone());
        }
        for request in mem::take(&mut self.requests).into_values() {
            if let Request::Client(waiter) = request {
                waiter.fail(unknown.clone());
            }
        }
        let flows: Vec<FlowId> = self.flows.keys().cloned().collect();
        for flow in flows {
            // Its clients get `flow-ended`.
            self.release(&flow, ctx);
        }
        self.settle(ctx);
    }
}

#[cfg(test)]
mod tests {
    use nowcast_conformance::probe;
    use nowcast_traits::fake::{FakeBrick, FakeLauncher, FakeRouter, Signal};
    use nowcast_traits::{AnchorId, BrickLauncher};
    use nowcast_types::StreamEvent;

    use super::*;

    const A: AnchorId = AnchorId(1);

    struct Fixture {
        state: State,
        router: FakeRouter,
        launcher: FakeLauncher,
        declaration: Arc<BrickDeclaration>,
    }

    fn fixture(config: Config) -> Fixture {
        Fixture {
            state: State::new(config),
            router: FakeRouter::new(),
            launcher: FakeLauncher::new(),
            declaration: Arc::new(probe::declaration(vec!["probe".into()], "/".into())),
        }
    }

    fn qualified(id: &str) -> QualifiedFlowId {
        QualifiedFlowId::new(probe::name(), FlowId::new(id).unwrap())
    }

    impl Fixture {
        fn phase(&self) -> Phase {
            self.state.phase(&probe::name())
        }

        fn resolve(&mut self, id: &str) -> oneshot::Receiver<Result<Resolved, ResolveError>> {
            let params = probe::params(id);
            self.state
                .resolve(A, &probe::name(), probe::KIND, &params, &self.router)
        }

        /// Takes the effects, expecting a launch, and carries it out.
        fn launch(&mut self) -> (u64, FakeBrick) {
            let generation = self
                .state
                .take_effects()
                .into_iter()
                .find_map(|e| match e {
                    Effect::Launch { generation, .. } => Some(generation),
                    Effect::Timer { .. } => None,
                })
                .expect("a launch");
            let connection = self.launcher.launch(&self.declaration).unwrap();
            let mut brick = self.launcher.try_next().unwrap();
            self.state.launched(
                &probe::name(),
                generation,
                self.declaration.clone(),
                connection.to_brick,
                connection.process,
                &self.router,
            );
            assert!(matches!(
                brick.from_service.try_recv(),
                Ok(ServiceMessage::Hello { .. })
            ));
            (generation, brick)
        }

        fn message(&mut self, generation: u64, msg: BrickMessage) {
            self.state
                .message(&probe::name(), generation, msg, &self.router);
        }

        /// The one phase timer among the effects, with its delay.
        fn phase_timer(&mut self) -> (Duration, Timer) {
            let mut timers = self
                .state
                .take_effects()
                .into_iter()
                .filter_map(|e| match e {
                    Effect::Timer { after, timer } if matches!(timer.kind, TimerKind::Phase(_)) => {
                        Some((after, timer))
                    }
                    _ => None,
                });
            let timer = timers.next().expect("a phase timer");
            assert!(timers.next().is_none(), "one phase timer");
            timer
        }

        fn fire(&mut self, timer: &Timer) {
            self.state.fired(timer, &self.router);
        }

        /// Answers the brick's next `subscribe` with flow `id`.
        fn serve(&mut self, generation: u64, brick: &mut FakeBrick, id: &str) {
            let Ok(ServiceMessage::Subscribe { req, .. }) = brick.from_service.try_recv() else {
                panic!("expected subscribe");
            };
            let flow = FlowId::new(id).unwrap();
            self.message(generation, BrickMessage::Subscribed { req, flow });
        }

        /// A brick launched, ready and serving flow `a` to anchor `A`.
        fn running(&mut self) -> (u64, FakeBrick) {
            let mut answer = self.resolve("a");
            let (generation, mut brick) = self.launch();
            self.message(generation, BrickMessage::Ready);
            self.serve(generation, &mut brick, "a");
            assert_eq!(answer.try_recv().unwrap().unwrap().flow, qualified("a"));
            self.state.take_effects();
            (generation, brick)
        }
    }

    #[test]
    fn a_brick_goes_from_launch_to_grace_to_each_stop_step() {
        let config = Config::default();
        let mut f = fixture(config.clone());
        let mut answer = f.resolve("a");
        assert_eq!(f.phase(), Phase::Starting);
        let (generation, mut brick) = f.launch();
        f.message(generation, BrickMessage::Ready);
        assert_eq!(f.phase(), Phase::Running);
        f.serve(generation, &mut brick, "a");
        let resolved = answer.try_recv().unwrap().unwrap();
        assert_eq!(resolved.flow, qualified("a"));
        assert_eq!(f.router.subscribers(&qualified("a")), 1);
        f.state.take_effects();

        // The anchor leaves: the flow is released.
        f.router.unsubscribe(A, &resolved.subscription).unwrap();
        f.state.emptied(&qualified("a"), &f.router);
        assert_eq!(
            brick.from_service.try_recv(),
            Ok(ServiceMessage::Release {
                flow: FlowId::new("a").unwrap()
            })
        );
        assert_eq!(f.phase(), Phase::Grace);
        let (after, grace) = f.phase_timer();
        assert_eq!(after, config.grace);

        f.fire(&grace);
        assert_eq!(brick.from_service.try_recv(), Ok(ServiceMessage::Shutdown));
        assert_eq!(f.phase(), Phase::Stopping(Stop::Shutdown));
        let (after, term) = f.phase_timer();
        assert_eq!(after, config.terminate_after);

        f.fire(&term);
        assert_eq!(brick.signals.try_recv(), Ok(Signal::Term));
        assert_eq!(f.phase(), Phase::Stopping(Stop::Terminated));
        let (after, kill) = f.phase_timer();
        assert_eq!(after, config.kill_after);

        f.fire(&kill);
        assert_eq!(brick.signals.try_recv(), Ok(Signal::Kill));
        assert_eq!(f.phase(), Phase::Stopping(Stop::Killed));
        assert!(f.state.take_effects().is_empty());

        f.state.exited(&probe::name(), generation, &f.router);
        assert_eq!(f.phase(), Phase::Idle);
        assert!(f.state.names().is_empty());
    }

    #[test]
    fn a_timer_of_an_earlier_phase_is_ignored() {
        let mut f = fixture(Config::default());
        let (_, mut brick) = f.running();
        f.router.remove_anchor(A);
        f.state.emptied(&qualified("a"), &f.router);
        brick.from_service.try_recv().unwrap();
        let (_, grace) = f.phase_timer();
        // A subscription within the grace period cancels it.
        let _answer = f.resolve("b");
        assert_eq!(f.phase(), Phase::Running);
        f.fire(&grace);
        assert_eq!(f.phase(), Phase::Running);
        assert!(matches!(
            brick.from_service.try_recv(),
            Ok(ServiceMessage::Subscribe { .. })
        ));
        assert!(brick.from_service.try_recv().is_err(), "no shutdown");
    }

    #[test]
    fn messages_and_exits_of_an_earlier_launch_are_ignored() {
        let mut f = fixture(Config::default());
        let (first, _brick) = f.running();
        f.state.exited(&probe::name(), first, &f.router);
        assert_eq!(f.phase(), Phase::Backoff);
        let (_, backoff) = f.phase_timer();
        f.fire(&backoff);
        let (second, _relaunched) = f.launch();
        f.message(first, BrickMessage::Ready);
        f.state.exited(&probe::name(), first, &f.router);
        assert_eq!(f.phase(), Phase::Starting);
        f.message(second, BrickMessage::Ready);
        assert_eq!(f.phase(), Phase::Running);
    }

    #[test]
    fn a_crash_beyond_the_restarts_allowed_ends_the_flows_and_forgets_the_brick() {
        let mut f = fixture(Config {
            max_restarts: 0,
            ..Config::default()
        });
        let (generation, _brick) = f.running();
        let mut stream = f.router.stream(A, None);
        f.state.exited(&probe::name(), generation, &f.router);
        assert!(matches!(
            stream.try_recv(),
            Ok(StreamEvent::FlowError { .. })
        ));
        assert!(matches!(
            stream.try_recv(),
            Ok(StreamEvent::FlowError { .. })
        ));
        assert_eq!(
            stream.try_recv(),
            Ok(StreamEvent::FlowEnded {
                flow: qualified("a")
            })
        );
        assert!(f.state.names().is_empty());
        assert!(f.state.take_effects().is_empty(), "no relaunch");
    }

    #[test]
    fn a_brick_given_up_on_gets_its_restarts_back_on_a_new_subscription() {
        let mut f = fixture(Config {
            max_restarts: 1,
            ..Config::default()
        });
        let (generation, _brick) = f.running();
        f.state.exited(&probe::name(), generation, &f.router);
        assert_eq!(f.phase(), Phase::Backoff);
        let (_, timer) = f.phase_timer();
        f.fire(&timer);
        let (generation, _brick) = f.launch();
        f.message(generation, BrickMessage::Ready);
        f.state.exited(&probe::name(), generation, &f.router);
        assert!(f.state.names().is_empty(), "given up");

        let (generation, _brick) = f.running();
        f.state.exited(&probe::name(), generation, &f.router);
        assert_eq!(f.phase(), Phase::Backoff, "relaunched, not given up");
    }
}
