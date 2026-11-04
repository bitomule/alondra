//! A discarded reading needs a fresh fence and native read, rather than the periodic interval.
use std::future::Future;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Finished,
    Stale,
}

impl Outcome {
    pub(crate) fn of(error: &crate::domain_store::StoreError) -> Self {
        match error {
            crate::domain_store::StoreError::Domain(error) if error.code == "stale_probe" => Self::Stale,
            _ => Self::Finished,
        }
    }
}

pub(crate) async fn retry<F, P>(probe: F)
where
    F: FnMut() -> P,
    P: Future<Output = Outcome>,
{
    retry_with(probe, tokio::time::sleep).await;
}

pub(crate) async fn retry_with<F, P, W, S>(mut probe: F, mut wait: W)
where
    F: FnMut() -> P,
    P: Future<Output = Outcome>,
    W: FnMut(Duration) -> S,
    S: Future<Output = ()>,
{
    let mut backoff = Backoff::default();
    while probe().await == Outcome::Stale {
        wait(backoff.next_delay()).await;
    }
}

#[derive(Default)]
pub(crate) struct Backoff(Duration);

impl Backoff {
    pub(crate) fn next_delay(&mut self) -> Duration {
        let delay = self.0;
        self.0 = if delay.is_zero() { Duration::from_secs(1) } else { (delay * 2).min(Duration::from_secs(30)) };
        delay
    }
}

#[cfg(test)]
#[path = "native_probe_tests.rs"]
mod tests;
