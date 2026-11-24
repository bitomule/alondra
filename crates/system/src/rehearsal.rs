//! The Mac of an upgrade rehearsal. A rehearsal's daemons run on the machine of whoever runs it,
//! next to their builds and nodes, so the admission gate must not read that machine's disk,
//! memory or load: with `ALONDRA_REHEARSAL_MAC=1` the `Disk` and `MachineProbe` ports answer with a
//! Mac that always has room, and the gate itself stays the one production runs.
use alondra_core::{Machine, MemoryPressure, now_millis};
use alondra_engine_ports::disk::{Disk, Space};
use alondra_engine_ports::machine::MachineProbe;
use std::path::Path;

const GB: u64 = 1_000_000_000;
pub const ROOM: Space = Space { free: 400 * GB, purgeable: 0, total: 500 * GB };
const MEMORY: u64 = 64 * GB;
const CORES: usize = 10;

pub fn requested() -> bool {
    std::env::var("ALONDRA_REHEARSAL_MAC").is_ok_and(|value| value == "1")
}

pub struct RoomyDisk;

impl Disk for RoomyDisk {
    fn space(&self, path: &Path) -> Option<Space> {
        path.exists().then_some(ROOM)
    }
}

pub struct RoomyMac;

impl MachineProbe for RoomyMac {
    fn sample(&self) -> Machine {
        Machine {
            at: now_millis(),
            memory_total: MEMORY,
            memory_available: MEMORY,
            memory_free_percent: Some(100),
            memory_pressure: Some(MemoryPressure::Normal),
            load: [0.0; 3],
            cpus: CORES,
            disk_total: ROOM.total,
            disk_available: ROOM.free,
            ..Machine::default()
        }
    }

    fn size(&self) -> (usize, u64) {
        (CORES, MEMORY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_engine_ports::Registry;
    use alondra_engine_ports::disk::BOOT_DATA;

    fn wired(rehearsal: bool) -> Registry {
        let registry = Registry::default();
        crate::wire_with(&registry, std::path::PathBuf::new, rehearsal);
        registry
    }

    #[test]
    fn a_rehearsal_reads_a_mac_with_room_whatever_the_real_disk_and_memory_hold() {
        let registry = wired(true);
        assert_eq!(registry.disk().space(Path::new("/")), Some(ROOM));
        let sample = registry.machine().sample();
        assert_eq!((sample.disk_available, sample.memory_available, sample.memory_pressure), (ROOM.free, MEMORY, Some(MemoryPressure::Normal)));
    }

    #[test]
    fn without_the_rehearsal_the_ports_read_the_real_mac() {
        let registry = wired(false);
        let real = registry.disk().space(Path::new(BOOT_DATA)).or_else(|| registry.disk().space(Path::new("/"))).unwrap();
        assert_ne!(real, ROOM, "the real disk is not the rehearsal's");
        assert_ne!(registry.machine().size().1, MEMORY);
    }
}
