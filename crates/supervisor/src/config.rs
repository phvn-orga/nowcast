use std::time::Duration;

use nowcast_types::READY_TIMEOUT;

/// The delays and limits of the supervisor. [`Config::default`] holds the
/// service's values; tests shorten them.
///
/// ```
/// use std::time::Duration;
/// use nowcast_supervisor::Config;
///
/// let config = Config::default();
/// assert_eq!(config.ready_timeout, Duration::from_secs(5));
/// assert_eq!(config.answer_timeout, Duration::from_secs(10));
/// assert_eq!(config.grace, Duration::from_secs(30));
/// assert_eq!(config.terminate_after, Duration::from_secs(5));
/// assert_eq!(config.kill_after, Duration::from_secs(5));
/// assert_eq!(config.first_backoff, Duration::from_secs(1));
/// assert_eq!(config.max_backoff, Duration::from_secs(30));
/// assert_eq!(config.stable_after, Duration::from_secs(60));
/// assert_eq!(config.max_restarts, 8);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// How long a launched brick has to answer `hello` with `ready`. The
    /// contract's [`READY_TIMEOUT`].
    pub ready_timeout: Duration,
    /// How long a ready brick has to answer `subscribe`.
    pub answer_timeout: Duration,
    /// How long a brick with no live flow keeps running before `shutdown`.
    /// The contract's 30 s.
    pub grace: Duration,
    /// From `shutdown` to `SIGTERM`, if the brick has not exited.
    pub terminate_after: Duration,
    /// From `SIGTERM` to `SIGKILL`, if the brick has not exited.
    pub kill_after: Duration,
    /// The wait before the first relaunch after a crash. Each further crash
    /// in a row doubles it, up to [`Self::max_backoff`].
    pub first_backoff: Duration,
    pub max_backoff: Duration,
    /// A brick that ran this long after `ready` before crashing starts its
    /// count of crashes in a row over.
    pub stable_after: Duration,
    /// How many relaunches in a row the supervisor tries. The crash after the
    /// last one ends every flow of the brick.
    pub max_restarts: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ready_timeout: READY_TIMEOUT,
            answer_timeout: Duration::from_secs(10),
            grace: Duration::from_secs(30),
            terminate_after: Duration::from_secs(5),
            kill_after: Duration::from_secs(5),
            first_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            stable_after: Duration::from_secs(60),
            max_restarts: 8,
        }
    }
}

impl Config {
    /// The wait before relaunching after the `crashes`-th crash in a row,
    /// counted from 1.
    pub(crate) fn backoff(&self, crashes: u32) -> Duration {
        let factor = 2u32.saturating_pow(crashes.saturating_sub(1));
        self.first_backoff
            .saturating_mul(factor)
            .min(self.max_backoff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backoff_doubles_up_to_its_ceiling() {
        let config = Config::default();
        let waits: Vec<u64> = (1..=8).map(|n| config.backoff(n).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(config.backoff(u32::MAX), config.max_backoff);
    }
}
