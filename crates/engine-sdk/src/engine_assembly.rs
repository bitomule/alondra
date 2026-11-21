//! The engine as `alondrad` puts it together, for the tests of the crates below the root, which
//! cannot see it: the same decisions of held cards, observers and ports. Each test crate that opens
//! the whole engine compiles this file; a test of `alondrad` fails when its observers differ.
use alondra_engine_sdk::domain_runtime::{CommitObserver, DomainRuntime};
use std::path::Path;

pub fn open_engine_at(home: &Path, database: &Path, files: &dyn alondra_engine_ports::journal::JournalFiles) -> anyhow::Result<DomainRuntime> {
    let runtime = DomainRuntime::open_observed(home, database, files, Some(alondra_daemon_engine::wiring::held_cards()), observers())?;
    alondra_daemon_engine::wiring::wire(runtime.ports());
    alondra_engine_sdk::wiring::wire(runtime.ports());
    alondra_engine_admission::wire(runtime.ports());
    Ok(runtime)
}

pub fn observers() -> Vec<Box<dyn CommitObserver>> {
    vec![
        Box::new(alondra_daemon_engine::gate::PauseResume),
        Box::new(alondra_daemon_engine::sdk_live::EndedWaits),
        Box::new(alondra_engine_sdk::task_closer::Unattended),
        Box::new(alondra_daemon_engine::allow_always::ChosenAlways),
        Box::new(alondra_engine_sdk::sdk_owner::Retakes),
    ]
}
