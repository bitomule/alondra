#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::line(format_args!($($arg)*))
    };
}

use alondra_core::daemon_log;
use std::io::{BufRead, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static ROTATION: OnceLock<Rotation> = OnceLock::new();
const STANDARD: [RawFd; 2] = [libc::STDERR_FILENO, libc::STDOUT_FILENO];

/// From now on, rotates `alondrad.log` in `home` whenever it reaches its limit, and at once if it
/// already has. Only when stderr is that file, as under launchd: a pipe or another file is left
/// alone.
pub fn rotate_when_full(home: &Path) {
    let path = home.join(daemon_log::FILE);
    if !refers_to(libc::STDERR_FILENO, &path) {
        return;
    }
    ROTATION
        .get_or_init(|| Rotation::new(path, daemon_log::ROTATE_AT))
        .check(&STANDARD);
}

pub fn after_write() {
    if let Some(rotation) = ROTATION.get() {
        rotation.check(&STANDARD);
    }
}

/// `alondrad.log` and the limit it rotates at. launchd opened the file once and the process holds
/// that descriptor, so a rotation renames the file and points the descriptors at a new one.
struct Rotation {
    path: PathBuf,
    limit: u64,
    /// A holder that panics leaves it whole, so it is recovered: it holds nothing.
    lock: Mutex<()>,
}

impl Rotation {
    fn new(path: PathBuf, limit: u64) -> Self {
        Self { path, limit, lock: Mutex::new(()) }
    }

    /// Rotates when the first of `fds` reached the limit. A failure leaves the descriptors
    /// writing where they were.
    fn check(&self, fds: &[RawFd]) {
        if size(fds[0]) < self.limit {
            return;
        }
        let _held = match self.lock.try_lock() {
            Ok(held) => held,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                self.lock.clear_poison();
                poisoned.into_inner()
            }
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        if size(fds[0]) < self.limit {
            return;
        }
        if let Err(error) = self.rotate(fds) {
            let _ = writeln!(std::io::stderr(), "{} alondrad: cannot rotate {}: {error}", stamp(), self.path.display());
        }
    }

    fn rotate(&self, fds: &[RawFd]) -> std::io::Result<()> {
        let held: Vec<RawFd> = fds.iter().copied().filter(|fd| refers_to(*fd, &self.path)).collect();
        if held.is_empty() {
            return Ok(());
        }
        let rotated = self.path.with_file_name(daemon_log::ROTATED);
        std::fs::rename(&self.path, &rotated)?;
        let mut fresh = match std::fs::OpenOptions::new().append(true).create(true).open(&self.path) {
            Ok(file) => file,
            Err(error) => {
                let _ = std::fs::rename(&rotated, &self.path);
                return Err(error);
            }
        };
        writeln!(fresh, "{} alondrad: log rotated; earlier lines are in {}", stamp(), daemon_log::ROTATED)?;
        for fd in held {
            if unsafe { libc::dup2(fresh.as_raw_fd(), fd) } < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        keep_last(&rotated, self.limit)
    }
}

fn identity(stat: &libc::stat) -> (libc::dev_t, libc::ino_t) {
    (stat.st_dev, stat.st_ino)
}

fn fstat(fd: RawFd) -> Option<libc::stat> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    (unsafe { libc::fstat(fd, &mut stat) } == 0).then_some(stat)
}

fn size(fd: RawFd) -> u64 {
    fstat(fd).map_or(0, |stat| stat.st_size as u64)
}

fn refers_to(fd: RawFd, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Some(stat), Ok(file)) = (fstat(fd), std::fs::metadata(path)) else { return false };
    identity(&stat) == (file.dev() as libc::dev_t, file.ino() as libc::ino_t)
}

/// Cuts `path` to its whole lines within the last `limit` bytes once it holds more than twice
/// that: a log from before rotation existed can be gigabytes, while a rotated one passes the
/// limit only by its last lines.
fn keep_last(path: &Path, limit: u64) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len <= 2 * limit {
        return Ok(());
    }
    file.seek(SeekFrom::Start(len - limit - 1))?;
    let mut reader = std::io::BufReader::new(file);
    reader.skip_until(b'\n')?;
    let cut = path.with_extension("1.tmp");
    std::io::copy(&mut reader, &mut std::fs::File::create(&cut)?)?;
    std::fs::rename(&cut, path)
}

pub fn line(message: std::fmt::Arguments<'_>) {
    let line = format!("{} alondrad: {message}\n", stamp());
    #[cfg(any(test, feature = "test-hooks"))]
    if let Some(mut disk) = crate::save::full_disk::log() {
        return write(&mut disk, &line);
    }
    write(&mut std::io::stderr().lock(), &line);
    after_write();
}

/// The log is `alondrad.log`: on a full disk the line is lost, and the daemon goes on. `eprintln!`
/// panics instead, and did so on 25 October holding the machine watch's lock.
fn write(out: &mut impl std::io::Write, line: &str) {
    let _ = out.write_all(line.as_bytes());
}

pub fn stamp() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    local_iso(now.as_secs() as libc::time_t, now.subsec_millis())
}

pub fn local_iso(secs: libc::time_t, millis: u32) -> String {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return format!("@{secs}");
    }
    let offset = tm.tm_gmtoff / 60;
    let sign = if offset < 0 { '-' } else { '+' };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{millis:03}{sign}{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        offset.abs() / 60,
        offset.abs() % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log_in(home: &Path) -> std::fs::File {
        std::fs::create_dir_all(home).unwrap();
        std::fs::OpenOptions::new().append(true).create(true).open(home.join(daemon_log::FILE)).unwrap()
    }

    #[test]
    fn past_the_limit_the_log_rotates_and_the_same_descriptor_writes_to_a_new_alondrad_log() {
        let home = crate::test_support::Directory::new();
        let mut inherited = log_in(&home);
        let rotation = Rotation::new(home.join(daemon_log::FILE), 64);
        writeln!(inherited, "first line before the limit").unwrap();
        rotation.check(&[inherited.as_raw_fd()]);
        assert!(!home.join(daemon_log::ROTATED).exists(), "under the limit nothing rotates");
        writeln!(inherited, "second line past the limit of the log").unwrap();
        rotation.check(&[inherited.as_raw_fd()]);
        writeln!(inherited, "after the rotation").unwrap();
        let rotated = std::fs::read_to_string(home.join(daemon_log::ROTATED)).unwrap();
        assert_eq!(rotated, "first line before the limit\nsecond line past the limit of the log\n");
        let current = std::fs::read_to_string(home.join(daemon_log::FILE)).unwrap();
        assert!(current.contains("log rotated; earlier lines are in alondrad.log.1\n"), "{current}");
        assert!(current.ends_with("\nafter the rotation\n"), "{current}");
        assert_eq!(
            daemon_log::tail(&home, 2, 1024).unwrap().last().map(String::as_str),
            Some("after the rotation")
        );
    }

    #[test]
    fn a_descriptor_on_another_file_never_rotates_the_log() {
        let home = crate::test_support::Directory::new();
        let mut log = log_in(&home);
        writeln!(log, "{}", "x".repeat(100)).unwrap();
        let mut elsewhere = std::fs::File::create(home.join("other.log")).unwrap();
        writeln!(elsewhere, "{}", "y".repeat(100)).unwrap();
        Rotation::new(home.join(daemon_log::FILE), 64).check(&[elsewhere.as_raw_fd()]);
        assert!(!home.join(daemon_log::ROTATED).exists());
        assert_eq!(std::fs::metadata(home.join(daemon_log::FILE)).unwrap().len(), 101);
    }

    #[test]
    fn a_huge_log_from_before_rotation_keeps_only_its_last_lines() {
        let home = crate::test_support::Directory::new();
        let mut inherited = log_in(&home);
        for n in 1..=100 {
            writeln!(inherited, "line {n:03}").unwrap();
        }
        Rotation::new(home.join(daemon_log::FILE), 20).check(&[inherited.as_raw_fd()]);
        let rotated = std::fs::read_to_string(home.join(daemon_log::ROTATED)).unwrap();
        assert_eq!(rotated, "line 099\nline 100\n");
        assert!(!home.join("alondrad.log.1.tmp").exists());
    }

    #[test]
    fn a_log_line_starts_with_the_local_time_and_its_offset() {
        let s = local_iso(1_790_000_000, 7);
        assert_eq!(s.len(), "2026-09-21T14:13:20.007+02:00".len(), "{s}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
        assert_eq!(&s[19..23], ".007");
        assert!(matches!(&s[23..24], "+" | "-"), "{s}");
        assert!(stamp().starts_with("20"));
    }
}
