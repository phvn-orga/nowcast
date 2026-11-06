// The service going and coming back. `nowcast watch` reconnects by itself,
// resuming with `Last-Event-ID`, and says so on stdout; the mod reports it,
// and starts the command again only when it exits.

import { expect, test } from 'claude-code/testing'
import { NOT_ON_PATH } from '../hooks/register'
import { START, update, world } from './world'

const ENV = { XDG_RUNTIME_DIR: '/run/user/1000' }

const RESUME = { id: 'session-1' }

test('a lost service shows as down, then up once the command is back', async ($, on) => {
  const w = world(on)
  await $.session.start(START)
  await w.clock.settle()

  w.child().connected()
  w.child().retrying('cannot reach the service at /x: connection refused')
  await w.clock.settle()
  expect(w.stream()).toEqual({
    session: 'session-1',
    status: 'down',
    error: 'cannot reach the service at /x: connection refused',
  })

  // The text on stderr is for people: the mod reads the status on stdout.
  w.child().stderr('nowcast: connected\n')
  await w.clock.settle()
  expect(w.stream()?.status).toBe('down')
  w.child().connected()
  await w.clock.settle()
  expect(w.stream()).toEqual({ session: 'session-1', status: 'up', error: null })

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

  w.child().retrying('cannot reach the service: no such file')
  w.child().connected()
  await w.clock.settle()
  expect(w.stream()).toEqual({ session: 'session-1', status: 'up', error: null })
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
  // It printed no update: nothing to resume from.
  expect(w.child().lastEventId).toBeUndefined()

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

test('a command not on PATH is reported as such, and tried again', async ($, on) => {
  // A relative entry is not looked into.
  const env = { ...ENV, PATH: '/usr/bin:bin:/opt/nowcast/' }
  const w = world(on, { spawnFails: true, env, files: ['bin/nowcast'] })
  await $.session.start(START)
  await w.clock.settle()

  expect(w.stream()?.status).toBe('stopped')
  expect(w.stream()?.error).toBe(NOT_ON_PATH)
  const writes = w.writes.length
  await w.clock.advance(1_000)
  expect(w.writes.length).toBeGreaterThan(writes)
})

test('a command on PATH that cannot start keeps its error', async ($, on) => {
  const env = { ...ENV, PATH: '/usr/bin:/home/u/.local/bin' }
  const w = world(on, { spawnFails: true, env, files: ['/home/u/.local/bin/nowcast'] })
  await $.session.start(START)
  await w.clock.settle()

  expect(w.stream()?.status).toBe('stopped')
  expect(w.stream()?.error).toBe('cannot run nowcast: nowcast: $.process.spawn: nowcast: not found')
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

test('a command started again shows as up once it connects at the first try', async ($, on) => {
  const w = world(on)
  await $.session.start(START)
  await w.clock.settle()
  w.child().connected()
  w.child().exit(1)
  await w.clock.settle()
  expect(w.stream()?.status).toBe('stopped')

  await w.clock.advance(1_000)
  w.child().connected()
  await w.clock.settle()
  expect(w.stream()).toEqual({ session: 'session-1', status: 'up', error: null })
})

test('a command started again resumes after the last update it printed', async ($, on) => {
  const w = world(on)
  await $.session.start(START)
  await w.clock.settle()
  w.child().connected()
  w.child().event('update', update('probe/x', '7', { n: 7 }))
  w.child().event('update', update('probe/y', '9', { n: 9 }))
  w.child().event('flow-error', { flow: 'probe/x', message: 'source down' })
  w.child().exit(1)
  await w.clock.advance(1_000)

  expect(w.child().argv).toEqual([
    'nowcast', '--json', '--session', 'session-1', 'watch', '--last-event-id', '9',
  ])

  // One that printed nothing keeps the cursor for the next.
  w.child().exit(1)
  await w.clock.advance(2_000)
  expect(w.children.map(c => c.lastEventId)).toEqual([undefined, '9', '9'])
})

for (const reason of ['clear', 'resume'] as const) {
  // Each clock step costs about a second of real time.
  const slow = { timeoutMs: 10_000 }
  test(`after a ${reason}, the new session's command starts with every last value`, slow, async ($, on) => {
    const w = world(on)
    await $.session.start(START)
    await w.clock.settle()
    w.child().event('update', update('probe/x', '5', { n: 5 }))
    await w.clock.settle()

    await $.session.end({ reason, sessionId: 'session-1', resume: RESUME })
    w.session.id = 'session-2'
    await w.clock.advance(50)
    expect(w.child().argv).toEqual(['nowcast', '--json', '--session', 'session-2', 'watch'])

    // The cursor of the session left behind is not carried over a crash
    // either; the new session's own is.
    w.child().exit(1)
    await w.clock.advance(1_000)
    w.child().event('update', update('probe/x', '6', { n: 6 }))
    w.child().exit(1)
    await w.clock.advance(2_000)
    expect(w.children.map(c => [c.session, c.lastEventId])).toEqual([
      ['session-1', undefined],
      ['session-2', undefined],
      ['session-2', undefined],
      ['session-2', '6'],
    ])
  })
}
