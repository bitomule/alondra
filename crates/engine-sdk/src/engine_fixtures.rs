//! What the tests that open the whole engine build on: the nodes of the Agent SDK they drive, the
//! orchestrator's tools they call and the automatic launches they set up. No library carries them:
//! each test crate that uses them compiles this file as a module of its own, and gives it
//! `crate::engine_tests::wired`, which says how its threads open a runtime
//! (`alondra_daemon_engine::wiring::open_with`), `crate::engine_tests::SdkLaunches`, the launches
//! its hosts record, and `crate::test_support`.
use alondra_engine_sdk::domain_runtime::DomainRuntime;
use std::path::Path;

#[path = "auto_launch/fixtures.rs"]
pub mod launches;
#[path = "engine_fixtures/orchestrator_tools.rs"]
pub mod orchestrator_tools;
#[path = "sdk_owner/fixtures.rs"]
pub mod owner;
#[path = "engine_fixtures/sdk_node_fixture.rs"]
pub mod sdk_node_fixture;

pub fn open(home: &Path) -> anyhow::Result<DomainRuntime> {
    open_at(home, &alondra_engine_ports::journal::active_path(home))
}

pub fn open_at(home: &Path, database: &Path) -> anyhow::Result<DomainRuntime> {
    crate::engine_tests::wired();
    alondra_daemon_engine::wiring::open_at(home, database)
}
