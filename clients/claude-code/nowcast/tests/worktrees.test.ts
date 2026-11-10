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
 * of its name unless `branches` says otherwise (null: a detached HEAD).
 * Nothing else is in a repository.
 */
function repository(branches: Record<string, string | null> = {}) {
  return (args: readonly string[], cwd: string | undefined) => {
    const name = cwd?.match(/^\/lab\/nowcast\/([^/.][^/]*)/)?.[1]
    if (name === undefined) return { exitCode: 128, stdout: '' }
    if (args[0] === 'rev-parse') return { exitCode: 0, stdout: `${ROOT(name)}\n${REPO}\n` }
    if (args[0] === 'symbolic-ref') {
      const branch = name in branches ? branches[name] : name
      return branch === null ? { exitCode: 1, stdout: '' } : { exitCode: 0, stdout: `${branch}\n` }
    }
    if (args[0] === 'config') return { exitCode: 0, stdout: `${REMOTE}\n` }
    return { exitCode: 128, stdout: '' }
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
