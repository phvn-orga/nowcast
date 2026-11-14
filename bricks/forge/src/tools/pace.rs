//! A pace machine providers may use: when to read a branch next, from what
//! its last reading saw. The first state that applies wins:
//!
//! 1. CI expected: a new head (a push, a PR just opened) with no pipeline
//!    yet, for at most [`EXPECTED_FOR`]; a push on a branch without PR
//!    expects one only while its repository's such pushes start one
//!    ([`Pushes`]);
//! 2. CI running: a pipeline or a job queued or running; one waiting for a
//!    person (an approval) does not count, it may wait for days;
//! 3. fast window: a `refresh` asked for, for [`WINDOW_FOR`];
//! 4. normal.
//!
//! A `refresh` only asks for a reading now: what that reading sees sets the
//! pace after it. Each provider sets the durations ([`Rhythm`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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

/// Whether pushes on a branch without PR start a CI, by repository, as the
/// readings of a provider's targets saw it. A push that showed no pipeline
/// for the whole of [`EXPECTED_FOR`] teaches that they start none, a
/// pipeline after such a push that they start one. In memory only: after a
/// restart, the first push expects a CI again.
#[derive(Debug, Clone, Default)]
pub struct Pushes(Arc<Mutex<HashMap<String, Pushed>>>);

impl Pushes {
    /// What the pushes of repository `id` taught, shared by its targets.
    pub fn of(&self, id: &str) -> Pushed {
        let mut by_repo = self.0.lock().unwrap();
        by_repo.entry(id.to_owned()).or_default().clone()
    }
}

/// What the pushes on a branch without PR of one repository taught: whether
/// they start no CI. Until a push shows it, they start one.
#[derive(Debug, Clone, Default)]
pub struct Pushed(Arc<AtomicBool>);

impl Pushed {
    fn start_none(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    fn learn(&self, start_none: bool) {
        self.0.store(start_none, Ordering::Relaxed);
    }
}

/// A push on a branch without PR, watched until its subject changes for
/// whether it starts a CI.
#[derive(Debug)]
struct Push {
    /// No CI is expected for it: the repository's pushes started none when
    /// it came, or it taught that they start none.
    quiet: bool,
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
    /// What the repository's pushes on a branch without PR taught.
    pushed: Pushed,
    /// The push the subject comes from, while its CI has not shown up.
    push: Option<Push>,
}

impl Pace {
    /// A pace that learns from, and follows, what `pushed` holds.
    pub fn new(pushed: Pushed) -> Self {
        Self {
            pushed,
            ..Self::default()
        }
    }

    /// A `refresh`: the reading it triggers starts a fast window.
    pub fn refreshed(&mut self, now: Instant) {
        self.window_until = Some(now + WINDOW_FOR);
    }

    /// A reading saw `head` and `pr`, with `pipelines` on that head, queued
    /// ones included. A change of either, after the first reading, makes a
    /// CI expected, but for a push on a branch without PR in a repository
    /// whose such pushes start none.
    pub fn saw(&mut self, now: Instant, head: Option<&str>, pr: Option<u64>, pipelines: usize) {
        let subject = (head.map(str::to_owned), pr);
        if let Some(before) = &self.subject
            && *before != subject
        {
            self.push = None;
            if subject.0.is_some() {
                self.changed_at = Some(now);
                if pr.is_none() && before.0 != subject.0 {
                    self.push = Some(Push {
                        quiet: self.pushed.start_none(),
                    });
                }
            }
        }
        self.subject = Some(subject);
        if let Some(push) = &mut self.push {
            if pipelines > 0 {
                self.pushed.learn(false);
                self.push = None;
            } else if !push.quiet && self.changed_at.is_some_and(|at| now >= at + EXPECTED_FOR) {
                self.pushed.learn(true);
                push.quiet = true;
            }
        }
    }

    /// Until when a CI is due while it has not shown up: [`EXPECTED_FOR`]
    /// after the last change, when there is no pipeline yet and the change
    /// is not a push known to start none. The deadline may have passed
    /// already.
    pub fn expected_until(&self, pipelines: usize) -> Option<Instant> {
        let quiet = self.push.as_ref().is_some_and(|p| p.quiet);
        if pipelines == 0 && !quiet {
            self.changed_at.map(|at| at + EXPECTED_FOR)
        } else {
            None
        }
    }

    /// Whether a CI is due and has not shown up: a recent change that is
    /// not a quiet push, and no pipeline yet.
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
        pace.saw(now, Some("a"), None, 0);
        assert_eq!(pace.next(&GITHUB, now, 0, false), GITHUB.normal);
    }

    #[test]
    fn a_new_head_without_pipeline_expects_a_ci_for_two_minutes() {
        let (mut pace, t0) = (Pace::default(), Instant::now());
        pace.saw(t0, Some("a"), None, 0);
        pace.saw(t0, Some("b"), None, 0);
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
        pace.saw(t0, Some("a"), None, 0);
        pace.saw(t0, Some("a"), Some(7), 0);
        assert!(pace.expects(t0, 0));
        assert_eq!(pace.expected_until(0), Some(t0 + EXPECTED_FOR));
        assert_eq!(pace.expected_until(1), None, "the CI showed up");
    }

    #[test]
    fn a_branch_deleted_expects_nothing() {
        let (mut pace, t0) = (Pace::default(), Instant::now());
        pace.saw(t0, Some("a"), None, 0);
        pace.saw(t0, None, None, 0);
        assert!(!pace.expects(t0, 0));
    }

    #[test]
    fn a_refresh_opens_a_short_fast_window() {
        let (mut pace, t0) = (Pace::default(), Instant::now());
        pace.saw(t0, Some("a"), None, 0);
        pace.refreshed(t0);
        pace.saw(t0, Some("a"), None, 0);
        assert_eq!(pace.next(&GITHUB, t0, 0, false), S(2));
        assert_eq!(pace.next(&GITHUB, t0 + S(31), 0, false), GITHUB.normal);
    }

    #[test]
    fn a_push_that_starts_no_ci_stops_the_next_ones_expecting_one() {
        let pushes = Pushes::default();
        let (mut pace, t0) = (Pace::new(pushes.of("r")), Instant::now());
        pace.saw(t0, Some("a"), None, 0);
        pace.saw(t0, Some("b"), None, 0);
        pace.saw(t0 + S(119), Some("b"), None, 0);
        assert!(pace.expects(t0 + S(119), 0), "the window is not over");
        pace.saw(t0 + S(120), Some("b"), None, 0);
        assert!(!pace.expects(t0 + S(120), 0));
        let t1 = t0 + S(200);
        pace.saw(t1, Some("c"), None, 0);
        assert_eq!(pace.expected_until(0), None, "known to start none");
        assert_eq!(pace.next(&GITHUB, t1, 0, false), GITHUB.normal);
        let mut other = Pace::new(pushes.of("r"));
        other.saw(t1, Some("x"), None, 0);
        other.saw(t1, Some("y"), None, 0);
        assert_eq!(
            other.expected_until(0),
            None,
            "every branch of the repository"
        );
    }

    #[test]
    fn a_pipeline_after_a_push_makes_the_next_ones_expect_a_ci_again() {
        let pushes = Pushes::default();
        let (mut pace, t0) = (Pace::new(pushes.of("r")), Instant::now());
        pace.saw(t0, Some("a"), None, 0);
        pace.saw(t0, Some("b"), None, 0);
        pace.saw(t0 + S(121), Some("b"), None, 0);
        pace.saw(t0 + S(200), Some("c"), None, 0);
        pace.saw(t0 + S(210), Some("c"), None, 1);
        assert_eq!(
            pace.expected_until(0),
            Some(t0 + S(200) + EXPECTED_FOR),
            "the push did start one"
        );
        let t1 = t0 + S(300);
        pace.saw(t1, Some("d"), None, 0);
        assert!(pace.expects(t1, 0));
    }

    #[test]
    fn a_pr_head_expects_a_ci_whatever_the_pushes_taught() {
        let pushes = Pushes::default();
        let (mut pace, t0) = (Pace::new(pushes.of("r")), Instant::now());
        pace.saw(t0, Some("a"), None, 0);
        pace.saw(t0, Some("b"), None, 0);
        pace.saw(t0 + S(121), Some("b"), None, 0);
        let t1 = t0 + S(200);
        pace.saw(t1, Some("b"), Some(7), 0);
        assert!(pace.expects(t1, 0), "a PR opened");
        pace.saw(t1 + S(10), Some("c"), Some(7), 0);
        assert!(pace.expects(t1 + S(10), 0), "a push on the PR");
    }

    #[test]
    fn a_pr_head_without_ci_teaches_nothing() {
        let pushes = Pushes::default();
        let (mut pace, t0) = (Pace::new(pushes.of("r")), Instant::now());
        pace.saw(t0, Some("a"), Some(7), 0);
        pace.saw(t0, Some("b"), Some(7), 0);
        pace.saw(t0 + S(121), Some("b"), Some(7), 0);
        let mut branch = Pace::new(pushes.of("r"));
        branch.saw(t0, Some("x"), None, 0);
        branch.saw(t0, Some("y"), None, 0);
        assert!(branch.expects(t0, 0));
    }

    #[test]
    fn the_pushes_are_learnt_by_repository() {
        let pushes = Pushes::default();
        let (mut pace, t0) = (Pace::new(pushes.of("r")), Instant::now());
        pace.saw(t0, Some("a"), None, 0);
        pace.saw(t0, Some("b"), None, 0);
        pace.saw(t0 + S(121), Some("b"), None, 0);
        let mut other = Pace::new(pushes.of("s"));
        other.saw(t0, Some("a"), None, 0);
        other.saw(t0, Some("b"), None, 0);
        assert!(other.expects(t0, 0));
    }
}
