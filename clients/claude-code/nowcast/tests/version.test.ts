// The oldest service the mod accepts (`02-clients.md` on the design branch):
// read from `nowcast --version` before each `watch`.

import { expect, test } from 'claude-code/testing'
import { OLDEST_SERVICE, isOlder, printedVersion } from '../hooks/version'
import { START, type WorldOptions, call, caller, world } from './world'

test('the printed version is read, and anything else is not', () => {
  expect(printedVersion('nowcast 0.1.0-alpha.2\n')).toBe('0.1.0-alpha.2')
  expect(printedVersion('nowcast 1.2.3+build.5')).toBe('1.2.3+build.5')
  expect(printedVersion('nowcast 1.2')).toBeNull()
  expect(printedVersion('other 1.2.3')).toBeNull()
  expect(printedVersion('')).toBeNull()
})

test('versions compare by semver precedence', () => {
  // In the order semver 11.4 gives, each before the next.
  const ordered = [
    '0.1.0-alpha.1',
    '0.1.0-alpha.2',
    '0.1.0-alpha.10',
    '0.1.0-alpha.beta',
    '0.1.0-beta',
    '0.1.0-beta.2',
    '0.1.0-rc.1',
    '0.1.0',
    '0.1.1',
    '0.2.0',
    '0.10.0',
    '1.0.0',
  ]
  for (let i = 0; i < ordered.length - 1; i++) {
    const [a, b] = [ordered[i] as string, ordered[i + 1] as string]
    expect(isOlder(a, b)).toBe(true)
    expect(isOlder(b, a)).toBe(false)
  }
  expect(isOlder('0.1.0-alpha.2', '0.1.0-alpha.2')).toBe(false)
  // Build metadata does not count.
  expect(isOlder('1.0.0+a', '1.0.0')).toBe(false)
})

const TOO_OLD = `nowcast 0.1.0-alpha.1 is older than ${OLDEST_SERVICE}, the oldest this mod accepts; upgrade it`

test('a service too old gets nothing, and the stream stays stopped until it is upgraded', async ($, on) => {
  const options: WorldOptions = { version: '0.1.0-alpha.1' }
  const w = world(on, options)
  await $.session.start(START)
  await w.clock.settle()

  // Not even the session's declaration, and never `up`.
  expect(w.requests).toEqual([])
  expect(w.children).toEqual([])
  expect(w.writes.filter(x => x.key === 'stream').map(x => (x.value as { status: string }).status)).toEqual(['stopped'])
  expect(w.stream()).toEqual({ session: 'session-1', status: 'stopped', error: TOO_OLD })

  // Upgraded during the session: the next attempt declares it and starts `watch`.
  options.version = OLDEST_SERVICE
  await w.clock.advance(1_000)
  expect(w.requests.map(r => `${r.method} ${r.url}`)).toEqual(['PUT http://nowcast/v1/session'])
  expect(w.children.length).toBe(1)
  w.child().connected()
  await w.clock.settle()
  expect(w.stream()?.status).toBe('up')
})

test('while the service is too old, requests answer so and subscriptions wait', { plugins: [caller] }, async ($, on) => {
  const options: WorldOptions = {
    version: '0.1.0-alpha.1',
    answer: request =>
      request.url.endsWith('/v1/subscriptions')
        ? { status: 201, text: JSON.stringify({ subscription: 's-1', flow: 'probe/x', last: null }) }
        : {},
  }
  const w = world(on, options)
  await $.session.start(START)
  await w.clock.settle()

  const refused = { ok: false, status: null, code: 'service_too_old', message: TOO_OLD }
  expect(await call($, 'refresh', { flow: 'probe/x' })).toEqual(refused)
  expect(await call($, 'last', { flow: 'probe/x' })).toEqual(refused)
  const item = { name: 'item', brick: 'probe', kind: 'item', params: { id: 'x' } }
  expect(await call($, 'subscribe', item)).toMatchObject({
    ok: true,
    status: 'retrying',
    error: { code: 'service_too_old', message: TOO_OLD },
  })
  expect(w.requests).toEqual([])

  // Upgraded: once the stream opens, the subscription is made.
  options.version = OLDEST_SERVICE
  await w.clock.advance(1_000)
  w.child().connected()
  await w.clock.settle()
  expect(w.subscription('caller/item')).toMatchObject({ status: 'active', flow: 'probe/x' })
})

test('a service at the oldest version accepted, or newer, starts `watch`', async ($, on) => {
  const w = world(on, { version: '9.0.0' })
  await $.session.start(START)
  await w.clock.settle()
  expect(w.children.length).toBe(1)
})

test('a session left while its version is read gets no `watch`', async ($, on) => {
  let read = () => {}
  const w = world(on, { versionRead: new Promise<void>(resolve => (read = resolve)) })
  await $.session.start(START)
  await w.clock.settle()

  w.session.id = 'session-2'
  await $.turn.start({ text: 'hi', turnId: 't-1' })
  await w.clock.settle()
  read()
  await w.clock.settle()

  expect(w.children.map(c => c.session)).toEqual(['session-2'])
})
