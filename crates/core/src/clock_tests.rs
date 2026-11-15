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
