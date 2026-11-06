# nowcast

A local service that provides ready-to-use data: the state of a PR and its CI, usage quotas, tasks, system resources. Its consumers (agent mods and extensions for Claude Code, Codex or others, dashboards, any app) connect, subscribe to bricks and receive their updates live. The service starts on the first connection and stays idle while nobody listens.

Status: implementation starting. The design notes live on the `design` branch, under `docs/design/`.

## Requirements

Linux 6.5 or later: the service identifies its peers with `SO_PEERPIDFD`.

## Install

One binary, `nowcast`: the command-line client, and the service itself as `nowcast daemon`. Both always ship at the same version.

Install it in `~/.local/bin`, which is usually on the `PATH`, unlike `~/.cargo/bin`:

```sh
cargo install --root ~/.local --path crates/cli
```

The Claude Code mod runs `nowcast` from the `PATH`, and the systemd unit runs `%h/.local/bin/nowcast daemon`. Installed elsewhere, edit `ExecStart=` in `nowcast.service`.

Then let systemd start the service on the first connection:

```sh
mkdir -p ~/.config/systemd/user
cp crates/daemon/systemd/nowcast.socket crates/daemon/systemd/nowcast.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now nowcast.socket
```

`nowcast daemon` also runs by hand, in the foreground, for a test. `nowcast daemon --help` lists the variables it reads.
