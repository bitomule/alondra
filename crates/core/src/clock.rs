//! The one clock of the daemon: every reading of the time and every wait goes through here, so a
//! test can run the daemon on a virtual clock that only moves when it is told to.
//!
//! The system clock is the wall clock and tokio's real timers. The virtual clock sits on tokio's
//! paused time: the runtime jumps to the next timer when everything is idle, and a fixed epoch
//! turns the elapsed virtual time into the milliseconds a reading returns.
//!
//! A clock is current for a thread. A virtual runtime installs its clock in every thread it owns,
//! blocking ones included. A thread of its own makes a clock current with [`Clock::enter`] after
//! taking [`Clock::current`] from the thread that starts it.

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

    /// Makes this clock the current one of the calling thread until the guard drops.
    pub fn enter(self) -> Entered {
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

/// A single-threaded tokio runtime with paused time and a clock that starts at a fixed epoch.
pub struct VirtualRuntime {
    runtime: tokio::runtime::Runtime,
    clock: Clock,
}

impl VirtualRuntime {
    pub fn new(epoch_millis: Millis) -> std::io::Result<VirtualRuntime> {
        Self::with_thread_start(epoch_millis, || ())
    }

    /// A virtual runtime whose threads also run `on_thread_start` once their clock is current.
    pub fn with_thread_start(epoch_millis: Millis, on_thread_start: impl Fn() + Send + Sync + 'static) -> std::io::Result<VirtualRuntime> {
        let shared: Arc<OnceLock<Clock>> = Arc::new(OnceLock::new());
        let for_threads = shared.clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .on_thread_start(move || {
                if let Some(clock) = for_threads.get() {
                    std::mem::forget(clock.enter());
                }
                on_thread_start();
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

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        let _entered = self.clock.enter();
        self.runtime.block_on(future)
    }
}

/// Moves the paused time forward by `duration`, running every timer that falls inside it.
pub async fn advance(duration: Duration) {
    tokio::time::advance(duration).await
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
