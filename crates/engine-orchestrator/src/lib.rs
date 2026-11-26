//! The orchestrator: the process that runs the orchestrator agent over its inbox, its recovery and
//! its watchdog. It knows the engine only through `engine-kernel`: what it asks of the rest of the
//! engine goes through the ports of engine-ports, which `alondrad` connects when it puts the daemon
//! together.
#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

#[macro_use]
extern crate alondra_foundation;

use alondra_daemon_support::{locks, watcher};
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

/// Gives the registry the orchestrator as the server shows and steers it.
pub fn wire(registry: &alondra_engine_ports::Registry) {
    registry.provide::<dyn alondra_engine_ports::domain_api::OrchestratorApi>(std::sync::Arc::new(domain_orchestrator::Api));
}

/// The runtime the orchestrator's tests open: the orchestrator's own port, the doubled agent hosts
/// the host models play, doubles of the rest of the engine, and the faults a test injects at its home failing the orchestrator's writes there.
#[cfg(test)]
mod wiring {
    use crate::engine::OrchestratorOf as _;
    use alondra_daemon_engine_model::faults;
    use alondra_engine_ports::hosts::SdkHosts;
    use alondra_engine_ports::orchestrator::OrchestratorEngine;
    use std::sync::Arc;

    pub fn open(home: &std::path::Path) -> anyhow::Result<super::DomainRuntime> {
        alondra_foundation::durability::relax();
        alondra_engine_kernel::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite(&alondra_domain_rules::Rules));
        let domain = super::DomainRuntime::open_unwired_at(home, &crate::domain_store::active_path(home))?;
        alondra_engine_doubles::machine::wire(domain.ports(), alondra_daemon_support::watcher::claude_home);
        domain.ports().provide::<dyn OrchestratorEngine>(Arc::new(alondra_engine_doubles::orchestrator::Engine));
        domain.ports().provide::<dyn SdkHosts>(Arc::new(alondra_engine_doubles::hosts::NoHosts));
        provide_hosts(&domain, home);
        let injected = &domain.orchestrator().injected;
        let at = home.to_owned();
        injected.fail_writes(move |point| faults::write(&at, point));
        let at = home.to_owned();
        injected.on_spawn(move |capability| faults::spawned(&at, capability));
        Ok(domain)
    }

    /// The doubled hosts a fixture serves at each home, held weakly so they end with the fixture.
    static SERVED: std::sync::Mutex<Vec<(std::path::PathBuf, std::sync::Weak<alondra_engine_doubles::agent_host::InProcessHosts>)>> = std::sync::Mutex::new(Vec::new());

    /// Every runtime opened at `home` from now on gets `hosts`, as a restarted daemon finds the
    /// hosts the one before left.
    pub fn serve(home: &std::path::Path, hosts: &Arc<alondra_engine_doubles::agent_host::InProcessHosts>) {
        let mut served = SERVED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        served.retain(|(at, kept)| at != home && kept.strong_count() > 0);
        served.push((home.to_owned(), Arc::downgrade(hosts)));
    }

    /// The hosts a fixture serves at `home`, or doubled hosts of its own that nothing plays. No
    /// doubled host is a process of the kernel: each is born at the same moment for the batch that
    /// records it.
    fn provide_hosts(domain: &super::DomainRuntime, home: &std::path::Path) {
        let served = SERVED.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter().find(|(at, _)| at == home).and_then(|(_, kept)| kept.upgrade());
        match served {
            Some(hosts) => hosts.provide(domain.ports()),
            None => drop(alondra_engine_doubles::agent_host::wire(domain.ports())),
        }
        domain.orchestrator().injected.date_hosts(|_| Some(1));
    }
}
