//! Where the engine is put together: the domain runtime of `engine-kernel` opens with the decisions
//! of the cards a node's mod holds and with every subsystem that hears of its commits. The kernel
//! itself knows none of them.
use crate::domain_runtime::{CommitObserver, DomainRuntime};
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

pub fn open(home: &Path) -> Result<DomainRuntime> {
    open_at(home, &crate::domain_database_migration::active_path(home))
}

pub fn open_at(home: &Path, database: &Path) -> Result<DomainRuntime> {
    DomainRuntime::open_observed(home, database, Some(Arc::new(crate::domain_holds::HeldCards)), observers())
}

/// Every subsystem that hears of the runtime's commits, in the order they hear of each one.
fn observers() -> Vec<Box<dyn CommitObserver>> {
    vec![
        Box::new(crate::admission::PauseResume),
        Box::new(crate::sdk_live::EndedWaits),
        Box::new(crate::task_closer::Unattended),
        Box::new(crate::allow_always::ChosenAlways),
        Box::new(crate::sdk_owner::Retakes),
    ]
}

#[cfg(test)]
#[path = "wiring/owner_tests.rs"]
mod owner_tests;
#[cfg(test)]
#[path = "wiring/system_command_tests.rs"]
mod system_command_tests;
#[cfg(test)]
#[path = "wiring/load_tests.rs"]
mod load_tests;
#[cfg(test)]
#[path = "wiring/metrics_tests.rs"]
mod metrics_tests;
