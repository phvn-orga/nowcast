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

    /// Ask the brick to refresh a flow the anchor process is subscribed to, or every flow of a brick
    ///
    /// Prints nothing. The brick publishes again once it has read its source:
    /// the new value comes on the stream, or through `get` afterwards.
    /// With --brick, every live flow of the brick is refreshed, whoever
    /// subscribed to it; none at all is no error.
    Refresh {
        /// Flow id, as `subscribe` prints it: <brick>/<flow>
        #[arg(required_unless_present = "brick", conflicts_with = "brick")]
        flow: Option<String>,
        /// Refresh every live flow of this brick instead
        #[arg(long, value_name = "NAME")]
        brick: Option<String>,
        /// With --brick, only the flows of this subscription kind
        #[arg(long, requires = "brick", conflicts_with = "flow")]
        kind: Option<String>,
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

    /// Print what runs: the service and its version beside this tool's, the
    /// bricks, the sessions with their subscriptions, and the active flows
    ///
    /// Never starts the service: when systemd holds the socket and
    /// nowcast.service is not running, it says the service is stopped without
    /// connecting. It asks systemd only when the socket is not given (--socket,
    /// NOWCAST_SOCKET).
    ///
    /// Says when `nowcast setup` has to run: its units are missing, or differ
    /// from the ones this binary writes (after an upgrade, for one).
    ///
    /// With --json, prints {"installed": <this tool's version>, "running":
    /// <the service's GET /v1/status answer, null when stopped>, "setup":
    /// "current" | "outdated" | "missing", null when it cannot tell}.
    Status,

    /// Restart the service systemd runs, so that it runs the installed binary
    ///
    /// Prints the service's version before and after; it restarts even when
    /// they are the same. Sessions see a short reconnection: systemd holds the
    /// socket meanwhile, and clients declare their session and subscribe
    /// again. Each flow's history is lost.
    ///
    /// A service stopped while systemd holds its socket is left stopped: the
    /// next connection starts the installed binary. Refused with a socket
    /// given (--socket, NOWCAST_SOCKET), without the units `nowcast setup`
    /// writes, or with outdated ones: run `nowcast setup` first.
    ///
    /// With --json, prints {"before": <version>, "after": <version>}, a
    /// version null when the service does not tell it, both null when the
    /// service was stopped.
    Reload,

    /// Enable or disable a brick
    ///
    /// Acts on the bricks directory the service follows,
    /// $XDG_CONFIG_HOME/nowcast/bricks/, where a link <name>.toml activates a
    /// brick.
    Bricks {
        #[command(subcommand)]
        action: BricksAction,
    },

    /// Let systemd start the service on the first connection, and enable native bricks
    ///
    /// Writes nowcast.socket and nowcast.service into
    /// $XDG_CONFIG_HOME/systemd/user/, the service running this binary
    /// (through Homebrew's opt path when Homebrew installed it), then enables
    /// the socket. Run again, after an upgrade for one, it rewrites a unit
    /// that differs and changes nothing else.
    ///
    /// The native bricks named are enabled. With none, in a terminal, it asks
    /// which native bricks to enable, the enabled ones checked: unchecking
    /// one disables it. Elsewhere it enables none. Any other brick is enabled
    /// with `nowcast bricks enable`.
    Setup {
        /// Native bricks to enable, by name (claude-code)
        #[arg(conflicts_with = "remove")]
        bricks: Vec<String>,
        /// Stop the service and remove its units instead, before
        /// uninstalling nowcast. The bricks stay as they are.
        #[arg(long)]
        remove: bool,
    },

    /// Run a brick native to nowcast on stdin and stdout, as the service launches it
    #[command(hide = true)]
    Brick {
        /// The native brick's name
        name: String,
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

#[derive(Debug, Subcommand)]
pub enum BricksAction {
    /// Enable a brick: a brick native to nowcast by its name (claude-code, forge),
    /// or any brick by the path of its declaration file
    ///
    /// A native brick's declaration and schemas are written under
    /// $XDG_DATA_HOME/nowcast/bricks/<name>/ first; the service keeps them in
    /// step with its own version.
    ///
    /// A brick whose program is a bare name (node, python3) is refused when
    /// that program is missing from the PATH it will be looked up in: the one
    /// its declaration sets, otherwise the service's, which enable asks the
    /// service for. When the service cannot tell, the brick is enabled
    /// unchecked.
    Enable {
        /// A native brick's name, or the path of a declaration file
        brick: String,
        /// Enable the brick without looking for its program
        #[arg(long)]
        force: bool,
    },
    /// Disable a brick, by its name
    Disable {
        /// The brick's name
        brick: String,
    },
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
    fn refresh_takes_a_flow_or_a_brick() {
        let cli = Cli::try_parse_from(["nowcast", "refresh", "a/b"]).unwrap();
        let Command::Refresh { flow, brick, kind } = cli.command else {
            panic!("refresh");
        };
        assert_eq!((flow.as_deref(), brick, kind), (Some("a/b"), None, None));
        let cli = Cli::try_parse_from(["nowcast", "refresh", "--brick", "forge", "--kind", "pr"])
            .unwrap();
        let Command::Refresh { flow, brick, kind } = cli.command else {
            panic!("refresh");
        };
        assert_eq!(
            (flow, brick.as_deref(), kind.as_deref()),
            (None, Some("forge"), Some("pr"))
        );
        for args in [
            &["nowcast", "refresh"][..],
            &["nowcast", "refresh", "a/b", "--brick", "forge"],
            &["nowcast", "refresh", "a/b", "--kind", "pr"],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
        }
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
    fn setup_takes_bricks_or_remove() {
        let cli = Cli::try_parse_from(["nowcast", "setup", "claude-code"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Setup { ref bricks, remove: false } if bricks == &["claude-code"]
        ));
        assert!(Cli::try_parse_from(["nowcast", "setup", "--remove"]).is_ok());
        assert!(Cli::try_parse_from(["nowcast", "setup", "--remove", "claude-code"]).is_err());
    }

    #[test]
    fn global_options_go_anywhere() {
        let cli =
            Cli::try_parse_from(["nowcast", "get", "a/b", "--json", "--session", "s1"]).unwrap();
        assert!(cli.json);
        assert_eq!(cli.session.as_deref(), Some("s1"));
    }
}
