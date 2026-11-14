// What the nowcast mod gives other mods: the `$.nowcast` noun, to subscribe,
// unsubscribe, ask for a refresh or read a flow's last value or history from
// the service, and its state (`$.state`, plugin `nowcast`), where every
// subscription and every flow the session receives lands. A mod using either
// lists `nowcast` under `dependencies` in its `plugin.json`.
//
// A consumer asks for a flow and reads its updates, nothing more: this mod
// keeps each subscription until it is unsubscribed, retries what the service
// refuses for now, takes again what the service forgot, and clears what an
// earlier subscription left. A consumer never retries, never filters a stale
// value and never calls the service itself.
//
// Any mod reads the state; only this mod writes it. A mod follows a value
// with a `state.set` hook matching `{ plugin: 'nowcast', key }`, and reads it
// with `$.state.get`. Keep such hooks short: they run inside this mod's write.
//
// The state belongs to the Claude Code session: after a `/clear` or a resume
// it starts empty, and this mod writes every subscription it keeps and the
// last value of every flow the session's process is subscribed to into it
// again.

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
 * One subscription this mod keeps, as the member `<mod>/<name>` of the
 * state's `subscriptions`: the mod that took it, and the name it gave.
 */
export type NowcastSubscription = {
  brick: string
  kind: string
  params: Readonly<Record<string, unknown>>
  /**
   * `active` while the service serves it; `retrying` while the service is
   * out of reach or refuses it for now, tried again after 1 s, doubled up to
   * 60 s, and at once when the stream comes back; `failed` when the service
   * refused it for good after it was kept (the brick removed, say), with no
   * attempt of its own: a `subscribe` with the same name and request tries
   * it again.
   */
  status: 'active' | 'retrying' | 'failed'
  /**
   * Why it is retrying or failed, as `NowcastFailure` says it, or `lost`
   * when the service no longer served it; null while active.
   */
  error: { code: string; message: string } | null
  /**
   * The flow id, the member of the state's `flows`; null until the service
   * first accepts the subscription.
   */
  flow: string | null
  /** The service's own id for it while active: this mod's bookkeeping. */
  service: string | null
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
 * A worktree the session wrote in: an `Edit` or `Write` that succeeded, of
 * the main loop or a subagent. Reads and Bash commands do not count.
 */
export type NowcastWorktree = {
  /** The worktree's root, absolute. */
  path: string
  /** The common git directory, the same for every worktree of a repository. */
  repo: string
  /**
   * The branch checked out at the last write, or after a command of the
   * agent that may move it; null on a detached HEAD.
   */
  branch: string | null
  /**
   * The `origin` remote's URL as git has it; null without one. Not the forge
   * key's remote: see `forge`.
   */
  remote: string | null
  /** The last write in it, RFC 3339. */
  written_at: string
  /**
   * Where `git push` sends the branch, the key of the forge brick's `pr` and
   * `pipelines` flows; null on a detached HEAD or without a push remote. Read
   * again at each write in the worktree and after the agent runs a command
   * that may move the branch (`git switch`, `git push -u`...). Absent from an
   * entry an older mod wrote.
   *
   * While the forge brick is enabled, this mod subscribes to both flows of
   * the key under the names `worktree-pr:<path>` and
   * `worktree-pipelines:<path>`, takes them again when the key changes, and
   * drops them when the worktree leaves the list. Each subscription's `flow`
   * names the member of the state's `flows` to read.
   *
   * @example
   * const id = `nowcast/worktree-pr:${worktree.path}`
   * const { value: subscription } = await $.state.get({ plugin: 'nowcast', key: 'subscriptions', id })
   * const flow = subscription?.flow ?? null
   * const pr = flow === null ? undefined : (await $.state.get({ plugin: 'nowcast', key: 'flows', id: flow })).value
   */
  forge?: NowcastForgeKey | null
}

/** A branch on a forge, as the forge brick's `pr` and `pipelines` kinds take it. */
export type NowcastForgeKey = {
  /** The push remote's URL, as `git remote get-url --push` gives it. */
  remote: string
  /** The branch's name on that remote. */
  branch: string
}

/** What to read again on a forge, in neutral terms: see `Nowcast.refreshForge`. */
export type NowcastForgeRefresh = {
  /** A remote's URL. Takes precedence over `cwd`. */
  remote?: string
  /**
   * A directory whose push remote names the repository; relative, from the
   * session's directory. The session's directory when neither it nor
   * `remote` is given.
   */
  cwd?: string
  /** What to read again; all of them when absent. `issues` covers single issues too. */
  types?: Array<'pr' | 'pipelines' | 'issues'>
  /** The number of a PR or issue that changed. */
  number?: number
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
 * const answer = await $.nowcast.subscribe({ name: 'pr', brick: 'forge', kind: 'pr', params: { remote, branch } })
 * if (answer.ok && answer.flow !== null) await $.nowcast.refresh({ flow: answer.flow })
 */
export type Nowcast = {
  /**
   * Keeps a subscription under `name`, until `unsubscribe` with that name.
   * Its id is `<mod>/<name>`, the calling mod's name first: the member of
   * the state's `subscriptions` that says where it stands.
   *
   * The same name with the same request answers the subscription in place;
   * with another request, it replaces it. A mod reloaded subscribes again
   * under the same names and finds its subscriptions; a `/clear` leaves them
   * as they are. A name the mod no longer uses stays subscribed until the
   * process ends.
   *
   * Only a request that can never succeed fails, and nothing is kept for it:
   * `unknown_brick` (a brick not enabled answers it too: enable it, then
   * reload the mod), `unknown_kind`, `invalid_params`, `invalid_request` (a
   * malformed body), or `invalid_name`.
   * Any other failure answers the subscription `retrying`. The flow's last
   * value, once the service gives it, is written to the state's `flows`.
   */
  subscribe(request: NowcastSubscribeRequest): Promise<NowcastSubscribed | NowcastFailure>
  /**
   * Ends the calling mod's subscription `name`; a name it does not hold
   * answers `ok` as well. A delete the service does not receive is sent
   * again each time the stream comes back.
   */
  unsubscribe(request: { name: string }): Promise<NowcastDone | NowcastFailure>
  /** `POST /v1/flows/{flow}/refresh`: the brick publishes again once it has read. */
  refresh(request: { flow: string }): Promise<NowcastDone | NowcastFailure>
  /**
   * `POST /v1/bricks/{brick}/refresh`: every live flow of the brick, or of
   * its subscription `kind`, whoever subscribed to it. For a mod that changed
   * a source others may follow. A brick that is not enabled answers
   * `unknown_brick`, a kind it does not declare `unknown_kind`; one without
   * flows answers `ok` and reads nothing.
   *
   * With `scope` instead of `kind`, the brick gets the scope and decides
   * which of its flows it touches. The scope must match the schema the brick
   * declares (`invalid_scope` otherwise); a brick that declares none answers
   * `scope_not_accepted`, and `kind` with `scope` `invalid_request`.
   */
  refreshBrick(request: {
    brick: string
    kind?: string
    scope?: Record<string, unknown>
  }): Promise<NowcastDone | NowcastFailure>
  /**
   * Asks the forge brick to read a repository again, for a mod that saw it
   * change (a refresh button, an action of its own). It answers once the
   * request went out, queued behind the refreshes of the agent's forge
   * actions, which this mod sends already. A repository not to be found
   * from `cwd` reads every flow of the types again; a service that takes no
   * scope gets the refresh by kind. A brick that is not enabled answers
   * `unknown_brick`, a malformed request `invalid_request`.
   *
   * @example
   * // The PR and CI shown for a worktree, after a click.
   * await $.nowcast.refreshForge({ cwd: worktree.path, types: ['pr', 'pipelines'] })
   * // An issue of another repository.
   * await $.nowcast.refreshForge({ remote: 'https://github.com/o/r', types: ['issues'], number: 52 })
   */
  refreshForge(request: NowcastForgeRefresh): Promise<NowcastDone | NowcastFailure>
  /**
   * `POST /v1/flows/{flow}/input`: sends `detail` to the flow's brick. The
   * flow's subscription kind must accept an input, and `detail` match the
   * type it accepts (`input_not_accepted`, `invalid_input` otherwise).
   */
  input(request: { flow: string; detail: unknown }): Promise<NowcastDone | NowcastFailure>
  /**
   * `GET /v1/flows/{flow}`, for a flow this process is subscribed to. Most
   * mods read the state instead, which follows the stream.
   */
  last(request: { flow: string }): Promise<NowcastLast | NowcastFailure>
  /**
   * `GET /v1/flows/{flow}?after=<id>`, for a flow this process is subscribed
   * to: the events the service keeps for it (its last 100) newer than
   * `after`, an event `id`. `'0'` reads them all. For a mod that subscribes to
   * a flow another session already follows, and wants what came before. The
   * state is left as is.
   */
  history(request: { flow: string; after: string }): Promise<NowcastHistory | NowcastFailure>
}

export type NowcastSubscribeRequest = {
  /** Names the subscription within the calling mod, `pr`. */
  name: string
  /** The brick's name, `forge`. */
  brick: string
  /** The subscription kind its declaration names, `pr`. */
  kind: string
  /** The subscription's content, checked against the kind's JSON Schema. */
  params: Readonly<Record<string, unknown>>
}

/** A subscription kept, where it stands when `subscribe` answers. */
export type NowcastSubscribed = Pick<NowcastSubscription, 'status' | 'error' | 'flow'> & {
  ok: true
  /** `<mod>/<name>`: the member of the state's `subscriptions`. */
  subscription: string
}

export type NowcastDone = { ok: true }

export type NowcastLast = {
  ok: true
  /** Null when the flow has published nothing yet. */
  last: NowcastEvent | null
}

export type NowcastHistory = {
  ok: true
  /**
   * Some events newer than `after` are gone: past the bound, or from before
   * the service last started.
   */
  gap: boolean
  /** Oldest first. */
  events: NowcastEvent[]
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
   * nowcast mod did not answer), `invalid_name` (a subscription without a
   * name), `service_too_old` (the installed service is older than this mod
   * accepts: nothing was sent).
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
      /**
       * One member per subscription kept, by `<mod>/<name>`; null once
       * unsubscribed.
       */
      subscriptions: StateFamily<NowcastSubscription | null>
      /**
       * The members of `subscriptions` kept: this mod's bookkeeping, to find
       * them again when it reloads.
       */
      subscribed: string[]
      /** One member per flow the session's stream delivered, by flow id. */
      flows: StateFamily<NowcastFlow>
      stream: NowcastStream
      /**
       * The worktrees the session wrote in, the most recent write first, at
       * most 16; one whose root is gone leaves at the next write, or after a
       * command of the agent that may move a branch. Absent
       * until the session writes: a consumer then follows the session's
       * directory. Kept whether or not the claude-code brick is enabled.
       */
      worktrees: NowcastWorktree[]
    }
  }
}
