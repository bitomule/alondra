//! What the model of the whole machine (`crates/machine-model`) runs of the gate: its decisions,
//! called as they are, which the gate itself keeps crate-private.
use alondra_core::Millis;
use alondra_domain_model::{DomainState, ReadIndex, TaskId};
use std::collections::BTreeSet;

pub mod admission {
    use super::*;
    pub use crate::admission::{Hold, Key, Waiting};

    pub fn decide_pacing(working: usize, limit: u32, warned: bool, last_admission: Option<Millis>, spacing: Millis, now: Millis) -> Result<(), Hold> {
        crate::admission::decide_pacing(working, limit, warned, last_admission, spacing, now)
    }
    pub fn ordered(queue: Vec<Waiting>) -> Vec<Waiting> {
        crate::admission::ordered(queue)
    }
    pub fn slot_tasks(state: &DomainState, index: &ReadIndex) -> BTreeSet<TaskId> {
        crate::admission::slot_tasks(state, index)
    }
    pub fn turns_to_grant(queue: &[Waiting], free: usize, start_reserves: bool) -> Vec<TaskId> {
        crate::admission::turns_to_grant(queue, free, start_reserves)
    }
    pub fn waiting_turns(state: &DomainState) -> BTreeSet<TaskId> {
        crate::admission::waiting_turns(state)
    }
}
