//! Managed, content-addressed copies of domain document revisions.
use alondra_core::domain::{Command, CommandEnvelope, DomainError};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

const FOLDER: &str = "domain-v2";

pub(crate) struct Prepared {
    pub envelope: CommandEnvelope,
    created_digest: Option<String>,
    // Keep this cross-process lease until the SQLite commit or cleanup ends.
    // A second daemon must not reuse a new blob that the first can still remove.
    _lease: Option<File>,
}

impl Prepared {
    pub fn discard_uncommitted(&self, home: &Path) {
        if let Some(digest) = &self.created_digest {
            if let (Ok(folder), Ok(name)) = (managed_folder(home), CString::new(digest.as_str())) {
                unsafe {
                    libc::unlinkat(folder.as_raw_fd(), name.as_ptr(), 0);
                }
                let _ = folder.sync_all();
            }
        }
    }
}

pub(crate) fn prepare(home: &Path, envelope: &CommandEnvelope) -> Result<Prepared, DomainError> {
    let mut prepared = envelope.clone();
    let (digest, reference) = match &mut prepared.command {
        Command::CreateDocument {
            content_digest,
            content_reference,
            ..
        }
        | Command::ReviseDocument {
            content_digest,
            content_reference,
            ..
        } => (content_digest, content_reference),
        _ => {
            return Ok(Prepared {
                envelope: prepared,
                created_digest: None,
                _lease: None,
            });
        }
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(DomainError::new("invalid_input", "invalid document SHA256"));
    }
    let lease = document_lease(home).map_err(|_| {
        DomainError::new("content_unavailable", "document storage lease unavailable")
    })?;
    let (managed, created) = store_verified(home, reference, digest).map_err(|_| {
        DomainError::new(
            "content_unavailable",
            "document content must be an accessible stored file matching its SHA256",
        )
    })?;
    *reference = managed;
    let created_digest = created.then(|| digest.clone());
    Ok(Prepared {
        envelope: prepared,
        created_digest,
        _lease: Some(lease),
    })
}

fn document_lease(home: &Path) -> Result<File> {
    let folder = managed_folder(home)?;
    let name = CString::new(".lease")?;
    let lease = open_at(&folder, &name, libc::O_RDWR | libc::O_CREAT)?;
    let locked = unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX) };
    ensure!(locked == 0, "document storage lease failed");
    Ok(lease)
}

fn store_verified(home: &Path, reference: &str, digest: &str) -> Result<(String, bool)> {
    let folder = managed_folder(home)?;
    let target = CString::new(digest)?;
    let managed = format!("files/{FOLDER}/{digest}");
    // Retries use the already verified immutable copy, even when the original
    // upload has since been removed or overwritten.
    if let Ok(mut existing) = open_at(&folder, &target, libc::O_RDONLY) {
        verify(&mut existing, digest)?;
        return Ok((managed, false));
    }
    let source_path = Path::new(reference);
    let source_path = if source_path.is_absolute() {
        source_path.to_path_buf()
    } else {
        home.join(source_path)
    };
    let mut source = crate::service_files::open_stored_file(home, &source_path)?;
    ensure!(
        source.metadata()?.is_file(),
        "document source is not a file"
    );
    let temporary = CString::new(format!(".{}", crate::auth::random_hex(16)))?;
    let mut output = open_at(
        &folder,
        &temporary,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
    )?;
    let mut created = false;
    let result = (|| -> Result<()> {
        let mut hash = Sha256::new();
        let mut total = 0usize;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            total = total.checked_add(count).context("document size overflow")?;
            ensure!(
                total <= crate::uploads::MAX_UPLOAD_BYTES,
                "document exceeds file limit"
            );
            hash.update(&buffer[..count]);
            output.write_all(&buffer[..count])?;
        }
        ensure!(hex(&hash.finalize()) == digest, "document SHA256 mismatch");
        output.sync_all()?;
        let linked = unsafe {
            libc::linkat(
                folder.as_raw_fd(),
                temporary.as_ptr(),
                folder.as_raw_fd(),
                target.as_ptr(),
                0,
            )
        };
        if linked != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.into());
            }
        } else {
            created = true;
        }
        let mut installed = open_at(&folder, &target, libc::O_RDONLY)?;
        verify(&mut installed, digest)?;
        folder.sync_all()?;
        Ok(())
    })();
    unsafe {
        libc::unlinkat(folder.as_raw_fd(), temporary.as_ptr(), 0);
    }
    if result.is_err() && created {
        unsafe {
            libc::unlinkat(folder.as_raw_fd(), target.as_ptr(), 0);
        }
    }
    folder.sync_all()?;
    result?;
    Ok((managed, created))
}

fn managed_folder(home: &Path) -> Result<File> {
    let root = home.join("files");
    std::fs::create_dir_all(&root)?;
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(root)?;
    let name = CString::new(FOLDER)?;
    let created = unsafe { libc::mkdirat(root.as_raw_fd(), name.as_ptr(), 0o700) };
    if created != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error.into());
        }
    } else {
        root.sync_all()?;
    }
    open_at(&root, &name, libc::O_RDONLY | libc::O_DIRECTORY)
}

fn open_at(folder: &File, name: &CString, flags: libc::c_int) -> Result<File> {
    let fd = unsafe {
        libc::openat(
            folder.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    Ok(File::from(fd))
}

fn verify(file: &mut File, expected: &str) -> Result<()> {
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() <= crate::uploads::MAX_UPLOAD_BYTES as u64,
        "managed document is not a bounded regular file"
    );
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        ensure!(
            total <= crate::uploads::MAX_UPLOAD_BYTES as u64,
            "managed document exceeds limit"
        );
        hash.update(&buffer[..count]);
    }
    let after = file.metadata()?;
    ensure!(
        total == before.len()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec()
            && hex(&hash.finalize()) == expected,
        "managed document SHA256 mismatch"
    );
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::{CommandId, DataEpoch};
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn rejected_copy_cannot_remove_a_second_runtime_admission() {
        let home = std::env::temp_dir().join(format!(
            "alondra-domain-copy-lease-{}", crate::auth::random_hex(16)));
        std::fs::create_dir_all(home.join("files")).unwrap();
        let body = b"A reviewed document";
        std::fs::write(home.join("files/source"), body).unwrap();
        let digest = hex(&Sha256::digest(body));
        let command = CommandEnvelope {
            protocol_version: 2, data_epoch: DataEpoch("epoch".into()),
            command_id: CommandId("copy".into()), expected_revision: 0,
            command: Command::CreateDocument {
                task_id: "task".into(), document_id: "document".into(),
                title: "Document".into(), revision_id: "revision".into(),
                content_digest: digest.clone(), content_reference: "files/source".into(),
                role: "result".into(),
            },
        };
        let (first_ready_tx, first_ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first_home = home.clone();
        let first_command = command.clone();
        let first = std::thread::spawn(move || {
            let prepared = prepare(&first_home, &first_command).unwrap();
            first_ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            prepared.discard_uncommitted(&first_home);
        });
        first_ready_rx.recv().unwrap();
        let (second_ready_tx, second_ready_rx) = mpsc::channel();
        let (second_done_tx, second_done_rx) = mpsc::channel();
        let second_home = home.clone();
        let second = std::thread::spawn(move || {
            second_ready_tx.send(()).unwrap();
            let prepared = prepare(&second_home, &command).unwrap();
            second_done_tx.send(prepared).unwrap();
        });
        second_ready_rx.recv().unwrap();
        assert!(second_done_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "second admission must wait for the first commit/cleanup lease");
        release_tx.send(()).unwrap();
        first.join().unwrap();
        let prepared = second_done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(prepared.created_digest.is_some());
        assert_eq!(std::fs::read(home.join("files/domain-v2").join(digest)).unwrap(), body);
        second.join().unwrap();
        drop(prepared);
        std::fs::remove_dir_all(home).unwrap();
    }
}
