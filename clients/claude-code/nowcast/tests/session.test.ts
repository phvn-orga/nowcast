// The session as the service sees it: declared first, declared again on
// each change, one stream per session (contract section 3, "Session").

import { expect, test } from 'claude-code/testing'
import { SOCKET, START, update, world } from './world'

const RESUME = { id: 'session-1' }

test('the session is declared before the stream opens', async ($, on) => {
  const w = world(on)
  await $.session.start(START)
  await w.clock.settle()

  expect(w.requests).toEqual([
    { method: 'PUT', url: 'http://nowcast/v1/session', body: '{"id":"session-1"}', socketPath: SOCKET },
  ])
  expect(w.children.map(c => c.argv)).toEqual([['nowcast', '--json', '--session', 'session-1', 'watch']])
  expect(w.stream()).toEqual({ session: 'session-1', status: 'up', reconnections: 0, error: null })
})

test('NOWCAST_SOCKET names the socket, as for the command', async ($, on) => {
  const w = world(on, { env: { NOWCAST_SOCKET: '/tmp/x/nowcast.sock', XDG_RUNTIME_DIR: '/run/user/1000' } })
  await $.session.start(START)
  await w.clock.settle()

  expect(w.requests.map(r => r.socketPath)).toEqual(['/tmp/x/nowcast.sock'])
})

test('without a socket path, the stream still starts and the state says why', async ($, on) => {
  const w = world(on, { env: { XDG_RUNTIME_DIR: 'relative' } })
  await $.session.start(START)
  await w.clock.settle()

  expect(w.requests).toEqual([])
  expect(w.children.length).toBe(1)
  expect(w.stream()).toEqual({
    session: 'session-1',
    status: 'starting',
    reconnections: 0,
    error: 'neither NOWCAST_SOCKET nor an absolute XDG_RUNTIME_DIR is set',
  })
})

test('a refused declaration is reported', async ($, on) => {
  const w = world(on, { answer: () => ({ status: 400, text: '{"error":{}}' }) })
  await $.session.start(START)
  await w.clock.settle()

  expect(w.stream()?.error).toBe('PUT /v1/session answered 400: {"error":{}}')
})

for (const reason of ['clear', 'resume'] as const) {
  test(`after a ${reason}, the new session is declared and the stream keeps delivering`, async ($, on) => {
    const w = world(on)
    await $.session.start(START)
    await w.clock.settle()
    const first = w.child()
    first.event('update', update('probe/x', '1', { n: 1 }))
    await w.clock.settle()

    await $.session.end({ reason, sessionId: 'session-1', resume: RESUME })
    w.session.id = 'session-2'
    await w.clock.advance(50)

    expect(w.requests.map(r => r.body)).toEqual(['{"id":"session-1"}', '{"id":"session-2"}'])
    expect(first.killed).toBe(true)
    const second = w.child()
    expect(second.session).toBe('session-2')
    expect(w.stream()?.session).toBe('session-2')

    // The new stream sends the last value again: the new session's state,
    // which starts empty, gets it.
    second.event('update', update('probe/x', '1', { n: 1 }))
    second.event('update', update('probe/x', '2', { n: 2 }))
    await w.clock.settle()
    expect(w.flowWrites('probe/x').map(f => f.last?.data)).toEqual([{ n: 1 }, { n: 1 }, { n: 2 }])

    // Subscriptions follow the process, not the session: nothing subscribes again.
    expect(w.requests.every(r => r.url === 'http://nowcast/v1/session')).toBe(true)
  })
}

test('the new id is awaited until it shows', async ($, on) => {
  const w = world(on)
  await $.session.start(START)
  await w.clock.settle()

  await $.session.end({ reason: 'clear', sessionId: 'session-1', resume: RESUME })
  await w.clock.advance(500)
  expect(w.children.length).toBe(1)
  w.session.id = 'session-2'
  await w.clock.advance(50)

  expect(w.children.map(c => c.session)).toEqual(['session-1', 'session-2'])
})

test('an exit ends nothing early: the stream stays until the process goes', async ($, on) => {
  const w = world(on)
  await $.session.start(START)
  await w.clock.settle()

  await $.session.end({ reason: 'prompt_input_exit', sessionId: 'session-1', resume: RESUME })
  await w.clock.advance(10_000)

  expect(w.children.length).toBe(1)
  expect(w.child().running).toBe(true)
  expect(w.requests.length).toBe(1)
})

test('a turn under another session id declares it', async ($, on) => {
  const w = world(on)
  await $.session.start(START)
  await w.clock.settle()

  await $.turn.start({ text: 'hi', turnId: 't-1' })
  await w.clock.settle()
  expect(w.children.length).toBe(1)

  w.session.id = 'session-2'
  await $.turn.start({ text: 'hi', turnId: 't-2' })
  await w.clock.settle()
  expect(w.children.map(c => c.session)).toEqual(['session-1', 'session-2'])
  expect(w.children[0]?.killed).toBe(true)
})

test('a second start under the same session changes nothing', async ($, on) => {
  const w = world(on)
  await $.session.start(START)
  await w.clock.settle()
  await $.session.start(START)
  await w.clock.settle()

  expect(w.children.length).toBe(1)
  expect(w.requests.length).toBe(1)
})
