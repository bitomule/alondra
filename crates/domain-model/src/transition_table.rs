//! The moves of the machines whose `state-machine` block in docs/architecture/state-machines.md
//! declares `initial` and `transitions`: each task's phase (M-LIFE-1), each merge attempt (M-GH-2),
//! each node's phase (M-LIFE-9), each round of validation (M-LIFE-17) and each validation in it
//! (M-LIFE-18). A command whose move the inventory does not declare fails the
//! tests that run it, so a new transition cannot land without its line in the inventory.
use std::collections::{BTreeMap, BTreeSet};

use super::*;

/// Where a subject's first state comes from: a move from `START` is one of the block's `initial`.
pub const START: &str = "(start)";

/// The state of every subject of those machines: (machine, subject) → state.
pub fn states(state: &DomainState) -> BTreeMap<(&'static str, String), String> {
    let mut states = BTreeMap::new();
    for task in state.tasks.keys() {
        if let Ok(lifecycle) = state.lifecycle_of(task) {
            states.insert(("M-LIFE-1", task.0.clone()), format!("{:?}", lifecycle.phase()));
        }
    }
    for authorization in state.merge_authorizations.values() {
        states.insert(("M-GH-2", authorization.id.0.clone()), format!("{:?}", authorization.attempt));
    }
    for task in state.tasks.values() {
        for round in &task.validation.rounds {
            states.insert(("M-LIFE-17", format!("{}/{}", task.id, round.number)), format!("{:?}", round.state));
            for item in &round.items {
                states.insert(("M-LIFE-18", format!("{}/{}/{}/{}", task.id, round.number, item.subject, item.kind.as_str())), format!("{:?}", item.state));
            }
        }
    }
    for agent in state.agents.keys() {
        if let Some(phase) = state.node_phase(agent) {
            states.insert(("M-LIFE-9", agent.0.clone()), format!("{phase:?}"));
        }
    }
    states
}

/// The moves from `before` to `after`: (machine, from, to), with `START` for a new subject.
pub fn moves(before: &DomainState, after: &DomainState) -> BTreeSet<(String, String, String)> {
    let was = states(before);
    states(after).into_iter()
        .filter_map(|((machine, subject), now)| {
            let from = was.get(&(machine, subject)).cloned().unwrap_or_else(|| START.to_owned());
            (from != now).then(|| (machine.to_owned(), from, now))
        })
        .collect()
}

/// The moves the inventory declares: each block's `transitions` (`"From -> To"`) and, from `START`,
/// each of its `initial` states.
pub fn declared(inventory: &str) -> BTreeSet<(String, String, String)> {
    let mut declared = BTreeSet::new();
    let strings = |value: &str| -> Vec<String> {
        value.trim().trim_start_matches('[').trim_end_matches(']').split(',')
            .map(|item| item.trim().trim_matches('"').to_owned()).filter(|item| !item.is_empty()).collect()
    };
    for block in inventory.split("```toml state-machine\n").skip(1) {
        let block = block.split("```").next().unwrap_or_default();
        let value = |key: &str| block.lines().find_map(|line| line.strip_prefix(key)?.trim_start().strip_prefix('='));
        let Some(id) = value("id").map(|id| id.trim().trim_matches('"').to_owned()) else { continue };
        for initial in value("initial").map(strings).unwrap_or_default() {
            declared.insert((id.clone(), START.to_owned(), initial));
        }
        for transition in value("transitions").map(strings).unwrap_or_default() {
            if let Some((from, to)) = transition.split_once(" -> ") {
                declared.insert((id.clone(), from.to_owned(), to.to_owned()));
            }
        }
    }
    declared
}
