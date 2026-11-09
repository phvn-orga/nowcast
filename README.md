# nowcast

A local service that provides ready-to-use data: the state of a PR and its CI, usage quotas, tasks, system resources. Its consumers (agent mods and extensions for Claude Code, Codex or others, dashboards, any app) connect, subscribe to bricks and receive their updates live. The service starts on the first connection and stays idle while nobody listens.

Status: alpha. Experimental, for testing only: interfaces, formats and behaviour change without notice. The design notes live on the `design` branch, under `docs/design/`.

## Requirements

Linux 6.5 or later: the service identifies its peers with `SO_PEERPIDFD`.

## Install

One binary, `nowcast`: the command-line client, and the service itself as `nowcast daemon`. Both always ship at the same version. The Claude Code mod runs `nowcast` from the `PATH`.

### Homebrew (x86_64 Linux)

The formula lives in this repository and installs a prebuilt static binary:

```sh
brew tap phvn-orga/nowcast https://github.com/phvn-orga/nowcast
brew install phvn-orga/nowcast/nowcast
```

The full name trusts this formula alone, which Homebrew 6 and later require for a tap outside Homebrew's own. Then let systemd start the service on the first connection, and choose the bricks to enable:

```sh
nowcast setup
```

It writes the systemd user units into `~/.config/systemd/user`, the service running from Homebrew's opt path, which stays the same across upgrades, and enables the socket. After an upgrade that changes the units, `nowcast status` says to run it again, and the Claude Code mod shows it at the start of a session: it rewrites a unit that changed and leaves the rest. Before `brew uninstall`, `nowcast setup --remove` stops the service and removes its units.

### From source

For development, install the checkout into `~/.local/bin`, then set it up the same way, the service running from there:

```sh
cargo install --root ~/.local --path crates/cli
~/.local/bin/nowcast setup
```

`nowcast daemon` also runs by hand, in the foreground, for a test. `nowcast daemon --help` lists the variables it reads.

`nowcast status` tells what runs: the service's version beside the installed one, its bricks, sessions and flows. After an upgrade, the running service keeps the old version until it restarts (`systemctl --user stop nowcast.service`, and the socket starts the new one on the next connection).

## Bricks

A brick runs only once enabled, and only while something subscribes to it. The bricks this repository ships are built into the binary: `nowcast setup` offers them in a list to check, and they are enabled by name too, there (`nowcast setup claude-code`, the way to go without a terminal) or on their own:

```sh
nowcast bricks enable claude-code
```

`claude-code` publishes the quotas of the Claude Code account and the measures of its running sessions. Any other brick is enabled by the path of its declaration file (`nowcast bricks enable path/to/brick.toml`), and `nowcast bricks disable <name>` turns one off.

### The bricks' PATH

A brick whose command starts with a bare program name (`node`, `python3`) finds that program in the `PATH` its declaration sets in `[run] env`, or else in the service's. Under systemd, the service's `PATH` is the user manager's (`systemctl --user show-environment`), which lacks the runtimes a version manager installs per user (fnm, nvm, mise, asdf). `nowcast bricks enable` refuses a brick whose program is missing from it (`--force` enables it anyway), and a subscription to such a brick fails with the cause.

Any way that gives the service the program's directory works. Point it at a directory that stays put: for fnm, its default alias (`~/.local/share/fnm/aliases/default/bin`), not the per-shell `fnm_multishells` path. Depending on who should see it:

- **Every user service**: a file in `~/.config/environment.d/`, such as `PATH=$HOME/.local/share/fnm/aliases/default/bin:$PATH`, then `systemctl --user daemon-reload`. Name the file so that it sorts after the distribution's own, some of which set `PATH` outright (`/usr/lib/environment.d/99-environment.conf` on Ubuntu): `999-nowcast.conf`, for example.
- **The nowcast service alone**: a drop-in, through `systemctl --user edit nowcast.service`, with `Environment=PATH=…` under `[Service]`. systemd expands no variable there: write the whole `PATH`.
- **One brick**: `PATH` in its declaration's `[run] env`, which replaces the service's, or an absolute path as the program.

A new `PATH` reaches the service once it starts again: `systemctl --user stop nowcast.service`, and the socket starts it on the next connection.
