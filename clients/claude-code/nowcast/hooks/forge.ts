// What this session's agent changes on a forge, refreshed in the forge brick
// (`06-brique-forge.md` on the design branch, "Ce que reconnaît le client").
// After each successful Bash or MCP tool call, of the main loop or a
// subagent, the forge actions it took (`actions.ts`) become scoped refreshes
// of the repository they touched. A repository not to be found refreshes
// every flow of the kinds concerned instead, as before scopes.
//
// The brick is optional: once the service answers `unknown_brick`, nothing
// more is sent for the session. A service or brick that does not take a
// scope (an older one) gets the refresh by kind, then on for the session.

import type { EngineInterface, On } from 'claude-code'
import {
  type AzureNames,
  type ForgeAction,
  type ForgeType,
  type Target,
  MAY_ACT,
  azureNames,
  azureRemote,
  commandActions,
  isUrl,
  mcpAction,
  ordered,
} from './actions'
import { refreshBrick } from './client'
import { git } from './worktrees'

type $ = EngineInterface

const BRICK = 'forge'
/** The answers that refuse the scope, not the refresh: an older service or brick. */
const SCOPE_REFUSED = new Set(['invalid_request', 'scope_not_accepted', 'invalid_scope'])
/** The kinds a type of a scope covers. */
const KINDS: Record<ForgeType, string[]> = { pr: ['pr'], pipelines: ['pipelines'], issues: ['issues', 'issue'] }

const MAY_SERVER = /github|azure|devops|ado/i

/** The session the brick is not enabled for; the session whose service refuses scopes. */
let off: string | undefined
let scopeless: string | undefined
// Refreshes go one call at a time, in order: an answer holds for the next.
let queue: Promise<void> = Promise.resolve()

export function registerForge(on: On): void {
  on('tool.call', async ($, e, next) => {
    // Only a command or a server that may speak to a forge.
    const may = e.tool === 'Bash' ? MAY_ACT.test(e.command) : e.tool.startsWith('mcp__') && MAY_SERVER.test(e.tool)
    if (!may) return next(e)
    // Read before the command runs: a `cd` in it moves the session.
    const cwd = await $.session.cwd().catch(() => undefined)
    const result = await next(e)
    if (result.deny !== undefined || result.isError === true) return result
    const value: unknown = result.result
    let actions: ForgeAction[]
    if (e.tool === 'Bash') {
      // `az` prints JSON on stdout, read whole; `gh` its messages on stderr.
      const stdout = isObject(value) && typeof value.stdout === 'string' ? value.stdout : (result.text ?? '')
      const stderr = isObject(value) && typeof value.stderr === 'string' ? value.stderr : ''
      actions = commandActions(e.command, cwd, stdout, stderr)
    } else {
      const output = `${result.text ?? ''}\n${typeof value === 'string' ? value : JSON.stringify(value ?? null)}`
      const found = mcpAction(e.tool, e as unknown as Record<string, unknown>, cwd, output)
      actions = found === null ? [] : [found]
    }
    if (actions.length > 0) {
      // The tool's answer waits for nothing.
      queue = queue
        .then(() => refreshFor($, actions))
        .catch(error => $.ui.log(`nowcast: cannot refresh the forge: ${message(error)}`, { to: 'debug' }))
    }
    return result
  })
}

/** Sends the refreshes `actions` call for: one per repository and number, else one per kind. */
export async function refreshFor($: $, actions: ForgeAction[]): Promise<void> {
  const session = await $.session.id()
  if (off === session) return
  const scopes = new Map<string, { remote: string; types: Set<ForgeType>; number?: number }>()
  const kinds = new Set<string>()
  for (const action of actions) {
    const remote = await resolve($, action.target, action.types)
    if (remote === null) {
      for (const type of action.types) for (const kind of KINDS[type]) kinds.add(kind)
      continue
    }
    const key = `${remote} ${action.number ?? ''}`
    const scope = scopes.get(key) ?? {
      remote,
      types: new Set<ForgeType>(),
      ...(action.number === undefined ? {} : { number: action.number }),
    }
    for (const type of action.types) scope.types.add(type)
    scopes.set(key, scope)
  }
  for (const { remote, types, number } of scopes.values()) {
    const scope = { remote, types: ordered(types), ...(number === undefined ? {} : { number }) }
    if (scopeless !== session) {
      const answer = await refreshBrick($, BRICK, undefined, scope)
      if (answer.ok) continue
      if (answer.code === 'unknown_brick') {
        off = session
        return
      }
      if (!SCOPE_REFUSED.has(answer.code)) {
        $.ui.log(`nowcast: the forge refresh failed: ${answer.message}`, { to: 'debug' })
        continue
      }
      scopeless = session
    }
    for (const type of scope.types) for (const kind of KINDS[type]) kinds.add(kind)
  }
  for (const kind of kinds) {
    const answer = await refreshBrick($, BRICK, kind)
    if (answer.ok) continue
    if (answer.code === 'unknown_brick') {
      off = session
      return
    }
    $.ui.log(`nowcast: the forge refresh of ${kind} failed: ${answer.message}`, { to: 'debug' })
  }
}

/** The remote of a target, as git or the names give it; null when not to be found. */
async function resolve($: $, target: Target, types: ForgeType[]): Promise<string | null> {
  if (target === null) return null
  if ('remote' in target) return target.remote
  if ('azure' in target) {
    const here = target.dir === undefined ? null : await pushUrl($, target.dir)
    return azure(target.azure, here === null ? null : azureNames(here), types)
  }
  if (target.name === undefined) return pushUrl($, target.dir)
  return git($, target.dir, ['remote', 'get-url', '--push', target.name])
}

/**
 * An Azure DevOps remote from the names given, completed from the
 * directory's remote `here` when each name given matches it. Work items
 * belong to the project: with no repository, the project's default one,
 * named as the project, finds it.
 */
export function azure(given: AzureNames, here: Required<AzureNames> | null, types: ForgeType[]): string | null {
  const same = (a: string, b: string) => a.toLowerCase() === b.toLowerCase()
  const org = given.org ?? here?.org
  const fromHere = (key: 'project' | 'repo') => {
    if (here === null || org === undefined || !same(org, here.org)) return undefined
    if (key === 'repo' && given.project !== undefined && !same(given.project, here.project)) return undefined
    return here[key]
  }
  const project = given.project ?? fromHere('project')
  if (org === undefined || project === undefined) return null
  const repo = given.repo ?? fromHere('repo') ?? (types.every(type => type === 'issues') ? project : undefined)
  return repo === undefined ? null : azureRemote(org, project, repo)
}

/**
 * The push URL of the remote `git push` sends `dir`'s branch to: its
 * `pushRemote`, else `remote.pushDefault`, else its upstream's remote, else
 * `origin` (`02-clients.md` on the design branch).
 */
async function pushUrl($: $, dir: string): Promise<string | null> {
  const branch = await git($, dir, ['symbolic-ref', '--quiet', '--short', 'HEAD'])
  const config = (key: string) => git($, dir, ['config', '--get', key])
  const name =
    (branch === null ? null : await config(`branch.${branch}.pushRemote`)) ??
    (await config('remote.pushDefault')) ??
    (branch === null ? null : await config(`branch.${branch}.remote`)) ??
    'origin'
  // A remote may be given as its URL.
  if (isUrl(name)) return name
  return git($, dir, ['remote', 'get-url', '--push', name])
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
