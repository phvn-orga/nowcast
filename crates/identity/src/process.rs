//! Reading processes from the kernel: the peer of a socket as a pidfd, then
//! each process's parent, start time and executable from `/proc`.
//!
//! A pid alone can be reused once its process is reaped, so every process is
//! held by a pidfd, and what `/proc/<pid>` says is trusted only once the pidfd
//! shows that pid was not released meanwhile.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::process::{Pid, PidfdFlags, pidfd_open};

/// A process held by a pidfd, with what `/proc` said about it.
#[derive(Debug)]
pub(crate) struct Process {
    pub(crate) pidfd: OwnedFd,
    pub(crate) pid: i32,
    /// Start time since boot, in clock ticks. With the pid, it names the
    /// process for the lifetime of the system.
    pub(crate) start: u64,
    ppid: i32,
    /// `None` when unreadable: a process of another user, a kernel thread or a
    /// process that has already exited.
    pub(crate) exe: Option<PathBuf>,
}

impl Process {
    /// The process connected on `socket`.
    pub(crate) fn peer_of(socket: BorrowedFd<'_>) -> io::Result<Self> {
        let pidfd = peer_pidfd(socket)?;
        let pid = pidfd_pid(pidfd.as_fd())?.ok_or_else(exited)?;
        Self::inspect(pidfd, pid)
    }

    /// Reads `/proc/<pid>` for the process `pidfd` holds, whose pid was `pid`.
    fn inspect(pidfd: OwnedFd, pid: i32) -> io::Result<Self> {
        let stat = fs::read(format!("/proc/{pid}/stat"))?;
        let (ppid, start) = parse_stat(&stat).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "unexpected /proc/<pid>/stat")
        })?;
        let exe = fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .map(strip_deleted);
        // Until the process is reaped its pid is not reused: if the pidfd still
        // shows it, what was just read is this process's.
        if pidfd_pid(pidfd.as_fd())? != Some(pid) {
            return Err(exited());
        }
        Ok(Self {
            pidfd,
            pid,
            start,
            ppid,
            exe,
        })
    }

    /// The parent, or `None` at the top of the visible tree, or when the
    /// parent can no longer be read: it exited, or this process exited or was
    /// reparented meanwhile.
    pub(crate) fn parent(&self) -> Option<Self> {
        let pidfd = pidfd_open(Pid::from_raw(self.ppid)?, PidfdFlags::empty()).ok()?;
        // The pidfd names the process that had the pid `ppid` when it was
        // opened. It is the parent if this process is still alive and still
        // has that parent: a parent that had exited would have left this
        // process reparented, and its pid could not be taken while it is used.
        let stat = fs::read(format!("/proc/{}/stat", self.pid)).ok()?;
        let (ppid, _) = parse_stat(&stat)?;
        if ppid != self.ppid || pidfd_pid(self.pidfd.as_fd()).ok()? != Some(self.pid) {
            return None;
        }
        Self::inspect(pidfd, self.ppid).ok()
    }
}

/// The connected process followed by its ancestors, read one step ahead.
pub(crate) struct Ancestors {
    next: Option<Process>,
}

impl Ancestors {
    pub(crate) fn of(process: Process) -> Self {
        Self {
            next: Some(process),
        }
    }
}

impl Iterator for Ancestors {
    type Item = Process;

    fn next(&mut self) -> Option<Process> {
        let process = self.next.take()?;
        self.next = process.parent();
        Some(process)
    }
}

/// Whether the process held by `pidfd` has exited. A pidfd becomes readable
/// when its process exits.
pub(crate) fn has_exited(pidfd: BorrowedFd<'_>) -> bool {
    let mut fds = [PollFd::from_borrowed_fd(pidfd, PollFlags::IN)];
    let now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    matches!(poll(&mut fds, Some(&now)), Ok(n) if n > 0)
}

fn exited() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "the process has exited")
}

/// A pidfd on the process connected on `socket`, the one that called
/// `connect`, taken by the kernel at connection time (`SO_PEERPIDFD`).
fn peer_pidfd(socket: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    let mut fd: libc::c_int = -1;
    let mut len = size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `fd` and `len` are valid for writes and `len` holds the size of
    // `fd`, which is what SO_PEERPIDFD writes.
    let ret = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERPIDFD,
            (&raw mut fd).cast(),
            &mut len,
        )
    };
    if ret != 0 {
        let error = io::Error::last_os_error();
        return Err(match error.raw_os_error() {
            Some(libc::ENOPROTOOPT) => io::Error::new(
                io::ErrorKind::Unsupported,
                "SO_PEERPIDFD is not supported: Linux 6.5 or later is required",
            ),
            _ => error,
        });
    }
    if fd < 0 {
        return Err(io::Error::other("SO_PEERPIDFD returned no descriptor"));
    }
    // SAFETY: on success the kernel installed a new pidfd in our table, which
    // nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The pid `pidfd` refers to, from `/proc/self/fdinfo`, or `None` once the
/// process is reaped or outside our pid namespace.
fn pidfd_pid(pidfd: BorrowedFd<'_>) -> io::Result<Option<i32>> {
    let info = fs::read_to_string(format!("/proc/self/fdinfo/{}", pidfd.as_raw_fd()))?;
    parse_fdinfo_pid(&info)
        .map(|pid| (pid > 0).then_some(pid))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a pidfd"))
}

/// The `Pid:` line of a pidfd's fdinfo: `-1` once reaped, `0` outside our pid
/// namespace.
fn parse_fdinfo_pid(info: &str) -> Option<i32> {
    info.lines()
        .find_map(|line| line.strip_prefix("Pid:"))
        .and_then(|pid| pid.trim().parse().ok())
}

/// The parent pid and start time from `/proc/<pid>/stat`. The command name in
/// parentheses may itself hold spaces and parentheses, so fields are counted
/// from the last `)`.
fn parse_stat(stat: &[u8]) -> Option<(i32, u64)> {
    let end = stat.iter().rposition(|&b| b == b')')?;
    let rest = std::str::from_utf8(&stat[end + 1..]).ok()?;
    let mut fields = rest.split_ascii_whitespace();
    // Fields 3 (state) onwards: ppid is field 4, starttime field 22.
    let ppid = fields.nth(1)?.parse().ok()?;
    let start = fields.nth(17)?.parse().ok()?;
    Some((ppid, start))
}

/// The kernel appends ` (deleted)` to the executable of a process whose file
/// was removed, which happens when an agent updates itself while running.
fn strip_deleted(exe: PathBuf) -> PathBuf {
    const SUFFIX: &[u8] = b" (deleted)";
    let bytes = exe.as_os_str().as_bytes();
    match bytes.strip_suffix(SUFFIX) {
        Some(path) => PathBuf::from(OsString::from_vec(path.to_vec())),
        None => exe,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;

    use super::*;

    #[test]
    fn stat_fields_are_counted_from_the_last_parenthesis() {
        let stat = b"1234 (a) b (c) S 77 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 98765 1000 100\n";
        assert_eq!(parse_stat(stat), Some((77, 98765)));
    }

    #[test]
    fn a_truncated_stat_is_rejected() {
        assert_eq!(parse_stat(b"1234 (sh) S 77 1"), None);
        assert_eq!(parse_stat(b"no parenthesis"), None);
    }

    #[test]
    fn the_fdinfo_pid_line_is_read() {
        let info = "pos:\t0\nflags:\t02000002\nmnt_id:\t15\nino:\t1057\nPid:\t4242\nNSpid:\t4242\n";
        assert_eq!(parse_fdinfo_pid(info), Some(4242));
        assert_eq!(parse_fdinfo_pid("Pid:\t-1\n"), Some(-1));
        assert_eq!(parse_fdinfo_pid("pos:\t0\n"), None);
    }

    #[test]
    fn the_deleted_suffix_is_stripped() {
        assert_eq!(
            strip_deleted("/opt/claude/versions/2.1.0 (deleted)".into()),
            PathBuf::from("/opt/claude/versions/2.1.0")
        );
        assert_eq!(
            strip_deleted("/usr/bin/sh".into()),
            PathBuf::from("/usr/bin/sh")
        );
    }

    #[test]
    fn the_peer_of_a_socket_pair_is_this_process() {
        let (ours, _theirs) = UnixStream::pair().unwrap();
        let process = Process::peer_of(ours.as_fd()).unwrap();
        assert_eq!(process.pid as u32, std::process::id());
        assert_eq!(process.exe, Some(std::env::current_exe().unwrap()));
        assert!(!has_exited(process.pidfd.as_fd()));
    }

    #[test]
    fn the_ancestors_reach_the_parent_of_this_process() {
        let (ours, _theirs) = UnixStream::pair().unwrap();
        let chain: Vec<_> = Ancestors::of(Process::peer_of(ours.as_fd()).unwrap())
            .take(2)
            .collect();
        assert_eq!(chain[1].pid as u32, std::os::unix::process::parent_id());
    }

    #[test]
    fn a_descriptor_that_is_not_a_pidfd_is_rejected() {
        let (ours, _theirs) = UnixStream::pair().unwrap();
        assert!(pidfd_pid(ours.as_fd()).is_err());
    }

    #[test]
    fn a_socket_that_is_not_unix_is_rejected() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        assert!(Process::peer_of(socket.as_fd()).is_err());
    }
}
