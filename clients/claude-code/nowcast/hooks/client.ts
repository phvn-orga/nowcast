// The library other mods use: what answers the calls of the `$.nowcast` noun,
// requests to the service over HTTP (subscribe, unsubscribe, refresh, input,
// last value, history). It runs in this mod, so in Claude Code's own process, the parent of
// the stream's `nowcast watch`: the service ties requests and stream to the
// same anchor. Living beside the stream also lets it write a flow's last
// value into the state, and subscribe again once a restarted service forgot.

import type { EngineInterface } from 'claude-code'
import type {
  Nowcast,
  NowcastDone,
  NowcastEvent,
  NowcastFailure,
  NowcastHistory,
  NowcastLast,
  NowcastSubscribeRequest,
  NowcastSubscription,
} from '../types'

type $ = EngineInterface

/** A subscription made through `$.nowcast`, kept to make it again. */
type Kept = { request: NowcastSubscribeRequest; flow: string; current: string }

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

// The subscriptions made through `$.nowcast`, by the id `subscribe` first
// gave, which is the one the mod holds; and the resubscription running.
const kept = new Map<string, Kept>()
let resubscribing: Promise<void> = Promise.resolve()

/** `$.nowcast.subscribe`: the subscription is kept, to make it again. */
export async function subscribeKept($: $, asked: NowcastSubscribeRequest): Promise<NowcastSubscription | NowcastFailure> {
  const request = { brick: asked.brick, kind: asked.kind, params: asked.params }
  const answer = await subscribe($, request)
  if (answer.ok) kept.set(answer.subscription, { request, flow: answer.flow, current: answer.subscription })
  return answer
}

/** `$.nowcast.unsubscribe`, with the id first given or the current one. */
export async function unsubscribe($: $, subscription: string): Promise<NowcastDone | NowcastFailure> {
  const id = kept.get(subscription)?.current ?? subscription
  kept.delete(subscription)
  return expect(await request($, 'DELETE', `/v1/subscriptions/${encodeURIComponent(id)}`), 204, done)
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

/**
 * Subscribes again to every flow of a kept subscription that the service no
 * longer knows, as after a restart, one run at a time. Resolves to the last
 * values the new subscriptions answered.
 */
export function resubscribe($: $): Promise<NowcastEvent[]> {
  const run = resubscribing.then(() => subscribeAgain($))
  resubscribing = run.then(
    () => undefined,
    () => undefined,
  )
  return run.catch(error => {
    log($, `cannot subscribe again: ${message(error)}`)
    return []
  })
}

async function subscribeAgain($: $): Promise<NowcastEvent[]> {
  const lasts: NowcastEvent[] = []
  const byFlow = new Map<string, Array<[string, Kept]>>()
  for (const [first, entry] of kept) {
    byFlow.set(entry.flow, [...(byFlow.get(entry.flow) ?? []), [first, entry]])
  }
  for (const [flow, entries] of byFlow) {
    // `404` is the one answer saying this process is no longer subscribed.
    const answer = await request($, 'GET', `/v1/flows/${encodeURIComponent(flow)}`)
    if ('code' in answer || answer.status !== 404) continue
    for (const [first, entry] of entries) {
      const again = await subscribe($, entry.request)
      if (!again.ok) {
        log($, `cannot subscribe again to ${flow}: ${again.message}`)
      } else if (kept.get(first) === entry) {
        entry.current = again.subscription
        if (again.last !== null) lasts.push(again.last)
      } else {
        // Unsubscribed while this ran: the new subscription is not wanted.
        await request($, 'DELETE', `/v1/subscriptions/${encodeURIComponent(again.subscription)}`)
      }
    }
  }
  return lasts
}

/** `POST /v1/subscriptions`. */
async function subscribe($: $, subscription: NowcastSubscribeRequest): Promise<NowcastSubscription | NowcastFailure> {
  const answer = await request($, 'POST', '/v1/subscriptions', subscription)
  return expect(answer, 201, text => {
    const body = parse(text)
    if (!isObject(body) || typeof body.subscription !== 'string' || typeof body.flow !== 'string') return null
    const last = isEvent(body.last) ? body.last : null
    return { ok: true as const, subscription: body.subscription, flow: body.flow, last }
  })
}

/**
 * One request to the service. One the service refuses for want of a declared
 * session (`428`), as when a mod asks before this mod has declared it,
 * declares the current session and is sent again.
 */
async function request($: $, method: string, path: string, body?: unknown): Promise<Answer | NowcastFailure> {
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

function log($: $, text: string): void {
  $.ui.log(`nowcast: ${text}`, { to: 'debug' })
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
