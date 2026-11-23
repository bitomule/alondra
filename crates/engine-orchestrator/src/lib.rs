//! The orchestrator: the process that runs the orchestrator agent over its inbox, its recovery and
//! its watchdog. It knows the engine only through `engine-kernel`: what it asks of the rest of the
//! engine goes through the ports of engine-ports, which `alondrad` connects when it puts the daemon
//! together.
#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

#[macro_use]
extern crate alondra_foundation;

use alondra_daemon_support::{auth, locks, watcher};
use alondra_engine_ports::plugins;

pub use alondra_engine_kernel::{domain_runtime, domain_store, metrics_capture, options, sdk_backend};

pub mod domain_orchestrator;
pub mod engine;
pub mod orchestrator_codex;
pub mod orchestrator_conversation;
pub mod orchestrator_inbox;
pub mod orchestrator_process;
pub mod orchestrator_provider;
pub mod orchestrator_sdk;
pub mod stall_context;

use alondra_engine_kernel::domain_runtime::DomainRuntime;

/// The runtime the orchestrator's tests open: the orchestrator's own port, the real agent hosts,
/// doubles of the rest of the engine, and the faults a test injects at its home failing the orchestrator's writes there.
#[cfg(test)]
mod wiring {
    use crate::engine::OrchestratorOf as _;
    use alondra_daemon_engine_model::faults;
    use alondra_engine_ports::hosts::SdkHosts;
    use alondra_engine_ports::orchestrator::OrchestratorEngine;
    use std::sync::Arc;

    pub fn open(home: &std::path::Path) -> anyhow::Result<super::DomainRuntime> {
        alondra_engine_kernel::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite);
        let domain = super::DomainRuntime::open_unwired_at(home, &crate::domain_store::active_path(home))?;
        alondra_engine_doubles::machine::wire(domain.ports(), alondra_daemon_support::watcher::claude_home);
        domain.ports().provide::<dyn OrchestratorEngine>(Arc::new(alondra_engine_doubles::orchestrator::Engine));
        domain.ports().provide::<dyn SdkHosts>(Arc::new(alondra_engine_doubles::hosts::NoHosts));
        alondra_agent_runtime::wire(domain.ports(), Default::default());
        let injected = &domain.orchestrator().injected;
        let at = home.to_owned();
        injected.fail_writes(move |point| faults::write(&at, point));
        let at = home.to_owned();
        injected.on_spawn(move |capability| faults::spawned(&at, capability));
        Ok(domain)
    }
}
