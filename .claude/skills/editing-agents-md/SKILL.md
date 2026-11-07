---
name: editing-agents-md
description: How to change an AGENTS.md or CLAUDE.md in this project, at the root or nested in any directory. Use every time one is created or edited, when adding a rule or a convention for agents, or when cleaning one up ("this CLAUDE.md is too long", "tidy AGENTS.md").
# baudrier editing-agents-md skill, version 1.
---

# Editing AGENTS.md and CLAUDE.md

The principles behind these steps are in the `harness` skill of the baudrier
plugin, under "What the agent reads".

## 1. Route the change

| The change is | It goes to |
|---|---|
| A rule a program can enforce | A check (lint, test, hook, CI), pointed to in one line |
| History, a decision, an incident | The design on the `design` branch (`docs/design/`) for decisions; GitHub issues and pull requests for the rest |
| An explanation of the code | Nowhere: the code and its comments carry it |
| Needed for a minority of tasks | A nested file or a skill |
| A directive almost every task needs | The file |

## 2. Check each line added, and the lines around it

- Without it, the agent would make a mistake. Otherwise, drop it.
- It is a directive. A reason is a link, never inline text.
- The agent could not learn it from the code, the configuration, the package
  scripts or the lint rules.
- No history: no date, "now", "we", "was removed", "previously", changelog.
- No other file owns the fact. If one does, point to it or say nothing.
- No "never", except for safety, data loss, or a mistake already observed.
- No session, person or ticket, unless that is the subject.
- Every path and command it cites exists.
- Where the directory holds both `CLAUDE.md` and `AGENTS.md`, one stays a
  symlink to the other, or a file whose only content is `@AGENTS.md`.

## 3. Respect the limits

- One amendment adds at most 300 characters per rule (a
  paragraph or a list item) and 1500 per contiguous block.
- The root file stays within 150 lines.
- A full rewrite is judged on the file budget only.

Past a limit, route the content again; never compress it into denser
sentences.

## 4. Run the check

```
node scripts/check-agents-md.mjs --base origin/main
```

Fix every violation it names, then run it again until it passes.
