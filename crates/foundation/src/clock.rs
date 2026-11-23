//! The one clock of the daemon: every reading of the time and every wait goes through here, so a
//! test can run the daemon on a virtual clock that only moves when it is told to.
//!
//! The system clock is the wall clock and tokio's real timers. The virtual clock sits on tokio's
//! paused time: the runtime jumps to the next timer when everything is idle, and a fixed epoch
//! turns the elapsed virtual time into the milliseconds a reading returns.
//!
//! A clock is current for a thread. A virtual runtime installs its clock in every thread it owns,
//! blocking ones included. A thread of its own starts with [`spawn_thread`], which hands it the
//! clock and the runtime of the thread that starts it, or enters both with [`enter`]: a virtual
//! clock without its runtime would read the real time.
//!
//! The runtime sees its tasks and its blocking pool, never a thread of its own: while such a thread
//! works, an idle runtime still jumps to its next timer. Work that virtual time must wait for runs
//! on the blocking pool, which holds virtual time until it ends.
//!
//! A test that moves virtual time itself waits first for [`working`] to reach zero: the threads of
//! its own the runtime started, or woke with a timer or a notification, that do not wait again yet.
//! What such a thread does in real time (ending a process, writing the store) then lands at the
//! instant that woke it, however long it takes.

use std::cell::Cell;
use std::future::Future;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

pub use tokio::time::{Instant, MissedTickBehavior, error::Elapsed};

use crate::Millis;

/// A virtual clock knows the deadlines of the timed waits pending on it, so whoever drives it can
/// tell, before moving the time forward, whether the move can wake anyone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clock {
    System,
    Virtual { epoch_millis: Millis, origin: Instant, work: &'static Work, deadlines: &'static Deadlines },
}

/// How many threads of a virtual runtime work in real time: each started with [`spawn_thread`] or
/// woken by the runtime's clock or a notification, until it waits again on the clock or on a stream,
/// or ends. One lives for each virtual runtime, for as long as the process.
///
/// While any works, a task of the runtime's blocking pool waits for them, so the runtime does not
/// jump to its next timer on its own either.
#[derive(Debug, Default)]
pub struct Work {
    count: AtomicUsize,
    held: std::sync::Mutex<Hold>,
    idle: std::sync::Condvar,
}

#[derive(Debug, Default)]
struct Hold {
    /// The runtime, until it ends.
    runtime: Option<tokio::runtime::Handle>,
    /// A task of its blocking pool waits for the threads that work.
    waiting: bool,
}

impl PartialEq for Work {
    fn eq(&self, other: &Work) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Eq for Work {}

impl Work {
    fn add(&'static self) {
        let mut held = self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.count.fetch_add(1, Ordering::SeqCst);
        if held.waiting {
            return;
        }
        let Some(runtime) = held.runtime.clone() else { return };
        held.waiting = true;
        drop(held);
        // Spawning it holds the runtime's own jumps at once; the task lets go once none works.
        runtime.spawn_blocking(move || {
            let mut held = self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            while self.count.load(Ordering::SeqCst) > 0 && held.runtime.is_some() {
                held = self.idle.wait(held).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            held.waiting = false;
        });
    }

    fn remove(&self) {
        let _held = self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.count.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.idle.notify_all();
        }
    }

    /// The runtime ends: nothing waits for its threads any more.
    fn end(&self) {
        self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner).runtime = None;
        self.idle.notify_all();
    }
}

/// Whether this thread counts in its runtime's [`Work`], and the unit it holds there while it works.
struct Held {
    counted: Cell<bool>,
    unit: Cell<Option<&'static Work>>,
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(work) = self.unit.take() {
            work.remove();
        }
    }
}

/// The deadlines of the timed waits pending on one virtual clock, each with how many waits share it.
#[derive(Debug, Default)]
pub struct Deadlines(Mutex<BTreeMap<Instant, usize>>);

impl Deadlines {
    fn pending(&self) -> std::sync::MutexGuard<'_, BTreeMap<Instant, usize>> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One timed wait's deadline, registered on the current virtual clock from the moment the wait is
/// made until it ends or is dropped.
struct Pending {
    deadlines: Option<&'static Deadlines>,
    at: Instant,
}

impl Pending {
    fn new(at: Instant) -> Pending {
        let deadlines = match Clock::current() {
            Clock::Virtual { deadlines, .. } => Some(deadlines),
            Clock::System => None,
        };
        if let Some(deadlines) = deadlines {
            *deadlines.pending().entry(at).or_default() += 1;
        }
        Pending { deadlines, at }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        let Some(deadlines) = self.deadlines else { return };
        let mut pending = deadlines.pending();
        if let Some(waits) = pending.get_mut(&self.at) {
            *waits -= 1;
            if *waits == 0 {
                pending.remove(&self.at);
            }
        }
    }
}

impl PartialEq for Deadlines {
    fn eq(&self, other: &Deadlines) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Eq for Deadlines {}

thread_local! {
    static CURRENT: Cell<Clock> = const { Cell::new(Clock::System) };
    static HELD: Held = const { Held { counted: Cell::new(false), unit: Cell::new(None) } };
}

/// The thread waits: virtual time no longer waits for it.
fn rest() {
    let _ = HELD.try_with(|held| {
        if let Some(work) = held.unit.take() {
            work.remove();
        }
    });
}

/// The thread works again. `handed` says whoever woke it already counted it: a thread that does not
/// count gives that unit back at once.
fn resume(work: &'static Work, handed: bool) {
    let counted = HELD.try_with(|held| {
        if !held.counted.get() {
            return false;
        }
        if !handed {
            work.add();
        }
        if let Some(previous) = held.unit.replace(Some(work)) {
            previous.remove();
        }
        true
    });
    if handed && counted != Ok(true) {
        work.remove();
    }
}

/// One wait of a thread on the virtual clock. The timer or the notification that ends it counts the
/// thread as working before the thread runs, so no test sees the runtime idle in between.
struct Waiter {
    work: &'static Work,
    woken: std::sync::Mutex<bool>,
}

impl Waiter {
    fn new(work: &'static Work) -> Arc<Waiter> {
        Arc::new(Waiter { work, woken: std::sync::Mutex::new(false) })
    }

    fn wake(&self) {
        let mut woken = self.woken.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*woken {
            *woken = true;
            self.work.add();
        }
    }

    fn woken(&self) -> bool {
        *self.woken.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Ends the wait: whether a wake counted the thread, which then holds that unit.
    fn claim(&self) -> bool {
        std::mem::replace(&mut *self.woken.lock().unwrap_or_else(std::sync::PoisonError::into_inner), true)
    }
}

/// How many threads of the current virtual runtime work in real time; zero under the system clock.
pub fn working() -> usize {
    match Clock::current() {
        Clock::Virtual { work, .. } => work.count.load(Ordering::SeqCst),
        Clock::System => 0,
    }
}

/// One unit of real-time work in flight, counted in [`working`] until dropped.
pub struct RealWork(Option<&'static Work>);

impl Drop for RealWork {
    fn drop(&mut self) {
        if let Some(work) = self.0 {
            work.remove();
        }
    }
}

/// Runs `future`, which waits on the real world (a process, the disk) from a task of the runtime,
/// as work virtual time waits for: from its first poll until it ends or is dropped, [`working`]
/// counts it and the runtime does not jump to its next timer. Without it a task parked on a
/// process lets the runtime look idle, and whatever the task does after is stamped some virtual
/// seconds later than the instant that started it. The future must not wait on the clock itself.
/// Under the system clock it is the future.
pub async fn real_time<F: Future>(future: F) -> F::Output {
    let _counted = match Clock::current() {
        Clock::Virtual { work, .. } => {
            work.add();
            RealWork(Some(work))
        }
        Clock::System => RealWork(None),
    };
    future.await
}

/// A wait for a stream another thread writes, such as a host's socket: what ends it is that
/// writer's, never the clock's, so virtual time does not wait for the thread while it lasts.
pub struct StreamWait(Option<&'static Work>);

pub fn stream_wait() -> StreamWait {
    match Clock::current() {
        Clock::Virtual { work, .. } => {
            rest();
            StreamWait(Some(work))
        }
        Clock::System => StreamWait(None),
    }
}

impl Drop for StreamWait {
    fn drop(&mut self) {
        if let Some(work) = self.0 {
            resume(work, false);
        }
    }
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
            Clock::Virtual { epoch_millis, origin, .. } => {
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

/// Whether a timed wait pending on the current virtual clock ends within `window` from now, so
/// moving the time forward by `window` would wake it; never on the system clock.
pub fn deadline_within(window: Duration) -> bool {
    match Clock::current() {
        Clock::Virtual { deadlines, .. } => {
            let horizon = instant() + window;
            deadlines.pending().range(..=horizon).next().is_some()
        }
        Clock::System => false,
    }
}

pub fn sleep(duration: Duration) -> impl Future<Output = ()> + Send {
    sleep_until(instant() + duration)
}

pub fn sleep_until(deadline: Instant) -> impl Future<Output = ()> + Send {
    let pending = Pending::new(deadline);
    async move {
        tokio::time::sleep_until(deadline).await;
        drop(pending);
    }
}

/// [`tokio::time::Interval`] on the current clock; its next tick stays registered as pending.
pub struct Interval {
    ticks: tokio::time::Interval,
    next: Pending,
}

impl Interval {
    fn new(start: Instant, period: Duration) -> Interval {
        Interval { ticks: tokio::time::interval_at(start, period), next: Pending::new(start) }
    }

    /// The tick after `at` is registered at `at + period`: no later than tokio schedules it,
    /// whichever the missed-tick behaviour.
    pub async fn tick(&mut self) -> Instant {
        let at = self.ticks.tick().await;
        self.next = Pending::new(at + self.ticks.period());
        at
    }

    pub fn set_missed_tick_behavior(&mut self, behavior: MissedTickBehavior) {
        self.ticks.set_missed_tick_behavior(behavior);
    }
}

pub fn interval(period: Duration) -> Interval {
    Interval::new(instant(), period)
}

pub fn interval_at(start: Instant, period: Duration) -> Interval {
    Interval::new(start, period)
}

pub fn timeout_at<F: Future>(deadline: Instant, future: F) -> impl Future<Output = Result<F::Output, Elapsed>> {
    let pending = Pending::new(deadline);
    async move {
        let ended = tokio::time::timeout_at(deadline, future).await;
        drop(pending);
        ended
    }
}

pub fn timeout<F: Future>(duration: Duration, future: F) -> impl Future<Output = Result<F::Output, Elapsed>> {
    timeout_at(instant() + duration, future)
}

/// Waits on a thread that is allowed to block. In a virtual runtime the wait is the runtime's
/// own, so it ends when virtual time reaches it, never after real time.
pub fn blocking_sleep(duration: Duration) {
    match (Clock::current(), tokio::runtime::Handle::try_current()) {
        (Clock::Virtual { work, .. }, Ok(runtime)) => {
            let waiter = Waiter::new(work);
            let (timer, deadline) = (waiter.clone(), Instant::now() + duration);
            let _pending = Pending::new(deadline);
            let woken = runtime.spawn(async move {
                tokio::time::sleep_until(deadline).await;
                timer.wake();
            });
            rest();
            let _ = runtime.block_on(woken);
            resume(work, waiter.claim());
        }
        _ => std::thread::sleep(duration),
    }
}

/// How long since `start`, on the current clock.
pub fn since(start: Instant) -> Duration {
    instant().saturating_duration_since(start)
}

/// The runtime of the virtual clock current on this thread, when there is one.
fn virtual_runtime() -> Option<(tokio::runtime::Handle, &'static Work)> {
    match Clock::current() {
        Clock::Virtual { work, .. } => tokio::runtime::Handle::try_current().ok().map(|runtime| (runtime, work)),
        Clock::System => None,
    }
}

/// Counts a thread about to start under `clock` as working until it first waits, so virtual time
/// waits for what it does from its first instant.
fn starting(clock: Clock) -> Option<&'static Work> {
    match clock {
        Clock::Virtual { work, .. } => {
            work.add();
            Some(work)
        }
        Clock::System => None,
    }
}

/// The started thread takes over the unit [`starting`] counted for it.
fn started(unit: Option<&'static Work>) {
    let _ = HELD.try_with(|held| {
        held.counted.set(true);
        if let Some(previous) = held.unit.replace(unit) {
            previous.remove();
        }
    });
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
    let unit = starting(clock);
    builder
        .spawn(move || {
            let _entered = enter(clock, runtime.as_ref());
            started(unit);
            let done = work();
            rest();
            done
        })
        .inspect_err(|_| {
            if let Some(work) = unit {
                work.remove();
            }
        })
}

/// [`tokio::task::spawn_blocking`] whose job counts as working from the moment it is submitted until
/// it ends, so a test that moves virtual time waits for it (see [`working`]) instead of moving the
/// time under a job that has not read the clock yet, or has read it and not committed what it found.
/// Under the system clock it is the plain call.
pub fn spawn_blocking<F, T>(work: F) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let Clock::Virtual { work: counter, .. } = Clock::current() else { return tokio::task::spawn_blocking(work) };
    counter.add();
    let unit = Unit(counter);
    tokio::task::spawn_blocking(move || {
        let _unit = unit;
        work()
    })
}

/// One unit of [`Work`], given back when dropped, whether its job ran, panicked or never started.
struct Unit(&'static Work);

impl Drop for Unit {
    fn drop(&mut self) {
        self.0.remove();
    }
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
        let unit = starting(clock);
        self.inner.spawn(move || {
            let _entered = enter(clock, runtime.as_ref());
            started(unit);
            let done = work();
            rest();
            done
        })
    }
}

/// A mutex with a condition whose timed waits run on the current clock: under a virtual clock a
/// timer of the runtime wakes the waiter when virtual time reaches its deadline, and a notification
/// still wakes it at once.
pub struct Monitor<T> {
    shared: Arc<(std::sync::Mutex<T>, std::sync::Condvar)>,
    /// The threads waiting under a virtual clock, which a notification counts as working.
    waiters: Arc<std::sync::Mutex<Vec<Arc<Waiter>>>>,
}

/// How often a thread waiting under a virtual clock looks, in real time, whether its wait ended: a
/// notification sent while it was between counting itself as waiting and sleeping is not lost.
const LOOK_AGAIN: Duration = Duration::from_millis(10);

impl<T: Default> Default for Monitor<T> {
    fn default() -> Self {
        Monitor::new(T::default())
    }
}

impl<T> Monitor<T> {
    pub fn new(value: T) -> Monitor<T> {
        Monitor { shared: Arc::new((std::sync::Mutex::new(value), std::sync::Condvar::new())), waiters: Arc::default() }
    }

    pub fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, T>> {
        self.shared.0.lock()
    }

    pub fn notify_all(&self) {
        for waiter in self.waiters.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter() {
            waiter.wake();
        }
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
        let Some((runtime, work)) = virtual_runtime() else {
            return match condvar.wait_timeout(guard, timeout) {
                Ok((guard, _)) => Ok(guard),
                Err(poisoned) => Err(std::sync::PoisonError::new(poisoned.into_inner().0)),
            };
        };
        let waiter = Waiter::new(work);
        self.waiters.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(waiter.clone());
        let (shared, timer) = (self.shared.clone(), waiter.clone());
        let deadline = instant() + timeout;
        let _pending = Pending::new(deadline);
        let _deadline = Deadline(runtime.spawn(async move {
            tokio::time::sleep_until(deadline).await;
            timer.wake();
            let (lock, condvar) = &*shared;
            drop(lock.lock());
            condvar.notify_all();
        }));
        rest();
        let mut guard = guard;
        let waited = loop {
            match condvar.wait_timeout(guard, LOOK_AGAIN) {
                Ok((again, _)) if waiter.woken() => break Ok(again),
                Ok((again, _)) => guard = again,
                Err(poisoned) => break Err(std::sync::PoisonError::new(poisoned.into_inner().0)),
            }
        };
        self.waiters.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|other| !Arc::ptr_eq(other, &waiter));
        resume(work, waiter.claim());
        waited
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
        let work: &'static Work = Box::leak(Box::default());
        work.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner).runtime = Some(runtime.handle().clone());
        // Leaked so the clock stays `Copy` on every thread that carries it: one map per runtime.
        let deadlines: &'static Deadlines = Box::leak(Box::default());
        let clock = Clock::Virtual { epoch_millis, origin, work, deadlines };
        shared.set(clock).ok();
        Ok(VirtualRuntime { runtime, clock })
    }

    pub fn clock(&self) -> Clock {
        self.clock
    }

    /// Makes this runtime and its clock the current ones of the calling thread until the guard drops.
    pub fn enter(&self) -> EnteredRuntime<'_> {
        enter(self.clock, Some(self.runtime.handle()))
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        let _entered = self.clock.enter();
        self.runtime.block_on(future)
    }
}

impl Drop for VirtualRuntime {
    fn drop(&mut self) {
        if let Clock::Virtual { work, .. } = self.clock {
            work.end();
        }
    }
}

pub struct EnteredRuntime<'a> {
    _runtime: Option<tokio::runtime::EnterGuard<'a>>,
    _clock: Entered,
}

/// Makes `clock` and the runtime it runs on the current ones of the calling thread until the guard
/// drops: a virtual clock reads its runtime's paused time.
pub fn enter(clock: Clock, runtime: Option<&tokio::runtime::Handle>) -> EnteredRuntime<'_> {
    EnteredRuntime { _clock: clock.enter(), _runtime: runtime.map(tokio::runtime::Handle::enter) }
}

/// Moves the paused time forward by `duration`, running every timer that falls inside it.
pub async fn advance(duration: Duration) {
    tokio::time::advance(duration).await
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
