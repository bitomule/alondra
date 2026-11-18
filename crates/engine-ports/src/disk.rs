//! Free disk space, measured twice. `free` is `statvfs`'s blocks available to an unprivileged
//! user, the number `df` prints: what a write can use right now. `purgeable` is what macOS's
//! "available for important usage" adds on top, mostly Time Machine's local snapshots and caches,
//! which macOS deletes by itself when space runs short, though not always in time: on 7 October a
//! build filled the disk while that number still showed tens of gigabytes. So the disk is low when
//! the space counting purgeable is short, or when the space free right now is under a floor.
//!
//! A new node starts only with room for its build on top of what the nodes at work may still take
//! (`disk_peaks`), above the low line, so the builds never take the disk under it. Under the floor
//! (`under_floor`), lower than the low line, the estimates fell short: the nodes' next turns wait.
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Where a Mac keeps its writable data; `/` on any other system.
pub const BOOT_DATA: &str = "/System/Volumes/Data";
pub const LOW_DISK_BYTES: u64 = 5_000_000_000;
const FLOOR_DISK_BYTES: u64 = 2_000_000_000;
const RECOVERY_MARGIN_BYTES: u64 = 2_000_000_000;
const FLOOR_RECOVERY_MARGIN_BYTES: u64 = 1_000_000_000;
const FLOOR_USABLE_BYTES: u64 = 3_000_000_000;
const FLOOR_FREE_BYTES: u64 = 1_500_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Space {
    pub free: u64,
    pub purgeable: u64,
    pub total: u64,
}

impl Space {
    pub fn usable(&self) -> u64 {
        self.free.saturating_add(self.purgeable)
    }

    /// Low by the line it was measured against: little usable space, or little free right now.
    pub fn low(&self, was_low: bool) -> bool {
        let lines = if was_low { recovered_lines() } else { low_lines() };
        self.usable() < lines.usable || self.free < lines.free
    }

    /// Low only because macOS has not freed the purgeable space yet.
    pub fn short_now(&self) -> bool {
        self.purgeable > 0 && self.free < low_lines().free && self.usable() >= low_lines().usable
    }

    /// The free space the user is told: what a write can use right now when macOS has not freed
    /// its purgeable space yet, else the usable space.
    pub fn shown_free(&self) -> u64 {
        if self.short_now() { self.free } else { self.usable() }
    }

    /// Under the floor nodes at work must not take the disk past: little usable space, or little
    /// free right now.
    pub fn under_floor(&self) -> bool {
        self.usable() < FLOOR_USABLE_BYTES || self.free < FLOOR_FREE_BYTES
    }

    /// Whether one more build fits, `needed` counting purgeable space; once it did not
    /// (`was_short`) it needs margin on both lines to fit again.
    pub fn room_for(&self, needed: u64, was_short: bool) -> Result<(), Room> {
        let (usable, free) = if was_short { (needed + RECOVERY_MARGIN_BYTES, recovered_lines().free) } else { (needed, low_lines().free) };
        if self.usable() < usable || self.free < free {
            return Err(Room { free: self.shown_free(), needed, volume: None });
        }
        Ok(())
    }
}

/// The disk short of room for one more build: the free space the user is told, what is needed,
/// and the volume when it is not the Mac's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Room {
    pub free: u64,
    pub needed: u64,
    pub volume: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lines {
    pub usable: u64,
    pub free: u64,
}

#[derive(Debug)]
pub struct Mounts {
    pub all: BTreeSet<PathBuf>,
    pub candidates: Vec<PathBuf>,
}

/// Below these lines Alondra starts no node and puts a card in front of the user.
/// `ALONDRA_LOW_DISK_BYTES` and `ALONDRA_LOW_DISK_FLOOR_BYTES` move them, so a sandbox can try the
/// guard on a healthy disk.
pub fn low_lines() -> Lines {
    static LINES: OnceLock<Lines> = OnceLock::new();
    let bytes = |name: &str, default: u64| std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default);
    *LINES.get_or_init(|| Lines { usable: bytes("ALONDRA_LOW_DISK_BYTES", LOW_DISK_BYTES), free: bytes("ALONDRA_LOW_DISK_FLOOR_BYTES", FLOOR_DISK_BYTES) })
}

/// Once low, the disk must climb past these lines to count as fine, so it does not flap at them.
pub fn recovered_lines() -> Lines {
    let low = low_lines();
    Lines { usable: low.usable + RECOVERY_MARGIN_BYTES, free: low.free + FLOOR_RECOVERY_MARGIN_BYTES }
}

/// Whether a write failed because the disk, or the user's quota on it, had no room left.
pub fn io_no_space(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::StorageFull || matches!(error.raw_os_error(), Some(libc::ENOSPC | libc::EDQUOT))
}


/// The disks of the Mac, as the engine reads them.
pub trait Disk: Send + Sync {
    /// The space of the volume `path` is on.
    fn space(&self, path: &Path) -> Option<Space>;
    /// What the volume of `path` itself holds, `df`'s Used.
    fn held(&self, path: &Path) -> Option<u64>;
    /// The top of the volume `path` is on.
    fn mount_point(&self, path: &Path) -> Option<PathBuf>;
    fn same_volume(&self, a: &Path, b: &Path) -> bool;
    /// Every mounted volume, with those builds may write to apart.
    fn mounted_volumes(&self) -> io::Result<Mounts>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_disk_or_quota_is_told_from_other_failures() {
        assert!(io_no_space(&io::Error::from_raw_os_error(libc::ENOSPC)));
        assert!(io_no_space(&io::Error::from_raw_os_error(libc::EDQUOT)));
        assert!(!io_no_space(&io::Error::from_raw_os_error(libc::EACCES)));
    }

    const GB: u64 = 1_000_000_000;

    fn disk(free: f64, purgeable: f64) -> Space {
        Space { free: (free * GB as f64) as u64, purgeable: (purgeable * GB as f64) as u64, total: 500 * GB }
    }


    #[test]
    fn purgeable_space_counts_as_usable_while_enough_is_free_right_now() {
        let snapshots = disk(4.8, 16.6);
        assert_eq!(snapshots.usable(), 21_400_000_000);
        assert!(!snapshots.low(false), "15 October: Time Machine snapshots held the space; no card");
        assert!(disk(4.8, 0.0).low(false), "without purgeable space the same free space is low");
    }

    #[test]
    fn a_disk_with_little_free_right_now_is_low_however_much_macos_could_purge() {
        let unpurged = disk(1.0, 14.0);
        assert!(unpurged.low(false), "7 October: the disk filled while macOS still counted 14 GB purgeable");
        assert!(unpurged.short_now());
        assert!(!disk(1.0, 2.0).short_now(), "little usable space: low for the usual reason");
        assert!(disk(3.0, 1.0).low(false) && !disk(3.0, 1.0).short_now());
    }

    #[test]
    fn once_low_the_disk_needs_margin_on_both_lines_to_count_as_fine() {
        assert!(!disk(4.0, 2.5).low(false));
        assert!(disk(4.0, 2.5).low(true), "6.5 GB usable is under the recovery line");
        assert!(disk(2.5, 10.0).low(true), "2.5 GB free is under the recovered floor");
        assert!(!disk(3.0, 4.5).low(true));
    }

    #[test]
    fn the_floor_is_under_the_low_line_and_counts_purgeable_space_once() {
        assert!(!disk(4.0, 0.0).under_floor(), "low, not under the floor");
        assert!(disk(2.9, 0.0).under_floor());
        assert!(!disk(2.0, 8.0).under_floor(), "purgeable space counts while enough is free right now");
        assert!(disk(1.4, 20.0).under_floor(), "little free right now, however much macOS could purge");
    }

    #[test]
    fn a_disk_short_of_room_for_a_build_needs_margin_to_have_it_again() {
        let needed = 13 * GB;
        assert_eq!(disk(12.0, 0.0).room_for(needed, false), Err(Room { free: 12 * GB, needed, volume: None }));
        assert_eq!(disk(4.0, 10.0).room_for(needed, false), Ok(()), "purgeable space counts");
        assert_eq!(disk(1.0, 30.0).room_for(needed, false), Err(Room { free: GB, needed, volume: None }), "little free right now: told what a write can use");
        assert!(disk(14.0, 0.0).room_for(needed, true).is_err(), "under the 2 GB margin once short");
        assert_eq!(disk(15.0, 0.0).room_for(needed, true), Ok(()));
    }

}
