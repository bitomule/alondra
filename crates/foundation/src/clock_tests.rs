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
        tokio::task::spawn_blocking(now_millis).await.unwrap()
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
        let sending = tokio::task::spawn_blocking(move || sender.send(7).unwrap());
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
        tokio::task::spawn_blocking(move || read.recv().unwrap()).await.unwrap();
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
        tokio::task::spawn_blocking(move || sleeper.join().unwrap()).await.unwrap();
        assert!(!deadline_within(2 * HOUR));

        let (_sender, receiver) = channel::<()>();
        let receiving = spawn_thread(move || recv_timeout(&receiver, HOUR));
        while !deadline_within(HOUR) {
            tokio::task::yield_now().await;
        }
        advance(HOUR).await;
        let received = tokio::task::spawn_blocking(move || receiving.join().unwrap()).await.unwrap();
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
        let task = tokio::task::spawn_blocking(move || {
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
