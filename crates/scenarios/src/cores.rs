//! Whether a process of a scenario still waits for a core: on a loaded Mac a host's relay, or a
//! thread of the daemon, can wait long to run, moving nothing the virtual clock counts, and is
//! slow, not stuck. A process stuck for good waits on something else, or is stopped.

/// How many threads of the process `pid` run or are ready to run and wait for a core; none once it
/// is gone, or when its threads cannot be read.
pub fn runnable_threads(pid: u32) -> usize {
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    let Ok(pid) = libc::c_int::try_from(pid) else { return 0 };
    // SAFETY: proc_pidinfo writes at most `size` bytes into the record it is given.
    let read = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, info.as_mut_ptr().cast(), size) };
    if read != size {
        return 0;
    }
    // SAFETY: a read of the whole record initialized it.
    usize::try_from(unsafe { info.assume_init() }.pti_numrunning).unwrap_or(0)
}
