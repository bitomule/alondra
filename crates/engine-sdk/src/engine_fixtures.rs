//! What the tests here and in the crates above build on: the nodes of the Agent SDK they drive, the
//! orchestrator's tools they call, and the runtime they open, as the test said on its thread with
//! `alondra_daemon_engine::wiring::open_with`.
use crate::domain_runtime::{CommitObserver, DomainRuntime};
use std::path::Path;

#[path = "engine_fixtures/orchestrator_tools.rs"]
pub mod orchestrator_tools;
#[path = "engine_fixtures/sdk_node_fixture.rs"]
pub mod sdk_node_fixture;

pub fn open(home: &Path) -> anyhow::Result<DomainRuntime> {
    open_at(home, &alondra_engine_ports::journal::active_path(home))
}

pub fn open_at(home: &Path, database: &Path) -> anyhow::Result<DomainRuntime> {
    #[cfg(test)]
    crate::engine_tests::wired();
    alondra_daemon_engine::wiring::open_at(home, database)
}

/// The engine as `alondrad` puts it together, which the tests of the crates below the root cannot
/// see: the same decisions of held cards, observers and ports.
pub fn open_engine_at(home: &Path, database: &Path, files: &dyn alondra_engine_ports::journal::JournalFiles) -> anyhow::Result<DomainRuntime> {
    let runtime = DomainRuntime::open_observed(home, database, files, Some(alondra_daemon_engine::wiring::held_cards()), observers())?;
    alondra_daemon_engine::wiring::wire(runtime.ports());
    crate::wiring::wire(runtime.ports());
    alondra_engine_admission::wire(runtime.ports());
    Ok(runtime)
}

/// A copy of `alondrad`'s observers, in the same order; a test of `alondrad` fails when they differ.
pub fn observers() -> Vec<Box<dyn CommitObserver>> {
    vec![
        Box::new(alondra_daemon_engine::gate::PauseResume),
        Box::new(alondra_daemon_engine::sdk_live::EndedWaits),
        Box::new(crate::task_closer::Unattended),
        Box::new(alondra_daemon_engine::allow_always::ChosenAlways),
        Box::new(crate::sdk_owner::Retakes),
    ]
}
