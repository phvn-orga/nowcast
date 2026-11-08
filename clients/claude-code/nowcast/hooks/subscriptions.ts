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
const DEFINITIVE = new Set(['unknown_brick', 'unknown_kind', 'invalid_params', 'invalid_request'])

/** Delay before trying a refused subscription again, doubled up to the max. */
const FIRST_RETRY_MS = 1_000
const MAX_RETRY_MS = 60_000
/** A subscription active this long and then lost is taken again at once. */
const STABLE_MS = 60_000

type Entry = {
  readonly id: string
  readonly wanted: Wanted
  /** What the state holds for it. */
  value: NowcastSubscription
  /** Attempts refused or lost in a row, for the delay before the next. */
  refused: number
  /** Bumped by each attempt: a timer set before it no longer runs. */
  attempt: number
  /** When it last turned active; null when not known, as after a restore. */
  activeSince: number | null
}

const entries = new Map<string, Entry>()
/** Service subscriptions to delete that the service did not hear about. */
const pending = new Set<string>()
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
    if (held !== undefined && same(held.wanted, wanted)) {
      // Left failed, it is tried again only when asked for again: a mod
      // reloaded once its brick is back gets it back.
      if (held.value.status === 'failed') {
        held.refused = 0
        const refused = await attempt($, held)
        if (refused !== null) await set($, held, { status: 'failed', error: reason(refused), service: null })
      }
      return answer(held)
    }
    if (held !== undefined) await drop($, held)
    const entry: Entry = {
      id,
      wanted,
      value: { ...wanted, status: 'retrying', error: null, flow: null, service: null },
      refused: 0,
      attempt: 0,
      activeSince: null,
    }
    // Kept only once the service answered: a request refused for good must
    // never reach the state, through a `rewrite` or another id's `saveIds`.
    const refused = await attempt($, entry)
    if (refused !== null) return refused
    entries.set(id, entry)
    await saveIds($)
    return answer(entry)
  })
}

/** `$.nowcast.unsubscribe` from `owner`. */
export function unsubscribeFor($: $, owner: string, name: unknown): Promise<NowcastDone | NowcastFailure> {
  if (typeof name !== 'string' || name === '') {
    return Promise.resolve(failure('invalid_name', 'a subscription needs a name, a non-empty string'))
  }
  const id = `${owner}/${name}`
  return serial(id, async () => {
    const held = entries.get(id)
    return held === undefined ? { ok: true as const } : drop($, held)
  })
}

/**
 * The stream opened, after an outage or with a `watch` started again: the
 * service may have restarted and forgotten. What waits is tried now, what
 * the service no longer serves is taken again, and the deletes it missed
 * are sent again.
 */
export async function reconnected($: $): Promise<void> {
  for (const service of pending) {
    pending.delete(service)
    void release($, service)
  }
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
 * new, its state is not. What waits goes on when the stream opens. A
 * subscription a mod took meanwhile is newer than the state's: it stays.
 */
export async function restore($: $): Promise<void> {
  const { value: ids } = await $.state.get(SUBSCRIBED)
  for (const id of ids ?? []) {
    const { value } = await $.state.get({ ...SUBSCRIPTIONS, id })
    if (value === undefined || value === null) continue
    const held = entries.get(id)
    if (held !== undefined) {
      // The service still serves the one replaced: not awaited, the start
      // does not wait for the service.
      if (value.service !== null && value.service !== held.value.service) void release($, value.service)
      continue
    }
    const wanted = { brick: value.brick, kind: value.kind, params: value.params }
    entries.set(id, { id, wanted, value, refused: 0, attempt: 0, activeSince: null })
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
    entry.activeSince = await $.clock.now()
    // The flow first: a consumer reads it once its subscription is active.
    await answered($, created.flow, created.last)
    await set($, entry, { status: 'active', error: null, flow: created.flow, service: created.subscription })
    return null
  }
  if (DEFINITIVE.has(created.code)) return created
  await set($, entry, { status: 'retrying', error: reason(created), service: null })
  later($, entry)
  return null
}

/** Tries `entry` again after the delay its refusals in a row call for. */
function later($: $, entry: Entry): void {
  const delay = Math.min(FIRST_RETRY_MS * 2 ** entry.refused, MAX_RETRY_MS)
  entry.refused++
  const made = ++entry.attempt
  $.clock.after(delay, async () => {
    if (entry.attempt === made) await retry($, entry)
  })
}

/** Tries a waiting subscription again, unless it changed meanwhile. */
function retry($: $, entry: Entry): Promise<void> {
  return serial(entry.id, async () => {
    if (entries.get(entry.id) !== entry || entry.value.status !== 'retrying') return
    const refused = await attempt($, entry)
    if (refused !== null) await set($, entry, { status: 'failed', error: reason(refused), service: null })
  })
}

/**
 * Takes again the subscriptions to `flow` the service no longer serves: at
 * once after a minute active, otherwise after the delay, doubled at each
 * loss. A brick the service gave up on is not relaunched without end.
 */
async function check($: $, flow: string, held: Entry[]): Promise<void> {
  if (await flowKnown($, flow)) return
  const now = await $.clock.now()
  const lost = { code: 'lost', message: 'the service no longer serves it' }
  await Promise.all(
    held.map(entry =>
      serial(entry.id, async () => {
        if (entries.get(entry.id) !== entry || entry.value.status !== 'active') return
        await set($, entry, { status: 'retrying', error: lost, service: null })
        if (now - (entry.activeSince ?? now) < STABLE_MS) return later($, entry)
        entry.refused = 0
        const refused = await attempt($, entry)
        if (refused !== null) await set($, entry, { status: 'failed', error: reason(refused), service: null })
      }),
    ),
  )
}

/**
 * Ends a subscription kept: nothing of it stays here, and the service is
 * told, now or at a later reconnection.
 */
async function drop($: $, entry: Entry): Promise<NowcastDone> {
  entries.delete(entry.id)
  entry.attempt++
  await save($, entry.id, null)
  await saveIds($)
  if (entry.value.service !== null) await release($, entry.value.service)
  return { ok: true }
}

/** Deletes a service subscription, kept to send again if it did not land. */
async function release($: $, service: string): Promise<void> {
  const deleted = await deleteSubscription($, service)
  if (!deleted.ok) pending.add(service)
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
