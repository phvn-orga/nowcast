// The sample mod of `example/` beside this mod, over a fake service and fake
// `nowcast watch` processes: it receives its flow through the library and
// asks for a refresh.

import { expect, test } from 'claude-code/testing'
import { register } from '../example/hooks/register'
import { COMPOSER, SOCKET, START, update, world } from './world'

const FLOW = 'probe/nowcast-example'
const SUBSCRIBE = { brick: 'probe', kind: 'item', params: { id: 'nowcast-example' } }
// Each clock settle takes about a second of real time.
const nowcast = { plugins: [{ name: 'nowcast-example', register }], timeoutMs: 15_000 }

/**
 * The service: `428` until a session is declared, then the subscription,
 * refreshes, and `204` to the rest.
 */
function service(last: unknown = null) {
  let declared = false
  return (request: { method: string; url: string }) => {
    if (request.url === 'http://nowcast/v1/session') {
      declared = true
      return {}
    }
    if (!declared) return { status: 428, text: '{"error":{"code":"session_required","message":"Declare it."}}' }
    if (request.method === 'POST' && request.url === 'http://nowcast/v1/subscriptions') {
      return { status: 201, text: JSON.stringify({ subscription: 's-1', flow: FLOW, last }) }
    }
    if (request.url === 'http://nowcast/v1/flows/probe%2Fnowcast-example/refresh') return { status: 202 }
    return {}
  }
}

test('the sample mod receives its flow through the library and asks for a refresh', nowcast, async ($, on) => {
  const w = world(on, { answer: service() })
  await $.session.start(START)
  await w.clock.settle()

  // It subscribed through `$.nowcast`, on the socket the session is declared
  // on, whichever of the two mods asked first.
  expect(w.requests.every(r => r.socketPath === SOCKET)).toBe(true)
  expect(w.requests.filter(r => r.method === 'PUT').map(r => r.body)).toContain('{"id":"session-1"}')
  expect(w.requests.filter(r => r.method === 'POST').map(r => r.body)).toContain(JSON.stringify(SUBSCRIBE))
  expect(w.statuses.at(-1)).toBe('nowcast example: no value yet')

  // The stream the nowcast mod holds delivers the flow; the sample mod shows it.
  w.child().event('update', update(FLOW, '1', { id: 'nowcast-example', refresh: 1 }))
  await w.clock.settle()
  expect(w.statuses.at(-1)).toBe('nowcast example: {"id":"nowcast-example","refresh":1}')

  const { text } = await $.command.run({ command: 'nowcast-refresh', args: '', ...COMPOSER })
  expect(text).toBe('nowcast example: refresh asked.')
  expect(w.requests.at(-1)).toMatchObject({ method: 'POST', url: 'http://nowcast/v1/flows/probe%2Fnowcast-example/refresh' })

  w.child().event('update', update(FLOW, '2', { id: 'nowcast-example', refresh: 2 }))
  await w.clock.settle()
  expect(w.statuses.at(-1)).toBe('nowcast example: {"id":"nowcast-example","refresh":2}')

  // Errors and the end of the flow show too.
  w.child().event('flow-error', { flow: FLOW, message: 'brick crashed' })
  await w.clock.settle()
  expect(w.statuses.at(-1)).toBe('nowcast example: error: brick crashed')
  w.child().event('flow-ended', { flow: FLOW })
  await w.clock.settle()
  expect(w.statuses.at(-1)).toBe('nowcast example: ended')
})

test('a value the flow had before the subscription shows at once', nowcast, async ($, on) => {
  const w = world(on, { answer: service(update(FLOW, '4', { id: 'nowcast-example', refresh: 4 })) })
  await $.session.start(START)
  await w.clock.settle()

  expect(w.statuses.at(-1)).toBe('nowcast example: {"id":"nowcast-example","refresh":4}')
  expect(w.flow(FLOW)?.last?.id).toBe('4')
})

test('after a /clear, the value given again to the new session shows', nowcast, async ($, on) => {
  const w = world(on, { answer: service() })
  await $.session.start(START)
  await w.clock.settle()
  w.child().event('update', update(FLOW, '1', { id: 'nowcast-example', refresh: 1 }))
  await w.clock.settle()

  await $.session.end({ reason: 'clear', sessionId: 'session-1', resume: { id: 'session-1' } })
  w.session.id = 'session-2'
  await w.clock.advance(50)
  w.statuses.length = 0
  w.child().event('update', update(FLOW, '1', { id: 'nowcast-example', refresh: 1 }))
  await w.clock.settle()

  expect(w.child().session).toBe('session-2')
  expect(w.statuses).toEqual(['nowcast example: {"id":"nowcast-example","refresh":1}'])
})

test('a refused subscription is shown, and refresh says so', nowcast, async ($, on) => {
  const w = world(on, {
    answer: request =>
      request.url === 'http://nowcast/v1/subscriptions'
        ? { status: 404, text: '{"error":{"code":"unknown_brick","message":"no brick probe"}}' }
        : {},
  })
  // Declared or not, the service refuses the brick.
  await $.session.start(START)
  await w.clock.settle()

  expect(w.statuses.at(-1)).toBe('nowcast example: no brick probe')
  const { text } = await $.command.run({ command: 'nowcast-refresh', args: '', ...COMPOSER })
  expect(text).toBe('nowcast example: not subscribed.')
})
