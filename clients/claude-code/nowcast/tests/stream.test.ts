// The session's stream redistributed through the state: the three events of
// contract section 3 ("Le flux d'événements"), as `nowcast watch --json`
// prints them.

import { expect, test } from 'claude-code/testing'
import { START, observer, update, world } from './world'
import type { World } from './world'
import type { On } from 'claude-code'
import type { Engine } from 'claude-code/testing'

async function started($: Engine, on: On): Promise<World> {
  const w = world(on)
  await $.session.start(START)
  await w.clock.settle()
  return w
}

test('each update is written under its flow id', async ($, on) => {
  const w = await started($, on)
  w.child().event('update', update('probe/x', '1', { n: 1 }))
  w.child().event('update', update('probe/a/b#c d', '2', { n: 2 }))
  w.child().event('update', update('probe/x', '3', { n: 3 }))
  await w.clock.settle()

  expect(w.flowWrites('probe/x')).toEqual([
    { last: update('probe/x', '1', { n: 1 }), error: null, ended: false },
    { last: update('probe/x', '3', { n: 3 }), error: null, ended: false },
  ])
  expect(w.flow('probe/a/b#c d')?.last).toEqual(update('probe/a/b#c d', '2', { n: 2 }))
})

test('other mods follow the flows and read them', { plugins: [observer] }, async ($, on) => {
  const w = await started($, on)
  w.child().event('update', update('probe/x', '1', { n: 1 }))
  await w.clock.settle()
  await $.turn.start({ text: '', turnId: 't-1' })

  expect(w.observed).toEqual(['set probe/x {"n":1}', 'get probe/x {"n":1}'])
})

test('a value seen already is not written again', async ($, on) => {
  const w = await started($, on)
  w.child().event('update', update('probe/x', '1', { n: 1 }))
  w.child().event('update', update('probe/x', '1', { n: 1 }))
  await w.clock.settle()

  expect(w.flowWrites('probe/x').length).toBe(1)
})

test('a flow error keeps the last value until the next update', async ($, on) => {
  const w = await started($, on)
  w.child().event('update', update('probe/x', '1', { n: 1 }))
  w.child().event('flow-error', { flow: 'probe/x', message: 'source down' })
  await w.clock.settle()
  expect(w.flow('probe/x')).toEqual({ last: update('probe/x', '1', { n: 1 }), error: 'source down', ended: false })

  w.child().event('update', update('probe/x', '1', { n: 1 }))
  await w.clock.settle()
  expect(w.flow('probe/x')).toEqual({ last: update('probe/x', '1', { n: 1 }), error: null, ended: false })
})

test('an error on a flow with no value yet', async ($, on) => {
  const w = await started($, on)
  w.child().event('flow-error', { flow: 'probe/y', message: 'no such item' })
  await w.clock.settle()

  expect(w.flow('probe/y')).toEqual({ last: null, error: 'no such item', ended: false })
})

test('an ended flow says so, and comes back with a later update', async ($, on) => {
  const w = await started($, on)
  w.child().event('update', update('probe/x', '1', { n: 1 }))
  w.child().event('flow-ended', { flow: 'probe/x' })
  await w.clock.settle()
  expect(w.flow('probe/x')).toEqual({ last: update('probe/x', '1', { n: 1 }), error: null, ended: true })

  w.child().event('update', update('probe/x', '1', { n: 1 }))
  await w.clock.settle()
  expect(w.flow('probe/x')?.ended).toBe(false)
})

test('an event split across pieces is read whole', async ($, on) => {
  const w = await started($, on)
  const line = `${JSON.stringify({ event: 'update', data: update('probe/x', '1', { text: 'é'.repeat(50) }) })}\n`
  w.child().stdout(line.slice(0, 40))
  await w.clock.settle()
  expect(w.flowWrites('probe/x')).toEqual([])
  w.child().stdout(line.slice(40))
  await w.clock.settle()

  expect(w.flow('probe/x')?.last?.data).toEqual({ text: 'é'.repeat(50) })
})

test('lines outside the contract are skipped', async ($, on) => {
  const w = await started($, on)
  w.child().stdout('not json\n')
  w.child().event('heartbeat', {})
  w.child().event('update', { id: '1' })
  w.child().event('flow-ended', {})
  w.child().stdout('[1,2]\n\n')
  w.child().event('update', update('probe/x', '1', { n: 1 }))
  await w.clock.settle()

  expect(w.writes.filter(write => write.key === 'flows').map(write => write.id)).toEqual(['probe/x'])
  expect(w.child().running).toBe(true)
})
