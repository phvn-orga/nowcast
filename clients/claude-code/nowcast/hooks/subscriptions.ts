// The subscriptions this mod keeps for every mod of the session, each under
// `<mod>/<name>` (`02-clients.md` on the design branch). One lives until its
// mod unsubscribes: what the service refuses for now is tried again, what it
// forgot (a restart, a brick removed) is taken again. Only a request that can
// never succeed is refused, and nothing is kept for it.

import type { EngineInterface } from 'claude-code'
import type { NowcastDone, NowcastFailure, NowcastSubscribed, NowcastSubscription } from '../types'
import { type Wanted, createSubscription, deleteSubscription, flowKnown } from './client'
import { answered, write } from './flows'

type $ = EngineInterface

const SUBSCRIPTIONS = { plugin: 'nowcast', key: 'subscriptions' } as const
const SUBSCRIBED = { plugin: 'nowcast', key: 'subscribed' } as const

/** The failures no later attempt can turn into a success. */
const DEFINITIVE = new Set(['unknown_brick', 'unknown_kind', 'invalid_params'])

/** Delay before trying a refused subscription again, doubled up to the max. */
const FIRST_RETRY_MS = 1_000
const MAX_RETRY_MS = 60_000

type Entry = {
  readonly id: string
  readonly wanted: Wanted
  /** What the state holds for it. */
  value: NowcastSubscription
  /** Attempts refused in a row, for the delay before the next. */
  refused: number
  /** Bumped by each attempt: a timer set before it no longer runs. */
  attempt: number
}

const entries = new Map<string, Entry>()
/** The work on each id, one piece at a time. */
const queues = new Map<string, Promise<unknown>>()

/** `$.nowcast.subscribe` from `owner`, the calling mod. */
export function subscribeFor(
  $: $,
  owner: string,
  asked: { name: unknown } & Wanted,
): Promise<NowcastSubscribed | NowcastFailure> {
  if (typeof asked.name !== 'string' || asked.name === '') {
    return Promise.resolve(failure('invalid_name', 'a subscription needs a name, a non-empty string'))
  }
  const id = `${owner}/${asked.name}`
  const wanted = { brick: asked.brick, kind: asked.kind, params: asked.params }
  return serial(id, async () => {
    const held = entries.get(id)
    if (held !== undefined && same(held.wanted, wanted)) return answer(held)
    if (held !== undefined) await drop($, held)
    const entry: Entry = {
      id,
      wanted,
      value: { ...wanted, status: 'retrying', error: null, flow: null, service: null },
      refused: 0,
      attempt: 0,
    }
    entries.set(id, entry)
    const refused = await attempt($, entry)
    if (refused !== null) {
      entries.delete(id)
      return refused
    }
    await saveIds($)
    return answer(entry)
  })
}

/** `$.nowcast.unsubscribe` from `owner`. */
export function unsubscribeFor($: $, owner: string, name: string): Promise<NowcastDone | NowcastFailure> {
  const id = `${owner}/${name}`
  return serial(id, async () => {
    const held = entries.get(id)
    return held === undefined ? { ok: true as const } : drop($, held)
  })
}

/**
 * The stream opened, after an outage or with a `watch` started again: the
 * service may have restarted and forgotten. What waits is tried now, and
 * what the service no longer serves is taken again.
 */
export async function reconnected($: $): Promise<void> {
  const byFlow = new Map<string, Entry[]>()
  const waiting: Array<Promise<void>> = []
  for (const entry of entries.values()) {
    if (entry.value.status === 'retrying') waiting.push(retry($, entry))
    else if (entry.value.status === 'active' && entry.value.flow !== null) {
      byFlow.set(entry.value.flow, [...(byFlow.get(entry.value.flow) ?? []), entry])
    }
  }
  const checks = [...byFlow].map(([flow, held]) => check($, flow, held))
  await Promise.all([...waiting, ...checks])
}

/** A flow ended or failed: its subscriptions may be gone from the service. */
export async function flowChanged($: $, flow: string): Promise<void> {
  const held = [...entries.values()].filter(e => e.value.status === 'active' && e.value.flow === flow)
  if (held.length > 0) await check($, flow, held)
}

/** Writes every subscription kept again, into a state a `/clear` emptied. */
export async function rewrite($: $): Promise<void> {
  for (const entry of entries.values()) await save($, entry.id, entry.value)
  await saveIds($)
}

/**
 * Takes back what the state holds, once this mod reloaded: its memory is
 * new, its state is not. What waits goes on when the stream opens.
 */
export async function restore($: $): Promise<void> {
  if (entries.size > 0) return
  const { value: ids } = await $.state.get(SUBSCRIBED)
  for (const id of ids ?? []) {
    const { value } = await $.state.get({ ...SUBSCRIPTIONS, id })
    if (value === undefined || value === null) continue
    const wanted = { brick: value.brick, kind: value.kind, params: value.params }
    entries.set(id, { id, wanted, value, refused: 0, attempt: 0 })
  }
}

/**
 * Asks the service once. Answers the definitive refusal, or null once the
 * subscription is saved, active or retrying.
 */
async function attempt($: $, entry: Entry): Promise<NowcastFailure | null> {
  entry.attempt++
  const created = await createSubscription($, entry.wanted)
  if (created.ok) {
    entry.refused = 0
    await set($, entry, { status: 'active', error: null, flow: created.flow, service: created.subscription })
    await answered($, created.flow, created.last)
    return null
  }
  if (DEFINITIVE.has(created.code)) return created
  await set($, entry, { status: 'retrying', error: reason(created), service: null })
  const delay = Math.min(FIRST_RETRY_MS * 2 ** entry.refused, MAX_RETRY_MS)
  entry.refused++
  const made = entry.attempt
  $.clock.after(delay, async () => {
    if (entry.attempt === made) await retry($, entry)
  })
  return null
}

/** Tries a waiting subscription again, unless it changed meanwhile. */
function retry($: $, entry: Entry): Promise<void> {
  return serial(entry.id, async () => {
    if (entries.get(entry.id) !== entry || entry.value.status !== 'retrying') return
    const refused = await attempt($, entry)
    if (refused !== null) await set($, entry, { status: 'failed', error: reason(refused), service: null })
  })
}

/** Takes again the subscriptions to `flow` the service no longer serves. */
async function check($: $, flow: string, held: Entry[]): Promise<void> {
  if (await flowKnown($, flow)) return
  for (const entry of held) {
    if (entries.get(entry.id) !== entry || entry.value.status !== 'active') continue
    entry.refused = 0
    entry.value = { ...entry.value, status: 'retrying', service: null }
  }
  await Promise.all(held.map(entry => retry($, entry)))
}

/** Ends a subscription kept: nothing of it stays, here or in the service. */
async function drop($: $, entry: Entry): Promise<NowcastDone | NowcastFailure> {
  entries.delete(entry.id)
  entry.attempt++
  await save($, entry.id, null)
  await saveIds($)
  return entry.value.service === null ? { ok: true } : deleteSubscription($, entry.value.service)
}

async function set($: $, entry: Entry, change: Partial<NowcastSubscription>): Promise<void> {
  entry.value = { ...entry.value, ...change }
  await save($, entry.id, entry.value)
}

function save($: $, id: string, value: NowcastSubscription | null): Promise<void> {
  return write($, () => $.state.set({ ...SUBSCRIPTIONS, id }, value), `subscription ${id}`)
}

function saveIds($: $): Promise<void> {
  const ids = [...entries.keys()]
  return write($, () => $.state.set(SUBSCRIBED, ids), 'subscriptions')
}

/** Runs `work` after the work queued on `id`, whatever became of it. */
function serial<T>(id: string, work: () => Promise<T>): Promise<T> {
  const run = (queues.get(id) ?? Promise.resolve()).then(work, work)
  const settled = run.then(
    () => undefined,
    () => undefined,
  )
  queues.set(id, settled)
  void settled.then(() => {
    if (queues.get(id) === settled) queues.delete(id)
  })
  return run
}

function answer(entry: Entry): NowcastSubscribed {
  const { status, error, flow } = entry.value
  return { ok: true, subscription: entry.id, status, error, flow }
}

function reason(failed: NowcastFailure): { code: string; message: string } {
  return { code: failed.code, message: failed.message }
}

function failure(code: string, message: string): NowcastFailure {
  return { ok: false, status: null, code, message }
}

/** Whether two requests ask for the same thing, `params` compared as JSON. */
function same(a: Wanted, b: Wanted): boolean {
  return a.brick === b.brick && a.kind === b.kind && canonical(a.params) === canonical(b.params)
}

/** JSON with object keys sorted, so that their order does not matter. */
function canonical(value: unknown): string {
  return JSON.stringify(value, (_, v: unknown) =>
    typeof v === 'object' && v !== null && !Array.isArray(v)
      ? Object.fromEntries(Object.entries(v).sort(([x], [y]) => (x < y ? -1 : x > y ? 1 : 0)))
      : v,
  )
}
