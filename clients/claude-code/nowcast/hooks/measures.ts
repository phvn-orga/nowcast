// What this session measures, sent to the claude-code brick (its
// `05-brique-claude-code.md` on the design branch): the session subscribes to
// its own flow there and sends a `claude-code.measure/v1` input after each
// request to the model and each `session.measure`, grouped when close.
//
// Nothing is sent until the session measures something, and nothing at all
// when the brick is not enabled (`unknown_brick`). The subscription is this
// mod's own, named `session`: the next session's replaces the previous one.
//
// Two values come from the session's transcripts, which no API gives: the
// tokens spent before this mod loaded (a resume, a reload), read once per
// session, and the lifetime of the main thread's prompt cache, read from the
// last response that wrote to it. An unreadable transcript only leaves them
// out: the count starts at the load, and the brick assumes one hour.

import type { EngineInterface, On } from 'claude-code'
import { input } from './client'
import { subscribeFor } from './subscriptions'

type $ = EngineInterface

const BRICK = 'claude-code'
/** Who owns the subscription, and under what name. */
const OWNER = 'nowcast'
const NAME = 'session'
/** Measures closer than this go out as one. */
const GROUP_MS = 500
/** The longest wait for the transcripts' count before a request. */
const SEED_MS = 5_000
/** How much of the transcript's end holds its last responses. */
const TAIL_BYTES = 262_144

export type Tokens = { input: number; output: number; cache_creation: number; cache_read: number }

type Usage = {
  input_tokens?: number
  output_tokens?: number
  cache_creation_input_tokens?: number
  cache_read_input_tokens?: number
}

/** The session being measured, and what it measured. */
type Measured = {
  id: string
  /** The flow, once the subscription is active. */
  flow?: string
  /** The subscription being made: one at a time. */
  subscribing?: Promise<boolean>
  /** The brick is not enabled: nothing is sent for this session. */
  off: boolean
  tokens: Tokens
  /** The transcripts' count, which requests wait for: see `seed`. */
  seeding: Promise<void>
  context?: unknown
  /**
   * The quota windows of the last `session.measure`, sent once: a reading
   * sent again later would pass for a newer one.
   */
  rateLimits?: Array<{ kind: string; percentUsed: number; resetsAt?: string }>
  cost?: number
  effort?: string | number
  lastRequestAt?: string
  cacheTtl?: '5m' | '1h'
  scheduled: boolean
}

let current: Measured | undefined

const zero = (): Tokens => ({ input: 0, output: 0, cache_creation: 0, cache_read: 0 })

/** Runs a measurement; what fails there never reaches the engine's event. */
async function quietly($: $, work: () => Promise<void>): Promise<void> {
  try {
    await work()
  } catch (error) {
    $.ui.log(`nowcast: cannot measure the session: ${message(error)}`, { to: 'debug' })
  }
}

export function registerMeasures(on: On): void {
  on('session.measure', async ($, e, next) => {
    await quietly($, async () => {
      const m = await measured($)
      m.context = e.context
      if (e.changed.includes('rateLimits')) {
        m.rateLimits = e.rateLimits.map(r => ({ kind: r.kind, percentUsed: r.percentUsed, resetsAt: r.resetsAt }))
      }
      if (e.cost !== undefined) m.cost = e.cost.usd
      schedule($, m)
    })
    return next(e)
  })

  // Every request to the model, the subagents' too: its usage adds to the
  // session's tokens. The main thread's also says its effort, and refreshes
  // its cache.
  on('turn.step', async function* ($, e, next) {
    let m: Measured | undefined
    await quietly($, async () => {
      m = await measured($)
      if (e.agentId === undefined) {
        if (e.effort !== undefined) m.effort = e.effort
        m.lastRequestAt = new Date(await $.clock.now()).toISOString()
      }
      await m.seeding
    })
    const result = yield* next(e)
    if (m !== undefined) {
      add(m.tokens, result.usage)
      schedule($, m)
    }
    return result
  })

  on('turn.complete', async ($, e, next) => {
    const done = await next(e)
    if (e.agentId === undefined && current !== undefined) {
      const m = current
      void cacheTtl($, m.id)
        .then(ttl => {
          if (ttl !== undefined && ttl !== m.cacheTtl) {
            m.cacheTtl = ttl
            schedule($, m)
          }
        })
        .catch(() => undefined)
    }
    return done
  })
}

/**
 * The session's measures, started over when the session changed (`/clear`,
 * resume): the previous session's flow is left, the new one's counted from
 * its transcripts.
 */
async function measured($: $): Promise<Measured> {
  const id = await $.session.id()
  if (current?.id === id) return current
  const m: Measured = { id, off: false, tokens: zero(), seeding: Promise.resolve(), scheduled: false }
  current = m
  m.seeding = seed($, m)
  return m
}

/**
 * Adds what the transcripts count to the tokens. Requests wait for it
 * (`turn.step`), so that a response is never both read there and counted
 * by this mod; a transcript that takes longer than `SEED_MS` to read is
 * left out.
 */
async function seed($: $, m: Measured): Promise<void> {
  const found = await Promise.race([
    transcriptTokens($, m.id).catch(() => undefined),
    $.clock.sleep(SEED_MS).then(() => undefined),
  ])
  if (found === undefined) return
  for (const key of Object.keys(found) as Array<keyof Tokens>) m.tokens[key] += found[key]
  schedule($, m)
}

function add(tokens: Tokens, usage: Usage | null | undefined): void {
  if (!usage) return
  tokens.input += usage.input_tokens ?? 0
  tokens.output += usage.output_tokens ?? 0
  tokens.cache_creation += usage.cache_creation_input_tokens ?? 0
  tokens.cache_read += usage.cache_read_input_tokens ?? 0
}

/** Sends the session's measures shortly, once for close changes. */
function schedule($: $, m: Measured): void {
  if (m.off || m.scheduled) return
  m.scheduled = true
  $.clock.after(GROUP_MS, async () => {
    m.scheduled = false
    if (m !== current) return
    await send($, m).catch(error => $.ui.log(`nowcast: cannot send the measures: ${message(error)}`, { to: 'debug' }))
  })
}

/**
 * Subscribes the session to its flow; whether it is active. One waiting is
 * asked again by the next measure, which finds it in place.
 */
async function subscribe($: $, m: Measured): Promise<boolean> {
  const answer = await subscribeFor($, OWNER, { name: NAME, brick: BRICK, kind: 'session', params: { id: m.id } })
  if (!answer.ok) {
    // Not enabled: nothing to send this session. Anything else may pass.
    if (answer.code === 'unknown_brick') m.off = true
    return false
  }
  // A later session's subscription replaces this one: it is not undone here.
  if (m !== current || answer.status !== 'active' || answer.flow === null) return false
  m.flow = answer.flow
  return true
}

async function send($: $, m: Measured): Promise<void> {
  if (m.flow === undefined) {
    m.subscribing ??= subscribe($, m).finally(() => (m.subscribing = undefined))
    if (!(await m.subscribing) || m.flow === undefined) return
  }
  const body = await detail($, m)
  m.rateLimits = undefined
  const sent = await input($, m.flow, body)
  if (!sent.ok) $.ui.log(`nowcast: the brick refused the measures: ${sent.message}`, { to: 'debug' })
}

async function detail($: $, m: Measured): Promise<Record<string, unknown>> {
  const out: Record<string, unknown> = {
    root: await $.session.root(),
    model: await $.session.model(),
    tokens: m.tokens,
  }
  if (m.context !== undefined) out.context = m.context
  if (m.effort !== undefined) out.effort = m.effort
  if (m.lastRequestAt !== undefined) out.last_request_at = m.lastRequestAt
  if (m.cacheTtl !== undefined) out.cache_ttl = m.cacheTtl
  if (m.cost !== undefined) out.cost_usd = m.cost
  if (m.rateLimits !== undefined) {
    out.rate_limits = m.rateLimits.map(r => ({ name: r.kind, percent: r.percentUsed, resets_at: r.resetsAt ?? null }))
  }
  return out
}

/** Where Claude Code keeps the transcripts, one directory per project. */
async function projects($: $): Promise<string | undefined> {
  const config = await $.env.get('CLAUDE_CONFIG_DIR')
  if (config?.startsWith('/')) return `${config}/projects`
  const home = await $.env.get('HOME')
  return home?.startsWith('/') ? `${home}/.claude/projects` : undefined
}

/** Runs `sh -c script` with arguments, and returns its standard output. */
async function shell($: $, script: string, args: string[]): Promise<string> {
  let out = ''
  const child = $.process.spawn({ argv: ['sh', '-c', script, 'sh', ...args] })
  for await (const { stream, text } of child) {
    if (stream === 'stdout') out += text
  }
  return out
}

/**
 * The tokens the session's transcript and its subagents' record, each
 * response once (its last record, the one with the most output), or
 * undefined when none could be read.
 */
export async function transcriptTokens($: $, id: string): Promise<Tokens | undefined> {
  const dir = await projects($)
  if (dir === undefined) return undefined
  const text = await shell($, 'cat "$1"/*/"$2".jsonl "$1"/*/"$2"/subagents/*.jsonl 2>/dev/null', [dir, id])
  return sumUsage(text)
}

export function sumUsage(text: string): Tokens | undefined {
  const byMessage = new Map<string, Usage>()
  for (const line of text.split('\n')) {
    const row = parse(line)
    const usage = row?.message?.usage
    if (row?.type !== 'assistant' || !usage) continue
    const key = row.message?.id ?? row.uuid ?? line
    const held = byMessage.get(key)
    if (!held || (usage.output_tokens ?? 0) >= (held.output_tokens ?? 0)) byMessage.set(key, usage)
  }
  if (byMessage.size === 0) return undefined
  const tokens = zero()
  for (const usage of byMessage.values()) add(tokens, usage)
  return tokens
}

/** The lifetime of the main thread's cache, from its last response that wrote to it. */
async function cacheTtl($: $, id: string): Promise<'5m' | '1h' | undefined> {
  const dir = await projects($)
  if (dir === undefined) return undefined
  const text = await shell($, 'tail -c "$3" "$1"/*/"$2".jsonl 2>/dev/null', [dir, id, String(TAIL_BYTES)])
  return lastTtl(text)
}

export function lastTtl(text: string): '5m' | '1h' | undefined {
  const lines = text.split('\n')
  for (let i = lines.length - 1; i >= 0; i--) {
    const row = parse(lines[i] ?? '')
    const written = row?.type === 'assistant' ? row.message?.usage?.cache_creation : undefined
    if (!written) continue
    if ((written.ephemeral_1h_input_tokens ?? 0) > 0) return '1h'
    if ((written.ephemeral_5m_input_tokens ?? 0) > 0) return '5m'
  }
  return undefined
}

type Row = {
  type?: string
  uuid?: string
  message?: {
    id?: string
    usage?: Usage & { cache_creation?: { ephemeral_1h_input_tokens?: number; ephemeral_5m_input_tokens?: number } }
  }
}

function parse(line: string): Row | undefined {
  if (!line.startsWith('{')) return undefined
  try {
    return JSON.parse(line) as Row
  } catch {
    return undefined
  }
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
