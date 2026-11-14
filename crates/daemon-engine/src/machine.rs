pub use alondra_daemon_engine_machine::machine::*;
use std::sync::Arc;

pub async fn run(gauges: Gauges, domain: Arc<crate::domain_runtime::DomainRuntime>) {
    alondra_daemon_engine_machine::machine::run(gauges, move |at| {
        let usage = crate::usage_probe::usage(&domain, at);
        let providers = domain.read().index().providers(at, &alondra_daemon_engine_model::local_day(at));
        (usage, providers)
    }).await
}
