// A fake world beneath the mod: the session, the environment, a nowcast
// service answering `GET /v1/service` and `PUT /v1/session`, and `nowcast watch` processes the test
// writes to and ends.

import type { HttpResponse, On, ProcessSpawnChunk, ProcessSpawnResult } from 'claude-code'
import { mock } from 'claude-code/testing'
import type { Engine, MockClock, Plugin } from 'claude-code/testing'
import type { NowcastFlow, NowcastStream, NowcastSubscription } from '../types'

export const SOCKET = '/run/user/1000/nowcast/nowcast.sock'

/** One `$.http.fetch` the mod made. */
export type Request = { method: string; url: string; body: string | undefined; socketPath: string | undefined }

/** One process the mod started, which the test drives. */
export class Child {
  readonly argv: readonly string[]
  /** Set when the mod stopped it, rather than it ending on its own. */
  killed = false
  private pending: Array<ProcessSpawnChunk | ProcessSpawnResult> = []
  private wake: (() => void) | undefined
  private ended = false

  constructor(argv: readonly string[]) {
    this.argv = argv
  }

  /** The value given after `--session`. */
  get session(): string | undefined {
    return this.argv[this.argv.indexOf('--session') + 1]
  }

  stdout(text: string): void {
    this.push({ stream: 'stdout', text })
  }

  stderr(text: string): void {
    this.push({ stream: 'stderr', text })
  }

  /** One `watch --json` line on stdout. */
  event(event: string, data: unknown): void {
    this.stdout(`${JSON.stringify({ event, data })}\n`)
  }

  /** `watch --json` saying the stream opened. */
  connected(): void {
    this.stdout('{"status":"connected"}\n')
  }

  /** `watch --json` saying the connection is lost, with the same on stderr. */
  retrying(reason: string): void {
    this.stdout(`${JSON.stringify({ status: 'retrying', reason })}\n`)
    this.stderr(`nowcast: ${reason}; retrying\n`)
  }

  /** The value given after `--last-event-id`, if any. */
  get lastEventId(): string | undefined {
    const at = this.argv.indexOf('--last-event-id')
    return at === -1 ? undefined : this.argv[at + 1]
  }

  exit(code: number | null, signal: string | null = null): void {
    this.push({ code, signal })
  }

  get running(): boolean {
    return !this.ended && !this.killed
  }

  async *run(signal: AbortSignal): AsyncGenerator<ProcessSpawnChunk, ProcessSpawnResult> {
    const aborted = new Promise<void>(resolve => signal.addEventListener('abort', () => resolve()))
    try {
      for (;;) {
        const item = this.pending.shift()
        if (item === undefined) {
          await Promise.race([new Promise<void>(resolve => (this.wake = resolve)), aborted])
          if (signal.aborted) {
            this.killed = true
            return { code: null, signal: 'SIGTERM' }
          }
          continue
        }
        if ('stream' in item) {
          yield item
        } else {
          this.ended = true
          return item
        }
      }
    } finally {
      if (!this.ended) this.killed = true
    }
  }

  private push(item: ProcessSpawnChunk | ProcessSpawnResult): void {
    this.pending.push(item)
    this.wake?.()
  }
}

export type WorldOptions = {
  env?: Record<string, string>
  /** The service's answer to `PUT /v1/session`; a string makes the call fail. */
  answer?: (request: Request) => Partial<HttpResponse> | string | Promise<Partial<HttpResponse> | string>
  /** Makes starting a process fail, as with a command not installed. */
  spawnFails?: boolean
  /**
   * The service's answer to `GET /v1/service`, kept out of `requests`; a
   * string makes the call fail, a promise answers once settled. A service too
   * old to tell its version when absent, so that the mod reads `nowcast
   * --version`.
   */
  service?: Partial<HttpResponse> | string | Promise<Partial<HttpResponse> | string>
  /** What `nowcast --version` prints; the oldest version the mod accepts when absent. */
  version?: string
  /** What `nowcast status --json` prints; units in place when absent. */
  status?: string
  /** What `nowcast --version` waits for before it answers; nothing when absent. */
  versionRead?: Promise<void>
  /** The files that exist, for `$.fs.exists`; none when absent. */
  files?: string[]
  /** A flow whose writes are refused, as an observer of the state may. */
  refuse?: string
}

export type World = {
  clock: MockClock
  requests: Request[]
  children: Child[]
  /** The session id `$.session.id()` answers. */
  session: { id: string }
  /** The last process started. */
  child: () => Child
  /** Every write the nowcast mod made to its state, in order. */
  writes: Write[]
  /** The last value written for a flow, by flow id. */
  flow: (id: string) => NowcastFlow | undefined
  /** The values written for a flow, in order. */
  flowWrites: (id: string) => NowcastFlow[]
  /** The last value written for the stream. */
  stream: () => NowcastStream | undefined
  /** The last value written for a subscription, by `<mod>/<name>`; undefined if none. */
  subscription: (id: string) => NowcastSubscription | null | undefined
  /** The last ids of the subscriptions kept written. */
  subscribed: () => string[] | undefined
  /** What the observer mod logged. */
  observed: string[]
  /** What mods put in the status line, in order. */
  statuses: string[]
  /** The toasts shown, in order. */
  toasts: string[]
}

export type Write = { key: string; id: string | undefined; value: unknown }

/** What a service too old to tell its version answers to `GET /v1/service`. */
const SERVICE = { status: 200, text: '{"path":null}' }

export function world(on: On, options: WorldOptions = {}): World {
  const clock = mock.clock(on, { now: 1_000_000 })
  mock.env(on, options.env ?? { XDG_RUNTIME_DIR: '/run/user/1000' })
  const requests: Request[] = []
  const children: Child[] = []
  const session = { id: 'session-1' }

  on('fs.exists', (_, e) => ({ value: options.files?.includes(e.path) ?? false }))
  on('session.id', () => ({ value: session.id }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('session.end', (_, e) => ({ sessionId: e.sessionId }))
  on('turn.start', (_, e) => ({ turnId: e.turnId }))
  const writes: Write[] = []
  const observed: string[] = []
  const statuses: string[] = []
  const toasts: string[] = []
  on('ui.toast', (_, e) => {
    toasts.push(e.text)
    return { value: undefined }
  })
  on('ui.status', (_, e) => {
    statuses.push(e.text ?? '')
    return { value: undefined }
  })
  on('command.register', (_, e) => ({ value: { command: e.name } }))
  on('ui.log', (_, e) => {
    if (e.text.startsWith('observer ')) observed.push(e.text.slice('observer '.length))
    return { value: undefined }
  })
  on('state.set', { plugin: 'nowcast', key: 'flows' }, (_, e, next) => {
    if (e.id === options.refuse) return { deny: 'refused by an observer' }
    writes.push({ key: e.key, id: e.id, value: e.value })
    return next(e)
  })
  on('state.set', { plugin: 'nowcast', key: 'stream' }, (_, e, next) => {
    writes.push({ key: e.key, id: undefined, value: e.value })
    return next(e)
  })
  on('state.set', { plugin: 'nowcast', key: 'subscriptions' }, (_, e, next) => {
    writes.push({ key: e.key, id: e.id, value: e.value })
    return next(e)
  })
  on('state.set', { plugin: 'nowcast', key: 'subscribed' }, (_, e, next) => {
    writes.push({ key: e.key, id: undefined, value: e.value })
    return next(e)
  })
  on('http.fetch', async (_, e) => {
    const request = {
      method: e.init?.method ?? 'GET',
      url: e.url,
      body: e.init?.body,
      socketPath: e.init?.socketPath,
    }
    const service = request.method === 'GET' && request.url === 'http://nowcast/v1/service'
    if (!service) requests.push(request)
    const answer = (service ? await options.service : await options.answer?.(request)) ?? (service ? SERVICE : {})
    if (typeof answer === 'string') return { deny: answer }
    const status = answer.status ?? 204
    return { value: { status, ok: status < 300, headers: {}, text: '', ...answer } }
  })
  on('process.run', async (_, e) => {
    if (options.spawnFails) return { deny: `${e.argv[0]}: not found` }
    if (e.argv[1] === 'status') {
      const stdout = options.status ?? '{"installed":"0.1.0","running":null,"setup":"current"}'
      return { value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
    }
    await options.versionRead
    const stdout = `nowcast ${options.version ?? '0.1.0-alpha.2'}\n`
    return { value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
  })
  on('process.spawn', async function* (_, e, next) {
    if (options.spawnFails) return { deny: `${e.argv[0]}: not found` }
    const child = new Child(e.argv)
    children.push(child)
    return { value: yield* child.run(next.signal) }
  })

  const flowWrites = (id: string) =>
    writes.filter(w => w.key === 'flows' && w.id === id).map(w => w.value as NowcastFlow)
  return {
    clock,
    requests,
    children,
    session,
    writes,
    observed,
    statuses,
    toasts,
    flowWrites,
    flow: id => flowWrites(id).at(-1),
    stream: () => writes.filter(w => w.key === 'stream').at(-1)?.value as NowcastStream | undefined,
    subscription: id =>
      writes.filter(w => w.key === 'subscriptions' && w.id === id).at(-1)?.value as NowcastSubscription | null | undefined,
    subscribed: () => writes.filter(w => w.key === 'subscribed').at(-1)?.value as string[] | undefined,
    child: () => {
      const last = children.at(-1)
      if (last === undefined) throw new Error('no process started')
      return last
    },
  }
}

/** A CloudEvents event as the service sends it. */
export function update(flow: string, id: string, data: unknown) {
  return {
    specversion: '1.0',
    id,
    source: `nowcast://bricks/${flow.split('/')[0]}`,
    type: 'probe.item/v1',
    subject: flow,
    time: `2026-11-06T10:00:${id.padStart(2, '0')}Z`,
    dataschema: 'nowcast://bricks/probe/types/probe.item/v1',
    data,
  }
}

export const START = { cwd: '/', surface: null, isInteractive: false } as const


/**
 * Another mod, following the nowcast mod's state as any mod would: through a
 * `state.set` hook, and `$.state.get`. It runs apart from the test, so it says
 * what it saw with `$.ui.log`, which the world records.
 */
export const observer: Plugin = {
  name: 'observer',
  register: on => {
    on('state.set', { plugin: 'nowcast', key: 'flows' }, async ($, e, next) => {
      const result = await next(e)
      $.ui.log(`observer set ${e.id} ${JSON.stringify(e.value.last?.data ?? null)}`)
      return result
    })
    on('turn.start', async ($, e, next) => {
      const { value } = await $.state.get({ plugin: 'nowcast', key: 'flows', id: 'probe/x' })
      $.ui.log(`observer get probe/x ${JSON.stringify(value?.last?.data ?? null)}`)
      return next(e)
    })
  },
}

/**
 * Another mod calling `$.nowcast`, as any mod would. The test runs its
 * `call` command (see `call`), and gets the answer back as the command's text.
 */
export const caller: Plugin = {
  name: 'caller',
  register: on => {
    on('command.run', { command: 'call' }, async ($, e) => {
      const { method, request } = JSON.parse(e.args) as { method: string; request: never }
      const answer =
        method === 'subscribe'
          ? await $.nowcast.subscribe(request)
          : method === 'unsubscribe'
            ? await $.nowcast.unsubscribe(request)
            : method === 'refresh'
              ? await $.nowcast.refresh(request)
              : method === 'input'
                ? await $.nowcast.input(request)
                : method === 'history'
                  ? await $.nowcast.history(request)
                  : await $.nowcast.last(request)
      return { text: JSON.stringify(answer) }
    })
  },
}

/**
 * A second mod, which subscribes and unsubscribes through its `call-other`
 * command. A plugin's `register` runs on its own, so it repeats `caller`'s.
 */
export const other: Plugin = {
  name: 'other',
  register: on => {
    on('command.run', { command: 'call-other' }, async ($, e) => {
      const { method, request } = JSON.parse(e.args) as { method: string; request: never }
      const answer = method === 'subscribe' ? await $.nowcast.subscribe(request) : await $.nowcast.unsubscribe(request)
      return { text: JSON.stringify(answer) }
    })
  },
}

/** What `$.nowcast.<method>(request)` answers, called by the `caller` mod. */
export async function call(
  $: Engine,
  method: 'subscribe' | 'unsubscribe' | 'refresh' | 'input' | 'last' | 'history',
  request: unknown,
  command = 'call',
) {
  const args = JSON.stringify({ method, request })
  const { text } = await $.command.run({ command, args, ...COMPOSER })
  return JSON.parse(text ?? 'null') as unknown
}

/** A command typed at the prompt of a headless session. */
export const COMPOSER = { origin: { kind: 'composer' }, presentation: { isFullscreen: false, columns: 80 } } as const
