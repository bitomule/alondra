//! What the model of the whole machine (`crates/machine-model`) runs of the daemon's loops: the
//! decisions the gate, the hosts' owners and the obligation watch take, called as they are. The
//! orchestrator's inbox it reads from `engine-orchestrator`. Only the `test-hooks` feature opens
//! these functions; the daemon keeps them crate-private.
use alondra_core::Millis;
use alondra_core::domain::{AgentId, Command, DomainError, DomainState, ReadIndex, TaskId};
use std::collections::{BTreeMap, BTreeSet};

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

pub mod obligation_watch {
    pub use crate::obligation_watch::{Action, Watch, World};
}

pub mod problems {
    pub use alondra_engine_kernel::domain_runtime::problems::tell_command;
}

pub mod provider_transition {
    use super::*;
    pub use crate::provider_transition::MoveStart;

    pub fn move_start(state: &DomainState, task: &TaskId) -> Result<MoveStart, DomainError> {
        crate::provider_transition::move_start(state, task)
    }
    pub fn ready_in(state: &DomainState, task: &TaskId) -> Result<bool, DomainError> {
        crate::provider_transition::ready_in(state, task)
    }
}

pub mod sdk_owner {
    use super::*;

    pub const HOST: &str = crate::sdk_owner::HOST;

    pub fn due_unowned(taken: &mut BTreeMap<TaskId, (Millis, u64)>, working: Vec<(TaskId, u64)>, now: Millis) -> Vec<TaskId> {
        crate::sdk_owner::due_unowned(taken, working, now)
    }
    pub fn ended_node_tasks(state: &DomainState) -> Vec<TaskId> {
        crate::sdk_owner::ended_node_tasks(state)
    }
    pub fn reads_as_working(state: &DomainState) -> Vec<TaskId> {
        crate::sdk_owner::reads_as_working(state)
    }
    pub fn takeable(state: &DomainState, task: &TaskId) -> bool {
        crate::sdk_owner::takeable(state, task)
    }

    pub mod untaken {
        use super::*;

        pub fn wake_command(state: &DomainState, task: &TaskId, agent: &AgentId) -> Option<(String, Command)> {
            crate::sdk_owner::untaken::wake_command(state, task, agent)
        }
    }
}
