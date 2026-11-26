//! Whether macOS lets alondrad send to the local network. A launchd agent needs the Local Network
//! permission to send UDP to a LAN address (TN3179), which the direct iroh path over the same
//! Wi-Fi does; accepting the TCP of the direct listener does not need it. One datagram to the
//! router tells: macOS answers a denied send with "No route to host" at once, and the first
//! attempt is also what makes it ask the user.
//!
//! The answer changes while the daemon runs: the user allows it in the prompt or in System
//! Settings, and after an update that kept the launcher's path macOS denies the first sends for a
//! few seconds and then lets them through. So it is checked again after start, on every network
//! change the reachability watch sees, when Bonjour is refused, and whenever someone reads it.
use alondra_core::clock;
use alondra_core::clock::Instant;
use alondra_foundation::locks::Poisonable as _;
use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// After start, while it is not allowed yet.
pub const AFTER_START: [Duration; 4] = [Duration::ZERO, Duration::from_secs(3), Duration::from_secs(10), Duration::from_secs(30)];
/// A read older than this checks again.
pub const FRESH: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Tracker {
    found: &'static str,
    checked: Option<Instant>,
    denied_since: Option<Instant>,
}

impl Tracker {
    pub const fn new() -> Self {
        Self { found: "unknown", checked: None, denied_since: None }
    }

    /// Takes a new answer; true when it differs from the last one.
    pub fn record(&mut self, found: &'static str, now: Instant) -> bool {
        let changed = found != self.found;
        self.denied_since = match found {
            "denied" => self.denied_since.or(Some(now)),
            "allowed" => None,
            _ => self.denied_since,
        };
        self.found = found;
        self.checked = Some(now);
        changed
    }

    pub fn stale(&self, now: Instant) -> bool {
        self.checked.is_none_or(|checked| now.saturating_duration_since(checked) >= FRESH)
    }

    pub fn found(&self) -> &'static str {
        self.found
    }

    /// How long every check has said denied, with no allowed in between.
    pub fn denied_for(&self, now: Instant) -> Option<Duration> {
        self.denied_since.map(|since| now.saturating_duration_since(since))
    }
}

/// A holder that panics leaves it whole, so it is recovered: copied fields set one by one; a
/// panicking probe leaves them as they were.
static TRACKER: Mutex<Tracker> = Mutex::new(Tracker::new());
static WATCHING: AtomicBool = AtomicBool::new(false);
static PROBE: OnceLock<Probe> = OnceLock::new();

/// One try at the local network: `allowed`, `denied` or `unknown`, and why a send failed.
pub type Probe = fn() -> (&'static str, String);

fn tracker() -> std::sync::MutexGuard<'static, Tracker> {
    TRACKER.lock_or_recover("the local network status")
}

/// The last answer, without checking.
pub fn status() -> &'static str {
    tracker().found()
}

/// The answer as of now: checked again when the last check is older than `FRESH`, once the
/// daemon watches the permission.
pub fn current() -> &'static str {
    read(&mut tracker(), WATCHING.load(Ordering::SeqCst), clock::instant(), probe)
}

fn read(tracker: &mut Tracker, watching: bool, now: Instant, probe: impl FnOnce() -> (&'static str, String)) -> &'static str {
    if watching && tracker.stale(now) {
        let (found, failure) = probe();
        if tracker.record(found, now) {
            log_change(found, &failure);
        }
    }
    tracker.found()
}

pub fn denied_for() -> Option<Duration> {
    tracker().denied_for(clock::instant())
}

fn probe() -> (&'static str, String) {
    PROBE.get().map_or(("unknown", "the daemon does not watch the local network".into()), |probe| probe())
}

/// Checks now and records the answer; a change is logged.
pub fn check() -> &'static str {
    let (found, failure) = probe();
    if tracker().record(found, clock::instant()) {
        log_change(found, &failure);
    }
    found
}

fn log_change(found: &str, failure: &str) {
    match found {
        "allowed" => alondra_foundation::log!("local network: allowed"),
        "denied" => alondra_foundation::log!("local network: denied; allow Local Network for Alondra (alondrad) in System Settings, Privacy & Security"),
        _ => alondra_foundation::log!("local network: unknown ({failure})"),
    }
}

/// `check` off the async runtime.
pub(crate) async fn check_soon() -> &'static str {
    alondra_core::clock::spawn_blocking(check).await.unwrap_or("unknown")
}

/// Checks with `probe` at start and again on `AFTER_START` until it is allowed; from then on the
/// readers and the reachability watch keep it current. The probe is in place once this returns,
/// before the first check runs.
pub fn watch(probe: Probe) -> impl std::future::Future<Output = ()> {
    let _ = PROBE.set(probe);
    WATCHING.store(true, Ordering::SeqCst);
    async {
        let mut waited = Duration::ZERO;
        for at in AFTER_START {
            clock::sleep(at.saturating_sub(waited)).await;
            waited = at;
            if check_soon().await == "allowed" {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_later_allowed_replaces_the_denied_found_at_start() {
        let start = Instant::now();
        let mut tracker = Tracker::new();
        assert!(tracker.record("denied", start));
        assert_eq!(tracker.found(), "denied");
        assert!(tracker.record("allowed", start + Duration::from_secs(3)));
        assert_eq!(tracker.found(), "allowed");
        assert_eq!(tracker.denied_for(start + Duration::from_secs(60)), None);
    }

    #[test]
    fn a_read_checks_again_once_the_last_check_is_older_than_fresh() {
        let start = Instant::now();
        let mut tracker = Tracker::new();
        assert!(tracker.stale(start));
        tracker.record("denied", start);
        assert!(!tracker.stale(start + FRESH / 2));
        assert!(tracker.stale(start + FRESH));
    }

    #[test]
    fn reading_after_the_user_allows_it_reports_allowed_without_a_restart() {
        let start = Instant::now();
        let mut tracker = Tracker::new();
        tracker.record("denied", start);
        let found = read(&mut tracker, true, start + Duration::from_secs(3), || ("allowed", String::new()));
        assert_eq!(found, "allowed");
        let unchecked = read(&mut tracker, true, start + Duration::from_secs(4), || panic!("checked again within FRESH"));
        assert_eq!(unchecked, "allowed");
    }

    #[test]
    fn a_check_tries_the_network_with_the_probe_the_daemon_watches_with() {
        drop(watch(|| ("denied", "No route to host".into())));
        assert_eq!(check(), "denied");
        assert_eq!(status(), "denied");
    }

    #[test]
    fn nothing_is_sent_before_the_daemon_watches_the_permission() {
        let mut tracker = Tracker::new();
        assert_eq!(read(&mut tracker, false, Instant::now(), || panic!("sent while not watching")), "unknown");
    }

    #[test]
    fn denied_counts_from_the_first_denial_and_an_unknown_does_not_reset_it() {
        let start = Instant::now();
        let mut tracker = Tracker::new();
        tracker.record("denied", start);
        assert!(!tracker.record("denied", start + Duration::from_secs(10)));
        tracker.record("unknown", start + Duration::from_secs(20));
        assert_eq!(tracker.denied_for(start + Duration::from_secs(40)), Some(Duration::from_secs(40)));
        tracker.record("allowed", start + Duration::from_secs(41));
        tracker.record("denied", start + Duration::from_secs(50));
        assert_eq!(tracker.denied_for(start + Duration::from_secs(55)), Some(Duration::from_secs(5)));
    }

    #[test]
    fn checks_after_start_cover_the_seconds_macos_takes_to_let_an_updated_binary_through() {
        assert_eq!(AFTER_START.first(), Some(&Duration::ZERO));
        assert!(AFTER_START.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(AFTER_START.iter().any(|at| *at >= Duration::from_secs(3) && *at <= Duration::from_secs(10)));
    }
}
