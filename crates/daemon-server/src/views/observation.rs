//! Optional aggregate counts of the work views execute, scoped to one daemon client.
use std::sync::{Arc, Mutex, Weak, atomic::{AtomicBool, AtomicU64, Ordering}};
use std::time::Duration;
use alondra_core::clock;
use crate::locks::Poisonable as _;

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
    FeedRequests, FeedChanged, FeedTimeout, FeedError,
    FeedSupersededTui, FeedSupersededIos, FeedSupersededMac,
    DigestNodesBytes, DigestPullRequestsBytes, DigestActivityBytes, DigestTaskHistoryBytes,
    DigestTaskMapsBytes, DigestOtherBytes, DigestStructuralBytes,
    SamplerNodesBytes, SamplerPullRequestsBytes, SamplerActivityBytes, SamplerTaskHistoryBytes,
    SamplerTaskMapsBytes, SamplerOtherBytes, SamplerStructuralBytes,
    NodeOutputRows,
    ComponentComparisons, ComponentRowsCompared, Materializations, MaterializedNodeRows,
    HashBytes, HashOperations, ComponentSerializations,
    NodeTreeVisits, NodeTreeCopies, NodeLeafComparisons, NodeLeafSerializations, NodeMaterializedRows, NodeFullBuilds, NodeIncrementalBuilds, NodeSourceRaces, NodeExternalRows, NodeStoreRowsCompared, NodeStoreRowsRead,
}
const NAMES: [&str; 71] = [
    "gather_request", "sampler_periodic", "sampler_domain", "sampler_hooks", "sampler_reports",
    "final_hit", "shared_hit", "missing", "unwatched",
    "gather_shared", "gather_sampler", "gather_keyed",
    "digest_serializations", "digest_bytes", "sampler_serializations", "sampler_bytes",
    "sampler_changed", "sampler_unchanged", "delivery_wake", "shared_wake",
    "waiting_task_rows", "agent_rows", "pull_request_rows",
    "pr_accepted", "pr_rejected", "pr_visible_changed", "pr_visible_unchanged",
    "wake_issues", "wake_mcp_started", "wake_mcp_finished", "wake_mcp_removed",
    "feed_requests", "feed_changed", "feed_timeout", "feed_error",
    "feed_superseded_tui", "feed_superseded_ios", "feed_superseded_mac",
    "digest_nodes_bytes", "digest_pull_requests_bytes", "digest_activity_bytes", "digest_task_history_bytes",
    "digest_task_maps_bytes", "digest_other_bytes", "digest_structural_bytes",
    "sampler_nodes_bytes", "sampler_pull_requests_bytes", "sampler_activity_bytes", "sampler_task_history_bytes",
    "sampler_task_maps_bytes", "sampler_other_bytes", "sampler_structural_bytes",
    "node_output_rows",
    "component_comparisons", "component_rows_compared", "materializations", "materialized_node_rows",
    "hash_bytes", "hash_operations", "component_serializations",
    "node_tree_visits", "node_tree_copies", "node_leaf_comparisons", "node_leaf_serializations", "node_materialized_rows", "node_full_builds", "node_incremental_builds", "node_source_races", "node_external_rows", "node_store_rows_compared", "node_store_rows_read",
];

#[derive(Clone, Copy)]
pub(crate) enum Serialization { Digest, Sampler }
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

/// Clones share control without retaining Client. Off only reads one atomic flag.
#[derive(Clone, Debug, Default)]
pub(crate) struct Observation(Arc<Control>);
#[derive(Debug, Default)]
struct Control {
    active: AtomicBool,
    window: Mutex<WindowState>,
}
#[derive(Debug, Default)]
struct WindowState {
    counts: Option<Arc<Counts>>,
    running: bool,
    started: Option<clock::Instant>,
}
#[derive(Debug)]
struct Counts {
    active: AtomicBool,
    elapsed_us: AtomicU64,
    metrics: [AtomicU64; NAMES.len()],
    invalid_events: [AtomicU64; STAGES.len()],
    causes: [[AtomicU64; CAUSES.len()]; STAGES.len()],
}
impl Default for Counts {
    fn default() -> Self {
        Self {
            active: AtomicBool::new(false),
            elapsed_us: AtomicU64::new(0),
            metrics: std::array::from_fn(|_| AtomicU64::new(0)),
            invalid_events: std::array::from_fn(|_| AtomicU64::new(0)),
            causes: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartError { InvalidWindow, AlreadyRunning, RuntimeUnavailable }

impl Observation {
    #[cfg(test)]
    pub(crate) fn armed() -> Self {
        let observation = Self::default();
        observation.0.window.lock_or_recover("the view observation window").counts = Some(Arc::default());
        observation
    }
    #[cfg(test)]
    pub(crate) fn start(&self) {
        let mut window = self.0.window.lock_or_recover("the view observation window");
        if let Some(counts) = &window.counts {
            counts.active.store(true, Ordering::Relaxed);
            self.0.active.store(true, Ordering::Release);
            window.running = true;
        }
    }
    #[cfg(test)]
    pub(crate) fn stop(&self) {
        let mut window = self.0.window.lock_or_recover("the view observation window");
        self.0.active.store(false, Ordering::Release);
        if let Some(counts) = &window.counts { counts.active.store(false, Ordering::Relaxed); }
        window.running = false;
    }
    pub(crate) fn is_running(&self) -> bool {
        self.0.window.lock_or_recover("the view observation window").running
    }
    pub(crate) fn start_window(&self, seconds: u64, delay_seconds: u64) -> Result<(), StartError> {
        if !(1..=3600).contains(&seconds) || delay_seconds > 3600 { return Err(StartError::InvalidWindow); }
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| StartError::RuntimeUnavailable)?;
        let counts = Arc::new(Counts::default());
        {
            let mut window = self.0.window.lock_or_recover("the view observation window");
            if window.running { return Err(StartError::AlreadyRunning); }
            window.counts = Some(counts.clone());
            window.running = true;
            window.started = None;
            if delay_seconds == 0 {
                log!("view observation start seconds={} totals={}", seconds, counts.summary());
                window.started = Some(clock::instant());
                counts.active.store(true, Ordering::Relaxed);
                self.0.active.store(true, Ordering::Release);
            }
        }
        let starts = clock::instant() + Duration::from_secs(delay_seconds);
        let control = Arc::downgrade(&self.0);
        let counts = Arc::downgrade(&counts);
        runtime.spawn(async move {
            if delay_seconds != 0 {
                clock::sleep_until(starts).await;
                if !transition(&control, &counts, true, seconds) { return; }
            }
            clock::sleep_until(starts + Duration::from_secs(seconds)).await;
            transition(&control, &counts, false, seconds);
        });
        Ok(())
    }
    fn active_counts(&self) -> Option<Arc<Counts>> {
        if !self.0.active.load(Ordering::Acquire) { return None; }
        self.0.window.lock_or_recover("the view observation window").counts.clone()
            .filter(|counts| counts.active.load(Ordering::Relaxed))
    }
    pub(crate) fn add(&self, metric: Metric, value: u64) {
        if let Some(counts) = self.active_counts() {
            counts.metrics[metric as usize].fetch_add(value, Ordering::Relaxed);
        }
    }
    pub(crate) fn serialize_component(
        &self, field: &'static str, value: &impl serde::Serialize, kind: Serialization,
    ) -> Vec<u8> {
        let counts = self.active_counts();
        let bytes = serde_json::to_vec(value).expect("live component serialization");
        if let Some(counts) = counts {
            let window = self.0.window.lock_or_recover("the view observation window");
            if counts.active.load(Ordering::Relaxed)
                && window.counts.as_ref().is_some_and(|current| Arc::ptr_eq(current, &counts)) {
                let (total, nodes, prs, activity, history, maps, other) = match kind {
                    Serialization::Digest => (Metric::DigestBytes, Metric::DigestNodesBytes,
                        Metric::DigestPullRequestsBytes, Metric::DigestActivityBytes,
                        Metric::DigestTaskHistoryBytes, Metric::DigestTaskMapsBytes, Metric::DigestOtherBytes),
                    Serialization::Sampler => (Metric::SamplerBytes, Metric::SamplerNodesBytes,
                        Metric::SamplerPullRequestsBytes, Metric::SamplerActivityBytes,
                        Metric::SamplerTaskHistoryBytes, Metric::SamplerTaskMapsBytes, Metric::SamplerOtherBytes),
                };
                let component = match field {
                    "nodes" => nodes,
                    "pull_requests" => prs,
                    "activity" => activity,
                    "task_history" => history,
                    "task_options" | "task_since" | "task_usage_until" | "task_started"
                        | "task_costs" | "cancel_reasons" | "launch_refusals" | "start_waits" => maps,
                    _ => other,
                };
                let serializations = match kind {
                    Serialization::Digest => Metric::DigestSerializations,
                    Serialization::Sampler => Metric::SamplerSerializations,
                };
                counts.metrics[serializations as usize].fetch_add(1, Ordering::Relaxed);
                counts.metrics[Metric::ComponentSerializations as usize].fetch_add(1, Ordering::Relaxed);
                counts.metrics[total as usize].fetch_add(bytes.len() as u64, Ordering::Relaxed);
                counts.metrics[component as usize].fetch_add(bytes.len() as u64, Ordering::Relaxed);
            }
        }
        bytes
    }

    #[cfg(test)]
    pub(crate) fn serialize_snapshot(&self, live: &alondra_view_model::LiveSnapshot, kind: Serialization) -> Vec<u8> {
        let Some(counts) = self.active_counts() else {
            return serde_json::to_vec(live).expect("live snapshot serialization");
        };
        let (bytes, breakdown) = super::serialization::serialize(live);
        self.record_serialization(&counts, kind, bytes.len() as u64, breakdown);
        bytes
    }

    #[cfg(test)]
    fn record_serialization(&self, counts: &Arc<Counts>, kind: Serialization, bytes: u64, parts: super::serialization::Breakdown) {
        let window = self.0.window.lock_or_recover("the view observation window");
        if !counts.active.load(Ordering::Relaxed)
            || !window.counts.as_ref().is_some_and(|current| Arc::ptr_eq(current, counts)) { return; }
        debug_assert_eq!(parts.total(), bytes);
        let (serializations, total, components) = match kind {
            Serialization::Digest => (Metric::DigestSerializations, Metric::DigestBytes, [
                Metric::DigestNodesBytes, Metric::DigestPullRequestsBytes, Metric::DigestActivityBytes,
                Metric::DigestTaskHistoryBytes, Metric::DigestTaskMapsBytes, Metric::DigestOtherBytes,
                Metric::DigestStructuralBytes,
            ]),
            Serialization::Sampler => (Metric::SamplerSerializations, Metric::SamplerBytes, [
                Metric::SamplerNodesBytes, Metric::SamplerPullRequestsBytes, Metric::SamplerActivityBytes,
                Metric::SamplerTaskHistoryBytes, Metric::SamplerTaskMapsBytes, Metric::SamplerOtherBytes,
                Metric::SamplerStructuralBytes,
            ]),
        };
        counts.metrics[serializations as usize].fetch_add(1, Ordering::Relaxed);
        counts.metrics[total as usize].fetch_add(bytes, Ordering::Relaxed);
        for (metric, bytes) in components.into_iter().zip([
            parts.nodes, parts.pull_requests, parts.activity, parts.task_history,
            parts.task_maps, parts.other, parts.structural,
        ]) {
            counts.metrics[metric as usize].fetch_add(bytes, Ordering::Relaxed);
        }
    }
    pub(crate) fn invalid(&self, stage: Stage, mask: u16) {
        if mask == 0 { return; }
        let Some(counts) = self.active_counts() else { return };
        let stage = stage as usize;
        counts.invalid_events[stage].fetch_add(1, Ordering::Relaxed);
        for cause in 0..CAUSES.len() {
            if mask & (1 << cause) != 0 {
                counts.causes[stage][cause].fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    #[cfg(test)]
    pub(crate) fn totals(&self) -> Totals {
        self.0.window.lock_or_recover("the view observation window").counts.as_ref()
            .map(|counts| counts.totals())
            .unwrap_or(Totals { metrics: [0; NAMES.len()], invalid_events: [0; STAGES.len()], causes: [[0; CAUSES.len()]; STAGES.len()] })
    }
    #[cfg(test)]
    fn summary(&self) -> String { summary(self.totals(), 0) }
}

impl Counts {
    fn totals(&self) -> Totals {
        let counts = self;
        Totals {
            metrics: std::array::from_fn(|i| counts.metrics[i].load(Ordering::Relaxed)),
            invalid_events: std::array::from_fn(|i| counts.invalid_events[i].load(Ordering::Relaxed)),
            causes: std::array::from_fn(|i| std::array::from_fn(|j| counts.causes[i][j].load(Ordering::Relaxed))),
        }
    }
    fn summary(&self) -> String {
        summary(self.totals(), self.elapsed_us.load(Ordering::Relaxed))
    }
}
fn summary(totals: Totals, elapsed_us: u64) -> String {
    let mut fields = std::collections::BTreeMap::new();
    fields.insert("elapsed_us".to_owned(), elapsed_us);
    for (name, value) in NAMES.iter().zip(totals.metrics) { fields.insert((*name).to_owned(), value); }
    for (stage, name) in STAGES.iter().enumerate() {
        fields.insert(format!("{name}_invalid"), totals.invalid_events[stage]);
        for (cause, label) in CAUSES.iter().enumerate() {
            fields.insert(format!("{name}_{label}"), totals.causes[stage][cause]);
        }
    }
    serde_json::to_string(&fields).expect("aggregate view counts serialize")
}

fn transition(control: &Weak<Control>, counts: &Weak<Counts>, active: bool, seconds: u64) -> bool {
    let (Some(control), Some(counts)) = (control.upgrade(), counts.upgrade()) else { return false };
    let mut window = control.window.lock_or_recover("the view observation window");
    if !window.running || !window.counts.as_ref().is_some_and(|current| Arc::ptr_eq(current, &counts)) { return false; }
    if active {
        log!("view observation start seconds={} totals={}", seconds, counts.summary());
        window.started = Some(clock::instant());
    }
    counts.active.store(active, Ordering::Relaxed);
    control.active.store(active, Ordering::Release);
    if !active {
        window.running = false;
        let elapsed = window.started.map(clock::since).unwrap_or_default();
        counts.elapsed_us.store(elapsed.as_micros().min(u64::MAX as u128) as u64, Ordering::Relaxed);
        log!("view observation end seconds={} totals={}", seconds, counts.summary());
    }
    true
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_counters_measure_each_executed_serialization_and_its_bytes() {
        let observation = Observation::armed();
        let value = vec!["synthetic text with an escaped quote: \""];
        let expected = serde_json::to_vec(&value).unwrap();
        assert_eq!(observation.serialize_component("nodes", &value, Serialization::Digest), expected);
        assert!(observation.totals().metrics.iter().all(|count| *count == 0));
        observation.start();
        for kind in [Serialization::Digest, Serialization::Sampler] {
            assert_eq!(observation.serialize_component("nodes", &value, kind), expected);
        }
        let totals = observation.totals();
        assert_eq!(totals.metrics[Metric::ComponentSerializations as usize], 2);
        for metric in [Metric::DigestSerializations, Metric::SamplerSerializations] {
            assert_eq!(totals.metrics[metric as usize], 1);
        }
        for metric in [Metric::DigestBytes, Metric::DigestNodesBytes, Metric::SamplerBytes, Metric::SamplerNodesBytes] {
            assert_eq!(totals.metrics[metric as usize], expected.len() as u64);
        }
        observation.stop();
        observation.serialize_component("nodes", &value, Serialization::Sampler);
        assert_eq!(observation.totals(), totals);
    }

    #[test]
    fn component_serialization_cannot_enter_a_window_started_while_it_was_running() {
        struct ReplaceWindow(Observation);
        impl serde::Serialize for ReplaceWindow {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                self.0.stop();
                self.0.start_window(60, 0).unwrap();
                serializer.serialize_str("synthetic value")
            }
        }
        clock::VirtualRuntime::new(0).unwrap().block_on(async {
            let observation = Observation::default();
            observation.start_window(60, 0).unwrap();
            let old = observation.active_counts().unwrap();
            observation.serialize_component("nodes", &ReplaceWindow(observation.clone()), Serialization::Digest);
            assert!(old.totals().metrics.iter().all(|count| *count == 0));
            assert!(observation.totals().metrics.iter().all(|count| *count == 0));
            observation.serialize_component("nodes", &vec![1], Serialization::Digest);
            assert_eq!(observation.totals().metrics[Metric::ComponentSerializations as usize], 1);
        });
    }

    #[test]
    fn snapshot_bytes_are_unchanged_and_components_are_counted_only_while_active() {
        let live: alondra_view_model::LiveSnapshot = serde_json::from_value(serde_json::json!({
            "issue_error": "fixture text: é 🐦 \n \" \\",
            "task_options": {"nodes": {"effort": "high"}},
            "activity": {"task_history": [{"since": "now", "summary": "nested"}]}
        })).unwrap();
        let expected = serde_json::to_vec(&live).unwrap();
        let (_, parts) = super::super::serialization::serialize(&live);
        let expected_parts = [parts.nodes, parts.pull_requests, parts.activity, parts.task_history,
            parts.task_maps, parts.other, parts.structural];
        let observation = Observation::armed();
        for kind in [Serialization::Digest, Serialization::Sampler] {
            assert_eq!(observation.serialize_snapshot(&live, kind), expected);
        }
        assert!(observation.totals().metrics.iter().all(|value| *value == 0));
        observation.start();
        for (kind, prefix) in [(Serialization::Digest, "digest"), (Serialization::Sampler, "sampler")] {
            assert_eq!(observation.serialize_snapshot(&live, kind), expected);
            let summary: std::collections::BTreeMap<String, u64> = serde_json::from_str(&observation.summary()).unwrap();
            assert_eq!(summary[&format!("{prefix}_serializations")], 1);
            assert_eq!(summary[&format!("{prefix}_bytes")], expected.len() as u64);
            let mut component_total = 0;
            for (component, expected) in ["nodes", "pull_requests", "activity", "task_history", "task_maps", "other", "structural"]
                .into_iter().zip(expected_parts) {
                let actual = summary[&format!("{prefix}_{component}_bytes")];
                assert_eq!(actual, expected);
                component_total += actual;
            }
            assert_eq!(component_total, summary[&format!("{prefix}_bytes")]);
        }
        let counted = observation.totals();
        assert_eq!(counted.metrics[Metric::DigestSerializations as usize], 1);
        assert_eq!(counted.metrics[Metric::SamplerSerializations as usize], 1);
        observation.stop();
        for kind in [Serialization::Digest, Serialization::Sampler] {
            assert_eq!(observation.serialize_snapshot(&live, kind), expected);
        }
        assert_eq!(observation.totals(), counted);
        assert!(!observation.summary().contains("fixture text"));
        assert!(!observation.summary().contains("nested"));
    }

    #[test]
    fn a_serialization_finishing_after_stop_cannot_enter_a_replacement_window() {
        clock::VirtualRuntime::new(0).unwrap().block_on(async {
            let observation = Observation::default();
            observation.start_window(60, 0).unwrap();
            let captured = observation.active_counts().unwrap();
            let (bytes, parts) = super::super::serialization::serialize(&alondra_view_model::LiveSnapshot::default());
            observation.stop();
            observation.record_serialization(&captured, Serialization::Digest, bytes.len() as u64, parts);
            assert!(captured.totals().metrics.iter().all(|count| *count == 0));
            observation.start_window(60, 0).unwrap();
            observation.record_serialization(&captured, Serialization::Sampler, bytes.len() as u64, parts);
            assert!(observation.totals().metrics.iter().all(|count| *count == 0));
            assert!(captured.totals().metrics.iter().all(|count| *count == 0));
            observation.serialize_snapshot(&alondra_view_model::LiveSnapshot::default(), Serialization::Digest);
            assert_eq!(observation.totals().metrics[Metric::DigestSerializations as usize], 1);
        });
    }

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

    #[test]
    fn hot_windows_validate_without_mutating_and_require_a_runtime() {
        let observation = Observation::default();
        for (seconds, delay) in [(0, 0), (3601, 0), (1, 3601)] {
            assert_eq!(observation.start_window(seconds, delay), Err(StartError::InvalidWindow));
        }
        assert_eq!(observation.start_window(1, 0), Err(StartError::RuntimeUnavailable));
        assert!(!observation.is_running());
        let _window = observation.0.window.lock().unwrap();
        observation.add(Metric::GatherRequest, 1);
        observation.invalid(Stage::Lookup, Cause::Revision.bit());
    }

    #[test]
    fn hot_windows_delay_reject_overlap_stop_and_rearm_with_fresh_counts() {
        clock::VirtualRuntime::new(0).unwrap().block_on(async {
            let observation = Observation::default();
            let existing_clone = observation.clone();
            observation.start_window(20, 10).unwrap();
            assert!(observation.is_running());
            assert_eq!(observation.start_window(1, 0), Err(StartError::AlreadyRunning));
            existing_clone.add(Metric::GatherRequest, 1);
            assert_eq!(observation.totals().metrics[Metric::GatherRequest as usize], 0);
            clock::sleep(Duration::from_secs(11)).await;
            existing_clone.add(Metric::GatherRequest, 3);
            assert_eq!(observation.totals().metrics[Metric::GatherRequest as usize], 3);
            assert_eq!(observation.start_window(1, 0), Err(StartError::AlreadyRunning));
            clock::sleep(Duration::from_secs(20)).await;
            assert!(!observation.is_running());
            existing_clone.add(Metric::GatherRequest, 1);
            assert_eq!(observation.totals().metrics[Metric::GatherRequest as usize], 3);
            observation.start_window(2, 0).unwrap();
            assert_eq!(observation.totals().metrics[Metric::GatherRequest as usize], 0);
            existing_clone.add(Metric::GatherRequest, 1);
            clock::sleep(Duration::from_secs(3)).await;
            assert!(!observation.is_running());
            assert_eq!(observation.totals().metrics[Metric::GatherRequest as usize], 1);
        });
    }

    #[test]
    fn timers_do_not_retain_the_owner_or_change_a_replacement_window() {
        clock::VirtualRuntime::new(0).unwrap().block_on(async {
            for delay in [0, 10] {
                let observation = Observation::default();
                observation.start_window(20, delay).unwrap();
                let weak = Arc::downgrade(&observation.0);
                let counts = Arc::downgrade(observation.0.window.lock().unwrap().counts.as_ref().unwrap());
                drop(observation);
                assert!(weak.upgrade().is_none());
                assert!(counts.upgrade().is_none());
                clock::sleep(Duration::from_secs(31)).await;
                assert!(weak.upgrade().is_none());
            }
            let observation = Observation::default();
            observation.start_window(10, 0).unwrap();
            let old_counts = observation.0.window.lock().unwrap().counts.clone().unwrap();
            observation.stop();
            observation.start_window(30, 0).unwrap();
            clock::sleep(Duration::from_secs(11)).await;
            assert!(observation.is_running(), "the old timer cannot stop the replacement window");
            observation.add(Metric::GatherRequest, 1);
            assert_eq!(observation.totals().metrics[Metric::GatherRequest as usize], 1);
            assert_eq!(old_counts.totals().metrics[Metric::GatherRequest as usize], 0);
        });
    }

    #[test]
    fn elapsed_time_measures_active_time_instead_of_the_requested_window() {
        clock::VirtualRuntime::new(0).unwrap().block_on(async {
            let observation = Observation::default();
            observation.start_window(20, 10).unwrap();
            clock::sleep(Duration::from_secs(15)).await;
            let counts = observation.0.window.lock().unwrap().counts.clone().unwrap();
            assert!(transition(&Arc::downgrade(&observation.0), &Arc::downgrade(&counts), false, 20));
            let totals: std::collections::BTreeMap<String, u64> = serde_json::from_str(&counts.summary()).unwrap();
            assert_eq!(totals["elapsed_us"], 5_000_000, "neither the delay nor nominal duration is counted");
            observation.start_window(20, 0).unwrap();
            let fresh = observation.0.window.lock().unwrap().counts.clone().unwrap();
            assert_eq!(fresh.elapsed_us.load(Ordering::Relaxed), 0);
            clock::sleep(Duration::from_secs(21)).await;
            assert_eq!(fresh.elapsed_us.load(Ordering::Relaxed), 20_000_000);
        });
    }

    #[test]
    fn hot_activation_counts_the_existing_clients_actual_feed_work() {
        clock::VirtualRuntime::new(0).unwrap().block_on(async {
            let home = crate::test_support::Directory::new();
            std::fs::create_dir(&home).unwrap();
            let domain = Arc::new(crate::engine_tests::open(&home).unwrap());
            let observation = Observation::default();
            let client = crate::client_routes::Client::open_observed(
                domain, &home, Arc::new(Mutex::new(None)), observation.clone(),
            );
            let query = "protocol=1&client=tui&lang=en&wait=0";
            let response = super::super::feed::handle(&client, "terminal", query, alondra_view_model::Transport::Loopback).await;
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            assert_eq!(observation.totals().metrics[Metric::FeedRequests as usize], 0);
            client.start_view_observation(20, 0).unwrap();
            let response = super::super::feed::handle(&client, "terminal", query, alondra_view_model::Transport::Loopback).await;
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            assert_eq!(observation.totals().metrics[Metric::FeedRequests as usize], 1);
            assert_eq!(observation.totals().metrics[Metric::FeedChanged as usize], 1);
            clock::sleep(Duration::from_secs(21)).await;
            assert!(!client.view_observation_is_running());
            let response = super::super::feed::handle(&client, "terminal", query, alondra_view_model::Transport::Loopback).await;
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            assert_eq!(observation.totals().metrics[Metric::FeedRequests as usize], 1);
            client.start_view_observation(20, 0).unwrap();
            assert_eq!(observation.totals().metrics[Metric::FeedRequests as usize], 0);
            let response = super::super::feed::handle(&client, "terminal", query, alondra_view_model::Transport::Loopback).await;
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            assert_eq!(observation.totals().metrics[Metric::FeedRequests as usize], 1);
        });
    }
}
