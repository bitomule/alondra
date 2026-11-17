//! The engine's tests that need the gate itself, over the engine's test-hooks fixtures: every runtime
//! they open has the gate, as `alondrad` gives it.
pub use crate::AdmissionOf as _;
pub use alondra_daemon_engine::gate::GateOf as _;
pub use alondra_daemon_engine::live::LiveOf as _;
pub use alondra_daemon_engine::sdk_owner::SdkOwners as _;
pub use alondra_core::accounts::{AccountId, AccountKey, AccountState, AccountStatus, Provider};
pub use alondra_domain_rules::*;
pub use alondra_daemon_engine::domain_orchestrator_tools::{LaunchBody, Origin};
pub use alondra_daemon_engine::domain_runtime::DomainRuntime;
pub use alondra_daemon_engine::sdk_owner::tests::SdkLaunches;
pub use alondra_daemon_engine_model::LaunchPlan;
pub use alondra_domain::ActivityView;
pub use serde_json::{Value, json};
pub use std::collections::{BTreeMap, BTreeSet};
pub use std::path::{Path, PathBuf};
pub use std::sync::Arc;

mod auto_launch_tests;
mod codex_tests;
mod orchestrator_tools_tests;
mod release_tests;
mod schedule_runner_tests;
mod sdk_owner_tests;
mod transport_tests;
mod untaken_tests;

/// Gives every runtime the engine's fixtures open the gate.
pub fn wired() {
    alondra_daemon_engine::wiring::TEST_WIRING.get_or_init(|| crate::wire);
}
