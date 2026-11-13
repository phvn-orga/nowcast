//! The slowdown rule of a change detector providers may use: a detector
//! whose answers keep changing polls less often, and speeds back up once
//! they settle. What "changed" means is the provider's: a `200` rather than
//! a `304` on a forge with `ETag`, a compared content that differed on one
//! without.

use std::time::Duration;

/// How often a detector polls a calm repository.
pub const PERIOD: Duration = Duration::from_secs(10);
/// How often, at the slowest, a detector polls a busy one.
pub const SLOWEST: Duration = Duration::from_secs(60);
/// The polls the slowdown rule looks back on.
const LOOKBACK: usize = 6;

/// How often a detector polls. It starts at [`PERIOD`]. Once it has
/// [`LOOKBACK`] answers at a period: when more than half changed, the
/// period doubles, up to [`SLOWEST`]; when one at most did, it halves, down
/// to [`PERIOD`]. A change of period starts the count again, so that each
/// period is judged on answers at that period; otherwise the oldest answer
/// leaves the count.
#[derive(Debug)]
pub struct Throttle {
    period: Duration,
    /// Whether each answer since the period was set changed.
    recent: Vec<bool>,
}

impl Default for Throttle {
    fn default() -> Self {
        Self {
            period: PERIOD,
            recent: Vec::new(),
        }
    }
}

impl Throttle {
    /// An answer, `changed` or not.
    pub fn answered(&mut self, changed: bool) {
        self.recent.push(changed);
        if self.recent.len() < LOOKBACK {
            return;
        }
        let changes = self.recent.iter().filter(|c| **c).count();
        let period = if changes > LOOKBACK / 2 {
            (self.period * 2).min(SLOWEST)
        } else if changes <= 1 {
            (self.period / 2).max(PERIOD)
        } else {
            self.period
        };
        if period == self.period {
            self.recent.remove(0);
        } else {
            self.period = period;
            self.recent.clear();
        }
    }

    pub const fn period(&self) -> Duration {
        self.period
    }

    /// Whether each answer counted changed, for tests.
    #[cfg(test)]
    pub fn recent(&self) -> &[bool] {
        &self.recent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_busy_detector_slows_down_and_speeds_back_up_when_it_settles() {
        let mut throttle = Throttle::default();
        let feed = |throttle: &mut Throttle, answers: &[bool]| {
            for changed in answers {
                throttle.answered(*changed);
            }
            throttle.period()
        };
        let busy = [true, true, false, true, true, false];
        assert_eq!(feed(&mut throttle, &busy[..5]), PERIOD, "not enough yet");
        assert_eq!(feed(&mut throttle, &busy[5..]), PERIOD * 2, "4 of 6");
        assert_eq!(feed(&mut throttle, &busy), PERIOD * 4);
        assert_eq!(feed(&mut throttle, &busy), SLOWEST);
        assert_eq!(feed(&mut throttle, &busy), SLOWEST, "no slower");
        let half = [true, false, true, false, true, false];
        assert_eq!(feed(&mut throttle, &half), SLOWEST, "half is not most");
        let calm = [false, false, true, false, false, false];
        assert_eq!(feed(&mut throttle, &calm), SLOWEST / 2);
        assert_eq!(feed(&mut throttle, &calm), SLOWEST / 4);
        assert_eq!(feed(&mut throttle, &calm), PERIOD);
        assert_eq!(feed(&mut throttle, &calm), PERIOD, "no faster");
    }
}
