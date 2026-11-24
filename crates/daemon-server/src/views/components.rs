//! Shared live-view fields with independently memoized content hashes.
use super::observation::{Metric, Observation, Serialization};
use super::node_components::Nodes;
use alondra_core::{issues::Issue, options::{ProjectEntry, Spec}};
use alondra_view_model::*;
use serde::{Serialize, Serializer, ser::SerializeStruct};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::{Arc, OnceLock}};

#[derive(Debug)]
struct PartData<T> {
    value: T,
    digest: OnceLock<[u8; 32]>,
}

#[derive(Debug)]
pub(super) struct Part<T>(Arc<PartData<T>>);

impl<T> Clone for Part<T> {
    fn clone(&self) -> Self { Self(self.0.clone()) }
}

impl<T> Part<T> {
    pub(super) fn new(value: T) -> Self {
        Self(Arc::new(PartData { value, digest: OnceLock::new() }))
    }

    pub(super) fn value(&self) -> &T { &self.0.value }

    fn same(&self, other: &Self, observation: &Observation, equal: fn(&T, &T, &Observation) -> bool) -> bool {
        if Arc::ptr_eq(&self.0, &other.0) { return true; }
        observation.add(Metric::ComponentComparisons, 1);
        equal(self.value(), other.value(), observation)
    }
}

impl<T: Serialize> Part<T> {
    fn digest(&self, field: &'static str, observation: &Observation, kind: Serialization) -> [u8; 32] {
        *self.0.digest.get_or_init(|| {
            let bytes = observation.serialize_component(field, self.value(), kind);
            observation.add(Metric::HashBytes, bytes.len() as u64);
            observation.add(Metric::HashOperations, 1);
            Sha256::digest(bytes).into()
        })
    }
}

fn scalar<T: PartialEq>(left: &T, right: &T, _: &Observation) -> bool { left == right }

fn rows<T: PartialEq>(left: &Vec<T>, right: &Vec<T>, observation: &Observation) -> bool {
    if left.len() != right.len() { return false; }
    let mut compared = 0;
    let equal = left.iter().zip(right).all(|(left, right)| {
        compared += 1;
        left == right
    });
    observation.add(Metric::ComponentRowsCompared, compared);
    equal
}

fn entries<K: PartialEq, V: PartialEq>(left: &BTreeMap<K, V>, right: &BTreeMap<K, V>, observation: &Observation) -> bool {
    if left.len() != right.len() { return false; }
    let mut compared = 0;
    let equal = left.iter().zip(right).all(|(left, right)| {
        compared += 1;
        left == right
    });
    observation.add(Metric::ComponentRowsCompared, compared);
    equal
}

fn gauges(left: &Vec<Gauge>, right: &Vec<Gauge>, observation: &Observation) -> bool {
    if left.len() != right.len() { return false; }
    let mut compared = 0;
    let equal = left.iter().zip(right).all(|(left, right)| {
        compared += 1;
        // JSON distinguishes signed zero. NaN deliberately does not reuse a component;
        // its serialized null still produces the same digest on the next computation.
        left == right && left.value.to_bits() == right.value.to_bits()
            && left.parts.iter().zip(&right.parts).all(|(left, right)| {
                left.value.to_bits() == right.value.to_bits()
            })
    });
    observation.add(Metric::ComponentRowsCompared, compared);
    equal
}

fn present<T>(_: &T) -> bool { true }
fn some<T>(value: &Option<T>) -> bool { value.is_some() }
fn nonempty_map<K, V>(value: &BTreeMap<K, V>) -> bool { !value.is_empty() }
fn nonempty_rows<T>(value: &Vec<T>) -> bool { !value.is_empty() }

macro_rules! storage {
    (nodes, $type:ty) => { Nodes };
    ($field:ident, $type:ty) => { Part<$type> };
}
macro_rules! stored {
    (nodes, $value:expr, $observation:expr) => { Nodes::new($value, $observation) };
    ($field:ident, $value:expr, $observation:expr) => { Part::new($value) };
}
macro_rules! materialized {
    (nodes, $value:expr, $observation:expr) => { $value.materialize($observation) };
    ($field:ident, $value:expr, $observation:expr) => { $value.value().clone() };
}
macro_rules! same {
    (nodes, $left:expr, $right:expr, $observation:expr, $equal:ident) => { $left.same($right, $observation) };
    ($field:ident, $left:expr, $right:expr, $observation:expr, $equal:ident) => { $left.same($right, $observation, $equal) };
}
macro_rules! reused {
    (nodes, $left:expr, $right:expr, $observation:expr, $equal:ident) => { $left.reuse($right, $observation) };
    ($field:ident, $left:expr, $right:expr, $observation:expr, $equal:ident) => {
        if $left.same($right, $observation, $equal) { $left = ($right).clone(); }
    };
}
macro_rules! hashed {
    (nodes, $value:expr, $observation:expr, $kind:expr) => { $value.digest($observation, $kind) };
    ($field:ident, $value:expr, $observation:expr, $kind:expr) => { $value.digest(stringify!($field), $observation, $kind) };
}

macro_rules! components {
    ($($field:ident: $type:ty => $equal:ident, $included:ident;)+) => {
        #[derive(Clone, Debug)]
        pub(super) struct Components {
            $(pub(super) $field: storage!($field, $type),)+
        }

        impl Components {
            pub(super) fn new(live: LiveSnapshot, observation: &Observation) -> Self {
                // No rest pattern: adding a public field requires assigning its component.
                let LiveSnapshot { $($field,)+ } = live;
                Self { $($field: stored!($field, $field, observation),)+ }
            }

            pub(super) fn reuse(&mut self, previous: &Self, observation: &Observation) {
                $(reused!($field, self.$field, &previous.$field, observation, $equal);)+
            }

            pub(super) fn same_payload(&self, other: &Self, observation: &Observation) -> bool {
                true $(&& same!($field, self.$field, &other.$field, observation, $equal))+
            }

            pub(super) fn materialize(&self, observation: &Observation) -> LiveSnapshot {
                observation.add(Metric::Materializations, 1);
                LiveSnapshot { $($field: materialized!($field, self.$field, observation),)+ }
            }

            pub(super) fn digest(&self, observation: &Observation, kind: Serialization) -> [u8; 32] {
                const DOMAIN: &[u8] = b"alondra/live-components\0";
                let mut hash = Sha256::new();
                hash.update(DOMAIN);
                let mut bytes = DOMAIN.len() as u64;
                $(if $included(self.$field.value()) {
                    let field = stringify!($field).as_bytes();
                    let digest = hashed!($field, self.$field, observation, kind);
                    hash.update((field.len() as u64).to_be_bytes());
                    hash.update(field);
                    hash.update((digest.len() as u64).to_be_bytes());
                    hash.update(digest);
                    bytes += 16 + field.len() as u64 + digest.len() as u64;
                })+
                observation.add(Metric::HashBytes, bytes);
                observation.add(Metric::HashOperations, 1);
                hash.finalize().into()
            }
        }

        impl Serialize for Components {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let length = 0 $(+ usize::from($included(self.$field.value())))+;
                let mut value = serializer.serialize_struct("LiveSnapshot", length)?;
                $(if $included(self.$field.value()) {
                    value.serialize_field(stringify!($field), self.$field.value())?;
                })+
                value.end()
            }
        }
    };
}

// The field order and omission predicates are those of LiveSnapshot's wire representation.
components! {
    observed_at: Option<alondra_core::Millis> => scalar, some;
    closed_nodes: Vec<String> => rows, present;
    closed_tasks: Vec<String> => rows, present;
    nodes: Vec<LiveNode> => rows, present;
    pull_requests: Vec<LivePullRequest> => rows, present;
    orchestrator: Option<LiveOrchestrator> => scalar, present;
    update: Option<Notice> => scalar, present;
    gauges: Vec<Gauge> => gauges, present;
    inbox_times: Vec<InboxTime> => rows, present;
    request_times: BTreeMap<String, String> => entries, present;
    projects: Vec<ProjectEntry> => rows, present;
    options: Vec<Spec> => rows, present;
    project_options: BTreeMap<String, Vec<Spec>> => entries, present;
    issues: Vec<Issue> => rows, present;
    issue_error: Option<String> => scalar, present;
    activity: BTreeMap<String, Vec<LiveActivity>> => entries, present;
    task_history: BTreeMap<String, Vec<LiveHistoryStep>> => entries, present;
    task_options: BTreeMap<String, LiveTaskOptions> => entries, present;
    message_times: BTreeMap<String, String> => entries, nonempty_map;
    task_since: BTreeMap<String, String> => entries, present;
    task_usage_until: BTreeMap<String, String> => entries, present;
    task_started: BTreeMap<String, LiveTaskStart> => entries, present;
    task_costs: BTreeMap<String, u64> => entries, present;
    cancel_reasons: BTreeMap<String, String> => entries, present;
    search_matches: Option<Vec<String>> => scalar, present;
    usage_exhausted: Option<LiveUsageExhausted> => scalar, present;
    waits_for_session: bool => scalar, present;
    launch_refusals: BTreeMap<String, LiveLaunchRefusal> => entries, present;
    start_waits: BTreeMap<String, LiveStartWait> => entries, present;
    pacing: Option<LivePacing> => scalar, present;
    allowed_rules: Vec<LiveAllowedRule> => rows, present;
    allow_always_missing: Vec<String> => rows, present;
    mcp_probes: BTreeMap<String, LiveMcpProbe> => entries, nonempty_map;
    mcp_removed: Vec<LiveMcpRemoval> => rows, nonempty_rows;
    metrics: Option<alondra_core::metrics::report::MetricsInputs> => scalar, some;
}

#[cfg(test)]
#[cfg(not(feature = "real-hosts"))]
mod tests {
    use super::*;

    #[test]
    fn serialization_and_materialization_preserve_the_public_wire() {
        let observation = Observation::default();
        let mut live = LiveSnapshot::default();
        for populated in [false, true] {
            if populated {
                live.observed_at = Some(42);
                live.message_times.insert("task".into(), "12:30".into());
                live.mcp_probes.insert("project".into(), LiveMcpProbe::Checking);
                live.metrics = Some(Default::default());
                live.nodes.push(LiveNode { agent_id: "node".into(), ..Default::default() });
            }
            let components = Components::new(live.clone(), &observation);
            assert_eq!(serde_json::to_vec(&components).unwrap(), serde_json::to_vec(&live).unwrap());
            assert_eq!(components.materialize(&observation), live);
        }
    }

    #[test]
    fn changing_one_field_reuses_all_other_field_storage_and_hashes() {
        let observation = Observation::armed();
        observation.start();
        let live = LiveSnapshot {
            nodes: vec![LiveNode::default(); 32],
            ..Default::default()
        };
        let previous = Components::new(live.clone(), &observation);
        let before = previous.digest(&observation, Serialization::Digest);
        let mut changed = Components::new(LiveSnapshot { waits_for_session: true, ..live }, &observation);
        changed.reuse(&previous, &observation);
        assert!(changed.nodes.same(&previous.nodes, &observation));
        let start = observation.totals();
        assert_ne!(changed.digest(&observation, Serialization::Digest), before);
        let end = observation.totals();
        assert_eq!(end.metrics[Metric::ComponentSerializations as usize] - start.metrics[Metric::ComponentSerializations as usize], 1);
        assert_eq!(end.metrics[Metric::HashOperations as usize] - start.metrics[Metric::HashOperations as usize], 2);
        assert_eq!(end.metrics[Metric::Materializations as usize], 0);
    }

    #[test]
    fn comparisons_count_only_rows_actually_examined_and_shared_parts_skip_them() {
        let observation = Observation::armed();
        observation.start();
        let previous = Components::new(LiveSnapshot { nodes: vec![LiveNode::default(); 32], ..Default::default() }, &observation);
        let mut next = Components::new(LiveSnapshot {
            nodes: vec![LiveNode { working: true, ..Default::default() }; 32],
            ..Default::default()
        }, &observation);
        next.reuse(&previous, &observation);
        assert_eq!(observation.totals().metrics[Metric::ComponentRowsCompared as usize], 32);
        let before = observation.totals();
        assert!(previous.same_payload(&previous.clone(), &observation));
        assert_eq!(observation.totals(), before);
    }

    #[test]
    fn float_comparisons_preserve_signed_zero_and_nonfinite_json_semantics() {
        let observation = Observation::default();
        let live = LiveSnapshot {
            gauges: vec![serde_json::from_value(serde_json::json!({
                "id": "gauge", "label": "Gauge", "value": 0.0, "text": "", "level": "calm",
                "parts": [{"label": "Part", "value": 0.0, "text": "", "level": "calm"}]
            })).unwrap()],
            ..Default::default()
        };
        let original = Components::new(live.clone(), &observation);
        let digest = original.digest(&observation, Serialization::Digest);
        for nested in [false, true] {
            let mut changed = live.clone();
            if nested { changed.gauges[0].parts[0].value = -0.0; }
            else { changed.gauges[0].value = -0.0; }
            let mut changed = Components::new(changed, &observation);
            changed.reuse(&original, &observation);
            assert!(!Arc::ptr_eq(&changed.gauges.0, &original.gauges.0));
            assert!(!changed.same_payload(&original, &observation));
            assert_ne!(changed.digest(&observation, Serialization::Digest), digest);
        }
        let mut nan = live;
        nan.gauges[0].value = f64::NAN;
        let previous = Components::new(nan.clone(), &observation);
        let mut next = Components::new(nan.clone(), &observation);
        next.reuse(&previous, &observation);
        assert!(!Arc::ptr_eq(&next.gauges.0, &previous.gauges.0));
        assert_eq!(next.digest(&observation, Serialization::Digest), previous.digest(&observation, Serialization::Digest));
        assert_eq!(serde_json::to_vec(&next).unwrap(), serde_json::to_vec(&nan).unwrap());
    }
}
