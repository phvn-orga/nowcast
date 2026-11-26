// Entering a conversation, Claude Code's session id, the same way at the
// process's start, after a `/clear` or a resume, and at a turn under another
// id: the subscriptions that follow it move, `conversation` is written, the
// measures go out at once. A reload of the mod is not an entry.

import type { HttpResponse, On } from 'claude-code'
import { expect, test } from 'claude-code/testing'
import type { NowcastSubscription } from '../types'
import { type Request, START, type World, call, caller, update, world } from './world'

const plugins = { plugins: [caller] }
const RESUME = { id: 'session-1' }
const ID = 'caller/session'
const FOLLOWS = { name: 'session', brick: 'claude-code', kind: 'sessions', params: {}, conversation: 'id' }
const at = (ms: number) => new Date(ms).toISOString()

/**
 * A service subscribing each request to `claude-code/<kind>/<id>`, under
 * `s-1`, `s-2`..., with the last value `last` gives for the flow, and taking
 * every input.
 */
function service(last: Record<string, unknown> = {}) {
  let made = 0
  return (request: Request): Partial<HttpResponse> => {
    if (request.method === 'POST' && request.url.endsWith('/v1/subscriptions')) {
      const body = JSON.parse(request.body ?? '{}') as { kind: string; params: { id: string } }
      const flow = `claude-code/${body.kind}/${body.params.id}`
      const value = flow in last ? update(flow, '1', last[flow]) : null
      return { status: 201, text: JSON.stringify({ subscription: `s-${++made}`, flow, last: value }) }
    }
    if (request.method === 'POST' && request.url.endsWith('/input')) return { status: 202 }
    return {}
  }
}

/** The engine's figures for the measures, the context by session id. */
function engine(on: On, w: World, contexts: Record<string, unknown>) {
  on('session.root', () => ({ value: '/project' }))
  on('session.model', () => ({ value: 'claude-opus-5-5' }))
  on('session.usage', () => ({ value: { startedAt: 0, context: contexts[w.session.id], rateLimits: [] } as never }))
}

const conversations = (w: World) => w.writes.filter(x => x.key === 'conversation').map(x => x.value)
const path = (r: Request) => `${r.method} ${r.url.replace('http://nowcast', '')}`
const inputsTo = (w: World, flow: string) =>
  w.requests
    .filter(r => r.url.endsWith(`/v1/flows/${encodeURIComponent(flow)}/input`))
    .map(r => JSON.parse(r.body ?? 'null') as Record<string, unknown>)

test('the process start enters the conversation, and a /clear or a resume the next one', async ($, on) => {
  const w = world(on)
  await $.session.start(START)
  await w.clock.settle()
  expect(conversations(w)).toEqual([{ id: 'session-1', since: at(1_000_000) }])

  await $.session.end({ reason: 'clear', sessionId: 'session-1', resume: RESUME })
  w.session.id = 'session-2'
  await w.clock.advance(50)
  await $.session.end({ reason: 'resume', sessionId: 'session-2', resume: { id: 'session-2' } })
  w.session.id = 'session-1'
  await w.clock.advance(50)

  expect(conversations(w)).toEqual([
    { id: 'session-1', since: at(1_000_000) },
    { id: 'session-2', since: at(1_000_050) },
    { id: 'session-1', since: at(1_000_100) },
  ])
})

test('a turn under another id enters it, once', async ($, on) => {
  const w = world(on)
  await $.session.start(START)
  await w.clock.settle()

  w.session.id = 'session-2'
  await $.turn.start({ text: 'hi', turnId: 't-1' })
  await $.turn.start({ text: 'hi', turnId: 't-2' })
  await w.clock.settle()

  expect(conversations(w).map(c => (c as { id: string }).id)).toEqual(['session-1', 'session-2'])
})

test('a reload of the mod is not an entry', async ($, on) => {
  const w = world(on, { answer: service() })
  engine(on, w, { 'session-1': { window: 200000 } })
  // What the mod wrote before it reloaded, which outlives the reload.
  on('state.get', { plugin: 'nowcast', key: 'conversation' }, () => ({
    value: { value: { id: 'session-1', since: at(900_000) }, version: 1 },
  }))
  await $.session.start(START)
  await w.clock.settle()
  await $.turn.start({ text: 'hi', turnId: 't-1' })
  await w.clock.advance(500)
  await w.clock.settle()

  expect(conversations(w)).toEqual([])
  // No measures go out until the session measures something.
  expect(w.requests.map(path)).toEqual(['PUT /v1/session'])
  // The stream comes back all the same.
  expect(w.children.map(c => c.session)).toEqual(['session-1'])
})

test('a subscription that follows the conversation gets its id, and the same request finds it in place', plugins, async ($, on) => {
  const w = world(on, { answer: service() })
  await $.session.start(START)
  await w.clock.settle()

  const answer = await call($, 'subscribe', FOLLOWS)
  expect(answer).toEqual({ ok: true, subscription: ID, status: 'active', error: null, flow: 'claude-code/sessions/session-1' })
  // The service is asked the parameters alone.
  const posts = w.requests.filter(r => path(r) === 'POST /v1/subscriptions')
  expect(posts.map(r => JSON.parse(r.body ?? 'null'))).toEqual([
    { brick: 'claude-code', kind: 'sessions', params: { id: 'session-1' } },
  ])
  expect(w.subscription(ID)).toEqual({
    brick: 'claude-code',
    kind: 'sessions',
    params: { id: 'session-1' },
    conversation: 'id',
    status: 'active',
    error: null,
    flow: 'claude-code/sessions/session-1',
    service: 's-1',
  })

  // A reloaded mod asks the same again; an id it gives is the conversation's all the same.
  expect(await call($, 'subscribe', FOLLOWS)).toEqual(answer)
  expect(await call($, 'subscribe', { ...FOLLOWS, params: { id: 'other' } })).toEqual(answer)
  expect(w.requests.filter(r => path(r) === 'POST /v1/subscriptions').length).toBe(1)
})

test('a conversation that names no parameter, or params that are not an object, are refused', plugins, async ($, on) => {
  const w = world(on, { answer: service() })
  await $.session.start(START)
  await w.clock.settle()

  for (const request of [
    { ...FOLLOWS, conversation: '' },
    { ...FOLLOWS, conversation: 1 },
    { ...FOLLOWS, params: ['id'] },
  ]) {
    expect(await call($, 'subscribe', request)).toMatchObject({ ok: false, status: null, code: 'invalid_request' })
  }
  expect(w.subscription(ID)).toBe(undefined)
})

test('after a /clear, a subscription that follows the conversation moves, and the previous flow never shows again', plugins, async ($, on) => {
  const previous = 'claude-code/sessions/session-1'
  const w = world(on, { answer: service({ [previous]: { context: { percent: 7 } } }) })
  await $.session.start(START)
  await w.clock.settle()
  await call($, 'subscribe', FOLLOWS)
  expect(w.flow(previous)?.last?.data).toEqual({ context: { percent: 7 } })
  const requests = w.requests.length
  const writes = w.writes.length

  await $.session.end({ reason: 'clear', sessionId: 'session-1', resume: RESUME })
  w.session.id = 'session-2'
  await w.clock.advance(50)
  await w.clock.settle()

  // The new session is declared, once; then the service ends the previous
  // subscription before the new one is asked for.
  expect(w.requests.slice(requests).map(path)).toEqual([
    'PUT /v1/session',
    'DELETE /v1/subscriptions/s-1',
    'POST /v1/subscriptions',
  ])
  expect(JSON.parse(w.requests[requests + 2]?.body ?? 'null')).toMatchObject({ params: { id: 'session-2' } })
  // But for the stream, the moved subscription is the first write into the new state, before the conversation.
  const after = w.writes.slice(writes).filter(x => x.key !== 'stream')
  const moved: NowcastSubscription = {
    brick: 'claude-code',
    kind: 'sessions',
    params: { id: 'session-2' },
    conversation: 'id',
    status: 'active',
    error: null,
    flow: 'claude-code/sessions/session-2',
    service: 's-2',
  }
  expect(after[0]).toEqual({ key: 'subscriptions', id: ID, value: moved })
  expect(after.findIndex(x => x.key === 'conversation')).toBeGreaterThan(0)
  expect(after.filter(x => x.id === previous || JSON.stringify(x.value).includes(previous))).toEqual([])
  expect(w.child().session).toBe('session-2')
})

test('after a resume, the first measure carries the resumed context; a new conversation shows its window alone', async ($, on) => {
  const w = world(on, { answer: service() })
  const resumed = { tokens: 72427, window: 1000000, percent: 7 }
  engine(on, w, { 'session-1': { window: 1000000 }, 'session-2': resumed })
  await $.session.start(START)
  await w.clock.advance(500)
  await w.clock.settle()
  expect(inputsTo(w, 'claude-code/session/session-1').map(m => m.context)).toEqual([{ window: 1000000 }])

  await $.session.end({ reason: 'resume', sessionId: 'session-1', resume: RESUME })
  w.session.id = 'session-2'
  await w.clock.advance(50)
  await w.clock.advance(500)
  await w.clock.settle()

  expect(inputsTo(w, 'claude-code/session/session-2').map(m => m.context)).toEqual([resumed])
})

test('a subscription asked for as the conversation changes ends in the new one', plugins, async ($, on) => {
  let open: () => void = () => undefined
  const gate = new Promise<void>(resolve => (open = resolve))
  const answer = service()
  // The first subscription is answered only once the /clear is entered.
  let first = true
  const w = world(on, {
    answer: async request => {
      if (first && path(request) === 'POST /v1/subscriptions') {
        first = false
        await gate
      }
      return answer(request)
    },
  })
  await $.session.start(START)
  await w.clock.settle()

  const asked = call($, 'subscribe', FOLLOWS)
  await w.clock.settle()
  await $.session.end({ reason: 'clear', sessionId: 'session-1', resume: RESUME })
  w.session.id = 'session-2'
  await w.clock.advance(50)
  open()
  await asked
  await w.clock.settle()

  expect(w.subscription(ID)).toMatchObject({ params: { id: 'session-2' }, flow: 'claude-code/sessions/session-2' })
  expect(w.requests.filter(r => r.method === 'DELETE').map(path)).toEqual(['DELETE /v1/subscriptions/s-1'])
})
