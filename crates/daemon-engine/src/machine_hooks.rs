//! What the model of the whole machine (`crates/machine-model`) runs of the daemon's loops: the
//! decisions the hosts' owners and the obligation watch take, called as they are. The gate's it
//! reads from `engine-admission`, the orchestrator's inbox from `engine-orchestrator`. Only the `test-hooks` feature opens
//! these functions; the daemon keeps them crate-private.
use alondra_core::Millis;
use alondra_domain_rules::{AgentId, Command, DomainError, DomainState, ReadIndex, TaskId};
use std::collections::{BTreeMap, BTreeSet};

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
