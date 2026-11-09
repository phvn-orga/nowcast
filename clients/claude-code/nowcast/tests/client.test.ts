// `$.nowcast`, the library other mods use (contract section 3, "Abonnements"
// and "Flux"), called by another mod as any mod would: what every request
// shares, and the requests on a flow. Subscriptions kept: subscriptions.test.ts.

import type { HttpResponse } from 'claude-code'
import { expect, test } from 'claude-code/testing'
import { type Request, SOCKET, call, caller, update, world } from './world'

const FLOW = 'forge/github.com/org/repo#main'
const ENCODED = 'forge%2Fgithub.com%2Forg%2Frepo%23main'
const WANTED = { brick: 'forge', kind: 'pr', params: { remote: 'git@github.com:org/repo.git', branch: 'main' } }
const PR = { name: 'pr', ...WANTED }

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

  expect(await call($, 'subscribe', PR)).toMatchObject({ ok: true, status: 'active' })
  expect(w.requests.map(r => `${r.method} ${r.url} ${r.body ?? ''}`)).toEqual([
    `POST http://nowcast/v1/subscriptions ${JSON.stringify(WANTED)}`,
    'PUT http://nowcast/v1/session {"id":"session-1"}',
    `POST http://nowcast/v1/subscriptions ${JSON.stringify(WANTED)}`,
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
    answer: service({ 'GET /v1/flows/x': { status: 200, text: '{}' }, 'POST /v1/flows/x/refresh': { status: 500 } }),
  })

  expect(await call($, 'last', { flow: 'x' })).toEqual({ ok: false, status: 200, code: 'bad_answer', message: 'unexpected answer: {}' })
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

test('refreshBrick reaches the brick, with its kind when given', plugins, async ($, on) => {
  const w = world(on, { answer: service({ 'POST /v1/bricks/forge/refresh': { status: 202 } }) })

  expect(await call($, 'refreshBrick', { brick: 'forge', kind: 'pr' })).toEqual({ ok: true })
  expect(await call($, 'refreshBrick', { brick: 'forge' })).toEqual({ ok: true })
  const sent = w.requests.filter(r => r.url.endsWith('/v1/bricks/forge/refresh'))
  expect(sent.map(r => r.method)).toEqual(['POST', 'POST'])
  expect(JSON.parse(sent[0]?.body ?? 'null')).toEqual({ kind: 'pr' })
  expect(sent[1]?.body ?? '').toBe('')
})

test('refresh, input and last reach the flow', plugins, async ($, on) => {
  const last = update(FLOW, '5', { number: 1 })
  const w = world(on, {
    answer: service({
      [`POST /v1/flows/${ENCODED}/refresh`]: { status: 202 },
      [`POST /v1/flows/${ENCODED}/input`]: { status: 202 },
      [`GET /v1/flows/${ENCODED}`]: { status: 200, text: JSON.stringify(last) },
      'GET /v1/flows/forge%2Fquiet': { status: 204 },
    }),
  })

  expect(await call($, 'refresh', { flow: FLOW })).toEqual({ ok: true })
  expect(await call($, 'input', { flow: FLOW, detail: { n: 1 } })).toEqual({ ok: true })
  const sent = w.requests.find(r => r.url.endsWith('/input'))
  expect(sent?.method).toBe('POST')
  expect(JSON.parse(sent?.body ?? 'null')).toEqual({ n: 1 })
  expect(await call($, 'last', { flow: FLOW })).toEqual({ ok: true, last })
  expect(await call($, 'last', { flow: 'forge/quiet' })).toEqual({ ok: true, last: null })
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

test('history reads the events after an id, and leaves the state as is', plugins, async ($, on) => {
  const events = [update(FLOW, '6', { number: 2 }), update(FLOW, '7', { number: 3 })]
  const w = world(on, {
    answer: service({
      [`GET /v1/flows/${ENCODED}?after=5`]: { status: 200, text: JSON.stringify({ gap: false, events }) },
      [`GET /v1/flows/${ENCODED}?after=0`]: { status: 200, text: JSON.stringify({ gap: true, events: [] }) },
      [`GET /v1/flows/${ENCODED}?after=x`]: {
        status: 400,
        text: '{"error":{"code":"invalid_request","message":"not an event id"}}',
      },
    }),
  })

  expect(await call($, 'history', { flow: FLOW, after: '5' })).toEqual({ ok: true, gap: false, events })
  expect(await call($, 'history', { flow: FLOW, after: '0' })).toEqual({ ok: true, gap: true, events: [] })
  expect(await call($, 'history', { flow: FLOW, after: 'x' })).toEqual({
    ok: false,
    status: 400,
    code: 'invalid_request',
    message: 'not an event id',
  })
  expect(w.flowWrites(FLOW)).toEqual([])
})

test('a history outside the contract is a bad answer', plugins, async ($, on) => {
  world(on, {
    answer: service({ [`GET /v1/flows/${ENCODED}?after=0`]: { status: 200, text: '{"gap":false,"events":[1]}' } }),
  })
  const answer = await call($, 'history', { flow: FLOW, after: '0' })
  expect(answer).toMatchObject({ ok: false, status: 200, code: 'bad_answer' })
})
