// `$.nowcast`, the library other mods use (contract section 3, "Abonnements"
// and "Flux"), called by another mod as any mod would.

import type { HttpResponse } from 'claude-code'
import { expect, test } from 'claude-code/testing'
import { type Request, SOCKET, START, call, caller, update, world } from './world'

const FLOW = 'forge/github.com/org/repo#main'
const ENCODED = 'forge%2Fgithub.com%2Forg%2Frepo%23main'
const PR = { brick: 'forge', kind: 'pr', params: { remote: 'git@github.com:org/repo.git', branch: 'main' } }

type Route = (request: Request) => Partial<HttpResponse> | string | undefined

/** A service answering `routes` by `<method> <path>`, and `204` to the rest. */
function service(routes: Record<string, Partial<HttpResponse> | string | Route>) {
  return (request: Request) => {
    const route = routes[`${request.method} ${request.url.replace('http://nowcast', '')}`]
    return (typeof route === 'function' ? route(request) : route) ?? {}
  }
}

const subscribed = (subscription: string, last: unknown = null) => ({
  status: 201,
  text: JSON.stringify({ subscription, flow: FLOW, last }),
})

const plugins = { plugins: [caller] }

test('subscribe sends the request and answers the subscription', plugins, async ($, on) => {
  const w = world(on, { answer: service({ 'POST /v1/subscriptions': subscribed('s-1') }) })

  expect(await call($, 'subscribe', PR)).toEqual({ ok: true, subscription: 's-1', flow: FLOW, last: null })
  expect(w.requests).toEqual([
    { method: 'POST', url: 'http://nowcast/v1/subscriptions', body: JSON.stringify(PR), socketPath: SOCKET },
  ])
  // No value yet: nothing to write.
  expect(w.flowWrites(FLOW)).toEqual([])
})

test("the flow's last value in the answer is written to the state", plugins, async ($, on) => {
  const last = update(FLOW, '7', { number: 1 })
  const w = world(on, { answer: service({ 'POST /v1/subscriptions': subscribed('s-1', last) }) })

  expect(await call($, 'subscribe', PR)).toEqual({ ok: true, subscription: 's-1', flow: FLOW, last })
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

  await call($, 'subscribe', PR)
  id = '9'
  await call($, 'subscribe', PR)
  // Ids from one counter, compared as numbers: 10 comes after 9.
  id = '10'
  await call($, 'subscribe', PR)
  expect(w.flowWrites(FLOW).map(f => f.last?.id)).toEqual(['8', '9', '10'])
})

test('a request before the session is declared declares it and goes again', plugins, async ($, on) => {
  let declared = false
  const w = world(on, {
    answer: service({
      'PUT /v1/session': () => {
        declared = true
        return {}
      },
      'POST /v1/subscriptions': () =>
        declared
          ? subscribed('s-1')
          : { status: 428, text: '{"error":{"code":"session_required","message":"Declare the session."}}' },
    }),
  })

  expect(await call($, 'subscribe', PR)).toMatchObject({ ok: true, subscription: 's-1' })
  expect(w.requests.map(r => `${r.method} ${r.url} ${r.body ?? ''}`)).toEqual([
    `POST http://nowcast/v1/subscriptions ${JSON.stringify(PR)}`,
    'PUT http://nowcast/v1/session {"id":"session-1"}',
    `POST http://nowcast/v1/subscriptions ${JSON.stringify(PR)}`,
  ])
})

test("the service's error comes back as a failure", plugins, async ($, on) => {
  world(on, {
    answer: service({
      'POST /v1/subscriptions': {
        status: 400,
        text: '{"error":{"code":"invalid_params","message":"branch is required"}}',
      },
    }),
  })

  expect(await call($, 'subscribe', PR)).toEqual({
    ok: false,
    status: 400,
    code: 'invalid_params',
    message: 'branch is required',
  })
})

test('an answer outside the contract is a failure', plugins, async ($, on) => {
  world(on, {
    answer: service({ 'POST /v1/subscriptions': { status: 201, text: '{}' }, 'POST /v1/flows/x/refresh': { status: 500 } }),
  })

  expect(await call($, 'subscribe', PR)).toEqual({ ok: false, status: 201, code: 'bad_answer', message: 'unexpected answer: {}' })
  expect(await call($, 'refresh', { flow: 'x' })).toEqual({
    ok: false,
    status: 500,
    code: 'unexpected_status',
    message: 'the service answered 500',
  })
})

test('a service that does not answer is a failure', plugins, async ($, on) => {
  const w = world(on, { answer: () => 'connection refused' })
  expect(await call($, 'refresh', { flow: FLOW })).toEqual({
    ok: false,
    status: null,
    code: 'unreachable',
    message: `cannot reach the service at ${SOCKET}: nowcast: $.http.fetch: connection refused`,
  })
  expect(w.requests.length).toBe(1)
})

test('without a socket path, nothing is sent', plugins, async ($, on) => {
  const w = world(on, { env: { XDG_RUNTIME_DIR: 'relative' } })
  expect(await call($, 'refresh', { flow: FLOW })).toEqual({
    ok: false,
    status: null,
    code: 'no_socket',
    message: 'neither NOWCAST_SOCKET nor an absolute XDG_RUNTIME_DIR is set',
  })
  expect(w.requests).toEqual([])
})

test('refresh, last and unsubscribe reach the flow and the subscription', plugins, async ($, on) => {
  const last = update(FLOW, '5', { number: 1 })
  const w = world(on, {
    answer: service({
      [`POST /v1/flows/${ENCODED}/refresh`]: { status: 202 },
      [`GET /v1/flows/${ENCODED}`]: { status: 200, text: JSON.stringify(last) },
      'GET /v1/flows/forge%2Fquiet': { status: 204 },
      'DELETE /v1/subscriptions/s-1': { status: 204 },
    }),
  })

  expect(await call($, 'refresh', { flow: FLOW })).toEqual({ ok: true })
  expect(await call($, 'last', { flow: FLOW })).toEqual({ ok: true, last })
  expect(await call($, 'last', { flow: 'forge/quiet' })).toEqual({ ok: true, last: null })
  expect(await call($, 'unsubscribe', { subscription: 's-1' })).toEqual({ ok: true })
  // The value read is written to the state, as the stream would have.
  expect(w.flowWrites(FLOW)).toEqual([{ last, error: null, ended: false }])
})

test('a flow this process is not subscribed to is a failure', plugins, async ($, on) => {
  world(on, {
    answer: service({
      [`GET /v1/flows/${ENCODED}`]: { status: 404, text: '{"error":{"code":"unknown_flow","message":"no"}}' },
    }),
  })
  expect(await call($, 'last', { flow: FLOW })).toEqual({ ok: false, status: 404, code: 'unknown_flow', message: 'no' })
})

/** A service that forgets every subscription when `restart` is called. */
function restartable() {
  let next = 1
  let known = new Set<string>()
  const answer = service({
    'POST /v1/subscriptions': () => {
      // The brick publishes again on each new subscription.
      const id = `s-${next}`
      known.add(id)
      return subscribed(id, update(FLOW, String(next++), { number: 1 }))
    },
    [`GET /v1/flows/${ENCODED}`]: () =>
      known.size > 0 ? { status: 204 } : { status: 404, text: '{"error":{"code":"unknown_flow","message":"no"}}' },
    'DELETE /v1/subscriptions/s-1': () => (known.delete('s-1') ? { status: 204 } : { status: 404 }),
    'DELETE /v1/subscriptions/s-2': () => (known.delete('s-2') ? { status: 204 } : { status: 404 }),
  })
  return {
    answer,
    known: () => [...known],
    restart: () => {
      known = new Set()
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
  // The value the new subscription answered is written.
  expect(w.flowWrites(FLOW).map(f => f.last?.id)).toEqual(['1', '2'])
  // The mod still holds the first id, which ends the subscription made again.
  expect(await call($, 'unsubscribe', { subscription: 's-1' })).toEqual({ ok: true })
  expect(svc.known()).toEqual([])
  expect(w.requests.at(-1)?.url).toBe('http://nowcast/v1/subscriptions/s-2')
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
  expect(w.requests.filter(r => r.method === 'POST').length).toBe(1)
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
  await call($, 'unsubscribe', { subscription: 's-1' })

  svc.restart()
  w.child().retrying('lost')
  w.child().connected()
  await w.clock.settle()

  expect(svc.known()).toEqual([])
  expect(w.requests.filter(r => r.method === 'POST').length).toBe(1)
})
