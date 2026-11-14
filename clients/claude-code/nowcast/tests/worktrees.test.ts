// The worktrees the session writes in: in this mod's state, and with the
// session's measures.

import type { On } from 'claude-code'
import { type Engine, expect, test } from 'claude-code/testing'
import type { NowcastWorktree } from '../types'
import { START, type World, world } from './world'

const REPO = '/lab/nowcast/.bare'
const REMOTE = 'git@github.com:o/r.git'
const ROOT = (name: string) => `/lab/nowcast/${name}`

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
      return { exitCode: 0, stdout: `${ROOT(name)}\n${REPO}\n` }
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

async function edit($: Engine, file: string) {
  await $.tool.call({ tool: 'Edit', file_path: file, old_string: 'a', new_string: 'b' })
}

async function writeFile($: Engine, file: string) {
  await $.tool.call({ tool: 'Write', file_path: file, content: '' })
}

/** The list last written, once the writes queued are done. */
async function listed(w: World): Promise<NowcastWorktree[] | undefined> {
  await w.clock.settle()
  return w.writes.filter(write => write.key === 'worktrees').at(-1)?.value as NowcastWorktree[] | undefined
}

const entry = (name: string, ms: number, branch: string | null = name): NowcastWorktree => ({
  path: ROOT(name),
  repo: REPO,
  branch,
  remote: REMOTE,
  written_at: new Date(ms).toISOString(),
  forge: branch === null ? null : { remote: REMOTE, branch },
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
  await w.clock.settle()
  const count = () => w.writes.filter(write => write.key === 'worktrees').length
  const before = count()

  // Nothing moves: no new reading, no write.
  branches.main = 'other'
  await bash($, 'git status && git log')
  await w.clock.settle()
  expect(count()).toBe(before)

  // `git switch` in the session's directory: that worktree only.
  branches.feature = 'elsewhere'
  await bash($, 'git switch other')
  expect(await listed(w)).toEqual([entry('main', 1_001_000, 'other'), entry('feature', 1_000_000)])

  // Through a `cd`, after a push that sets the upstream.
  git.feature = { [PUSH]: 'origin/elsewhere-x' }
  await bash($, 'cd ../feature && git push -u origin HEAD:elsewhere-x')
  expect((await listed(w))?.[1]).toEqual({
    ...entry('feature', 1_000_000, 'elsewhere'),
    forge: { remote: REMOTE, branch: 'elsewhere-x' },
  })

  // Detached by `git -C`.
  branches.feature = null
  await bash($, `git -C ${ROOT('feature')} checkout --detach`)
  expect((await listed(w))?.[1]).toEqual(entry('feature', 1_000_000, null))
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

  expect(await listed(w)).toEqual([entry('main', 1_000_000)])
})
