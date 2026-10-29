//! Cross-process serialization of User approval and domain commits.
//!
//! Domain writers take a shared lease before checking approval and hold it through
//! the SQLite commit. Approval/enrollment writers take an exclusive lease through
//! their durable write. A separate process can therefore revoke a device without
//! a domain command committing after that revocation has returned.
//!
//! Lock order: domain store → approval lease. Holding rules that keep the daemon live:
//! - A shared lease may be held across an `.await` (a relay delivery holds it through its
//!   upload). An exclusive lease never is, and its holder never takes the domain store, so a
//!   wait for the lease under the store is always bounded by synchronous work.
//! - Inside this process a shared lease never waits for another shared lease. macOS `flock`
//!   wakes one waiter when an exclusive lock is released and parks the others behind that
//!   waiter's lock, so two shared `flock` waiters can queue behind each other. Shared holders
//!   therefore share one `flock` per file, counted here, and an exclusive holder waits for the
//!   count to reach zero before it touches `flock`. The kernel sees at most one lock from this
//!   process per file.
use crate::locks::{Poisonable as _, AfterPanic as _};
use anyhow::{Context, Result, ensure};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, Weak};

pub(crate) struct ApprovalLease {
    gate: Arc<Gate>,
    exclusive: Option<File>,
}

#[derive(Default)]
struct Gate {
    /// A holder that panics leaves it whole, so it is recovered: each step changes one field, and a
    /// failed `flock` returns before it counts or undoes the exclusive flag.
    holders: Mutex<Holders>,
    released: Condvar,
}

#[derive(Default)]
struct Holders {
    shared: usize,
    shared_lock: Option<File>,
    exclusive: bool,
}

/// A holder that panics leaves it whole, so it is recovered: weak pointers, pruned and inserted one
/// by one.
static GATES: LazyLock<Mutex<HashMap<(u64, u64), Weak<Gate>>>> = LazyLock::new(Default::default);

impl ApprovalLease {
    pub(crate) fn shared(home: &Path) -> Result<Self> {
        let (file, gate) = open(home)?;
        let mut holders = gate.wait_while(|holders| holders.exclusive);
        if holders.shared == 0 {
            flock(&file, libc::LOCK_SH)?;
            holders.shared_lock = Some(file);
        }
        holders.shared += 1;
        drop(holders);
        Ok(Self { gate, exclusive: None })
    }

    pub(crate) fn exclusive(home: &Path) -> Result<Self> {
        let (file, gate) = open(home)?;
        let mut holders = gate.wait_while(|holders| holders.exclusive || holders.shared > 0);
        holders.exclusive = true;
        drop(holders);
        if let Err(error) = flock(&file, libc::LOCK_EX) {
            gate.holders().exclusive = false;
            gate.released.notify_all();
            return Err(error);
        }
        Ok(Self { gate, exclusive: Some(file) })
    }
}

impl Gate {
    fn holders(&self) -> MutexGuard<'_, Holders> {
        self.holders.lock_or_recover("the approval lease holders")
    }

    fn wait_while(&self, busy: impl FnMut(&mut Holders) -> bool) -> MutexGuard<'_, Holders> {
        self.released
            .wait_while(self.holders(), busy)
            .or_recover("the approval lease holders", || self.holders.clear_poison())
    }
}

fn open(home: &Path) -> Result<(File, Arc<Gate>)> {
    let path = home.join("domain-approval.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .with_context(|| format!("cannot open approval lease {}", path.display()))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.nlink() == 1,
        "approval lease is not a private regular file"
    );
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "approval lease has another owner"
    );
    ensure!(
        metadata.mode() & 0o077 == 0,
        "approval lease is not private"
    );
    let mut gates = GATES.lock_or_recover("the approval lease gates");
    gates.retain(|_, gate| gate.strong_count() > 0);
    let gate = gates
        .get(&(metadata.dev(), metadata.ino()))
        .and_then(Weak::upgrade)
        .unwrap_or_default();
    gates.insert((metadata.dev(), metadata.ino()), Arc::downgrade(&gate));
    Ok((file, gate))
}

fn flock(file: &File, operation: libc::c_int) -> Result<()> {
    loop {
        // SAFETY: fd remains owned by `file`, and operation is one of LOCK_SH/LOCK_EX.
        if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
}

fn unlock(file: File) {
    // Closing also releases flock; explicit unlock makes the lease boundary clear.
    unsafe {
        libc::flock(file.as_raw_fd(), libc::LOCK_UN);
    }
}

impl Drop for ApprovalLease {
    fn drop(&mut self) {
        let mut holders = self.gate.holders();
        if let Some(file) = self.exclusive.take() {
            unlock(file);
            holders.exclusive = false;
        } else {
            holders.shared -= 1;
            if holders.shared == 0
                && let Some(file) = holders.shared_lock.take()
            {
                unlock(file);
            }
        }
        drop(holders);
        self.gate.released.notify_all();
    }
}
