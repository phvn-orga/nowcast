// The worktrees the session writes in: in this mod's state, and with the
// session's measures.

import type { On } from 'claude-code'
import { type Engine, expect, test } from 'claude-code/testing'
import type { NowcastWorktree, NowcastWriter } from '../types'
import { START, type World, world } from './world'

const REPO = '/lab/nowcast/.bare'
const REMOTE = 'git@github.com:o/r.git'
const ROOT = (name: string) => `/lab/nowcast/${name}`
const GIT_DIR = (name: string) => `${REPO}/worktrees/${name}`

/**
 * A repository whose worktrees sit under `/lab/nowcast/`, each on the branch
 * of its name unless `branches` says otherwise (null: a detached HEAD), with
 * `origin` for remote and no branch pushed. `git` gives other answers by
 * worktree, then command line. Nothing else is in a repository.
 */
function repository(branches: Record<string, string | null> = {}, git: Record<string, Record<string, string>> = {}) {
  return (args: readonly string[], cwd: string | undefined) => {
    const name = cwd?.match(/^\/lab\/nowcast\/([^/.][^/]*)/)?.[1]
    if (name === undefined) return { exitCode: 128, stdout: '' }
    const line = args.join(' ')
    const given = git[name]?.[line]
    if (given !== undefined) return { exitCode: 0, stdout: `${given}\n` }
    if (args[0] === 'rev-parse' && args[1] === '--path-format=absolute') {
      return { exitCode: 0, stdout: `${ROOT(name)}\n${REPO}\n${GIT_DIR(name)}\n` }
    }
    if (args[0] === 'symbolic-ref') {
      const branch = name in branches ? branches[name] : name
      return branch === null ? { exitCode: 1, stdout: '' } : { exitCode: 0, stdout: `${branch}\n` }
    }
    if (line === 'config --get remote.origin.url' || line === 'remote get-url --push origin') {
      return { exitCode: 0, stdout: `${REMOTE}\n` }
    }
    return { exitCode: 1, stdout: '' }
  }
}

/** The tools beneath the mod: a file under a `fail` directory errors. */
function tools(on: On) {
  on('tool.call', (_, e) => {
    const file = (e as { file_path?: string }).file_path ?? ''
    return (file.includes('/fail/') ? { isError: true, result: 'failed', text: 'failed' } : { result: {} }) as never
  })
}

async function edit($: Engine, file: string, agentId?: string) {
  await $.tool.call({ tool: 'Edit', file_path: file, old_string: 'a', new_string: 'b', agentId })
}

async function writeFile($: Engine, file: string) {
  await $.tool.call({ tool: 'Write', file_path: file, content: '' })
}

/** The list last written, once the writes queued are done. */
async function listed(w: World): Promise<NowcastWorktree[] | undefined> {
  await w.clock.settle()
  return w.writes.filter(write => write.key === 'worktrees').at(-1)?.value as NowcastWorktree[] | undefined
}

const entry = (
  name: string,
  ms: number,
  branch: string | null = name,
  writers: NowcastWriter[] = ['main'],
  active = false,
): NowcastWorktree => ({
  path: ROOT(name),
  repo: REPO,
  branch,
  remote: REMOTE,
  written_at: new Date(ms).toISOString(),
  forge: branch === null ? null : { remote: REMOTE, branch },
  writers,
  active,
})

test('an Edit or a Write marks the worktree of its file, the latest write first', async ($, on) => {
  const w = world(on, { git: repository(), files: [ROOT('main'), ROOT('feature')] })
  tools(on)
  await $.session.start(START)

  await edit($, `${ROOT('main')}/src/a.ts`)
  await w.clock.advance(1_000)
  await writeFile($, `${ROOT('feature')}/b.ts`)
  await w.clock.advance(1_000)
  await edit($, `${ROOT('feature')}/docs/c.md`)

  expect(await listed(w)).toEqual([entry('feature', 1_002_000), entry('main', 1_000_000)])
})

/** The engine beneath the mod starts each agent under its `description` as `agentId`. */
function agents(on: On) {
  on('agent.spawn', (_, e) => ({ model: 'm', agentId: e.description }))
}

/** Starts the agent `agentId` from the loop `parentAgentId`, by a workflow's `agent()` when `workflow` is set. */
async function spawn($: Engine, agentId: string, workflow: boolean, parentAgentId?: string): Promise<void> {
  await $.agent.spawn({
    tool_use_id: 'toolu-1',
    prompt: 'p',
    description: agentId,
    subagentType: 'general-purpose',
    provider: { plugin: 'engine', tier: 'core' },
    parentModel: 'm',
    background: true,
    fork: false,
    parentAgentId,
    ...(workflow ? { workflow: { runId: 'wf_1', agentIndex: 1 } } : {}),
  } as never)
}

test("each entry keeps the kinds of loop that wrote in it: main, a subagent, a workflow's agent", async ($, on) => {
  const w = world(on, { git: repository(), files: [ROOT('main'), ROOT('feature'), ROOT('flow'), ROOT('nested')] })
  tools(on)
  agents(on)
  await $.session.start(START)
  await spawn($, 'agent-wf', true)
  await spawn($, 'agent-sub', false)
  // Started from a workflow's loop, at any depth: the workflow's too.
  await spawn($, 'agent-wf-child', false, 'agent-wf')
  await spawn($, 'agent-wf-grandchild', false, 'agent-wf-child')
  await spawn($, 'agent-sub-child', false, 'agent-sub')

  await edit($, `${ROOT('flow')}/a.ts`, 'agent-wf')
  await edit($, `${ROOT('flow')}/a.ts`, 'agent-wf-grandchild')
  await edit($, `${ROOT('nested')}/a.ts`, 'agent-sub-child')
  await edit($, `${ROOT('feature')}/a.ts`, 'agent-wf')
  await edit($, `${ROOT('feature')}/a.ts`, 'agent-sub')
  await edit($, `${ROOT('main')}/a.ts`, 'agent-unknown')
  await edit($, `${ROOT('main')}/a.ts`)

  expect((await listed(w))?.map(worktree => worktree.writers)).toEqual([
    ['main', 'subagent'],
    ['subagent', 'workflow'],
    ['subagent'],
    ['workflow'],
  ])
})

test('an entry an older mod wrote stays without writers', async ($, on) => {
  const w = world(on, { git: repository(), files: [ROOT('main')] })
  tools(on)
  const { writers: _, ...before } = entry('main', 900_000)
  on('state.get', { plugin: 'nowcast', key: 'worktrees' }, () => ({ value: { value: [before], version: 1 } }))
  await $.session.start(START)

  await edit($, `${ROOT('main')}/a.ts`)

  expect((await listed(w))?.[0]).toEqual({ ...before, written_at: new Date(1_000_000).toISOString() })
  expect((await listed(w))?.[0]).not.toHaveProperty('writers')
})

test('a failed call, a relative path or a file outside a worktree marks nothing', async ($, on) => {
  const w = world(on, { git: repository() })
  tools(on)
  await $.session.start(START)

  await edit($, `${ROOT('main')}/fail/a.ts`)
  await writeFile($, 'relative/b.ts')
  await writeFile($, '/tmp/c.txt')
  await edit($, `${REPO}/config`)

  expect(await listed(w)).toBeUndefined()
})

test('a git that does not know --path-format marks nothing', async ($, on) => {
  // Before 2.31, rev-parse echoes the option and answers a relative common dir.
  const w = world(on, {
    git: args =>
      args[0] === 'rev-parse'
        ? { exitCode: 0, stdout: `--path-format=absolute\n${ROOT('main')}\n.git\n` }
        : { exitCode: 0, stdout: 'main\n' },
  })
  tools(on)
  await $.session.start(START)

  await edit($, `${ROOT('main')}/a.ts`)

  expect(await listed(w)).toBeUndefined()
})

test('each write reads the branch again; a detached HEAD has none', async ($, on) => {
  const branches: Record<string, string | null> = {}
  const w = world(on, { git: repository(branches), files: [ROOT('main')] })
  tools(on)
  await $.session.start(START)

  await edit($, `${ROOT('main')}/a.ts`)
  branches.main = 'other'
  await edit($, `${ROOT('main')}/a.ts`)
  expect(await listed(w)).toEqual([entry('main', 1_000_000, 'other')])

  branches.main = null
  await edit($, `${ROOT('main')}/a.ts`)
  expect(await listed(w)).toEqual([entry('main', 1_000_000, null)])
})

test('a worktree removed leaves the list at the next write', async ($, on) => {
  const w = world(on, { git: repository(), files: [ROOT('feature')] })
  tools(on)
  await $.session.start(START)

  await edit($, `${ROOT('main')}/a.ts`)
  await edit($, `${ROOT('feature')}/b.ts`)

  expect(await listed(w)).toEqual([entry('feature', 1_000_000)])
})

test('the list keeps the 16 worktrees written in last', async ($, on) => {
  const names = Array.from({ length: 17 }, (_, i) => `w${i}`)
  const w = world(on, { git: repository(), files: names.map(ROOT) })
  tools(on)
  await $.session.start(START)

  for (const name of names) await edit($, `${ROOT(name)}/a.ts`)

  const list = await listed(w)
  expect(list?.map(worktree => worktree.path)).toEqual(names.slice(1).reverse().map(ROOT))
})

test('a new session starts with an empty list', async ($, on) => {
  const w = world(on, { git: repository(), files: [ROOT('main'), ROOT('feature')] })
  tools(on)
  await $.session.start(START)

  await edit($, `${ROOT('main')}/a.ts`)
  await w.clock.settle()
  w.session.id = 'session-2'
  await edit($, `${ROOT('feature')}/b.ts`)

  expect(await listed(w)).toEqual([entry('feature', 1_000_000)])
})

test('a reloaded mod takes back the list its state holds', async ($, on) => {
  const w = world(on, { git: repository(), files: [ROOT('main'), ROOT('feature')] })
  tools(on)
  // What the mod wrote before it reloaded, which outlives the reload.
  const before = entry('feature', 900_000)
  on('state.get', { plugin: 'nowcast', key: 'worktrees' }, () => ({ value: { value: [before], version: 1 } }))
  await $.session.start(START)

  await edit($, `${ROOT('main')}/a.ts`)

  expect(await listed(w)).toEqual([entry('main', 1_000_000), before])
})

test('the measures carry the list', async ($, on) => {
  const flow = 'claude-code/session/session-1'
  const input = `/v1/flows/${encodeURIComponent(flow)}/input`
  const w = world(on, {
    git: repository(),
    files: [ROOT('main')],
    answer: request =>
      request.url.endsWith('/v1/subscriptions')
        ? { status: 201, text: JSON.stringify({ subscription: 's-1', flow, last: null }) }
        : request.url.endsWith(input)
          ? { status: 202 }
          : {},
  })
  tools(on)
  on('session.root', () => ({ value: '/project' }))
  on('session.model', () => ({ value: 'claude-opus-5-5' }))
  on('session.measure', (_, e) => ({ changed: e.changed }))
  await $.session.start(START)

  await edit($, `${ROOT('main')}/a.ts`)
  await w.clock.settle()
  await $.session.measure({ context: { tokens: 1, window: 2, percent: 50 }, rateLimits: [], changed: ['context'] } as never)
  await w.clock.advance(500)
  await w.clock.settle()

  const sent = w.requests.filter(r => r.url.endsWith(input)).at(-1)
  expect(JSON.parse(sent?.body ?? 'null').worktrees).toEqual([entry('main', 1_000_000)])
})

const FORK = 'git@github.com:me/r.git'
const PUSH = 'rev-parse --abbrev-ref --symbolic-full-name @{push}'

test('the forge key is where git push sends the branch, before and after a first push', async ($, on) => {
  const git: Record<string, Record<string, string>> = {}
  const w = world(on, { git: repository({}, git), files: [ROOT('feature')] })
  tools(on)
  await $.session.start(START)

  // Not pushed: `@{push}` fails, the same name on `origin`.
  await edit($, `${ROOT('feature')}/a.ts`)
  expect((await listed(w))?.[0]?.forge).toEqual({ remote: REMOTE, branch: 'feature' })

  // Pushed under another name: `@{push}` says so.
  git.feature = { [PUSH]: 'origin/feature-x' }
  await edit($, `${ROOT('feature')}/a.ts`)
  expect((await listed(w))?.[0]?.forge).toEqual({ remote: REMOTE, branch: 'feature-x' })
})

test("the forge key follows the branch's push remote, not origin", async ($, on) => {
  const w = world(on, {
    git: repository(
      { a: 'feature', b: 'feature', c: 'feature' },
      {
        // `pushRemote`, with `@{push}` resolved.
        a: {
          'config --get branch.feature.pushRemote': 'fork',
          'remote get-url --push fork': FORK,
          [PUSH]: 'fork/feature',
        },
        // `remote.pushDefault`, given as a URL: git resolves no `@{push}`.
        b: { 'config --get remote.pushDefault': FORK },
        // No remote at all.
        c: { 'remote get-url --push origin': '' },
      },
    ),
    files: [ROOT('a'), ROOT('b'), ROOT('c')],
  })
  tools(on)
  await $.session.start(START)

  await edit($, `${ROOT('a')}/x.ts`)
  await edit($, `${ROOT('b')}/x.ts`)
  await edit($, `${ROOT('c')}/x.ts`)

  expect((await listed(w))?.map(worktree => worktree.forge)).toEqual([
    null,
    { remote: FORK, branch: 'feature' },
    { remote: FORK, branch: 'feature' },
  ])
})

/** The tools beneath the mod, run from `cwd`, with Bash. */
function shell(on: On, cwd: string) {
  on('session.cwd', () => ({ value: cwd }))
  tools(on)
}

const bash = ($: Engine, command: string) => $.tool.call({ tool: 'Bash', command })

test('a command that moves a branch reads the worktrees it ran in again', async ($, on) => {
  const branches: Record<string, string | null> = {}
  const git: Record<string, Record<string, string>> = {}
  const w = world(on, { git: repository(branches, git), files: [ROOT('main'), ROOT('feature')] })
  shell(on, ROOT('main'))
  await $.session.start(START)
  await edit($, `${ROOT('feature')}/a.ts`)
  await w.clock.advance(1_000)
  await edit($, `${ROOT('main')}/a.ts`)
  const active = (name: string, ms: number, branch: string | null = name) => entry(name, ms, branch, ['main'], true)

  // Nothing moves: the worktree the command ran in only.
  branches.feature = 'elsewhere'
  await bash($, 'git status && git log')
  expect(await listed(w)).toEqual([active('main', 1_001_000), entry('feature', 1_000_000)])

  // `git switch` in the session's directory: that worktree only.
  branches.main = 'other'
  await bash($, 'git switch other')
  expect(await listed(w)).toEqual([active('main', 1_001_000, 'other'), entry('feature', 1_000_000)])

  // Through a `cd`, after a push that sets the upstream.
  git.feature = { [PUSH]: 'origin/elsewhere-x' }
  await bash($, 'cd ../feature && git push -u origin HEAD:elsewhere-x')
  expect((await listed(w))?.[0]).toEqual({
    ...entry('feature', 1_001_000, 'elsewhere'),
    forge: { remote: REMOTE, branch: 'elsewhere-x' },
  })

  // Detached by `git -C`.
  branches.feature = null
  await bash($, `git -C ${ROOT('feature')} checkout --detach`)
  expect(await listed(w)).toEqual([entry('feature', 1_001_000, null), active('main', 1_001_000, 'other')])
})

test('git worktree reads every worktree again: one removed leaves the list', async ($, on) => {
  const files = [ROOT('main'), ROOT('feature')]
  const w = world(on, { git: repository(), files })
  shell(on, ROOT('main'))
  await $.session.start(START)
  await edit($, `${ROOT('feature')}/a.ts`)
  await edit($, `${ROOT('main')}/a.ts`)
  await w.clock.settle()

  files.pop()
  await bash($, 'git worktree remove ../feature')

  expect(await listed(w)).toEqual([entry('main', 1_000_000, 'main', ['main'], true)])
})

test('a worktree removed however leaves the list at the start or the end of a turn of the main thread', async ($, on) => {
  const files = [ROOT('main'), ROOT('feature'), ROOT('other')]
  const w = world(on, { git: repository(), files })
  tools(on)
  on('turn.complete', () => ({ text: '' }))
  await $.session.start(START)
  await edit($, `${ROOT('other')}/a.ts`)
  await edit($, `${ROOT('feature')}/a.ts`)
  await edit($, `${ROOT('main')}/a.ts`)
  await w.clock.settle()
  const complete = (agentId?: string) =>
    $.turn.complete({ turnId: 't', agentId, answer: '', durationMs: 1, isAborted: false, reason: 'answer' } as never)

  // Removed in the background by `wt remove`, or from a terminal: no command of the session says so.
  files.splice(files.indexOf(ROOT('feature')), 1)
  await complete('agent-1')
  expect(await listed(w)).toHaveLength(3)
  await complete()
  expect(await listed(w)).toEqual([entry('main', 1_000_000), entry('other', 1_000_000)])

  files.splice(files.indexOf(ROOT('other')), 1)
  await $.turn.start({ text: 'hi', turnId: 't-2', agentId: 'agent-1' })
  expect(await listed(w)).toHaveLength(2)
  await $.turn.start({ text: 'hi', turnId: 't-2' })
  expect(await listed(w)).toEqual([entry('main', 1_000_000)])
})

/** The session's directory, which the test moves; the tools beneath the mod run from it. */
function directory(on: On, cwd: string): { cwd: string } {
  const where = { cwd }
  on('session.cwd', () => ({ value: where.cwd }))
  tools(on)
  return where
}

const enter = ($: Engine, path: string) => $.tool.call({ tool: 'EnterWorktree', path })
const exit = ($: Engine) => $.tool.call({ tool: 'ExitWorktree', action: 'keep' })
const active = (name: string, ms: number, writers: NowcastWriter[] = ['main']) =>
  entry(name, ms, name, writers, true)

test('entering a worktree without writing makes it listed and active', async ($, on) => {
  const w = world(on, { git: repository(), files: [ROOT('main'), ROOT('feature')] })
  const where = directory(on, ROOT('main'))
  await $.session.start(START)
  await $.turn.start({ text: 'hi', turnId: 't-1' })
  expect(await listed(w)).toEqual([active('main', 1_000_000)])

  await w.clock.advance(1_000)
  where.cwd = `${ROOT('feature')}/src`
  await enter($, ROOT('feature'))

  expect(await listed(w)).toEqual([active('feature', 1_001_000), entry('main', 1_000_000)])
})

test('moving to a directory outside git leaves no active entry', async ($, on) => {
  const w = world(on, { git: repository(), files: [ROOT('main'), ROOT('feature')] })
  const where = directory(on, ROOT('feature'))
  await $.session.start(START)
  await enter($, ROOT('feature'))
  expect(await listed(w)).toEqual([active('feature', 1_000_000)])

  where.cwd = '/tmp'
  await exit($)

  expect(await listed(w)).toEqual([entry('feature', 1_000_000)])
})

test('going back to a listed worktree moves the flag, and dates it', async ($, on) => {
  const w = world(on, { git: repository(), files: [ROOT('main'), ROOT('feature')] })
  const where = directory(on, ROOT('main'))
  on('turn.complete', () => ({ text: '' }))
  await $.session.start(START)
  await $.turn.start({ text: 'hi', turnId: 't-1' })
  await w.clock.advance(1_000)
  where.cwd = ROOT('feature')
  await bash($, 'cd ../feature')
  expect(await listed(w)).toEqual([active('feature', 1_001_000), entry('main', 1_000_000)])

  // Staying there counts for nothing; coming back to `main`, seen at the end of the turn, does.
  await w.clock.advance(1_000)
  await $.turn.start({ text: 'hi', turnId: 't-2' })
  where.cwd = ROOT('main')
  await $.turn.complete({ turnId: 't-2', answer: '', durationMs: 1, isAborted: false, reason: 'answer' } as never)

  expect(await listed(w)).toEqual([active('main', 1_002_000), entry('feature', 1_001_000)])
})

test("a subagent's Bash call in another worktree lists it without changing the active one", async ($, on) => {
  const w = world(on, { git: repository(), files: [ROOT('main'), ROOT('feature')] })
  directory(on, ROOT('main'))
  agents(on)
  await $.session.start(START)
  await $.turn.start({ text: 'hi', turnId: 't-1' })
  await spawn($, 'agent-sub', false)

  await $.tool.call({ tool: 'Bash', command: `cd ${ROOT('feature')} && nr test`, agentId: 'agent-sub' })

  expect(await listed(w)).toEqual([
    entry('feature', 1_000_000, 'feature', ['subagent']),
    active('main', 1_000_000, ['main', 'subagent']),
  ])
})

test('a Bash call that failed counts, one denied does not', async ($, on) => {
  const w = world(on, { git: repository(), files: [ROOT('main'), ROOT('feature')] })
  on('session.cwd', () => ({ value: '/tmp' }))
  on('tool.call', (_, e) => {
    const command = (e as { command?: string }).command ?? ''
    return (command.includes('rm ') ? { deny: 'denied' } : { isError: true, result: 'failed', text: 'failed' }) as never
  })
  await $.session.start(START)

  await bash($, `cd ${ROOT('main')} && rm -rf target`)
  expect(await listed(w)).toBeUndefined()
  await bash($, `cd ${ROOT('feature')} && false`)
  expect(await listed(w)).toEqual([entry('feature', 1_000_000)])
})

test('a command run in a worktree removed since does not bring it back', async ($, on) => {
  const files = [ROOT('main'), ROOT('feature')]
  const w = world(on, { git: repository(), files })
  const where = directory(on, ROOT('main'))
  await $.session.start(START)
  await bash($, `cd ${ROOT('feature')} && nr test`)
  where.cwd = ROOT('feature')
  await enter($, ROOT('feature'))
  expect(await listed(w)).toEqual([active('feature', 1_000_000), entry('main', 1_000_000)])

  files.splice(files.indexOf(ROOT('feature')), 1)
  await bash($, `cd ${ROOT('feature')} && ls`)
  await $.turn.start({ text: 'hi', turnId: 't-1' })

  expect(await listed(w)).toEqual([entry('main', 1_000_000)])
})

test('the cap never drops the active entry', async ($, on) => {
  const names = Array.from({ length: 17 }, (_, i) => `w${i}`)
  const w = world(on, { git: repository(), files: names.map(ROOT) })
  directory(on, ROOT('w0'))
  await $.session.start(START)
  await $.turn.start({ text: 'hi', turnId: 't-1' })

  for (const name of names.slice(1)) await edit($, `${ROOT(name)}/a.ts`)

  const list = await listed(w)
  expect(list?.map(worktree => worktree.path)).toEqual([...names.slice(2).reverse(), 'w0'].map(ROOT))
  expect(list?.at(-1)?.active).toBe(true)
})

/** The contents of the files git reads, by path, as `$.fs` reads them. A path absent is missing. */
function disk(on: On): Record<string, string> {
  const files: Record<string, string> = {}
  on('fs.read', (_, e) => {
    const text = files[e.path]
    if (text === undefined) throw new Error(`ENOENT: ${e.path}`)
    return { value: text }
  })
  return files
}

const LIST = 'config --list --show-origin -z'
const AT_PUSH = 'rev-parse --abbrev-ref --symbolic-full-name @{push}'

test('a work in a worktree whose HEAD and config files are unchanged asks git only @{push}', async ($, on) => {
  const branches: Record<string, string | null> = {}
  const asked: string[] = []
  const answer = repository(branches)
  const w = world(on, {
    git: (args, cwd) => {
      asked.push(args.join(' '))
      return args.join(' ') === LIST
        ? { exitCode: 0, stdout: `file:${REPO}/config\0core.bare\ntrue\0file:/home/me/.gitconfig\0include.path\n~/.gitconfig-extra\0` }
        : answer(args, cwd)
    },
    files: [ROOT('main')],
    env: { HOME: '/home/me' },
  })
  const files = disk(on)
  files[`${GIT_DIR('main')}/HEAD`] = 'ref: refs/heads/main\n'
  files[`${REPO}/config`] = 'v1'
  files['/home/me/.gitconfig'] = 'v1'
  tools(on)
  await $.session.start(START)
  /** Edits in `main` again: the list, and what git was asked meanwhile in `asked`. */
  const again = async () => {
    await w.clock.settle()
    asked.length = 0
    await edit($, `${ROOT('main')}/a.ts`)
    return listed(w)
  }

  await edit($, `${ROOT('main')}/a.ts`)
  expect(await again()).toEqual([entry('main', 1_000_000)])
  expect(asked).toEqual([AT_PUSH])

  // A checkout rewrites HEAD.
  branches.main = 'other'
  files[`${GIT_DIR('main')}/HEAD`] = 'ref: refs/heads/other\n'
  expect(await again()).toEqual([entry('main', 1_000_000, 'other')])
  expect(asked).toContain(LIST)
  expect(await again()).toEqual([entry('main', 1_000_000, 'other')])
  expect(asked).toEqual([AT_PUSH])

  // A config file changes, its size the same.
  files[`${REPO}/config`] = 'v2'
  await again()
  expect(asked).toContain(LIST)

  // An included file, missing so far, appears.
  files['/home/me/.gitconfig-extra'] = 'v2'
  await again()
  expect(asked).toContain(LIST)
  await again()
  expect(asked).toEqual([AT_PUSH])
})

test('a session directory that becomes a git repository is listed once it does', async ($, on) => {
  let inited = false
  const answer = repository()
  const w = world(on, {
    git: (args, cwd) => (inited ? answer(args, cwd) : { exitCode: 128, stdout: '' }),
    files: [ROOT('fresh')],
  })
  directory(on, ROOT('fresh'))
  await $.session.start(START)
  await $.turn.start({ text: 'hi', turnId: 't-1' })
  expect(await listed(w)).toBeUndefined()

  inited = true
  await $.turn.start({ text: 'hi', turnId: 't-2' })

  expect(await listed(w)).toEqual([active('fresh', 1_000_000)])
})

test('a worktree recreated at the same root with another git directory is read again', async ($, on) => {
  const branches: Record<string, string | null> = {}
  let gitDir = GIT_DIR('main')
  const answer = repository(branches)
  const w = world(on, {
    git: (args, cwd) =>
      args[1] === '--path-format=absolute'
        ? { exitCode: 0, stdout: `${ROOT('main')}\n${REPO}\n${gitDir}\n` }
        : answer(args, cwd),
    files: [ROOT('main')],
  })
  const files = disk(on)
  files[`${GIT_DIR('main')}/HEAD`] = 'ref: refs/heads/main\n'
  files[`${ROOT('main')}/.git`] = `gitdir: ${GIT_DIR('main')}\n`
  tools(on)
  await $.session.start(START)
  await edit($, `${ROOT('main')}/a.ts`)
  await w.clock.settle()

  // The old git directory is left behind, unpruned, and its HEAD stays.
  gitDir = `${REPO}/worktrees/main1`
  branches.main = 'other'
  files[`${gitDir}/HEAD`] = 'ref: refs/heads/other\n'
  files[`${ROOT('main')}/.git`] = `gitdir: ${gitDir}\n`
  await edit($, `${ROOT('main')}/a.ts`)
  expect(await listed(w)).toEqual([entry('main', 1_000_000, 'other')])

  // Checkouts in the new one are seen.
  branches.main = 'third'
  files[`${gitDir}/HEAD`] = 'ref: refs/heads/third\n'
  await edit($, `${ROOT('main')}/a.ts`)
  expect(await listed(w)).toEqual([entry('main', 1_000_000, 'third')])
})

test('in the reftable format, whose HEAD file stays the same, the reftable says the checkout', async ($, on) => {
  const branches: Record<string, string | null> = {}
  const w = world(on, { git: repository(branches), files: [ROOT('main')] })
  const files = disk(on)
  files[`${GIT_DIR('main')}/HEAD`] = 'ref: refs/heads/.invalid\n'
  files[`${GIT_DIR('main')}/reftable/tables.list`] = 'v1'
  tools(on)
  await $.session.start(START)

  await edit($, `${ROOT('main')}/a.ts`)
  await w.clock.settle()
  branches.main = 'other'
  files[`${GIT_DIR('main')}/reftable/tables.list`] = 'v2'
  await edit($, `${ROOT('main')}/a.ts`)

  expect(await listed(w)).toEqual([entry('main', 1_000_000, 'other')])
})
