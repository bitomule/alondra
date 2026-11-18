//! Optional aggregate counts of the work views execute, scoped to one daemon client.
use std::sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}};

#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum Metric {
    GatherRequest, SamplerPeriodic, SamplerDomain, SamplerHooks, SamplerReports,
    FinalHit, SharedHit, Missing, Unwatched,
    GatherShared, GatherSampler, GatherKeyed,
    DigestSerializations, DigestBytes, SamplerSerializations, SamplerBytes,
    SamplerChanged, SamplerUnchanged, DeliveryWake, SharedWake,
    WaitingTaskRows, AgentRows, PullRequestRows,
    PrAccepted, PrRejected, PrVisibleChanged, PrVisibleUnchanged,
    WakeIssues, WakeMcpStarted, WakeMcpFinished, WakeMcpRemoved,
}
const NAMES: [&str; 31] = [
    "gather_request", "sampler_periodic", "sampler_domain", "sampler_hooks", "sampler_reports",
    "final_hit", "shared_hit", "missing", "unwatched",
    "gather_shared", "gather_sampler", "gather_keyed",
    "digest_serializations", "digest_bytes", "sampler_serializations", "sampler_bytes",
    "sampler_changed", "sampler_unchanged", "delivery_wake", "shared_wake",
    "waiting_task_rows", "agent_rows", "pull_request_rows",
    "pr_accepted", "pr_rejected", "pr_visible_changed", "pr_visible_unchanged",
    "wake_issues", "wake_mcp_started", "wake_mcp_finished", "wake_mcp_removed",
];
#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum Cause {
    Epoch, Revision, Domain, Hooks, Reports, Usage, Gate, Sampled,
    Store, PullRequests, Contexts, ReadError, ClosedWatch,
}
impl Cause {
    pub(crate) const fn bit(self) -> u16 { 1 << self as usize }
}
const CAUSES: [&str; 13] = [
    "epoch", "revision", "domain", "hooks", "reports", "usage", "gate", "sampled",
    "store", "pull_requests", "contexts", "read_error", "closed_watch",
];
#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum Stage { Lookup, Insert, Observe }
const STAGES: [&str; 3] = ["lookup", "insert", "observe"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Totals {
    pub metrics: [u64; NAMES.len()],
    pub invalid_events: [u64; STAGES.len()],
    pub causes: [[u64; CAUSES.len()]; STAGES.len()],
}

/// A disabled handle allocates nothing; clones never retain Client.
#[derive(Clone, Debug, Default)]
pub(crate) struct Observation(Option<Arc<Counts>>);
#[derive(Debug)]
struct Counts {
    active: AtomicBool,
    metrics: [AtomicU64; NAMES.len()],
    invalid_events: [AtomicU64; STAGES.len()],
    causes: [[AtomicU64; CAUSES.len()]; STAGES.len()],
}
impl Observation {
    pub(crate) fn armed() -> Self {
        Self(Some(Arc::new(Counts {
            active: AtomicBool::new(false),
            metrics: std::array::from_fn(|_| AtomicU64::new(0)),
            invalid_events: std::array::from_fn(|_| AtomicU64::new(0)),
            causes: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
        })))
    }
    pub(crate) fn start(&self) {
        if let Some(counts) = &self.0 { counts.active.store(true, Ordering::Relaxed); }
    }
    pub(crate) fn stop(&self) {
        if let Some(counts) = &self.0 { counts.active.store(false, Ordering::Relaxed); }
    }
    pub(crate) fn add(&self, metric: Metric, value: u64) {
        if let Some(counts) = &self.0 && counts.active.load(Ordering::Relaxed) {
            counts.metrics[metric as usize].fetch_add(value, Ordering::Relaxed);
        }
    }
    pub(crate) fn invalid(&self, stage: Stage, mask: u16) {
        let Some(counts) = &self.0 else { return };
        if mask == 0 || !counts.active.load(Ordering::Relaxed) { return; }
        let stage = stage as usize;
        counts.invalid_events[stage].fetch_add(1, Ordering::Relaxed);
        for cause in 0..CAUSES.len() {
            if mask & (1 << cause) != 0 {
                counts.causes[stage][cause].fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    pub(crate) fn totals(&self) -> Totals {
        let Some(counts) = &self.0 else {
            return Totals { metrics: [0; NAMES.len()], invalid_events: [0; STAGES.len()], causes: [[0; CAUSES.len()]; STAGES.len()] };
        };
        Totals {
            metrics: std::array::from_fn(|i| counts.metrics[i].load(Ordering::Relaxed)),
            invalid_events: std::array::from_fn(|i| counts.invalid_events[i].load(Ordering::Relaxed)),
            causes: std::array::from_fn(|i| std::array::from_fn(|j| counts.causes[i][j].load(Ordering::Relaxed))),
        }
    }
    fn summary(&self) -> String {
        let totals = self.totals();
        let mut fields = std::collections::BTreeMap::new();
        for (name, value) in NAMES.iter().zip(totals.metrics) { fields.insert((*name).to_owned(), value); }
        for (stage, name) in STAGES.iter().enumerate() {
            fields.insert(format!("{name}_invalid"), totals.invalid_events[stage]);
            for (cause, label) in CAUSES.iter().enumerate() {
                fields.insert(format!("{name}_{label}"), totals.causes[stage][cause]);
            }
        }
        serde_json::to_string(&fields).expect("aggregate view counts serialize")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Window { pub delay_seconds: u64, pub seconds: u64 }
pub(crate) fn window(seconds: Option<&str>, delay: Option<&str>) -> Option<Window> {
    let seconds = seconds?.parse::<u64>().ok().filter(|s| (1..=3600).contains(s))?;
    let delay_seconds = match delay {
        Some(value) => value.parse::<u64>().ok().filter(|s| *s <= 3600)?,
        None => 60,
    };
    Some(Window { delay_seconds, seconds })
}

/// Two aggregate records per explicit startup request; the observer creates no route or file.
pub(crate) fn start_window(client: &Arc<crate::client_routes::Client>, window: Window) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
    let weak = Arc::downgrade(client);
    runtime.spawn(async move {
        alondra_core::clock::sleep(std::time::Duration::from_secs(window.delay_seconds)).await;
        let Some(client) = weak.upgrade() else { return };
        let observation = client.view_observation.clone();
        drop(client);
        log!("view observation start seconds={} totals={}", window.seconds, observation.summary());
        observation.start();
        alondra_core::clock::sleep(std::time::Duration::from_secs(window.seconds)).await;
        observation.stop();
        log!("view observation end seconds={} totals={}", window.seconds, observation.summary());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_is_explicit_bounded_and_parsed_without_process_environment() {
        assert_eq!(window(None, None), None);
        assert_eq!(window(Some("300"), None), Some(Window { seconds: 300, delay_seconds: 60 }));
        assert_eq!(window(Some("1"), Some("0")), Some(Window { seconds: 1, delay_seconds: 0 }));
        for value in ["", "0", "3601", "-1", "invalid"] { assert_eq!(window(Some(value), None), None); }
        for value in ["", "3601", "-1", "invalid"] { assert_eq!(window(Some("300"), Some(value)), None); }
    }

    #[test]
    fn disabled_and_stopped_observers_do_not_count_and_instances_are_independent() {
        let disabled = Observation::default();
        disabled.start();
        disabled.add(Metric::GatherShared, 1);
        assert!(disabled.totals().metrics.iter().all(|value| *value == 0));
        let active = Observation::armed();
        active.add(Metric::GatherShared, 1);
        assert_eq!(active.totals().metrics[Metric::GatherShared as usize], 0);
        active.start();
        active.clone().add(Metric::GatherShared, 1);
        active.stop();
        active.add(Metric::GatherShared, 1);
        assert_eq!(active.totals().metrics[Metric::GatherShared as usize], 1);
        assert!(Observation::armed().totals().metrics.iter().all(|value| *value == 0));
    }

    #[test]
    fn an_invalidation_counts_one_event_and_every_cause() {
        let observation = Observation::armed();
        observation.start();
        observation.invalid(Stage::Lookup, Cause::Store.bit() | Cause::Reports.bit());
        let totals = observation.totals();
        assert_eq!(totals.invalid_events, [1, 0, 0]);
        assert_eq!(totals.causes[0][Cause::Store as usize], 1);
        assert_eq!(totals.causes[0][Cause::Reports as usize], 1);
        assert_eq!(totals.causes[0].iter().sum::<u64>(), 2);
        let summary: std::collections::BTreeMap<String, u64> = serde_json::from_str(&observation.summary()).unwrap();
        assert_eq!(summary["lookup_invalid"], 1);
        assert_eq!(summary["lookup_store"], 1);
    }
}
