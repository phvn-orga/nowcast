// The service going and coming back. `nowcast watch` reconnects by itself,
// resuming with `Last-Event-ID`; the mod reports it, and starts the command
// again only when it exits.

import { expect, test } from 'claude-code/testing'
import { START, world } from './world'

test('a lost service shows as down, then up once the command is back', async ($, on) => {
  const w = world(on)
  await $.session.start(START)
  await w.clock.settle()

  w.child().stderr('nowcast: cannot reach the service at /x: connection refused; retrying\n')
  await w.clock.settle()
  expect(w.stream()).toEqual({
    session: 'session-1',
    status: 'down',
    reconnections: 0,
    error: 'cannot reach the service at /x: connection refused',
  })

  w.child().stderr('nowcast: connected\n')
  await w.clock.settle()
  expect(w.stream()).toEqual({ session: 'session-1', status: 'up', reconnections: 1, error: null })

  // The command reconnected itself: the same process, no new declaration.
  expect(w.children.length).toBe(1)
  expect(w.requests.length).toBe(1)
})

test('a service absent at start is waited for', async ($, on) => {
  const w = world(on, { answer: () => 'connect ENOENT' })
  await $.session.start(START)
  await w.clock.settle()
  expect(w.stream()?.status).toBe('starting')
  expect(w.stream()?.error).toBe('cannot reach the service at /run/user/1000/nowcast/nowcast.sock: nowcast: $.http.fetch: connect ENOENT')
  expect(w.child().running).toBe(true)

  w.child().stderr('nowcast: cannot reach the service: no such file; retrying\nnowcast: connected\n')
  await w.clock.settle()
  expect(w.stream()).toEqual({ session: 'session-1', status: 'up', reconnections: 1, error: null })
})

test('a command that exits is started again, later each time', async ($, on) => {
  const w = world(on)
  await $.session.start(START)
  await w.clock.settle()

  w.child().stderr('{"error":{"code":"unexpected_response","message":"unexpected answer"}}\n')
  w.child().exit(4)
  await w.clock.settle()
  expect(w.stream()?.status).toBe('stopped')
  expect(w.stream()?.error).toBe('nowcast watch exited (status 4): unexpected answer')

  await w.clock.advance(999)
  expect(w.children.length).toBe(1)
  await w.clock.advance(1)
  expect(w.children.length).toBe(2)
  expect(w.child().session).toBe('session-1')

  w.child().exit(1)
  await w.clock.advance(1_999)
  expect(w.children.length).toBe(2)
  await w.clock.advance(1)
  expect(w.children.length).toBe(3)
})

test('a command that ran a while is started again at once', async ($, on) => {
  const w = world(on)
  await $.session.start(START)
  await w.clock.settle()
  w.child().exit(1)
  await w.clock.advance(1_000)
  await w.clock.advance(60_000)
  w.child().exit(1)
  await w.clock.advance(1_000)

  expect(w.children.length).toBe(3)
})

test('a command not installed is reported and tried again', async ($, on) => {
  const w = world(on, { spawnFails: true })
  await $.session.start(START)
  await w.clock.settle()

  expect(w.stream()?.status).toBe('stopped')
  expect(w.stream()?.error).toBe('cannot run nowcast: nowcast: $.process.spawn: nowcast: not found')
  const writes = w.writes.length
  await w.clock.advance(1_000)
  expect(w.writes.length).toBeGreaterThan(writes)
})

test('a command the mod stopped is not started again', async ($, on) => {
  const w = world(on)
  await $.session.start(START)
  await w.clock.settle()
  w.session.id = 'session-2'
  await $.turn.start({ text: '', turnId: 't-1' })
  await w.clock.advance(120_000)

  expect(w.children.map(c => [c.session, c.running])).toEqual([
    ['session-1', false],
    ['session-2', true],
  ])
})
