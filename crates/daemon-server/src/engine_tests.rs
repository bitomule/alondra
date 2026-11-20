//! Tests of the engine that go through alondrad's routes, views or delivery. The engine crate
//! cannot see those, so these run here, on the fixtures its `test-hooks` feature exposes.
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

/// Opens every runtime the engine's fixtures open as the daemon does, with the gate of a test.
pub(crate) fn wired() {
    alondra_engine_kernel::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite);
    alondra_daemon_engine::wiring::TEST_OPEN.get_or_init(|| alondra_engine_sdk::engine_tests::open_at);
}
