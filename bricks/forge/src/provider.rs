//! What a forge implements to be read by the brick. The core knows no forge:
//! it asks each provider whether it recognises a remote, and has the one
//! that does follow a branch, a tracker's open issues, or one issue. How and when the provider reads (protocol,
//! pace, quotas, cache) is its own business; what it hands back is the
//! neutral model of [`crate::model`].

use std::future::Future;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::AbortHandle;

use crate::model::{Ci, Issue, Issues, Pr};

/// A repository a provider recognised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The repository's identity, the same for every spelling of its remote:
    /// `github.com/<owner>/<name>`.
    pub id: String,
    /// The identity of what holds the repository's issues, which several
    /// repositories may share: `github.com/<owner>/<name>` on GitHub, the
    /// repository itself.
    pub tracker: String,
    /// The remote as the first subscriber wrote it.
    pub remote: String,
}

/// What one reading of a branch found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchReading {
    /// The branch's head on the forge; `None` when the branch is not there.
    pub head_sha: Option<String>,
    /// The branch's PR, open first, else the latest.
    pub pr: Option<Pr>,
    /// The CI of the open PR's head, or of the branch's head; `None` without
    /// a commit to read.
    pub ci: Option<Ci>,
}

/// Why a reading gave nothing: `source.status` in the details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    NoCredentials,
    Unauthorized,
    NotFound,
    RateLimited,
    Unreachable,
    Unreadable,
}

impl Failure {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoCredentials => "no_credentials",
            Self::Unauthorized => "unauthorized",
            Self::NotFound => "not_found",
            Self::RateLimited => "rate_limited",
            Self::Unreachable => "unreachable",
            Self::Unreadable => "unreadable",
        }
    }
}

/// What one reading of a target found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    Branch(Box<BranchReading>),
    /// A tracker's open issues.
    Issues(Issues),
    /// One issue.
    Issue(Issue),
}

pub type Outcome = Result<Reading, Failure>;

/// Where a provider sends the readings of one target it follows.
pub struct Updates {
    target: u64,
    tx: UnboundedSender<(u64, Outcome)>,
}

impl Updates {
    pub fn new(target: u64, tx: UnboundedSender<(u64, Outcome)>) -> Self {
        Self { target, tx }
    }

    /// `false` once the core is gone: the provider stops.
    pub fn send(&self, outcome: Outcome) -> bool {
        self.tx.send((self.target, outcome)).is_ok()
    }
}

/// A target a provider follows. Dropping it stops the following.
pub struct Watch {
    refresh: UnboundedSender<()>,
    task: AbortHandle,
}

impl Watch {
    /// Spawns `follow`, which receives each `refresh` on its channel.
    pub fn spawn<F>(follow: impl FnOnce(UnboundedReceiver<()>) -> F) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let (refresh, refreshes) = unbounded_channel();
        let task = tokio::spawn(follow(refreshes)).abort_handle();
        Self { refresh, task }
    }

    /// Asks for a reading now.
    pub fn refresh(&self) {
        let _ = self.refresh.send(());
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A forge, compiled into the brick.
///
/// A provider keeps to the output contract of the design
/// (`06-brique-forge.md`): a `refresh` reads at once, or as soon as the
/// forge's limits allow; a running CI is followed closely; a reading that
/// fails sends its [`Failure`], and the core keeps the last value; nothing
/// is written, neither to the forge nor to its tools' configuration.
pub trait Provider: Send + Sync {
    /// The repository `remote` designates, when this forge holds it.
    fn recognise(&self, remote: &str) -> Option<Repo>;

    /// Starts following `branch` of `repo`, a repository this provider
    /// recognised: every reading, a [`Reading::Branch`], goes to `updates`,
    /// until the returned [`Watch`] is dropped.
    fn follow(&self, repo: &Repo, branch: &str, updates: Updates) -> Watch;

    /// Starts following the open issues of the tracker of `repo`: every
    /// reading, a [`Reading::Issues`], goes to `updates`, until the returned
    /// [`Watch`] is dropped.
    fn follow_issues(&self, repo: &Repo, updates: Updates) -> Watch;

    /// Starts following issue `number` of the tracker of `repo`: every
    /// reading, a [`Reading::Issue`], goes to `updates`, until the returned
    /// [`Watch`] is dropped. A number that is no issue is
    /// [`Failure::NotFound`].
    fn follow_issue(&self, repo: &Repo, number: u64, updates: Updates) -> Watch;
}
