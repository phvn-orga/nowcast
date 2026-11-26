// The session's measures, sent to the claude-code brick on the session's own
// flow (the brick's `claude-code.measure/v1` input).

import type { HttpResponse, On } from 'claude-code'
import { type Engine, type Plugin, expect, test } from 'claude-code/testing'
import { lastCachedAt, lastTtl, sumUsage } from '../hooks/measures'
import { COMPOSER, type Request, START, type World, world } from './world'

const FLOW = 'claude-code/session/session-1'
const INPUT = `/v1/flows/${encodeURIComponent(FLOW)}/input`

const assistant = (id: string, usage: Record<string, unknown>) =>
  JSON.stringify({ type: 'assistant', message: { id, usage } })

const USAGE = { input_tokens: 10, output_tokens: 20, cache_creation_input_tokens: 30, cache_read_input_tokens: 40 }
/** What a failed call reports: nothing counted. */
const ZERO = { input_tokens: 0, output_tokens: 0, cache_creation_input_tokens: 0, cache_read_input_tokens: 0 }
/** `USAGE` counted `n` times, as the measures send it. */
const times = (n: number) => ({ input: 10 * n, output: 20 * n, cache_creation: 30 * n, cache_read: 40 * n })

/**
 * The engine's answers the measures read, beneath the mod. Each request
 * answers `usage`; null makes the main thread's fail with no response.
 * `withEffort` false is a model that takes no effort.
 */
function engine(on: On, usage: typeof USAGE | null = USAGE, withEffort = true) {
  on('session.root', () => ({ value: '/project' }))
  on('session.model', () => ({ value: 'claude-opus-5-5' }))
  on('session.measure', (_, e) => ({ changed: e.changed }))
  on('session.usage', () => ({ value: { startedAt: 0, context: { window: 200000 }, rateLimits: [] } }))
  on('turn.step', async function* (_, e) {
    return {
      turnId: e.turnId,
      index: e.index,
      answer: '',
      toolUses: [],
      stopReason: usage === null ? null : 'end_turn',
      usage,
    } as never
  })
  // A fork runs at the effort it names, else at the session's; a model that
  // takes none is sent none.
  on('model.fork', (_, e) => ({
    value:
      usage === null
        ? ({ isAnswered: false, reason: 'api-error', status: 529, error: 'overloaded', usage: ZERO } as never)
        : { isAnswered: true, text: '', usage, effort: withEffort ? (e.effort ?? 'high') : undefined },
  }))
  on('model.complete', () => ({ value: { isAnswered: true, text: '', usage: USAGE } }))
  on('session.compact', (_, e) => ({ messages: e.messages, usage: USAGE }))
}

/** One request of the main thread, at `effort`: null on a model that takes none. */
async function step($: Engine, effort: 'high' | null = 'high') {
  for await (const _ of $.turn.step({ turnId: 't', index: 0, model: 'm', effort: effort ?? undefined } as never)) {
    // The step's pieces: none here.
  }
}

/**
 * Another mod, calling the model as a mod does: its `fork` and `complete`
 * commands take the request as their arguments.
 */
const caller: Plugin = {
  name: 'model-caller',
  register: on => {
    on('command.run', { command: 'fork' }, async ($, e) => {
      await $.model.fork(JSON.parse(e.args) as never)
      return { text: '' }
    })
    on('command.run', { command: 'complete' }, async ($, e) => {
      await $.model.complete(JSON.parse(e.args) as never)
      return { text: '' }
    })
  },
}
const plugins = { plugins: [caller] }

/** The `caller` mod calls `$.model.<command>(request)`. */
async function model($: Engine, command: 'fork' | 'complete', request: unknown) {
  await $.command.run({ command, args: JSON.stringify(request), ...COMPOSER })
}

/** The measures sent last, once the grouping delay passed. */
async function sent(w: World) {
  await w.clock.advance(500)
  await w.clock.settle()
  return inputs(w.requests).at(-1)
}

const at = (ms: number) => new Date(ms).toISOString()

/** A service where the brick is enabled, or not. */
function service(enabled: boolean) {
  return (request: Request): Partial<HttpResponse> => {
    const path = request.url.replace('http://nowcast', '')
    if (request.method === 'POST' && path === '/v1/subscriptions') {
      return enabled
        ? { status: 201, text: JSON.stringify({ subscription: 's-1', flow: FLOW, last: null }) }
        : { status: 404, text: '{"error":{"code":"unknown_brick","message":"no brick"}}' }
    }
    if (request.method === 'POST' && path === INPUT) return { status: 202 }
    return {}
  }
}

const MEASURE = {
  context: { tokens: 1000, window: 200000, percent: 1 },
  rateLimits: [{ kind: 'five_hour', percentUsed: 23.5, resetsAt: '2026-11-07T18:00:00Z' }],
  cost: { usd: 1.5 },
  changed: ['context', 'rateLimits', 'cost'],
} as const

const inputs = (requests: Request[]) =>
  requests.filter(r => r.url.endsWith(INPUT)).map(r => JSON.parse(r.body ?? 'null') as Record<string, unknown>)

test('a measure subscribes the session to its flow, then sends what it measured', async ($, on) => {
  const w = world(on, { answer: service(true) })
  engine(on)
  await $.session.start(START)
  await w.clock.settle()

  await $.session.measure(MEASURE as never)
  for await (const _ of $.turn.step({ turnId: 't', index: 0, model: 'm', effort: 'high' } as never)) {
    // The step's pieces: none here.
  }
  await w.clock.advance(500)
  await w.clock.settle()

  const subscribe = w.requests.find(r => r.url.endsWith('/v1/subscriptions'))
  expect(JSON.parse(subscribe?.body ?? 'null')).toEqual({ brick: 'claude-code', kind: 'session', params: { id: 'session-1' } })
  expect(inputs(w.requests)).toEqual([
    {
      root: '/project',
      model: 'claude-opus-5-5',
      tokens: { input: 10, output: 20, cache_creation: 30, cache_read: 40 },
      side_tokens: times(0),
      side_since: new Date(1_000_000).toISOString(),
      worktrees: [],
      context: MEASURE.context,
      effort: 'high',
      last_request_at: new Date(1_000_000).toISOString(),
      cost_usd: 1.5,
      rate_limits: [{ name: 'five_hour', percent: 23.5, resets_at: '2026-11-07T18:00:00Z' }],
    },
  ])
})

test("a mod's fork counts in the tokens, and refreshes the main thread's cache", plugins, async ($, on) => {
  const w = world(on, { answer: service(true) })
  engine(on)
  await $.session.start(START)
  await w.clock.settle()
  await step($)

  await w.clock.advance(60_000)
  await model($, 'fork', { prompt: 'Keep warm.' })
  const measures = await sent(w)
  expect(measures?.tokens).toEqual(times(2))
  // The transcripts do not hold the fork: it counts apart as well.
  expect(measures?.side_tokens).toEqual(times(1))
  expect(measures?.last_request_at).toBe(at(1_060_000))
})

// Each move of the clock takes about a second.
test("a fork at another effort than the main thread's leaves the cache's time", { ...plugins, timeoutMs: 10_000 }, async ($, on) => {
  const w = world(on, { answer: service(true) })
  engine(on)
  await $.session.start(START)
  await w.clock.settle()
  await step($)

  // It may have written a cache entry of its own.
  await w.clock.advance(60_000)
  await model($, 'fork', { prompt: 'Shorter.', effort: 'low' })
  const measures = await sent(w)
  expect(measures?.tokens).toEqual(times(2))
  expect(measures?.last_request_at).toBe(at(1_000_000))

  // Named, the main thread's own effort makes the same request.
  await model($, 'fork', { prompt: 'Again.', effort: 'high' })
  expect((await sent(w))?.last_request_at).toBe(at(1_060_500))
})

test("a request that failed leaves the cache's time as it was", plugins, async ($, on) => {
  const w = world(on, { answer: service(true) })
  engine(on, null)
  await $.session.start(START)
  await w.clock.settle()

  await step($)
  await model($, 'fork', { prompt: 'Keep warm.' })
  const measures = await sent(w)
  expect(measures?.tokens).toEqual(times(0))
  expect(measures?.last_request_at).toBe(undefined)
})

test(
  'on a model that takes no effort, a fork naming one is the main thread\'s request',
  { ...plugins, timeoutMs: 10_000 },
  async ($, on) => {
    const w = world(on, { answer: service(true) })
    engine(on, USAGE, false)
    await $.session.start(START)
    await w.clock.settle()
    // At `high`, then on a model without effort (`/model`).
    await step($, 'high')
    await step($, null)

    await w.clock.advance(60_000)
    await model($, 'fork', { prompt: 'Shorter.', effort: 'high' })
    expect((await sent(w))?.last_request_at).toBe(at(1_060_000))
  },
)

test("a mod's fork does not wait for the transcripts' count", plugins, async ($, on) => {
  const w = world(on, { answer: service(true), env: { XDG_RUNTIME_DIR: '/run/user/1000', HOME: '/home/u' } })
  engine(on)
  await $.session.start(START)
  await w.clock.settle()

  // The transcripts are still being read: the fork goes on all the same.
  await model($, 'fork', { prompt: 'Keep warm.' })
  await w.clock.settle()
  const cat = w.children.find(c => c.argv[0] === 'sh')
  cat?.stdout(`${assistant('a', { input_tokens: 1, output_tokens: 2, cache_creation_input_tokens: 3, cache_read_input_tokens: 4 })}\n`)
  cat?.exit(0)
  expect((await sent(w))?.tokens).toEqual({ input: 11, output: 22, cache_creation: 33, cache_read: 44 })
})

test("a mod's completion and a compaction's summary count in the tokens, and leave the cache's time", plugins, async ($, on) => {
  const w = world(on, { answer: service(true) })
  engine(on)
  await $.session.start(START)
  await w.clock.settle()
  await step($)

  await w.clock.advance(60_000)
  await model($, 'complete', { model: 'haiku', prompt: 'One word.' })
  await $.session.compact({ trigger: 'manual', messages: [{ role: 'user', text: 'Hi.', toolUses: [] }] } as never)
  const measures = await sent(w)
  expect(measures?.tokens).toEqual(times(3))
  expect(measures?.side_tokens).toEqual(times(2))
  expect(measures?.last_request_at).toBe(at(1_000_000))
})

test('a quota reading goes out once, with the measure that brought it', async ($, on) => {
  const w = world(on, { answer: service(true) })
  engine(on)
  await $.session.start(START)
  await w.clock.settle()

  await $.session.measure(MEASURE as never)
  await w.clock.advance(500)
  await w.clock.settle()
  await $.session.measure({ ...MEASURE, changed: ['context'] } as never)
  await w.clock.advance(500)
  await w.clock.settle()

  const sent = inputs(w.requests)
  expect(sent.length).toBe(2)
  expect(sent[0]?.rate_limits).toEqual([{ name: 'five_hour', percent: 23.5, resets_at: '2026-11-07T18:00:00Z' }])
  expect(sent[1]?.rate_limits).toBe(undefined)
})

test('a failed subscription the reloaded mod restores is asked once more, then measures stop', async ($, on) => {
  const w = world(on, { answer: service(false) })
  engine(on)
  // Kept failed before the mod reloaded: the brick was removed.
  const id = 'nowcast/session'
  const failed = {
    brick: 'claude-code',
    kind: 'session',
    params: { id: 'session-1' },
    status: 'failed',
    error: { code: 'unknown_brick', message: 'no brick' },
    flow: FLOW,
    service: null,
  }
  on('state.get', { plugin: 'nowcast', key: 'subscribed' }, () => ({ value: { value: [id], version: 1 } }))
  on('state.get', { plugin: 'nowcast', key: 'subscriptions' }, (_, e) => ({
    value: e.id === id ? { value: failed as never, version: 1 } : { value: undefined, version: 0 },
  }))
  await $.session.start(START)
  await w.clock.settle()

  for (let i = 0; i < 3; i++) {
    await $.session.measure(MEASURE as never)
    await w.clock.advance(500)
    await w.clock.settle()
  }
  expect(w.requests.filter(r => r.url.endsWith('/v1/subscriptions')).length).toBe(1)
  expect(inputs(w.requests)).toEqual([])
})

test('without the brick, nothing is sent and the subscription is not asked again', async ($, on) => {
  const w = world(on, { answer: service(false) })
  engine(on)
  await $.session.start(START)
  await w.clock.settle()

  await $.session.measure(MEASURE as never)
  await w.clock.advance(500)
  await w.clock.settle()
  // Once refused, a later measure is not sent at all.
  await $.session.measure(MEASURE as never)
  await w.clock.advance(500)

  expect(w.requests.filter(r => r.url.endsWith('/v1/subscriptions')).length).toBe(1)
  expect(inputs(w.requests)).toEqual([])
})

test("the tokens spent before the mod loaded come from the session's transcripts", async ($, on) => {
  const w = world(on, { answer: service(true), env: { XDG_RUNTIME_DIR: '/run/user/1000', HOME: '/home/u' } })
  engine(on)
  await $.session.start(START)
  await w.clock.settle()

  await $.session.measure(MEASURE as never)
  await w.clock.settle()
  const cat = w.children.find(c => c.argv[0] === 'sh')
  expect(cat?.argv.slice(-3, -1)).toEqual(['/home/u/.claude/projects', 'session-1'])
  cat?.stdout(`${assistant('a', { input_tokens: 1, output_tokens: 2, cache_creation_input_tokens: 3, cache_read_input_tokens: 4 })}\n`)
  cat?.exit(0)
  await w.clock.advance(500)
  await w.clock.settle()

  expect(inputs(w.requests).at(-1)?.tokens).toEqual({ input: 1, output: 2, cache_creation: 3, cache_read: 4 })
})

test('each response counts once, at its last record', () => {
  const text = [
    assistant('a', { input_tokens: 5, output_tokens: 1 }),
    assistant('a', { input_tokens: 5, output_tokens: 9, cache_read_input_tokens: 100 }),
    JSON.stringify({ type: 'user', message: { content: 'hi' } }),
    assistant('b', { input_tokens: 2, output_tokens: 3, cache_creation_input_tokens: 7 }),
    'not json',
  ].join('\n')
  expect(sumUsage(text)).toEqual({ input: 7, output: 12, cache_creation: 7, cache_read: 100 })
  expect(sumUsage('')).toBe(undefined)
})

test("the cache's lifetime is the last one a response wrote with", () => {
  const wrote = (h: number, m: number) =>
    assistant('x', { cache_creation: { ephemeral_1h_input_tokens: h, ephemeral_5m_input_tokens: m } })
  // The first line of a tail is cut: it is skipped.
  expect(lastTtl(['t": 1}', wrote(0, 50), wrote(0, 0)].join('\n'))).toBe('5m')
  expect(lastTtl([wrote(0, 50), wrote(10, 0)].join('\n'))).toBe('1h')
  expect(lastTtl(wrote(0, 0))).toBe(undefined)
})

test("at entry, the cache's time and lifetime come from the main thread's last request that used it", async ($, on) => {
  const w = world(on, { answer: service(true), env: { XDG_RUNTIME_DIR: '/run/user/1000', HOME: '/home/u' } })
  engine(on)
  await $.session.start(START)
  await w.clock.settle()

  const row = (id: string, timestamp: string, usage: Record<string, unknown>, isSidechain = false) =>
    JSON.stringify({ type: 'assistant', timestamp, isSidechain, message: { id, usage } })
  const cat = w.children.find(c => c.argv[0] === 'sh')
  cat?.stdout(
    [
      row('a', '2026-11-26T10:00:00.000Z', { input_tokens: 1, cache_creation_input_tokens: 5, cache_creation: { ephemeral_1h_input_tokens: 5 } }),
      // The tool results the next request answers, written as it went out.
      JSON.stringify({ type: 'user', timestamp: '2026-11-26T10:04:30.000Z', message: { content: [] } }),
      row('b', '2026-11-26T10:05:00.000Z', { input_tokens: 1, cache_read_input_tokens: 5 }),
      row('c', '2026-11-26T10:06:00.000Z', { input_tokens: 1 }),
      '',
      '--nowcast-subagents--',
      // A subagent's request does not refresh the main thread's cache.
      row('d', '2026-11-26T10:09:00.000Z', { input_tokens: 1, cache_read_input_tokens: 5 }, true),
    ].join('\n'),
  )
  cat?.exit(0)
  const measures = await sent(w)

  expect(measures?.tokens).toEqual({ input: 4, output: 0, cache_creation: 5, cache_read: 10 })
  expect(measures?.last_request_at).toBe('2026-11-26T10:04:30.000Z')
  expect(measures?.cache_ttl).toBe('1h')
  expect(measures?.context).toEqual({ window: 200000 })
})

test('the last request that used the cache went out when the row before its response was written', () => {
  const response = (id: string, timestamp: unknown, usage: Record<string, unknown>, isSidechain?: boolean) =>
    JSON.stringify({ type: 'assistant', timestamp, isSidechain, message: { id, usage } })
  const input = (timestamp: unknown) => JSON.stringify({ type: 'user', timestamp, message: { content: [] } })
  const cached = { cache_read_input_tokens: 3 }
  expect(
    lastCachedAt(
      [
        input('2026-11-26T10:00:00Z'),
        // A row with no time, and a subagent's, tell nothing of the request.
        JSON.stringify({ type: 'file-history-snapshot' }),
        response('s', '2026-11-26T10:00:30Z', cached, true),
        // One response, one record per content block.
        response('r', '2026-11-26T10:01:00Z', cached),
        response('r', '2026-11-26T10:02:00Z', cached),
        response('n', '2026-11-26T10:03:00Z', { input_tokens: 3 }),
      ].join('\n'),
    ),
  ).toBe('2026-11-26T10:00:00.000Z')
  // Nothing before the response: its own first record.
  expect(lastCachedAt(response('r', '2026-11-26T10:01:00Z', cached))).toBe('2026-11-26T10:01:00.000Z')
  expect(lastCachedAt(response('r', '2026-11-26T10:01:00Z', { input_tokens: 3 }))).toBe(undefined)
  expect(lastCachedAt('')).toBe(undefined)
})
