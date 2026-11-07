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

The full name trusts this formula alone, which Homebrew 6 and later require for a tap outside Homebrew's own. Then let systemd start the service on the first connection, with the units the formula installs:

```sh
mkdir -p ~/.config/systemd/user/sockets.target.wants
ln -sf "$(brew --prefix)"/opt/nowcast/share/nowcast/systemd/nowcast.{socket,service} ~/.config/systemd/user/
ln -sfn ../nowcast.socket ~/.config/systemd/user/sockets.target.wants/nowcast.socket
systemctl --user daemon-reload
systemctl --user start nowcast.socket
```

The socket is enabled by a link made by hand, through `~/.config/systemd/user`: `systemctl enable` would record the versioned path behind it, which the next `brew upgrade` removes.

### From source

For development, install the checkout into `~/.local/bin`, where the repository's unit runs it from:

```sh
cargo install --root ~/.local --path crates/cli
mkdir -p ~/.config/systemd/user
cp crates/daemon/systemd/nowcast.socket crates/daemon/systemd/nowcast.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now nowcast.socket
```

`nowcast daemon` also runs by hand, in the foreground, for a test. `nowcast daemon --help` lists the variables it reads.

## Bricks

A brick runs only once enabled, and only while something subscribes to it. The bricks this repository ships are built into the binary and enabled by name:

```sh
nowcast bricks enable claude-code
```

`claude-code` publishes the quotas of the Claude Code account and the measures of its running sessions. Any other brick is enabled by the path of its declaration file (`nowcast bricks enable path/to/brick.toml`), and `nowcast bricks disable <name>` turns one off.
