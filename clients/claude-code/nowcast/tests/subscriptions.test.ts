// The subscriptions the nowcast mod keeps for every mod, under `<mod>/<name>`
// (`02-clients.md` on the design branch), taken through `$.nowcast` by other
// mods as any mod would.

import type { HttpResponse } from 'claude-code'
import { expect, test } from 'claude-code/testing'
import { type Request, START, call, caller, other, update, world } from './world'

const FLOW = 'forge/github.com/org/repo#main'
const ENCODED = 'forge%2Fgithub.com%2Forg%2Frepo%23main'
const WANTED = { brick: 'forge', kind: 'pr', params: { remote: 'git@github.com:org/repo.git', branch: 'main' } }
const PR = { name: 'pr', ...WANTED }
const ID = 'caller/pr'

type Route = (request: Request) => Partial<HttpResponse> | string | undefined

/** A service answering `routes` by `<method> <path>`, and `204` to the rest. */
function service(routes: Record<string, Partial<HttpResponse> | string | Route>) {
  return (request: Request) => {
    const route = routes[`${request.method} ${request.url.replace('http://nowcast', '')}`]
    return (typeof route === 'function' ? route(request) : route) ?? {}
  }
}

const subscribed = (subscription: string, last: unknown = null, flow = FLOW) => ({
  status: 201,
  text: JSON.stringify({ subscription, flow, last }),
})

const refused = (status: number, code: string, message = 'no') => ({
  status,
  text: JSON.stringify({ error: { code, message } }),
})

const posts = (requests: Request[]) => requests.filter(r => r.method === 'POST' && r.url.endsWith('/v1/subscriptions'))

const plugins = { plugins: [caller, other] }

test('a subscription is kept under the mod and its name, and the state says where it stands', plugins, async ($, on) => {
  const w = world(on, { answer: service({ 'POST /v1/subscriptions': subscribed('s-1') }) })

  expect(await call($, 'subscribe', PR)).toEqual({ ok: true, subscription: ID, status: 'active', error: null, flow: FLOW })
  // The service gets the request without its name.
  expect(posts(w.requests).map(r => r.body)).toEqual([JSON.stringify(WANTED)])
  expect(w.subscription(ID)).toEqual({ ...WANTED, status: 'active', error: null, flow: FLOW, service: 's-1' })
  expect(w.subscribed()).toEqual([ID])
  // No value yet: nothing to write.
  expect(w.flowWrites(FLOW)).toEqual([])
})

test("the flow's last value in the answer is written to the state", plugins, async ($, on) => {
  const last = update(FLOW, '7', { number: 1 })
  const w = world(on, { answer: service({ 'POST /v1/subscriptions': subscribed('s-1', last) }) })

  await call($, 'subscribe', PR)
  expect(w.flowWrites(FLOW)).toEqual([{ last, error: null, ended: false }])
})

test('a last value older than the one the stream gave is not written', plugins, async ($, on) => {
  const older = update(FLOW, '7', { number: 1 })
  const w = world(on, { answer: service({ 'POST /v1/subscriptions': subscribed('s-1', older) }) })
  await $.session.start(START)
  await w.clock.settle()
  w.child().event('update', update(FLOW, '8', { number: 2 }))
  await w.clock.settle()

  await call($, 'subscribe', PR)
  expect(w.flowWrites(FLOW).map(f => f.last?.id)).toEqual(['8'])
})

test('a newer last value is written; the same one again is not', plugins, async ($, on) => {
  let id = '9'
  const w = world(on, {
    answer: service({ 'POST /v1/subscriptions': () => subscribed('s-1', update(FLOW, id, { number: 3 })) }),
  })
  await $.session.start(START)
  await w.clock.settle()
  w.child().event('update', update(FLOW, '8', { number: 2 }))
  await w.clock.settle()

  await call($, 'subscribe', { ...PR, name: 'a' })
  await call($, 'subscribe', { ...PR, name: 'b' })
  // Ids from one counter, compared as numbers: 10 comes after 9.
  id = '10'
  await call($, 'subscribe', { ...PR, name: 'c' })
  expect(w.flowWrites(FLOW).map(f => f.last?.id)).toEqual(['8', '9', '10'])
})

test('the same name and request answer the subscription in place', plugins, async ($, on) => {
  const w = world(on, { answer: service({ 'POST /v1/subscriptions': subscribed('s-1') }) })

  await call($, 'subscribe', PR)
  // The order of the params' keys does not matter.
  const again = { name: 'pr', brick: 'forge', kind: 'pr', params: { branch: 'main', remote: WANTED.params.remote } }
  expect(await call($, 'subscribe', again)).toEqual({ ok: true, subscription: ID, status: 'active', error: null, flow: FLOW })
  expect(posts(w.requests).length).toBe(1)
})

test('the same name with another request replaces the subscription', plugins, async ($, on) => {
  let next = 1
  const w = world(on, {
    answer: service({
      'POST /v1/subscriptions': () => subscribed(`s-${next++}`),
      'DELETE /v1/subscriptions/s-1': { status: 204 },
    }),
  })

  await call($, 'subscribe', PR)
  const dev = { ...PR, params: { ...WANTED.params, branch: 'dev' } }
  expect(await call($, 'subscribe', dev)).toMatchObject({ ok: true, subscription: ID, status: 'active' })
  expect(w.requests.map(r => `${r.method} ${r.url.replace('http://nowcast', '')}`)).toEqual([
    'POST /v1/subscriptions',
    'DELETE /v1/subscriptions/s-1',
    'POST /v1/subscriptions',
  ])
  expect(w.subscription(ID)).toMatchObject({ params: dev.params, service: 's-2' })
  expect(w.subscribed()).toEqual([ID])
})

test('two mods may use the same name', plugins, async ($, on) => {
  let next = 1
  const w = world(on, {
    answer: service({
      'POST /v1/subscriptions': () => subscribed(`s-${next++}`),
      'DELETE /v1/subscriptions/s-1': { status: 204 },
    }),
  })

  await call($, 'subscribe', PR)
  expect(await call($, 'subscribe', PR, 'call-other')).toMatchObject({ ok: true, subscription: 'other/pr' })
  expect(posts(w.requests).length).toBe(2)

  // One mod's unsubscribe leaves the other's alone.
  expect(await call($, 'unsubscribe', { name: 'pr' })).toEqual({ ok: true })
  expect(w.subscription(ID)).toBeNull()
  expect(w.subscription('other/pr')).toMatchObject({ status: 'active', service: 's-2' })
  expect(w.subscribed()).toEqual(['other/pr'])
})

test('unsubscribe ends the subscription in the service and in the state', plugins, async ($, on) => {
  const w = world(on, {
    answer: service({ 'POST /v1/subscriptions': subscribed('s-1'), 'DELETE /v1/subscriptions/s-1': { status: 204 } }),
  })
  await call($, 'subscribe', PR)

  expect(await call($, 'unsubscribe', { name: 'pr' })).toEqual({ ok: true })
  expect(w.requests.at(-1)).toMatchObject({ method: 'DELETE', url: 'http://nowcast/v1/subscriptions/s-1' })
  expect(w.subscription(ID)).toBeNull()
  expect(w.subscribed()).toEqual([])
  // A name not held answers ok too, and asks nothing.
  expect(await call($, 'unsubscribe', { name: 'pr' })).toEqual({ ok: true })
  expect(w.requests.length).toBe(2)
})

for (const [status, code] of [
  [404, 'unknown_brick'],
  [404, 'unknown_kind'],
  [400, 'invalid_params'],
] as const) {
  test(`${code} fails, and nothing is kept nor tried again`, plugins, async ($, on) => {
    const w = world(on, { answer: service({ 'POST /v1/subscriptions': refused(status, code, 'nope') }) })

    expect(await call($, 'subscribe', PR)).toEqual({ ok: false, status, code, message: 'nope' })
    expect(w.subscription(ID)).toBeUndefined()
    await w.clock.advance(120_000)
    expect(posts(w.requests).length).toBe(1)
  })
}

test('a subscription without a name fails, and nothing is sent', plugins, async ($, on) => {
  const w = world(on)
  expect(await call($, 'subscribe', WANTED)).toMatchObject({ ok: false, status: null, code: 'invalid_name' })
  expect(await call($, 'subscribe', { ...WANTED, name: '' })).toMatchObject({ ok: false, code: 'invalid_name' })
  expect(w.requests).toEqual([])
})

test('a subscription refused for now is kept, and tried again later each time', plugins, async ($, on) => {
  let up = false
  const last = update(FLOW, '3', { number: 1 })
  const w = world(on, {
    answer: service({
      'POST /v1/subscriptions': () => (up ? subscribed('s-1', last) : refused(503, 'brick_unavailable', 'not ready')),
    }),
  })

  const error = { code: 'brick_unavailable', message: 'not ready' }
  expect(await call($, 'subscribe', PR)).toEqual({ ok: true, subscription: ID, status: 'retrying', error, flow: null })
  expect(w.subscription(ID)).toEqual({ ...WANTED, status: 'retrying', error, flow: null, service: null })
  expect(w.subscribed()).toEqual([ID])

  // 1 s, then 2 s, then 4 s.
  await w.clock.advance(999)
  expect(posts(w.requests).length).toBe(1)
  await w.clock.advance(1)
  expect(posts(w.requests).length).toBe(2)
  await w.clock.advance(2_000)
  expect(posts(w.requests).length).toBe(3)
  up = true
  await w.clock.advance(4_000)
  expect(posts(w.requests).length).toBe(4)

  expect(w.subscription(ID)).toEqual({ ...WANTED, status: 'active', error: null, flow: FLOW, service: 's-1' })
  expect(w.flowWrites(FLOW)).toEqual([{ last, error: null, ended: false }])
  await w.clock.advance(120_000)
  expect(posts(w.requests).length).toBe(4)
})

test('the delay between attempts stops growing at a minute', plugins, async ($, on) => {
  const w = world(on, { answer: () => 'connection refused' })
  expect(await call($, 'subscribe', PR)).toMatchObject({ ok: true, status: 'retrying', error: { code: 'unreachable' } })

  // 1 + 2 + 4 + 8 + 16 + 32 s, then every 60 s.
  await w.clock.advance(63_000)
  expect(posts(w.requests).length).toBe(7)
  await w.clock.advance(59_999)
  expect(posts(w.requests).length).toBe(7)
  await w.clock.advance(1)
  expect(posts(w.requests).length).toBe(8)
})

test('a subscription unsubscribed while it waits is not tried again', plugins, async ($, on) => {
  const w = world(on, { answer: service({ 'POST /v1/subscriptions': refused(422, 'subscription_refused') }) })
  await call($, 'subscribe', PR)

  expect(await call($, 'unsubscribe', { name: 'pr' })).toEqual({ ok: true })
  await w.clock.advance(120_000)
  expect(posts(w.requests).length).toBe(1)
  // It never reached the service: nothing to delete there.
  expect(w.requests.some(r => r.method === 'DELETE')).toBe(false)
  expect(w.subscription(ID)).toBeNull()
})

test('a waiting subscription is tried again at once when the stream opens', plugins, async ($, on) => {
  let up = false
  const w = world(on, {
    answer: service({ 'POST /v1/subscriptions': () => (up ? subscribed('s-1') : 'connection refused') }),
  })
  await $.session.start(START)
  await w.clock.settle()
  await call($, 'subscribe', PR)

  up = true
  w.child().connected()
  await w.clock.settle()
  expect(posts(w.requests).length).toBe(2)
  expect(w.subscription(ID)).toMatchObject({ status: 'active', service: 's-1' })
  // The timer set before does not try once more.
  await w.clock.advance(120_000)
  expect(posts(w.requests).length).toBe(2)
})

test('a new subscription clears the ended value an earlier one left', plugins, async ($, on) => {
  const w = world(on, { answer: service({ 'POST /v1/subscriptions': subscribed('s-1') }) })
  await $.session.start(START)
  await w.clock.settle()
  w.child().event('update', update(FLOW, '4', { number: 1 }))
  w.child().event('flow-ended', { flow: FLOW })
  await w.clock.settle()
  expect(w.flow(FLOW)?.ended).toBe(true)

  await call($, 'subscribe', PR)
  expect(w.flow(FLOW)).toEqual({ last: null, error: null, ended: false })
})

/** A service that forgets every subscription when `restart` is called. */
function restartable() {
  let next = 1
  let known = new Set<string>()
  let brick = true
  const answer = service({
    'POST /v1/subscriptions': () => {
      if (!brick) return refused(404, 'unknown_brick', 'no such brick')
      // The brick publishes again on each new subscription.
      const id = `s-${next}`
      known.add(id)
      return subscribed(id, update(FLOW, String(next++), { number: 1 }))
    },
    [`GET /v1/flows/${ENCODED}`]: () => (known.size > 0 ? { status: 204 } : refused(404, 'unknown_flow')),
    'DELETE /v1/subscriptions/s-1': () => (known.delete('s-1') ? { status: 204 } : refused(404, 'unknown_subscription')),
    'DELETE /v1/subscriptions/s-2': () => (known.delete('s-2') ? { status: 204 } : refused(404, 'unknown_subscription')),
  })
  return {
    answer,
    known: () => [...known],
    restart: () => {
      known = new Set()
    },
    removeBrick: () => {
      known = new Set()
      brick = false
    },
  }
}

test('after the stream comes back, a subscription the service forgot is made again', plugins, async ($, on) => {
  const svc = restartable()
  const w = world(on, { answer: svc.answer })
  await $.session.start(START)
  await w.clock.settle()
  await call($, 'subscribe', PR)

  svc.restart()
  w.child().retrying('the service closed the stream')
  w.child().connected()
  await w.clock.settle()

  expect(svc.known()).toEqual(['s-2'])
  expect(w.subscription(ID)).toMatchObject({ status: 'active', service: 's-2' })
  // The value the new subscription answered is written.
  expect(w.flowWrites(FLOW).map(f => f.last?.id)).toEqual(['1', '2'])
  // The name ends the subscription made again.
  expect(await call($, 'unsubscribe', { name: 'pr' })).toEqual({ ok: true })
  expect(svc.known()).toEqual([])
})

test('after the stream comes back, a subscription still known is left alone', plugins, async ($, on) => {
  const svc = restartable()
  const w = world(on, { answer: svc.answer })
  await $.session.start(START)
  await w.clock.settle()
  await call($, 'subscribe', PR)

  w.child().retrying('the service closed the stream')
  w.child().connected()
  await w.clock.settle()

  expect(svc.known()).toEqual(['s-1'])
  expect(posts(w.requests).length).toBe(1)
})

test('once `watch` is started again, forgotten subscriptions are made again', plugins, async ($, on) => {
  const svc = restartable()
  const w = world(on, { answer: svc.answer })
  await $.session.start(START)
  await w.clock.settle()
  await call($, 'subscribe', PR)
  // The value the subscription answered, which the stream also sends.
  w.child().event('update', update(FLOW, '1', { number: 1 }))
  await w.clock.settle()

  svc.restart()
  w.child().exit(1)
  await w.clock.advance(1_000)
  expect(w.children.length).toBe(2)
  expect(w.child().lastEventId).toBe('1')
  w.child().connected()
  await w.clock.settle()

  expect(svc.known()).toEqual(['s-2'])
  expect(w.flowWrites(FLOW).map(f => f.last?.id)).toEqual(['1', '2'])
})

test('a subscription ended is not made again', plugins, async ($, on) => {
  const svc = restartable()
  const w = world(on, { answer: svc.answer })
  await $.session.start(START)
  await w.clock.settle()
  await call($, 'subscribe', PR)
  await call($, 'unsubscribe', { name: 'pr' })

  svc.restart()
  w.child().retrying('lost')
  w.child().connected()
  await w.clock.settle()

  expect(svc.known()).toEqual([])
  expect(posts(w.requests).length).toBe(1)
})

test('a flow that ends is checked, and its subscription made again', plugins, async ($, on) => {
  const svc = restartable()
  const w = world(on, { answer: svc.answer })
  await $.session.start(START)
  await w.clock.settle()
  await call($, 'subscribe', PR)

  svc.restart()
  w.child().event('flow-ended', { flow: FLOW })
  await w.clock.settle()

  expect(svc.known()).toEqual(['s-2'])
  expect(w.subscription(ID)).toMatchObject({ status: 'active', service: 's-2' })
  expect(w.flow(FLOW)?.ended).toBe(false)
})

test('a brick removed leaves its subscriptions failed, until unsubscribed', plugins, async ($, on) => {
  const svc = restartable()
  const w = world(on, { answer: svc.answer })
  await $.session.start(START)
  await w.clock.settle()
  await call($, 'subscribe', PR)

  svc.removeBrick()
  w.child().event('flow-ended', { flow: FLOW })
  await w.clock.settle()

  expect(w.subscription(ID)).toEqual({
    ...WANTED,
    status: 'failed',
    error: { code: 'unknown_brick', message: 'no such brick' },
    flow: FLOW,
    service: null,
  })
  await w.clock.advance(120_000)
  expect(posts(w.requests).length).toBe(2)
  // Asked again as it was, it answers where it stands.
  expect(await call($, 'subscribe', PR)).toMatchObject({ ok: true, status: 'failed' })
  expect(await call($, 'unsubscribe', { name: 'pr' })).toEqual({ ok: true })
  expect(w.subscribed()).toEqual([])
})

test('after a /clear, the subscriptions are written again into the new state', plugins, async ($, on) => {
  const w = world(on, { answer: service({ 'POST /v1/subscriptions': subscribed('s-1') }) })
  await $.session.start(START)
  await w.clock.settle()
  await call($, 'subscribe', PR)
  const before = w.writes.length

  await $.session.end({ reason: 'clear', sessionId: 'session-1', resume: { id: 'session-1' } })
  w.session.id = 'session-2'
  await w.clock.advance(50)

  const again = w.writes.slice(before).filter(x => x.key === 'subscriptions' || x.key === 'subscribed')
  expect(again).toEqual([
    { key: 'subscriptions', id: ID, value: { ...WANTED, status: 'active', error: null, flow: FLOW, service: 's-1' } },
    { key: 'subscribed', id: undefined, value: [ID] },
  ])
  // Nothing is asked of the service again.
  expect(posts(w.requests).length).toBe(1)
})

test('a reloaded mod takes back the subscriptions its state holds', plugins, async ($, on) => {
  const svc = restartable()
  const w = world(on, { answer: svc.answer })
  // What the mod wrote before it reloaded, which outlives the reload; the
  // service forgot meanwhile.
  on('state.get', { plugin: 'nowcast', key: 'subscribed' }, () => ({ value: { value: [ID], version: 1 } }))
  on('state.get', { plugin: 'nowcast', key: 'subscriptions' }, (_, e) => ({
    value:
      e.id === ID
        ? { value: { ...WANTED, status: 'active', error: null, flow: FLOW, service: 's-0' }, version: 1 }
        : { value: undefined, version: 0 },
  }))

  await $.session.start(START)
  await w.clock.settle()
  w.child().connected()
  await w.clock.settle()

  expect(svc.known()).toEqual(['s-1'])
  expect(w.subscription(ID)).toMatchObject({ status: 'active', service: 's-1' })
  expect(await call($, 'unsubscribe', { name: 'pr' })).toEqual({ ok: true })
  expect(svc.known()).toEqual([])
})
