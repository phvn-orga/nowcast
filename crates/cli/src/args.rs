//! The command line.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Client of the nowcast service.
///
/// Exit status: 0 on success, 1 when the service answers with an error (its
/// code is printed on stderr), 2 on invalid usage, 3 when the service cannot
/// be reached or gives no answer within 10 s, 4 when the service answers
/// outside the contract.
#[derive(Debug, Parser)]
#[command(name = "nowcast", version)]
pub struct Cli {
    /// Path of the service's socket [default: $XDG_RUNTIME_DIR/nowcast/nowcast.sock]
    #[arg(long, global = true, env = "NOWCAST_SOCKET", value_name = "PATH")]
    pub socket: Option<PathBuf>,

    /// Session id to declare before any other call [default: cli-<pid>]
    ///
    /// From an agent's hook, pass the agent's session id.
    #[arg(long, global = true, env = "NOWCAST_SESSION", value_name = "ID")]
    pub session: Option<String>,

    /// Print JSON: one value per line, errors as {"error": {"code", "message"}} on stderr
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the events of the session's stream until interrupted
    ///
    /// Reconnects when the service goes away or is not there yet, and resumes
    /// from the last event seen.
    ///
    /// With --json, each line is one of:
    ///
    /// {"event": <name>, "data": <data>}: an event of the stream. An update's
    /// data.id is its id, the one --last-event-id takes.
    ///
    /// {"status": "connected"}: the stream is open. Printed at every
    /// connection, the first one included.
    ///
    /// {"status": "retrying", "reason": <text>}: the connection is lost or
    /// refused. Printed once per outage.
    Watch {
        /// Subscribe first, and subscribe again if the service lost the
        /// subscription. Repeatable.
        #[arg(long, num_args = 3, value_names = ["BRICK", "KIND", "PARAMS"])]
        subscribe: Vec<String>,

        /// Resume after this update id, the last one a previous watch printed
        ///
        /// The first connection then gets only the values newer than ID. One
        /// counter numbers every flow: a flow the previous watch did not
        /// follow gets its last value only if it is newer too.
        #[arg(long, value_name = "ID")]
        last_event_id: Option<String>,
    },

    /// Print the last value of a flow the anchor process is subscribed to
    ///
    /// Prints nothing (`null` with --json) when the flow has no value yet.
    Get {
        /// Flow id, as `subscribe` prints it: <brick>/<flow>
        flow: String,
    },

    /// Ask the brick to refresh a flow the anchor process is subscribed to
    ///
    /// Prints nothing. The brick publishes again once it has read its source:
    /// the new value comes on the stream, or through `get` afterwards.
    Refresh {
        /// Flow id, as `subscribe` prints it: <brick>/<flow>
        flow: String,
    },

    /// Subscribe to a brick and print the subscription, its flow and its last value
    ///
    /// The subscription lives as long as the anchor process: the agent when run
    /// from one of its hooks, this command itself when run from a plain shell.
    /// From a shell, use `watch --subscribe` to keep it.
    Subscribe {
        /// Brick name, for example `forge`
        brick: String,
        /// Kind of subscription the brick declares, for example `pr`
        kind: String,
        /// Content of the subscription, a JSON value [default: {}]
        params: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn watch_takes_repeated_subscriptions() {
        let cli = Cli::try_parse_from([
            "nowcast",
            "watch",
            "--subscribe",
            "forge",
            "pr",
            "{}",
            "--subscribe",
            "quota",
            "usage",
            r#"{"a":1}"#,
        ])
        .unwrap();
        let Command::Watch { subscribe, .. } = cli.command else {
            panic!("watch");
        };
        assert_eq!(
            subscribe,
            ["forge", "pr", "{}", "quota", "usage", r#"{"a":1}"#]
        );
    }

    #[test]
    fn a_subscription_needs_three_values() {
        assert!(Cli::try_parse_from(["nowcast", "watch", "--subscribe", "forge", "pr"]).is_err());
    }

    #[test]
    fn watch_takes_a_last_event_id() {
        let cli = Cli::try_parse_from(["nowcast", "watch", "--last-event-id", "42"]).unwrap();
        let Command::Watch { last_event_id, .. } = cli.command else {
            panic!("watch");
        };
        assert_eq!(last_event_id.as_deref(), Some("42"));
    }

    #[test]
    fn refresh_takes_a_flow() {
        let cli = Cli::try_parse_from(["nowcast", "refresh", "a/b"]).unwrap();
        let Command::Refresh { flow } = cli.command else {
            panic!("refresh");
        };
        assert_eq!(flow, "a/b");
        assert!(Cli::try_parse_from(["nowcast", "refresh"]).is_err());
    }

    #[test]
    fn global_options_go_anywhere() {
        let cli =
            Cli::try_parse_from(["nowcast", "get", "a/b", "--json", "--session", "s1"]).unwrap();
        assert!(cli.json);
        assert_eq!(cli.session.as_deref(), Some("s1"));
    }
}
