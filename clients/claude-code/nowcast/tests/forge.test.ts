// The agent's forge actions, refreshed in the forge brick: a successful Bash
// or MCP tool call reaches `POST /v1/bricks/forge/refresh` with a scope, or
// by kind when the repository is not found or the service takes no scope.

import type { HttpResponse, On } from 'claude-code'
import { type Engine, expect, test } from 'claude-code/testing'
import { type Request, START, type World, call, caller, world } from './world'

const REPO = '/lab/repo'
const REFRESH = 'http://nowcast/v1/bricks/forge/refresh'

/** A repository at `REPO` on `feature`, pushing to `fork`, and nothing elsewhere. */
function repository(args: readonly string[], cwd: string | undefined) {
  if (cwd !== REPO) return { exitCode: 128, stdout: '' }
  const answers: Record<string, string> = {
    'symbolic-ref --quiet --short HEAD': 'feature',
    'config --get branch.feature.pushRemote': 'fork',
    'remote get-url --push fork': 'git@github.com:me/r.git',
    'remote get-url --push origin': 'git@github.com:o/r.git',
  }
  const out = answers[args.join(' ')]
  return out === undefined ? { exitCode: 1, stdout: '' } : { exitCode: 0, stdout: `${out}\n` }
}

/** The tools beneath the mod: a command with `fail` in it errors; `printed` is stdout. */
function tools(on: On, printed = '') {
  on('session.cwd', () => ({ value: REPO }))
  on('tool.call', (_, e) => {
    const command = (e as { command?: string }).command ?? ''
    if (command.includes('fail')) return { isError: true, result: 'failed', text: 'failed' } as never
    if (e.tool === 'Bash') return { result: { stdout: printed, stderr: '', interrupted: false } } as never
    return { result: '{"html_url":"https://github.com/o/r/pull/9"}' } as never
  })
}

/** A service answering each refresh with `answer`. */
const serving = (answer: (body: Record<string, unknown>) => Partial<HttpResponse>) => (request: Request) =>
  request.url === REFRESH ? answer(JSON.parse(request.body ?? '{}') as Record<string, unknown>) : {}

/** The bodies of the refreshes sent, once the mod is done. */
async function refreshes(w: World): Promise<unknown[]> {
  await w.clock.settle()
  return w.requests.filter(r => r.url === REFRESH).map(r => JSON.parse(r.body ?? 'null'))
}

const bash = ($: Engine, command: string) => $.tool.call({ tool: 'Bash', command })
const refused = (status: number, code: string) => ({ status, text: JSON.stringify({ error: { code, message: code } }) })

test('gh pr create refreshes the PR it printed, with its number', async ($, on) => {
  const w = world(on, { git: repository, answer: serving(() => ({ status: 202 })) })
  tools(on, 'https://github.com/o/r/pull/41\n')
  await $.session.start(START)

  await bash($, 'gh pr create --fill')

  expect(await refreshes(w)).toEqual([
    { scope: { remote: 'https://github.com/o/r', types: ['pr', 'pipelines'], number: 41 } },
  ])
})

test("git push refreshes the branch's push remote, read with git", async ($, on) => {
  const w = world(on, { git: repository, answer: serving(() => ({ status: 202 })) })
  tools(on)
  await $.session.start(START)

  await bash($, 'git add -A && git commit -m "x" && git push -u')
  await bash($, 'git push origin HEAD')

  expect(await refreshes(w)).toEqual([
    { scope: { remote: 'git@github.com:me/r.git', types: ['pr', 'pipelines'] } },
    { scope: { remote: 'git@github.com:o/r.git', types: ['pr', 'pipelines'] } },
  ])
})

test('an MCP tool reaches the brick; a failed call and a read-only one send nothing', async ($, on) => {
  const w = world(on, { git: repository, answer: serving(() => ({ status: 202 })) })
  tools(on)
  await $.session.start(START)

  await bash($, 'git push fail')
  await bash($, 'gh pr view 3')
  await $.tool.call({ tool: 'mcp__github__get_pull_request', owner: 'o', repo: 'r', pull_number: 3 } as never)
  await $.tool.call({ tool: 'mcp__github__create_pull_request', owner: 'o', repo: 'r', title: 't' } as never)

  expect(await refreshes(w)).toEqual([
    { scope: { remote: 'https://github.com/o/r', types: ['pr', 'pipelines'], number: 9 } },
  ])
})

test('a repository not found refreshes every flow of each kind concerned', async ($, on) => {
  const w = world(on, { git: repository, answer: serving(() => ({ status: 202 })) })
  tools(on)
  await $.session.start(START)

  await bash($, 'cd /elsewhere && gh issue close 3')

  expect(await refreshes(w)).toEqual([{ kind: 'issues' }, { kind: 'issue' }])
})

test('a service that refuses the scope gets the refresh by kind, then only that', async ($, on) => {
  // An older service rejects the unknown field.
  const w = world(on, {
    git: repository,
    answer: serving(body => ('scope' in body ? refused(400, 'invalid_request') : { status: 202 })),
  })
  tools(on)
  await $.session.start(START)

  await bash($, 'git push')
  await bash($, 'gh issue comment 2 -R o/r -b hi')

  expect(await refreshes(w)).toEqual([
    { scope: { remote: 'git@github.com:me/r.git', types: ['pr', 'pipelines'] } },
    { kind: 'pr' },
    { kind: 'pipelines' },
    { kind: 'issues' },
    { kind: 'issue' },
  ])
})

test('without the forge brick, nothing more is sent in the session', async ($, on) => {
  const w = world(on, { git: repository, answer: serving(() => refused(404, 'unknown_brick')) })
  tools(on)
  await $.session.start(START)

  await bash($, 'git push && cd /elsewhere && gh issue close 3')
  await bash($, 'git push')

  expect(await refreshes(w)).toEqual([{ scope: { remote: 'git@github.com:me/r.git', types: ['pr', 'pipelines'] } }])
})

const plugins = { plugins: [caller] }

test('refreshForge sends a scope in the same terms, from a remote or a directory', plugins, async ($, on) => {
  const w = world(on, { git: repository, answer: serving(() => ({ status: 202 })) })
  tools(on)
  await $.session.start(START)

  const answers = [
    await call($, 'refreshForge', { remote: 'https://github.com/o/r', types: ['issues'], number: 52 }),
    // The session's directory, every type.
    await call($, 'refreshForge', {}),
    await call($, 'refreshForge', { cwd: '.', types: ['pipelines', 'pr'] }),
    // Not a repository: every flow of the kinds.
    await call($, 'refreshForge', { cwd: '/elsewhere', types: ['issues'] }),
  ]

  expect(answers).toEqual([{ ok: true }, { ok: true }, { ok: true }, { ok: true }])
  expect(await refreshes(w)).toEqual([
    { scope: { remote: 'https://github.com/o/r', types: ['issues'], number: 52 } },
    { scope: { remote: 'git@github.com:me/r.git', types: ['pr', 'pipelines', 'issues'] } },
    { scope: { remote: 'git@github.com:me/r.git', types: ['pr', 'pipelines'] } },
    { kind: 'issues' },
    { kind: 'issue' },
  ])
})

test('refreshForge refuses a malformed request, and says when the brick is not enabled', plugins, async ($, on) => {
  const w = world(on, { git: repository, answer: serving(() => refused(404, 'unknown_brick')) })
  tools(on)
  await $.session.start(START)

  const malformed = [{ types: ['nope'] }, { types: [] }, { number: 0 }, { remote: '' }, { cwd: 3 }]
  for (const request of malformed) {
    expect(await call($, 'refreshForge', request)).toMatchObject({ ok: false, code: 'invalid_request' })
  }
  expect(await refreshes(w)).toEqual([])

  expect(await call($, 'refreshForge', { types: ['pr'] })).toMatchObject({ ok: false, code: 'unknown_brick' })
  // Asked no more in the session.
  expect(await call($, 'refreshForge', { types: ['pr'] })).toMatchObject({ ok: false, code: 'unknown_brick' })
  expect((await refreshes(w)).length).toBe(1)
})
