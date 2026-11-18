//! Free disk space as the Mac measures it, and the space files take: what the `Disk` port reads.
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub use alondra_engine_ports::disk::{io_no_space, low_lines, recovered_lines, Lines, Mounts, Room, Space, BOOT_DATA, LOW_DISK_BYTES};

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
    pub fn add(&mut self, meta: &std::fs::Metadata) {
        let file = (meta.dev(), meta.ino());
        if self.linked.contains(&file) {
            return;
        }
        if meta.nlink() > 1 && !meta.is_dir() {
            self.linked.insert(file);
        }
        self.bytes += meta.blocks() * 512;
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

/// A complete mount table, with the local, writable volumes shown in Finder separated for
/// learning. A known volume stays present even when it becomes read-only or hidden.
pub fn mounted_volumes() -> std::io::Result<Mounts> {
    let mut mounts = platform::mounted()?;
    mounts.candidates.retain(|mount| !matches!(mount.to_str(), Some("/" | BOOT_DATA)));
    Ok(mounts)
}

pub fn space(path: &Path) -> Option<Space> {
    Some(with_usable(free_space(path)?, platform::usable(path)))
}

/// macOS measures "available for important usage" on the whole APFS container, ignoring a
/// volume's quota, so on a volume with a quota it can exceed the volume's own size. Purgeable
/// space is only what still fits in the volume.
fn with_usable(free: Space, usable: Option<u64>) -> Space {
    let purgeable = usable.map_or(0, |usable| usable.saturating_sub(free.free)).min(free.total.saturating_sub(free.free));
    Space { purgeable, ..free }
}

/// What the volume of `path` itself holds, `df`'s Used: on APFS the free space is the whole
/// container's, so it falls with what any of its volumes writes; this grows only with the volume's own.
pub fn held(path: &Path) -> Option<u64> {
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
    pub fn usable(path: &Path) -> Option<u64> {
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

    fn complete_count(filled: libc::c_int, capacity: usize) -> std::io::Result<usize> {
        let filled = usize::try_from(filled).map_err(|_| std::io::Error::last_os_error())?;
        if filled >= capacity {
            return Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "mount table changed while reading"));
        }
        Ok(filled)
    }

    pub fn mounted() -> std::io::Result<super::Mounts> {
        use std::os::unix::ffi::OsStrExt as _;
        let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
        let count = usize::try_from(count).map_err(|_| std::io::Error::last_os_error())?;
        let mut mounts: Vec<libc::statfs> = Vec::with_capacity(count + 4);
        let size = libc::c_int::try_from(mounts.capacity() * std::mem::size_of::<libc::statfs>())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "mount table is too large"))?;
        let filled = unsafe { libc::getfsstat(mounts.as_mut_ptr(), size, libc::MNT_NOWAIT) };
        let filled = complete_count(filled, mounts.capacity())?;
        unsafe { mounts.set_len(filled) };
        let wanted = |flags: u32| flags & libc::MNT_LOCAL as u32 != 0 && flags & (libc::MNT_RDONLY | libc::MNT_DONTBROWSE) as u32 == 0;
        let mut all = std::collections::BTreeSet::new();
        let mut candidates = vec![];
        for mount in &mounts {
            let name = unsafe { std::ffi::CStr::from_ptr(mount.f_mntonname.as_ptr()) };
            let path = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(name.to_bytes()));
            all.insert(path.clone());
            if wanted(mount.f_flags) { candidates.push(path); }
        }
        if !all.contains(Path::new("/")) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "mount table has no root filesystem"));
        }
        Ok(super::Mounts { all, candidates })
    }

    #[test]
    fn a_failed_or_possibly_truncated_mount_table_cannot_confirm_an_unmount() {
        assert!(complete_count(-1, 8).is_err());
        assert!(complete_count(8, 8).is_err());
        assert!(complete_count(9, 8).is_err());
        assert_eq!(complete_count(7, 8).unwrap(), 7);
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub fn usable(_path: &std::path::Path) -> Option<u64> {
        None
    }

    pub fn mounted() -> std::io::Result<super::Mounts> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "mount table is unavailable"))
    }

    pub(super) fn held(_path: &std::path::Path) -> Option<u64> {
        None
    }
}

/// The volume's mount point: the highest ancestor still on the same device.
pub fn mount_point(path: &Path) -> Option<PathBuf> {
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

pub fn same_volume(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_volume_with_a_quota_never_counts_more_purgeable_space_than_it_can_hold() {
        let quota = Space { free: 31_442_436_096, purgeable: 0, total: 40 * GB };
        let measured = with_usable(quota, Some(42_562_916_416));
        assert!(measured.usable() <= measured.total, "9 November: a 40 GB Bazel cache volume read 42.6 GB usable and the gate saw 0 B free");
        assert_eq!(measured.usable(), 40 * GB);
        let plain = with_usable(disk(4.8, 0.0), Some(21_400_000_000));
        assert_eq!(plain.purgeable, 16_600_000_000, "a volume without a quota keeps its purgeable space");
        assert_eq!(with_usable(disk(4.8, 0.0), None).purgeable, 0);
    }

    /// On this Mac `/` is sealed and read-only, the system's own volumes and the simulators' images
    /// are hidden from Finder or read-only, and the data volume is the Mac's own disk.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_volumes_builds_may_write_to_are_local_writable_and_shown_in_finder() {
        let candidates = mounted_volumes().map(|mounts| mounts.candidates).unwrap_or_default();
        let path = |name: &str| PathBuf::from(name);
        for system in ["/", BOOT_DATA, "/System/Volumes/VM", "/System/Volumes/Preboot", "/System/Volumes/Update"] {
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
