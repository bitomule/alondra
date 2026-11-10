//! Keeps the GitHub reads a final delivery needs apart from the call that asked for them. A slow
//! GitHub answers that call with a typed "still reading", and the same call sent again, with the
//! same interaction id, joins the read already running instead of starting another.
use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use alondra_core::domain::{DomainError, InteractionId};
use tokio::sync::watch;

use crate::public_content::Reviewed;

pub(crate) type Review = Result<Option<Reviewed>, DomainError>;

/// How long a finished review waits for the call that asked for it to come back.
const KEPT: Duration = Duration::from_secs(600);

struct Running {
    finished: watch::Receiver<Option<Review>>,
    started: Instant,
}

#[derive(Default)]
pub(crate) struct DeliveryReviews {
    running: Mutex<HashMap<InteractionId, Running>>,
}

fn still_reading() -> DomainError {
    DomainError::new(
        "delivery_review_pending",
        "Alondra is still reading this delivery's pull requests on GitHub and has not stored the result yet. Nothing was lost: send the same present_result again, with the same interaction_id, in a few seconds. It joins the read already running and answers when it ends.",
    )
}

impl DeliveryReviews {
    /// The review of `interaction`: what `work` finds if it ends within `budget`, otherwise
    /// `delivery_review_pending` while `work` keeps running. The result is handed over once.
    pub(crate) async fn review<W>(
        &self,
        interaction: &InteractionId,
        budget: Duration,
        work: impl FnOnce() -> W,
    ) -> Review
    where
        W: Future<Output = Review> + Send + 'static,
    {
        let mut finished = self.join_or_start(interaction, work);
        match tokio::time::timeout(budget, finished.wait_for(Option::is_some)).await {
            Err(_) => Err(still_reading()),
            Ok(Ok(review)) => {
                let review = review.clone().expect("waited for a finished review");
                review_guard(&self.running).remove(interaction);
                review
            }
            Ok(Err(_)) => {
                review_guard(&self.running).remove(interaction);
                Err(DomainError::new(
                    "public_content_review_unavailable",
                    "The delivery review stopped before it finished. Deliver again.",
                ))
            }
        }
    }

    fn join_or_start<W>(
        &self,
        interaction: &InteractionId,
        work: impl FnOnce() -> W,
    ) -> watch::Receiver<Option<Review>>
    where
        W: Future<Output = Review> + Send + 'static,
    {
        let mut running = review_guard(&self.running);
        running.retain(|_, entry| {
            entry.finished.borrow().is_none() || entry.started.elapsed() < KEPT
        });
        if let Some(entry) = running.get(interaction) {
            return entry.finished.clone();
        }
        let (sender, finished) = watch::channel(None);
        let work = work();
        tokio::spawn(async move {
            sender.send_replace(Some(work.await));
        });
        running.insert(
            interaction.clone(),
            Running {
                finished: finished.clone(),
                started: Instant::now(),
            },
        );
        finished
    }
}

fn review_guard<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
