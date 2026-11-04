# pulse

A local service that agent sessions (Claude Code today, others later) connect to. Each session subscribes to bricks (GitHub, Azure DevOps, quotas, Todoist, system resources...) and receives their updates live. The service starts on the first connection and stays idle while nobody listens.

Status: design. The design notes live on the `design` branch, under `docs/design/`.
