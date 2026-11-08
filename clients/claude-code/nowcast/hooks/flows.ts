// What this mod writes about each flow into its state: the stream's events,
// and the last value a request's answer gave. It is the only writer, and a
// read through the `$` of a long-lived loop would answer the moment that
// loop's dispatch began: what it last wrote is kept here.

import type { EngineInterface } from 'claude-code'
import type { NowcastEvent, NowcastFlow } from '../types'
import { type StreamEvent, isNewer, nextFlow } from './stream'

type $ = EngineInterface

const FLOWS = { plugin: 'nowcast', key: 'flows' } as const

const flows = new Map<string, NowcastFlow>()

/** Forgets what was written: a new session starts with an empty state. */
export function forgetFlows(): void {
  flows.clear()
}

/** Applies one event of the stream to its flow. */
export async function apply($: $, event: StreamEvent): Promise<void> {
  const next = nextFlow(flows.get(event.flow), event)
  if (next === null) return
  await setFlow($, event.flow, next)
}

/**
 * A flow's last value from a request's answer: the stream does not send it,
 * so it is written here, unless the state holds it or a newer one.
 */
export async function offer($: $, last: NowcastEvent): Promise<void> {
  const previous = flows.get(last.subject)
  if (previous?.last && !previous.ended && !isNewer(last, previous.last)) return
  await apply($, { event: 'update', flow: last.subject, update: last })
}

/**
 * A flow's last value as the service answered a subscription. With none, a
 * value an earlier subscription left ended is cleared: it is not this one's.
 */
export async function answered($: $, flow: string, last: NowcastEvent | null): Promise<void> {
  if (last !== null) return offer($, last)
  if (flows.get(flow)?.ended) await setFlow($, flow, { last: null, error: null, ended: false })
}

async function setFlow($: $, flow: string, value: NowcastFlow): Promise<void> {
  flows.set(flow, value)
  await write($, () => $.state.set({ ...FLOWS, id: flow }, value), `flow ${flow}`)
}

/**
 * A write to the state. One another mod refuses (its `state.set` hook may)
 * is logged and skipped: it must not end the stream, which would only replay
 * the same value into the same refusal.
 */
export async function write($: $, set: () => Promise<unknown>, what: string): Promise<void> {
  try {
    await set()
  } catch (error) {
    $.ui.log(`nowcast: cannot write ${what}: ${error instanceof Error ? error.message : String(error)}`, {
      to: 'debug',
    })
  }
}
