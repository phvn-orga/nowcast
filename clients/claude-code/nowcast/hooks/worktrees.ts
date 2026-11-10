// The worktrees this session writes in (`05-brique-claude-code.md` on the
// design branch, "Les worktrees de la session"). A session anchored in one
// worktree may write in others by their path: a consumer that follows the
// session's directory would show the wrong branch. Each `Edit` or `Write`
// that succeeded, of the main loop or a subagent, marks the worktree of its
// file; reads and Bash commands do not count.
//
// The list lives in this mod's state (`worktrees`), whether or not the
// claude-code brick is enabled, and goes out with the session's measures.
// It belongs to the session: a `/clear` starts it over, a reload of this mod
// finds it in the state.

import type { EngineInterface, On } from 'claude-code'
import type { NowcastWorktree } from '../types'
import { write } from './flows'

type $ = EngineInterface

const WORKTREES = { plugin: 'nowcast', key: 'worktrees' } as const
/** The most worktrees kept: past it, the one written in longest ago leaves. */
export const MAX_WORKTREES = 16
const GIT_TIMEOUT_MS = 5_000

/** A directory's worktree root and common git directory; null outside a worktree. */
type Place = { root: string; repo: string } | null

/** What git answered for each directory seen. */
const places = new Map<string, Place>()

/** The list, for the session it belongs to. */
let held: { session: string; list: NowcastWorktree[] } | undefined

// Writes are applied one at a time, in the order the calls ended.
let queue: Promise<void> = Promise.resolve()

export function registerWorktrees(on: On): void {
  on('tool.call', { tool: 'Edit' }, async ($, e, next) => {
    const result = await next(e)
    if (result.deny === undefined && result.isError !== true) await record($, e.file_path)
    return result
  })
  on('tool.call', { tool: 'Write' }, async ($, e, next) => {
    const result = await next(e)
    if (result.deny === undefined && result.isError !== true) await record($, e.file_path)
    return result
  })
}

/** Queues a write to `file`, dated now; the tool's answer does not wait for git. */
async function record($: $, file: string): Promise<void> {
  const at = new Date(await $.clock.now()).toISOString()
  queue = queue
    .then(() => wrote($, file, at))
    .catch(error => $.ui.log(`nowcast: cannot record the worktree of ${file}: ${message(error)}`, { to: 'debug' }))
}

/** The session's worktrees, most recent write first. */
export async function worktrees($: $): Promise<NowcastWorktree[]> {
  const session = await $.session.id()
  if (held === undefined) {
    // This mod loaded: the state holds the session's list from before.
    const { value } = await $.state.get(WORKTREES)
    held = { session, list: Array.isArray(value) ? value : [] }
  } else if (held.session !== session) {
    // A `/clear` or a resume, which also empties the state.
    held = { session, list: [] }
  }
  return held.list
}

async function wrote($: $, file: string, at: string): Promise<void> {
  if (!file.startsWith('/')) return
  const place = await placeOf($, file.slice(0, file.lastIndexOf('/')) || '/')
  if (place === null) return
  const [branch, remote, list] = await Promise.all([
    git($, place.root, ['symbolic-ref', '--quiet', '--short', 'HEAD']),
    git($, place.root, ['config', '--get', 'remote.origin.url']),
    worktrees($),
  ])
  const others: NowcastWorktree[] = []
  for (const worktree of list) {
    if (worktree.path === place.root) continue
    // A worktree removed since leaves the list.
    if (await $.fs.exists(worktree.path).catch(() => true)) others.push(worktree)
  }
  const next = [{ path: place.root, repo: place.repo, branch, remote, written_at: at }, ...others].slice(
    0,
    MAX_WORKTREES,
  )
  if (held !== undefined) held.list = next
  await write($, () => $.state.set(WORKTREES, next), 'worktrees')
}

/** The worktree a directory belongs to, asked of git once per directory. */
async function placeOf($: $, dir: string): Promise<Place> {
  const known = places.get(dir)
  if (known !== undefined) return known
  const out = await git($, dir, ['rev-parse', '--path-format=absolute', '--show-toplevel', '--git-common-dir'])
  const [root, repo] = out?.split('\n') ?? []
  const place = root && repo ? { root, repo } : null
  places.set(dir, place)
  return place
}

/** A git command's output, trimmed; null when it fails or prints nothing. */
async function git($: $, cwd: string, args: string[]): Promise<string | null> {
  const answer = await $.process.run(['git', ...args], { cwd, timeoutMs: GIT_TIMEOUT_MS }).catch(() => undefined)
  if (answer === undefined || answer.exitCode !== 0) return null
  const out = answer.stdout.trim()
  return out === '' ? null : out
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
