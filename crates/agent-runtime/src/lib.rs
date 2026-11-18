#![forbid(unsafe_code)]
//! Owned local host processes. Readiness wakeups and child exit are technical observations.
pub mod owned_host;

use alondra_engine_ports::Registry;
use alondra_engine_ports::agent_host::AgentHost;
use std::sync::Arc;

/// Provides the port `AgentHost`: hosts start through `launcher`.
pub fn wire(registry: &Registry, launcher: owned_host::Launcher) {
    registry.provide::<dyn AgentHost>(Arc::new(owned_host::ProcessHosts::new(launcher)));
}

#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "../tests/connect.rs"]
mod connect_tests;
