# nowcast-cli

- Run `nowcast setup` only with the binary the user's service runs: from a
  development build (`target/debug`, `cargo run`), it points the service's
  `ExecStart` at that build, and `nowcast status` reports `outdated` for that
  reason alone. Exercise setup as `tests/setup.rs` does: a temporary
  `XDG_CONFIG_HOME` and a stub `systemctl`.
