use crate::locks::Poisonable as _;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// The permit follows the physical job, even when its async caller is cancelled.
#[derive(Clone)]
pub struct BlockingWork(
    Arc<Semaphore>,
    Arc<std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
);

impl BlockingWork {
    pub fn new(workers: usize) -> Self {
        Self(Arc::new(Semaphore::new(workers)), Default::default())
    }

    pub fn available(&self) -> usize { self.0.available_permits() }

    pub fn on_start(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self.1.lock_or_recover("the blocking work's start hook") = Some(Arc::new(hook));
    }

    pub async fn run<R: Send + 'static>(
        &self,
        work: impl FnOnce() -> R + Send + 'static,
    ) -> Result<R, tokio::task::JoinError> {
        let permit = self.0.clone().acquire_owned().await.expect("blocking work closed");
            let hook = self.1.lock_or_recover("the blocking work's start hook").clone();
        alondra_core::clock::spawn_blocking(move || {
                    if let Some(hook) = hook { hook(); }
            let _permit = permit;
            work()
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;

    #[tokio::test]
    async fn cancellation_keeps_the_slot_until_the_physical_work_finishes() {
        let lane = BlockingWork::new(1);
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, held) = std::sync::mpsc::channel();
        let worker = tokio::spawn({
            let lane = lane.clone();
            async move { lane.run(move || { entered.send(()).unwrap(); held.recv().unwrap(); }).await }
        });
        started.await.unwrap();
        worker.abort();
        assert!(worker.await.unwrap_err().is_cancelled());
        assert_eq!(lane.0.available_permits(), 0);
        let mut next = Box::pin(lane.run(|| 42));
        assert!(next.as_mut().poll(&mut std::task::Context::from_waker(std::task::Waker::noop())).is_pending());
        release.send(()).unwrap();
        assert_eq!(next.await.unwrap(), 42);
        assert_eq!(lane.0.available_permits(), 1);
    }

    #[tokio::test]
    async fn a_panicking_job_returns_its_slot_and_reports_failure() {
        let lane = BlockingWork::new(1);
        assert!(lane.run(|| panic!("failed job")).await.unwrap_err().is_panic());
        assert_eq!(lane.run(|| 7).await.unwrap(), 7);
        assert_eq!(lane.0.available_permits(), 1);
    }
}

/// Runs `wait`, which waits for processes to exit, without stalling the runtime's other tasks.
pub fn blocking<T>(wait: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current().map(|handle| handle.runtime_flavor()) {
        Ok(tokio::runtime::RuntimeFlavor::MultiThread) => tokio::task::block_in_place(wait),
        _ => wait(),
    }
}
