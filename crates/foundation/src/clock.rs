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
use std::collections::{BTreeMap, BTreeSet};
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
    /// The frames written to each host that it has not read and acted on yet, and the hosts gone.
    transit: std::sync::Mutex<Transit>,
    /// The jobs of the blocking pool still running, by where they were submitted.
    jobs: std::sync::Mutex<BTreeMap<&'static std::panic::Location<'static>, usize>>,
    /// The runtime's own thread has no task to run and sleeps.
    runtime_parked: std::sync::atomic::AtomicBool,
    /// How many times a unit of work started or ended: a wait for calm that sees it grow knows the
    /// daemon is not stuck, only slow.
    moves: std::sync::atomic::AtomicU64,
}

#[derive(Debug, Default)]
struct Transit {
    /// The frames written to each host, by its id, that it has not read and acted on yet.
    frames: BTreeMap<String, usize>,
    /// The hosts that read nothing more, ever: a frame written to one of them never counts.
    gone: BTreeSet<String>,
}

#[derive(Debug, Default)]
struct Hold {
    /// The runtime, until it ends.
    runtime: Option<tokio::runtime::Handle>,
    /// A task of its blocking pool waits for the threads that work.
    waiting: bool,
    /// How many times the count came back to zero.
    rests: u64,
}

impl PartialEq for Work {
    fn eq(&self, other: &Work) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Eq for Work {}

impl Work {
    fn add(&'static self) {
        self.add_moving(true);
    }

    /// Counts one more unit; `moved` says whether it is a move the patience of a wait for calm sees.
    fn add_moving(&'static self, moved: bool) {
        let mut held = self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.count.fetch_add(1, Ordering::SeqCst);
        if moved {
            self.moves.fetch_add(1, Ordering::SeqCst);
        }
        if held.waiting {
            return;
        }
        let Some(runtime) = held.runtime.clone() else { return };
        held.waiting = true;
        drop(held);
        // Spawning it holds the runtime's own jumps at once; the task lets go once none works.
        #[expect(clippy::disallowed_methods, reason = "this job is the hold itself: counted, it would wait for itself")]
        runtime.spawn_blocking(move || {
            let mut held = self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            while self.count.load(Ordering::SeqCst) > 0 && held.runtime.is_some() {
                held = self.idle.wait(held).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            held.waiting = false;
        });
    }

    fn remove(&self) {
        self.remove_moving(true);
    }

    fn remove_moving(&self, moved: bool) {
        let mut held = self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if moved {
            self.moves.fetch_add(1, Ordering::SeqCst);
        }
        if self.count.fetch_sub(1, Ordering::SeqCst) == 1 {
            held.rests += 1;
            self.idle.notify_all();
        }
    }

    /// The runtime's thread sleeps: a wait for calm looks again.
    fn parked(&self) {
        let mut held = self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.runtime_parked.store(true, Ordering::SeqCst);
        held.rests += 1;
        self.idle.notify_all();
    }

    fn unparked(&self) {
        self.runtime_parked.store(false, Ordering::SeqCst);
    }

    /// The runtime's thread sleeps and no task was woken for it from another thread.
    fn runtime_idle(&self) -> bool {
        let runtime = self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner).runtime.clone();
        self.runtime_parked.load(Ordering::SeqCst) && runtime.is_some_and(|runtime| runtime.metrics().global_queue_depth() == 0)
    }

    fn until_idle(&self, patience: Duration, settled: impl Fn() -> bool) -> bool {
        let mut patience = Patience::new(patience, std::time::Instant::now(), self.moves.load(Ordering::SeqCst));
        loop {
            let rests = self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner).rests;
            if self.count.load(Ordering::SeqCst) == 0 && self.runtime_idle() && settled() {
                return true;
            }
            let mut held = self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            while held.rests == rests {
                let now = std::time::Instant::now();
                let left = patience.left(now);
                if held.runtime.is_none() || left.is_zero() && !patience.extend(now, self.moves.load(Ordering::SeqCst)) {
                    return false;
                }
                held = self.idle.wait_timeout(held, patience.left(now).max(Duration::from_millis(1))).unwrap_or_else(std::sync::PoisonError::into_inner).0;
            }
        }
    }

    fn jobs(&self) -> std::sync::MutexGuard<'_, BTreeMap<&'static std::panic::Location<'static>, usize>> {
        self.jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn job_ended(&self, at: &'static std::panic::Location<'static>) {
        let mut jobs = self.jobs();
        if let Some(running) = jobs.get_mut(at) {
            *running -= 1;
            if *running == 0 {
                jobs.remove(at);
            }
        }
    }

    fn transit(&self) -> std::sync::MutexGuard<'_, Transit> {
        self.transit.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The runtime ends: nothing waits for its threads any more.
    fn end(&self) {
        self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner).runtime = None;
        self.idle.notify_all();
    }
}

/// How long a wait for calm lasts: `patience` of real time with nothing moving. While units of work
/// keep starting and ending the daemon is slow, not stuck, as on a Mac whose threads wait long for a
/// core; a wait that no unit moved for `patience` is a hang.
#[derive(Debug)]
struct Patience {
    patience: Duration,
    limit: std::time::Instant,
    moves: u64,
}

impl Patience {
    fn new(patience: Duration, now: std::time::Instant, moves: u64) -> Patience {
        Patience { patience, limit: now + patience, moves }
    }

    fn left(&self, now: std::time::Instant) -> Duration {
        self.limit.saturating_duration_since(now)
    }

    /// The limit came: it moves `patience` on from `now` when work moved since the last look, and
    /// the wait is over when none did.
    fn extend(&mut self, now: std::time::Instant, moves: u64) -> bool {
        if moves == self.moves {
            return false;
        }
        self.moves = moves;
        self.limit = now + self.patience;
        true
    }
}

/// Whether this thread counts in its runtime's [`Work`], and the unit it holds there while it works.
struct Held {
    counted: Cell<bool>,
    unit: Cell<Option<&'static Work>>,
    /// The thread's last wait for a stream ended on its own timeout, with nothing to read: until it
    /// is woken by something, it moves nothing.
    idle: Cell<bool>,
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
    static HELD: Held = const { Held { counted: Cell::new(false), unit: Cell::new(None), idle: Cell::new(false) } };
}

/// The thread waits: virtual time no longer waits for it. A thread that only looked at a silent
/// stream since its last wait ends no work the patience of a wait for calm counts.
pub(crate) fn rest() {
    let _ = HELD.try_with(|held| {
        let idle = held.idle.take();
        if let Some(work) = held.unit.take() {
            work.remove_moving(!idle);
        }
    });
}

/// The thread works again. `handed` says whoever woke it already counted it: a thread that does not
/// count gives that unit back at once.
fn resume(work: &'static Work, handed: bool) {
    resume_after(work, handed, true);
}

/// [`resume`], after a wait that `found` something to do. A wait for a stream that ended on its own
/// timeout found nothing: a thread that looks again and again at a silent stream, as the owner of a
/// host whose Claude works on a turn does, is no daemon moving, and never lets a wait for calm that
/// waits for something else last past its patience.
fn resume_after(work: &'static Work, handed: bool, found: bool) {
    let counted = HELD.try_with(|held| {
        if !held.counted.get() {
            return false;
        }
        held.idle.set(!handed && !found);
        if !handed {
            work.add_moving(found);
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

/// Blocks until no thread of the current virtual runtime works, the runtime's own thread sleeps with
/// no task woken for it, and `settled` holds, looking again each time [`working`] comes back to zero
/// or the runtime's thread goes to sleep, which is what notifies it: whatever makes `settled` hold
/// must be done by work these see. False once `patience` of real time passes with no unit of work
/// starting or ending (a daemon that moves, however slowly, is not stuck), or the runtime ended. Under the system clock it is `settled`.
pub fn until_idle(patience: Duration, settled: impl Fn() -> bool) -> bool {
    match Clock::current() {
        Clock::Virtual { work, .. } => work.until_idle(patience, settled),
        Clock::System => settled(),
    }
}

/// A frame was written to the host `host` (its instance): until the host has read it and done what
/// it says ([`frame_handled`]), it counts in [`working`], since what the host answers moves the
/// daemon.
pub fn frame_sent(host: &str) {
    Clock::current().frame_sent(host);
}

/// The host `host` read one frame written to it and did what it says.
pub fn frame_handled(host: &str) {
    Clock::current().frame_handled(host);
}

/// The host `host` reads nothing more of what was written to it: it ended, or stopped reading.
pub fn frames_dropped(host: &str) {
    Clock::current().frames_dropped(host);
}

/// The host `host` is gone for good, its process exited or ended: what was written to it and is
/// still unread is never handled, and neither is anything written to it from now on, whenever the
/// writer counted it.
pub fn host_gone(host: &str) {
    Clock::current().host_gone(host);
}

/// Whether the current virtual runtime's own thread sleeps with no task woken for it; true under the
/// system clock.
pub fn runtime_idle() -> bool {
    match Clock::current() {
        Clock::Virtual { work, .. } => work.runtime_idle(),
        Clock::System => true,
    }
}

/// The frames written to hosts that they have not read and acted on yet.
pub fn frames_in_transit() -> usize {
    Clock::current().frames_in_transit()
}

impl Clock {
    /// [`frame_sent`] on this clock, from a thread that may not have entered it.
    pub fn frame_sent(self, host: &str) {
        let Clock::Virtual { work, .. } = self else { return };
        let mut transit = work.transit();
        if transit.gone.contains(host) {
            return;
        }
        *transit.frames.entry(host.to_owned()).or_default() += 1;
        // Counted before the frame can be dropped: a host that goes drops it under the same lock.
        work.add();
        drop(transit);
        #[cfg(test)]
        tests::transit_released();
    }

    pub fn frame_handled(self, host: &str) {
        let Clock::Virtual { work, .. } = self else { return };
        let mut transit = work.transit();
        let Some(frames) = transit.frames.get_mut(host) else { return };
        *frames -= 1;
        if *frames == 0 {
            transit.frames.remove(host);
        }
        drop(transit);
        work.remove();
    }

    pub fn frames_dropped(self, host: &str) {
        let Clock::Virtual { work, .. } = self else { return };
        let frames = work.transit().frames.remove(host).unwrap_or(0);
        for _ in 0..frames {
            work.remove();
        }
    }

    pub fn host_gone(self, host: &str) {
        let Clock::Virtual { work, .. } = self else { return };
        let frames = {
            let mut transit = work.transit();
            transit.gone.insert(host.to_owned());
            transit.frames.remove(host).unwrap_or(0)
        };
        for _ in 0..frames {
            work.remove();
        }
    }

    pub fn frames_in_transit(self) -> usize {
        match self {
            Clock::Virtual { work, .. } => work.transit().frames.values().sum(),
            Clock::System => 0,
        }
    }
}

/// What wakes a thread that waits for a stream before the stream has anything to read, such as a
/// new input for the owner of a host that waits for the host. Ringing it counts the waiting thread
/// as working before it runs, as a notification of a [`Monitor`] does; a ring while nobody waits
/// ends the next wait at once.
pub struct Bell {
    ringing: Mutex<Ringing>,
    rope: std::os::unix::net::UnixStream,
    pub(crate) clapper: std::os::unix::net::UnixStream,
}

#[derive(Default)]
struct Ringing {
    /// The clock of the thread that waits for the bell now.
    waiting: Option<&'static Work>,
    /// A ring counted that thread as working.
    counted: bool,
}

impl Bell {
    pub fn new() -> std::io::Result<Bell> {
        let (rope, clapper) = std::os::unix::net::UnixStream::pair()?;
        rope.set_nonblocking(true)?;
        clapper.set_nonblocking(true)?;
        Ok(Bell { ringing: Mutex::default(), rope, clapper })
    }

    pub fn ring(&self) {
        use std::io::Write as _;
        let mut ringing = self.ringing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(work) = ringing.waiting
            && !std::mem::replace(&mut ringing.counted, true)
        {
            work.add();
        }
        // A full buffer already holds a ring that ends the wait.
        let _ = (&self.rope).write(&[1]);
    }

    /// The calling thread waits for the bell from now on: what [`Bell::ring`] counts is its.
    pub(crate) fn arm(&self) -> Option<&'static Work> {
        let work = match Clock::current() {
            Clock::Virtual { work, .. } => Some(work),
            Clock::System => None,
        };
        *self.ringing.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Ringing { waiting: work, counted: false };
        work
    }

    /// The wait ends: whether a ring counted the thread, which then holds that unit.
    pub(crate) fn disarm(&self) -> bool {
        std::mem::take(&mut *self.ringing.lock().unwrap_or_else(std::sync::PoisonError::into_inner)).counted
    }

    /// Empties the rings the wait saw.
    pub(crate) fn silence(&self) {
        use std::io::Read as _;
        let mut rings = [0u8; 64];
        while matches!((&self.clapper).read(&mut rings), Ok(read) if read > 0) {}
    }
}

/// The thread that waited for a stream and a bell goes on: it works again, holding the unit a ring
/// counted for it if one did. `found` says whether the stream or the bell ended the wait, and not
/// its timeout.
pub(crate) fn woke(work: Option<&'static Work>, rested: bool, counted: bool, found: bool) {
    let Some(work) = work else { return };
    if rested || counted {
        resume_after(work, counted, found);
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
pub struct StreamWait {
    work: Option<&'static Work>,
    found: bool,
}

pub fn stream_wait() -> StreamWait {
    match Clock::current() {
        Clock::Virtual { work, .. } => {
            rest();
            StreamWait { work: Some(work), found: true }
        }
        Clock::System => StreamWait { work: None, found: true },
    }
}

impl StreamWait {
    /// The wait ended on its own timeout, with nothing to read.
    pub fn timed_out(mut self) {
        self.found = false;
    }
}

impl Drop for StreamWait {
    fn drop(&mut self) {
        if let Some(work) = self.work {
            resume_after(work, false, self.found);
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
///
/// Every job of the daemon goes through here: a plain `tokio::task::spawn_blocking` is invisible to
/// [`working`], so a loaded Mac that starts its thread late lets the scenario runner declare the
/// daemon calm before the job read the clock or committed, and what it did lands at a later instant.
/// `clippy.toml` refuses the plain call. A job that must not count, because it waits on the clock
/// for a long time or is what the runner polls with, uses [`spawn_blocking_unwatched`].
#[track_caller]
pub fn spawn_blocking<F, T>(work: F) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let Clock::Virtual { work: counter, .. } = Clock::current() else { return spawn_blocking_unwatched(work) };
    let at = std::panic::Location::caller();
    *counter.jobs().entry(at).or_default() += 1;
    counter.add();
    let submitted = Submitted(Some((counter, at)));
    spawn_blocking_unwatched(move || {
        let _running = submitted.start();
        work()
    })
}

/// The unit [`spawn_blocking`] counted for a job, held until its thread starts it, or given back if
/// the job never starts.
struct Submitted(Option<(&'static Work, &'static std::panic::Location<'static>)>);

impl Submitted {
    /// The thread of the pool takes over the unit as a thread of its own does ([`started`]): the job
    /// rests while it waits on the clock or on a stream, and works again after.
    fn start(mut self) -> Running {
        let job = self.0.take();
        started(job.map(|(work, _)| work));
        Running(job)
    }
}

impl Drop for Submitted {
    fn drop(&mut self) {
        if let Some((work, at)) = self.0.take() {
            work.job_ended(at);
            work.remove();
        }
    }
}

/// A job that runs on a thread of the pool: when it ends, or panics, the thread gives back what it
/// held and counts for no later job.
struct Running(Option<(&'static Work, &'static std::panic::Location<'static>)>);

impl Drop for Running {
    fn drop(&mut self) {
        if let Some((work, at)) = self.0 {
            work.job_ended(at);
        }
        rest();
        let _ = HELD.try_with(|held| held.counted.set(false));
    }
}

/// Where each job of the blocking pool still running was submitted, with how many run from there.
pub fn running_jobs() -> Vec<(String, usize)> {
    let Clock::Virtual { work, .. } = Clock::current() else { return Vec::new() };
    work.jobs().iter().map(|(at, running)| (at.to_string(), *running)).collect()
}

/// Runs `wait`, a wait for something outside the daemon that no wait on the clock or on a stream
/// expresses (a gate the test that drives the clock opens), as a thread that does not hold virtual
/// time while it waits and works again once it ends. Under the system clock it is `wait`.
pub fn resting<T>(wait: impl FnOnce() -> T) -> T {
    let Clock::Virtual { work, .. } = Clock::current() else { return wait() };
    rest();
    let done = wait();
    resume(work, false);
    done
}

/// [`tokio::task::spawn_blocking`] without counting the job in [`working`]: for a job that waits on
/// the clock itself (a sleep, a timed wait for a stream, a loop that lasts a turn) or that is the one
/// watching [`working`], which counted would hold virtual time, or the watcher, for ever. Under a
/// virtual clock the runtime still does not jump while it runs.
pub fn spawn_blocking_unwatched<F, T>(work: F) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    #[expect(clippy::disallowed_methods, reason = "the one place that submits a job to the blocking pool")]
    tokio::task::spawn_blocking(work)
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
        let work: &'static Work = Box::leak(Box::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .on_thread_start(move || {
                if let Some(clock) = for_threads.get() {
                    std::mem::forget(clock.enter());
                }
            })
            .on_thread_park(|| work.parked())
            .on_thread_unpark(|| work.unparked())
            .build()?;
        let origin = runtime.block_on(async { Instant::now() });
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
