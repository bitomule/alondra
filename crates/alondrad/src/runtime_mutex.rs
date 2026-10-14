use std::sync::{LockResult, Mutex, MutexGuard, TryLockError};

/// Existing synchronous producers must not strand Tokio's workers while waiting for a lock.
/// CPU-heavy work still belongs in a bounded blocking job; this covers contended lock waits.
pub(crate) struct RuntimeMutex<T> {
    inner: Mutex<T>,
    #[cfg(test)]
    wait_hook: Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>,
}
impl<T> RuntimeMutex<T> {
    pub(crate) fn new(value: T) -> Self {
        Self { inner: Mutex::new(value), #[cfg(test)] wait_hook: Mutex::new(None) }
    }

    pub(crate) fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
        match self.inner.try_lock() {
            Ok(guard) => Ok(guard),
            Err(TryLockError::Poisoned(error)) => Err(error),
            Err(TryLockError::WouldBlock) => {
                let wait = || {
                    #[cfg(test)]
                    if let Some(hook) = self.wait_hook.lock().unwrap().clone() { hook(); }
                    self.inner.lock()
                };
                if tokio::runtime::Handle::try_current().is_ok_and(|handle|
                    matches!(handle.runtime_flavor(), tokio::runtime::RuntimeFlavor::MultiThread))
                {
                    tokio::task::block_in_place(wait)
                } else {
                    wait()
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn on_wait(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self.wait_hook.lock().unwrap() = Some(std::sync::Arc::new(hook));
    }
}
impl<T: Default> Default for RuntimeMutex<T> {
    fn default() -> Self { Self::new(T::default()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Holder {
        release: Option<std::sync::mpsc::Sender<()>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Holder {
        fn drop(&mut self) {
            if let Some(release) = self.release.take() { let _ = release.send(()); }
            if let Some(thread) = self.thread.take() { thread.join().unwrap(); }
        }
    }
    fn hold(lock: &Arc<RuntimeMutex<usize>>) -> Holder {
        let (taken, ready) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        let lock = lock.clone();
        let thread = std::thread::spawn(move || {
            let _guard = lock.lock().unwrap();
            taken.send(()).unwrap();
            held.recv().unwrap();
        });
        ready.recv().unwrap();
        Holder { release: Some(release), thread: Some(thread) }
    }

    #[test]
    fn runtime_progress_a_contended_lock_hands_the_worker_to_other_tasks() {
        let lock = Arc::new(RuntimeMutex::new(0));
        let holder = hold(&lock);
        let (entered, started) = tokio::sync::oneshot::channel();
        let entered = Mutex::new(Some(entered));
        lock.on_wait(move || {
            let entered = entered.lock().unwrap().take().unwrap();
            // Re-entering is legal only after block_in_place leaves the async context.
            tokio::runtime::Handle::current().block_on(async {});
            entered.send(()).unwrap();
        });
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        runtime.block_on(async {
            let holder = holder;
            let waiting = tokio::spawn({
                let lock = lock.clone();
                async move { *lock.lock().unwrap() += 1; }
            });
            started.await.expect("the contended wait must hand off its worker");
            assert_eq!(tokio::spawn(async { 42 }).await.unwrap(), 42, "another task progresses while the lock is held");
            drop(holder);
            waiting.await.unwrap();
        });
        assert_eq!(*lock.lock().unwrap(), 1);
    }

    #[test]
    fn a_current_thread_runtime_preserves_synchronous_locking_without_panicking() {
        let lock = Arc::new(RuntimeMutex::new(0));
        let mut holder = hold(&lock);
        let release = holder.release.take().unwrap();
        lock.on_wait(move || { let _ = release.send(()); });
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async { *lock.lock().unwrap() += 1; });
        drop(holder);
        assert_eq!(*lock.lock().unwrap(), 1);
    }

    #[test]
    fn poison_keeps_the_guard_and_the_original_value() {
        let lock = Arc::new(RuntimeMutex::new(7));
        let poison = lock.clone();
        assert!(std::thread::spawn(move || { let _guard = poison.lock().unwrap(); panic!("poison"); }).join().is_err());
        assert_eq!(*lock.lock().unwrap_err().into_inner(), 7);
    }
}
