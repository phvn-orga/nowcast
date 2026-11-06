//! The listening socket: the one systemd passes on socket activation, or one
//! the daemon binds itself under `$XDG_RUNTIME_DIR/nowcast/`.

use std::ffi::OsString;
use std::fs::{self, DirBuilder, Permissions};
use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use rustix::io::{FdFlags, fcntl_setfd};

/// The socket's directory under `$XDG_RUNTIME_DIR`, and its file name. The
/// command line tool looks for the same path.
const SOCKET_DIR: &str = "nowcast";
const SOCKET_NAME: &str = "nowcast.sock";

/// The first descriptor systemd passes (`SD_LISTEN_FDS_START`).
const LISTEN_FDS_START: RawFd = 3;

/// The variables of the socket activation protocol, removed once read so
/// that the processes the daemon launches do not see them.
const ACTIVATION_VARS: [&str; 4] = [
    "LISTEN_PID",
    "LISTEN_FDS",
    "LISTEN_FDNAMES",
    "LISTEN_PIDFDID",
];

/// The socket the daemon listens on.
pub struct Socket {
    pub listener: UnixListener,
    /// The socket file to remove on exit: set only when the daemon bound it
    /// itself, since a socket passed by systemd belongs to systemd.
    pub bound: Option<PathBuf>,
}

/// Takes the socket systemd passed, or binds the default one.
///
/// Edits the environment: call it before any other thread starts.
pub fn open() -> io::Result<Socket> {
    if let Some(listener) = from_systemd()? {
        tracing::info!("listening on the socket passed by systemd");
        return Ok(Socket {
            listener,
            bound: None,
        });
    }
    let path = default_path(std::env::var_os("XDG_RUNTIME_DIR"))?;
    let listener = bind(&path)?;
    tracing::info!("listening on {}", path.display());
    Ok(Socket {
        listener,
        bound: Some(path),
    })
}

/// `$XDG_RUNTIME_DIR/nowcast/nowcast.sock`. The XDG specification says to
/// ignore a relative runtime directory.
fn default_path(runtime_dir: Option<OsString>) -> io::Result<PathBuf> {
    match runtime_dir.map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => Ok(dir.join(SOCKET_DIR).join(SOCKET_NAME)),
        _ => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "XDG_RUNTIME_DIR is not set to an absolute path",
        )),
    }
}

/// The socket systemd passed, `None` when the daemon was not activated.
fn from_systemd() -> io::Result<Option<UnixListener>> {
    let pid = std::env::var("LISTEN_PID").ok();
    let fds = std::env::var("LISTEN_FDS").ok();
    let count = activated_fds(pid.as_deref(), fds.as_deref(), std::process::id());
    for var in ACTIVATION_VARS {
        // SAFETY: `open` runs before any other thread exists, so nothing
        // reads the environment concurrently.
        unsafe { std::env::remove_var(var) };
    }
    match count? {
        0 => Ok(None),
        1 => {
            // SAFETY: systemd passed this descriptor to this process, and
            // nothing else in it owns the descriptor.
            let fd = unsafe { OwnedFd::from_raw_fd(LISTEN_FDS_START) };
            // Bricks launched later must not inherit the socket.
            fcntl_setfd(&fd, FdFlags::CLOEXEC)?;
            let listener = UnixListener::from(fd);
            // Fails unless the descriptor is a Unix socket.
            listener.local_addr()?;
            Ok(Some(listener))
        }
        n => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("systemd passed {n} sockets, the daemon expects one"),
        )),
    }
}

/// How many descriptors systemd passed to the process `pid`, from
/// `LISTEN_PID` and `LISTEN_FDS`. Zero when they are unset or meant for
/// another process, as `sd_listen_fds` does.
fn activated_fds(
    listen_pid: Option<&str>,
    listen_fds: Option<&str>,
    pid: u32,
) -> io::Result<usize> {
    let invalid = |var: &str, value: &str| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{var} is not a number: {value:?}"),
        )
    };
    let Some(listen_pid) = listen_pid else {
        return Ok(0);
    };
    let listen_pid: u32 = listen_pid
        .parse()
        .map_err(|_| invalid("LISTEN_PID", listen_pid))?;
    if listen_pid != pid {
        return Ok(0);
    }
    match listen_fds {
        None => Ok(0),
        Some(fds) => fds.parse().map_err(|_| invalid("LISTEN_FDS", fds)),
    }
}

/// Binds `path` in a directory only the user can enter, as a socket only the
/// user can open.
fn bind(path: &Path) -> io::Result<UnixListener> {
    let dir = path.parent().expect("the socket path has a directory");
    match DirBuilder::new().mode(0o700).create(dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    // A directory that already existed keeps its mode unless set again.
    fs::set_permissions(dir, Permissions::from_mode(0o700))?;
    remove_stale(path)?;
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Removes a socket file left by a daemon that did not exit cleanly. Refuses
/// to take over a socket that something still listens on.
fn remove_stale(path: &Path) -> io::Result<()> {
    match UnixStream::connect(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("another process listens on {}", path.display()),
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
            tracing::info!("removing the stale socket {}", path.display());
            fs::remove_file(path)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_socket_is_where_the_cli_looks() {
        let path = default_path(Some("/run/user/1000".into())).unwrap();
        assert_eq!(path, PathBuf::from("/run/user/1000/nowcast/nowcast.sock"));
    }

    #[test]
    fn no_usable_runtime_dir_is_an_error() {
        for runtime_dir in [None, Some("".into()), Some("relative/dir".into())] {
            assert!(default_path(runtime_dir).is_err());
        }
    }

    #[test]
    fn activation_counts_the_sockets_passed_to_this_process() {
        assert_eq!(activated_fds(Some("42"), Some("1"), 42).unwrap(), 1);
        assert_eq!(activated_fds(Some("42"), Some("2"), 42).unwrap(), 2);
    }

    #[test]
    fn no_activation_without_the_variables() {
        assert_eq!(activated_fds(None, None, 42).unwrap(), 0);
        assert_eq!(activated_fds(None, Some("1"), 42).unwrap(), 0);
        assert_eq!(activated_fds(Some("42"), None, 42).unwrap(), 0);
    }

    #[test]
    fn sockets_meant_for_another_process_are_ignored() {
        assert_eq!(activated_fds(Some("7"), Some("1"), 42).unwrap(), 0);
    }

    #[test]
    fn malformed_variables_are_errors() {
        assert!(activated_fds(Some("x"), Some("1"), 42).is_err());
        assert!(activated_fds(Some("42"), Some("one"), 42).is_err());
    }
}
