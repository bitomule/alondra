//! Free disk space as the Mac measures it, and the space files take: what the `Disk` port reads.
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

pub use alondra_engine_ports::disk::{io_no_space, low_lines, recovered_lines, Lines, Room, Space, BOOT_DATA, LOW_DISK_BYTES};

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
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub fn usable(_path: &std::path::Path) -> Option<u64> {
        None
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

    #[test]
    fn a_missing_path_has_no_space() {
        assert_eq!(space(Path::new("/no/such/alondra/folder")), None);
    }

}
