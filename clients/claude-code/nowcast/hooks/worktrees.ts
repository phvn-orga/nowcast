// The worktrees this session works in (`05-brique-claude-code.md` on the
// design branch, "Les worktrees de la session"). A session anchored in one
// worktree may work in others by their path: a consumer that follows the
// session's directory would show the wrong branch. Each `Edit` or `Write`
// that succeeded, of the main loop, a subagent or a workflow's agent, marks
// the worktree of its file; each Bash call that ran, failed or not, marks the
// worktree of each directory it runs in; reads do not count. Each entry keeps
// which kinds of loop worked in it (`writers`), so that a consumer can tell a
// worktree only a workflow's agents worked in.
//
// The worktree holding the session's directory is the active entry, listed
// as soon as the directory moves into it, as a work of the main loop. The
// directory is read again after EnterWorktree, ExitWorktree and the main
// loop's Bash calls, and at the start and the end of each of its turns:
// `CwdChanged` may not reach this mod.
//
// Each entry carries its forge key, where `git push` sends its branch
// (`02-clients.md`), read again at each work in it and after a command of
// the agent that may move a branch (`actions.ts`); `branches.ts` follows
// the forge flows of these keys.
//
// The list lives in this mod's state (`worktrees`), whether or not the
// claude-code brick is enabled, and goes out with the session's measures.
// It belongs to the session: a `/clear` starts it over, a reload of this mod
// finds it in the state.

import type { EngineInterface, On } from 'claude-code'
import type { NowcastWorktree, NowcastWriter } from '../types'
import { branchMoves, commandDirs, isUrl } from './actions'
import { followForge } from './branches'
import { write } from './flows'

type $ = EngineInterface

const WORKTREES = { plugin: 'nowcast', key: 'worktrees' } as const
/** The most worktrees kept: past it, the one worked in longest ago leaves, never the active one. */
export const MAX_WORKTREES = 16
const GIT_TIMEOUT_MS = 5_000

/** A directory's worktree root, common git directory and own git directory; null outside a worktree. */
type Place = { root: string; repo: string; gitDir: string } | null
type Found = NonNullable<Place>

/** What git answered for each directory seen in a worktree. */
const places = new Map<string, Place>()

/**
 * What git last answered of a worktree, but `@{push}`, with the stamp of the
 * files those answers rest on (`stampOf`): the same stamp, the same answers.
 */
type Read = {
  stamp: string | undefined
  files: string[]
  branch: string | null
  remote: string | null
  /** The remote `git push` sends the branch to, its name or its URL; null on a detached HEAD. */
  push: string | null
  pushUrl: string | null
}

/** The last reading of each worktree, by root. */
const reads = new Map<string, Read>()

/** The list, for the session it belongs to. */
type Held = { session: string; list: NowcastWorktree[] }

/** The list being held: one load from the state, shared by every caller. */
let held: Promise<Held> | undefined

// Writes are applied one at a time, in the order the calls ended.
let queue: Promise<void> = Promise.resolve()

/** The kinds of writer, in the order an entry lists them. */
const WRITERS: readonly NowcastWriter[] = ['main', 'subagent', 'workflow']

/**
 * The loops of a workflow, by `agentId`: those its script's `agent()`
 * started, and those started from one of them, at any depth.
 */
const workflowAgents = new Set<string>()

export function registerWorktrees(on: On): void {
  on('agent.spawn', async ($, e, next) => {
    const result = await next(e)
    const ofWorkflow = e.workflow !== undefined || (e.parentAgentId !== undefined && workflowAgents.has(e.parentAgentId))
    if (ofWorkflow && result.agentId !== undefined) workflowAgents.add(result.agentId)
    return result
  })
  on('tool.call', { tool: 'Edit' }, async ($, e, next) => marked($, e.file_path, e.agentId, await next(e)))
  on('tool.call', { tool: 'Write' }, async ($, e, next) => marked($, e.file_path, e.agentId, await next(e)))
  on('tool.call', { tool: 'Bash' }, async ($, e, next) => {
    // Read before the command runs: a `cd` in it moves the session. Inside
    // the hook, a subagent's own directory.
    const cwd = await $.session.cwd().catch(() => undefined)
    const result = await next(e)
    // A command denied never ran. One that failed ran all the same, and may
    // have switched before it did.
    if (result.deny !== undefined) return result
    record($, commandDirs(e.command, cwd), writerOf(e.agentId))
    const moved = branchMoves(e.command, cwd)
    if (moved.length > 0) reread($, moved)
    if (e.agentId === undefined) locate($)
    return result
  })
  on('tool.call', { tool: 'EnterWorktree' }, async ($, e, next) => located($, e.agentId, await next(e)))
  on('tool.call', { tool: 'ExitWorktree' }, async ($, e, next) => located($, e.agentId, await next(e)))
}

/**
 * Queues a check that each worktree of the list still exists: one removed by
 * anything other than a write or a command seen (`wt remove`, which removes in
 * the background, a terminal, another session) leaves. Called at the start and
 * the end of each turn of the main thread, whose hooks live in `register.ts`
 * and `measures.ts`: a mod registers an event once without a matcher.
 */
export function checkWorktrees($: $): void {
  reread($, [])
  locate($)
}

/**
 * Records the worktree of `file` when the call that wrote it succeeded, with
 * the kind of loop `agentId` names; the call's result unchanged.
 */
function marked<R extends { deny?: string; isError?: true }>($: $, file: string, agentId: string | undefined, result: R): R {
  if (result.deny === undefined && result.isError !== true && file.startsWith('/')) {
    record($, [file.slice(0, file.lastIndexOf('/')) || '/'], writerOf(agentId))
  }
  return result
}

/** Reads the session's directory again after a call of the main loop; the call's result unchanged. */
function located<R>($: $, agentId: string | undefined, result: R): R {
  if (agentId === undefined) locate($)
  return result
}

/** The kind of loop: none is main, one of a workflow is the workflow's, any other a subagent. */
function writerOf(agentId: string | undefined): NowcastWriter {
  if (agentId === undefined) return 'main'
  return workflowAgents.has(agentId) ? 'workflow' : 'subagent'
}

/**
 * Queues a work in the worktree of each directory of `dirs`, in that order,
 * dated now and tied to the session it ran in; the tool's answer waits for
 * nothing. The queue is taken at once, so it keeps the order the calls ended.
 */
function record($: $, dirs: string[], writer: NowcastWriter): void {
  if (dirs.length === 0) return
  const asked = Promise.all([$.clock.now(), $.session.id()])
  // Handled when its turn comes, below.
  asked.catch(() => undefined)
  queue = queue
    .then(async () => {
      const [now, session] = await asked
      for (const dir of dirs) {
        const place = dir.startsWith('/') ? await livePlaceOf($, dir) : null
        if (place !== null) await worked($, place, new Date(now).toISOString(), session, writer, false)
      }
    })
    .catch(error => $.ui.log(`nowcast: cannot record the worktree of ${dirs.join(', ')}: ${message(error)}`, { to: 'debug' }))
}

/**
 * Queues a new reading of the session's directory: the worktree holding it
 * becomes the active entry, a work of the main loop when it was not; none is
 * active outside a worktree. Read at once, the hook's directory being the
 * session's on the main loop.
 */
function locate($: $): void {
  const asked = Promise.all([$.clock.now(), $.session.id(), $.session.cwd()])
  // Handled when its turn comes, below.
  asked.catch(() => undefined)
  queue = queue
    .then(async () => {
      const [now, session, cwd] = await asked
      const place = await livePlaceOf($, cwd)
      if (place !== null) return worked($, place, new Date(now).toISOString(), session, 'main', true)
      const mine = await hold($)
      if (mine.session !== session || !mine.list.some(worktree => worktree.active)) return
      await save($, mine, session, mine.list.map(worktree => ({ ...worktree, active: false })))
    })
    .catch(error => $.ui.log(`nowcast: cannot read the session's directory: ${message(error)}`, { to: 'debug' }))
}

/**
 * Queues a new reading of the worktrees under `dirs`, any of them for an
 * undefined one: their branch and key. Every worktree gone leaves, under
 * `dirs` or not. Order and dates stay.
 */
function reread($: $, dirs: Array<string | undefined>): void {
  const asked = $.session.id()
  // Handled when its turn comes, below.
  asked.catch(() => undefined)
  queue = queue
    .then(async () => {
      const session = await asked
      const mine = await hold($)
      if (mine.session !== session || mine.list.length === 0) return
      const under = (path: string) => dirs.some(dir => dir === undefined || dir === path || dir.startsWith(`${path}/`))
      const read = await Promise.all(
        mine.list.map(async worktree => {
          if (!(await $.fs.exists(worktree.path).catch(() => true))) return undefined
          return under(worktree.path) ? { ...worktree, ...(await describe($, worktree.path)) } : worktree
        }),
      )
      const next = read.filter((worktree): worktree is NowcastWorktree => worktree !== undefined)
      if (JSON.stringify(next) === JSON.stringify(mine.list)) return
      mine.list = next
      if ((await $.session.id()) !== session) return
      await write($, () => $.state.set(WORKTREES, next), 'worktrees')
      followForge($, next)
    })
    .catch(error => $.ui.log(`nowcast: cannot read the worktrees again: ${message(error)}`, { to: 'debug' }))
}

/** The session's worktrees, most recent work first. */
export async function worktrees($: $): Promise<NowcastWorktree[]> {
  return (await hold($)).list
}

/** The list of the current session, loaded once per load of this mod. */
async function hold($: $): Promise<Held> {
  const session = await $.session.id()
  const current = held
  if (current === undefined) {
    // This mod loaded: the state holds the session's list from before.
    // An entry an older mod wrote has no `active`.
    const loading = $.state.get(WORKTREES).then(({ value }) => ({
      session,
      list: Array.isArray(value) ? value.map(worktree => ({ ...worktree, active: worktree.active === true })) : [],
    }))
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

/**
 * A work in the worktree at `place`: its entry goes first, dated `at`, with
 * `writer` among its writers. `activate` makes it the active entry, and does
 * nothing when it already is.
 */
async function worked($: $, place: Found, at: string, session: string, writer: NowcastWriter, activate: boolean): Promise<void> {
  const mine = await hold($)
  // The session of the call ended before its turn: its list is gone.
  if (mine.session !== session) return
  const before = mine.list.find(worktree => worktree.path === place.root)
  if (activate && before?.active === true) return
  const described = await describe($, place.root)
  // A worktree removed since leaves the list.
  const kept = await Promise.all(
    mine.list
      .filter(worktree => worktree.path !== place.root)
      .map(async worktree => ((await $.fs.exists(worktree.path).catch(() => true)) ? worktree : undefined)),
  )
  const others = kept
    .filter((worktree): worktree is NowcastWorktree => worktree !== undefined)
    .map(worktree => (activate && worktree.active ? { ...worktree, active: false } : worktree))
  const { branch, remote, forge } = described
  const written: NowcastWorktree = {
    path: place.root,
    repo: place.repo,
    branch,
    remote,
    written_at: at,
    forge,
    active: activate || before?.active === true,
  }
  // An entry an older mod wrote does not say who wrote in it: it stays so.
  if (before === undefined || before.writers !== undefined) {
    written.writers = WRITERS.filter(kind => kind === writer || before?.writers?.includes(kind))
  }
  await save($, mine, session, capped([written, ...others]))
}

/** `list` within `MAX_WORKTREES`: the entry worked in longest ago leaves, never the active one. */
function capped(list: NowcastWorktree[]): NowcastWorktree[] {
  if (list.length <= MAX_WORKTREES) return list
  const dropped = list.findLast(worktree => !worktree.active)
  return capped(list.filter(worktree => worktree !== dropped))
}

/** Holds `next` as the list of `mine`, and writes it unless the session ended meanwhile. */
async function save($: $, mine: Held, session: string, next: NowcastWorktree[]): Promise<void> {
  mine.list = next
  // The session ended meanwhile: the new one's state is not this list's.
  if ((await $.session.id()) !== session) return
  await write($, () => $.state.set(WORKTREES, next), 'worktrees')
  followForge($, next)
}

/**
 * What git says of a worktree now: its branch, its `origin` and its forge key.
 * The answers but `@{push}` come from the last reading while the files they
 * rest on are unchanged: one git command instead of up to seven.
 */
async function describe($: $, root: string): Promise<Pick<NowcastWorktree, 'branch' | 'remote' | 'forge'>> {
  const place = await placeOf($, root)
  const known = reads.get(root)
  const same = known?.stamp !== undefined && place !== null && (await stampOf($, place, known.files)) === known.stamp
  const read = same && known !== undefined ? known : await readGit($, root)
  const forge =
    read.branch === null || read.push === null || read.pushUrl === null
      ? null
      : { remote: read.pushUrl, branch: await pushedName($, root, read.push, read.branch) }
  return { branch: read.branch, remote: read.remote, forge }
}

/** Asks git what `describe` keeps of a worktree, and keeps it with its stamp. */
async function readGit($: $, root: string): Promise<Read> {
  // Asked again: a worktree recreated at the same root may have another git directory.
  places.delete(root)
  const place = await placeOf($, root)
  // Stamped before git answers: a change meanwhile shows at the next stamp.
  const files = place === null ? [] : await configFiles($, place)
  const stamp = place === null ? undefined : await stampOf($, place, files)
  const [branch, remote] = await Promise.all([
    git($, root, ['symbolic-ref', '--quiet', '--short', 'HEAD']),
    git($, root, ['config', '--get', 'remote.origin.url']),
  ])
  const push = branch === null ? null : await pushRemote($, root, branch)
  const read: Read = { stamp, files, branch, remote, push, pushUrl: push === null ? null : await remoteUrl($, root, push) }
  reads.set(root, read)
  return read
}

/**
 * The name `git push` gives `branch` from `dir` on the remote `push` (`02-clients.md` on
 * the design branch): `@{push}`, `<remote>/<branch>`; when git does not
 * resolve it, as before a first push with `push.default` at `simple`, the
 * same name. Asked each time: it also rests on the remote-tracking branch,
 * which a push or a fetch creates or deletes without touching a stamped file.
 */
async function pushedName($: $, dir: string, push: string, branch: string): Promise<string> {
  const pushed = await git($, dir, ['rev-parse', '--abbrev-ref', '--symbolic-full-name', '@{push}'])
  // `@{push}` names the remote git pushes to, `push`.
  const name = pushed?.startsWith(`${push}/`) ? pushed.slice(push.length + 1) : ''
  return name === '' ? branch : name
}

/**
 * What a worktree's answers but `@{push}` rest on: its `HEAD`, the reftable
 * that holds it in that format (where the file stays `.invalid`), and the
 * config files git reads there, a missing one included; and where the root's
 * `.git` leads, which a worktree recreated there with another git directory
 * changes. Undefined when `HEAD` cannot be read: nothing is then kept.
 */
async function stampOf($: $, place: Found, files: string[]): Promise<string | undefined> {
  const [head, link] = await Promise.all([
    $.fs.read(`${place.gitDir}/HEAD`).catch(() => undefined),
    // A file names the git directory; a directory, or none, reads the same each time.
    $.fs.read(`${place.root}/.git`).catch(() => '-'),
  ])
  if (typeof head !== 'string') return undefined
  const stats = await Promise.all(
    [`${place.gitDir}/reftable/tables.list`, ...files].map(file =>
      $.fs.stat(file).then(
        stat => `${stat.mtimeMs}:${stat.size}`,
        () => '-',
      ),
    ),
  )
  return [head, link, ...stats].join('\n')
}

/**
 * The config files git reads in a worktree, existing or not yet: those it
 * reports, the repository's and the worktree's own, the global and system
 * ones, and the targets of `include.path` and `includeIf.<condition>.path`. An include that
 * applies only under a condition is listed once git reads it: a change that
 * meets the condition changes a file already listed.
 */
async function configFiles($: $, place: Found): Promise<string[]> {
  const [home, xdg] = await Promise.all([
    $.env.get('HOME').catch(() => undefined),
    $.env.get('XDG_CONFIG_HOME').catch(() => undefined),
  ])
  const files = new Set([`${place.repo}/config`, `${place.gitDir}/config.worktree`, '/etc/gitconfig'])
  if (home !== undefined) files.add(`${home}/.gitconfig`)
  // Git takes an empty `XDG_CONFIG_HOME` as unset.
  const config = xdg !== undefined && xdg !== '' ? xdg : home === undefined ? undefined : `${home}/.config`
  if (config !== undefined) files.add(`${config}/git/config`)
  const answer = await $.process
    .run(['git', 'config', '--list', '--show-origin', '-z'], { cwd: place.root, timeoutMs: GIT_TIMEOUT_MS })
    .catch(() => undefined)
  const fields = answer?.exitCode === 0 ? answer.stdout.split('\0') : []
  // Each entry is its origin, then `key\nvalue`; a path git gives relative is the worktree's.
  for (let i = 0; i + 1 < fields.length; i += 2) {
    const origin = fields[i] as string
    if (!origin.startsWith('file:')) continue
    const file = origin.startsWith('file:/') ? origin.slice(5) : `${place.root}/${origin.slice(5)}`
    files.add(file)
    const entry = fields[i + 1] as string
    const cut = entry.indexOf('\n')
    if (cut < 0 || !/^include(if\..*)?\.path$/i.test(entry.slice(0, cut))) continue
    const path = entry.slice(cut + 1)
    const target = path.startsWith('~/')
      ? home === undefined
        ? undefined
        : `${home}${path.slice(1)}`
      : path.startsWith('/')
        ? path
        : `${file.slice(0, file.lastIndexOf('/'))}/${path}`
    if (target !== undefined) files.add(target)
  }
  return [...files].sort()
}

/**
 * The push URL of the remote `git push` sends `dir`'s branch to: its
 * `pushRemote`, else `remote.pushDefault`, else its upstream's remote, else
 * `origin` (`02-clients.md` on the design branch).
 */
export async function pushUrl($: $, dir: string): Promise<string | null> {
  const branch = await git($, dir, ['symbolic-ref', '--quiet', '--short', 'HEAD'])
  return remoteUrl($, dir, await pushRemote($, dir, branch))
}

/** The name of the remote `git push` sends `branch` to, or its URL when given as one. */
async function pushRemote($: $, dir: string, branch: string | null): Promise<string> {
  const config = (key: string) => git($, dir, ['config', '--get', key])
  return (
    (branch === null ? null : await config(`branch.${branch}.pushRemote`)) ??
    (await config('remote.pushDefault')) ??
    (branch === null ? null : await config(`branch.${branch}.remote`)) ??
    'origin'
  )
}

/** A remote's push URL; a remote may be given as its URL. */
function remoteUrl($: $, dir: string, name: string): Promise<string | null> {
  return isUrl(name) ? Promise.resolve(name) : git($, dir, ['remote', 'get-url', '--push', name])
}

/** The worktree a directory belongs to, asked of git once per directory in one. */
async function placeOf($: $, dir: string): Promise<Place> {
  const known = places.get(dir)
  if (known !== undefined) return known
  const out = await git($, dir, [
    'rev-parse',
    '--path-format=absolute',
    '--show-toplevel',
    '--git-common-dir',
    '--git-dir',
  ])
  const [root, repo, gitDir] = out?.split('\n') ?? []
  // A git before 2.31 echoes `--path-format=absolute` it does not know.
  const place =
    root?.startsWith('/') && repo?.startsWith('/') && gitDir?.startsWith('/') ? { root, repo, gitDir } : null
  // A directory outside git is asked again: a `git init` there makes it a worktree.
  if (place !== null) places.set(dir, place)
  return place
}

/**
 * The worktree a directory belongs to, null when its root is gone: `placeOf`
 * keeps git's answer, and a command run in a removed worktree must not bring
 * it back.
 */
async function livePlaceOf($: $, dir: string): Promise<Place> {
  const place = await placeOf($, dir)
  return place !== null && (await $.fs.exists(place.root).catch(() => true)) ? place : null
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
