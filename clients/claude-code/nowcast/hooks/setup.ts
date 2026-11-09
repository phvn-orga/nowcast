// Whether `nowcast setup` has to run: `nowcast status --json` tells whether
// the systemd units in place are the ones the installed binary writes
// (`02-clients.md` on the design branch). Missing or outdated, the session
// says so once, in a toast. A binary too old to tell says nothing.

import type { EngineInterface } from 'claude-code'
import { CLI } from './client'

type $ = EngineInterface

/** How long `nowcast status` has to answer. */
const STATUS_TIMEOUT_MS = 5_000

/** How long the toast stays: long enough to read the command. */
const TOAST_MS = 15_000

/** Shows a toast when `nowcast status` says the units need `nowcast setup`. */
export async function warnSetup($: $): Promise<void> {
  const ran = await $.process.run([CLI, 'status', '--json'], { timeoutMs: STATUS_TIMEOUT_MS }).catch(() => null)
  if (ran?.exitCode !== 0) return
  const warning = setupWarning(ran.stdout)
  if (warning !== null) $.ui.toast(warning, { timeoutMs: TOAST_MS })
}

/** What to tell from `nowcast status --json` output, or null when nothing. */
export function setupWarning(stdout: string): string | null {
  let status: unknown
  try {
    status = JSON.parse(stdout)
  } catch {
    return null
  }
  const setup = typeof status === 'object' && status !== null ? (status as { setup?: unknown }).setup : undefined
  if (setup === 'missing') return `systemd does not start the ${CLI} service yet: run \`${CLI} setup\``
  if (setup === 'outdated') return `the ${CLI} service's setup is outdated: run \`${CLI} setup\` again`
  return null
}
