//! Optional, instance-local counts of signed HTTP middleware work. Declared clients are not identities.
use alondra_core::clock;
use axum::http::HeaderMap;
use std::sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}};
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

/// Off is a None: no counters, allocation, request classification or timer.
#[derive(Clone, Default)]
pub(super) struct Observation(Option<Arc<Counts>>);
struct Counts {
    active: AtomicBool,
    events: [[AtomicU64; METRICS.len()]; BUCKETS],
}

/// Counts are events, not transactions: a request crossing a boundary may contribute only some
/// stages, and a writer already past the active check can race the final snapshot.
pub(super) struct RequestObservation<'a> { counts: Option<&'a Counts>, bucket: usize }
impl RequestObservation<'_> {
    pub(super) fn add(&self, metric: Metric) {
        if let Some(counts) = self.counts && counts.active.load(Ordering::Relaxed) {
            counts.events[self.bucket][metric as usize].fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Observation {
    pub(super) fn armed() -> Self {
        Self(Some(Arc::new(Counts {
            active: AtomicBool::new(false),
            events: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
        })))
    }
    pub(super) fn start(&self) {
        if let Some(counts) = &self.0 { counts.active.store(true, Ordering::Relaxed); }
    }
    fn stop(&self) {
        if let Some(counts) = &self.0 { counts.active.store(false, Ordering::Relaxed); }
    }
    pub(super) fn request<'a>(&'a self, headers: &HeaderMap, path_and_query: &str, local: bool) -> RequestObservation<'a> {
        let Some(counts) = self.0.as_deref() else { return RequestObservation { counts: None, bucket: 0 } };
        if !counts.active.load(Ordering::Relaxed) { return RequestObservation { counts: None, bucket: 0 }; }
        RequestObservation { counts: Some(counts), bucket: classify(headers, path_and_query, local).index() }
    }
    #[cfg(test)]
    pub(super) fn total(&self, metric: Metric) -> u64 {
        self.0.as_ref().map_or(0, |counts| counts.events.iter().map(|row| row[metric as usize].load(Ordering::Relaxed)).sum())
    }
    fn summary(&self, window: Window, elapsed: Duration) -> String {
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("configured_seconds".to_owned(), window.seconds);
        fields.insert("elapsed_us".to_owned(), elapsed.as_micros().min(u64::MAX as u128) as u64);
        if let Some(counts) = &self.0 {
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
}

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

/// Opening synchronously outside a Tokio runtime disables observation. It is not started later.
fn configured_window(window: Option<Window>) -> Observation {
    let Some(window) = window else { return Observation::default() };
    let Ok(runtime) = tokio::runtime::Handle::try_current() else { return Observation::default() };
    let observation = Observation::armed();
    let weak = Arc::downgrade(observation.0.as_ref().expect("armed observation"));
    runtime.spawn(async move {
        clock::sleep(Duration::from_secs(window.delay_seconds)).await;
        let Some(counts) = weak.upgrade() else { return };
        let observation = Observation(Some(counts));
        log!("auth observation start totals={}", observation.summary(window, Duration::ZERO));
        let started = clock::instant();
        observation.start();
        drop(observation);
        clock::sleep(Duration::from_secs(window.seconds)).await;
        let Some(counts) = weak.upgrade() else { return };
        let observation = Observation(Some(counts));
        observation.stop();
        log!("auth observation end totals={}", observation.summary(window, clock::since(started)));
    });
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
        assert!(configured_window(window(Some("1"), Some("0"))).0.is_none(), "sync open has no runtime");
    }

    #[test]
    fn disabled_stopped_and_independent_instances_have_no_counts() {
        let headers = HeaderMap::new();
        let disabled = Observation::default();
        disabled.start();
        disabled.request(&headers, "/views/home?client=ios", true).add(Metric::Guard);
        assert!(disabled.0.is_none());
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
        let weak = Arc::downgrade(enabled.0.as_ref().unwrap());
        drop(enabled);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn one_window_starts_after_delay_stops_and_does_not_retain_the_owner() {
        clock::VirtualRuntime::new(0).unwrap().block_on(async {
            let observation = configured_window(Some(Window { delay_seconds: 10, seconds: 20 }));
            assert!(configured_window(None).0.is_none());
            let headers = HeaderMap::new();
            observation.request(&headers, "/runtime", true).add(Metric::Guard);
            assert_eq!(observation.total(Metric::Guard), 0);
            clock::sleep(Duration::from_secs(11)).await;
            observation.request(&headers, "/runtime", true).add(Metric::Guard);
            assert_eq!(observation.total(Metric::Guard), 1);
            clock::sleep(Duration::from_secs(21)).await;
            observation.request(&headers, "/runtime", true).add(Metric::Guard);
            assert_eq!(observation.total(Metric::Guard), 1);
            assert!(!observation.0.as_ref().unwrap().active.load(Ordering::Relaxed));

            let short_lived = configured_window(Some(Window { delay_seconds: 10, seconds: 20 }));
            let weak = Arc::downgrade(short_lived.0.as_ref().unwrap());
            drop(short_lived);
            assert!(weak.upgrade().is_none());
            clock::sleep(Duration::from_secs(11)).await;
            assert!(weak.upgrade().is_none());

            let active = configured_window(Some(Window { delay_seconds: 0, seconds: 20 }));
            clock::sleep(Duration::from_secs(1)).await;
            assert!(active.0.as_ref().unwrap().active.load(Ordering::Relaxed));
            let weak = Arc::downgrade(active.0.as_ref().unwrap());
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
