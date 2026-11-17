//! Why the Mac has no room for one more node, as the admission gate reports it.
use alondra_core::MemoryPressure;

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
