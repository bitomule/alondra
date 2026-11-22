//! A per-language row cache. Every retained boundary is checked again after construction.
use super::{live::node_row, node_components::Nodes, observation::{Metric, Observation}};
use alondra_daemon_engine::{live::{LiveChangeKey, LiveOf as _}, contexts::ContextsOf as _};
use alondra_domain_rules::{AgentId, DomainState, NodeChangeCursor, NodeChanges, ReadSnapshot, RequestId, TaskId};
use alondra_foundation::changes::{Changed, Cursor};
use alondra_view_model::Lang;
use std::{collections::{BTreeMap, BTreeSet}, sync::{Arc, Mutex}};

type Launches = BTreeMap<RequestId, (Option<String>, Option<String>)>;
type Client = crate::client_routes::Client;

#[derive(Clone, PartialEq, Eq)]
struct Sources {
    reports: Cursor,
    hooks: Cursor,
    contexts: Cursor,
    published: NodeChangeCursor,
    usage: Option<crate::usage_pause::Exhausted>,
}
impl Sources {
    fn now(client: &Client) -> Self {
        Self {
            reports: client.domain.live().mods.change_cursor(),
            hooks: client.domain.live().change_cursor(),
            contexts: client.domain.contexts().change_cursor(),
            published: client.domain.read().node_cursor(),
            usage: client.domain.live().usage_pause.current().filter(|usage| !usage.no_session),
        }
    }
}

struct Entry {
    nodes: Nodes,
    state: Arc<DomainState>,
    domain: NodeChangeCursor,
    sources: Sources,
    store: alondra_engine_ports::journal::ReadCursor,
    launches: Arc<Launches>,
    /// These sources still lack a complete event contract; never silently retain their values.
    external: BTreeSet<AgentId>,
    clock: (i64, String, Option<String>),
}

#[derive(Default)]
pub(super) struct Cache(Mutex<BTreeMap<u8, Entry>>);

fn same_projects(left: &Launches, right: &Launches, observation: &Observation) -> bool {
    if left.len() != right.len() { return false; }
    left.iter().zip(right).all(|((a, (project_a, _)), (b, (project_b, _)))| {
        observation.add(Metric::NodeStoreRowsCompared, 1);
        a == b && project_a == project_b
    })
}

fn owners(task: &TaskId, previous: &DomainState, current: &DomainState, changed: &mut BTreeSet<AgentId>) {
    for state in [previous, current] {
        if let Some(agent) = state.tasks.get(task).and_then(|task| task.agent_id.as_ref()) {
            changed.insert(agent.clone());
        }
    }
}

fn clock_key(now: i64) -> (i64, String, Option<String>) {
    // clock_text changes at local midnight; timestamps also depend on the local UTC offset.
    let stamp = crate::log::local_iso(now.div_euclid(1000) as libc::time_t, 0);
    (crate::usage_pause::local_midnight(now), stamp.chars().rev().take(6).collect(), std::env::var("TZ").ok())
}

fn has_external_source(client: &Client, read: &ReadSnapshot, id: &AgentId) -> bool {
    read.index().live_node(id).is_some_and(|reading| reading.reported_wait.as_ref().is_some_and(|(provider, instance)|
        alondra_daemon_engine::sdk_live::wait_file(&client.domain, *provider, instance).is_some()))
        || client.domain.live().mods.session(&id.0).is_some_and(|report|
            report.work.workflows.iter().any(|workflow| workflow.transcript_dir.is_some()))
}

impl Cache {
    pub(super) fn gather(&self, client: &Client, read: &ReadSnapshot, lang: Lang,
        launches: &Launches, boundary: Option<alondra_engine_ports::journal::ReadCursor>, launches_ok: bool,
        usage: &Option<crate::usage_pause::Exhausted>, now: i64) -> Nodes {
        self.gather_checked(client, read, lang, launches, boundary, launches_ok, usage, now, || {})
    }

    fn gather_checked(&self, client: &Client, read: &ReadSnapshot, lang: Lang,
        launches: &Launches, boundary: Option<alondra_engine_ports::journal::ReadCursor>, launches_ok: bool,
        usage: &Option<crate::usage_pause::Exhausted>, now: i64, after_build: impl FnOnce()) -> Nodes {
        let observation = &client.view_observation;
        let mut entries = super::live::cache(&self.0);
        let key = u8::from(lang == Lang::Es);
        let before = Sources::now(client);
        let clock = clock_key(now);
        let mut changed = BTreeSet::new();
        let previous = entries.get(&key);
        let mut full = previous.is_none() || !launches_ok || boundary.is_none();
        if let Some(previous) = previous {
            match read.node_changes_since(&previous.domain) {
                NodeChanges::Reset { .. } => full = true,
                NodeChanges::Changed { nodes, .. } => changed.extend(nodes),
            }
            match client.domain.live().mods.changes_since(&previous.sources.reports).changed {
                Changed::Reset => full = true,
                Changed::Keys(ids) => changed.extend(ids.into_iter().map(AgentId)),
            }
            match client.domain.live().changes_since(&previous.sources.hooks).changed {
                Changed::Reset => full = true,
                Changed::Keys(keys) => for key in keys {
                    match key {
                        LiveChangeKey::Node(id) => { changed.insert(AgentId(id)); },
                        LiveChangeKey::Task(id) => owners(&TaskId(id), &previous.state, read.state(), &mut changed),
                    }
                },
            }
            match client.domain.contexts().changes_since(&previous.sources.contexts).changed {
                Changed::Reset => full = true,
                Changed::Keys(tasks) => for task in tasks { owners(&task, &previous.state, read.state(), &mut changed); },
            }
            if previous.sources.usage != *usage || previous.clock != clock { full = true; }
            if boundary.as_ref() != Some(&previous.store) && !same_projects(&previous.launches, launches, observation) { full = true; }
            for id in &previous.external {
                observation.add(Metric::NodeExternalRows, 1);
                changed.insert(id.clone());
            }
        }
        let (mut nodes, mut external) = if full {
            observation.add(Metric::NodeFullBuilds, 1);
            let mut external = BTreeSet::new();
            let rows = read.state().agents.keys().filter_map(|id| {
                if has_external_source(client, read, id) { external.insert(id.clone()); }
                let row = node_row(client, read, id, lang, launches, usage, now);
                if row.is_some() { observation.add(Metric::NodeOutputRows, 1); }
                row
            }).collect();
            (Nodes::new(rows, observation), external)
        } else {
            observation.add(Metric::NodeIncrementalBuilds, 1);
            let previous = previous.expect("incremental rows have a previous boundary");
            let mut nodes = previous.nodes.clone();
            let mut external = previous.external.clone();
            for id in changed {
                let row = node_row(client, read, &id, lang, launches, usage, now);
                if row.is_some() { observation.add(Metric::NodeOutputRows, 1); }
                assert!(nodes.update(&id.0, row, observation), "domain node rows have unique ordered identities");
                if has_external_source(client, read, &id) { external.insert(id); } else { external.remove(&id); }
            }
            (nodes, external)
        };
        if full && let Some(previous) = previous { nodes.reuse(&previous.nodes, observation); }
        after_build();
        let current_store = client.domain.live_read().read_cursor().ok();
        let retain = launches_ok && boundary.is_some() && boundary == current_store
            && before.usage == *usage && before == Sources::now(client);
        if retain {
            let launches = if let Some(previous) = previous.filter(|entry| boundary.as_ref() == Some(&entry.store)) {
                previous.launches.clone()
            } else { Arc::new(launches.clone()) };
            entries.insert(key, Entry {
                nodes: nodes.clone(), state: read.state().clone(), domain: read.node_cursor(), sources: before,
                store: boundary.expect("a retained read has a store boundary"), launches,
                external: std::mem::take(&mut external), clock,
            });
        } else {
            observation.add(Metric::NodeSourceRaces, 1);
            // A failed build never advances a reader past a change it did not incorporate.
            entries.remove(&key);
        }
        nodes
    }
}

#[cfg(test)]
#[path = "node_rows_tests.rs"]
mod tests;
