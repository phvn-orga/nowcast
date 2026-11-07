// What `nowcast --json watch` prints, read without touching the engine.

import type { NowcastEvent, NowcastFlow } from '../types'

/** Splits text that arrives in pieces into whole lines. */
export class Lines {
  private rest = ''

  /** The lines `text` completes; an unfinished one waits for the next piece. */
  push(text: string): string[] {
    const lines = (this.rest + text).split('\n')
    this.rest = lines.pop() ?? ''
    return lines.map(line => line.replace(/\r$/, '')).filter(line => line !== '')
  }
}

/** One event of the stream, as `watch --json` prints it on stdout. */
export type StreamEvent =
  | { event: 'update'; flow: string; update: NowcastEvent }
  | { event: 'flow-error'; flow: string; message: string }
  | { event: 'flow-ended'; flow: string }

/**
 * What `watch --json` says on stdout about its connection: `connected` each
 * time the stream opens, the first one included, and `retrying` once per
 * outage.
 */
export type Status = { status: 'connected' } | { status: 'retrying'; reason: string }

/**
 * Reads one stdout line: `{"event": <name>, "data": <data>}` or `{"status":
 * <status>, ...}`. Null for a line that is none of the three events the state
 * follows (`flow-gap` is not one) nor a status, or an event that lacks its
 * flow.
 */
export function parseLine(line: string): StreamEvent | Status | null {
  let parsed: unknown
  try {
    parsed = JSON.parse(line)
  } catch {
    return null
  }
  if (!isObject(parsed)) return null
  if (parsed.status === 'connected') return { status: 'connected' }
  if (parsed.status === 'retrying') return { status: 'retrying', reason: String(parsed.reason ?? '') }
  if (!isObject(parsed.data)) return null
  const data = parsed.data
  switch (parsed.event) {
    case 'update':
      return typeof data.subject === 'string' && typeof data.id === 'string'
        ? { event: 'update', flow: data.subject, update: data as NowcastEvent }
        : null
    case 'flow-error':
      return typeof data.flow === 'string'
        ? { event: 'flow-error', flow: data.flow, message: String(data.message ?? '') }
        : null
    case 'flow-ended':
      return typeof data.flow === 'string' ? { event: 'flow-ended', flow: data.flow } : null
    default:
      return null
  }
}

/**
 * The flow's state once `event` is applied to `previous`, or null when it
 * changes nothing: an update seen already, which a new stream sends again,
 * or one older than the value held, which a replay after an id may send.
 */
export function nextFlow(previous: NowcastFlow | undefined, event: StreamEvent): NowcastFlow | null {
  const last = previous?.last ?? null
  switch (event.event) {
    case 'update':
      if (previous !== undefined && !previous.ended && last !== null && isOlder(event.update, last)) return null
      if (
        previous !== undefined &&
        !previous.ended &&
        previous.error === null &&
        last?.id === event.update.id &&
        last.time === event.update.time
      ) {
        return null
      }
      return { last: event.update, error: null, ended: false }
    case 'flow-error':
      return { last, error: event.message, ended: false }
    case 'flow-ended':
      return { last, error: null, ended: true }
  }
}

const DECIMAL = /^[0-9]+$/

/**
 * Whether `update` is more recent than `than`. Event ids come from one
 * counter for every flow (contract section 3); an id that is not a decimal
 * number is newer whenever it differs.
 */
export function isNewer(update: NowcastEvent, than: NowcastEvent): boolean {
  if (DECIMAL.test(update.id) && DECIMAL.test(than.id)) return BigInt(update.id) > BigInt(than.id)
  return update.id !== than.id
}

/** Whether `update` is less recent than `than`; never for a non-decimal id. */
function isOlder(update: NowcastEvent, than: NowcastEvent): boolean {
  return DECIMAL.test(update.id) && DECIMAL.test(than.id) && BigInt(update.id) < BigInt(than.id)
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}
