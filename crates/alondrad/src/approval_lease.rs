//! Cross-process serialization of User approval and domain commits.
//!
//! Domain writers take a shared lease before checking approval and hold it through
//! the SQLite commit. Approval/enrollment writers take an exclusive lease through
//! their durable write. A separate process can therefore revoke a device without
//! a domain command committing after that revocation has returned.
use anyhow::{Context, Result, ensure};
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

pub(crate) struct ApprovalLease(File);

impl ApprovalLease {
    pub(crate) fn shared(home: &Path) -> Result<Self> {
        Self::acquire(home, libc::LOCK_SH)
    }
    pub(crate) fn exclusive(home: &Path) -> Result<Self> {
        Self::acquire(home, libc::LOCK_EX)
    }

    fn acquire(home: &Path, operation: libc::c_int) -> Result<Self> {
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
        loop {
            // SAFETY: fd remains owned by `file`, and operation is one of LOCK_SH/LOCK_EX.
            if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.into());
            }
        }
        Ok(Self(file))
    }
}

impl Drop for ApprovalLease {
    fn drop(&mut self) {
        // Closing also releases flock; explicit unlock makes the lease boundary clear.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
