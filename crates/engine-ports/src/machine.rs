//! The Mac the daemon runs on, read through [`MachineProbe`] and sampled into [`Gauges`], and why it
//! has no room for one more node, as the admission gate reports it.
use alondra_core::{clock, Machine, MemoryPressure};
use alondra_foundation::locks::Poisonable as _;
use std::sync::Arc;
use std::time::Duration;

/// Where the gauges of the Mac come from: its memory, load and own disk in production, a fixed or
/// scripted Mac in a test or a scenario. It only measures; what to do with a sample is decided above.
pub trait MachineProbe: Send + Sync + 'static {
    /// One sample of the Mac, taken now.
    fn sample(&self) -> Machine;
    /// How many cores the Mac has and how many bytes of memory, which stay put while it runs.
    fn size(&self) -> (usize, u64);
}

const GIB: i64 = 1 << 30;
const MIB: u64 = 1 << 20;

/// Why one more node may not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoRoom {
    Unmeasured,
    /// macOS reports critical pressure.
    Pressure(MemoryPressure),
    /// Free memory, and what it must be for `nodes` more at `cost` each to leave `floor` free.
    Free { free: u32, needed: u32, floor: u32, nodes: usize, cost: i64 },
    /// What macOS swapped out in the last minute, in bytes, with `free` memory, `needed` to start.
    Swapping { swapped: u64, free: u32, needed: u32 },
    Available { available: u64, needed: u64 },
    SwapGrowth { used: u64, allocated: u64, swapped: u64 },
}

impl std::fmt::Display for NoRoom {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoRoom::Unmeasured => write!(out, "waiting for a valid system memory measurement"),
            NoRoom::Available { available, needed } => write!(out, "{available} bytes available, {needed} bytes needed including the memory floor and pending nodes"),
            NoRoom::SwapGrowth { used, allocated, swapped } => write!(out, "positive swap growth of {used} bytes used and {allocated} bytes allocated with {swapped} bytes swapped out; waiting for a stable minute"),
            NoRoom::Pressure(pressure) => write!(out, "macOS reports {pressure:?} memory pressure"),
            NoRoom::Free { free, needed, floor, nodes, cost } => write!(out, "{free}% free, {needed}% needed to keep {floor}% free with {nodes} node(s) at {:.2} GiB each", *cost as f64 / GIB as f64),
            NoRoom::Swapping { swapped, free, needed } => write!(out, "macOS swapped out {} MiB in the last minute with {free}% free, {needed}% needed while it swaps", swapped / MIB),
        }
    }
}

/// The latest gauges of the resource API daemon, read by `/daemon` for the TUI header. A sample is
/// only ever replaced whole, so a lock whose holder panicked still holds a whole one.
pub type Gauges = Arc<std::sync::Mutex<Option<(Machine, Option<alondra_core::usage::Usage>, Vec<alondra_core::usage::ProviderUsage>)>>>;

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
        let machine = clock::spawn_blocking(move || sampling.sample()).await.expect("machine sample worker failed");
        publish(machine);
        clock::sleep(Duration::from_secs(10)).await;
    }
}

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;
