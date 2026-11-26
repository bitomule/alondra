//! What the budgets are measured in: instructions the CPU retired, the same however loaded the
//! Mac is, on which core the test lands or how fast that core runs. Time, wall or CPU, is not.
//! They count every walk of the domain, allocating or not; they do not see cache misses.
//!
//! macOS counts them for the whole process, so the target runs its tests one at a time. Nowhere
//! else is there a counter an unprivileged test can count on: there the work runs unmeasured.

pub const SAMPLES: usize = 5;

/// Millions of instructions one run of `work` retires, the median of `SAMPLES` runs; what it
/// returns is dropped outside the count.
pub fn median_minstr<T>(mut work: impl FnMut() -> T) -> Option<f64> {
    assert!(std::env::args().any(|arg| arg == "--test-threads=1"),
        "the count is the whole process's: run with --test-threads=1, as the Bazel target does");
    let mut spent: Vec<Option<u64>> = (0..SAMPLES).map(|_| {
        let before = retired();
        let done = std::hint::black_box(work());
        let after = retired();
        drop(done);
        Some(after? - before?)
    }).collect();
    spent.sort();
    spent[SAMPLES / 2].map(|count| count as f64 / 1e6)
}

#[cfg(target_os = "macos")]
fn retired() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    let status = unsafe { libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V4, usage.as_mut_ptr().cast()) };
    assert_eq!(status, 0, "proc_pid_rusage: {}", std::io::Error::last_os_error());
    let count = unsafe { usage.assume_init() }.ri_instructions;
    assert!(count > 0, "this Mac does not count instructions, so no budget would hold anything");
    Some(count)
}

#[cfg(not(target_os = "macos"))]
fn retired() -> Option<u64> {
    None
}
