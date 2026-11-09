// The toast that says to run `nowcast setup`, from what `nowcast status
// --json` tells of the systemd units.

import { expect, test } from 'claude-code/testing'
import { setupWarning } from '../hooks/setup'
import { START, world } from './world'

const status = (setup: unknown) => JSON.stringify({ installed: '0.1.0', running: null, setup })

test('missing or outdated units call for `nowcast setup`, anything else for nothing', () => {
  expect(setupWarning(status('missing'))).toBe('systemd does not start the nowcast service yet: run `nowcast setup`')
  expect(setupWarning(status('outdated'))).toBe("the nowcast service's systemd units are outdated: run `nowcast setup`")
  expect(setupWarning(status('current'))).toBeNull()
  expect(setupWarning(status(null))).toBeNull()
  // A binary older than the field.
  expect(setupWarning(JSON.stringify({ installed: '0.1.0', running: null }))).toBeNull()
  expect(setupWarning('not json')).toBeNull()
})

test('the session start shows the toast once when the units are outdated', async ($, on) => {
  const w = world(on, { status: status('outdated') })
  await $.session.start(START)
  await w.clock.settle()
  expect(w.toasts).toEqual(["the nowcast service's systemd units are outdated: run `nowcast setup`"])

  await $.turn.start({ text: 'hi', turnId: 't-1' })
  await w.clock.settle()
  expect(w.toasts.length).toBe(1)
})

test('units in place show no toast', async ($, on) => {
  const w = world(on)
  await $.session.start(START)
  await w.clock.settle()
  expect(w.toasts).toEqual([])
})
