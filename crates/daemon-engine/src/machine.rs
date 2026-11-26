pub use alondra_daemon_engine_machine::machine::*;
use alondra_engine_ports::Runtime;
use std::sync::Arc;

/// What the gauges show of the accounts at `at`: their usage, and what each provider has left.
fn usage_at(domain: &dyn Runtime, at: alondra_core::Millis) -> (Option<alondra_core::usage::Usage>, Vec<alondra_core::usage::ProviderUsage>) {
    let usage = crate::usage_probe::usage(domain, at);
    let providers = domain.read().index().providers(at, &alondra_daemon_engine_model::local_day(at));
    (usage, providers)
}

/// Samples the Mac through the registry's `MachineProbe` into `gauges`.
pub async fn run(gauges: Gauges, domain: Arc<dyn Runtime>, published: Option<Arc<dyn Fn() + Send + Sync>>) {
    let probe = domain.ports().machine();
    alondra_daemon_engine_machine::machine::run(gauges, probe, move |at| usage_at(&*domain, at), published).await
}

/// Reads again, from what the domain holds now, the accounts' part of the latest sample. A sample
/// takes its accounts when the sampler publishes it, and a probe that woke at the same instant of
/// the clock commits before or after that, as the threads happen to run: a scenario that has
/// settled calls this to see what the daemon holds, not what won that race.
pub fn resample(gauges: &Gauges, domain: &dyn Runtime) {
    let at = gauges.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().map(|(machine, _, _)| machine.at);
    let Some(at) = at else { return };
    let (usage, providers) = usage_at(domain, at);
    if let Some((_, kept_usage, kept_providers)) = gauges.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut() {
        (*kept_usage, *kept_providers) = (usage, providers);
    }
}

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;
