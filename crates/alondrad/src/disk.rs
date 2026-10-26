//! Free disk space, measured twice. `free` is `statvfs`'s blocks available to an unprivileged
//! user, the number `df` prints: what a write can use right now. `purgeable` is what macOS's
//! "available for important usage" adds on top, mostly Time Machine's local snapshots and caches,
//! which macOS deletes by itself when space runs short, though not always in time: on 7 October a
//! build filled the disk while that number still showed tens of gigabytes. So the disk is low when
//! the space counting purgeable is short, or when the space free right now is under a floor.
//!
//! A new node starts only with room for one more build: what a typical task of this Mac took, the
//! space deleting its worktree freed, on top of the low line, so the build never takes the disk
//! under it.
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub(crate) const LOW_DISK_BYTES: u64 = 5_000_000_000;
pub(crate) const FLOOR_DISK_BYTES: u64 = 2_000_000_000;
const RECOVERY_MARGIN_BYTES: u64 = 2_000_000_000;
const FLOOR_RECOVERY_MARGIN_BYTES: u64 = 1_000_000_000;
/// A typical build while none was measured yet.
pub(crate) const UNMEASURED_BUILD_BYTES: u64 = 5_000_000_000;
/// The builds the typical one is taken from, the latest.
pub(crate) const BUILDS_KEPT: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Space {
    pub free: u64,
    pub purgeable: u64,
    pub total: u64,
}

impl Space {
    pub(crate) fn usable(&self) -> u64 {
        self.free.saturating_add(self.purgeable)
    }

    /// Low by the line it was measured against: little usable space, or little free right now.
    pub(crate) fn low(&self, was_low: bool) -> bool {
        let lines = if was_low { recovered_lines() } else { low_lines() };
        self.usable() < lines.usable || self.free < lines.free
    }

    /// Low only because macOS has not freed the purgeable space yet.
    pub(crate) fn short_now(&self) -> bool {
        self.purgeable > 0 && self.free < low_lines().free && self.usable() >= low_lines().usable
    }

    /// The free space the user is told: what a write can use right now when macOS has not freed
    /// its purgeable space yet, else the usable space.
    pub(crate) fn shown_free(&self) -> u64 {
        if self.short_now() { self.free } else { self.usable() }
    }

    /// Whether one more build fits, `needed` counting purgeable space; once it did not
    /// (`was_short`) it needs margin on both lines to fit again.
    pub(crate) fn room_for(&self, needed: u64, was_short: bool) -> Result<(), Room> {
        let (usable, free) = if was_short { (needed + RECOVERY_MARGIN_BYTES, recovered_lines().free) } else { (needed, low_lines().free) };
        if self.usable() < usable || self.free < free {
            return Err(Room { free: self.shown_free(), needed });
        }
        Ok(())
    }
}

/// The disk short of room for one more build: the free space the user is told and what is needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Room {
    pub free: u64,
    pub needed: u64,
}

/// The space one more build needs: the median of the builds measured, or
/// `UNMEASURED_BUILD_BYTES` before any, on top of the low line.
pub(crate) fn needed_for_build(builds: &[u64]) -> u64 {
    let mut sorted = builds.to_vec();
    sorted.sort_unstable();
    let typical = sorted.get(sorted.len() / 2).copied().unwrap_or(UNMEASURED_BUILD_BYTES);
    typical + low_lines().usable
}

/// The space the files under `path` take on disk, links not followed and a file with several
/// hard links counted once.
#[derive(Debug, Default)]
pub(crate) struct Tally {
    bytes: u64,
    linked: std::collections::HashSet<(u64, u64)>,
}

impl Tally {
    /// Counts a file met before deleting it: once one of its links is deleted the others count
    /// one link fewer, so every file with more than one is remembered.
    pub(crate) fn add(&mut self, meta: &std::fs::Metadata) {
        let file = (meta.dev(), meta.ino());
        if self.linked.contains(&file) {
            return;
        }
        if meta.nlink() > 1 && !meta.is_dir() {
            self.linked.insert(file);
        }
        self.bytes += meta.blocks() * 512;
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Lines {
    pub usable: u64,
    pub free: u64,
}

pub(crate) fn space(path: &Path) -> Option<Space> {
    let free = free_space(path)?;
    let purgeable = platform::usable(path).map_or(0, |usable| usable.saturating_sub(free.free));
    Some(Space { purgeable, ..free })
}

fn free_space(path: &Path) -> Option<Space> {
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    let stats = unsafe { stats.assume_init() };
    let block = stats.f_frsize as u64;
    Some(Space { free: stats.f_bavail as u64 * block, purgeable: 0, total: stats.f_blocks as u64 * block })
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2_foundation::{NSNumber, NSString, NSURL, NSURLVolumeAvailableCapacityForImportantUsageKey};
    use std::path::Path;

    /// macOS's "available for important usage": free space plus what it can purge by itself.
    pub(super) fn usable(path: &Path) -> Option<u64> {
        let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str()?));
        let mut value = None;
        unsafe { url.getResourceValue_forKey_error(&mut value, NSURLVolumeAvailableCapacityForImportantUsageKey) }.ok()?;
        let number = value?.downcast::<NSNumber>().ok()?;
        u64::try_from(number.longLongValue()).ok()
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub(super) fn usable(_path: &std::path::Path) -> Option<u64> {
        None
    }
}

/// The volume's mount point: the highest ancestor still on the same device.
pub(crate) fn mount_point(path: &Path) -> Option<PathBuf> {
    let device = std::fs::metadata(path).ok()?.dev();
    let mut mount = path.canonicalize().ok()?;
    while let Some(parent) = mount.parent() {
        if std::fs::metadata(parent).ok()?.dev() != device {
            break;
        }
        mount = parent.to_path_buf();
    }
    Some(mount)
}

pub(crate) fn same_volume(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

/// Below these lines Alondra starts no node and puts a card in front of the user.
/// `ALONDRA_LOW_DISK_BYTES` and `ALONDRA_LOW_DISK_FLOOR_BYTES` move them, so a sandbox can try the
/// guard on a healthy disk.
pub(crate) fn low_lines() -> Lines {
    static LINES: OnceLock<Lines> = OnceLock::new();
    let bytes = |name: &str, default: u64| std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default);
    *LINES.get_or_init(|| Lines { usable: bytes("ALONDRA_LOW_DISK_BYTES", LOW_DISK_BYTES), free: bytes("ALONDRA_LOW_DISK_FLOOR_BYTES", FLOOR_DISK_BYTES) })
}

/// Once low, the disk must climb past these lines to count as fine, so it does not flap at them.
pub(crate) fn recovered_lines() -> Lines {
    let low = low_lines();
    Lines { usable: low.usable + RECOVERY_MARGIN_BYTES, free: low.free + FLOOR_RECOVERY_MARGIN_BYTES }
}

/// Whether a write failed because the disk, or the user's quota on it, had no room left.
pub(crate) fn io_no_space(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::StorageFull || matches!(error.raw_os_error(), Some(libc::ENOSPC | libc::EDQUOT))
}

/// `io_no_space` anywhere in the error's chain, or SQLite saying its database is full.
pub(crate) fn no_space(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(io_no_space)
            || cause.downcast_ref::<rusqlite::Error>().is_some_and(|error| error.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_disk_is_told_from_other_failures_through_the_whole_chain() {
        let full = || std::io::Error::from_raw_os_error(libc::ENOSPC);
        assert!(io_no_space(&full()));
        assert!(io_no_space(&std::io::Error::from_raw_os_error(libc::EDQUOT)));
        assert!(!io_no_space(&std::io::Error::from_raw_os_error(libc::EACCES)));
        assert!(no_space(&anyhow::Error::from(full()).context("writing domain-capabilities.json")));
        let sqlite = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL), None);
        assert!(no_space(&anyhow::Error::from(alondra_store::Error::Database(sqlite)).context("committing")));
        assert!(!no_space(&anyhow::anyhow!("No space left on device")), "only the error itself counts, never its text");
    }

    #[test]
    fn the_free_space_of_a_real_folder_is_what_df_reports() {
        let dir = crate::test_support::Directory::new();
        std::fs::create_dir(&dir).unwrap();
        let measured = space(&dir).unwrap();
        assert!(measured.total > 0 && measured.free > 0 && measured.free <= measured.total, "{measured:?}");
        let df = std::process::Command::new("df").arg("-k").arg(&dir).output().unwrap();
        let line = String::from_utf8_lossy(&df.stdout).lines().nth(1).unwrap().to_owned();
        let available_kb: u64 = line.split_whitespace().nth(3).unwrap().parse().unwrap();
        let difference = (measured.free / 1024).abs_diff(available_kb);
        assert!(difference < 1024 * 1024, "statvfs {} KB, df {available_kb} KB", measured.free / 1024);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_reports_the_space_available_for_important_usage_of_a_real_folder() {
        let dir = crate::test_support::Directory::new();
        std::fs::create_dir(&dir).unwrap();
        let usable = platform::usable(&dir).expect("NSURL gives the volume's capacity for important usage");
        assert!(usable > 0 && usable <= space(&dir).unwrap().total, "{usable}");
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
    fn one_more_build_needs_the_typical_one_measured_on_top_of_the_low_line() {
        assert_eq!(needed_for_build(&[]), 10 * GB, "none measured yet");
        assert_eq!(needed_for_build(&[3 * GB, 13 * GB, 8 * GB]), 13 * GB, "the median of 3, 8 and 13 GB");
        assert_eq!(needed_for_build(&[GB / 2, 4 * GB, 12 * GB, 9 * GB]), 14 * GB);
    }

    #[test]
    fn a_disk_short_of_room_for_a_build_needs_margin_to_have_it_again() {
        let needed = 13 * GB;
        assert_eq!(disk(12.0, 0.0).room_for(needed, false), Err(Room { free: 12 * GB, needed }));
        assert_eq!(disk(4.0, 10.0).room_for(needed, false), Ok(()), "purgeable space counts");
        assert_eq!(disk(1.0, 30.0).room_for(needed, false), Err(Room { free: GB, needed }), "little free right now: told what a write can use");
        assert!(disk(14.0, 0.0).room_for(needed, true).is_err(), "under the 2 GB margin once short");
        assert_eq!(disk(15.0, 0.0).room_for(needed, true), Ok(()));
    }

    #[test]
    fn a_missing_path_has_no_space_and_no_volume() {
        let missing = Path::new("/no/such/alondra/folder");
        assert_eq!(space(missing), None);
        assert_eq!(mount_point(missing), None);
    }

    #[test]
    fn a_folder_and_its_mount_point_are_on_the_same_volume() {
        let dir = crate::test_support::Directory::new();
        std::fs::create_dir(&dir).unwrap();
        let mount = mount_point(&dir).unwrap();
        assert!(dir.canonicalize().unwrap().starts_with(&mount));
        assert!(same_volume(&dir, &mount));
    }
}
