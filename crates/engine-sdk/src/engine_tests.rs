//! The tests of the engine's and the gate's modules that start nodes through the SDK, so they run
//! with this crate: each file is named after the module it tests.
mod accounts_tests;
pub mod orchestrator_tools_tests;
mod owner_tests;
mod pauses_tests;
mod system_command_tests;

/// Opens every runtime the engine's fixtures open as the daemon does, with this crate's observers
/// and ports.
pub fn wired() {
    alondra_daemon_engine::wiring::TEST_OPEN.get_or_init(|| open_at);
}

/// The runtime as the daemon opens it, with the gate of a test: unpaced, its launches on the disk
/// the machine watch measured.
pub fn open_at(home: &std::path::Path, database: &std::path::Path) -> anyhow::Result<crate::domain_runtime::DomainRuntime> {
    let runtime = crate::wiring::open_at(home, database)?;
    alondra_engine_admission::for_tests(&runtime);
    Ok(runtime)
}

/// `open_at` at the home's active database.
pub fn open(home: &std::path::Path) -> anyhow::Result<crate::domain_runtime::DomainRuntime> {
    open_at(home, &crate::domain_database_migration::active_path(home))
}
