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

Release by raising the workspace version in `Cargo.toml` in a pull request,
and nothing by hand: no tag, no formula edit. Merged into `main`, it publishes
the release and updates `Formula/`; a failed run is rerun as is.
`.github/workflows/release.yml` describes it.

The Claude Code plugins version apart from the service: a pull request raising
`version` in `clients/claude-code/nowcast/.claude-plugin/plugin.json` (and the
example's) releases them, once a batch is ready.

A change that makes the mod rely on a newer service raises `OLDEST_SERVICE` in
`clients/claude-code/nowcast/hooks/version.ts`.

## Harness

- After a finding that could recur beyond the current work (a review, a
  validation, a human report), ask what in the harness would have prevented
  it: a check first; otherwise a rule in the closest AGENTS.md; otherwise
  nothing.
- When a change removes the cause behind such a rule (a check now covers it,
  the code changed), update or remove the rule.
