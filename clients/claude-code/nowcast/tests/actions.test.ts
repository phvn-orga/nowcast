// The forge actions read from a tool call (`06-brique-forge.md` on the design
// branch, "Ce que reconnaît le client"): shell commands, REST calls and MCP
// tools, without the engine.

import { expect, test } from 'claude-code/testing'
import { azureNames, branchMoves, commandActions, mcpAction, resolveDir, simpleCommands } from '../hooks/actions'
import { azure } from '../hooks/forge'

const CWD = '/lab/repo'
const PR = ['pr', 'pipelines']
const MERGE = ['pr', 'pipelines', 'issues']
const GH = 'https://github.com/o/r'
const ADO = 'https://dev.azure.com/org/proj/_git/repo'

const actions = (command: string, output = '') => commandActions(command, CWD, output)

test('a command line splits into simple commands as the shell reads it', () => {
  const words = (line: string) => simpleCommands(line).map(c => c.words)
  expect(words('a && b || c; d | e & f')).toEqual([['a'], ['b'], ['c'], ['d'], ['e'], ['f']])
  expect(words(`git commit -m 'a && b' && echo "x; \\"y\\"" \\\n  z`)).toEqual([
    ['git', 'commit', '-m', 'a && b'],
    ['echo', 'x; "y"', 'z'],
  ])
  expect(words('make 2>&1 >/dev/null | tail -n 3')).toEqual([['make'], ['tail', '-n', '3']])
  expect(words('(cd a && b) # git push')).toEqual([['cd', 'a'], ['b']])
  // A heredoc's body is text, not commands, even inside `$(...)`.
  const commit = `git commit -m "$(cat <<'EOF'\nfix: don't git push here\nEOF\n)" && git push`
  expect(words(commit)).toEqual([['cat'], ['git', 'commit', '-m', '$(...)'], ['git', 'push']])
  expect(simpleCommands('url=$(gh pr create -f)').map(c => [c.words, c.depth])).toEqual([
    [['gh', 'pr', 'create', '-f'], 1],
    [['url=$(...)'], 0],
  ])
})

test('directories resolve from the one before, and what the shell expands is unknown', () => {
  expect(resolveDir('/a/b', '../c/./d')).toBe('/a/c/d')
  expect(resolveDir(undefined, '/x')).toBe('/x')
  expect(resolveDir(undefined, 'x')).toBeUndefined()
  expect(resolveDir('/a', '~/x')).toBeUndefined()
  expect(resolveDir('/a', '$HOME')).toBeUndefined()
  expect(resolveDir('/a', '-')).toBeUndefined()
})

test('git push, in any form, refreshes the PR and pipelines of its push remote', () => {
  expect(actions('git push')).toEqual([{ types: PR, target: { dir: CWD } }])
  expect(actions('git push -u origin HEAD')).toEqual([{ types: PR, target: { dir: CWD, name: 'origin' } }])
  expect(actions('git push --force-with-lease -o ci.skip fork feature')).toEqual([
    { types: PR, target: { dir: CWD, name: 'fork' } },
  ])
  expect(actions('cd ../other && GIT_TRACE=1 git -c a=b -C sub push')).toEqual([
    { types: PR, target: { dir: '/lab/other/sub' } },
  ])
  expect(actions('git -C /x/y --no-pager push --repo=up')).toEqual([{ types: PR, target: { dir: '/x/y', name: 'up' } }])
  expect(actions('git push git@github.com:o/r.git HEAD:main')).toEqual([
    { types: PR, target: { remote: 'git@github.com:o/r.git' } },
  ])
  expect(actions('timeout 60 env -u X git push')).toEqual([{ types: PR, target: { dir: CWD } }])
  // A directory the mod cannot follow leaves the repository unknown.
  expect(actions('cd ~/x && git push')).toEqual([{ types: PR, target: null }])
  // A subshell's `cd` ends with it.
  expect(actions('(cd sub && git push) && git push && (git push)')).toEqual([
    { types: PR, target: { dir: '/lab/repo/sub' } },
    { types: PR, target: { dir: CWD } },
    { types: PR, target: { dir: CWD } },
  ])
  expect(commandActions('git push', undefined, '')).toEqual([{ types: PR, target: null }])
})

test('read-only git and gh commands, and a dry run, refresh nothing', () => {
  for (const command of [
    'git fetch && git status',
    'git push --dry-run',
    'git push -n origin',
    'git push ./local',
    'gh pr view 12 && gh pr list && gh pr status && gh pr checks && gh pr diff',
    'gh issue view 3; gh issue list',
    'echo git push',
    'gh run list && gh run view 5 && gh workflow list',
    'ls',
  ]) {
    expect([command, actions(command)]).toEqual([command, []])
  }
})

test('gh pr takes its number and repository from the arguments, else from what gh printed', () => {
  expect(actions('gh pr merge 12 --squash -d')).toEqual([{ types: MERGE, target: { dir: CWD }, number: 12 }])
  expect(actions('gh pr merge -m 12')).toEqual([{ types: MERGE, target: { dir: CWD }, number: 12 }])
  expect(actions('gh pr merge -r 12')).toEqual([{ types: MERGE, target: { dir: CWD }, number: 12 }])
  expect(actions('gh pr review -a 12')).toEqual([{ types: PR, target: { dir: CWD }, number: 12 }])
  // `gh` prints its messages on stderr.
  expect(commandActions('gh issue close', CWD, '', '✓ Closed issue o/r#3 (title)')).toEqual([
    { types: ['issues'], target: { remote: GH }, number: 3 },
  ])
  expect(actions("gh pr close '#7' -R o/r -c done")).toEqual([{ types: MERGE, target: { remote: GH }, number: 7 }])
  expect(actions('gh -R github.com/o/r pr ready 3')).toEqual([{ types: PR, target: { remote: GH }, number: 3 }])
  expect(actions('gh pr comment https://github.com/x/y/pull/9 --body "a b"')).toEqual([
    { types: PR, target: { remote: 'https://github.com/x/y' }, number: 9 },
  ])
  expect(actions('gh pr create --title "x" --body-file - --milestone 3', 'https://github.com/o/r/pull/41\n')).toEqual([
    { types: PR, target: { remote: GH }, number: 41 },
  ])
  expect(actions('gh pr merge --auto', '✓ Merged pull request o/r#5 (title)')).toEqual([
    { types: MERGE, target: { remote: GH }, number: 5 },
  ])
  expect(actions('gh pr edit --add-label bug')).toEqual([{ types: PR, target: { dir: CWD } }])
  expect(actions('gh pr review --approve feature-branch')).toEqual([{ types: PR, target: { dir: CWD } }])
  expect(actions('gh pr update-branch 4 --rebase')).toEqual([{ types: PR, target: { dir: CWD }, number: 4 }])
})

test('gh issue refreshes issues; gh run and gh workflow run refresh the pipelines', () => {
  expect(actions('gh issue create -R o/other -t x -b y', 'https://github.com/o/other/issues/8')).toEqual([
    { types: ['issues'], target: { remote: 'https://github.com/o/other' }, number: 8 },
  ])
  expect(actions('gh issue close 3 --reason "not planned"')).toEqual([
    { types: ['issues'], target: { dir: CWD }, number: 3 },
  ])
  expect(actions('gh issue comment 3 -b hi && gh issue develop 4 --name b')).toEqual([
    { types: ['issues'], target: { dir: CWD }, number: 3 },
    { types: ['issues'], target: { dir: CWD }, number: 4 },
  ])
  expect(actions('gh run rerun 123 --failed')).toEqual([{ types: PR, target: { dir: CWD } }])
  expect(actions('gh run cancel 1 -R o/r')).toEqual([{ types: PR, target: { remote: GH } }])
  expect(actions('gh workflow run ci.yml --ref main')).toEqual([{ types: PR, target: { dir: CWD } }])
})

test('gh api maps a writing call to its path', () => {
  expect(actions('gh api -X PATCH repos/o/r/pulls/12 -f state=closed')).toEqual([
    { types: PR, target: { remote: GH }, number: 12 },
  ])
  expect(actions('gh api repos/o/r/pulls/12/merge -X PUT')).toEqual([
    { types: MERGE, target: { remote: GH }, number: 12 },
  ])
  // A field implies POST.
  expect(actions("gh api repos/{owner}/{repo}/issues -f title='x'")).toEqual([
    { types: ['issues'], target: { dir: CWD } },
  ])
  expect(actions('gh api --method=POST /repos/o/r/issues/3/comments --input body.json')).toEqual([
    { types: ['issues'], target: { remote: GH }, number: 3 },
  ])
  expect(actions('gh api -XPOST repos/o/r/actions/runs/9/rerun')).toEqual([{ types: PR, target: { remote: GH } }])
  // Reading, or a path outside a repository's PRs, issues and runs.
  expect(actions('gh api repos/o/r/pulls/12')).toEqual([])
  expect(actions('gh api repos/o/r/pulls -X GET -f state=open')).toEqual([])
  expect(actions('gh api -X POST repos/o/r/labels -f name=x')).toEqual([])
  expect(actions("gh api graphql -f query='mutation { x }'")).toEqual([])
})

test('curl and http calls that write to a forge API are mapped like gh api', () => {
  expect(
    actions('curl -s -X POST -H "Authorization: Bearer $T" https://api.github.com/repos/o/r/issues -d \'{}\''),
  ).toEqual([{ types: ['issues'], target: { remote: GH } }])
  expect(
    actions('curl --json \'{"merge_method":"squash"}\' -XPUT https://api.github.com/repos/o/r/pulls/2/merge'),
  ).toEqual([{ types: MERGE, target: { remote: GH }, number: 2 }])
  expect(actions('curl -d x=1 https://api.github.com/repos/o/r/pulls/2/reviews')).toEqual([
    { types: PR, target: { remote: GH }, number: 2 },
  ])
  expect(actions('curl https://api.github.com/repos/o/r/pulls/2')).toEqual([])
  // Short options run together.
  expect(actions('curl -sX POST -sH "Authorization: x" https://api.github.com/repos/o/r/issues')).toEqual([
    { types: ['issues'], target: { remote: GH } },
  ])
  expect(actions('curl -sXPATCH https://api.github.com/repos/o/r/issues/4')).toEqual([
    { types: ['issues'], target: { remote: GH }, number: 4 },
  ])
  expect(actions('curl -X POST https://example.com/repos/o/r/issues')).toEqual([])
  expect(
    actions(
      'curl -u :$PAT -X PATCH "https://dev.azure.com/org/proj/_apis/git/repositories/repo/pullrequests/5?api-version=7.1"',
    ),
  ).toEqual([{ types: PR, target: { dir: CWD, azure: { org: 'org', project: 'proj', repo: 'repo' } }, number: 5 }])
  expect(actions('curl -X PATCH https://org.visualstudio.com/My%20Proj/_apis/wit/workitems/42')).toEqual([
    { types: ['issues'], target: { dir: CWD, azure: { org: 'org', project: 'My Proj' } }, number: 42 },
  ])
  expect(actions('curl -X POST https://dev.azure.com/org/proj/_apis/pipelines/3/runs')).toEqual([
    { types: PR, target: { dir: CWD, azure: { org: 'org', project: 'proj' } } },
  ])
  // A repository named by its id cannot be matched: every flow of the kinds.
  const id = '0f5b3c4e-1a2b-4c3d-8e9f-001122334455'
  expect(actions(`curl -X POST https://dev.azure.com/org/proj/_apis/git/repositories/${id}/pullrequests`)).toEqual([
    { types: PR, target: null },
  ])
  expect(actions('http POST https://api.github.com/repos/o/r/issues title=x')).toEqual([
    { types: ['issues'], target: { remote: GH } },
  ])
  expect(actions('xh api.github.com/repos/o/r/issues/1/comments body=hi')).toEqual([
    { types: ['issues'], target: { remote: GH }, number: 1 },
  ])
  expect(actions('http https://api.github.com/repos/o/r/issues')).toEqual([])
  expect(actions('http https://api.github.com/repos/o/r/issues state==open')).toEqual([])
  expect(actions('http -a u:t POST https://api.github.com/repos/o/r/issues')).toEqual([
    { types: ['issues'], target: { remote: GH } },
  ])
})

test('az commands that write refresh by group, the names from their flags', () => {
  expect(
    actions(
      'az repos pr create --title x --repository repo --project proj --org https://dev.azure.com/org/',
      '{"pullRequestId": 77}',
    ),
  ).toEqual([{ types: PR, target: { dir: CWD, azure: { org: 'org', project: 'proj', repo: 'repo' } }, number: 77 }])
  expect(actions('az repos pr update --id 5 --status completed')).toEqual([
    { types: PR, target: { dir: CWD, azure: {} }, number: 5 },
  ])
  expect(actions('az repos pr reviewer add --id 5 --reviewers a@b.c')).toEqual([
    { types: PR, target: { dir: CWD, azure: {} }, number: 5 },
  ])
  expect(actions('az boards work-item create --type Bug --title x -p "My Proj"', '{"id": 12, "fields": {}}')).toEqual([
    { types: ['issues'], target: { dir: CWD, azure: { project: 'My Proj' } }, number: 12 },
  ])
  expect(actions('az boards work-item relation add --id 3 --relation-type parent --target-id 4')).toEqual([
    { types: ['issues'], target: { dir: CWD, azure: {} }, number: 3 },
  ])
  expect(actions('az pipelines run --name ci --organization=https://org.visualstudio.com')).toEqual([
    { types: PR, target: { dir: CWD, azure: { org: 'org' } } },
  ])
  // The id a create prints is a PR's or a work item's only.
  expect(actions('az repos policy create --config p.json', '{"id": 3}')).toEqual([
    { types: PR, target: { dir: CWD, azure: {} } },
  ])
  expect(actions('az pipelines build queue --definition-id 4')).toEqual([
    { types: PR, target: { dir: CWD, azure: {} } },
  ])
  for (const command of [
    'az repos pr list',
    'az repos pr show --id 5',
    'az boards work-item show --id 3',
    'az boards iteration project create',
    'az pipelines runs list',
    'az login',
  ]) {
    expect([command, actions(command)]).toEqual([command, []])
  }
})

test('an Azure DevOps remote is completed from the directory, a work item by its project', () => {
  const here = azureNames('git@ssh.dev.azure.com:v3/Org/Proj/Repo')
  expect(here).toEqual({ org: 'Org', project: 'Proj', repo: 'Repo' })
  expect(azureNames('https://org@dev.azure.com/org/My%20Proj/_git/repo')).toEqual({
    org: 'org',
    project: 'My Proj',
    repo: 'repo',
  })
  expect(azureNames('https://org.visualstudio.com/DefaultCollection/proj/_git/repo')).toEqual({
    org: 'org',
    project: 'proj',
    repo: 'repo',
  })
  expect(azureNames('https://dev.azure.com/org/_git/proj')).toEqual({ org: 'org', project: 'proj', repo: 'proj' })
  expect(azureNames('git@github.com:o/r.git')).toBeNull()

  expect(azure({}, here, ['pr', 'pipelines'])).toBe('https://dev.azure.com/Org/Proj/_git/Repo')
  expect(azure({ org: 'org', project: 'proj' }, here, ['pr'])).toBe('https://dev.azure.com/org/proj/_git/Repo')
  expect(azure({ project: 'Other' }, here, ['pr'])).toBeNull()
  expect(azure({ project: 'My Proj' }, here, ['issues'])).toBe('https://dev.azure.com/Org/My%20Proj/_git/My%20Proj')
  expect(azure({ org: 'elsewhere', project: 'p' }, here, ['issues'])).toBe('https://dev.azure.com/elsewhere/p/_git/p')
  expect(azure({ org: 'elsewhere', project: 'p' }, here, ['pr'])).toBeNull()
  expect(azure({}, null, ['issues'])).toBeNull()
})

test('MCP tools of GitHub and Azure DevOps servers that write are classified by name', () => {
  const mcp = (tool: string, params: Record<string, unknown> = {}, output = '') => mcpAction(tool, params, CWD, output)
  expect(mcp('mcp__github__merge_pull_request', { owner: 'o', repo: 'r', pull_number: 4 })).toEqual({
    types: MERGE,
    target: { remote: GH },
    number: 4,
  })
  expect(
    mcp(
      'mcp__claude_ai_GitHub__create_pull_request',
      { owner: 'o', repo: 'r' },
      '{"html_url":"https://github.com/o/r/pull/9"}',
    ),
  ).toEqual({
    types: PR,
    target: { remote: GH },
    number: 9,
  })
  expect(mcp('mcp__github__add_issue_comment', { owner: 'o', repo: 'r', issue_number: '3' })).toEqual({
    types: ['issues'],
    target: { remote: GH },
    number: 3,
  })
  expect(mcp('mcp__github__issue_write', { method: 'create' })).toEqual({ types: ['issues'], target: { dir: CWD } })
  expect(mcp('mcp__github__push_files', { owner: 'o', repo: 'r', branch: 'b' })).toEqual({
    types: PR,
    target: { remote: GH },
  })
  expect(mcp('mcp__github__rerun_workflow_run', { owner: 'o', repo: 'r', run_id: 5 })).toEqual({
    types: PR,
    target: { remote: GH },
  })
  expect(
    mcp('mcp__azure-devops__repo_update_pull_request', { project: 'proj', repositoryId: 'repo', pullRequestId: 6 }),
  ).toEqual({
    types: PR,
    target: { dir: CWD, azure: { project: 'proj', repo: 'repo' } },
    number: 6,
  })
  expect(mcp('mcp__ado__wit_add_work_item_comment', { project: 'proj', workItemId: 8 })).toEqual({
    types: ['issues'],
    target: { dir: CWD, azure: { project: 'proj' } },
    number: 8,
  })
  expect(mcp('mcp__ado__wit_update_work_item', { id: 9 })).toEqual({
    types: ['issues'],
    target: { dir: CWD, azure: {} },
    number: 9,
  })
  expect(mcp('mcp__ado__repo_create_pull_request', { project: 'proj' }, '{"pullRequestId": 15}')).toEqual({
    types: PR,
    target: { dir: CWD, azure: { project: 'proj' } },
    number: 15,
  })
  expect(mcp('mcp__ado__pipelines_run_pipeline', { project: 'proj', pipelineId: 2 })).toEqual({
    types: PR,
    target: { dir: CWD, azure: { project: 'proj' } },
  })
  for (const tool of [
    'mcp__github__get_pull_request',
    'mcp__github__list_issues',
    'mcp__github__pull_request_read',
    'mcp__github__get_pull_request_comments',
    'mcp__github__list_workflow_runs',
    'mcp__ado__wit_get_work_item',
    'mcp__slack__create_issue',
    'mcp__claude_ai_Todoist__add-tasks',
    'Bash',
  ]) {
    expect([tool, mcp(tool, { owner: 'o', repo: 'r' })]).toEqual([tool, null])
  }
})

test('the commands that may move a branch name the directory they ran in', () => {
  const moves = (command: string) => branchMoves(command, CWD)
  expect(
    moves('git status && git log && git branch new && git push origin HEAD && gh pr view 3 && git worktree list'),
  ).toEqual([])
  expect(moves('git switch -c x')).toEqual([CWD])
  expect(moves('cd ../other && git checkout main')).toEqual(['/lab/other'])
  expect(moves('git -C /w branch -m new')).toEqual(['/w'])
  expect(moves('git branch --set-upstream-to=origin/x')).toEqual([CWD])
  expect(moves('git branch -u origin/x')).toEqual([CWD])
  // A branch named besides may be checked out in another worktree.
  expect(moves('git branch -m old new')).toEqual([undefined])
  expect(moves('git branch -u origin/x feat')).toEqual([undefined])
  expect(moves('git branch --unset-upstream feat')).toEqual([undefined])
  expect(moves('git commit -m x && git push -u origin HEAD')).toEqual([CWD])
  expect(moves('gh -R o/r pr checkout 12')).toEqual([CWD])
  // Any worktree: `git worktree`, or a directory the shell expands.
  expect(moves('git worktree add ../x -b x')).toEqual([undefined])
  expect(moves('cd "$DIR" && git switch x')).toEqual([undefined])
})
