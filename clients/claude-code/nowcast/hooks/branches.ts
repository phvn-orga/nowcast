// The forge flows of the session's worktrees (`02-clients.md` on the design
// branch; `06-brique-forge.md`, "La vie d'une session"): while the forge
// brick is enabled, this mod subscribes to the `pr` and `pipelines` flows of
// each worktree's key, where `git push` sends its branch (`worktrees.ts`). A
// key that changes takes its subscriptions again; a worktree that leaves the
// list, or a `/clear` that empties it, drops them.
//
// They are this mod's own, named `worktree-<kind>:<path>`: a display mod finds
// a worktree's flow through the subscription of that name (`types/index.d.ts`).
//
// The brick is optional: once the service answers `unknown_brick`, nothing is
// subscribed for the session, and what was is dropped. A key the brick
// refuses (a remote no provider recognises) is not asked again until it
// changes.

import type { EngineInterface } from 'claude-code'
import type { NowcastForgeKey, NowcastWorktree } from '../types'
import { heldBy, subscribeFor, unsubscribeFor } from './subscriptions'

type $ = EngineInterface

const BRICK = 'forge'
const OWNER = 'nowcast'
const KINDS = ['pr', 'pipelines'] as const
const PREFIX = 'worktree-'

/** The session the brick is not enabled for. */
let off: string | undefined
/** The key each subscription was last asked with, by name: asked again only once it changes. */
const asked = new Map<string, string>()
// The lists are followed one at a time, in the order they were written.
let queue: Promise<void> = Promise.resolve()

/**
 * Queues following the forge flows of `list`, the session's worktrees as
 * just written; an empty list, after a `/clear` or a resume, drops them all.
 */
export function followForge($: $, list: readonly NowcastWorktree[]): void {
  queue = queue
    .then(() => sync($, list))
    .catch(error => $.ui.log(`nowcast: cannot follow the worktrees' forge: ${message(error)}`, { to: 'debug' }))
}

async function sync($: $, list: readonly NowcastWorktree[]): Promise<void> {
  const session = await $.session.id()
  const wanted = new Map<string, { kind: string; key: NowcastForgeKey }>()
  if (off !== session) {
    for (const worktree of list) {
      if (worktree.forge === undefined || worktree.forge === null) continue
      for (const kind of KINDS) wanted.set(`${PREFIX}${kind}:${worktree.path}`, { kind, key: worktree.forge })
    }
  }
  for (const name of asked.keys()) if (!wanted.has(name)) asked.delete(name)
  for (const name of heldBy(OWNER)) {
    if (name.startsWith(PREFIX) && !wanted.has(name)) await unsubscribeFor($, OWNER, name)
  }
  for (const [name, { kind, key }] of wanted) {
    const params = { remote: key.remote, branch: key.branch }
    const json = JSON.stringify(params)
    if (asked.get(name) === json) continue
    asked.set(name, json)
    // The same name with another key replaces the subscription, and its flow.
    const answer = await subscribeFor($, OWNER, { name, brick: BRICK, kind, params })
    // A subscription kept but failed or waiting says why as well.
    const code = answer.ok ? answer.error?.code : answer.code
    if (code === 'unknown_brick') {
      off = session
      // Drops what this session subscribed before.
      return sync($, list)
    }
    // The brick refuses the key itself: waiting will not change that.
    if (code === 'subscription_refused') await unsubscribeFor($, OWNER, name)
  }
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
