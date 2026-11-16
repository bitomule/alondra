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
    let clock = runtime.block_on(async { Clock::current() });
    let read = std::thread::spawn(move || {
        let _entered = clock.enter();
        now_millis()
    })
    .join()
    .unwrap();
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
        let sending = spawn_thread(move || sender.send(7).unwrap());
        let message = recv_timeout(&receiver, HOUR);
        sending.join().unwrap();
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
