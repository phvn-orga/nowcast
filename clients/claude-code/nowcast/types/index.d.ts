// What the nowcast mod gives other mods: the `$.nowcast` noun, to subscribe,
// unsubscribe, ask for a refresh or read a flow's last value from the
// service, and its state (`$.state`, plugin `nowcast`), where every flow the
// session receives lands. A mod using either lists `nowcast` under
// `dependencies` in its `plugin.json`.
//
// Any mod reads the state; only this mod writes it. A mod follows a value
// with a `state.set` hook matching `{ plugin: 'nowcast', key }`, and reads it
// with `$.state.get`. Keep such hooks short: they run inside this mod's write.
//
// The state belongs to the Claude Code session: after a `/clear` or a resume
// it starts empty, and this mod writes the last value of every flow the
// session's process is subscribed to into it again.

/**
 * One update of a flow: a CloudEvents event in JSON, as the service sends it
 * (contract section 3, "Le flux d'événements").
 */
export type NowcastEvent = {
  specversion: string
  /** Given by the service from one counter for every flow. */
  id: string
  /** `nowcast://bricks/<brick>`. */
  source: string
  /** The published type, for example `forge.pr/v1`. */
  type: string
  /** The flow id, `<brick>/<flow>`. */
  subject: string
  /** RFC 3339. */
  time: string
  dataschema: string
  /**
   * The brick's detail, the JSON it published. Typed `unknown`: a recursive
   * JSON type here makes the `state.set` hooks of other mods too deep to
   * type-check.
   */
  data: unknown
}

/**
 * What the session's stream said last about one flow. The member's `id` is
 * the flow id, as `POST /v1/subscriptions` answers it in `flow`.
 */
export type NowcastFlow = {
  /** The flow's last update; null until one arrives. */
  last: NowcastEvent | null
  /**
   * The brick's message from the last `flow-error`, cleared by the next
   * update. `last` stays: it is the last value the brick served.
   */
  error: string | null
  /**
   * The flow no longer exists (subscription deleted, brick removed). A later
   * subscription to the same flow brings it back with its next update.
   */
  ended: boolean
}

/**
 * The stream this mod holds for the session. While `status` is `up`, the
 * service knows `session`: requests of any mod in this process are tied to it.
 */
export type NowcastStream = {
  /** The session id this mod declares to the service; null before the first. */
  session: string | null
  /**
   * `starting` until the service first answers; `up` while it answers; `down`
   * while the `nowcast` command has lost it and retries; `stopped` when the
   * command is not running (not installed, or exited), see `error`.
   */
  status: 'starting' | 'up' | 'down' | 'stopped'
  /** Why the stream is down or stopped; null while up. */
  error: string | null
}

/**
 * Requests to the service, as `$.nowcast`. Each goes from this process, as
 * the stream does, so the service ties both to the same session.
 *
 * A request answers `{ ok: false }` rather than throwing. A request the
 * service refuses because no session is declared yet declares the current
 * one and is sent again.
 *
 * @example
 * const answer = await $.nowcast.subscribe({ brick: 'forge', kind: 'pr', params: { remote, branch } })
 * if (answer.ok) await $.nowcast.refresh({ flow: answer.flow })
 */
export type Nowcast = {
  /**
   * `POST /v1/subscriptions`. The flow's last value, when it has one, is
   * written to the state as well, since the stream does not send it again.
   *
   * The subscription is kept: if the service restarts and forgets it, it is
   * made again under a new id, and `unsubscribe` with the first id still ends
   * it.
   */
  subscribe(request: NowcastSubscribeRequest): Promise<NowcastSubscription | NowcastFailure>
  /** `DELETE /v1/subscriptions/{subscription}`, with the id `subscribe` gave. */
  unsubscribe(request: { subscription: string }): Promise<NowcastDone | NowcastFailure>
  /** `POST /v1/flows/{flow}/refresh`: the brick publishes again once it has read. */
  refresh(request: { flow: string }): Promise<NowcastDone | NowcastFailure>
  /**
   * `GET /v1/flows/{flow}`, for a flow this process is subscribed to. Most
   * mods read the state instead, which follows the stream.
   */
  last(request: { flow: string }): Promise<NowcastLast | NowcastFailure>
}

export type NowcastSubscribeRequest = {
  /** The brick's name, `forge`. */
  brick: string
  /** The subscription kind its declaration names, `pr`. */
  kind: string
  /** The subscription's content, checked against the kind's JSON Schema. */
  params: Readonly<Record<string, unknown>>
}

export type NowcastSubscription = {
  ok: true
  /** This subscription's own id, for `unsubscribe`. */
  subscription: string
  /** The flow id, `<brick>/<flow>`: the member of the state's `flows`. */
  flow: string
  /** The flow's last update, or null when it has published nothing yet. */
  last: NowcastEvent | null
}

export type NowcastDone = { ok: true }

export type NowcastLast = {
  ok: true
  /** Null when the flow has published nothing yet. */
  last: NowcastEvent | null
}

/** A request that did not succeed. */
export type NowcastFailure = {
  ok: false
  /** The service's HTTP status; null when it was not reached. */
  status: number | null
  /**
   * The contract's error code (`unknown_brick`, `invalid_params`,
   * `brick_unavailable`...), or one of this mod's: `unreachable` (no service
   * on the socket), `no_socket` (no socket path) and `unavailable` (the
   * nowcast mod did not answer).
   */
  code: string
  message: string
}

declare module 'claude-code' {
  interface EngineInterface {
    nowcast: Nowcast
  }

  interface PluginState {
    nowcast: {
      /** One member per flow the session's stream delivered, by flow id. */
      flows: StateFamily<NowcastFlow>
      stream: NowcastStream
    }
  }
}
