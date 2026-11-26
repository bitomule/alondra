//! Launches through the SDK with no process behind them: the domain's hosts are the doubles of
//! `engine-doubles`; the Claude host model of `provider-claude` plays each Claude host and every
//! probe, and the Codex host model of `provider-codex` each Codex host. It reads what each native
//! Claude was started with and the prompts it took as the fake native Claude records them, and the
//! Codex model writes the native requests where the fake native Codex writes them, so a test reads
//! the same thing whichever hosts it runs on. The launches (`launch_fixture`) are shared with the tests
//! of `daemon-server`; the hosts of `engine_fixtures::owner::Slice` they play are
//! `engine_fixtures::model_hosts`.
mod launch_fixture;
pub use launch_fixture::*;

/// A runtime of this crate's own pieces with doubles of the rest of the engine: the Mac, the agent
/// hosts, a gate that holds nothing back, what the gate and the orchestrator ask of the engine, and
/// the part of `SdkEngine` a launch asks (`alondra_engine_doubles::sdk`).
#[cfg(not(feature = "whole-engine"))]
pub fn open(home: &std::path::Path) -> anyhow::Result<crate::domain_runtime::DomainRuntime> {
    use crate::domain_runtime::DomainRuntime;
    use alondra_engine_ports::admission::AdmissionGate;
    use alondra_engine_ports::orchestrator::OrchestratorEngine;
    use std::sync::Arc;
    alondra_foundation::durability::relax();
    let files = *alondra_engine_kernel::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite(&alondra_domain_rules::Rules));
    let observers: Vec<Box<dyn crate::domain_runtime::CommitObserver>> = vec![Box::new(crate::task_closer::Unattended), Box::new(crate::sdk_owner::Retakes)];
    let runtime = DomainRuntime::open_observed(home, &alondra_engine_ports::journal::active_path(home), files, None, observers)?;
    let ports = runtime.ports();
    crate::wiring::wire(ports);
    alondra_engine_doubles::machine::wire(ports, alondra_daemon_support::watcher::claude_home);
    alondra_engine_doubles::agent_host::wire(ports);
    alondra_provider_claude::wire(ports);
    alondra_provider_codex::wire(ports);
    alondra_provider_antigravity::wire(ports);
    ports.provide::<dyn AdmissionGate>(Arc::new(alondra_engine_doubles::gate::OpenGate::default()));
    alondra_engine_doubles::admission::Engine::wire(ports);
    ports.provide::<dyn OrchestratorEngine>(Arc::new(alondra_engine_doubles::orchestrator::Engine));
    alondra_engine_doubles::sdk::Engine::wire(ports);
    Ok(runtime)
}
