//! A pace machine providers may use: when to read a branch next, from what
//! its last reading saw. The first state that applies wins:
//!
//! 1. CI expected: a new head (a push, a PR just opened) with no pipeline
//!    yet, for at most [`EXPECTED_FOR`], unless its provider knows that
//!    it starts none ([`Starts`]);
//! 2. CI running: a pipeline or a job queued or running; one waiting for a
//!    person (an approval) does not count, it may wait for days;
//! 3. fast window: a `refresh` asked for, for [`WINDOW_FOR`];
//! 4. normal.
//!
//! A `refresh` only asks for a reading now: what that reading sees sets the
//! pace after it. Each provider sets the durations ([`Rhythm`]).

use std::time::Duration;

use tokio::time::Instant;

/// How long a new head without pipeline counts as expecting a CI. Past it,
/// the event triggered none.
pub const EXPECTED_FOR: Duration = Duration::from_secs(120);
/// How long the first, fastest part of the expectation lasts.
pub const EXPECTED_FIRST: Duration = Duration::from_secs(30);
/// How long a `refresh` keeps the pace fast.
pub const WINDOW_FOR: Duration = Duration::from_secs(30);

/// The intervals of one provider.
#[derive(Debug, Clone, Copy)]
pub struct Rhythm {
    pub expected_first: Duration,
    pub expected_after: Duration,
    pub running: Duration,
    pub window: Duration,
    pub normal: Duration,
}

/// Whether the branch's head, as its provider knows it, starts a CI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Starts {
    /// No CI starts for it: nothing is expected.
    None,
    /// A CI starts for it.
    Certain,
    /// It may start one: the provider cannot tell.
    #[default]
    Maybe,
}

/// What the pace remembers of a branch between readings.
#[derive(Debug, Default)]
pub struct Pace {
    /// What the CI of the branch is about: the head read last and the PR it
    /// belongs to. `None` before the first reading.
    subject: Option<(Option<String>, Option<u64>)>,
    /// When the subject last changed, for a CI to be expected.
    changed_at: Option<Instant>,
    window_until: Option<Instant>,
    /// Whether the subject starts a CI, as its provider said last.
    starts: Starts,
}

impl Pace {
    /// A `refresh`: the reading it triggers starts a fast window.
    pub fn refreshed(&mut self, now: Instant) {
        self.window_until = Some(now + WINDOW_FOR);
    }

    /// A reading saw `head` and `pr`. A change of either, after the first
    /// reading, makes a CI expected.
    pub fn saw(&mut self, now: Instant, head: Option<&str>, pr: Option<u64>) {
        let subject = (head.map(str::to_owned), pr);
        if let Some(before) = &self.subject
            && *before != subject
            && subject.0.is_some()
        {
            self.changed_at = Some(now);
        }
        self.subject = Some(subject);
    }

    /// Whether the subject read last starts a CI; until said, it may.
    pub fn starts(&mut self, starts: Starts) {
        self.starts = starts;
    }

    /// Until when a CI is due while it has not shown up: [`EXPECTED_FOR`]
    /// after the last change, when there is no pipeline yet and the subject
    /// may start one. The deadline may have passed already.
    pub fn expected_until(&self, pipelines: usize) -> Option<Instant> {
        if pipelines == 0 && self.starts != Starts::None {
            self.changed_at.map(|at| at + EXPECTED_FOR)
        } else {
            None
        }
    }

    /// Whether a CI is due and has not shown up: a recent change that may
    /// start one, and no pipeline yet.
    pub fn expects(&self, now: Instant, pipelines: usize) -> bool {
        self.expected_until(pipelines)
            .is_some_and(|until| now < until)
    }

    /// How long to wait before the next reading.
    pub fn next(&self, rhythm: &Rhythm, now: Instant, pipelines: usize, running: bool) -> Duration {
        if self.expects(now, pipelines) {
            let since = now - self.changed_at.unwrap_or(now);
            if since < EXPECTED_FIRST {
                rhythm.expected_first
            } else {
                rhythm.expected_after
            }
        } else if running {
            rhythm.running
        } else if self.window_until.is_some_and(|until| now < until) {
            rhythm.window
        } else {
            rhythm.normal
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GITHUB: Rhythm = Rhythm {
        expected_first: Duration::from_secs(2),
        expected_after: Duration::from_secs(5),
        running: Duration::from_secs(5),
        window: Duration::from_secs(2),
        normal: Duration::from_secs(60),
    };

    const S: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn the_first_reading_expects_nothing() {
        let (mut pace, now) = (Pace::default(), Instant::now());
        pace.saw(now, Some("a"), None);
        assert_eq!(pace.next(&GITHUB, now, 0, false), GITHUB.normal);
    }

    #[test]
    fn a_new_head_without_pipeline_expects_a_ci_for_two_minutes() {
        let (mut pace, t0) = (Pace::default(), Instant::now());
        pace.saw(t0, Some("a"), None);
        pace.saw(t0, Some("b"), None);
        assert_eq!(pace.next(&GITHUB, t0, 0, false), S(2));
        assert_eq!(pace.next(&GITHUB, t0 + S(31), 0, false), S(5));
        assert_eq!(
            pace.next(&GITHUB, t0 + S(121), 0, false),
            GITHUB.normal,
            "no CI for this push"
        );
        assert_eq!(
            pace.next(&GITHUB, t0 + S(10), 1, true),
            S(5),
            "the CI showed up and runs"
        );
        assert_eq!(
            pace.next(&GITHUB, t0 + S(10), 1, false),
            GITHUB.normal,
            "and is over"
        );
    }

    #[test]
    fn a_pr_opened_on_the_same_head_expects_a_ci() {
        let (mut pace, t0) = (Pace::default(), Instant::now());
        pace.saw(t0, Some("a"), None);
        pace.saw(t0, Some("a"), Some(7));
        assert!(pace.expects(t0, 0));
        assert_eq!(pace.expected_until(0), Some(t0 + EXPECTED_FOR));
        assert_eq!(pace.expected_until(1), None, "the CI showed up");
    }

    #[test]
    fn a_branch_deleted_expects_nothing() {
        let (mut pace, t0) = (Pace::default(), Instant::now());
        pace.saw(t0, Some("a"), None);
        pace.saw(t0, None, None);
        assert!(!pace.expects(t0, 0));
    }

    #[test]
    fn a_head_that_starts_no_ci_expects_nothing() {
        let (mut pace, t0) = (Pace::default(), Instant::now());
        pace.saw(t0, Some("a"), None);
        pace.saw(t0, Some("b"), None);
        pace.starts(Starts::None);
        assert_eq!(pace.expected_until(0), None);
        assert_eq!(pace.next(&GITHUB, t0, 0, false), GITHUB.normal);
        pace.saw(t0, Some("c"), None);
        for starts in [Starts::Certain, Starts::Maybe] {
            pace.starts(starts);
            assert_eq!(
                pace.expected_until(0),
                Some(t0 + EXPECTED_FOR),
                "{starts:?}"
            );
        }
    }

    #[test]
    fn a_refresh_opens_a_short_fast_window() {
        let (mut pace, t0) = (Pace::default(), Instant::now());
        pace.saw(t0, Some("a"), None);
        pace.refreshed(t0);
        pace.saw(t0, Some("a"), None);
        assert_eq!(pace.next(&GITHUB, t0, 0, false), S(2));
        assert_eq!(pace.next(&GITHUB, t0 + S(31), 0, false), GITHUB.normal);
    }
}
