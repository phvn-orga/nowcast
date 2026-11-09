// The nowcast mod: one per Claude Code session. It declares the session to
// the service, holds the session's one event stream through `nowcast watch`,
// keeps the subscriptions every mod takes, and writes each subscription and
// each flow it receives into its state for other mods to read. Requests go
// from this process to the service: all mods run in it, so the service ties
// them to the same session.

import type { EngineInterface, Register } from 'claude-code'
import type { NowcastStream } from '../types'
import { CLI, STUB, checkService, history, input, last, refresh, refreshBrick, socketPath } from './client'
import { apply, forgetFlows, offer, write } from './flows'
import { registerMeasures } from './measures'
import { warnSetup } from './setup'
import { Lines, type Status, parseLine } from './stream'
import { flowChanged, reconnected, restore, rewrite, subscribeFor, unsubscribeFor } from './subscriptions'

const STREAM = { plugin: 'nowcast', key: 'stream' } as const

/** Why the stream stops when the command cannot start and is not on `PATH`. */
export const NOT_ON_PATH = `the ${CLI} command is not on PATH; install it, see https://github.com/phvn-orga/nowcast#install`

/** Delay before starting `watch` again after it exits, doubled up to the max. */
const FIRST_RESTART_MS = 1_000
const MAX_RESTART_MS = 60_000
/** A `watch` that ran this long starts again after the first delay. */
const STABLE_MS = 60_000
/** After a `/clear` or a resume, how often to look for the new session id. */
const NEW_ID_POLL_MS = 50
const NEW_ID_ATTEMPTS = 100
/** What `watch --last-event-id` takes: a value an HTTP header can carry. */
const HEADER_VALUE = /^[^\x00-\x08\x0a-\x1f\x7f]+$/

type $ = EngineInterface

/** The running `watch`, for the session it declares. */
type Watch = { session: string; stop: () => void }

// The running `watch`, and the session changes applied one at a time, in
// order. Both start over when the mod reloads, as `session.start` does.
let watch: Watch | undefined
let queue: Promise<void> = Promise.resolve()

// What this mod last wrote about the stream: see `flows.ts`.
let stream: NowcastStream = { session: null, status: 'starting', error: null }

export const register: Register = on => {
  // The library other mods use: `$.nowcast`, answered below.
  on('engine.create', async (_, e, next) => ({ ...(await next(e)), nowcast: STUB }))
  // Subscriptions belong to the calling mod, which the engine names.
  on('nowcast.subscribe', async ($, e, next) => ({ value: await subscribeFor($, next.origin.plugin, e) }))
  on('nowcast.unsubscribe', async ($, e, next) => ({ value: await unsubscribeFor($, next.origin.plugin, e.name) }))
  on('nowcast.refresh', async ($, e) => ({ value: await refresh($, e.flow) }))
  on('nowcast.refreshBrick', async ($, e) => ({ value: await refreshBrick($, e.brick, e.kind) }))
  on('nowcast.input', async ($, e) => ({ value: await input($, e.flow, e.detail) }))
  on('nowcast.last', async ($, e) => {
    const value = await last($, e.flow)
    if (value.ok && value.last !== null) await offer($, value.last)
    return { value }
  })
  on('nowcast.history', async ($, e) => ({ value: await history($, e.flow, e.after) }))

  // Once per load of the mod: the session's start, or a reload. The start
  // does not wait for the service: `stream` says when the session is declared.
  // A reload finds the subscriptions it kept in its state, before the mods
  // that start with it subscribe again under the same names. It also says
  // when the service needs `nowcast setup`.
  on('session.start', async ($, e, next) => {
    await restore($).catch(error => $.ui.log(`nowcast: cannot restore: ${message(error)}`, { to: 'debug' }))
    const started = await next(e)
    void follow($, await $.session.id())
    void warnSetup($).catch(error => $.ui.log(`nowcast: cannot read the setup: ${message(error)}`, { to: 'debug' }))
    return started
  })

  // A `/clear` or a resume ends the session and the process goes on under a
  // new id, with no `session.start`: the new id shows once the end is done.
  on('session.end', async ($, e, next) => {
    const ended = await next(e)
    if (e.reason === 'clear' || e.reason === 'resume') awaitNewId($, e.sessionId, 0)
    return ended
  })

  // The session's measures, for the claude-code brick when it is enabled.
  registerMeasures(on)

  // A backstop: a turn always runs under the session the service knows.
  on('turn.start', async ($, e, next) => {
    if (e.agentId === undefined) {
      const session = await $.session.id()
      if (watch?.session !== session) void follow($, session)
    }
    return next(e)
  })
}

/** Follows the session that replaces `ended`, as soon as its id shows. */
function awaitNewId($: $, ended: string, attempt: number): void {
  $.clock.after(NEW_ID_POLL_MS, async () => {
    const session = await $.session.id()
    if (session !== ended) await follow($, session)
    else if (attempt < NEW_ID_ATTEMPTS) awaitNewId($, ended, attempt + 1)
  })
}

/** Runs `watch` for `session`, which `start` declares, unless it already runs. */
function follow($: $, session: string): Promise<void> {
  queue = queue
    .then(async () => {
      if (watch?.session === session) return
      watch?.stop()
      watch = undefined
      // A new session starts with an empty state: the subscriptions are
      // written again, and the new `watch` sends the last value of every
      // flow again, each written anew.
      forgetFlows()
      await rewrite($)
      watch = start($, session)
    })
    .catch(error => $.ui.log(`nowcast: cannot follow session ${session}: ${message(error)}`, { to: 'debug' }))
  return queue
}

/**
 * `PUT /v1/session`. A service that does not answer is not an error here:
 * `watch` declares the session again each time it connects.
 */
async function declare($: $, session: string): Promise<void> {
  const socket = await socketPath($)
  if (socket === undefined) {
    return setStream($, { session, error: 'neither NOWCAST_SOCKET nor an absolute XDG_RUNTIME_DIR is set' })
  }
  try {
    const answer = await $.http.fetch('http://nowcast/v1/session', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ id: session }),
      socketPath: socket,
    })
    await setStream(
      $,
      answer.status === 204
        ? { session, status: 'up', error: null }
        : { session, error: `PUT /v1/session answered ${answer.status}: ${answer.text}` },
    )
  } catch (error) {
    await setStream($, {
      session,
      status: stream.status === 'up' ? 'down' : stream.status,
      error: `cannot reach the service at ${socket}: ${message(error)}`,
    })
  }
}

/**
 * Runs `watch` for `session` until stopped, starting it again if it exits.
 * Each start reads the service's version first; the session is declared
 * once the version is accepted.
 * The `watch` started again resumes after the last update the previous one
 * printed, rather than sending every last value again. A new session gets a
 * new `start`, with no update seen: its state is empty and needs them all.
 */
function start($: $, session: string): Watch {
  let child: ReturnType<$['process']['spawn']> | undefined
  let stopped = false
  let lastEventId: string | undefined
  let declared = false
  void (async () => {
    let delay = FIRST_RESTART_MS
    for (;;) {
      const startedAt = await $.clock.now()
      if (stopped) return
      // Read before anything goes to the service, and again before each
      // start: an upgrade during the session is taken.
      let why = await checkService($)
      // Stopped while the version was read: no `watch` for the session left behind.
      if (stopped) return
      if (why === null && !declared) {
        await declare($, session)
        declared = true
        if (stopped) return
      }
      if (why === null) {
        const resume = lastEventId === undefined ? [] : ['--last-event-id', lastEventId]
        child = $.process.spawn({ argv: [CLI, '--json', '--session', session, 'watch', ...resume] })
        why = await run($, child, () => stopped, id => (lastEventId = id))
      }
      if (stopped) return
      await setStream($, { session, status: 'stopped', error: why })
      $.ui.log(`nowcast: ${why}`, { to: 'debug' })
      if ((await $.clock.now()) - startedAt >= STABLE_MS) delay = FIRST_RESTART_MS
      await $.clock.sleep(delay)
      delay = Math.min(delay * 2, MAX_RESTART_MS)
    }
  })().catch(() => {
    // The mod is unloading: its calls on `$` are refused from now on.
  })
  return {
    session,
    stop: () => {
      stopped = true
      void child?.return({ code: null, signal: null })
    },
  }
}

/**
 * Reads one `watch` until it ends, and says why it ended. Each update's id
 * goes to `seen`. Once `stopped`, what the stopped `watch` still prints
 * belongs to the session left behind: it is dropped.
 */
async function run(
  $: $,
  child: ReturnType<$['process']['spawn']>,
  stopped: () => boolean,
  seen: (id: string) => void,
): Promise<string> {
  const stdout = new Lines()
  const stderr = new Lines()
  let lastError = ''
  try {
    for await (const { stream, text } of child) {
      if (stopped()) break
      if (stream === 'stdout') {
        for (const line of stdout.push(text)) {
          if (stopped()) break
          const parsed = parseLine(line)
          if (parsed === null) continue
          if ('status' in parsed) {
            if (parsed.status === 'connected') lastError = ''
            await onStatus($, parsed)
          } else {
            // `watch` refuses an id it could not send back as a header.
            if (parsed.event === 'update' && HEADER_VALUE.test(parsed.update.id)) seen(parsed.update.id)
            await apply($, parsed)
            // The service may have dropped the subscriptions of a flow that
            // ends or fails.
            if (parsed.event !== 'update') void flowChanged($, parsed.flow).catch(() => undefined)
          }
        }
      } else {
        for (const line of stderr.push(text)) {
          if (stopped()) break
          lastError = errorText(line)
          $.ui.log(line, { to: 'debug' })
        }
      }
    }
    const { code, signal } = await child.result
    const status = signal ?? `status ${code}`
    return lastError ? `${CLI} watch exited (${status}): ${lastError}` : `${CLI} watch exited (${status})`
  } catch (error) {
    return (await onPath($)) ? `cannot run ${CLI}: ${message(error)}` : NOT_ON_PATH
  }
}

/**
 * Whether a file named like the command sits in a directory of `PATH`: when
 * it cannot start, the likely cause is that it is not installed, or not
 * where the session looks. Relative entries are skipped.
 */
async function onPath($: $): Promise<boolean> {
  const dirs = ((await $.env.get('PATH')) ?? '').split(':').filter(dir => dir.startsWith('/'))
  for (const dir of dirs) {
    if (await $.fs.exists(`${dir.replace(/\/+$/, '')}/${CLI}`).catch(() => false)) return true
  }
  return false
}

async function onStatus($: $, status: Status): Promise<void> {
  if (status.status === 'connected') {
    await setStream($, { status: 'up', error: null })
    // Each time the stream opens, after an outage or in a `watch` started
    // again: the service may have restarted and forgotten the subscriptions.
    void reconnected($).catch(error => $.ui.log(`nowcast: cannot subscribe again: ${message(error)}`, { to: 'debug' }))
  } else {
    await setStream($, { status: 'down', error: status.reason })
  }
}

async function setStream($: $, change: Partial<NowcastStream>): Promise<void> {
  stream = { ...stream, ...change }
  const value = stream
  await write($, () => $.state.set(STREAM, value), 'stream')
}

/** A fatal error's message from `--json` output, or the line as it is. */
function errorText(line: string): string {
  try {
    const parsed = JSON.parse(line) as { error?: { message?: unknown } }
    if (typeof parsed.error?.message === 'string') return parsed.error.message
  } catch {
    // Not JSON: a line meant for people.
  }
  return line
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
