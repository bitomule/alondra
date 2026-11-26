use super::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

/// The domain inputs the live row uses. Reports, hook state, contexts, launch options and files
/// have their own change sources; a row cannot be reused from this reading alone.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveNodeReading {
    pub name: String,
    pub phase: NodePhase,
    /// All tasks, including archived history, in the node machine's task-id order.
    pub tasks: Vec<TaskId>,
    /// Requests of non-archived tasks, in the order used to find a launch's project.
    pub launch_requests: Vec<RequestId>,
    pub open_task: bool,
    /// The node machine's selection, which is not `ReadIndex::task_of_node`.
    pub current_task: Option<TaskId>,
    pub sdk: Option<alondra_domain::LiveState>,
    pub waiting_on_pause: bool,
    /// Provider and execution instance of the first task, whose wait file the row reads.
    pub reported_wait: Option<(alondra_core::accounts::Provider, String)>,
}

impl LiveNodeReading {
    pub(super) fn of(state: &DomainState, agent: &Agent, tasks: &[&Task]) -> Self {
        let current = super::super::node_machine::current_of(tasks);
        let sdk = (agent.phase == NodePhase::ThroughSdk)
            .then(|| state.sdk_live_of(&agent.id, current)).flatten();
        let waiting_on_pause = matches!(&sdk, Some(alondra_domain::LiveState::WaitingUser(cards))
            if cards.iter().all(|card| state.node_pause_card(card)));
        let reported_wait = tasks.first().and_then(|task| {
            let lifecycle = state.lifecycle_of(&task.id).ok()?;
            Some((task.account_key().provider, lifecycle.execution()?.instance().as_str().to_owned()))
        });
        Self {
            name: agent.name.clone(),
            phase: agent.phase.clone(),
            tasks: tasks.iter().map(|task| task.id.clone()).collect(),
            launch_requests: tasks.iter().filter(|task| !task.archived).map(|task| task.request_id.clone()).collect(),
            open_task: tasks.iter().any(|task| !task.archived && !matches!(task.state, TaskState::Done | TaskState::Closing | TaskState::Cancelled)),
            current_task: current.map(|task| task.id.clone()),
            sdk,
            waiting_on_pause,
            reported_wait,
        }
    }
}

/// A private lineage and publication position; revisions alone do not identify reloads.
#[derive(Clone, Debug)]
pub struct NodeChangeCursor {
    lineage: Arc<AtomicU64>,
    generation: u64,
}

impl PartialEq for NodeChangeCursor {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.lineage, &other.lineage) && self.generation == other.generation
    }
}
impl Eq for NodeChangeCursor {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeChanges {
    /// The reader must rebuild: its lineage or retained history no longer describes this one.
    Reset { cursor: NodeChangeCursor },
    /// Includes removed nodes. Readers use their own cursor; no reader consumes another's IDs.
    Changed { cursor: NodeChangeCursor, nodes: BTreeSet<AgentId> },
}

const RETAINED_NODE_IDS: usize = 4096;

#[derive(Clone, Debug, Default)]
pub(super) struct NodeHistory {
    lineage: Arc<AtomicU64>,
    generation: u64,
    floor: u64,
    entries: VecDeque<(u64, AgentId)>,
}

impl NodeHistory {
    fn cursor(&self) -> NodeChangeCursor {
        NodeChangeCursor { lineage: self.lineage.clone(), generation: self.generation }
    }

    fn after(previous: &ReadSnapshot, index: &ReadIndex) -> Self {
        let Some(generation) = previous.node_history.generation.checked_add(1) else { return Self::default() };
        // A second successor is a fork, not another publication in the same linear history.
        if previous.node_history.lineage.compare_exchange(previous.node_history.generation, generation, Ordering::Relaxed, Ordering::Relaxed).is_err() {
            return Self::default();
        }
        let mut history = previous.node_history.clone();
        history.generation = generation;
        for (id, row) in &index.live_nodes {
            if previous.index.live_nodes.get(id) != Some(row) {
                history.entries.push_back((generation, id.clone()));
            }
        }
        for id in previous.index.live_nodes.keys() {
            if !index.live_nodes.contains_key(id) { history.entries.push_back((generation, id.clone())); }
        }
        while history.entries.len() > RETAINED_NODE_IDS {
            let oldest = history.entries.front().expect("nonempty node history").0;
            while history.entries.front().is_some_and(|(at, _)| *at == oldest) {
                history.entries.pop_front();
            }
            history.floor = oldest;
        }
        history
    }

    fn since(&self, before: &NodeChangeCursor) -> NodeChanges {
        let cursor = self.cursor();
        if !Arc::ptr_eq(&self.lineage, &before.lineage)
            || before.generation < self.floor || before.generation > self.generation {
            return NodeChanges::Reset { cursor };
        }
        if before.generation == self.generation {
            return NodeChanges::Changed { cursor, nodes: BTreeSet::new() };
        }
        let nodes = self.entries.iter().rev().inspect(|_| crate::measure::node_change_entry_read()).take_while(|(at, _)| *at > before.generation)
            .map(|(_, id)| id.clone()).collect();
        NodeChanges::Changed { cursor, nodes }
    }
}

impl ReadSnapshot {
    /// Publishes the next immutable index and a bounded history of its changed live node inputs.
    /// A reload at the same revision, an epoch change or a fork starts a separate lineage.
    /// Revision jumps are compared in full; only a gap in retained history requires a reset.
    pub fn build_after(state: Arc<DomainState>, previous: &Self) -> Self {
        if Arc::ptr_eq(&state, &previous.state) { return previous.clone(); }
        let index = Arc::new(ReadIndex::build(&state));
        let node_history = if state.data_epoch == previous.state.data_epoch && state.revision > previous.state.revision {
            NodeHistory::after(previous, &index)
        } else {
            NodeHistory::default()
        };
        Self { state, index, node_history }
    }

    pub fn node_cursor(&self) -> NodeChangeCursor { self.node_history.cursor() }

    /// Reads only the bounded publication history, never the domain's agent or task tables.
    pub fn node_changes_since(&self, before: &NodeChangeCursor) -> NodeChanges {
        self.node_history.since(before)
    }
}

#[cfg(test)]
#[path = "node_changes_tests.rs"]
mod tests;
