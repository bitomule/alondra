//! Whether a write waits until the disk holds it. The daemon always waits: a file flushed and a
//! SQLite commit synced survive a power cut. A test's home is deleted when the test ends, and
//! waiting for the disk is most of what its commits cost, so the fixtures of a test run relax it.
use std::sync::atomic::{AtomicBool, Ordering};

static RELAXED: AtomicBool = AtomicBool::new(false);

/// From here on this process no longer waits for the disk. Only the fixtures of a test run call
/// it: outside `bazel test` it panics, so nothing the daemon runs can turn durability off.
pub fn relax() {
    relax_in(crate::under_test());
}

fn relax_in(under_test: bool) {
    assert!(under_test, "durability is relaxed only under bazel test");
    RELAXED.store(true, Ordering::Relaxed);
}

pub fn relaxed() -> bool {
    RELAXED.load(Ordering::Relaxed)
}

/// Waits until the disk holds `file`, a file or a folder, unless a fixture relaxed durability.
pub fn flush(file: &std::fs::File) -> std::io::Result<()> {
    if relaxed() {
        return Ok(());
    }
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durability_is_relaxed_only_by_a_test_run() {
        assert!(!relaxed(), "nothing relaxed it yet");
        assert!(std::panic::catch_unwind(|| relax_in(false)).is_err(), "the daemon cannot relax it");
        assert!(!relaxed());
        relax_in(true);
        assert!(relaxed());
    }
}
