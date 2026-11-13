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
type Held = { session: string; list: NowcastWorktree[] }

/** The list being held: one load from the state, shared by every caller. */
let held: Promise<Held> | undefined

// Writes are applied one at a time, in the order the calls ended.
let queue: Promise<void> = Promise.resolve()

export function registerWorktrees(on: On): void {
  on('tool.call', { tool: 'Edit' }, async ($, e, next) => marked($, e.file_path, await next(e)))
  on('tool.call', { tool: 'Write' }, async ($, e, next) => marked($, e.file_path, await next(e)))
}

/** Records `file` when the call that wrote it succeeded; the call's result unchanged. */
function marked<R extends { deny?: string; isError?: true }>($: $, file: string, result: R): R {
  if (result.deny === undefined && result.isError !== true) record($, file)
  return result
}

/**
 * Queues a write to `file`, dated now and tied to the session it ran in; the
 * tool's answer waits for nothing. The queue is taken at once, so it keeps
 * the order the calls ended.
 */
function record($: $, file: string): void {
  const asked = Promise.all([$.clock.now(), $.session.id()])
  // Handled when its turn comes, below.
  asked.catch(() => undefined)
  queue = queue
    .then(async () => {
      const [now, session] = await asked
      await wrote($, file, new Date(now).toISOString(), session)
    })
    .catch(error => $.ui.log(`nowcast: cannot record the worktree of ${file}: ${message(error)}`, { to: 'debug' }))
}

/** The session's worktrees, most recent write first. */
export async function worktrees($: $): Promise<NowcastWorktree[]> {
  return (await hold($)).list
}

/** The list of the current session, loaded once per load of this mod. */
async function hold($: $): Promise<Held> {
  const session = await $.session.id()
  const current = held
  if (current === undefined) {
    // This mod loaded: the state holds the session's list from before.
    const loading = $.state.get(WORKTREES).then(({ value }) => ({ session, list: Array.isArray(value) ? value : [] }))
    held = loading
    // A failed read is tried again by the next caller.
    loading.catch(() => {
      if (held === loading) held = undefined
    })
    return loading
  }
  const known = await current
  if (known.session === session) return known
  // A `/clear` or a resume, which also empties the state. Another caller may
  // have started the new session's list meanwhile.
  if (held !== current) return hold($)
  held = Promise.resolve({ session, list: [] })
  return held
}

async function wrote($: $, file: string, at: string, session: string): Promise<void> {
  if (!file.startsWith('/')) return
  const place = await placeOf($, file.slice(0, file.lastIndexOf('/')) || '/')
  if (place === null) return
  const [branch, remote, mine] = await Promise.all([
    git($, place.root, ['symbolic-ref', '--quiet', '--short', 'HEAD']),
    git($, place.root, ['config', '--get', 'remote.origin.url']),
    hold($),
  ])
  // The session of the call ended before its turn: its list is gone.
  if (mine.session !== session) return
  // A worktree removed since leaves the list.
  const kept = await Promise.all(
    mine.list
      .filter(worktree => worktree.path !== place.root)
      .map(async worktree => ((await $.fs.exists(worktree.path).catch(() => true)) ? worktree : undefined)),
  )
  const others = kept.filter((worktree): worktree is NowcastWorktree => worktree !== undefined)
  const next = [{ path: place.root, repo: place.repo, branch, remote, written_at: at }, ...others].slice(
    0,
    MAX_WORKTREES,
  )
  mine.list = next
  // The session ended meanwhile: the new one's state is not this list's.
  if ((await $.session.id()) !== session) return
  await write($, () => $.state.set(WORKTREES, next), 'worktrees')
}

/** The worktree a directory belongs to, asked of git once per directory. */
async function placeOf($: $, dir: string): Promise<Place> {
  const known = places.get(dir)
  if (known !== undefined) return known
  const out = await git($, dir, ['rev-parse', '--path-format=absolute', '--show-toplevel', '--git-common-dir'])
  const [root, repo] = out?.split('\n') ?? []
  // A git before 2.31 echoes `--path-format=absolute` it does not know.
  const place = root?.startsWith('/') && repo?.startsWith('/') ? { root, repo } : null
  places.set(dir, place)
  return place
}

/** A git command's output, trimmed; null when it fails or prints nothing. */
export async function git($: $, cwd: string, args: string[]): Promise<string | null> {
  const answer = await $.process.run(['git', ...args], { cwd, timeoutMs: GIT_TIMEOUT_MS }).catch(() => undefined)
  if (answer === undefined || answer.exitCode !== 0) return null
  const out = answer.stdout.trim()
  return out === '' ? null : out
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
