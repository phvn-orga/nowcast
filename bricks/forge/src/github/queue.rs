//! The merge queue of a base, through GraphQL, the only API that shows it:
//! one query gives all its entries, shared by every branch followed. A base
//! is asked at the first reading whether it has a queue, then read again at
//! [`RHYTHM`]'s normal pace, and every [`FAST`] while the PR asking is in
//! it. A query that fails keeps what the base last gave: the branch's
//! reading never fails for it. Why a PR left the queue is read on demand
//! ([`removal`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiff::Timestamp;
use serde_json::{Value, json};
use tokio::time::Instant;

use super::RHYTHM;
use super::required::{Base, graphql};
use crate::model::Dequeued;
use crate::tools::http::Failure;

/// How often a base is read while a PR followed is in its queue.
pub const FAST: Duration = Duration::from_secs(10);

const QUEUE: &str = "query($owner: String!, $name: String!, $branch: String!) { \
    repository(owner: $owner, name: $name) { mergeQueue(branch: $branch) { \
    entries(first: 100) { nodes { position state enqueuedAt estimatedTimeToMerge \
    headCommit { oid } pullRequest { number } } } } } }";

const REMOVAL: &str = "query($owner: String!, $name: String!, $number: Int!) { \
    repository(owner: $owner, name: $name) { pullRequest(number: $number) { \
    timelineItems(last: 1, itemTypes: [REMOVED_FROM_MERGE_QUEUE_EVENT]) { nodes { \
    ... on RemovedFromMergeQueueEvent { reason createdAt } } } } } }";

/// A PR in a queue, as GitHub gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub number: u64,
    /// GitHub's state, in lowercase.
    pub state: String,
    pub position: u64,
    pub enqueued_at: String,
    /// When the queue read expected the PR to merge.
    pub estimated_merge_at: Option<String>,
    /// The commit the queue tests.
    pub sha: Option<String>,
}

/// What one base last gave, and when.
#[derive(Default)]
struct Slot {
    read_at: Option<Instant>,
    /// `None` until the base answered; empty without a queue.
    entries: Option<Vec<Entry>>,
}

impl Slot {
    fn find(&self, number: u64) -> Option<Option<Entry>> {
        let entries = self.entries.as_ref()?;
        Some(entries.iter().find(|e| e.number == number).cloned())
    }
}

/// The queue of each base, shared by every branch followed. One lock per
/// base: two branches on one base read it once.
#[derive(Clone, Default)]
pub struct Queues(Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<Slot>>>>>);

impl Queues {
    /// The entry of PR `number` in the queue of `base`, read again when
    /// due: `None` until the base answered once, `Some(None)` when the PR
    /// is not in its queue or the base has none.
    pub async fn entry(&self, base: &Base<'_>, number: u64) -> Option<Option<Entry>> {
        let key = format!("{}/{}#{}", base.owner, base.name, base.base);
        let slot = self.0.lock().unwrap().entry(key).or_default().clone();
        let mut slot = slot.lock().await;
        let pace = match slot.find(number) {
            Some(Some(_)) => FAST,
            _ => RHYTHM.normal,
        };
        let now = Instant::now();
        if slot.read_at.is_none_or(|at| now >= at + pace) {
            match entries(base).await {
                Ok(entries) => slot.entries = Some(entries),
                Err(failure) => crate::log(format_args!(
                    "github.com/{}/{}@{}: merge queue unread: {failure:?}",
                    base.owner, base.name, base.base
                )),
            }
            // Set once read: a reading cancelled midway leaves the next
            // caller to read again.
            slot.read_at = Some(now);
        }
        slot.find(number)
    }

    /// Makes every base read `by` earlier than it was.
    #[cfg(test)]
    pub fn age(&self, by: Duration) {
        for slot in self.0.lock().unwrap().values() {
            let mut slot = slot.try_lock().unwrap();
            slot.read_at = slot.read_at.and_then(|at| at.checked_sub(by));
        }
    }
}

/// The entries of the base's queue; none without a queue.
async fn entries(base: &Base<'_>) -> Result<Vec<Entry>, Failure> {
    let variables = json!({ "owner": base.owner, "name": base.name, "branch": base.base });
    let answer = graphql(base, QUEUE, variables).await?;
    let now = Timestamp::now().as_second();
    let nodes = &answer["data"]["repository"]["mergeQueue"]["entries"]["nodes"];
    Ok(nodes
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| entry(n, now))
        .collect())
}

/// An entry read at `now`, in seconds: GitHub estimates the time left
/// until it merges.
fn entry(node: &Value, now: i64) -> Option<Entry> {
    // To the minute: `now` drifts from GitHub's clock by a second from one
    // reading to the next, which would publish a new estimate each time.
    let estimated = node["estimatedTimeToMerge"]
        .as_i64()
        .and_then(|left| now.checked_add(left))
        .and_then(|at| Timestamp::from_second(at.saturating_add(30).div_euclid(60) * 60).ok());
    Some(Entry {
        number: node["pullRequest"]["number"].as_u64()?,
        state: node["state"].as_str()?.to_ascii_lowercase(),
        position: node["position"].as_u64()?,
        enqueued_at: node["enqueuedAt"].as_str()?.to_owned(),
        estimated_merge_at: estimated.map(|t| t.to_string()),
        sha: node["headCommit"]["oid"].as_str().map(str::to_owned),
    })
}

/// Why PR `number` last left the queue; `None` when it never did.
pub async fn removal(base: &Base<'_>, number: u64) -> Result<Option<Dequeued>, Failure> {
    let variables = json!({ "owner": base.owner, "name": base.name, "number": number });
    let answer = graphql(base, REMOVAL, variables).await?;
    let nodes = &answer["data"]["repository"]["pullRequest"]["timelineItems"]["nodes"];
    Ok(nodes.as_array().and_then(|n| n.last()).and_then(|n| {
        Some(Dequeued {
            reason: n["reason"].as_str().map(str::to_owned),
            at: n["createdAt"].as_str()?.to_owned(),
        })
    }))
}
