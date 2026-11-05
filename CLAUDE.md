# nowcast

## The `design` branch

This branch holds the design of the whole project, under `docs/design/`. Design work
happens here and nowhere else. Once a part is settled, it becomes GitHub issues, and
implementation starts from those issues on branches of their own, never on this one.

Each document keeps what is decided apart from what is still open: an open question goes
under "À trancher", not written as a decision.

## A public repository, independent of its clients

The repository is public. No personal data in it: no names, home paths, private
repositories, accounts, tokens, or details of one person's machine.

nowcast stands on its own. The data it publishes is not shaped for any particular client:
an agent extension, a dashboard or another app connects to it the way it would to any
external service. When a need comes from one client, describe it as a capability any
client could use.
