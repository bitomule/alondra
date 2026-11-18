//! The Mac a test's runtime reaches: the doubled git, disks and gauges, never the real ones.
use alondra_core::Machine;
use alondra_engine_ports::Registry;
use alondra_engine_ports::disk::Disk;
use alondra_engine_ports::machine::MachineProbe;
use alondra_engine_ports::worktrees::Worktrees;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const GIB: u64 = 1 << 30;

/// Gives `registry` the doubled git, which finds sessions' worktrees in the Claude folder
/// `claude_home` names, the doubled disks and a calm Mac.
pub fn wire(registry: &Registry, claude_home: fn() -> PathBuf) {
    registry.provide::<dyn Worktrees>(Arc::new(crate::worktrees::Worktrees::new(claude_home)));
    registry.provide::<dyn Disk>(Arc::new(crate::disk::Disks::default()));
    registry.provide::<dyn MachineProbe>(Arc::new(CalmMac::default()));
}

/// A Mac always rested, with room for everything: 8 cores, 16 GiB of memory, 12 of them available,
/// and a 1 TiB disk half free. It counts the samples taken of it.
#[derive(Clone, Default)]
pub struct CalmMac(Arc<AtomicU64>);

impl CalmMac {
    pub const CORES: usize = 8;
    pub const MEMORY: u64 = 16 * GIB;

    /// How many samples were taken of it.
    pub fn samples(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

impl MachineProbe for CalmMac {
    fn sample(&self) -> Machine {
        self.0.fetch_add(1, Ordering::SeqCst);
        Machine {
            at: alondra_core::now_millis(),
            memory_total: Self::MEMORY,
            memory_available: 12 * GIB,
            memory_free_percent: Some(75),
            memory_pressure: None,
            compressor: None,
            load: [0.5; 3],
            cpus: Self::CORES,
            cpu_usage: 5.0,
            disk_total: 1024 * GIB,
            disk_available: 512 * GIB,
            disk_purgeable: 0,
            unreliable: false,
        }
    }

    fn size(&self) -> (usize, u64) {
        (Self::CORES, Self::MEMORY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_calm_mac_keeps_the_contract_the_macs_gauges_keep() {
        crate::contract::machine(&CalmMac::default());
    }

    #[test]
    fn the_calm_mac_counts_the_samples_taken_of_it() {
        let mac = CalmMac::default();
        let seen = mac.clone();
        mac.sample();
        mac.sample();
        assert_eq!(seen.samples(), 2);
    }
}
