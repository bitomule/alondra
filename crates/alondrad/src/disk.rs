//! Free disk space as a process that writes sees it: `statvfs`'s blocks available to an
//! unprivileged user, the number `df` prints. macOS's "available for important usage", which
//! `sysinfo` reports, also counts purgeable space the system may never free in time: a build can
//! fill the disk while that number still shows tens of gigabytes.
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub(crate) const LOW_DISK_BYTES: u64 = 5_000_000_000;
const RECOVERY_MARGIN_BYTES: u64 = 2_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Space {
    pub free: u64,
    pub total: u64,
}

pub(crate) fn space(path: &Path) -> Option<Space> {
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    let stats = unsafe { stats.assume_init() };
    let block = stats.f_frsize as u64;
    Some(Space { free: stats.f_bavail as u64 * block, total: stats.f_blocks as u64 * block })
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

/// Below this many free bytes Alondra starts no node and puts a card in front of the user.
/// `ALONDRA_LOW_DISK_BYTES` moves the line, so a sandbox can try the guard on a healthy disk.
pub(crate) fn low_line() -> u64 {
    static LINE: OnceLock<u64> = OnceLock::new();
    *LINE.get_or_init(|| std::env::var("ALONDRA_LOW_DISK_BYTES").ok().and_then(|value| value.parse().ok()).unwrap_or(LOW_DISK_BYTES))
}

/// Once low, the disk must climb past this line to count as fine, so it does not flap at the line.
pub(crate) fn recovered_line() -> u64 {
    low_line() + RECOVERY_MARGIN_BYTES
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
