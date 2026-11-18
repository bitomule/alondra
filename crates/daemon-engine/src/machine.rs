pub use alondra_daemon_engine_machine::machine::*;
use std::sync::Arc;

/// Samples the Mac through the registry's `MachineProbe` into `gauges`.
pub async fn run(gauges: Gauges, domain: Arc<crate::domain_runtime::DomainRuntime>, published: Option<Arc<dyn Fn() + Send + Sync>>) {
    let probe = domain.ports().machine();
    alondra_daemon_engine_machine::machine::run(gauges, probe, move |at| {
        let usage = crate::usage_probe::usage(&domain, at);
        let providers = domain.read().index().providers(at, &alondra_daemon_engine_model::local_day(at));
        (usage, providers)
    }, published).await
}
