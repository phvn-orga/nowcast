//! What `status` and `live` share to show the service to a human: short ids,
//! relative times, and how a brick's state looks.

use std::time::Duration;

use jiff::Timestamp;

/// How many characters of a session id are shown, enough to tell sessions
/// apart: a UUID's first group.
const SHORT_SESSION: usize = 8;

/// A session id as shown: its start, when it is long.
pub fn short_session(id: &str) -> &str {
    match id.char_indices().nth(SHORT_SESSION) {
        // Cutting saves nothing on an id barely longer than the cut.
        Some((cut, _)) if id.len() > cut + 3 => &id[..cut],
        _ => id,
    }
}

/// A flow id as shown under `session`: the session id it repeats becomes `…`.
pub fn flow_in_session(flow: &str, session: &str) -> String {
    if session.is_empty() {
        return flow.to_owned();
    }
    flow.replace(session, "…")
}

/// How long ago `time`, an RFC 3339 timestamp, was at `now`; `None` when it
/// cannot be read. A time ahead of `now`, from a clock gone back, reads as
/// now.
pub fn since(time: &str, now: Timestamp) -> Option<Duration> {
    let time: Timestamp = time.parse().ok()?;
    let seconds = now.as_second().saturating_sub(time.as_second());
    Some(Duration::from_secs(seconds.try_into().unwrap_or(0)))
}

/// A duration in its two largest units: `42 s`, `3 min`, `2 h 05 min`,
/// `3 d 4 h`.
pub fn duration(elapsed: Duration) -> String {
    let s = elapsed.as_secs();
    match s {
        0..60 => format!("{s} s"),
        60..3_600 => format!("{} min", s / 60),
        3_600..86_400 => format!("{} h {:02} min", s / 3_600, s % 3_600 / 60),
        _ => format!("{} d {} h", s / 86_400, s % 86_400 / 3_600),
    }
}

/// `duration` followed by `ago`, or `unknown` when `time` cannot be read.
pub fn ago(time: &str, now: Timestamp) -> String {
    since(time, now).map_or_else(|| "unknown".to_owned(), |d| format!("{} ago", duration(d)))
}

/// How a brick's state looks: its symbol and its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// `running`.
    Up,
    /// `starting`, `stopping`: on its way.
    Moving,
    /// `stopped`: nothing wrong, nothing running.
    Idle,
    /// `waiting`, `given_up`, or a state this tool does not know.
    Failing,
}

impl Health {
    pub fn of(state: &str) -> Self {
        match state {
            "running" => Self::Up,
            "starting" | "stopping" => Self::Moving,
            "stopped" => Self::Idle,
            _ => Self::Failing,
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Up => "●",
            Self::Moving => "◐",
            Self::Idle => "○",
            Self::Failing => "▲",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_session_ids_are_cut() {
        assert_eq!(
            short_session("5f1c2e9a-0b1c-4d2e-8f3a-4b5c6d7e8f90"),
            "5f1c2e9a"
        );
        assert_eq!(short_session("cli-123456"), "cli-123456");
        assert_eq!(short_session("s"), "s");
        assert_eq!(short_session("éééééééééééééé"), "éééééééé");
    }

    #[test]
    fn a_flow_does_not_repeat_its_session() {
        assert_eq!(
            flow_in_session("claude-code/session/5f1c-9a", "5f1c-9a"),
            "claude-code/session/…"
        );
        assert_eq!(flow_in_session("forge/x", "5f1c"), "forge/x");
        assert_eq!(flow_in_session("forge/x", ""), "forge/x");
    }

    #[test]
    fn durations_keep_two_units() {
        let d = |s| duration(Duration::from_secs(s));
        assert_eq!(d(0), "0 s");
        assert_eq!(d(59), "59 s");
        assert_eq!(d(60), "1 min");
        assert_eq!(d(3_599), "59 min");
        assert_eq!(d(3_600 + 5 * 60), "1 h 05 min");
        assert_eq!(d(86_400 * 3 + 3_600 * 4 + 59), "3 d 4 h");
    }

    #[test]
    fn times_read_relative_to_now() {
        let now: Timestamp = "2026-11-08T16:45:12Z".parse().unwrap();
        assert_eq!(ago("2026-11-08T16:45:09Z", now), "3 s ago");
        assert_eq!(ago("2026-11-08T16:45:20Z", now), "0 s ago");
        assert_eq!(ago("yesterday", now), "unknown");
    }

    #[test]
    fn states_map_to_a_health() {
        assert_eq!(Health::of("running"), Health::Up);
        assert_eq!(Health::of("stopping"), Health::Moving);
        assert_eq!(Health::of("stopped"), Health::Idle);
        assert_eq!(Health::of("given_up"), Health::Failing);
        assert_eq!(Health::of("new-state"), Health::Failing);
    }
}
