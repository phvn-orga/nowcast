// The forge actions an agent's tool call may have taken (`06-brique-forge.md`
// on the design branch, "Ce que reconnaît le client"): a shell command
// (`git push`, `gh`, `az`, a REST call with `curl` or `http`) or an MCP tool
// of a GitHub or Azure DevOps server. Pure functions, read without touching
// the engine: `forge.ts` resolves what they leave to the working directory
// and sends the refreshes.

/** What a scoped refresh of the forge brick names. */
export type ForgeType = 'pr' | 'pipelines' | 'issues'

/** The names of an Azure DevOps repository, each decoded; absent when not known. */
export type AzureNames = { org?: string; project?: string; repo?: string }

/**
 * Where the repository an action touched comes from:
 * - `remote`: known, a URL the brick recognises;
 * - `dir`: the push remote of a directory, or its remote `name`;
 * - `azure`: some Azure DevOps names, the rest from the directory's remote;
 * - null: not to be found, every flow of the types is refreshed instead.
 */
export type Target = { remote: string } | { dir: string; name?: string } | { dir?: string; azure: AzureNames } | null

export type ForgeAction = { types: ForgeType[]; target: Target; number?: number }

const PR: ForgeType[] = ['pr', 'pipelines']
// A run's state shows on the PR as well: the design reads a pipelines action
// as `pr` and `pipelines` alike.
const PIPELINES: ForgeType[] = ['pr', 'pipelines']
const ISSUES: ForgeType[] = ['issues']
const MERGE: ForgeType[] = ['pr', 'pipelines', 'issues']

/** The types in one order, each once. */
export function ordered(types: Iterable<ForgeType>): ForgeType[] {
  const set = new Set(types)
  return (['pr', 'pipelines', 'issues'] as const).filter(type => set.has(type))
}

// ---------------------------------------------------------------------------
// Shell

/** A command line that may name a program taking forge actions. */
export const MAY_ACT = /\b(git|gh|az|curl|https?|xh)\b/

/**
 * One simple command: its words, quotes removed; `depth` > 0 inside `$(...)`;
 * `group` the path of the `( ... )` subshells around it (`/1/2`), empty outside.
 */
export type Simple = { words: string[]; depth: number; group: string }

/**
 * The simple commands of a shell command line: split on `&&`, `||`, `;`,
 * `|`, `&`, newlines and parentheses, with quotes, escapes, redirections and
 * heredocs read as the shell reads them. Commands inside `$(...)` and
 * backquotes come too, before the command that holds them. Expansions are
 * left as written.
 */
export function simpleCommands(line: string): Simple[] {
  const parser = new Parser(line)
  parser.commands(0)
  return parser.out
}

class Parser {
  i = 0
  readonly out: Simple[] = []
  /** The subshells open, innermost last, and the count of those opened so far. */
  private groups: number[] = []
  private opened = 0
  constructor(private readonly s: string) {}

  /** Reads commands up to `close` (consumed) or the end. */
  commands(depth: number, close?: string): void {
    const s = this.s
    let words: string[] = []
    let word: string | null = null
    // What the next word is: a redirection's target, dropped, or a heredoc's tag.
    let next: 'word' | 'drop' | 'tag' | 'tag-' = 'word'
    let heredocs: Array<{ tag: string; strip: boolean }> = []
    let groups = 0
    const endWord = () => {
      if (word === null) return
      if (next === 'word') words.push(word)
      else if (next !== 'drop') heredocs.push({ tag: word, strip: next === 'tag-' })
      word = null
      next = 'word'
    }
    const endCommand = () => {
      endWord()
      if (words.length > 0) this.out.push({ words, depth, group: this.groups.map(id => `/${id}`).join('') })
      words = []
    }
    while (this.i < s.length) {
      const c = s[this.i] as string
      const after = s[this.i + 1]
      if (c === close && (close !== ')' || groups === 0)) {
        this.i++
        endCommand()
        return
      }
      if (c === '\n') {
        this.i++
        endCommand()
        this.heredocs(heredocs)
        heredocs = []
      } else if (c === ' ' || c === '\t') {
        this.i++
        endWord()
      } else if (c === '#' && word === null) {
        while (this.i < s.length && s[this.i] !== '\n') this.i++
      } else if (c === '\\') {
        if (after !== '\n' && after !== undefined) word = (word ?? '') + after
        this.i += 2
      } else if (c === "'") {
        const end = s.indexOf("'", this.i + 1)
        const stop = end === -1 ? s.length : end
        word = (word ?? '') + s.slice(this.i + 1, stop)
        this.i = stop + 1
      } else if (c === '"') {
        this.i++
        word = (word ?? '') + this.quoted(depth)
      } else if (c === '$' && after === '(') {
        this.i += 2
        this.commands(depth + 1, ')')
        word = `${word ?? ''}$(...)`
      } else if (c === '`') {
        this.i++
        this.commands(depth + 1, '`')
        word = `${word ?? ''}\`...\``
      } else if (c === '<' || c === '>' || (c === '&' && after === '>')) {
        // A file descriptor before the operator is part of it.
        if (word !== null && /^\d+$/.test(word)) word = null
        endWord()
        if (c === '<' && after === '<' && s[this.i + 2] !== '<') {
          this.i += 2
          next = s[this.i] === '-' ? 'tag-' : 'tag'
          if (next === 'tag-') this.i++
        } else {
          while (this.i < s.length && '<>&|'.includes(s[this.i] as string)) this.i++
          next = 'drop'
        }
      } else if (';|&()'.includes(c)) {
        this.i += after === c || (c === '|' && after === '&') ? 2 : 1
        // The command before a parenthesis is outside the subshell it opens or closes.
        endCommand()
        if (c === '(') {
          groups++
          this.groups.push(++this.opened)
        }
        if (c === ')' && groups > 0) {
          groups--
          this.groups.pop()
        }
      } else {
        word = (word ?? '') + c
        this.i++
      }
    }
    endCommand()
  }

  /** The rest of a double-quoted string, its commands read. */
  private quoted(depth: number): string {
    const s = this.s
    let out = ''
    while (this.i < s.length) {
      const c = s[this.i] as string
      const after = s[this.i + 1]
      if (c === '"') {
        this.i++
        return out
      }
      if (c === '\\' && after !== undefined && '"\\$`\n'.includes(after)) {
        if (after !== '\n') out += after
        this.i += 2
      } else if (c === '$' && after === '(') {
        this.i += 2
        this.commands(depth + 1, ')')
        out += '$(...)'
      } else if (c === '`') {
        this.i++
        this.commands(depth + 1, '`')
        out += '`...`'
      } else {
        out += c
        this.i++
      }
    }
    return out
  }

  /** Skips the bodies of the heredocs opened on the line just ended. */
  private heredocs(open: Array<{ tag: string; strip: boolean }>): void {
    for (const { tag, strip } of open) {
      while (this.i < this.s.length) {
        const end = this.s.indexOf('\n', this.i)
        const stop = end === -1 ? this.s.length : end
        const text = this.s.slice(this.i, stop)
        this.i = stop + 1
        if ((strip ? text.replace(/^\t+/, '') : text) === tag) break
      }
    }
  }
}

/** Commands that run the command after them, and their options taking a value. */
const WRAPPERS: Record<string, string[]> = {
  command: [],
  exec: [],
  nohup: [],
  time: [],
  nice: ['-n'],
  sudo: ['-u', '-g', '-C', '-D', '-h', '-p', '-U'],
  env: ['-u', '-C', '-S'],
  timeout: ['-s', '-k'],
}

/** The program and its arguments: environment assignments and wrappers left out. */
function argv(words: string[]): string[] {
  let at = 0
  for (;;) {
    while (at < words.length && /^[A-Za-z_][A-Za-z0-9_]*=/.test(words[at] as string)) at++
    const program = basename(words[at] ?? '')
    const valued = WRAPPERS[program]
    if (valued === undefined) break
    at++
    while (at < words.length && (words[at] as string).startsWith('-')) {
      if (valued.includes(words[at] as string)) at++
      at++
    }
    // `timeout` takes a duration first.
    if (program === 'timeout') at++
  }
  return words.slice(at)
}

function basename(path: string): string {
  return path.slice(path.lastIndexOf('/') + 1)
}

/** `path` from `base`, normalised; undefined when the shell would expand it or `base` is not known. */
export function resolveDir(base: string | undefined, path: string | undefined): string | undefined {
  if (path === undefined || path === '' || path === '-' || path.startsWith('~') || /[$`*?]/.test(path)) {
    return undefined
  }
  if (!path.startsWith('/') && base === undefined) return undefined
  const parts: string[] = []
  for (const part of (path.startsWith('/') ? path : `${base}/${path}`).split('/')) {
    if (part === '' || part === '.') continue
    if (part === '..') parts.pop()
    else parts.push(part)
  }
  return `/${parts.join('/')}`
}

/**
 * The forge actions of a successful shell command run in `cwd`, `output`
 * what it printed (`stderr` apart, which `gh` prints its messages on). A
 * leading `cd` moves the commands after it, up to the end of its subshell;
 * one the mod cannot follow leaves their directory unknown.
 */
export function commandActions(
  command: string,
  cwd: string | undefined,
  output: string,
  stderr = '',
): ForgeAction[] {
  if (!MAY_ACT.test(command)) return []
  const actions: ForgeAction[] = []
  // The directory each subshell moved to; a subshell starts in the one around it.
  const dirs = new Map<string, string | undefined>([['', cwd]])
  const dirOf = (group: string): string | undefined =>
    dirs.has(group) ? dirs.get(group) : dirOf(group.slice(0, group.lastIndexOf('/')))
  for (const { words, depth, group } of simpleCommands(command)) {
    const args = argv(words)
    const program = basename(args[0] ?? '')
    const dir = dirOf(group)
    if (program === 'cd' || program === 'pushd' || program === 'popd') {
      if (depth === 0) dirs.set(group, program === 'popd' ? undefined : resolveDir(dir, args[1]))
      continue
    }
    const found =
      program === 'git'
        ? gitAction(args.slice(1), dir)
        : program === 'gh'
          ? ghAction(args.slice(1), dir, stderr === '' ? output : `${output}\n${stderr}`)
          : program === 'az'
            ? azAction(args.slice(1), dir, output)
            : program === 'curl'
              ? curlAction(args.slice(1), dir)
              : program === 'http' || program === 'https' || program === 'xh'
                ? httpieAction(args.slice(1), dir)
                : null
    if (found !== null) actions.push(found)
  }
  return actions
}

/** Where a directory's remote is: the directory itself, or nowhere when not known. */
function inDir(dir: string | undefined, name?: string): Target {
  if (dir === undefined) return null
  return name === undefined ? { dir } : { dir, name }
}

/** Whether a push destination is a URL rather than a remote's name. */
export function isUrl(destination: string): boolean {
  return destination.includes('://') || /^[^/]*@[^/:]+:/.test(destination)
}

const GIT_VALUED = ['-C', '-c', '--git-dir', '--work-tree', '--namespace', '--config-env', '--super-prefix']
const PUSH_VALUED = ['-o', '--push-option', '--receive-pack', '--exec', '--repo']

/** `git [options] push [options] [<repository> [<refspec>...]]`. */
function gitAction(args: string[], cwd: string | undefined): ForgeAction | null {
  let dir = cwd
  let at = 0
  while (at < args.length && (args[at] as string).startsWith('-')) {
    const option = args[at] as string
    if (option === '-C') dir = resolveDir(dir, args[at + 1])
    at += GIT_VALUED.includes(option) ? 2 : 1
  }
  if (args[at] !== 'push') return null
  let destination: string | undefined
  for (let i = at + 1; i < args.length; i++) {
    const arg = args[i] as string
    if (['-n', '--dry-run', '-h', '--help'].includes(arg)) return null
    if (arg.startsWith('--repo=')) destination = arg.slice('--repo='.length)
    else if (arg === '--repo') destination = args[i + 1]
    if (PUSH_VALUED.includes(arg)) i++
    else if (!arg.startsWith('-') && destination === undefined) destination = arg
  }
  if (destination !== undefined && isUrl(destination)) return { types: PR, target: { remote: destination } }
  // A push to a local path changes no forge.
  if (destination !== undefined && /^[./]/.test(destination)) return null
  return { types: PR, target: inDir(dir, destination) }
}

// ---------------------------------------------------------------------------
// GitHub

const GITHUB_WEB = /https?:\/\/(?:www\.)?github\.com\/([\w.-]+)\/([\w.-]+)\/(pull|issues)\/(\d+)/g

/** The GitHub remote the brick recognises. */
export function githubRemote(owner: string, repo: string): string {
  return `https://github.com/${owner}/${repo.replace(/\.git$/, '')}`
}

/** `[HOST/]OWNER/REPO` or a repository URL, as `gh -R` takes it. */
function ghRepo(value: string): string | null {
  const parts = value
    .replace(/^https?:\/\//, '')
    .replace(/\/+$/, '')
    .split('/')
  if (parts.length === 2) return githubRemote(parts[0] as string, parts[1] as string)
  if (parts.length === 3) return `https://${parts[0]}/${parts[1]}/${(parts[2] as string).replace(/\.git$/, '')}`
  return null
}

/** A PR or issue page of GitHub printed in `text`, of `kind`, in `remote` when given. */
function printedGitHub(
  text: string,
  kind: 'pull' | 'issues',
  remote?: string,
): { remote: string; number: number } | null {
  for (const match of text.matchAll(GITHUB_WEB)) {
    const found = githubRemote(match[1] as string, match[2] as string)
    if (match[3] !== kind) continue
    if (remote !== undefined && found.toLowerCase() !== remote.toLowerCase()) continue
    return { remote: found, number: Number(match[4]) }
  }
  // `gh` says `Merged pull request o/r#12`, `Closed issue #3`.
  const said = new RegExp(`${kind === 'pull' ? 'pull request' : 'issue'} (?:([\\w.-]+)/([\\w.-]+))?#(\\d+)`, 'i').exec(
    text,
  )
  if (said === null) return null
  const found = said[1] === undefined ? remote : githubRemote(said[1], said[2] as string)
  if (found === undefined) return null
  if (remote !== undefined && found.toLowerCase() !== remote.toLowerCase()) return null
  return { remote: found, number: Number(said[3]) }
}

const PR_WRITE = new Set([
  'create',
  'merge',
  'close',
  'reopen',
  'ready',
  'edit',
  'review',
  'comment',
  'update-branch',
  'lock',
  'unlock',
])
const ISSUE_WRITE = new Set([
  'create',
  'edit',
  'close',
  'reopen',
  'comment',
  'delete',
  'transfer',
  'lock',
  'unlock',
  'develop',
  'pin',
  'unpin',
])
const GH_VALUED = new Set([
  '-R',
  '--repo',
  '-t',
  '--title',
  '-b',
  '--body',
  '-F',
  '--body-file',
  '-B',
  '--base',
  '-H',
  '--head',
  '-l',
  '--label',
  '--add-label',
  '--remove-label',
  '-a',
  '--assignee',
  '--add-assignee',
  '--remove-assignee',
  '-m',
  '--milestone',
  '-r',
  '--reviewer',
  '--add-reviewer',
  '--remove-reviewer',
  '-p',
  '--project',
  '--add-project',
  '--remove-project',
  '-T',
  '--template',
  '--reason',
  '--subject',
  '--match-head-commit',
  '--author-email',
  '-c',
  '--comment',
  '-n',
  '--name',
  '--branch-repo',
  '--base-branch',
  '-X',
  '--method',
  '-f',
  '--raw-field',
  '--field',
  '--input',
  '--header',
  '-q',
  '--jq',
  '--hostname',
  '--cache',
  '--preview',
  '-j',
  '--job',
  '--ref',
])

/** The short options of `GH_VALUED` that a verb reads as switches, taking nothing. */
const GH_SWITCHES = new Map([
  // `--merge`, `--rebase`.
  ['merge', ['-m', '-r']],
  // `--approve`, `--request-changes`, `--comment`.
  ['review', ['-a', '-r', '-c']],
])

/** `gh [-R repo] <group> <verb> [args]`. */
function ghAction(args: string[], dir: string | undefined, output: string): ForgeAction | null {
  let repo: string | undefined
  const positional: string[] = []
  for (let i = 0; i < args.length; i++) {
    const arg = args[i] as string
    if (arg === '-R' || arg === '--repo') repo = args[i + 1]
    else if (arg.startsWith('--repo=')) repo = arg.slice('--repo='.length)
    else if (arg.startsWith('-R') && arg.length > 2) repo = arg.slice(2)
    // A short option some verbs read as a switch: `gh pr merge -m` is `--merge`.
    const valued = GH_VALUED.has(arg) && !GH_SWITCHES.get(positional[1] ?? '')?.includes(arg)
    if (valued) i++
    else if (!arg.startsWith('-')) positional.push(arg)
  }
  const [group, verb, subject] = positional
  if (group === 'api') return ghApi(args, positional[1], dir)
  if (verb === undefined) return null
  const remote = repo === undefined ? undefined : (ghRepo(repo) ?? undefined)
  if (group === 'run' && ['rerun', 'cancel', 'delete'].includes(verb)) return withRepo(PIPELINES, remote, dir)
  if (group === 'workflow' && verb === 'run') return withRepo(PIPELINES, remote, dir)
  const kind =
    group === 'pr' && PR_WRITE.has(verb) ? 'pull' : group === 'issue' && ISSUE_WRITE.has(verb) ? 'issues' : null
  if (kind === null) return null
  const types = kind === 'issues' ? ISSUES : verb === 'merge' || verb === 'close' ? MERGE : PR
  // The PR or issue named, by number or URL; `create` names none.
  if (verb !== 'create' && subject !== undefined) {
    const number = /^#?(\d+)$/.exec(subject)
    if (number !== null) return { ...withRepo(types, remote, dir), number: Number(number[1]) }
    const url = printedGitHub(subject, kind)
    if (url !== null) return { types, target: { remote: url.remote }, number: url.number }
  }
  const printed = printedGitHub(output, kind, remote)
  if (printed !== null) return { types, target: { remote: printed.remote }, number: printed.number }
  return withRepo(types, remote, dir)
}

function withRepo(types: ForgeType[], remote: string | undefined, dir: string | undefined): ForgeAction {
  return { types, target: remote === undefined ? inDir(dir) : { remote } }
}

const WRITING = new Set(['POST', 'PATCH', 'PUT', 'DELETE'])

/** `gh api <endpoint>`, with a writing method. */
function ghApi(args: string[], endpoint: string | undefined, dir: string | undefined): ForgeAction | null {
  if (endpoint === undefined) return null
  let method: string | undefined
  let fields = false
  for (let i = 0; i < args.length; i++) {
    const arg = args[i] as string
    if (arg === '-X' || arg === '--method') method = args[i + 1]
    else if (arg.startsWith('--method=')) method = arg.slice('--method='.length)
    else if (arg.startsWith('-X') && arg.length > 2) method = arg.slice(2)
    else if (/^(-f|-F|--raw-field|--field|--input)(=|$)/.test(arg)) fields = true
  }
  const verb = (method ?? (fields ? 'POST' : 'GET')).toUpperCase()
  if (!WRITING.has(verb)) return null
  return githubPath(endpoint, dir)
}

/**
 * What a writing call to the GitHub REST API path `path` touches: under
 * `repos/<owner>/<repo>/`, its PRs, issues, runs and checks, or a push.
 * `{owner}` and `{repo}`, which `gh api` fills in, stand for the directory's
 * repository.
 */
export function githubPath(path: string, dir: string | undefined): ForgeAction | null {
  const segments =
    path
      .replace(/^(?:https?:\/\/)?api\.github\.com/i, '')
      .split(/[?#]/)[0]
      ?.split('/')
      .filter(Boolean) ?? []
  if (segments[0] !== 'repos' || segments.length < 4) return null
  const owner = segments[1] as string
  const repo = segments[2] as string
  const target: Target =
    owner === '{owner}' || repo === '{repo}' || owner === ':owner' ? inDir(dir) : { remote: githubRemote(owner, repo) }
  const [area, second, third] = segments.slice(3)
  const number = second !== undefined && /^\d+$/.test(second) ? Number(second) : undefined
  const action = (types: ForgeType[]): ForgeAction =>
    number === undefined ? { types, target } : { types, target, number }
  switch (area) {
    case 'pulls':
      return action(third === 'merge' ? MERGE : PR)
    case 'issues':
      return action(ISSUES)
    case 'actions':
    case 'check-runs':
    case 'check-suites':
    case 'statuses':
    case 'commits':
    case 'dispatches':
      return { types: PIPELINES, target }
    case 'git':
    case 'merges':
    case 'contents':
    case 'branches':
      return { types: PR, target }
    default:
      return null
  }
}

// ---------------------------------------------------------------------------
// Azure DevOps

const GUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i

/** The Azure DevOps remote the brick recognises. */
export function azureRemote(org: string, project: string, repo: string): string {
  const e = encodeURIComponent
  return `https://dev.azure.com/${e(org)}/${e(project)}/_git/${e(repo)}`
}

function decode(segment: string): string {
  try {
    return decodeURIComponent(segment)
  } catch {
    return segment
  }
}

/**
 * The names of an Azure DevOps remote, as the brick reads them: `https://
 * [<user>@]dev.azure.com/<org>/<project>/_git/<repo>`, `git@ssh.dev.azure.com:
 * v3/<org>/<project>/<repo>` and the old `visualstudio.com` forms. An SSH
 * alias is not resolved: null.
 */
export function azureNames(remote: string): Required<AzureNames> | null {
  const ssh =
    /^(?:ssh:\/\/)?[^@/]+@(?:ssh\.dev\.azure\.com|vs-ssh\.visualstudio\.com)[:/]v3\/([^/]+)\/([^/]+)\/([^/]+?)\/?$/.exec(
      remote,
    )
  if (ssh !== null)
    return { org: decode(ssh[1] as string), project: decode(ssh[2] as string), repo: decode(ssh[3] as string) }
  const https = /^https?:\/\/(?:[^@/]+@)?([^/]+)\/(.*?)\/?$/.exec(remote)
  if (https === null) return null
  const host = (https[1] as string).toLowerCase()
  let segments = (https[2] as string).split('/')
  let org: string
  if (host === 'dev.azure.com') {
    org = segments[0] ?? ''
    segments = segments.slice(1)
  } else if (host.endsWith('.visualstudio.com')) {
    org = host.slice(0, -'.visualstudio.com'.length)
    if (segments[0]?.toLowerCase() === 'defaultcollection') segments = segments.slice(1)
  } else {
    return null
  }
  const [a, b, c] = segments
  if (segments.length === 3 && b === '_git')
    return { org: decode(org), project: decode(a as string), repo: decode(c as string) }
  if (segments.length === 2 && a === '_git')
    return { org: decode(org), project: decode(b as string), repo: decode(b as string) }
  return null
}

/** The organisation of `--org`, a URL (`https://dev.azure.com/<org>`) or a bare name. */
function azureOrg(value: string): string | undefined {
  const url = /^https?:\/\/(?:[^@/]+@)?([^/]+)(?:\/([^/]+))?/.exec(value)
  if (url === null) return value === '' ? undefined : value
  const host = (url[1] as string).toLowerCase()
  if (host === 'dev.azure.com') return url[2] === undefined ? undefined : decode(url[2])
  if (host.endsWith('.visualstudio.com')) return host.slice(0, -'.visualstudio.com'.length)
  return undefined
}

/** Names that identify nothing the brick can match (an id) make the target unknown. */
function azureTarget(types: ForgeType[], names: AzureNames, dir: string | undefined, number?: number): ForgeAction {
  const ids = Object.values(names).some(name => name !== undefined && GUID.test(name))
  const target: Target = ids ? null : { dir, azure: names }
  return number === undefined ? { types, target } : { types, target, number }
}

const AZ_READ = new Set(['list', 'show', 'get', 'download', 'checkout', 'open'])

/** `az repos|boards|pipelines ... [--org] [--project] [--repository] [--id]`. */
function azAction(args: string[], dir: string | undefined, output: string): ForgeAction | null {
  const groups: string[] = []
  let at = 0
  while (at < args.length && !(args[at] as string).startsWith('-')) groups.push(args[at++] as string)
  const options: Record<string, string> = {}
  for (; at < args.length; at++) {
    const arg = args[at] as string
    if (!arg.startsWith('-')) continue
    const equals = arg.indexOf('=')
    if (equals !== -1) options[arg.slice(0, equals)] = arg.slice(equals + 1)
    else if (args[at + 1] !== undefined && !(args[at + 1] as string).startsWith('-'))
      options[arg] = args[++at] as string
  }
  const last = groups.at(-1)
  if (last === undefined || AZ_READ.has(last)) return null
  const [group, sub] = groups
  const types =
    group === 'repos'
      ? PR
      : group === 'pipelines'
        ? PIPELINES
        : group === 'boards' && sub === 'work-item'
          ? ISSUES
          : null
  if (types === null || groups.length < 2) return null
  const org = options['--org'] ?? options['--organization']
  const names: AzureNames = {}
  const orgName = org === undefined ? undefined : azureOrg(org)
  if (orgName !== undefined) names.org = orgName
  const project = options['--project'] ?? options['-p']
  if (project !== undefined) names.project = project
  const repo = group === 'repos' ? (options['--repository'] ?? options['-r']) : undefined
  if (repo !== undefined) names.repo = repo
  // Only a PR or a work item has a number: the id of a policy or a pipeline is not one.
  if (sub !== 'pr' && sub !== 'work-item') return azureTarget(types, names, dir)
  let number: number | undefined
  const id = options['--id']
  if (id !== undefined && /^\d+$/.test(id)) number = Number(id)
  if (number === undefined && last === 'create') number = printedId(output, sub === 'pr' ? 'pullRequestId' : 'id')
  return azureTarget(types, names, dir, number)
}

/** A number `az` printed as `key` of its JSON answer. */
function printedId(output: string, key: string): number | undefined {
  try {
    const value = (JSON.parse(output) as Record<string, unknown>)[key]
    return typeof value === 'number' && Number.isInteger(value) && value > 0 ? value : undefined
  } catch {
    return undefined
  }
}

/**
 * What a writing call to the Azure DevOps REST API at `url` touches: a
 * repository's PRs or pushes, work items, pipelines and builds.
 */
export function azurePath(url: string, dir: string | undefined): ForgeAction | null {
  const match = /^(?:https?:\/\/)?(?:[^@/]+@)?([^/]+)\/([^?#]*)/.exec(url)
  if (match === null) return null
  const host = (match[1] as string).toLowerCase()
  let segments = (match[2] as string).split('/').filter(Boolean).map(decode)
  const names: AzureNames = {}
  if (host === 'dev.azure.com') {
    if (segments[0] !== undefined) names.org = segments[0]
    segments = segments.slice(1)
  } else if (host.endsWith('.visualstudio.com')) {
    names.org = host.slice(0, -'.visualstudio.com'.length)
    if (segments[0]?.toLowerCase() === 'defaultcollection') segments = segments.slice(1)
  } else {
    return null
  }
  const apis = segments.indexOf('_apis')
  if (apis === -1 || apis > 1) return null
  if (apis === 1) names.project = segments[0] as string
  const [area, resource, id, more, moreId] = segments.slice(apis + 1).map(s => s.toLowerCase())
  const numeric = (value: string | undefined) =>
    value !== undefined && /^\d+$/.test(value) ? Number(value) : undefined
  if (area === 'git' && resource === 'repositories' && id !== undefined) {
    names.repo = segments[apis + 3] as string
    return azureTarget(PR, names, dir, more === 'pullrequests' ? numeric(moreId) : undefined)
  }
  if (area === 'git' && resource === 'pullrequests') return azureTarget(PR, names, dir, numeric(id))
  if (area === 'wit' && resource === 'workitems') return azureTarget(ISSUES, names, dir, numeric(id))
  if (area === 'build' || area === 'pipelines') return azureTarget(PIPELINES, names, dir)
  return null
}

// ---------------------------------------------------------------------------
// REST calls

const API = /^(?:https?:\/\/)?(?:api\.github\.com|(?:[^@/]+@)?dev\.azure\.com|[\w-]+\.visualstudio\.com)\//i

function restAction(url: string, dir: string | undefined): ForgeAction | null {
  return /api\.github\.com\//i.test(url) ? githubPath(url, dir) : azurePath(url, dir)
}

const CURL_DATA =
  /^(-d|--data|--data-raw|--data-binary|--data-urlencode|--data-ascii|--json|-F|--form|--form-string)(=|$)/
const CURL_VALUED = new Set([
  '-H',
  '--header',
  '-u',
  '--user',
  '-o',
  '--output',
  '-w',
  '--write-out',
  '-A',
  '--user-agent',
  '-e',
  '--referer',
  '-b',
  '--cookie',
  '-c',
  '--cookie-jar',
  '-x',
  '--proxy',
  '-K',
  '--config',
  '-m',
  '--max-time',
  '--retry',
  '--connect-timeout',
  '-r',
  '--range',
  '-E',
  '--cert',
  '--cacert',
  '--key',
  '--oauth2-bearer',
  '-U',
  '--proxy-user',
])

/**
 * Short options run together split apart, a value stuck to its option
 * included: `-sXPOST` is `-s -X POST`, `-sH x` is `-s -H x`.
 */
function curlArgs(args: string[]): string[] {
  const out: string[] = []
  for (const arg of args) {
    if (!/^-[^-]./.test(arg)) {
      out.push(arg)
      continue
    }
    for (let at = 1; at < arg.length; at++) {
      const option = `-${arg[at]}`
      out.push(option)
      const valued = CURL_VALUED.has(option) || CURL_DATA.test(option) || option === '-X' || option === '-T'
      if (valued && at + 1 < arg.length) {
        out.push(arg.slice(at + 1))
        break
      }
      if (valued) break
    }
  }
  return out
}

/** `curl` with a writing method to a forge's API. */
function curlAction(given: string[], dir: string | undefined): ForgeAction | null {
  const args = curlArgs(given)
  let method: string | undefined
  let implied: string | undefined
  let url: string | undefined
  for (let i = 0; i < args.length; i++) {
    const arg = args[i] as string
    if (arg === '-X' || arg === '--request') method = args[++i]
    else if (arg.startsWith('--request=')) method = arg.slice('--request='.length)
    else if (arg === '-T' || arg === '--upload-file') {
      implied = 'PUT'
      i++
    } else if (arg === '-G' || arg === '--get' || arg === '-I' || arg === '--head') implied = 'GET'
    else if (CURL_DATA.test(arg)) {
      implied ??= 'POST'
      if (!arg.includes('=')) i++
    } else if (arg === '--url') url ??= args[++i]
    else if (CURL_VALUED.has(arg)) i++
    else if (!arg.startsWith('-') && url === undefined) url = arg
  }
  if (url === undefined || !API.test(url)) return null
  if (!WRITING.has((method ?? implied ?? 'GET').toUpperCase())) return null
  return restAction(url, dir)
}

const HTTPIE_VALUED = new Set([
  '-a',
  '--auth',
  '-A',
  '--auth-type',
  '--session',
  '--session-read-only',
  '-o',
  '--output',
  '-p',
  '--print',
  '--proxy',
  '--timeout',
  '--verify',
  '--cert',
  '--cert-key',
  '--default-scheme',
])

/** `http|https|xh [METHOD] URL [items]`: data items imply POST, not query ones (`==`). */
function httpieAction(args: string[], dir: string | undefined): ForgeAction | null {
  const words: string[] = []
  for (let i = 0; i < args.length; i++) {
    const arg = args[i] as string
    if (HTTPIE_VALUED.has(arg)) i++
    else if (!arg.startsWith('-')) words.push(arg)
  }
  let method: string | undefined
  if (words[0] !== undefined && /^[A-Za-z]+$/.test(words[0]) && !API.test(words[0])) method = words.shift()
  const url = words.shift()
  if (url === undefined || !API.test(url)) return null
  const data = words.some(item => /^[^=:@]+(:=|=(?!=)|@)/.test(item))
  if (!WRITING.has((method ?? (data ? 'POST' : 'GET')).toUpperCase())) return null
  return restAction(url, dir)
}

// ---------------------------------------------------------------------------
// MCP tools

const READ_WORDS = new Set(['get', 'list', 'search', 'read', 'show', 'view', 'fetch', 'download', 'query', 'find'])
const WRITE_WORDS = new Set([
  'create',
  'update',
  'merge',
  'close',
  'comment',
  'add',
  'run',
  'rerun',
  'queue',
  'edit',
  'delete',
  'remove',
  'reopen',
  'assign',
  'link',
  'unlink',
  'push',
  'cancel',
  'trigger',
  'approve',
  'submit',
  'vote',
  'complete',
  'lock',
  'unlock',
  'write',
  'label',
  'dispatch',
  'set',
  'transfer',
  'resolve',
  'abandon',
  'publish',
])

/** The words of a tool's name, in lowercase: `create_pull_request`, `wit_updateWorkItem`. */
function nameWords(name: string): string[] {
  return name
    .split(/[_\-\s]+|(?<=[a-z0-9])(?=[A-Z])/)
    .filter(Boolean)
    .map(word => word.toLowerCase())
}

const NUMBER_PARAMS = [
  'pull_number',
  'pullNumber',
  'issue_number',
  'issueNumber',
  'pullRequestId',
  'workItemId',
  'number',
  'id',
]

/**
 * The forge action of a successful call to the MCP tool `tool`
 * (`mcp__<server>__<name>`), when its server is GitHub's or Azure DevOps'
 * and the tool writes: its types from the tool's name, the repository and
 * number from its parameters, else from the pages `output` names.
 */
export function mcpAction(
  tool: string,
  params: Record<string, unknown>,
  dir: string | undefined,
  output: string,
): ForgeAction | null {
  const split = tool.lastIndexOf('__')
  if (!tool.startsWith('mcp__') || split <= 4) return null
  const server = tool.slice(5, split).toLowerCase()
  const github = server.includes('github')
  const azure = /azure|devops|(^|[_-])ado([_-]|$)/.test(server)
  if (!github && !azure) return null
  const words = nameWords(tool.slice(split + 2))
  const has = (...wanted: string[]) => wanted.some(word => words.includes(word))
  if (has(...READ_WORDS) || !has(...WRITE_WORDS)) return null
  const joined = words.join('')
  const pr = joined.includes('pullrequest') || has('pr', 'prs')
  const issue = has('issue', 'issues', 'wit', 'subissue') || joined.includes('workitem')
  const pipeline =
    has('pipeline', 'pipelines', 'build', 'builds', 'workflow', 'workflows', 'runs', 'job', 'jobs') ||
    (has('run', 'rerun') && !pr && !issue)
  const push = has('file', 'files', 'push', 'commit', 'branch')
  const types = ordered([...(pr || pipeline || push ? PR : []), ...(issue || (pr && has('merge')) ? ISSUES : [])])
  if (types.length === 0) return null
  const text = (key: string) =>
    typeof params[key] === 'string' && params[key] !== '' ? (params[key] as string) : undefined
  let number: number | undefined
  for (const key of NUMBER_PARAMS) {
    const value = params[key]
    const parsed =
      typeof value === 'number' ? value : typeof value === 'string' && /^\d+$/.test(value) ? Number(value) : NaN
    if (Number.isInteger(parsed) && parsed > 0 && (key !== 'id' || pr || issue)) {
      number = parsed
      break
    }
  }
  if (github) {
    const owner = text('owner')
    const repo = text('repo') ?? text('repository')
    const known = owner !== undefined && repo !== undefined ? githubRemote(owner, repo) : undefined
    let target: Target = known === undefined ? inDir(dir) : { remote: known }
    if (number === undefined && (pr || issue)) {
      const printed = printedGitHub(output, pr ? 'pull' : 'issues', known)
      if (printed !== null) {
        number = printed.number
        target = { remote: printed.remote }
      }
    }
    return number === undefined ? { types, target } : { types, target, number }
  }
  const names: AzureNames = {}
  const org = text('organization') ?? text('org')
  if (org !== undefined) {
    const name = azureOrg(org)
    if (name !== undefined) names.org = name
  }
  const project = text('project')
  if (project !== undefined) names.project = project
  const repo = text('repositoryId') ?? text('repository') ?? text('repositoryName') ?? text('repo')
  if (repo !== undefined) names.repo = repo
  if (number === undefined) number = printedAzure(output, pr)
  return azureTarget(types, names, dir, number)
}

/** The number of an Azure DevOps PR or work item page named in `text`, or of the PR its JSON gives. */
function printedAzure(text: string, pr: boolean): number | undefined {
  const match = pr
    ? (/\/_git\/[^/\s]+\/pullrequest\/(\d+)/.exec(text) ?? /"pullRequestId"\s*:\s*(\d+)/.exec(text))
    : /\/_workitems\/edit\/(\d+)/.exec(text)
  return match === null ? undefined : Number(match[1])
}
