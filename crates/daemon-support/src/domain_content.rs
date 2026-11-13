//! Managed, content-addressed copies of domain document revisions.
use alondra_core::domain::{AttachmentRef, Command, CommandEnvelope, DomainError};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const FOLDER: &str = "domain-v2";

pub struct Prepared {
    pub envelope: CommandEnvelope,
    created_digests: Vec<String>,
    // Keep this cross-process lease until the SQLite commit or cleanup ends.
    // A second daemon must not reuse a new blob that the first can still remove.
    _lease: Option<File>,
}

impl Prepared {
    pub fn discard_uncommitted(&self, home: &Path) {
        for digest in &self.created_digests {
            if let (Ok(folder), Ok(name)) = (managed_folder(home), CString::new(digest.as_str())) {
                unsafe {
                    libc::unlinkat(folder.as_raw_fd(), name.as_ptr(), 0);
                }
                let _ = folder.sync_all();
            }
        }
    }
}

/// Copies what a command stores into Alondra's own folder, named by its SHA256: a document's
/// content and the files a node attaches to a card. `source_roots` are the folders besides Alondra's
/// files folder an attachment may be read from: the worktrees of its tasks. Whatever is not a
/// match of its SHA256 refuses the command here, never when someone opens the card.
pub fn prepare(home: &Path, envelope: &CommandEnvelope, source_roots: &[PathBuf]) -> Result<Prepared, DomainError> {
    let mut prepared = envelope.clone();
    if node_attachments(&mut prepared.command).is_some() {
        return prepare_attachments(home, prepared, source_roots);
    }
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
                created_digests: vec![],
                _lease: None,
            });
        }
    };
    if !is_digest(digest) {
        return Err(DomainError::new("invalid_input", "invalid document SHA256"));
    }
    let lease = document_lease(home).map_err(|_| {
        DomainError::new("content_unavailable", "document storage lease unavailable")
    })?;
    let (managed, created) = store_verified(home, reference, digest, &[]).map_err(|_| {
        DomainError::new(
            "content_unavailable",
            "document content must be an accessible stored file matching its SHA256",
        )
    })?;
    *reference = managed;
    let created_digests = created.then(|| digest.clone()).into_iter().collect();
    Ok(Prepared {
        envelope: prepared,
        created_digests,
        _lease: Some(lease),
    })
}

/// Keeps a copy of an attachment recorded before cards owned theirs: the reference it should
/// have, or why nothing could be kept.
pub fn keep_attachment(home: &Path, reference: &str, digest: &str, source_roots: &[PathBuf]) -> Result<String> {
    ensure!(is_digest(digest), "invalid attachment SHA256");
    let _lease = document_lease(home)?;
    Ok(store_verified(home, reference, digest, source_roots)?.0)
}

fn node_attachments(command: &mut Command) -> Option<&mut Vec<AttachmentRef>> {
    match command {
        Command::PublishInformation { attachments, .. }
        | Command::AskQuestion { attachments, .. }
        | Command::RequestAction { attachments, .. }
        | Command::RaiseProblem { attachments, .. }
        | Command::PresentResult { attachments, .. } => Some(attachments).filter(|list| !list.is_empty()),
        _ => None,
    }
}

fn prepare_attachments(home: &Path, mut prepared: CommandEnvelope, source_roots: &[PathBuf]) -> Result<Prepared, DomainError> {
    let lease = document_lease(home).map_err(|_| {
        DomainError::new("content_unavailable", "attachment storage lease unavailable")
    })?;
    let mut created_digests = vec![];
    let result = (|| {
        let attachments = node_attachments(&mut prepared.command).expect("the command carries attachments");
        for attachment in attachments.iter_mut() {
            if !is_digest(&attachment.content_digest) {
                return Err(DomainError::new("invalid_input", format!("attachment {} has an invalid SHA256", attachment.id)));
            }
            let (managed, created) = store_verified(home, &attachment.content_reference, &attachment.content_digest, source_roots)
                .map_err(|_| DomainError::new("content_unavailable", format!(
                    "attachment {} must be an accessible file matching its SHA256 when the card is presented: {} no longer holds the content it was described with",
                    attachment.id, attachment.name)))?;
            if created {
                created_digests.push(attachment.content_digest.clone());
            }
            attachment.content_reference = managed;
        }
        Ok(())
    })();
    let prepared = Prepared { envelope: prepared, created_digests, _lease: Some(lease) };
    match result {
        Ok(()) => Ok(prepared),
        Err(error) => {
            prepared.discard_uncommitted(home);
            Err(error)
        }
    }
}

fn is_digest(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The file a stored reference names: inside Alondra's files folder, or inside one of `roots`.
fn open_source(home: &Path, path: &Path, roots: &[PathBuf]) -> Result<File> {
    if let Ok(file) = crate::service_files::open_stored_file(home, path) {
        return Ok(file);
    }
    let canonical = path.canonicalize()?;
    ensure!(
        roots.iter().filter_map(|root| root.canonicalize().ok()).any(|root| canonical.starts_with(root)),
        "attachment source is outside Alondra's folders and the task's worktrees"
    );
    Ok(File::open(canonical)?)
}

fn document_lease(home: &Path) -> Result<File> {
    let folder = managed_folder(home)?;
    let name = CString::new(".lease")?;
    let lease = open_at(&folder, &name, libc::O_RDWR | libc::O_CREAT)?;
    let locked = unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX) };
    ensure!(locked == 0, "document storage lease failed");
    Ok(lease)
}

fn store_verified(home: &Path, reference: &str, digest: &str, source_roots: &[PathBuf]) -> Result<(String, bool)> {
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
    let mut source = open_source(home, &source_path, source_roots)?;
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
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!(
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
            let prepared = prepare(&first_home, &first_command, &[]).unwrap();
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
            let prepared = prepare(&second_home, &command, &[]).unwrap();
            second_done_tx.send(prepared).unwrap();
        });
        second_ready_rx.recv().unwrap();
        assert!(second_done_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "second admission must wait for the first commit/cleanup lease");
        release_tx.send(()).unwrap();
        first.join().unwrap();
        let prepared = second_done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!prepared.created_digests.is_empty());
        assert_eq!(std::fs::read(home.join("files/domain-v2").join(digest)).unwrap(), body);
        second.join().unwrap();
        drop(prepared);
        std::fs::remove_dir_all(home).unwrap();
    }

    fn present(attachments: Vec<AttachmentRef>) -> CommandEnvelope {
        CommandEnvelope {
            protocol_version: 2, data_epoch: DataEpoch("epoch".into()),
            command_id: CommandId("present".into()), expected_revision: 0,
            command: Command::PresentResult {
                attachments, task_id: "task".into(), interaction_id: "result".into(), body: "Done".into(),
                targets: vec![], is_final: false, nothing_to_review: false, checklist: vec![],
                card: alondra_core::domain::CardSpec::new(alondra_core::domain::CardKind::Review, "Done"),
            },
        }
    }

    fn reference(name: &str, path: &Path, body: &[u8]) -> AttachmentRef {
        AttachmentRef {
            id: name.into(), name: name.into(), content_digest: hex(&Sha256::digest(body)),
            content_reference: path.display().to_string(), bytes: body.len() as u64,
        }
    }

    fn carried(envelope: &CommandEnvelope) -> Vec<AttachmentRef> {
        match &envelope.command {
            Command::PresentResult { attachments, .. } => attachments.clone(),
            _ => unreachable!(),
        }
    }

    #[test]
    fn an_attachment_outlives_the_file_it_was_presented_from() {
        let scratch = crate::test_support::Directory::new();
        let home = scratch.join("home");
        let worktree = scratch.join("worktree");
        std::fs::create_dir_all(home.join("files")).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        let body = b"The validation report";
        let source = worktree.join("report.md");
        std::fs::write(&source, body).unwrap();
        let prepared = prepare(&home, &present(vec![reference("report.md", &source, body)]), std::slice::from_ref(&worktree)).unwrap();
        let stored = carried(&prepared.envelope).remove(0);
        std::fs::write(&source, b"Regenerated after presenting").unwrap();
        std::fs::remove_dir_all(&worktree).unwrap();
        assert!(stored.content_reference.starts_with("files/domain-v2/"), "{}", stored.content_reference);
        assert_eq!(std::fs::read(home.join(&stored.content_reference)).unwrap(), body);
    }

    #[test]
    fn an_attachment_that_no_longer_matches_its_digest_refuses_the_card() {
        let scratch = crate::test_support::Directory::new();
        let home = scratch.join("home");
        let worktree = scratch.join("worktree");
        std::fs::create_dir_all(home.join("files")).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        let source = worktree.join("report.md");
        std::fs::write(&source, b"First version").unwrap();
        let described = reference("report.md", &source, b"First version");
        std::fs::write(&source, b"Second version").unwrap();
        let error = prepare(&home, &present(vec![described]), std::slice::from_ref(&worktree)).err().expect("refused");
        assert_eq!(error.code, "content_unavailable");
        assert!(error.message.contains("report.md"), "{}", error.message);
        let left: Vec<_> = home.join("files/domain-v2").read_dir().unwrap().map(|entry| entry.unwrap().file_name()).filter(|name| is_digest(&name.to_string_lossy())).collect();
        assert!(left.is_empty(), "nothing is left behind: {left:?}");
    }

    #[test]
    fn an_attachment_outside_the_task_folders_is_refused() {
        let scratch = crate::test_support::Directory::new();
        let home = scratch.join("home");
        let elsewhere = scratch.join("elsewhere");
        std::fs::create_dir_all(home.join("files")).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let source = elsewhere.join("secret.txt");
        std::fs::write(&source, b"Not for the card").unwrap();
        let error = prepare(&home, &present(vec![reference("secret.txt", &source, b"Not for the card")]), &[]).err().expect("refused");
        assert_eq!(error.code, "content_unavailable");
    }
}
