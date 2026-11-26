//! What the model of the whole machine (`crates/machine-model`) runs of the hosts' owners and the
//! moves between providers: their decisions, called as they are.
use alondra_core::Millis;
use alondra_domain_model::{AgentId, Command, DomainError, DomainState, TaskId};
use std::collections::BTreeMap;

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

    pub const HOST: &str = alondra_engine_ports::hosts::HOST;

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
            alondra_engine_kernel::untaken::wake_command(state, task, agent)
        }
    }
}
