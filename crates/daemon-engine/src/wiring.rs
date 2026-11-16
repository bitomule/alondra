//! Where the engine is put together: the domain runtime opens with every subsystem that hears of
//! its commits. The runtime itself knows none of them.
use crate::domain_runtime::{CommitObserver, DomainRuntime};
use anyhow::Result;
use std::path::Path;

impl DomainRuntime {
    pub fn open(home: &Path) -> Result<Self> {
        Self::open_at(home, &crate::domain_database_migration::active_path(home))
    }

    pub(crate) fn open_at(home: &Path, database: &Path) -> Result<Self> {
        Self::open_observed(home, database, observers())
    }
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
