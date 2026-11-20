//! What the tests here and in the crates above build on: the nodes of the Agent SDK they drive, the
//! orchestrator's tools they call, and the runtime they open, as the test said on its thread with
//! `alondra_daemon_engine::wiring::open_with`.
use crate::domain_runtime::DomainRuntime;
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
