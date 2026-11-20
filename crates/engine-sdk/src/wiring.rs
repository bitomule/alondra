//! Where the engine is put together: the domain runtime of `engine-kernel` opens with the decisions
//! of the cards a node's mod holds, with every subsystem that hears of its commits, in the order
//! they hear of each one, and with the ports the engine's crates ask each other through. The kernel
//! itself knows none of them; `alondrad` adds the orchestrator's.
use crate::domain_runtime::{CommitObserver, DomainRuntime};
use crate::hosts::SdkHosts;
use alondra_engine_ports::Registry;
use alondra_engine_ports::journal::JournalFiles;
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

pub fn open(home: &Path, files: &dyn JournalFiles) -> Result<DomainRuntime> {
    open_at(home, &crate::domain_store::active_path(home), files)
}

pub fn open_at(home: &Path, database: &Path, files: &dyn JournalFiles) -> Result<DomainRuntime> {
    let runtime = DomainRuntime::open_observed(home, database, files, Some(alondra_daemon_engine::wiring::held_cards()), observers())?;
    alondra_daemon_engine::wiring::wire(runtime.ports());
    wire(runtime.ports());
    alondra_engine_admission::wire(runtime.ports());
    #[cfg(any(test, feature = "test-hooks"))]
    alondra_agent_runtime::wire(runtime.ports(), Default::default());
    Ok(runtime)
}

/// Gives the registry the owner of the SDK hosts the rest of the engine asks.
pub fn wire(registry: &Registry) {
    registry.provide::<dyn SdkHosts>(Arc::new(crate::sdk_owner::Hosts));
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
