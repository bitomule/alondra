//! What a supervisor that owns a process group needs from macOS and cannot ask for in safe Rust:
//! who is still in the group, which process is at the other end of a Unix socket, and a wait for
//! processes to exit that reaps nothing. The supervisor decides; this only asks.
#![deny(unsafe_op_in_unsafe_fn)]
use std::collections::BTreeSet;
use std::ffi::{c_int, c_void};
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};

/// One process of a group, named by its PID and the moment it started: a PID alone can be reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Member {
    pub pid: i32,
    pub started: (u64, u64),
    pub zombie: bool,
}

impl std::fmt::Display for Member {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pid {} started {}.{:06}", self.pid, self.started.0, self.started.1)
    }
}

/// The processes whose group is `group` right now, zombies included.
pub fn members(group: i32) -> io::Result<Vec<Member>> {
    let mut capacity = 64;
    loop {
        let mut pids = vec![0 as libc::pid_t; capacity];
        let bytes = c_int::try_from(capacity * size_of::<libc::pid_t>()).map_err(|_| io::ErrorKind::OutOfMemory)?;
        clear_errno();
        // SAFETY: the buffer holds exactly `bytes` bytes of pid_t and outlives the call. Unlike
        // proc_listpids, this returns a count of PIDs.
        let written = unsafe { libc::proc_listpgrppids(group, pids.as_mut_ptr().cast::<c_void>(), bytes) };
        let error = io::Error::last_os_error();
        if written <= 0 {
            return match error.raw_os_error() {
                Some(0) | Some(libc::ESRCH) | None => Ok(Vec::new()),
                _ => Err(error),
            };
        }
        let count = usize::try_from(written).unwrap_or(0);
        if count >= capacity {
            capacity *= 2;
            continue;
        }
        let mut found = Vec::new();
        for pid in pids.into_iter().take(count).filter(|pid| *pid > 0) {
            if let Some(member) = inspect(pid)?.filter(|(_, pgid)| *pgid == group).map(|(member, _)| member) {
                found.push(member);
            }
        }
        return Ok(found);
    }
}

/// The process `pid` names right now, with the moment it started; none once it is gone.
pub fn process(pid: i32) -> io::Result<Option<Member>> {
    Ok(inspect(pid)?.map(|(member, _)| member))
}

/// The parent of the process `pid` names right now.
pub fn parent(pid: i32) -> io::Result<Option<Member>> {
    match parent_pid(pid)? {
        Some(parent) => process(parent),
        None => Ok(None),
    }
}

fn parent_pid(pid: i32) -> io::Result<Option<i32>> {
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = c_int::try_from(size_of::<libc::proc_bsdinfo>()).expect("proc_bsdinfo fits in c_int");
    clear_errno();
    // SAFETY: the buffer is a zeroed proc_bsdinfo of `size` bytes, the flavor's exact layout.
    let written = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast::<c_void>(), size) };
    if written != size {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ESRCH) | Some(0) | None => Ok(None),
            _ => Err(error),
        };
    }
    // SAFETY: the kernel filled all `size` bytes.
    let info = unsafe { info.assume_init() };
    Ok(Some(i32::try_from(info.pbi_ppid).map_err(|_| io::ErrorKind::InvalidData)?))
}

/// Whether `member` is still that same process and has not exited.
pub fn alive(member: &Member) -> io::Result<bool> {
    Ok(inspect(member.pid)?.is_some_and(|(now, _)| now.started == member.started && !now.zombie))
}

/// The process at the other end of the connected Unix socket `stream`, as the kernel records it.
pub fn peer(stream: &impl AsFd) -> io::Result<Option<Member>> {
    let mut pid: libc::pid_t = 0;
    let mut size = libc::socklen_t::try_from(size_of::<libc::pid_t>()).expect("pid_t fits in socklen_t");
    // SAFETY: getsockopt writes at most `size` bytes into the pid_t it is given.
    let result = unsafe {
        libc::getsockopt(
            stream.as_fd().as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast::<c_void>(),
            &mut size,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(inspect(pid)?.map(|(member, _)| member))
}

fn inspect(pid: i32) -> io::Result<Option<(Member, i32)>> {
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = c_int::try_from(size_of::<libc::proc_bsdinfo>()).expect("proc_bsdinfo fits in c_int");
    clear_errno();
    // SAFETY: the buffer is a zeroed proc_bsdinfo of `size` bytes, the flavor's exact layout.
    let written = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast::<c_void>(), size) };
    if written != size {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ESRCH) | Some(0) | None => Ok(None),
            _ => Err(error),
        };
    }
    // SAFETY: the kernel filled all `size` bytes.
    let info = unsafe { info.assume_init() };
    let pgid = i32::try_from(info.pbi_pgid).map_err(|_| io::ErrorKind::InvalidData)?;
    Ok(Some((
        Member { pid, started: (info.pbi_start_tvsec, info.pbi_start_tvusec), zombie: info.pbi_status == libc::SZOMB },
        pgid,
    )))
}

fn clear_errno() {
    // SAFETY: __error returns this thread's errno slot.
    unsafe { *libc::__error() = 0 };
}

/// The kernel's word that processes exited, without reaping any of them.
pub struct Exits {
    queue: OwnedFd,
    pending: BTreeSet<i32>,
}

impl Exits {
    pub fn new() -> io::Result<Self> {
        // SAFETY: kqueue takes no arguments; a non-negative result is a new descriptor we own.
        let queue = unsafe { libc::kqueue() };
        if queue < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `queue` is a fresh descriptor owned by nothing else.
        let queue = unsafe { OwnedFd::from_raw_fd(queue) };
        Ok(Self { queue, pending: BTreeSet::new() })
    }

    /// Waits for `member`'s exit unless it is already gone or is no longer that process.
    pub fn watch(&mut self, member: &Member) -> io::Result<()> {
        let change = event(member.pid, libc::EV_ADD | libc::EV_ONESHOT);
        // SAFETY: one change from a valid kevent, no event list.
        let result = unsafe { libc::kevent(self.queue.as_raw_fd(), &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
        if result < 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::ESRCH) { Ok(()) } else { Err(error) };
        }
        if alive(member)? {
            self.pending.insert(member.pid);
        }
        Ok(())
    }

    /// Blocks until every watched process has exited.
    pub fn wait(&mut self) -> io::Result<()> {
        while !self.pending.is_empty() {
            let mut fired = event(0, 0);
            // SAFETY: room for exactly one event; no timeout, so it returns only on an event or a signal.
            let result = unsafe { libc::kevent(self.queue.as_raw_fd(), std::ptr::null(), 0, &mut fired, 1, std::ptr::null()) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if result == 1 {
                self.pending.remove(&i32::try_from(fired.ident).unwrap_or(-1));
            }
        }
        Ok(())
    }
}

fn event(pid: i32, flags: u16) -> libc::kevent {
    libc::kevent {
        ident: usize::try_from(pid).unwrap_or(0),
        filter: libc::EVFILT_PROC,
        flags,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    #[test]
    fn a_group_lists_its_live_members_and_its_exits_reap_nothing() {
        let mut child = Command::new("/bin/sleep").arg("600").process_group(0).stdin(Stdio::null()).spawn().unwrap();
        let group = i32::try_from(child.id()).unwrap();
        let listed = members(group).unwrap();
        assert_eq!(listed.iter().map(|member| member.pid).collect::<Vec<_>>(), vec![group]);
        assert!(alive(&listed[0]).unwrap());
        let mut exits = Exits::new().unwrap();
        exits.watch(&listed[0]).unwrap();
        kill_child(group);
        exits.wait().unwrap();
        assert!(!alive(&listed[0]).unwrap(), "an exited, unreaped child is a zombie");
        assert!(members(group).unwrap().iter().all(|member| member.zombie), "only the unreaped zombie stays in the group");
        child.wait().unwrap();
        assert!(members(group).unwrap().is_empty());
    }

    #[test]
    fn the_peer_of_a_socket_is_the_process_at_its_other_end() {
        let (here, _there) = std::os::unix::net::UnixStream::pair().unwrap();
        let me = peer(&here).unwrap().expect("this process is alive");
        assert_eq!(me.pid, i32::try_from(std::process::id()).unwrap());
        assert!(alive(&me).unwrap());
    }

    #[test]
    fn a_process_is_named_by_its_start_and_its_parent_is_found() {
        let mut child = Command::new("/bin/sleep").arg("600").stdin(Stdio::null()).spawn().unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        let named = process(pid).unwrap().expect("the child runs");
        assert!(alive(&named).unwrap());
        assert_eq!(parent(pid).unwrap().map(|parent| parent.pid), Some(i32::try_from(std::process::id()).unwrap()));
        kill_child(pid);
        child.wait().unwrap();
        assert!(!alive(&named).unwrap(), "a reaped process is gone");
        assert!(process(pid).unwrap().is_none_or(|now| now.started != named.started), "a reused PID is another process");
    }

    fn kill_child(pid: i32) {
        // SAFETY: kill on our own child's PID, before it is reaped.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    }
}
