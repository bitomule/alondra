//! The orchestrator: the process that runs the orchestrator agent over its inbox, its recovery and
//! its watchdog. It knows the engine only through `engine-kernel`: what it asks of the rest of the
//! engine goes through the kernel's ports, which `alondrad` connects when it puts the daemon
//! together.
#![cfg_attr(all(feature = "test-hooks", not(test)), allow(dead_code, unused_imports))]

#[cfg(any(test, feature = "test-hooks"))]
#[path = "../../test_support.rs"]
pub mod test_support;

#[macro_use]
extern crate alondra_daemon_support;

use alondra_daemon_support::{auth, locks, plugins, watcher};
#[cfg(test)]
use alondra_daemon_engine::{accounts, live, usage_probe};

pub use alondra_engine_kernel::{domain_database_migration, domain_runtime, domain_store, metrics_capture, options, sdk_backend};

pub mod domain_orchestrator;
pub mod engine;
pub mod orchestrator_codex;
pub mod orchestrator_inbox;
pub mod orchestrator_process;
pub mod orchestrator_provider;
pub mod orchestrator_sdk;
pub mod stall_context;

use alondra_engine_kernel::domain_runtime::DomainRuntime;
use alondra_engine_kernel::ports::OrchestratorAnswers;
use std::sync::Arc;

/// Gives `domain` what the rest of the engine asks of the orchestrator.
pub fn wire(domain: &DomainRuntime) {
    domain.provide::<dyn OrchestratorAnswers>(Arc::new(orchestrator_inbox::Answers));
}

/// The engine as `alondrad` puts it together, for the orchestrator's tests.
#[cfg(test)]
mod wiring {
    pub fn open(home: &std::path::Path) -> anyhow::Result<super::DomainRuntime> {
        let domain = alondra_daemon_engine::wiring::open(home)?;
        crate::wire(&domain);
        Ok(domain)
    }
}
