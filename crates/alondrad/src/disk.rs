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
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub(crate) const LOW_DISK_BYTES: u64 = 5_000_000_000;
pub(crate) const FLOOR_DISK_BYTES: u64 = 2_000_000_000;
const RECOVERY_MARGIN_BYTES: u64 = 2_000_000_000;
const FLOOR_RECOVERY_MARGIN_BYTES: u64 = 1_000_000_000;
const FLOOR_USABLE_BYTES: u64 = 3_000_000_000;
const FLOOR_FREE_BYTES: u64 = 1_500_000_000;

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

    /// Under the floor nodes at work must not take the disk past: little usable space, or little
    /// free right now.
    pub(crate) fn under_floor(&self) -> bool {
        self.usable() < FLOOR_USABLE_BYTES || self.free < FLOOR_FREE_BYTES
    }

    /// Whether one more build fits, `needed` counting purgeable space; once it did not
    /// (`was_short`) it needs margin on both lines to fit again.
    pub(crate) fn room_for(&self, needed: u64, was_short: bool) -> Result<(), Room> {
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
pub(crate) struct Room {
    pub free: u64,
    pub needed: u64,
    pub volume: Option<String>,
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

/// The volumes mounted on this Mac a build may write to besides its own disk: local, writable and
/// shown in Finder, so never `/`, the system's own volumes or the simulators' images.
pub(crate) fn candidate_volumes() -> Vec<PathBuf> {
    platform::mounted().into_iter().filter(|mount| !matches!(mount.to_str(), Some("/" | crate::machine::BOOT_DATA))).collect()
}

pub(crate) fn space(path: &Path) -> Option<Space> {
    let free = free_space(path)?;
    let purgeable = platform::usable(path).map_or(0, |usable| usable.saturating_sub(free.free));
    Some(Space { purgeable, ..free })
}

/// What the volume of `path` itself holds, `df`'s Used: on APFS the free space is the whole
/// container's, so it falls with what any of its volumes writes; this grows only with the volume's own.
pub(crate) fn held(path: &Path) -> Option<u64> {
    platform::held(path)
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

    pub(super) fn held(path: &Path) -> Option<u64> {
        use std::os::unix::ffi::OsStrExt as _;
        #[repr(C, packed(4))]
        struct Reply {
            length: u32,
            used: libc::off_t,
        }
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut wanted = libc::attrlist {
            bitmapcount: libc::ATTR_BIT_MAP_COUNT, reserved: 0, commonattr: 0,
            volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_SPACEUSED, dirattr: 0, fileattr: 0, forkattr: 0,
        };
        let mut reply = std::mem::MaybeUninit::<Reply>::zeroed();
        let size = std::mem::size_of::<Reply>();
        if unsafe { libc::getattrlist(path.as_ptr(), (&raw mut wanted).cast(), reply.as_mut_ptr().cast(), size, 0) } != 0 {
            return None;
        }
        let reply = unsafe { reply.assume_init() };
        let (length, used) = (reply.length, reply.used);
        if (length as usize) < size {
            return None;
        }
        u64::try_from(used).ok()
    }

    /// Every mount point that is local, writable and not hidden from Finder.
    pub(super) fn mounted() -> Vec<std::path::PathBuf> {
        use std::os::unix::ffi::OsStrExt as _;
        let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
        let Ok(count) = usize::try_from(count) else { return vec![] };
        let mut mounts: Vec<libc::statfs> = Vec::with_capacity(count + 4);
        let size = libc::c_int::try_from(mounts.capacity() * std::mem::size_of::<libc::statfs>()).unwrap_or(libc::c_int::MAX);
        let filled = unsafe { libc::getfsstat(mounts.as_mut_ptr(), size, libc::MNT_NOWAIT) };
        let Ok(filled) = usize::try_from(filled) else { return vec![] };
        unsafe { mounts.set_len(filled.min(mounts.capacity())) };
        let wanted = |flags: u32| flags & libc::MNT_LOCAL as u32 != 0 && flags & (libc::MNT_RDONLY | libc::MNT_DONTBROWSE) as u32 == 0;
        mounts.iter().filter(|mount| wanted(mount.f_flags)).map(|mount| {
            let name = unsafe { std::ffi::CStr::from_ptr(mount.f_mntonname.as_ptr()) };
            std::path::PathBuf::from(std::ffi::OsStr::from_bytes(name.to_bytes()))
        }).collect()
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub(super) fn usable(_path: &std::path::Path) -> Option<u64> {
        None
    }

    pub(super) fn mounted() -> Vec<std::path::PathBuf> {
        vec![]
    }

    pub(super) fn held(_path: &std::path::Path) -> Option<u64> {
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

    /// On this Mac `/` is sealed and read-only, the system's own volumes and the simulators' images
    /// are hidden from Finder or read-only, and the data volume is the Mac's own disk.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_volumes_builds_may_write_to_are_local_writable_and_shown_in_finder() {
        let candidates = candidate_volumes();
        let path = |name: &str| PathBuf::from(name);
        for system in ["/", crate::machine::BOOT_DATA, "/System/Volumes/VM", "/System/Volumes/Preboot", "/System/Volumes/Update"] {
            assert!(!candidates.contains(&path(system)), "{system} in {candidates:?}");
        }
        assert!(candidates.iter().all(|mount| !mount.starts_with("/Library/Developer/CoreSimulator/Volumes")), "{candidates:?}");
        assert!(candidates.iter().all(|mount| mount.is_dir()), "{candidates:?}");
    }

    /// What `df` reports as Used for the folder's volume, which on APFS is not its size less its
    /// free space: that free space is the whole container's.
    #[cfg(target_os = "macos")]
    #[test]
    fn what_a_volume_holds_is_what_df_reports_as_used() {
        let dir = crate::test_support::Directory::new();
        std::fs::create_dir(&dir).unwrap();
        let held = held(&dir).expect("getattrlist gives the volume's space used");
        let df = std::process::Command::new("df").arg("-k").arg(&dir).output().unwrap();
        let line = String::from_utf8_lossy(&df.stdout).lines().nth(1).unwrap().to_owned();
        let used_kb: u64 = line.split_whitespace().nth(2).unwrap().parse().unwrap();
        assert!((held / 1024).abs_diff(used_kb) < 1024 * 1024, "getattrlist {} KB, df {used_kb} KB", held / 1024);
        assert_eq!(super::held(Path::new("/no/such/alondra/folder")), None);
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
