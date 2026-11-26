//! The child a supervisor owns across its own exit: a lock the child inherits, and a durable record
//! of which daemon started which process group for which run. A later daemon reads the record to end
//! that exact group, and never signals a PID the record no longer names.
use serde::{Deserialize, Serialize};
use std::io::{self, Write as _};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const RECORD: &str = "owner.json";

fn refused(why: impl Into<String>) -> io::Error {
    io::Error::other(why.into())
}

/// This lock remains inherited by the child across an abrupt daemon exit. A replacement daemon must
/// not act on the same conversation until the old process has exited. Unlocking releases every
/// duplicate, including one a sibling thread's fork still holds before it execs.
pub struct Ownership {
    file: std::fs::File,
}

impl Drop for Ownership {
    fn drop(&mut self) {
        // SAFETY: the descriptor is this lock's own open file.
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl Ownership {
    pub fn acquire(directory: &Path) -> io::Result<Option<Self>> {
        std::fs::create_dir_all(directory)?;
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(directory.join("executor.lock"))?;
        // SAFETY: the descriptor is the file just opened.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            return Err(error);
        }
        Ok(Some(Self { file }))
    }
}

/// Which daemon started which child for which run, each process named by its kernel birth time.
#[derive(Serialize, Deserialize)]
struct Owner {
    daemon_pid: u32,
    daemon_started: u64,
    child_pid: u32,
    child_started: u64,
    run_id: String,
}

/// The kernel birth time of `pid`, in microseconds. macOS may redact argv and environment even for a
/// same-user child, so the environment is never an ownership proof.
fn identity(pid: u32) -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    // SAFETY: the buffer is a zeroed proc_bsdinfo of `size` bytes, the flavor's exact layout.
    let read = unsafe { libc::proc_pidinfo(pid as i32, libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size as i32) };
    if read != size as i32 {
        return None;
    }
    // SAFETY: proc_pidinfo filled the whole structure.
    let info = unsafe { info.assume_init() };
    Some(info.pbi_start_tvsec.saturating_mul(1_000_000).saturating_add(info.pbi_start_tvusec))
}

fn read(directory: &Path) -> io::Result<Option<Owner>> {
    match std::fs::read(directory.join(RECORD)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn gone(pid: u32) -> bool {
    // SAFETY: signal 0 only asks whether the process exists.
    let exists = unsafe { libc::kill(pid as i32, 0) };
    exists != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

fn leads_its_group(pid: u32) -> bool {
    // SAFETY: getpgid only reads the process table.
    let group = unsafe { libc::getpgid(pid as i32) };
    group == pid as i32
}

fn kill_group(leader: u32) -> io::Result<()> {
    // SAFETY: the group was proven to be the recorded child's own before this is called.
    if unsafe { libc::kill(-(leader as i32), libc::SIGKILL) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Records that this daemon owns `child_pid` for `run_id`. A child the kernel does not know, as a
/// test's doubled host, is born when `birth` says. Each write first asks `fault` at its named point
/// (`owner_create`, `owner_write`, `owner_sync`, `owner_rename`, `owner_directory_sync`) and fails
/// with its error, leaving no temporary file behind.
pub fn record(
    directory: &Path,
    child_pid: u32,
    run_id: &str,
    birth: impl Fn(u32) -> Option<u64>,
    fault: impl Fn(&'static str) -> io::Result<()>,
) -> io::Result<()> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let daemon_pid = std::process::id();
    let owner = Owner {
        daemon_pid,
        daemon_started: identity(daemon_pid).ok_or_else(|| refused("cannot identify orchestrator daemon process"))?,
        child_pid,
        child_started: identity(child_pid).or_else(|| birth(child_pid)).ok_or_else(|| refused("cannot identify owned Claude process"))?,
        run_id: run_id.into(),
    };
    let temporary = directory.join(format!("owner-{daemon_pid}-{}.tmp", WRITES.fetch_add(1, Ordering::Relaxed)));
    let written = (|| -> io::Result<()> {
        fault("owner_create")?;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        fault("owner_write")?;
        file.write_all(&serde_json::to_vec(&owner)?)?;
        fault("owner_sync")?;
        alondra_foundation::durability::flush(&file)?;
        fault("owner_rename")?;
        std::fs::rename(&temporary, directory.join(RECORD))?;
        fault("owner_directory_sync")?;
        alondra_foundation::durability::flush(&std::fs::File::open(directory)?)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// At recovery, under this daemon's ownership: a process a previous daemon recorded for the run it
/// left unfinished is ended if it still runs, and its record goes. One the lock already ended, or
/// one that is not the recorded process any more, is left alone.
pub fn end_recorded(directory: &Path, active_run: &str) -> io::Result<()> {
    let Some(owner) = read(directory)? else { return Ok(()) };
    if owner.run_id == active_run
        && identity(owner.daemon_pid) != Some(owner.daemon_started)
        && identity(owner.child_pid) == Some(owner.child_started)
        && leads_its_group(owner.child_pid)
    {
        kill_group(owner.child_pid)?;
    }
    std::fs::remove_file(directory.join(RECORD))
}

/// The run whose child the record names, if there is a record.
pub fn recorded_run(directory: &Path) -> io::Result<Option<String>> {
    Ok(read(directory)?.map(|owner| owner.run_id))
}

/// Between batches, fence a retained host even when it belonged to this daemon. A provider worker
/// that failed to reap its host must leave its record until that exact group is gone.
pub fn fence_recorded(directory: &Path, run: &str) -> io::Result<()> {
    let Some(owner) = read(directory)? else { return Ok(()) };
    if owner.run_id != run {
        return Err(refused("the retained host belongs to another orchestrator batch"));
    }
    let daemon = identity(owner.daemon_pid);
    let current_daemon = owner.daemon_pid == std::process::id() && daemon == Some(owner.daemon_started);
    if !current_daemon && daemon == Some(owner.daemon_started) {
        return Err(refused("another live daemon owns the retained orchestrator host"));
    }
    if !current_daemon && daemon.is_none() && !gone(owner.daemon_pid) {
        return Err(refused("cannot prove that the previous orchestrator daemon exited"));
    }
    if identity(owner.child_pid) == Some(owner.child_started) {
        if !leads_its_group(owner.child_pid) {
            return Err(refused("the retained orchestrator host is not its process-group leader"));
        }
        if let Err(error) = kill_group(owner.child_pid)
            && error.raw_os_error() != Some(libc::ESRCH)
        {
            return Err(error);
        }
        if crate::members(owner.child_pid as i32)?.iter().any(|member| !member.zombie) {
            return Err(refused("the retained orchestrator host is still stopping; reconciliation will retry"));
        }
        if current_daemon {
            // SAFETY: WNOHANG never blocks, and a null status is allowed.
            let reaped = unsafe { libc::waitpid(owner.child_pid as i32, std::ptr::null_mut(), libc::WNOHANG) };
            if reaped == 0 {
                return Err(refused("the retained orchestrator host has not exited; reconciliation will retry"));
            }
            if reaped < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ECHILD) {
                return Err(io::Error::last_os_error());
            }
        }
    } else if identity(owner.child_pid).is_none() && !gone(owner.child_pid) {
        return Err(refused("cannot prove that the retained orchestrator host exited"));
    }
    if crate::members(owner.child_pid as i32)?.iter().any(|member| !member.zombie) {
        return Err(refused("the retained orchestrator group still has live processes; ownership is retained"));
    }
    std::fs::remove_file(directory.join(RECORD))
}

/// Called only while the inherited lock is held by a previous process. A PID alone is never
/// authority to signal: its kernel birth identity, durable run and group are verified first.
pub fn terminate_orphan(directory: &Path, active_run: Option<&str>) -> io::Result<()> {
    let owner = read(directory)
        .and_then(|owner| owner.ok_or_else(|| io::ErrorKind::NotFound.into()))
        .map_err(|error| refused(format!("execution lock is held, but its owner metadata is unavailable: {error}")))?;
    if active_run != Some(owner.run_id.as_str()) {
        return Err(refused("execution lock owner does not match the durable execution"));
    }
    let daemon = identity(owner.daemon_pid);
    if daemon == Some(owner.daemon_started) {
        return Err(refused("another live daemon owns the orchestrator execution"));
    }
    // Failure to inspect a live PID is not proof that the owning daemon disappeared.
    if daemon.is_none() && !gone(owner.daemon_pid) {
        return Err(refused("cannot prove that the prior daemon exited"));
    }
    if identity(owner.child_pid) != Some(owner.child_started) {
        return Err(refused("execution lock remains held; its process identity cannot be verified safely"));
    }
    if !leads_its_group(owner.child_pid) {
        return Err(refused("orchestrator owner is not the expected process-group leader"));
    }
    kill_group(owner.child_pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};

    /// A directory of the test's own under Bazel's TEST_TMPDIR, removed when the test ends.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let root = PathBuf::from(std::env::var_os("TEST_TMPDIR").expect("Bazel gives each test its own temporary folder"));
            let scratch = Self(root.join(format!("owner-{name}-{}", std::process::id())));
            std::fs::create_dir_all(&scratch.0).unwrap();
            scratch
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A child killed and reaped when the test ends, whatever it asserted.
    struct Owned(Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn write(directory: &Path, owner: &Owner) {
        std::fs::write(directory.join(RECORD), serde_json::to_vec(owner).unwrap()).unwrap();
    }

    fn exited_pid() -> u32 {
        let mut gone = Command::new("/usr/bin/true").spawn().unwrap();
        let pid = gone.id();
        gone.wait().unwrap();
        pid
    }

    #[test]
    fn a_process_a_dead_daemon_left_for_its_unfinished_run_is_ended_at_recovery() {
        let directory = Scratch::new("recovery");
        let daemon = exited_pid();
        let mut left = Owned(Command::new("/bin/cat").stdin(Stdio::piped()).process_group(0).spawn().unwrap());
        let (child_pid, child_started) = (left.0.id(), identity(left.0.id()).unwrap());
        let record = |run: &str| write(&directory.0, &Owner { daemon_pid: daemon, daemon_started: 1, child_pid, child_started, run_id: run.into() });
        record("another-run");
        end_recorded(&directory.0, "unfinished").unwrap();
        assert!(left.0.try_wait().unwrap().is_none(), "a process recorded for another run is left alone");
        assert!(!directory.0.join(RECORD).exists());
        record("unfinished");
        end_recorded(&directory.0, "unfinished").unwrap();
        assert_eq!(left.0.wait().unwrap().signal(), Some(libc::SIGKILL));
        assert!(!directory.0.join(RECORD).exists());
        end_recorded(&directory.0, "unfinished").unwrap();
    }

    /// Holds a child between fork and exec, where it owns duplicates of every descriptor in this
    /// process, including other threads' close-on-exec lock descriptors.
    fn paused_fork() -> (std::thread::JoinHandle<()>, libc::c_int) {
        let (mut forked, mut release) = ([0; 2], [0; 2]);
        assert_eq!(unsafe { libc::pipe(forked.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { libc::pipe(release.as_mut_ptr()) }, 0);
        let (forked_write, release_read) = (forked[1], release[0]);
        let child = std::thread::spawn(move || {
            let mut command = Command::new("/usr/bin/true");
            unsafe {
                command.pre_exec(move || {
                    let mut byte = 0u8;
                    libc::write(forked_write, (&raw const byte).cast(), 1);
                    libc::read(release_read, (&raw mut byte).cast(), 1);
                    Ok(())
                });
            }
            assert!(command.status().unwrap().success());
        });
        let mut byte = 0u8;
        assert_eq!(unsafe { libc::read(forked[0], (&raw mut byte).cast(), 1) }, 1);
        for fd in [forked[0], forked[1], release[0]] {
            unsafe { libc::close(fd) };
        }
        (child, release[1])
    }

    #[test]
    fn dropped_ownership_is_free_while_another_thread_is_forking() {
        let scratch = Scratch::new("fork");
        let directory = scratch.0.join("lock");
        let ownership = Ownership::acquire(&directory).unwrap().unwrap();
        let (child, release) = paused_fork();
        drop(ownership);
        let reacquired = Ownership::acquire(&directory).unwrap();
        unsafe {
            libc::write(release, [0u8].as_ptr().cast(), 1);
            libc::close(release);
        }
        child.join().unwrap();
        assert!(reacquired.is_some(), "a forked sibling must not keep a released ownership alive");
    }

    #[test]
    fn orphan_cleanup_requires_dead_daemon_and_exact_child_identity() {
        let directory = Scratch::new("orphan");
        let mut child = Owned(Command::new("/bin/sleep").arg("30").stdin(Stdio::null()).process_group(0).spawn().unwrap());
        record(&directory.0, child.0.id(), "test-run", |_| None, |_| Ok(())).unwrap();
        assert!(terminate_orphan(&directory.0, Some("test-run")).unwrap_err().to_string().contains("live daemon"));
        assert!(child.0.try_wait().unwrap().is_none());
        let mut owner: Owner = serde_json::from_slice(&std::fs::read(directory.0.join(RECORD)).unwrap()).unwrap();
        owner.daemon_pid = exited_pid();
        owner.daemon_started = 0;
        let actual_start = owner.child_started;
        owner.child_started += 1;
        write(&directory.0, &owner);
        assert!(terminate_orphan(&directory.0, Some("test-run")).is_err());
        assert!(child.0.try_wait().unwrap().is_none());
        owner.child_started = actual_start;
        write(&directory.0, &owner);
        assert!(terminate_orphan(&directory.0, Some("stale-run")).is_err());
        terminate_orphan(&directory.0, Some("test-run")).unwrap();
        assert_eq!(child.0.wait().unwrap().signal(), Some(libc::SIGKILL));
    }

    #[test]
    fn a_write_that_fails_leaves_no_record_and_no_temporary_file() {
        let directory = Scratch::new("fault");
        let child = Owned(Command::new("/bin/sleep").arg("30").stdin(Stdio::null()).process_group(0).spawn().unwrap());
        for point in ["owner_create", "owner_write", "owner_sync", "owner_rename"] {
            let failed = record(&directory.0, child.0.id(), "test-run", |_| None, |at| if at == point { Err(io::Error::other(point)) } else { Ok(()) });
            assert_eq!(failed.unwrap_err().to_string(), point);
            assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 0, "{point} left a file behind");
        }
        record(&directory.0, child.0.id(), "test-run", |_| None, |_| Ok(())).unwrap();
        assert_eq!(recorded_run(&directory.0).unwrap().as_deref(), Some("test-run"));
    }
}
