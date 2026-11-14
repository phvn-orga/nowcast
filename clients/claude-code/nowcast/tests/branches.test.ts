// The forge flows of the session's worktrees: the mod subscribes to the `pr`
// and `pipelines` flows of each worktree's key, takes them again when the key
// changes, and drops them when the worktree leaves the list or the session
// starts over.

import type { HttpResponse, On } from 'claude-code'
import { type Engine, expect, test } from 'claude-code/testing'
import { type Request, START, type World, world } from './world'

const REPO = '/lab/nowcast/.bare'
const REMOTE = 'git@github.com:o/r.git'
const ROOT = (name: string) => `/lab/nowcast/${name}`
const SUBSCRIPTIONS = 'http://nowcast/v1/subscriptions'

/** Worktrees under `/lab/nowcast/`, on the branch `branches` gives, else their name; `origin` pushes. */
function repository(branches: Record<string, string | null>) {
  return (args: readonly string[], cwd: string | undefined) => {
    const name = cwd?.match(/^\/lab\/nowcast\/([^/.][^/]*)/)?.[1]
    if (name === undefined) return { exitCode: 128, stdout: '' }
    const line = args.join(' ')
    if (args[1] === '--path-format=absolute') return { exitCode: 0, stdout: `${ROOT(name)}\n${REPO}\n` }
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

/** A forge brick that accepts every subscription, `s-<n>` on the flow of its key, unless `refuse` answers. */
function brick(refuse?: (body: Record<string, unknown>) => Partial<HttpResponse> | undefined) {
  let made = 0
  return (request: Request): Partial<HttpResponse> => {
    if (request.method === 'DELETE') return { status: 204 }
    if (request.url !== SUBSCRIPTIONS) return {}
    const body = JSON.parse(request.body ?? '{}') as { kind: string; params: { branch: string } }
    const refused = refuse?.(body)
    if (refused !== undefined) return refused
    const flow = `forge/${body.kind}/github.com/o/r#${body.params.branch}`
    return { status: 201, text: JSON.stringify({ subscription: `s-${++made}`, flow, last: null }) }
  }
}

const refused = (status: number, code: string) => ({ status, text: JSON.stringify({ error: { code, message: code } }) })

function tools(on: On, cwd: string) {
  on('session.cwd', () => ({ value: cwd }))
  on('tool.call', () => ({ result: {} }) as never)
}

const edit = ($: Engine, file: string) =>
  $.tool.call({ tool: 'Edit', file_path: file, old_string: 'a', new_string: 'b' })
const bash = ($: Engine, command: string) => $.tool.call({ tool: 'Bash', command })

/** What went to the service about subscriptions, once the mod is done: `+kind params` or `-id`. */
async function sent(w: World): Promise<string[]> {
  await w.clock.settle()
  return w.requests
    .filter(r => r.url.startsWith(SUBSCRIPTIONS))
    .map(r => {
      if (r.method === 'DELETE') return `-${r.url.slice(SUBSCRIPTIONS.length + 1)}`
      const body = JSON.parse(r.body ?? '{}') as { brick: string; kind: string; params: unknown }
      return `+${body.brick}.${body.kind} ${JSON.stringify(body.params)}`
    })
}

const key = (branch: string) => JSON.stringify({ remote: REMOTE, branch })

test("each worktree's key is subscribed to, pr and pipelines, under names display mods find", async ($, on) => {
  const w = world(on, { git: repository({}), files: [ROOT('feature')], answer: brick() })
  tools(on, ROOT('feature'))
  await $.session.start(START)

  await edit($, `${ROOT('feature')}/a.ts`)
  // A write that leaves the key as it was asks nothing more.
  await edit($, `${ROOT('feature')}/b.ts`)

  expect(await sent(w)).toEqual([`+forge.pr ${key('feature')}`, `+forge.pipelines ${key('feature')}`])
  expect(w.subscription(`nowcast/worktree-pr:${ROOT('feature')}`)).toMatchObject({
    status: 'active',
    flow: 'forge/pr/github.com/o/r#feature',
  })
  expect(w.subscription(`nowcast/worktree-pipelines:${ROOT('feature')}`)?.flow).toBe(
    'forge/pipelines/github.com/o/r#feature',
  )
})

test('a key that changes is subscribed to again, the old flows dropped', async ($, on) => {
  const branches: Record<string, string | null> = {}
  const w = world(on, { git: repository(branches), files: [ROOT('feature')], answer: brick() })
  tools(on, ROOT('feature'))
  await $.session.start(START)
  await edit($, `${ROOT('feature')}/a.ts`)
  await w.clock.settle()

  branches.feature = 'other'
  await bash($, 'git switch other')

  expect(await sent(w)).toEqual([
    `+forge.pr ${key('feature')}`,
    `+forge.pipelines ${key('feature')}`,
    '-s-1',
    `+forge.pr ${key('other')}`,
    '-s-2',
    `+forge.pipelines ${key('other')}`,
  ])
  expect(w.subscription(`nowcast/worktree-pr:${ROOT('feature')}`)?.flow).toBe('forge/pr/github.com/o/r#other')

  // Detached: no key, nothing followed.
  branches.feature = null
  await bash($, 'git checkout --detach')
  expect((await sent(w)).slice(6)).toEqual(['-s-3', '-s-4'])
  expect(w.subscription(`nowcast/worktree-pr:${ROOT('feature')}`)).toBeNull()
})

test('a worktree that leaves the list, or a /clear, drops its flows', async ($, on) => {
  const files = [ROOT('main'), ROOT('feature')]
  const w = world(on, { git: repository({}), files, answer: brick() })
  tools(on, ROOT('main'))
  await $.session.start(START)
  await edit($, `${ROOT('feature')}/a.ts`)
  await edit($, `${ROOT('main')}/a.ts`)
  await w.clock.settle()

  files.pop()
  await bash($, 'git worktree remove ../feature')
  expect((await sent(w)).slice(4)).toEqual(['-s-1', '-s-2'])
  expect(w.subscription(`nowcast/worktree-pr:${ROOT('feature')}`)).toBeNull()

  await $.session.end({ reason: 'clear', sessionId: 'session-1', resume: { id: 'session-1' } })
  expect((await sent(w)).slice(6)).toEqual(['-s-3', '-s-4'])
  expect(w.subscription(`nowcast/worktree-pr:${ROOT('main')}`)).toBeNull()
})

test('without the forge brick, the session asks once and no more, with no error', async ($, on) => {
  const w = world(on, {
    git: repository({}),
    files: [ROOT('main'), ROOT('feature')],
    answer: brick(() => refused(404, 'unknown_brick')),
  })
  tools(on, ROOT('main'))
  await $.session.start(START)

  await edit($, `${ROOT('main')}/a.ts`)
  await edit($, `${ROOT('feature')}/a.ts`)
  await bash($, 'git switch other')

  expect(await sent(w)).toEqual([`+forge.pr ${key('main')}`])
  expect(w.subscription(`nowcast/worktree-pr:${ROOT('main')}`)).toBeUndefined()

  // The next session asks again.
  w.session.id = 'session-2'
  await edit($, `${ROOT('main')}/a.ts`)
  expect(await sent(w)).toEqual([`+forge.pr ${key('main')}`, `+forge.pr ${key('main')}`])
})

test('a key the brick refuses is let go, and asked again only once it changes', async ($, on) => {
  const branches: Record<string, string | null> = {}
  const w = world(on, {
    git: repository(branches),
    files: [ROOT('feature')],
    answer: brick(body => (body.kind === 'pr' ? refused(422, 'subscription_refused') : undefined)),
  })
  tools(on, ROOT('feature'))
  await $.session.start(START)

  await edit($, `${ROOT('feature')}/a.ts`)
  await edit($, `${ROOT('feature')}/b.ts`)
  expect(await sent(w)).toEqual([`+forge.pr ${key('feature')}`, `+forge.pipelines ${key('feature')}`])
  expect(w.subscription(`nowcast/worktree-pr:${ROOT('feature')}`)).toBeNull()
  // Nothing is tried again later either.
  await w.clock.advance(120_000)
  expect((await sent(w)).length).toBe(2)

  branches.feature = 'other'
  await edit($, `${ROOT('feature')}/a.ts`)
  expect((await sent(w)).slice(2)).toEqual([`+forge.pr ${key('other')}`, '-s-1', `+forge.pipelines ${key('other')}`])
})
