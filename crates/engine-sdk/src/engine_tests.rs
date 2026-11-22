//! The tests of the engine's and the gate's modules that start nodes through the SDK, so they run
//! with this crate: each file is named after the module it tests.
#[cfg(all(test, feature = "real-hosts"))]
mod accounts_tests;
#[cfg(all(test, feature = "whole-engine", not(feature = "real-hosts")))]
mod orchestrator_tools_tests;
#[cfg(all(test, feature = "whole-engine", not(feature = "real-hosts")))]
mod owner_tests;
#[cfg(all(test, feature = "real-hosts"))]
mod pauses_tests;
#[cfg(all(test, feature = "whole-engine", not(feature = "real-hosts")))]
mod system_command_tests;

/// The SQLite-backed fixtures of an SDK node's owner, which the engine's tests share.
#[cfg(all(test, feature = "whole-engine", not(feature = "real-hosts")))]
#[path = "../../daemon-engine/src/wiring/owner_fixtures.rs"]
pub(crate) mod owner_fixtures;

#[cfg(not(feature = "real-hosts"))]
pub use crate::model_fixture::SdkLaunches;
#[cfg(feature = "real-hosts")]
pub use crate::engine_fixtures::owner::SdkLaunches;

/// Opens every runtime the engine's fixtures open on this thread as the daemon does, with this
/// crate's observers and ports.
pub fn wired() {
    alondra_foundation::durability::relax();
    alondra_engine_kernel::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite);
    alondra_daemon_engine::wiring::open_with(open_at);
}

/// The runtime as the daemon opens it, with the doubles of the Mac and a gate that is unpaced and
/// launches on the disk the machine watch measured.
pub fn open_at(home: &std::path::Path, database: &std::path::Path, files: &dyn alondra_engine_ports::journal::JournalFiles) -> anyhow::Result<crate::domain_runtime::DomainRuntime> {
    let runtime = crate::engine_assembly::open_engine_at(home, database, files)?;
    alondra_engine_doubles::machine::wire(runtime.ports(), alondra_daemon_support::watcher::claude_home);
    #[cfg(feature = "real-hosts")]
    alondra_agent_runtime::wire(runtime.ports(), Default::default());
    #[cfg(not(feature = "real-hosts"))]
    alondra_engine_doubles::agent_host::wire(runtime.ports());
    alondra_engine_admission::for_tests(&runtime);
    Ok(runtime)
}
