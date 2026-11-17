//! Where the engine is put together: the domain runtime of `engine-kernel` opens with the decisions
//! of the cards a node's mod holds, with every subsystem that hears of its commits, in the order
//! they hear of each one, and with the ports the engine's crates ask each other through. The kernel
//! itself knows none of them; `alondrad` adds the orchestrator's.
use crate::domain_runtime::{CommitObserver, DomainRuntime};
use crate::hosts::SdkHosts;
use crate::ports::OrchestratorEngine;
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

pub fn open(home: &Path) -> Result<DomainRuntime> {
    open_at(home, &crate::domain_database_migration::active_path(home))
}

pub fn open_at(home: &Path, database: &Path) -> Result<DomainRuntime> {
    let runtime = DomainRuntime::open_observed(home, database, Some(alondra_daemon_engine::wiring::held_cards()), observers())?;
    runtime.provide::<dyn OrchestratorEngine>(Arc::new(alondra_daemon_engine::wiring::Engine));
    runtime.provide::<dyn SdkHosts>(Arc::new(crate::sdk_owner::Hosts));
    alondra_engine_admission::wire(&runtime);
    Ok(runtime)
}

/// Every subsystem that hears of the runtime's commits, in the order they hear of each one.
fn observers() -> Vec<Box<dyn CommitObserver>> {
    vec![
        Box::new(crate::gate::PauseResume),
        Box::new(crate::sdk_live::EndedWaits),
        Box::new(crate::task_closer::Unattended),
        Box::new(crate::allow_always::ChosenAlways),
        Box::new(crate::sdk_owner::Retakes),
    ]
}
