//! The command line.

use std::path::PathBuf;

use clap::parser::ValueSource;
use clap::{ArgMatches, Parser, Subcommand};

/// Client of the nowcast service, and the service itself (`nowcast daemon`).
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
        /// The first connection then gets every kept event newer than ID,
        /// across all flows, and `flow-gap <flow>` first for a flow where
        /// some of them are gone.
        #[arg(long, value_name = "ID")]
        last_event_id: Option<String>,
    },

    /// Print the last value of a flow the anchor process is subscribed to
    ///
    /// Prints nothing (`null` with --json) when the flow has no value yet.
    Get {
        /// Flow id, as `subscribe` prints it: <brick>/<flow>
        flow: String,

        /// Print instead every kept event newer than this id, oldest first
        ///
        /// Readable: `flow-gap <flow>` first when some of them are gone, then
        /// each event as without --after. --json: the service's answer,
        /// {"gap": <bool>, "events": [<event>...]}. 0 reads the whole kept
        /// history.
        #[arg(long, value_name = "ID")]
        after: Option<u64>,
    },

    /// Ask the brick to refresh a flow the anchor process is subscribed to
    ///
    /// Prints nothing. The brick publishes again once it has read its source:
    /// the new value comes on the stream, or through `get` afterwards.
    Refresh {
        /// Flow id, as `subscribe` prints it: <brick>/<flow>
        flow: String,
    },

    /// Send the JSON value read on stdin to the brick of a flow the anchor process is subscribed to
    ///
    /// Prints nothing. The flow's subscription kind must accept an input, and
    /// the value must match the type it accepts.
    Input {
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

    /// Run the service, as systemd starts it on the first connection
    ///
    /// Listens on the socket systemd passes, otherwise binds
    /// $XDG_RUNTIME_DIR/nowcast/nowcast.sock itself. Stops on SIGTERM or
    /// SIGINT, and once unused for 60 s when systemd passed the socket:
    /// NOWCAST_IDLE_TIMEOUT sets that delay in seconds, and also applies when
    /// started by hand. Logs go to stderr, filtered by NOWCAST_LOG (default:
    /// info).
    ///
    /// The client options --socket, --session and --json are refused;
    /// NOWCAST_SOCKET and NOWCAST_SESSION are ignored.
    Daemon,
}

/// The client options `daemon` refuses, as they were written on the command
/// line. Their variables do not count: a shell or an agent may export them
/// for the clients it runs.
pub const CLIENT_OPTIONS: [(&str, &str); 3] = [
    ("socket", "--socket"),
    ("session", "--session"),
    ("json", "--json"),
];

/// The client options given on the command line, among [`CLIENT_OPTIONS`].
pub fn client_options_given(matches: &ArgMatches) -> Vec<&'static str> {
    CLIENT_OPTIONS
        .iter()
        .filter(|(id, _)| matches.value_source(id) == Some(ValueSource::CommandLine))
        .map(|(_, flag)| *flag)
        .collect()
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

    fn daemon_matches(args: &[&str]) -> ArgMatches {
        let matches = Cli::command().try_get_matches_from(args).unwrap();
        assert_eq!(matches.subcommand_name(), Some("daemon"));
        matches
    }

    #[test]
    fn daemon_takes_no_client_option() {
        let none: [&str; 0] = [];
        assert_eq!(
            client_options_given(&daemon_matches(&["nowcast", "daemon"])),
            none
        );
        let matches = daemon_matches(&["nowcast", "--json", "daemon", "--socket", "/s"]);
        assert_eq!(client_options_given(&matches), ["--socket", "--json"]);
        let matches = daemon_matches(&["nowcast", "daemon", "--session", "s1"]);
        assert_eq!(client_options_given(&matches), ["--session"]);
    }

    #[test]
    fn get_after_takes_an_event_id() {
        let cli = Cli::try_parse_from(["nowcast", "get", "a/b", "--after", "42"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Get {
                after: Some(42),
                ..
            }
        ));
        assert!(Cli::try_parse_from(["nowcast", "get", "a/b", "--after", "x"]).is_err());
    }

    #[test]
    fn global_options_go_anywhere() {
        let cli =
            Cli::try_parse_from(["nowcast", "get", "a/b", "--json", "--session", "s1"]).unwrap();
        assert!(cli.json);
        assert_eq!(cli.session.as_deref(), Some("s1"));
    }
}
