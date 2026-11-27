use super::*;

const EPOCH: Millis = 1_700_000_000_000;
const HOUR: Duration = Duration::from_secs(3600);

#[test]
fn a_virtual_clock_starts_at_its_epoch_and_moves_only_when_advanced() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        assert_eq!(now_millis(), EPOCH);
        advance(2 * HOUR).await;
        assert_eq!(now_millis(), EPOCH + 2 * 3_600_000);
    });
}

#[test]
fn a_sleep_of_two_hours_ends_without_waiting_for_real_time() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    let started = std::time::Instant::now();
    runtime.block_on(async {
        sleep(2 * HOUR).await;
        assert_eq!(now_millis(), EPOCH + 2 * 3_600_000);
    });
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn an_interval_ticks_on_virtual_time() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let mut ticks = interval(Duration::from_secs(600));
        ticks.tick().await;
        let mut count = 0;
        while now_millis() < EPOCH + 2 * 3_600_000 {
            ticks.tick().await;
            count += 1;
        }
        assert_eq!(count, 12);
    });
}

#[test]
fn a_timeout_elapses_on_virtual_time() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let waited = timeout(HOUR, std::future::pending::<()>()).await;
        assert!(waited.is_err());
        assert_eq!(now_millis(), EPOCH + 3_600_000);
    });
}

#[test]
fn a_blocking_thread_of_the_runtime_reads_the_virtual_clock() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    let read = runtime.block_on(async {
        advance(HOUR).await;
        spawn_blocking_unwatched(now_millis).await.unwrap()
    });
    assert_eq!(read, EPOCH + 3_600_000);
}

#[test]
fn a_thread_of_its_own_reads_the_clock_it_enters() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    let read = std::thread::scope(|threads| {
        threads
            .spawn(|| {
                let _entered = runtime.enter();
                now_millis()
            })
            .join()
            .unwrap()
    });
    assert_eq!(read, EPOCH);
}

#[test]
fn the_system_clock_is_current_again_after_a_virtual_runtime_ends() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {});
    assert_eq!(Clock::current(), Clock::System);
    assert!(now_millis() > EPOCH);
}

fn on_its_own_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (done, finished) = tokio::sync::oneshot::channel();
        spawn_thread(move || done.send(work()).ok());
        finished.await.unwrap()
    })
}

#[test]
fn a_thread_it_starts_reads_the_virtual_clock_and_sleeps_on_it() {
    let (started, slept) = on_its_own_thread(|| {
        let started = (now_millis(), instant());
        blocking_sleep(2 * HOUR);
        (started, (now_millis(), since(started.1)))
    });
    assert_eq!(started.0, EPOCH);
    assert_eq!(slept, (EPOCH + 2 * 3_600_000, 2 * HOUR));
}

#[test]
fn the_threads_of_a_scope_read_the_virtual_clock() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    let read = runtime.block_on(async {
        advance(HOUR).await;
        scope(|threads| threads.spawn(now_millis).join().unwrap())
    });
    assert_eq!(read, EPOCH + 3_600_000);
}

#[test]
fn a_receiver_times_out_when_virtual_time_reaches_its_timeout() {
    let (waited, at) = on_its_own_thread(|| {
        let (_sender, receiver) = channel::<()>();
        (recv_timeout(&receiver, HOUR), now_millis())
    });
    assert_eq!(waited, Err(std::sync::mpsc::RecvTimeoutError::Timeout));
    assert_eq!(at, EPOCH + 3_600_000);
}

#[test]
fn a_message_ends_the_wait_at_once_and_a_gone_sender_ends_it_too() {
    let (message, gone, at) = on_its_own_thread(|| {
        let (sender, receiver) = channel();
        let sending = spawn_blocking_unwatched(move || sender.send(7).unwrap());
        let message = recv_timeout(&receiver, HOUR);
        tokio::runtime::Handle::current().block_on(sending).unwrap();
        (message, recv_timeout(&receiver, HOUR), now_millis())
    });
    assert_eq!(message, Ok(7));
    assert_eq!(gone, Err(std::sync::mpsc::RecvTimeoutError::Disconnected));
    assert_eq!(at, EPOCH);
}

#[test]
fn a_monitor_wait_ends_at_its_virtual_deadline() {
    let at = on_its_own_thread(|| {
        let monitor = Monitor::new(false);
        let deadline = instant() + HOUR;
        let mut ready = monitor.lock().unwrap();
        while !*ready && instant() < deadline {
            ready = monitor.wait(ready, deadline.saturating_duration_since(instant())).unwrap();
        }
        now_millis()
    });
    assert_eq!(at, EPOCH + 3_600_000);
}

#[test]
fn on_the_system_clock_the_waits_are_the_standard_ones() {
    let (sender, receiver) = channel();
    sender.send(1).unwrap();
    assert_eq!(recv_timeout(&receiver, Duration::ZERO), Ok(1));
    assert_eq!(recv_timeout(&receiver, Duration::from_millis(1)), Err(std::sync::mpsc::RecvTimeoutError::Timeout));
    drop(receiver);
    assert!(sender.send(2).is_err());
    assert_eq!(spawn_thread(Clock::current).join().unwrap(), Clock::System);
}

/// Yields to the runtime until `done`, for at most ten real seconds: what a thread does in real time
/// has no virtual instant to wait for.
async fn until(what: &str, done: impl Fn() -> bool) {
    let limit = std::time::Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(std::time::Instant::now() < limit, "{what}");
        tokio::task::yield_now().await;
    }
}

/// A thread the clock woke at the end of its wait counts as working before it runs, and until it
/// waits again or ends: virtual time that waits for it lets nothing it does land a second later.
#[test]
fn a_thread_the_clock_wakes_works_until_it_waits_again() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (go, gone) = std::sync::mpsc::channel::<()>();
        let monitor = std::sync::Arc::new(Monitor::new(()));
        let waiting = monitor.clone();
        let thread = spawn_thread(move || {
            drop(waiting.wait(waiting.lock().unwrap(), Duration::from_secs(1)).unwrap());
            let at = now_millis();
            gone.recv().ok();
            at
        });
        until("the started thread waits on the clock", || working() == 0).await;
        advance(Duration::from_secs(1)).await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        assert_eq!(working(), 1, "the timer counts the thread it wakes");
        go.send(()).unwrap();
        until("the thread ended", || thread.is_finished()).await;
        assert_eq!(thread.join().unwrap(), EPOCH + 1_000);
        assert_eq!(working(), 0);
    });
}

#[test]
fn a_notified_thread_works_until_it_waits_again() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (go, gone) = std::sync::mpsc::channel::<()>();
        let monitor = std::sync::Arc::new(Monitor::new(false));
        let waiting = monitor.clone();
        let thread = spawn_thread(move || {
            let mut ready = waiting.lock().unwrap();
            while !*ready {
                ready = waiting.wait(ready, HOUR).unwrap();
            }
            drop(ready);
            gone.recv().ok();
        });
        until("the started thread waits on the clock", || working() == 0).await;
        *monitor.lock().unwrap() = true;
        monitor.notify_all();
        assert_eq!(working(), 1, "the notification counts the thread it wakes");
        go.send(()).unwrap();
        until("the thread ended", || thread.is_finished()).await;
        assert_eq!(working(), 0);
        assert_eq!(now_millis(), EPOCH);
    });
}

#[test]
fn a_thread_the_clock_wakes_from_a_blocking_sleep_works_until_it_ends() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (go, gone) = std::sync::mpsc::channel::<()>();
        let thread = spawn_thread(move || {
            blocking_sleep(Duration::from_secs(1));
            gone.recv().ok();
        });
        until("the started thread sleeps on the clock", || working() == 0).await;
        advance(Duration::from_secs(1)).await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        assert_eq!(working(), 1);
        go.send(()).unwrap();
        until("the thread ended", || thread.is_finished()).await;
        assert_eq!(working(), 0);
    });
}

/// A job of the blocking pool counts from the moment it is submitted: a test that moves virtual time
/// waits for it, so what it reads and commits lands at the instant that started it.
#[test]
fn a_blocking_job_works_from_its_submission_until_it_ends() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (go, gone) = std::sync::mpsc::channel::<()>();
        let (reading, read) = std::sync::mpsc::channel::<()>();
        let job = spawn_blocking(move || {
            reading.send(()).unwrap();
            gone.recv().ok();
            now_millis()
        });
        assert_eq!(working(), 1, "it counts before its thread even starts");
        spawn_blocking_unwatched(move || read.recv().unwrap()).await.unwrap();
        advance(Duration::from_secs(1)).await;
        assert_eq!(working(), 1);
        go.send(()).unwrap();
        assert_eq!(job.await.unwrap(), EPOCH + 1_000);
        assert_eq!(working(), 0);
    });
}

#[test]
fn a_blocking_job_that_panics_gives_its_unit_back() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        assert!(spawn_blocking(|| panic!("a failed job")).await.unwrap_err().is_panic());
        assert_eq!(working(), 0);
    });
}

/// A wait for a stream ends when its writer writes, never on the clock: the thread does not hold
/// virtual time while it waits, and works again once it reads.
#[test]
fn a_thread_waiting_for_a_stream_holds_no_virtual_time() {
    use std::io::Write as _;
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let (go, gone) = std::sync::mpsc::channel::<()>();
        let thread = spawn_thread(move || {
            let read = crate::readable(&reader, HOUR).unwrap();
            gone.recv().ok();
            read
        });
        until("the started thread waits for its stream", || working() == 0).await;
        writer.write_all(b"x").unwrap();
        until("the thread read its stream", || working() == 1).await;
        go.send(()).unwrap();
        assert!(thread.join().unwrap());
        assert_eq!(working(), 0);
    });
}

/// A look at a stream that waits for nothing, because it asks for no time or finds the stream
/// ready, keeps the thread working: a host closing at the instant that asked for it, whatever the
/// look costs in real time, lands at that instant.
#[test]
fn a_look_at_a_stream_that_waits_for_nothing_keeps_the_thread_working() {
    use std::io::Write as _;
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let thread = spawn_thread(move || {
            let empty = crate::readable(&reader, Duration::ZERO).unwrap();
            writer.write_all(b"x").unwrap();
            let ready = crate::readable(&reader, HOUR).unwrap();
            (empty, ready, crate::poll::WORKING_AFTER_POLL.with(|seen| seen.take()))
        });
        let (empty, ready, working) = thread.join().unwrap();
        assert!(!empty && ready);
        assert_eq!(working, vec![1, 1], "the thread kept working through each look");
    });
}

#[test]
fn no_pending_deadline_inside_the_window_means_the_advance_wakes_nobody() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        assert!(!deadline_within(HOUR));
        let waiting = tokio::spawn(sleep(2 * HOUR));
        assert!(!deadline_within(HOUR));
        assert!(deadline_within(2 * HOUR));
        advance(2 * HOUR).await;
        waiting.await.unwrap();
        assert!(!deadline_within(HOUR));
    });
}

#[test]
fn every_kind_of_timed_wait_is_pending_from_the_moment_it_is_made() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let until = sleep_until(instant() + HOUR);
        assert!(deadline_within(HOUR));
        drop(until);
        let timed = timeout(HOUR, std::future::pending::<()>());
        assert!(deadline_within(HOUR));
        drop(timed);
        let timed_at = timeout_at(instant() + HOUR, std::future::pending::<()>());
        assert!(deadline_within(HOUR));
        drop(timed_at);
        assert!(!deadline_within(HOUR));
        let mut ticks = interval_at(instant() + HOUR, HOUR);
        assert!(deadline_within(HOUR));
        ticks.tick().await;
        assert!(!deadline_within(HOUR - Duration::from_secs(1)));
        assert!(deadline_within(HOUR));
    });
}

#[test]
fn a_finished_or_dropped_wait_is_forgotten() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        assert!(timeout(HOUR, std::future::ready(())).await.is_ok());
        assert!(!deadline_within(2 * HOUR));
        sleep(HOUR).await;
        assert!(!deadline_within(2 * HOUR));
        let first = sleep(HOUR);
        let second = sleep(HOUR);
        drop(first);
        assert!(deadline_within(HOUR));
        drop(second);
        assert!(!deadline_within(HOUR));
    });
}

#[test]
fn a_wait_on_another_thread_is_pending_before_the_advance() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    // The runtime keeps busy instead of awaiting the threads: idle, its paused clock would jump
    // to their deadlines on its own.
    runtime.block_on(async {
        let sleeper = spawn_thread(|| blocking_sleep(HOUR));
        while !deadline_within(HOUR) {
            tokio::task::yield_now().await;
        }
        advance(HOUR).await;
        spawn_blocking_unwatched(move || sleeper.join().unwrap()).await.unwrap();
        assert!(!deadline_within(2 * HOUR));

        let (_sender, receiver) = channel::<()>();
        let receiving = spawn_thread(move || recv_timeout(&receiver, HOUR));
        while !deadline_within(HOUR) {
            tokio::task::yield_now().await;
        }
        advance(HOUR).await;
        let received = spawn_blocking_unwatched(move || receiving.join().unwrap()).await.unwrap();
        assert_eq!(received, Err(std::sync::mpsc::RecvTimeoutError::Timeout));
        assert!(!deadline_within(2 * HOUR));
    });
}

#[test]
fn a_thread_of_the_blocking_pool_holds_no_virtual_time_after_its_wait() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let monitor = std::sync::Arc::new(Monitor::new(false));
        let waiting = monitor.clone();
        let task = spawn_blocking_unwatched(move || {
            let mut ready = waiting.lock().unwrap();
            while !*ready {
                ready = waiting.wait(ready, HOUR).unwrap();
            }
        });
        until("the blocking task waits", || !monitor.waiters.lock().unwrap().is_empty()).await;
        *monitor.lock().unwrap() = true;
        monitor.notify_all();
        task.await.unwrap();
        assert_eq!(working(), 0);
    });
}

#[test]
fn each_virtual_clock_has_its_own_deadlines_and_the_system_clock_none() {
    let first = VirtualRuntime::new(EPOCH).unwrap();
    let second = VirtualRuntime::new(EPOCH).unwrap();
    let pending = first.block_on(async { sleep(HOUR) });
    assert!(first.block_on(async { deadline_within(HOUR) }));
    assert!(!second.block_on(async { deadline_within(HOUR) }));
    assert_ne!(first.clock(), second.clock());
    assert!(!deadline_within(HOUR));
    drop(pending);
}

#[test]
fn work_in_real_time_counts_from_its_first_poll_until_it_ends() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (finish, finished) = tokio::sync::oneshot::channel::<()>();
        let waiting = tokio::spawn(real_time(async move { finished.await.unwrap() }));
        until("the work started", || working() == 1).await;
        finish.send(()).unwrap();
        waiting.await.unwrap();
        assert_eq!(working(), 0);
        let (_keep, dropped) = tokio::sync::oneshot::channel::<()>();
        let cancelled = tokio::spawn(real_time(async move { let _ = dropped.await; }));
        until("the second work started", || working() == 1).await;
        cancelled.abort();
        let _ = cancelled.await;
        assert_eq!(working(), 0, "a cancelled work gives its unit back");
    });
}

/// A job that waits on the clock for long, or that watches `working` itself, is submitted without
/// counting: counted, it would hold virtual time for as long as it waits.
#[test]
fn an_unwatched_blocking_job_does_not_count_as_work() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (go, gone) = std::sync::mpsc::channel::<()>();
        let (reading, read) = std::sync::mpsc::channel::<()>();
        let job = spawn_blocking_unwatched(move || {
            reading.send(()).unwrap();
            gone.recv().ok();
            working()
        });
        spawn_blocking_unwatched(move || read.recv().unwrap()).await.unwrap();
        assert_eq!(working(), 0);
        go.send(()).unwrap();
        assert_eq!(job.await.unwrap(), 0);
    });
}

/// A counted job that waits on the clock rests like a thread of its own: it holds no virtual time
/// while it sleeps, and the timer that wakes it counts it again before it runs.
#[test]
fn a_blocking_job_that_waits_on_the_clock_holds_no_virtual_time() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let job = spawn_blocking(|| {
            blocking_sleep(Duration::from_secs(5));
            now_millis()
        });
        assert_eq!(working(), 1, "it counts from its submission");
        until("the job sleeps", || working() == 0).await;
        advance(Duration::from_secs(5)).await;
        assert_eq!(job.await.unwrap(), EPOCH + 5_000);
        assert_eq!(working(), 0);
    });
}

/// A wait for a gate that is neither the clock nor a stream rests the job too, and the job works
/// again once the gate opens, until it ends.
#[test]
fn a_blocking_job_resting_for_a_gate_holds_no_virtual_time_and_works_after() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (open, opened) = std::sync::mpsc::channel::<()>();
        let (after, was_after) = std::sync::mpsc::channel::<()>();
        let (end, ended) = std::sync::mpsc::channel::<()>();
        let job = spawn_blocking(move || {
            resting(|| opened.recv().ok());
            after.send(()).unwrap();
            ended.recv().ok();
        });
        until("the job waits at its gate", || working() == 0).await;
        open.send(()).unwrap();
        spawn_blocking_unwatched(move || was_after.recv().unwrap()).await.unwrap();
        assert_eq!(working(), 1, "past its gate it works again");
        end.send(()).unwrap();
        job.await.unwrap();
        assert_eq!(working(), 0);
    });
}

/// The wait for calm is told each time the last working thread rests: it returns once nothing works
/// and its own condition holds, not before, and without looking in between.
#[test]
fn until_idle_returns_once_the_last_thread_rests_and_the_condition_holds() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (go, gone) = std::sync::mpsc::channel::<()>();
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let setting = done.clone();
        let thread = spawn_thread(move || {
            gone.recv().ok();
            setting.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let looks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (looking, watched) = (looks.clone(), done.clone());
        let waiting = spawn_blocking_unwatched(move || {
            until_idle(Duration::from_secs(10), || {
                looking.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                watched.load(std::sync::atomic::Ordering::SeqCst)
            })
        });
        std::thread::sleep(Duration::from_millis(50));
        assert!(!waiting.is_finished(), "a thread still works");
        assert_eq!(looks.load(std::sync::atomic::Ordering::SeqCst), 0, "nothing rested, so nothing looked");
        go.send(()).unwrap();
        assert!(waiting.await.unwrap());
        assert_eq!(looks.load(std::sync::atomic::Ordering::SeqCst), 1);
        thread.join().unwrap();
    });
}

#[test]
fn until_idle_gives_up_after_its_patience_while_a_thread_works() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (go, gone) = std::sync::mpsc::channel::<()>();
        let thread = spawn_thread(move || gone.recv().ok());
        let started = std::time::Instant::now();
        assert!(!spawn_blocking_unwatched(|| until_idle(Duration::from_millis(50), || true)).await.unwrap());
        assert!(started.elapsed() >= Duration::from_millis(50));
        go.send(()).unwrap();
        thread.join().unwrap();
    });
}

/// A frame written to a host counts as work until the host read it and did what it says, so a wait
/// for calm never sees the daemon idle while the host is about to answer.
#[test]
fn a_frame_works_from_its_writing_until_its_host_handled_it() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        frame_sent("host-a");
        frame_sent("host-a");
        frame_sent("host-b");
        assert_eq!((working(), frames_in_transit()), (3, 3));
        frame_handled("host-a");
        assert_eq!((working(), frames_in_transit()), (2, 2));
        frames_dropped("host-a");
        assert_eq!((working(), frames_in_transit()), (1, 1), "a host that ended reads nothing more");
        frame_handled("host-b");
        frame_handled("host-b");
        assert_eq!((working(), frames_in_transit()), (0, 0), "a frame nobody wrote is never handled");
    });
}

/// A host whose process is gone reads nothing more: what is unread stops counting, and so does a
/// frame its writer counts after it went, which the write into a dying process's socket can be.
#[test]
fn a_frame_to_a_host_that_is_gone_never_works() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        frame_sent("host-a");
        frame_sent("host-b");
        host_gone("host-a");
        assert_eq!((working(), frames_in_transit()), (1, 1));
        frame_sent("host-a");
        assert_eq!((working(), frames_in_transit()), (1, 1), "a frame written after it went never counts");
        frame_handled("host-a");
        assert_eq!((working(), frames_in_transit()), (1, 1), "nor does its late read take another host's count");
        frame_handled("host-b");
        assert_eq!((working(), frames_in_transit()), (0, 0));
    });
}

thread_local! {
    /// What runs on a thread the moment its `frame_sent` lets go of the frames in transit.
    static TRANSIT_RELEASED: std::cell::RefCell<Option<Box<dyn FnMut()>>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn transit_released() {
    let hook = TRANSIT_RELEASED.with(|hook| hook.borrow_mut().take());
    if let Some(mut hook) = hook {
        hook();
    }
}

/// The host goes the moment a frame written to it is in transit and the writer let go of what is
/// in transit: the work it drops with the frame was already counted, so the count never runs
/// below nothing and the rest it reaches wakes a wait for calm.
#[test]
fn a_host_gone_while_a_frame_to_it_is_being_written_drops_work_that_was_counted() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let seen = std::rc::Rc::new(Cell::new(None));
        let after = seen.clone();
        TRANSIT_RELEASED.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                host_gone("host");
                after.set(Some(working()));
            }))
        });
        frame_sent("host");
        assert_eq!(seen.get(), Some(0), "the host's frame was counted as work before it went");
        assert_eq!((working(), frames_in_transit()), (0, 0));
    });
}

#[test]
fn frames_count_nothing_on_the_system_clock() {
    frame_sent("host");
    assert_eq!((working(), frames_in_transit()), (0, 0));
    frame_handled("host");
}

/// A ring wakes a thread that waits for a stream with nothing to read, and counts it as working
/// before it runs: the wait for calm never sees an idle moment between the ring and the thread.
#[test]
fn a_ring_wakes_a_thread_waiting_for_a_stream_and_counts_it_before_it_runs() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (stream, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let bell = std::sync::Arc::new(Bell::new().unwrap());
        let (go, gone) = std::sync::mpsc::channel::<()>();
        let waiting = bell.clone();
        let thread = spawn_thread(move || {
            let readable = crate::readable_unless_rung(&stream, &waiting, HOUR).unwrap();
            gone.recv().ok();
            readable
        });
        until("the started thread waits for its stream", || working() == 0).await;
        bell.ring();
        assert_eq!(working(), 1, "the ring counts the thread it wakes");
        bell.ring();
        assert_eq!(working(), 1, "a second ring counts it once");
        go.send(()).unwrap();
        assert!(!thread.join().unwrap(), "the stream had nothing");
        assert_eq!(working(), 0);
    });
}

/// A ring while the thread does something else ends its next wait at once, counting nothing.
#[test]
fn a_ring_before_the_wait_ends_it_at_once() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (stream, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let bell = Bell::new().unwrap();
        bell.ring();
        assert_eq!(working(), 0);
        let started = std::time::Instant::now();
        assert!(!crate::readable_unless_rung(&stream, &bell, HOUR).unwrap());
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(working(), 0);
        let waited = std::time::Instant::now();
        assert!(!crate::readable_unless_rung(&stream, &bell, Duration::from_millis(30)).unwrap(), "the ring was used up");
        assert!(waited.elapsed() >= Duration::from_millis(30));
    });
}

#[test]
fn a_stream_with_something_to_read_wins_over_the_bell() {
    use std::io::Write as _;
    let (stream, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
    let bell = Bell::new().unwrap();
    writer.write_all(b"x").unwrap();
    bell.ring();
    assert!(crate::readable_unless_rung(&stream, &bell, HOUR).unwrap());
}

/// A job of the blocking pool that waits for a stream holds no virtual time while it waits, as a
/// thread of its own does, and works again once the stream has something.
#[test]
fn a_blocking_job_waiting_for_a_stream_rests() {
    use std::io::Write as _;
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let (go, gone) = std::sync::mpsc::channel::<()>();
        let job = spawn_blocking(move || {
            let read = crate::readable(&reader, HOUR).unwrap();
            gone.recv().ok();
            read
        });
        until("the job waits for its stream", || working() == 0).await;
        assert_eq!(running_jobs().len(), 1, "it still runs");
        writer.write_all(b"x").unwrap();
        until("the job read its stream", || working() == 1).await;
        go.send(()).unwrap();
        assert!(job.await.unwrap());
        assert_eq!(working(), 0);
        assert!(running_jobs().is_empty());
        let plain = spawn_blocking_unwatched(|| {
            let _waiting = stream_wait();
            working()
        });
        assert_eq!(plain.await.unwrap(), 0, "a plain job on the same pool counts nothing");
    });
}

/// A task of the runtime woken from another thread is work too: the wait for calm returns only once
/// the runtime's thread ran it and went back to sleep.
#[test]
fn until_idle_waits_for_a_task_woken_from_another_thread() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (wake, woken) = tokio::sync::oneshot::channel::<()>();
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let running = ran.clone();
        let task = tokio::spawn(async move {
            woken.await.ok();
            running.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let seen = ran.clone();
        let waiting = spawn_blocking_unwatched(move || {
            std::thread::spawn(move || wake.send(())).join().unwrap().unwrap();
            assert!(until_idle(Duration::from_secs(10), || true));
            seen.load(std::sync::atomic::Ordering::SeqCst)
        });
        assert!(waiting.await.unwrap(), "calm came before the woken task ran");
        task.await.unwrap();
    });
}

/// A wait for calm gives up after `patience` with no unit of work moving, and each unit that starts
/// or ends in the meantime moves its limit on, so a daemon slowed by a loaded Mac is waited for and
/// one that stopped is not.
#[test]
fn a_wait_for_calm_lasts_as_long_as_the_daemon_moves() {
    let start = std::time::Instant::now();
    let patience = Duration::from_secs(20);
    let mut wait = Patience::new(patience, start, 7);
    assert_eq!(wait.left(start + Duration::from_secs(5)), Duration::from_secs(15));
    assert!(!wait.extend(start + patience, 7), "nothing moved in the whole patience");
    assert!(wait.extend(start + patience, 9), "two units moved");
    assert_eq!(wait.left(start + patience), patience, "the limit moved on by a whole patience");
    assert!(!wait.extend(start + 2 * patience, 9), "and nothing moved after");
}

#[test]
fn every_unit_of_work_that_starts_or_ends_is_a_move() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let Clock::Virtual { work, .. } = Clock::current() else { unreachable!("a virtual runtime") };
        let before = work.moves.load(Ordering::SeqCst);
        frame_sent("host");
        frame_handled("host");
        assert_eq!(work.moves.load(Ordering::SeqCst) - before, 2);
    });
}

fn moves() -> u64 {
    let Clock::Virtual { work, .. } = Clock::current() else { unreachable!("a virtual runtime") };
    work.moves.load(Ordering::SeqCst)
}

/// A thread that looks again and again at a silent stream, each look ending on its own timeout,
/// moves nothing: the owner of a host whose Claude works on a turn does that every quarter of a
/// second, and must not keep a wait for calm that waits for something else from ever giving up.
#[test]
fn a_wait_for_a_stream_that_ends_on_its_timeout_moves_nothing() {
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (reader, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let bell = Bell::new().unwrap();
        let looked = spawn_thread(move || {
            assert!(!crate::readable(&reader, Duration::from_millis(1)).unwrap());
            let before = moves();
            for _ in 0..3 {
                assert!(!crate::readable(&reader, Duration::from_millis(1)).unwrap());
                assert!(!crate::readable_unless_rung(&reader, &bell, Duration::from_millis(1)).unwrap());
            }
            moves() - before
        });
        assert_eq!(looked.join().unwrap(), 0, "six looks that found nothing moved the daemon");
    });
}

/// A wait for a stream that its writer ends still moves the daemon, after any number of looks that
/// found nothing.
#[test]
fn a_wait_for_a_stream_its_writer_ends_moves_the_daemon() {
    use std::io::Write as _;
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let (go, gone) = std::sync::mpsc::channel::<()>();
        let looked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let looking = looked.clone();
        let thread = spawn_thread(move || {
            assert!(!crate::readable(&reader, Duration::from_millis(1)).unwrap());
            looking.store(true, Ordering::SeqCst);
            let read = crate::readable(&reader, HOUR).unwrap();
            gone.recv().ok();
            read
        });
        until("the thread waits for its stream after a look that found nothing", || looked.load(Ordering::SeqCst) && working() == 0).await;
        let before = moves();
        writer.write_all(b"x").unwrap();
        until("the write woke the thread", || moves() > before).await;
        assert_eq!(moves() - before, 1, "the write woke the thread, which works again");
        assert_eq!(working(), 1);
        go.send(()).unwrap();
        assert!(thread.join().unwrap());
    });
}

/// A wait for calm gives up after its patience while a thread does nothing but look at a silent
/// stream: before, each look that ended on its timeout counted as the daemon moving, and the wait
/// lasted as long as the looks did, for ever with a host whose turn never ends.
#[test]
fn a_wait_for_calm_gives_up_while_a_thread_only_looks_at_a_silent_stream() {
    use std::sync::atomic::AtomicBool;
    const LOOKS: usize = 20_000;
    let runtime = VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (reader, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let (gave_up, looks) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)));
        let (stop, counting) = (gave_up.clone(), looks.clone());
        let looking = spawn_thread(move || {
            while !stop.load(Ordering::SeqCst) && counting.load(Ordering::SeqCst) < LOOKS {
                assert!(!crate::readable(&reader, Duration::from_millis(1)).unwrap());
                counting.fetch_add(1, Ordering::SeqCst);
            }
        });
        until("the thread looked at its stream twice", || looks.load(Ordering::SeqCst) >= 2).await;
        let calm = spawn_blocking_unwatched(|| until_idle(Duration::from_millis(100), || false)).await.unwrap();
        gave_up.store(true, Ordering::SeqCst);
        looking.join().unwrap();
        assert!(!calm);
        let looked = looks.load(Ordering::SeqCst);
        assert!(looked < LOOKS, "the wait for calm lasted while the thread only looked at a silent stream, {looked} times");
    });
}
