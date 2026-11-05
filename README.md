# nowcast

A local service that provides ready-to-use data: the state of a PR and its CI, usage quotas, tasks, system resources. Its consumers (agent mods and extensions for Claude Code, Codex or others, dashboards, any app) connect, subscribe to bricks and receive their updates live. The service starts on the first connection and stays idle while nobody listens.

Status: design. The design notes live on the `design` branch, under `docs/design/`.
