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

/// The session the tool declares, and whether it replaces the anchor's.
pub struct Declaration {
    pub id: SessionId,
    pub replace: bool,
}

/// The session given on the command line or in `NOWCAST_SESSION`, which
/// replaces the anchor's, otherwise `cli-<pid>`: a tool anchored on itself is
/// its own session, while one run inside an agent keeps the agent's.
pub fn session(explicit: Option<String>, pid: u32) -> Result<Declaration, CliError> {
    match explicit {
        Some(id) if id.is_empty() => Err(CliError::Usage("the session id is empty".into())),
        Some(id) => Ok(Declaration {
            id: SessionId::new(id),
            replace: true,
        }),
        None => Ok(Declaration {
            id: SessionId::new(format!("cli-{pid}")),
            replace: false,
        }),
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
    fn the_default_session_names_the_process_without_replacing() {
        let declaration = session(None, 4242).unwrap();
        assert_eq!(declaration.id.as_str(), "cli-4242");
        assert!(!declaration.replace);
    }

    #[test]
    fn an_explicit_session_wins_and_replaces() {
        let declaration = session(Some("abc".into()), 1).unwrap();
        assert_eq!(declaration.id.as_str(), "abc");
        assert!(declaration.replace);
    }

    #[test]
    fn an_empty_session_is_a_usage_error() {
        assert!(matches!(
            session(Some(String::new()), 1),
            Err(CliError::Usage(_))
        ));
    }
}
