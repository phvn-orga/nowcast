//! What can go wrong, how it is reported and the exit status it maps to.

use std::fmt;
use std::io::{self, Write};
use std::path::PathBuf;

use serde_json::json;

/// The exit statuses, documented in `nowcast --help`.
pub mod exit {
    /// The service answered with an error, or the output could not be written.
    pub const FAILURE: u8 = 1;
    pub const USAGE: u8 = 2;
    pub const UNREACHABLE: u8 = 3;
    pub const PROTOCOL: u8 = 4;
}

#[derive(Debug)]
pub enum CliError {
    /// The service answered with an error of the client API.
    Api { code: String, message: String },
    /// An argument the tool checks itself is invalid. No call was made.
    Usage(String),
    /// The service cannot be reached, the connection broke, or no answer came
    /// in time.
    Unreachable { socket: PathBuf, reason: String },
    /// The service answered outside the contract.
    Protocol(String),
    /// Setting the service up or removing it failed: a unit file, or
    /// `systemctl`.
    Setup(String),
    /// Writing the output failed.
    Io(io::Error),
}

impl CliError {
    pub fn exit_code(&self) -> u8 {
        match self {
            // Nobody reads a closed pipe: stop quietly, like `head` expects.
            Self::Io(e) if e.kind() == io::ErrorKind::BrokenPipe => 0,
            Self::Api { .. } | Self::Setup(_) | Self::Io(_) => exit::FAILURE,
            Self::Usage(_) => exit::USAGE,
            Self::Unreachable { .. } => exit::UNREACHABLE,
            Self::Protocol(_) => exit::PROTOCOL,
        }
    }

    /// The service's code for an API error, the tool's own code otherwise.
    pub fn code(&self) -> &str {
        match self {
            Self::Api { code, .. } => code,
            Self::Usage(_) => "invalid_usage",
            Self::Unreachable { .. } => "service_unreachable",
            Self::Protocol(_) => "unexpected_response",
            Self::Setup(_) => "setup_failed",
            Self::Io(_) => "output_failed",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Api { message, .. } => message.clone(),
            Self::Usage(message) | Self::Protocol(message) | Self::Setup(message) => {
                message.clone()
            }
            Self::Unreachable { socket, reason } => {
                format!("cannot reach the service at {}: {reason}", socket.display())
            }
            Self::Io(e) => e.to_string(),
        }
    }

    /// Writes the error on stderr, as JSON with `--json`.
    pub fn report(&self, json: bool) {
        if self.exit_code() == 0 {
            return;
        }
        let mut err = io::stderr().lock();
        let _ = if json {
            let body = json!({ "error": { "code": self.code(), "message": self.message() } });
            writeln!(err, "{body}")
        } else {
            writeln!(err, "nowcast: {self}")
        };
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api { code, message, .. } => write!(f, "{code}: {message}"),
            Self::Protocol(message) => write!(f, "unexpected answer from the service: {message}"),
            _ => f.write_str(&self.message()),
        }
    }
}

impl From<io::Error> for CliError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_errors_show_the_code() {
        let error = CliError::Api {
            code: "unknown_flow".into(),
            message: "not subscribed".into(),
        };
        assert_eq!(error.to_string(), "unknown_flow: not subscribed");
        assert_eq!(error.exit_code(), exit::FAILURE);
        assert_eq!(error.code(), "unknown_flow");
    }

    #[test]
    fn a_broken_pipe_exits_quietly() {
        let error = CliError::Io(io::Error::from(io::ErrorKind::BrokenPipe));
        assert_eq!(error.exit_code(), 0);
        let error = CliError::Io(io::Error::from(io::ErrorKind::StorageFull));
        assert_eq!(error.exit_code(), exit::FAILURE);
    }

    #[test]
    fn each_kind_has_its_status() {
        assert_eq!(CliError::Usage(String::new()).exit_code(), exit::USAGE);
        assert_eq!(CliError::Setup(String::new()).exit_code(), exit::FAILURE);
        assert_eq!(
            CliError::Protocol(String::new()).exit_code(),
            exit::PROTOCOL
        );
        let unreachable = CliError::Unreachable {
            socket: "/x".into(),
            reason: "gone".into(),
        };
        assert_eq!(unreachable.exit_code(), exit::UNREACHABLE);
        assert_eq!(
            unreachable.to_string(),
            "cannot reach the service at /x: gone"
        );
    }
}
