//! Under launchd, a daemon that cannot start is restarted every 10 seconds forever. Starts are
//! counted in HOME until one serves; from the third failure in a row on, a failed start waits
//! before exiting, and returns early once the binary is replaced so a fix is picked up at once.
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use alondra_core::startup;

static ACTIVE: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn begin(home: PathBuf, supervised: bool) {
    if !supervised { return; }
    match startup::begin(&home, env!("CARGO_PKG_VERSION"), alondra_core::now_millis()) {
        Ok(_) => *ACTIVE.lock().expect("start guard poisoned") = Some(home),
        Err(e) => log!("cannot record this start in {}: {e}", home.display()),
    }
}

pub fn served() {
    let Some(home) = ACTIVE.lock().expect("start guard poisoned").take() else { return };
    if let Err(e) = startup::served(&home) {
        log!("cannot clear {}: {e}", startup::path(&home).display());
    }
}

/// Records why this start failed; with a crash loop, how long to wait before exiting.
pub fn failed(error: &anyhow::Error) -> Option<(startup::Record, Duration)> {
    let home = ACTIVE.lock().expect("start guard poisoned").take()?;
    let record = match startup::failed(&home, &format!("{error:#}"), alondra_core::now_millis()) {
        Ok(record) => record,
        Err(e) => { log!("cannot record the failed start: {e}"); return None; }
    };
    let wait = startup::wait_before_exit(record.failures)?;
    Some((record, wait))
}

/// Sleeps out a crash-loop wait, returning early once `replaced` reports a new binary.
pub fn wait(total: Duration, step: Duration, mut replaced: impl FnMut() -> bool) -> bool {
    let mut waited = Duration::ZERO;
    while waited < total {
        if replaced() { return true; }
        let next = step.min(total - waited);
        std::thread::sleep(next);
        waited += next;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replaced_binary_ends_the_wait_early() {
        let mut checks = 0;
        let started = std::time::Instant::now();
        assert!(wait(Duration::from_secs(300), Duration::from_millis(1), || { checks += 1; checks == 3 }));
        assert_eq!(checks, 3);
        assert!(started.elapsed() < Duration::from_secs(60));
        let mut never = 0;
        assert!(!wait(Duration::from_millis(5), Duration::from_millis(1), || { never += 1; false }));
        assert!(never >= 5);
    }
}
