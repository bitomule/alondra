//! Optional, instance-local counts of signed HTTP middleware work. Declared clients are not identities.
use alondra_core::clock;
use crate::locks::Poisonable as _;
use axum::http::HeaderMap;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU64, Ordering}};
use std::time::Duration;

#[derive(Clone, Copy)]
#[repr(usize)]
pub(super) enum Metric {
    Guard, SignatureAbsent, LocalBypass, BodyRejected, Attempts, KeyDecodes, KeyPreparations, Ecdsa, Verified, Rejected,
}
const METRICS: [&str; 10] = [
    "guard", "signature_absent", "local_bypass", "body_rejected", "attempts", "key_decodes", "key_preparations", "ecdsa", "verified", "rejected",
];
const ROUTES: [&str; 7] = ["changes", "views", "runtime", "usage", "mcp", "tasks", "other"];
const ORIGINS: [&str; 5] = ["unknown", "declared_ios", "declared_mac", "declared_tui", "declared_mcp"];
const LOCALS: [&str; 2] = ["nonlocal", "local"];
const BUCKETS: usize = ROUTES.len() * ORIGINS.len() * LOCALS.len();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Class { route: usize, origin: usize, local: bool }
impl Class {
    fn index(self) -> usize { (self.route * ORIGINS.len() + self.origin) * LOCALS.len() + usize::from(self.local) }
}

fn route(path: &str) -> usize {
    match path {
        "/changes" => 0,
        p if p.starts_with("/views/") => 1,
        "/runtime" => 2,
        "/usage" => 3,
        "/mcp" => 4,
        "/tasks" => 5,
        p if p.starts_with("/tasks/") => 5,
        _ => 6,
    }
}

// Compare URL form fields without allocating or retaining their text. Invalid escapes are refused.
fn decoded_eq(encoded: &str, expected: &[u8]) -> Option<bool> {
    let mut bytes = encoded.bytes();
    let mut at = 0;
    let mut matches = true;
    while let Some(byte) = bytes.next() {
        let byte = match byte {
            b'%' => {
                let high = (bytes.next()? as char).to_digit(16)?;
                let low = (bytes.next()? as char).to_digit(16)?;
                (high * 16 + low) as u8
            }
            b'+' => b' ',
            byte => byte,
        };
        matches &= expected.get(at) == Some(&byte);
        at += 1;
    }
    Some(matches && at == expected.len())
}

fn declared_client(query: Option<&str>) -> Option<usize> {
    let Some(query) = query else { return Some(0) };
    if query.len() > 4096 { return None; }
    let mut client = None;
    for field in query.split('&') {
        let (key, value) = field.split_once('=').unwrap_or((field, ""));
        let is_client = decoded_eq(key, b"client")?;
        // A malformed query is not evidence of a client, even if another field names one.
        decoded_eq(value, b"")?;
        if is_client {
            if client.is_some() { return None; }
            client = Some(if decoded_eq(value, b"ios")? { 1 }
                else if decoded_eq(value, b"mac")? { 2 }
                else if decoded_eq(value, b"tui")? { 3 }
                else { return None });
        }
    }
    Some(client.unwrap_or(0))
}

fn classify(headers: &HeaderMap, path_and_query: &str, local: bool) -> Class {
    let (path, query) = path_and_query.split_once('?').map_or((path_and_query, None), |(p, q)| (p, Some(q)));
    let route = route(path);
    let client = if route <= 1 { declared_client(query) } else { Some(0) };
    let mut mcp = headers.get_all("x-alondra-mcp-operation").iter();
    let origin = match mcp.next() {
        None => client.unwrap_or(0),
        Some(value) if mcp.next().is_none() && value.len() <= 256 && value.to_str().is_ok_and(|value| !value.is_empty())
            && client == Some(0) => 4,
        Some(_) => 0,
    };
    Class { route, origin, local }
}

/// Disabled requests only read `active`: no locks, classification or counter allocation.
#[derive(Clone, Default)]
pub(super) struct Observation(Arc<Control>);
#[derive(Default)]
struct Control {
    active: AtomicBool,
    // Every mutation leaves a complete counter set; recovering loses at most diagnostic events.
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    counts: Option<Arc<Counts>>,
    running: bool,
}
struct Counts {
    active: AtomicBool,
    events: [[AtomicU64; METRICS.len()]; BUCKETS],
}

/// Counts are events, not transactions: a request crossing a boundary may contribute only some
/// stages, and a writer already past the active check can race the final snapshot.
pub(super) struct RequestObservation { counts: Option<Arc<Counts>>, bucket: usize }
impl RequestObservation {
    pub(super) fn add(&self, metric: Metric) {
        if let Some(counts) = &self.counts && counts.active.load(Ordering::Relaxed) {
            counts.events[self.bucket][metric as usize].fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Observation {
    pub(super) fn running(&self) -> bool { self.0.state.lock_or_recover("the auth observation window").running }
    #[cfg(test)]
    pub(super) fn armed() -> Self {
        let observation = Self::default();
        observation.0.state.lock_or_recover("the auth observation window").counts = Some(Self::counts());
        observation
    }
    fn counts() -> Arc<Counts> {
        Arc::new(Counts {
            active: AtomicBool::new(false),
            events: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
        })
    }
    #[cfg(test)]
    pub(super) fn start(&self) {
        if let Some(counts) = &self.0.state.lock_or_recover("the auth observation window").counts {
            counts.active.store(true, Ordering::Relaxed);
            self.0.active.store(true, Ordering::Release);
        }
    }
    #[cfg(test)]
    fn stop(&self) {
        let mut state = self.0.state.lock_or_recover("the auth observation window");
        self.0.active.store(false, Ordering::Release);
        if let Some(counts) = &state.counts { counts.active.store(false, Ordering::Relaxed); }
        state.running = false;
    }
    pub(super) fn request(&self, headers: &HeaderMap, path_and_query: &str, local: bool) -> RequestObservation {
        if !self.0.active.load(Ordering::Acquire) { return RequestObservation { counts: None, bucket: 0 }; }
        let counts = self.0.state.lock_or_recover("the auth observation window").counts.clone();
        if !counts.as_ref().is_some_and(|counts| counts.active.load(Ordering::Relaxed)) {
            return RequestObservation { counts: None, bucket: 0 };
        }
        RequestObservation { counts, bucket: classify(headers, path_and_query, local).index() }
    }
    #[cfg(test)]
    pub(super) fn total(&self, metric: Metric) -> u64 {
        self.0.state.lock_or_recover("the auth observation window").counts.as_ref().map_or(0, |counts| counts.events.iter().map(|row| row[metric as usize].load(Ordering::Relaxed)).sum())
    }
    #[cfg(test)]
    fn summary(&self, window: Window, elapsed: Duration) -> String {
        Self::summarize(self.0.state.lock_or_recover("the auth observation window").counts.as_deref(), window, elapsed)
    }
    fn summarize(counts: Option<&Counts>, window: Window, elapsed: Duration) -> String {
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("configured_seconds".to_owned(), window.seconds);
        fields.insert("elapsed_us".to_owned(), elapsed.as_micros().min(u64::MAX as u128) as u64);
        if let Some(counts) = counts {
            for (route, route_name) in ROUTES.iter().enumerate() {
                for (origin, origin_name) in ORIGINS.iter().enumerate() {
                    for (local, local_name) in LOCALS.iter().enumerate() {
                        let bucket = Class { route, origin, local: local == 1 }.index();
                        for (metric, metric_name) in METRICS.iter().enumerate() {
                            let value = counts.events[bucket][metric].load(Ordering::Relaxed);
                            if value != 0 { fields.insert(format!("{route_name}_{origin_name}_{local_name}_{metric_name}"), value); }
                        }
                    }
                }
            }
        }
        serde_json::to_string(&fields).expect("aggregate auth counts serialize")
    }

    pub(super) fn start_window(&self, seconds: u64, delay_seconds: u64) -> Result<(), ObservationError> {
        if !(1..=3600).contains(&seconds) || delay_seconds > 3600 { return Err(ObservationError::InvalidWindow); }
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| ObservationError::NoRuntime)?;
        let starts = clock::instant() + Duration::from_secs(delay_seconds);
        let mut state = self.0.state.lock_or_recover("the auth observation window");
        if state.running { return Err(ObservationError::AlreadyRunning); }
        let counts = Self::counts();
        let weak_counts = Arc::downgrade(&counts);
        state.counts = Some(counts.clone());
        state.running = true;
        let weak = Arc::downgrade(&self.0);
        let window = Window { seconds, delay_seconds };
        let started = if delay_seconds == 0 {
            log!("auth observation start totals={}", Self::summarize(Some(&counts), window, Duration::ZERO));
            let started = clock::instant();
            counts.active.store(true, Ordering::Relaxed);
            self.0.active.store(true, Ordering::Release);
            Some(started)
        } else { None };
        runtime.spawn(async move {
            let started = if let Some(started) = started { started } else {
                clock::sleep_until(starts).await;
                let (Some(control), Some(counts)) = (weak.upgrade(), weak_counts.upgrade()) else { return };
                let state = control.state.lock_or_recover("the auth observation window");
                if !state.running || !state.counts.as_ref().is_some_and(|current| Arc::ptr_eq(current, &counts)) { return; }
                log!("auth observation start totals={}", Self::summarize(Some(&counts), window, Duration::ZERO));
                let started = clock::instant();
                counts.active.store(true, Ordering::Relaxed);
                control.active.store(true, Ordering::Release);
                started
            };
            clock::sleep_until(starts + Duration::from_secs(seconds)).await;
            let (Some(control), Some(counts)) = (weak.upgrade(), weak_counts.upgrade()) else { return };
            let mut state = control.state.lock_or_recover("the auth observation window");
            if !state.running || !state.counts.as_ref().is_some_and(|current| Arc::ptr_eq(current, &counts)) { return; }
            control.active.store(false, Ordering::Release);
            counts.active.store(false, Ordering::Relaxed);
            state.running = false;
            log!("auth observation end totals={}", Self::summarize(Some(&counts), window, clock::since(started)));
        });
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationError { InvalidWindow, AlreadyRunning, NoRuntime }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Window { delay_seconds: u64, seconds: u64 }
fn window(seconds: Option<&str>, delay: Option<&str>) -> Option<Window> {
    let seconds = seconds?.parse::<u64>().ok().filter(|s| (1..=3600).contains(s))?;
    let delay_seconds = match delay {
        Some(value) => value.parse::<u64>().ok().filter(|s| *s <= 3600)?,
        None => 60,
    };
    Some(Window { delay_seconds, seconds })
}

/// Startup configuration outside a Tokio runtime stays off; an explicit hot request can arm it later.
fn configured_window(window: Option<Window>) -> Observation {
    let observation = Observation::default();
    if let Some(window) = window { let _ = observation.start_window(window.seconds, window.delay_seconds); }
    observation
}

#[cfg(not(test))]
pub(super) fn configured() -> Observation {
    configured_window(window(std::env::var("ALONDRA_AUTH_OBSERVE_SECONDS").ok().as_deref(),
        std::env::var("ALONDRA_AUTH_OBSERVE_DELAY_SECONDS").ok().as_deref()))
}
#[cfg(test)]
pub(super) fn configured() -> Observation { Observation::default() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_is_explicit_and_bounded_without_process_environment() {
        assert_eq!(window(None, None), None);
        assert_eq!(window(Some("300"), None), Some(Window { seconds: 300, delay_seconds: 60 }));
        assert_eq!(window(Some("1"), Some("0")), Some(Window { seconds: 1, delay_seconds: 0 }));
        assert_eq!(window(Some("3600"), Some("3600")), Some(Window { seconds: 3600, delay_seconds: 3600 }));
        for value in ["", "0", "3601", "-1", "invalid"] { assert_eq!(window(Some(value), None), None); }
        for value in ["", "3601", "-1", "invalid"] { assert_eq!(window(Some("300"), Some(value)), None); }
        assert!(configured_window(window(Some("1"), Some("0"))).0.state.lock_or_recover("the auth observation window").counts.is_none(), "sync open has no runtime");
    }

    #[test]
    fn disabled_stopped_and_independent_instances_have_no_counts() {
        let headers = HeaderMap::new();
        let disabled = Observation::default();
        let locked = disabled.0.state.lock_or_recover("the auth observation window");
        let ignored = disabled.request(&headers, "/changes?client=%invalid", true);
        assert!(ignored.counts.is_none(), "disabled requests do not lock or classify");
        drop(locked);
        disabled.start();
        disabled.request(&headers, "/views/home?client=ios", true).add(Metric::Guard);
        assert!(disabled.0.state.lock_or_recover("the auth observation window").counts.is_none());
        assert_eq!(disabled.total(Metric::Guard), 0);
        let enabled = Observation::armed();
        enabled.request(&headers, "/runtime", false).add(Metric::Guard);
        assert_eq!(enabled.total(Metric::Guard), 0);
        enabled.start();
        enabled.clone().request(&headers, "/runtime", false).add(Metric::Guard);
        enabled.stop();
        enabled.request(&headers, "/runtime", false).add(Metric::Guard);
        assert_eq!(enabled.total(Metric::Guard), 1);
        assert_eq!(Observation::armed().total(Metric::Guard), 0);
        let weak = Arc::downgrade(&enabled.0);
        drop(enabled);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn hot_windows_validate_overlap_rearm_and_isolate_old_request_handles() {
        let observation = Observation::default();
        assert_eq!(observation.start_window(0, 0), Err(ObservationError::InvalidWindow));
        assert_eq!(observation.start_window(3601, 0), Err(ObservationError::InvalidWindow));
        assert_eq!(observation.start_window(1, 3601), Err(ObservationError::InvalidWindow));
        assert_eq!(observation.start_window(1, 0), Err(ObservationError::NoRuntime));
        assert!(!observation.running());
        clock::VirtualRuntime::new(0).unwrap().block_on(async {
            let headers = HeaderMap::new();
            observation.start_window(20, 10).unwrap();
            assert!(observation.running(), "delay reserves the window");
            assert_eq!(observation.start_window(20, 0), Err(ObservationError::AlreadyRunning));
            assert!(observation.request(&headers, "/changes", true).counts.is_none());
            clock::sleep(Duration::from_secs(11)).await;
            let old_request = observation.request(&headers, "/changes", true);
            old_request.add(Metric::Guard);
            assert_eq!(observation.total(Metric::Guard), 1);
            assert_eq!(observation.start_window(20, 0), Err(ObservationError::AlreadyRunning));
            clock::sleep(Duration::from_secs(21)).await;
            assert!(!observation.running());
            old_request.add(Metric::Guard);
            assert_eq!(observation.total(Metric::Guard), 1);
            observation.start_window(20, 0).unwrap();
            assert_eq!(observation.total(Metric::Guard), 0);
            assert!(observation.0.active.load(Ordering::Acquire), "zero delay activates before returning");
            old_request.add(Metric::Guard);
            assert_eq!(observation.total(Metric::Guard), 0, "an earlier request cannot contaminate a new window");
            observation.request(&headers, "/changes", true).add(Metric::Guard);
            assert_eq!(observation.total(Metric::Guard), 1);
            clock::sleep(Duration::from_secs(21)).await;
            assert!(!observation.running());
        });
    }

    #[test]
    fn stale_timer_cannot_stop_a_replacement_window() {
        clock::VirtualRuntime::new(0).unwrap().block_on(async {
            let observation = Observation::default();
            observation.start_window(10, 0).unwrap();
            clock::sleep(Duration::from_secs(1)).await;
            observation.stop();
            observation.start_window(30, 0).unwrap();
            clock::sleep(Duration::from_secs(11)).await;
            assert!(observation.running());
            observation.request(&HeaderMap::new(), "/runtime", true).add(Metric::Guard);
            assert_eq!(observation.total(Metric::Guard), 1);
            clock::sleep(Duration::from_secs(21)).await;
            assert!(!observation.running());
        });
    }

    #[test]
    fn concurrent_start_reserves_exactly_one_window() {
        clock::VirtualRuntime::new(0).unwrap().block_on(async {
            let observation = Observation::default();
            let runtime = tokio::runtime::Handle::current();
            let barrier = std::sync::Barrier::new(4);
            let results = std::thread::scope(|scope| {
                let threads = (0..4).map(|_| scope.spawn(|| {
                    let _entered = runtime.enter();
                    barrier.wait();
                    observation.start_window(20, 10)
                })).collect::<Vec<_>>();
                threads.into_iter().map(|thread| thread.join().unwrap()).collect::<Vec<_>>()
            });
            assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
            assert_eq!(results.iter().filter(|result| **result == Err(ObservationError::AlreadyRunning)).count(), 3);
        });
    }

    #[test]
    fn one_window_starts_after_delay_stops_and_does_not_retain_the_owner() {
        clock::VirtualRuntime::new(0).unwrap().block_on(async {
            let observation = configured_window(Some(Window { delay_seconds: 10, seconds: 20 }));
            assert!(configured_window(None).0.state.lock_or_recover("the auth observation window").counts.is_none());
            let headers = HeaderMap::new();
            observation.request(&headers, "/runtime", true).add(Metric::Guard);
            assert_eq!(observation.total(Metric::Guard), 0);
            clock::sleep(Duration::from_secs(11)).await;
            observation.request(&headers, "/runtime", true).add(Metric::Guard);
            assert_eq!(observation.total(Metric::Guard), 1);
            clock::sleep(Duration::from_secs(21)).await;
            observation.request(&headers, "/runtime", true).add(Metric::Guard);
            assert_eq!(observation.total(Metric::Guard), 1);
            assert!(!observation.0.active.load(Ordering::Relaxed));

            let short_lived = configured_window(Some(Window { delay_seconds: 10, seconds: 20 }));
            let weak = Arc::downgrade(&short_lived.0);
            drop(short_lived);
            assert!(weak.upgrade().is_none());
            clock::sleep(Duration::from_secs(11)).await;
            assert!(weak.upgrade().is_none());

            let active = configured_window(Some(Window { delay_seconds: 0, seconds: 20 }));
            clock::sleep(Duration::from_secs(1)).await;
            assert!(active.0.active.load(Ordering::Relaxed));
            let weak = Arc::downgrade(&active.0);
            drop(active);
            assert!(weak.upgrade().is_none(), "the active timer must not retain counters");
            clock::sleep(Duration::from_secs(21)).await;
            assert!(weak.upgrade().is_none());
        });
    }

    #[test]
    fn concurrent_writers_count_events_without_losing_updates() {
        let observation = Observation::armed();
        observation.start();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    let headers = HeaderMap::new();
                    let request = observation.request(&headers, "/views/home?client=tui", true);
                    for _ in 0..64 { request.add(Metric::Guard); }
                });
            }
        });
        observation.stop();
        assert_eq!(observation.total(Metric::Guard), 256);
    }

    #[test]
    fn declarations_are_bounded_untrusted_and_ambiguous_values_are_unknown() {
        let headers = HeaderMap::new();
        for (path, route, origin) in [
            ("/views/task:synthetic?client=ios", 1, 1), ("/changes?client=mac", 0, 2),
            ("/views/home?%63lient=%74%75%69", 1, 3), ("/runtime?client=ios", 2, 0),
            ("/usage", 3, 0), ("/mcp", 4, 0), ("/tasks/synthetic", 5, 0), ("/unknown?client=tui", 6, 0),
            ("/changes?client=ios&%63lient=ios", 0, 0), ("/changes?client=ios&client=mac", 0, 0),
            ("/changes?client=ios&broken=%xy", 0, 0), ("/changes?client=IOS", 0, 0),
            ("/changes?client=ios+", 0, 0), ("/changes?client", 0, 0),
        ] {
            assert_eq!(classify(&headers, path, true), Class { route, origin, local: true });
        }
        let mut headers = HeaderMap::new();
        headers.insert("x-alondra-mcp-operation", "synthetic-operation".parse().unwrap());
        assert_eq!(classify(&headers, "/mcp", false).origin, 4);
        assert_eq!(classify(&headers, "/views/home?client=ios", false).origin, 0);
        for query in ["client=web", "client=", "client", "client=mac&client=tui"] {
            assert_eq!(classify(&headers, &format!("/changes?{query}"), false).origin, 0);
        }
        headers.append("x-alondra-mcp-operation", "duplicate".parse().unwrap());
        assert_eq!(classify(&headers, "/mcp", false).origin, 0);
        headers.remove("x-alondra-mcp-operation");
        headers.insert("x-alondra-mcp-operation", axum::http::HeaderValue::from_bytes(&[0x80]).unwrap());
        assert_eq!(classify(&headers, "/mcp", false).origin, 0);
        assert_eq!(declared_client(Some(&"x".repeat(4097))), None);
    }

    #[test]
    fn output_contains_only_fixed_names_and_numbers_and_actual_elapsed() {
        let observation = Observation::armed();
        observation.start();
        let mut headers = HeaderMap::new();
        headers.insert("x-alondra-device", "private-device-sentinel".parse().unwrap());
        headers.insert("x-alondra-mcp-operation", "private-operation-sentinel".parse().unwrap());
        observation.request(&headers, "/tasks/private-task-sentinel?secret=private-query-sentinel", true).add(Metric::Guard);
        let output = observation.summary(Window { seconds: 120, delay_seconds: 60 }, Duration::from_millis(120_500));
        let values: std::collections::BTreeMap<String, u64> = serde_json::from_str(&output).unwrap();
        assert_eq!(values.len(), 3);
        assert_eq!(values["configured_seconds"], 120);
        assert_eq!(values["elapsed_us"], 120_500_000);
        assert_eq!(values["tasks_declared_mcp_local_guard"], 1);
        for forbidden in ["private", "sentinel", "/tasks", "secret", "x-alondra"] { assert!(!output.contains(forbidden)); }
    }
}
