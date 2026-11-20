//! Tests of the engine that go through alondrad's routes, views or delivery. The engine crate
//! cannot see those, so these run here, on the fixtures the engine's crates expose.
mod acceptance_catalog_tests;
mod admission_tests;
mod domain_github_tests;
mod domain_orchestrator_tools_tests;
mod domain_store_tests;
mod domain_transport_tests;
mod launch_plans_tests;
mod load_bench_tests;
mod orchestrator_accounts_tests;
mod orchestrator_inbox_tests;
mod pauses_tests;
mod retention_tests;
mod sdk_owner_tests;
mod task_list_tests;
mod usage_pause_tests;

/// The SQLite-backed fixtures of an SDK node's owner, which the engine's tests share.
#[path = "../../daemon-engine/src/wiring/owner_fixtures.rs"]
pub(crate) mod owner_fixtures;

/// Opens every runtime the engine's fixtures open on this thread as the daemon does, with the gate
/// of a test.
pub(crate) fn wired() {
    alondra_engine_kernel::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite);
    alondra_daemon_engine::wiring::open_with(open_at);
}

/// The runtime as the daemon opens it, with the doubles of the Mac and a gate that is unpaced and
/// launches on the disk the machine watch measured.
pub(crate) fn open_at(home: &std::path::Path, database: &std::path::Path, files: &dyn alondra_engine_ports::journal::JournalFiles) -> anyhow::Result<alondra_engine_sdk::domain_runtime::DomainRuntime> {
    let runtime = alondra_engine_sdk::wiring::open_at(home, database, files)?;
    alondra_engine_doubles::machine::wire(runtime.ports(), alondra_daemon_support::watcher::claude_home);
    alondra_agent_runtime::wire(runtime.ports(), Default::default());
    alondra_engine_admission::for_tests(&runtime);
    Ok(runtime)
}

/// `open_at` at the home's active database; the engine's fixtures this thread opens open the same.
pub(crate) fn open(home: &std::path::Path) -> anyhow::Result<alondra_engine_sdk::domain_runtime::DomainRuntime> {
    wired();
    open_at(home, &alondra_engine_ports::journal::active_path(home), &alondra_store_sqlite::Sqlite)
}

