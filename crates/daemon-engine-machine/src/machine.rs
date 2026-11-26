use alondra_core::clock;
use crate::locks::Poisonable as _;
use std::sync::Arc;
use std::time::Duration;

use alondra_core::Machine;
pub use alondra_engine_ports::disk::BOOT_DATA;
pub use alondra_engine_ports::machine::{Gauges, MachineProbe};


/// The plan's limits travel with the machine's gauges, read from what each account measured.
pub async fn run(
    gauges: Gauges,
    probe: Arc<dyn MachineProbe>,
    usage_at: impl Fn(alondra_core::Millis) -> (Option<alondra_core::usage::Usage>, Vec<alondra_core::usage::ProviderUsage>),
    published: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    sample_loop(probe, |machine| {
        let (usage, providers) = usage_at(machine.at);
        *gauges.lock_or_recover("the machine gauges") = Some((machine, usage, providers));
        if let Some(published) = &published {
            published();
        }
    }).await
}

async fn sample_loop(probe: Arc<dyn MachineProbe>, mut publish: impl FnMut(Machine)) {
    loop {
        let sampling = probe.clone();
        let machine = alondra_core::clock::spawn_blocking(move || sampling.sample()).await.expect("machine sample worker failed");
        publish(machine);
        clock::sleep(Duration::from_secs(10)).await;
    }
}

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;
