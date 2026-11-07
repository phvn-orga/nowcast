# nowcast

## Design

The design lives on the `design` branch, in `docs/design/`, written in French.
Design pull requests target `design`, not `main`. A code change that reverses
or extends a decision recorded there updates the design in its own pull
request.

## Checks

Locally, run only what the change touches:

- a Rust crate: `cargo test -p nowcast-<crate>`;
- the Claude Code client, from `clients/claude-code`: `nr typecheck` and
  `nr test`, after `nr types` once in a fresh worktree (it fetches the plugin
  API's types);
- the static release binary, after a dependency change: `docker build -f
  build/release.Dockerfile --output dist .`, the container the release workflow
  runs.

The CI (`.github/workflows/ci.yml`) runs the whole workspace and is the judge.

## Release

Release with `gh workflow run release.yml -f version=<version>`, and nothing
by hand: no version edit, no tag. The run sets the version, tags, publishes and
updates the Homebrew formula in `Formula/`, merging its own pull requests;
`.github/workflows/release.yml` describes it.

## Harness

- After a finding that could recur beyond the current work (a review, a
  validation, a human report), ask what in the harness would have prevented
  it: a check first; otherwise a rule in the closest AGENTS.md; otherwise
  nothing.
- When a change removes the cause behind such a rule (a check now covers it,
  the code changed), update or remove the rule.
