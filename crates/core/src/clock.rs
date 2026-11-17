//! The one clock of the daemon: every reading of the time and every wait goes through here, so a
//! test can run the daemon on a virtual clock that only moves when it is told to.
//!
//! The system clock is the wall clock and tokio's real timers. The virtual clock sits on tokio's
//! paused time: the runtime jumps to the next timer when everything is idle, and a fixed epoch
//! turns the elapsed virtual time into the milliseconds a reading returns.
//!
//! A clock is current for a thread. A virtual runtime installs its clock in every thread it owns,
//! blocking ones included. A thread of its own starts with [`spawn_thread`], which hands it the
//! clock and the runtime of the thread that starts it, or enters both with [`VirtualRuntime::enter`]:
//! a virtual clock without its runtime would read the real time.
//!
//! The runtime sees its tasks and its blocking pool, never a thread of its own: while such a thread
//! works, an idle runtime still jumps to its next timer. Work that virtual time must wait for runs
//! on the blocking pool, which holds virtual time until it ends.

use std::cell::Cell;
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

pub use tokio::time::{Instant, Interval, error::Elapsed};

use crate::Millis;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clock {
    System,
    Virtual { epoch_millis: Millis, origin: Instant },
}

thread_local! {
    static CURRENT: Cell<Clock> = const { Cell::new(Clock::System) };
}

pub struct Entered {
    previous: Clock,
}

impl Drop for Entered {
    fn drop(&mut self) {
        CURRENT.with(|current| current.set(self.previous));
    }
}

impl Clock {
    pub fn current() -> Clock {
        CURRENT.with(Cell::get)
    }

    fn enter(self) -> Entered {
        Entered { previous: CURRENT.with(|current| current.replace(self)) }
    }

    pub fn now_millis(self) -> Millis {
        match self {
            Clock::System => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as Millis)
                .unwrap_or(0),
            Clock::Virtual { epoch_millis, origin } => {
                epoch_millis.saturating_add(Instant::now().saturating_duration_since(origin).as_millis() as Millis)
            }
        }
    }

    pub fn system_time(self) -> std::time::SystemTime {
        let millis = self.now_millis().max(0) as u64;
        std::time::UNIX_EPOCH + Duration::from_millis(millis)
    }

    /// A monotonic reading. Tokio's instant is the real one, and the paused one in a virtual runtime.
    pub fn instant(self) -> Instant {
        Instant::now()
    }
}

pub fn now_millis() -> Millis {
    Clock::current().now_millis()
}

pub fn system_time() -> std::time::SystemTime {
    Clock::current().system_time()
}

pub fn instant() -> Instant {
    Clock::current().instant()
}

pub async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await
}

pub async fn sleep_until(deadline: Instant) {
    tokio::time::sleep_until(deadline).await
}

pub fn interval(period: Duration) -> Interval {
    tokio::time::interval(period)
}

pub fn interval_at(start: Instant, period: Duration) -> Interval {
    tokio::time::interval_at(start, period)
}

pub async fn timeout_at<F: Future>(deadline: Instant, future: F) -> Result<F::Output, Elapsed> {
    tokio::time::timeout_at(deadline, future).await
}

pub async fn timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, Elapsed> {
    tokio::time::timeout(duration, future).await
}

/// Waits on a thread that is allowed to block. In a virtual runtime the wait is the runtime's
/// own, so it ends when virtual time reaches it, never after real time.
pub fn blocking_sleep(duration: Duration) {
    match (Clock::current(), tokio::runtime::Handle::try_current()) {
        (Clock::Virtual { .. }, Ok(runtime)) => runtime.block_on(tokio::time::sleep(duration)),
        _ => std::thread::sleep(duration),
    }
}

/// How long since `start`, on the current clock.
pub fn since(start: Instant) -> Duration {
    instant().saturating_duration_since(start)
}

/// The runtime of the virtual clock current on this thread, when there is one.
fn virtual_runtime() -> Option<tokio::runtime::Handle> {
    match Clock::current() {
        Clock::Virtual { .. } => tokio::runtime::Handle::try_current().ok(),
        Clock::System => None,
    }
}

/// A thread that keeps the clock of the thread that starts it, and its runtime when it has one,
/// so its readings and waits are virtual under a virtual clock.
pub fn spawn_thread<F, T>(work: F) -> std::thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    spawn_named_thread(None, work).expect("failed to spawn thread")
}

pub fn spawn_named_thread<F, T>(name: Option<String>, work: F) -> std::io::Result<std::thread::JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let clock = Clock::current();
    let runtime = tokio::runtime::Handle::try_current().ok();
    let builder = match name {
        Some(name) => std::thread::Builder::new().name(name),
        None => std::thread::Builder::new(),
    };
    builder.spawn(move || {
        let _clock = clock.enter();
        let _runtime = runtime.as_ref().map(tokio::runtime::Handle::enter);
        work()
    })
}

/// [`std::thread::scope`] whose threads keep the clock and the runtime of the thread that opens it.
pub fn scope<'env, F, T>(work: F) -> T
where
    F: for<'scope> FnOnce(Scope<'scope, 'env>) -> T,
{
    let (clock, runtime) = (Clock::current(), tokio::runtime::Handle::try_current().ok());
    std::thread::scope(|inner| work(Scope { inner, clock, runtime }))
}

pub struct Scope<'scope, 'env: 'scope> {
    inner: &'scope std::thread::Scope<'scope, 'env>,
    clock: Clock,
    runtime: Option<tokio::runtime::Handle>,
}

impl<'scope> Scope<'scope, '_> {
    pub fn spawn<F, T>(&self, work: F) -> std::thread::ScopedJoinHandle<'scope, T>
    where
        F: FnOnce() -> T + Send + 'scope,
        T: Send + 'scope,
    {
        let (clock, runtime) = (self.clock, self.runtime.clone());
        self.inner.spawn(move || {
            let _clock = clock.enter();
            let _runtime = runtime.as_ref().map(tokio::runtime::Handle::enter);
            work()
        })
    }
}

/// A mutex with a condition whose timed waits run on the current clock: under a virtual clock a
/// timer of the runtime wakes the waiter when virtual time reaches its deadline, and a notification
/// still wakes it at once.
pub struct Monitor<T> {
    shared: Arc<(std::sync::Mutex<T>, std::sync::Condvar)>,
}

impl<T: Default> Default for Monitor<T> {
    fn default() -> Self {
        Monitor::new(T::default())
    }
}

impl<T> Monitor<T> {
    pub fn new(value: T) -> Monitor<T> {
        Monitor { shared: Arc::new((std::sync::Mutex::new(value), std::sync::Condvar::new())) }
    }

    pub fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, T>> {
        self.shared.0.lock()
    }

    pub fn notify_all(&self) {
        self.shared.1.notify_all();
    }

    pub fn clear_poison(&self) {
        self.shared.0.clear_poison();
    }
}

impl<T: Send + 'static> Monitor<T> {
    /// Waits for a notification or for `timeout`. It may return earlier, as a condvar may: the
    /// caller checks its condition and its deadline on the clock.
    pub fn wait<'a>(
        &'a self,
        guard: std::sync::MutexGuard<'a, T>,
        timeout: Duration,
    ) -> std::sync::LockResult<std::sync::MutexGuard<'a, T>> {
        let (_, condvar) = &*self.shared;
        let Some(runtime) = virtual_runtime() else {
            return match condvar.wait_timeout(guard, timeout) {
                Ok((guard, _)) => Ok(guard),
                Err(poisoned) => Err(std::sync::PoisonError::new(poisoned.into_inner().0)),
            };
        };
        let shared = self.shared.clone();
        let deadline = instant() + timeout;
        let _deadline = Deadline(runtime.spawn(async move {
            tokio::time::sleep_until(deadline).await;
            let (lock, condvar) = &*shared;
            drop(lock.lock());
            condvar.notify_all();
        }));
        condvar.wait(guard)
    }
}

/// The timer that ends a thread's virtual wait; the wait that ends first takes it away.
struct Deadline(tokio::task::JoinHandle<()>);

impl Drop for Deadline {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Queue<T> {
    items: std::collections::VecDeque<T>,
    senders: usize,
}

/// A channel whose receiver waits with a timeout on the current clock: what a thread of the daemon
/// uses to wait for another one, such as the one that waits for a child process.
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let queue = Arc::new(Monitor::new(Queue { items: std::collections::VecDeque::new(), senders: 1 }));
    (Sender(queue.clone()), Receiver(queue))
}

pub struct Sender<T>(Arc<Monitor<Queue<T>>>);

pub struct Receiver<T>(Arc<Monitor<Queue<T>>>);

impl<T> Sender<T> {
    pub fn send(&self, value: T) -> Result<(), std::sync::mpsc::SendError<T>> {
        let mut queue = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if Arc::strong_count(&self.0) == queue.senders {
            return Err(std::sync::mpsc::SendError(value));
        }
        queue.items.push_back(value);
        self.0.notify_all();
        Ok(())
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).senders += 1;
        Sender(self.0.clone())
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).senders -= 1;
        self.0.notify_all();
    }
}

/// [`std::sync::mpsc::Receiver::recv_timeout`] on the current clock.
pub fn recv_timeout<T: Send + 'static>(receiver: &Receiver<T>, timeout: Duration) -> Result<T, std::sync::mpsc::RecvTimeoutError> {
    let deadline = instant() + timeout;
    let monitor = &*receiver.0;
    let mut queue = monitor.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        if let Some(item) = queue.items.pop_front() {
            return Ok(item);
        }
        if queue.senders == 0 {
            return Err(std::sync::mpsc::RecvTimeoutError::Disconnected);
        }
        let left = deadline.saturating_duration_since(instant());
        if left.is_zero() {
            return Err(std::sync::mpsc::RecvTimeoutError::Timeout);
        }
        queue = monitor.wait(queue, left).unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

/// A single-threaded tokio runtime with paused time and a clock that starts at a fixed epoch.
pub struct VirtualRuntime {
    runtime: tokio::runtime::Runtime,
    clock: Clock,
}

impl VirtualRuntime {
    pub fn new(epoch_millis: Millis) -> std::io::Result<VirtualRuntime> {
        let shared: Arc<OnceLock<Clock>> = Arc::new(OnceLock::new());
        let for_threads = shared.clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .on_thread_start(move || {
                if let Some(clock) = for_threads.get() {
                    std::mem::forget(clock.enter());
                }
            })
            .build()?;
        let origin = runtime.block_on(async { Instant::now() });
        let clock = Clock::Virtual { epoch_millis, origin };
        shared.set(clock).ok();
        Ok(VirtualRuntime { runtime, clock })
    }

    pub fn clock(&self) -> Clock {
        self.clock
    }

    /// Makes this runtime and its clock the current ones of the calling thread until the guard drops.
    pub fn enter(&self) -> EnteredRuntime<'_> {
        EnteredRuntime { _clock: self.clock.enter(), _runtime: self.runtime.enter() }
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        let _entered = self.clock.enter();
        self.runtime.block_on(future)
    }
}

pub struct EnteredRuntime<'a> {
    _clock: Entered,
    _runtime: tokio::runtime::EnterGuard<'a>,
}

/// Moves the paused time forward by `duration`, running every timer that falls inside it.
pub async fn advance(duration: Duration) {
    tokio::time::advance(duration).await
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
