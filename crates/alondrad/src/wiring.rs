//! Where the engine is put together: the domain runtime of `engine-kernel` opens with the decisions
//! of the cards a node's mod holds, with every subsystem that hears of its commits, in the order
//! they hear of each one, and with the ports the engine's crates ask each other through. The kernel
//! itself knows none of them; the server adds the orchestrator's.
use alondra_engine_sdk::domain_runtime::{CommitObserver, DomainRuntime};
use alondra_engine_ports::journal::JournalFiles;
use anyhow::Result;
use std::path::Path;

pub fn open(home: &Path, files: &dyn JournalFiles) -> Result<DomainRuntime> {
    open_at(home, &alondra_engine_ports::journal::active_path(home), files)
}

pub fn open_at(home: &Path, database: &Path, files: &dyn JournalFiles) -> Result<DomainRuntime> {
    let runtime = DomainRuntime::open_observed(home, database, files, Some(alondra_daemon_engine::wiring::held_cards()), observers())?;
    alondra_daemon_engine::wiring::wire(runtime.ports());
    alondra_engine_sdk::wiring::wire(runtime.ports());
    alondra_engine_admission::wire(runtime.ports());
    crate::wire_providers(runtime.ports());
    Ok(runtime)
}

/// Every subsystem that hears of the runtime's commits, in the order they hear of each one. The
/// engine's tests open it with their copy, `engine-sdk/src/engine_assembly.rs`.
pub fn observers() -> Vec<Box<dyn CommitObserver>> {
    vec![
        Box::new(alondra_daemon_engine::gate::PauseResume),
        Box::new(alondra_daemon_engine::sdk_live::EndedWaits),
        Box::new(alondra_engine_sdk::task_closer::Unattended),
        Box::new(alondra_daemon_engine::allow_always::ChosenAlways),
        Box::new(alondra_engine_sdk::sdk_owner::Retakes),
    ]
}
