//! Keeps everything a final delivery goes through before it is stored (its pull requests on
//! GitHub, its worktree in git, the public review and the store itself) apart from the call that
//! asked for it. The call is answered within a budget below what a node waits: with the outcome,
//! or with a typed "still checking". The work goes on when the call is cut, and the same call
//! sent again, with the same interaction id, joins it instead of starting another.
use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use alondra_core::domain::{DomainError, InteractionId};
use serde_json::Value;
use tokio::sync::watch;

/// What a delivery ended in: the store's answer, or why nothing was stored.
pub(crate) type Delivered = Result<Value, DomainError>;

/// How long a finished delivery waits for the call that asked for it to come back.
const KEPT: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// How long a call waits for its delivery before it answers "still checking": less than the
    /// 30 s a node's MCP call waits for alondrad.
    pub answer_within: Duration,
    /// How long git may take on each check in the node's worktree.
    pub git_within: Duration,
}

impl Limits {
    /// A sandbox answers in seconds, so a journey sees "still checking" without waiting 20 s.
    pub(crate) const SANDBOX: Limits = Limits { answer_within: Duration::from_secs(2), git_within: crate::worktree::CHECK_LIMIT };
}

impl Default for Limits {
    fn default() -> Self {
        Self { answer_within: Duration::from_secs(20), git_within: crate::worktree::CHECK_LIMIT }
    }
}

struct Running {
    /// The caller and command it runs for: the same interaction id sent with another body, or by
    /// another session, never picks up its outcome.
    fingerprint: String,
    finished: watch::Receiver<Option<Delivered>>,
    started: Instant,
}

#[derive(Default)]
pub(crate) struct DeliveryReviews {
    running: Mutex<HashMap<InteractionId, Running>>,
    limits: Mutex<Limits>,
}

fn still_checking() -> DomainError {
    DomainError::new(
        "delivery_review_pending",
        "Alondra is still checking this delivery (its pull requests on GitHub, its worktree in git) and has not stored it yet. Nothing was lost: send the same present_result again, with the same interaction_id, in a few seconds. It joins the check already running and answers when it ends.",
    )
}

fn earlier_still_checking() -> DomainError {
    DomainError::new(
        "delivery_review_pending",
        "Alondra is still checking an earlier version of this delivery, sent with the same interaction_id. Wait a few seconds and send it again: once that check ends, this version is checked.",
    )
}

impl DeliveryReviews {
    pub(crate) fn limits(&self) -> Limits {
        *guard(&self.limits)
    }

    pub(crate) fn limit(&self, limits: Limits) {
        *guard(&self.limits) = limits;
    }

    /// The outcome of the delivery `interaction`: what `work` ends in if it ends within the
    /// budget, otherwise `delivery_review_pending` while `work` keeps running. The outcome is
    /// handed over once.
    pub(crate) async fn deliver<W>(
        &self,
        interaction: &InteractionId,
        fingerprint: String,
        work: impl FnOnce() -> W,
    ) -> Delivered
    where
        W: Future<Output = Delivered> + Send + 'static,
    {
        let Some(mut finished) = self.join_or_start(interaction, fingerprint, work) else {
            return Err(earlier_still_checking());
        };
        match tokio::time::timeout(self.limits().answer_within, finished.wait_for(Option::is_some)).await {
            Err(_) => Err(still_checking()),
            Ok(Ok(delivered)) => {
                let delivered = delivered.clone().expect("waited for a finished delivery");
                guard(&self.running).remove(interaction);
                delivered
            }
            Ok(Err(_)) => {
                guard(&self.running).remove(interaction);
                Err(DomainError::new(
                    "public_content_review_unavailable",
                    "The check of this delivery stopped before it finished, and nothing was stored. Deliver again.",
                ))
            }
        }
    }

    /// The delivery running for `interaction` and `fingerprint`, started now if none is. None
    /// while another body of the same interaction is still being checked.
    fn join_or_start<W>(
        &self,
        interaction: &InteractionId,
        fingerprint: String,
        work: impl FnOnce() -> W,
    ) -> Option<watch::Receiver<Option<Delivered>>>
    where
        W: Future<Output = Delivered> + Send + 'static,
    {
        let mut running = guard(&self.running);
        running.retain(|_, entry| {
            entry.finished.borrow().is_none() || entry.started.elapsed() < KEPT
        });
        if let Some(entry) = running.get(interaction) {
            if entry.fingerprint == fingerprint {
                return Some(entry.finished.clone());
            }
            if entry.finished.borrow().is_none() {
                return None;
            }
        }
        let (sender, finished) = watch::channel(None);
        let work = work();
        tokio::spawn(async move {
            sender.send_replace(Some(work.await));
        });
        running.insert(
            interaction.clone(),
            Running {
                fingerprint,
                finished: finished.clone(),
                started: Instant::now(),
            },
        );
        Some(finished)
    }
}

fn guard<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
