// The session's measures, sent to the claude-code brick on the session's own
// flow (the brick's `claude-code.measure/v1` input).

import type { HttpResponse, On } from 'claude-code'
import { expect, test } from 'claude-code/testing'
import { lastTtl, sumUsage } from '../hooks/measures'
import { type Request, START, world } from './world'

const FLOW = 'claude-code/session/session-1'
const INPUT = `/v1/flows/${encodeURIComponent(FLOW)}/input`

const assistant = (id: string, usage: Record<string, unknown>) =>
  JSON.stringify({ type: 'assistant', message: { id, usage } })

/** The engine's answers the measures read, beneath the mod. */
function engine(on: On) {
  on('session.root', () => ({ value: '/project' }))
  on('session.model', () => ({ value: 'claude-opus-5-5' }))
  on('session.measure', (_, e) => ({ changed: e.changed }))
  on('turn.step', async function* (_, e) {
    return {
      turnId: e.turnId,
      index: e.index,
      answer: '',
      toolUses: [],
      stopReason: 'end_turn',
      usage: { input_tokens: 10, output_tokens: 20, cache_creation_input_tokens: 30, cache_read_input_tokens: 40 },
    } as never
  })
}

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
      context: MEASURE.context,
      effort: 'high',
      last_request_at: new Date(1_000_000).toISOString(),
      cost_usd: 1.5,
      rate_limits: [{ name: 'five_hour', percent: 23.5, resets_at: '2026-11-07T18:00:00Z' }],
    },
  ])
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
  expect(cat?.argv.slice(-2)).toEqual(['/home/u/.claude/projects', 'session-1'])
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
