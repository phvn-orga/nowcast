// A sample mod built on the nowcast mod. It subscribes to one flow through
// `$.nowcast`, follows that flow in the nowcast mod's state, shows its last
// value in the status line, and adds a `/nowcast-refresh` command that asks
// the brick to publish again. Its manifest lists `nowcast` under
// `dependencies`, which types `$.nowcast` and the nowcast mod's state here.
//
// It subscribes to the `item` kind of the `probe` brick, the test brick of
// `crates/test-brick`, which publishes on each refresh. A mod for another
// brick changes the subscription below.
//
// Everything sits inside `register`, so that the nowcast mod's tests can load
// this module beside it as it is.

import type { Register } from 'claude-code'

export const register: Register = on => {
  const subscription = { brick: 'probe', kind: 'item', params: { id: 'nowcast-example' } }
  // The flow subscribed to, once the service answered.
  let flow: string | undefined

  /** A flow as the nowcast mod's state holds it, for the status line. */
  const describe = (value: { last: { data: unknown } | null; error: string | null; ended: boolean }) =>
    `nowcast example: ${
      value.ended
        ? 'ended'
        : value.error !== null
          ? `error: ${value.error}`
          : value.last === null
            ? 'no value yet'
            : JSON.stringify(value.last.data)
    }`

  on('session.start', async ($, e, next) => {
    const started = await next(e)
    await $.command.register({ name: 'nowcast-refresh', description: 'Ask the nowcast example flow to publish again' })
    // Not awaited: the session does not wait for the service.
    void $.nowcast.subscribe(subscription).then(async answer => {
      if (!answer.ok) return $.ui.status(`nowcast example: ${answer.message}`)
      flow = answer.flow
      // The value the flow already had is in the state now; null if none.
      const { value } = await $.state.get({ plugin: 'nowcast', key: 'flows', id: answer.flow })
      $.ui.status(describe(value ?? { last: null, error: null, ended: false }))
    })
    return started
  })

  // The nowcast mod writes each flow it receives to its state; this hook sees
  // every write, the value given again after a `/clear` included.
  on('state.set', { plugin: 'nowcast', key: 'flows' }, async ($, e, next) => {
    const result = await next(e)
    if (e.id === flow) $.ui.status(describe(e.value))
    return result
  })

  on('command.run', { command: 'nowcast-refresh' }, async $ => {
    if (flow === undefined) return { text: 'nowcast example: not subscribed.' }
    const answer = await $.nowcast.refresh({ flow })
    return { text: answer.ok ? 'nowcast example: refresh asked.' : `nowcast example: ${answer.message}` }
  })
}
