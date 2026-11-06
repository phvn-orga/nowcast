// The state the nowcast mod shares with other mods (`$.state`, plugin
// `nowcast`). Any mod reads it; only this mod writes it. A mod follows a value
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

declare module 'claude-code' {
  interface PluginState {
    nowcast: {
      /** One member per flow the session's stream delivered, by flow id. */
      flows: StateFamily<NowcastFlow>
      stream: NowcastStream
    }
  }
}
