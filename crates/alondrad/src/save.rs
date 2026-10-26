//! Saving a small state file whole, and telling a save that fails once rather than on every try.
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Writes `bytes` beside `file`, flushes them to disk and renames them over it, so a reader sees
/// the old file or the new one, never half of one. When a step fails, what was written beside is
/// removed, so it takes none of the little space left, and `file` keeps what it held.
pub(crate) fn atomic(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(test)]
    full_disk::write()?;
    let next = file.with_extension("next");
    let written = std::fs::File::create(&next)
        .and_then(|mut written| written.write_all(bytes).and_then(|()| written.sync_all()))
        .and_then(|()| std::fs::rename(&next, file));
    if written.is_err() {
        let _ = std::fs::remove_file(&next);
    }
    written
}

/// Whether the last save of one file failed. A full disk fails every save until there is space:
/// the log says so when it starts and when it ends, not once per save.
#[derive(Default)]
pub(crate) struct Failures(AtomicBool);

impl Failures {
    pub(crate) fn record(&self, file: &Path, saved: std::io::Result<()>) {
        match saved {
            Err(error) if !self.0.swap(true, Ordering::Relaxed) => {
                log!("could not save {}: {error}; it is saved again on its next change, and this is not logged again until it is", file.display());
            }
            Ok(()) if self.0.swap(false, Ordering::Relaxed) => log!("saved {} again", file.display()),
            _ => {}
        }
    }
}

/// A disk with no space left, as one test thread sees it: saves and log lines fail with ENOSPC,
/// and the log keeps every line it was asked to write.
#[cfg(test)]
pub(crate) mod full_disk {
    use std::cell::RefCell;

    #[derive(Default)]
    struct Disk {
        full: bool,
        lines: Vec<String>,
    }

    thread_local! {
        static DISK: RefCell<Option<Disk>> = const { RefCell::new(None) };
    }

    fn no_space() -> std::io::Error {
        std::io::Error::from_raw_os_error(libc::ENOSPC)
    }

    /// Fills this thread's disk until the guard is dropped.
    pub(crate) fn fill() -> Guard {
        DISK.with_borrow_mut(|disk| *disk = Some(Disk { full: true, lines: vec![] }));
        Guard
    }

    pub(crate) struct Guard;

    impl Guard {
        pub(crate) fn free(&self) {
            DISK.with_borrow_mut(|disk| disk.as_mut().expect("a simulated disk").full = false);
        }

        /// Every line logged since the disk filled, written or not.
        pub(crate) fn lines(&self) -> Vec<String> {
            DISK.with_borrow(|disk| disk.as_ref().expect("a simulated disk").lines.clone())
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            DISK.with_borrow_mut(|disk| *disk = None);
        }
    }

    pub(super) fn write() -> std::io::Result<()> {
        DISK.with_borrow(|disk| if disk.as_ref().is_some_and(|disk| disk.full) { Err(no_space()) } else { Ok(()) })
    }

    pub(crate) fn log() -> Option<Log> {
        DISK.with_borrow(|disk| disk.as_ref().map(|_| Log))
    }

    pub(crate) struct Log;

    impl std::io::Write for Log {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            DISK.with_borrow_mut(|disk| {
                let disk = disk.as_mut().expect("a simulated disk");
                disk.lines.push(String::from_utf8_lossy(buf).into_owned());
                if disk.full { Err(no_space()) } else { Ok(buf.len()) }
            })
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(folder).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn a_saved_file_holds_the_new_bytes_and_nothing_is_left_beside_it() {
        let folder = crate::test_support::Directory::new();
        std::fs::create_dir(&folder).unwrap();
        let file = folder.join("admission.json");
        std::fs::write(&file, b"before").unwrap();
        atomic(&file, b"after").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"after");
        assert_eq!(entries(&folder), ["admission.json"]);
    }

    #[test]
    fn a_save_that_fails_leaves_the_file_before_whole_and_nothing_beside_it() {
        let folder = crate::test_support::Directory::new();
        std::fs::create_dir(&folder).unwrap();
        let file = folder.join("launch-options.json");
        std::fs::create_dir(&file).unwrap();
        std::fs::write(file.join("kept"), b"before").unwrap();
        assert!(atomic(&file, b"after").is_err(), "a file is never renamed over a folder that holds something");
        assert_eq!(std::fs::read(file.join("kept")).unwrap(), b"before");
        assert_eq!(entries(&folder), ["launch-options.json"]);
    }
}
