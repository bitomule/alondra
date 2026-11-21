//! The tests of the engine's and the gate's modules that start nodes through the SDK, so they run
//! with this crate: each file is named after the module it tests.
#[cfg(test)]
mod accounts_tests;
mod orchestrator_tools_tests;
mod owner_tests;
#[cfg(test)]
mod pauses_tests;
mod system_command_tests;

/// The SQLite-backed fixtures of an SDK node's owner, which the engine's tests share.
#[path = "../../daemon-engine/src/wiring/owner_fixtures.rs"]
pub(crate) mod owner_fixtures;

/// Opens every runtime the engine's fixtures open on this thread as the daemon does, with this
/// crate's observers and ports.
pub fn wired() {
    alondra_engine_kernel::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite);
    alondra_daemon_engine::wiring::open_with(open_at);
}

/// The runtime as the daemon opens it, with the doubles of the Mac and a gate that is unpaced and
/// launches on the disk the machine watch measured.
pub fn open_at(home: &std::path::Path, database: &std::path::Path, files: &dyn alondra_engine_ports::journal::JournalFiles) -> anyhow::Result<crate::domain_runtime::DomainRuntime> {
    let runtime = crate::engine_fixtures::open_engine_at(home, database, files)?;
    alondra_engine_doubles::machine::wire(runtime.ports(), alondra_daemon_support::watcher::claude_home);
    alondra_agent_runtime::wire(runtime.ports(), Default::default());
    alondra_engine_admission::for_tests(&runtime);
    Ok(runtime)
}
