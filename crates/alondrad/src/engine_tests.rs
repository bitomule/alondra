//! Tests of the engine that go through alondrad's routes, views or delivery. The engine crate
//! cannot see those, so these run here, on the fixtures its `test-hooks` feature exposes.
mod acceptance_catalog_tests;
mod domain_github_tests;
mod domain_orchestrator_tools_tests;
mod domain_store_tests;
mod domain_transport_tests;
mod launch_plans_tests;
mod load_bench_tests;
mod sdk_owner_tests;
mod task_list_tests;

/// Gives every runtime the engine's fixtures open the admission gate, as the daemon does.
fn wired() {
    alondra_daemon_engine::wiring::TEST_WIRING.get_or_init(|| alondra_engine_admission::wire);
}
