// Requests to the service over HTTP, behind the `$.nowcast` noun: subscribe,
// unsubscribe, refresh, input, last value and history. They run in this mod,
// so in Claude Code's own process, the parent of the stream's `nowcast
// watch`: the service ties requests and stream to the same anchor. The
// subscriptions themselves are kept by `subscriptions.ts`.

import type { EngineInterface } from 'claude-code'
import { OLDEST_SERVICE, isOlder, isVersion, printedVersion } from './version'
import type {
  Nowcast,
  NowcastDone,
  NowcastEvent,
  NowcastFailure,
  NowcastHistory,
  NowcastLast,
  NowcastSubscribeRequest,
} from '../types'

type $ = EngineInterface

/** The command line tool, looked up in the session's `PATH`. */
export const CLI = 'nowcast'

/** What `POST /v1/subscriptions` asks for. */
export type Wanted = Omit<NowcastSubscribeRequest, 'name'>

/** What `POST /v1/subscriptions` answers. */
export type Created = { ok: true; subscription: string; flow: string; last: NowcastEvent | null }

/** An HTTP answer from the service. */
type Answer = { status: number; text: string }

const unavailable = async (): Promise<NowcastFailure> => ({
  ok: false,
  status: null,
  code: 'unavailable',
  message: 'the nowcast mod did not answer',
})

/**
 * The noun as `engine.create` adds it. This mod's hooks answer its calls, so
 * a call gets this only when they fail.
 */
export const STUB: Nowcast = {
  subscribe: unavailable,
  unsubscribe: unavailable,
  refresh: unavailable,
  input: unavailable,
  last: unavailable,
  history: unavailable,
}

const done = () => ({ ok: true as const })

/** `POST /v1/subscriptions`. */
export async function createSubscription($: $, wanted: Wanted): Promise<Created | NowcastFailure> {
  const answer = await request($, 'POST', '/v1/subscriptions', wanted)
  return expect(answer, 201, text => {
    const body = parse(text)
    if (!isObject(body) || typeof body.subscription !== 'string' || typeof body.flow !== 'string') return null
    const last = isEvent(body.last) ? body.last : null
    return { ok: true as const, subscription: body.subscription, flow: body.flow, last }
  })
}

/**
 * `DELETE /v1/subscriptions/{subscription}`, with the service's id. One the
 * service no longer knows (`404`) is ended all the same.
 */
export async function deleteSubscription($: $, subscription: string): Promise<NowcastDone | NowcastFailure> {
  const answer = await request($, 'DELETE', `/v1/subscriptions/${encodeURIComponent(subscription)}`)
  if (!('code' in answer) && answer.status === 404) return done()
  return expect(answer, 204, done)
}

/**
 * Whether this process is still subscribed to `flow`: false only on the
 * `404` that says so, true whenever the service cannot tell.
 */
export async function flowKnown($: $, flow: string): Promise<boolean> {
  const answer = await request($, 'GET', `/v1/flows/${encodeURIComponent(flow)}`)
  return 'code' in answer || answer.status !== 404
}

/** `$.nowcast.refresh`. */
export async function refresh($: $, flow: string): Promise<NowcastDone | NowcastFailure> {
  return expect(await request($, 'POST', `/v1/flows/${encodeURIComponent(flow)}/refresh`), 202, done)
}

/** `$.nowcast.input`. */
export async function input($: $, flow: string, detail: unknown): Promise<NowcastDone | NowcastFailure> {
  return expect(await request($, 'POST', `/v1/flows/${encodeURIComponent(flow)}/input`, detail), 202, done)
}

/** `$.nowcast.last`. */
export async function last($: $, flow: string): Promise<NowcastLast | NowcastFailure> {
  const answer = await request($, 'GET', `/v1/flows/${encodeURIComponent(flow)}`)
  if (!('code' in answer) && answer.status === 204) return { ok: true, last: null }
  return expect(answer, 200, text => {
    const event = parse(text)
    return isEvent(event) ? { ok: true as const, last: event } : null
  })
}

/** `$.nowcast.history`. */
export async function history($: $, flow: string, after: string): Promise<NowcastHistory | NowcastFailure> {
  const path = `/v1/flows/${encodeURIComponent(flow)}?after=${encodeURIComponent(after)}`
  return expect(await request($, 'GET', path), 200, text => {
    const body = parse(text)
    if (!isObject(body) || typeof body.gap !== 'boolean') return null
    const events = body.events
    if (!Array.isArray(events) || !events.every(isEvent)) return null
    return { ok: true as const, gap: body.gap, events }
  })
}

// Whether the installed service is too old, as last read: every request
// waits for it.
let verdict: Promise<string | null> | undefined

/**
 * Reads the service's version again: why the service is too old for this
 * mod, or null. Until the next reading, every request goes by this one.
 */
export function checkService($: $): Promise<string | null> {
  verdict = tooOld($)
  return verdict
}

/**
 * The running service's version, from `GET /v1/service`. A service that does
 * not tell it, older than the field or out of reach, falls back on the
 * installed binary's, `nowcast --version`. A version that cannot be read is
 * let through: what fails next says why.
 */
async function tooOld($: $): Promise<string | null> {
  const running = await runningVersion($)
  if (running !== null) {
    if (!isOlder(running, OLDEST_SERVICE)) return null
    return `the running ${CLI} service is ${running}, older than ${OLDEST_SERVICE}, the oldest this mod accepts; upgrade it and restart the service`
  }
  const ran = await $.process.run([CLI, '--version'], { timeoutMs: 5_000 }).catch(() => null)
  const version = ran?.exitCode === 0 ? printedVersion(ran.stdout) : null
  if (version === null || !isOlder(version, OLDEST_SERVICE)) return null
  return `${CLI} ${version} is older than ${OLDEST_SERVICE}, the oldest this mod accepts; upgrade it`
}

/** The `version` `GET /v1/service` answers, or null when it tells none. */
async function runningVersion($: $): Promise<string | null> {
  const answer = await send($, 'GET', '/v1/service', undefined)
  if ('code' in answer || answer.status !== 200) return null
  const body = parse(answer.text)
  return isObject(body) && typeof body.version === 'string' && isVersion(body.version) ? body.version : null
}

/**
 * One request to the service, unless the service is too old. One the
 * service refuses for want of a declared session (`428`), as when a mod
 * asks before this mod has declared it, declares the current session and is
 * sent again.
 */
async function request($: $, method: string, path: string, body?: unknown): Promise<Answer | NowcastFailure> {
  const why = await (verdict ?? checkService($))
  if (why !== null) return { ok: false, status: null, code: 'service_too_old', message: why }
  const answer = await send($, method, path, body)
  if ('code' in answer || answer.status !== 428) return answer
  const declared = await send($, 'PUT', '/v1/session', { id: await $.session.id() })
  if ('code' in declared) return declared
  if (declared.status !== 204) return failure(declared)
  return send($, method, path, body)
}

async function send($: $, method: string, path: string, body: unknown): Promise<Answer | NowcastFailure> {
  const socket = await socketPath($)
  if (socket === undefined) {
    return { ok: false, status: null, code: 'no_socket', message: NO_SOCKET }
  }
  try {
    const answer = await $.http.fetch(`http://nowcast${path}`, {
      method,
      ...(body === undefined ? {} : { headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }),
      socketPath: socket,
    })
    return { status: answer.status, text: answer.text }
  } catch (error) {
    return {
      ok: false,
      status: null,
      code: 'unreachable',
      message: `cannot reach the service at ${socket}: ${message(error)}`,
    }
  }
}

/**
 * The answer read by `read` when its status is `status`, else the failure it
 * says. `read` returns null for a body it cannot read.
 */
function expect<T>(
  answer: Answer | NowcastFailure,
  status: number,
  read: (text: string) => T | null,
): T | NowcastFailure {
  if ('code' in answer) return answer
  if (answer.status !== status) return failure(answer)
  return (
    read(answer.text) ?? {
      ok: false,
      status: answer.status,
      code: 'bad_answer',
      message: `unexpected answer: ${answer.text}`,
    }
  )
}

/** A service error, `{"error": {"code", "message"}}` (contract section 3). */
function failure(answer: Answer): NowcastFailure {
  const body = parse(answer.text)
  const error = isObject(body) && isObject(body.error) ? body.error : {}
  return {
    ok: false,
    status: answer.status,
    code: typeof error.code === 'string' ? error.code : 'unexpected_status',
    message: typeof error.message === 'string' ? error.message : `the service answered ${answer.status}`,
  }
}

const NO_SOCKET = 'neither NOWCAST_SOCKET nor an absolute XDG_RUNTIME_DIR is set'

/** Where the service listens, as the `nowcast` command finds it. */
export async function socketPath($: $): Promise<string | undefined> {
  const explicit = await $.env.get('NOWCAST_SOCKET')
  if (explicit) return explicit
  const runtime = await $.env.get('XDG_RUNTIME_DIR')
  return runtime?.startsWith('/') ? `${runtime}/nowcast/nowcast.sock` : undefined
}

function isEvent(value: unknown): value is NowcastEvent {
  return isObject(value) && typeof value.id === 'string' && typeof value.subject === 'string'
}

function parse(text: string): unknown {
  try {
    return JSON.parse(text)
  } catch {
    return undefined
  }
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
