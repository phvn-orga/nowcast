//! Where the service listens and which session the tool declares.

use std::ffi::OsString;
use std::path::PathBuf;

use nowcast_types::SessionId;

use crate::error::CliError;

/// The socket's directory under `$XDG_RUNTIME_DIR`, and its file name.
const SOCKET_DIR: &str = "nowcast";
const SOCKET_NAME: &str = "nowcast.sock";

/// The socket given on the command line or in `NOWCAST_SOCKET`, otherwise
/// `$XDG_RUNTIME_DIR/nowcast/nowcast.sock`.
pub fn socket_path(
    explicit: Option<PathBuf>,
    runtime_dir: Option<OsString>,
) -> Result<PathBuf, CliError> {
    if let Some(path) = explicit {
        if path.as_os_str().is_empty() {
            return Err(CliError::Usage("the socket path is empty".into()));
        }
        return Ok(path);
    }
    // The XDG specification says to ignore a relative runtime directory.
    match runtime_dir.map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => Ok(dir.join(SOCKET_DIR).join(SOCKET_NAME)),
        _ => Err(CliError::Usage(
            "XDG_RUNTIME_DIR is not set to an absolute path: pass --socket or set NOWCAST_SOCKET"
                .into(),
        )),
    }
}

/// The session given on the command line or in `NOWCAST_SESSION`, otherwise
/// `cli-<pid>`: a tool anchored on itself is its own session.
pub fn session_id(explicit: Option<String>, pid: u32) -> Result<SessionId, CliError> {
    match explicit {
        Some(id) if id.is_empty() => Err(CliError::Usage("the session id is empty".into())),
        Some(id) => Ok(SessionId::new(id)),
        None => Ok(SessionId::new(format!("cli-{pid}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_socket_wins() {
        let path = socket_path(Some("/tmp/s.sock".into()), Some("/run/user/1".into())).unwrap();
        assert_eq!(path, PathBuf::from("/tmp/s.sock"));
    }

    #[test]
    fn the_default_socket_is_under_the_runtime_dir() {
        let path = socket_path(None, Some("/run/user/1000".into())).unwrap();
        assert_eq!(path, PathBuf::from("/run/user/1000/nowcast/nowcast.sock"));
    }

    #[test]
    fn no_usable_runtime_dir_is_a_usage_error() {
        for runtime_dir in [None, Some("".into()), Some("relative/dir".into())] {
            let error = socket_path(None, runtime_dir).unwrap_err();
            assert!(matches!(error, CliError::Usage(_)), "{error:?}");
        }
    }

    #[test]
    fn an_empty_socket_is_a_usage_error() {
        let error = socket_path(Some("".into()), Some("/run/user/1".into())).unwrap_err();
        assert!(matches!(error, CliError::Usage(_)));
    }

    #[test]
    fn the_default_session_names_the_process() {
        assert_eq!(session_id(None, 4242).unwrap().as_str(), "cli-4242");
    }

    #[test]
    fn an_explicit_session_wins() {
        assert_eq!(session_id(Some("abc".into()), 1).unwrap().as_str(), "abc");
    }

    #[test]
    fn an_empty_session_is_a_usage_error() {
        assert!(matches!(
            session_id(Some(String::new()), 1),
            Err(CliError::Usage(_))
        ));
    }
}
