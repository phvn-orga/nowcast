//! When to read the usage API next: about every 60 s, never twice within
//! 15 s, backing off on `429`. Claude Code expects `429` answers there.

use std::time::Duration;

use tokio::time::Instant;

/// The interval between two readings while all goes well.
pub const INTERVAL: Duration = Duration::from_secs(60);
/// The shortest gap between two readings, refreshes included.
pub const MIN_GAP: Duration = Duration::from_secs(15);
/// The longest interval a run of `429` leads to.
pub const MAX_BACKOFF: Duration = Duration::from_secs(600);

#[derive(Debug)]
pub struct Pacer {
    interval: Duration,
    last: Option<Instant>,
    next: Instant,
}

impl Pacer {
    /// A pacer whose first reading is due now.
    pub fn new(now: Instant) -> Self {
        Self {
            interval: INTERVAL,
            last: None,
            next: now,
        }
    }

    /// When the next reading is due.
    pub fn next(&self) -> Instant {
        self.next
    }

    /// A reading starts.
    pub fn started(&mut self, now: Instant) {
        self.last = Some(now);
        self.next = now + self.interval;
    }

    /// The reading succeeded: back to the normal interval.
    pub fn succeeded(&mut self, now: Instant) {
        self.interval = INTERVAL;
        self.next = now + INTERVAL;
    }

    /// The API answered `429`: wait `retry_after` when given, otherwise
    /// double the interval, up to [`MAX_BACKOFF`].
    pub fn limited(&mut self, now: Instant, retry_after: Option<Duration>) {
        self.interval = (self.interval * 2).min(MAX_BACKOFF);
        self.next = now + retry_after.unwrap_or(self.interval).max(MIN_GAP);
    }

    /// Someone asked for a fresh reading: as soon as [`MIN_GAP`] allows, and
    /// never later than already planned. Close requests fold into one. While
    /// backing off after a `429`, the plan stands.
    pub fn hurry(&mut self, now: Instant) {
        if self.interval > INTERVAL {
            return;
        }
        let earliest = self.last.map_or(now, |last| (last + MIN_GAP).max(now));
        self.next = self.next.min(earliest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_are_spaced_and_back_off_on_429() {
        let t0 = Instant::now();
        let mut pacer = Pacer::new(t0);
        assert_eq!(pacer.next(), t0);
        pacer.started(t0);
        pacer.succeeded(t0);
        assert_eq!(pacer.next(), t0 + INTERVAL);
        pacer.started(t0 + INTERVAL);
        pacer.limited(t0 + INTERVAL, None);
        assert_eq!(pacer.next(), t0 + INTERVAL + INTERVAL * 2);
        pacer.limited(t0 + INTERVAL, None);
        pacer.limited(t0 + INTERVAL, None);
        pacer.limited(t0 + INTERVAL, None);
        assert_eq!(pacer.next(), t0 + INTERVAL + MAX_BACKOFF);
        pacer.limited(t0 + INTERVAL, Some(Duration::from_secs(30)));
        assert_eq!(pacer.next(), t0 + INTERVAL + Duration::from_secs(30));
        pacer.succeeded(t0 + INTERVAL);
        assert_eq!(pacer.next(), t0 + INTERVAL * 2);
    }

    #[test]
    fn a_hurry_waits_out_the_minimal_gap() {
        let t0 = Instant::now();
        let mut pacer = Pacer::new(t0);
        pacer.started(t0);
        pacer.succeeded(t0);
        pacer.hurry(t0 + Duration::from_secs(1));
        assert_eq!(pacer.next(), t0 + MIN_GAP);
        pacer.hurry(t0 + Duration::from_secs(20));
        assert_eq!(pacer.next(), t0 + MIN_GAP);
        let mut fresh = Pacer::new(t0);
        fresh.hurry(t0);
        assert_eq!(fresh.next(), t0);
    }

    #[test]
    fn a_hurry_does_not_cut_a_backoff() {
        let t0 = Instant::now();
        let mut pacer = Pacer::new(t0);
        pacer.started(t0);
        pacer.limited(t0, Some(Duration::from_secs(300)));
        pacer.hurry(t0 + Duration::from_secs(20));
        assert_eq!(pacer.next(), t0 + Duration::from_secs(300));
    }
}
