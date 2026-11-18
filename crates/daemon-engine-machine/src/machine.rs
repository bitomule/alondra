use alondra_core::clock;
use crate::locks::Poisonable as _;
use std::sync::Arc;
use std::time::Duration;

use alondra_core::Machine;
pub use alondra_engine_ports::disk::BOOT_DATA;
pub use alondra_engine_ports::machine::MachineProbe;

/// The latest gauges of the resource API daemon, read by `/daemon` for the TUI header. A sample is
/// only ever replaced whole, so a lock whose holder panicked still holds a whole one.
pub type Gauges = Arc<std::sync::Mutex<Option<(Machine, Option<alondra_core::usage::Usage>, Vec<alondra_core::usage::ProviderUsage>)>>>;

/// The plan's limits travel with the machine's gauges, read from what each account measured.
pub async fn run(
    gauges: Gauges,
    probe: Arc<dyn MachineProbe>,
    usage_at: impl Fn(alondra_core::Millis) -> (Option<alondra_core::usage::Usage>, Vec<alondra_core::usage::ProviderUsage>),
) {
    sample_loop(probe, |machine| {
        let (usage, providers) = usage_at(machine.at);
        *gauges.lock_or_recover("the machine gauges") = Some((machine, usage, providers));
    }).await
}

async fn sample_loop(probe: Arc<dyn MachineProbe>, mut publish: impl FnMut(Machine)) {
    loop {
        let sampling = probe.clone();
        let machine = tokio::task::spawn_blocking(move || sampling.sample()).await.expect("machine sample worker failed");
        publish(machine);
        clock::sleep(Duration::from_secs(10)).await;
    }
}
