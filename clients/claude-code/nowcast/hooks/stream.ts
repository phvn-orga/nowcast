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
 * Reads one stdout line: `{"event": <name>, "data": <data>}`. Null for a line
 * that is not one of the three events of the contract, or lacks its flow.
 */
export function parseEvent(line: string): StreamEvent | null {
  let parsed: unknown
  try {
    parsed = JSON.parse(line)
  } catch {
    return null
  }
  if (!isObject(parsed) || !isObject(parsed.data)) return null
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
 * changes nothing: an update seen already, which a new stream sends again.
 */
export function nextFlow(previous: NowcastFlow | undefined, event: StreamEvent): NowcastFlow | null {
  const last = previous?.last ?? null
  switch (event.event) {
    case 'update':
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

/** What a stderr line of `watch` says about the connection. */
export type Connection = { status: 'up' } | { status: 'down'; reason: string } | null

/**
 * `nowcast: connected` once the service is back, `nowcast: <reason>;
 * retrying` when it is lost. Anything else says nothing about it.
 */
export function parseConnection(line: string): Connection {
  const text = line.replace(/^nowcast: /, '')
  if (text === line) return null
  if (text === 'connected') return { status: 'up' }
  const reason = text.match(/^(.*); retrying$/)?.[1]
  return reason === undefined ? null : { status: 'down', reason }
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}
