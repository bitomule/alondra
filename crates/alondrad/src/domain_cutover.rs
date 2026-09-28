//! Offline promotion of a rehearsed domain image. The marker fences legacy
//! startup before any staged bytes enter HOME; it is committed only after the
//! new database and content have been verified. Startup enforces both marker
//! phases before opening any legacy store or route.
use alondra_core::home_lock::HomeLock;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub(crate) const MARKER: &str = "domain-v2-cutover.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Prepared,
    Committed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Marker {
    pub format: u32,
    pub phase: Phase,
    pub backup_manifest_sha256: String,
    pub source_seq: u64,
    pub data_epoch: String,
}

/// Call while holding the HOME lock, before opening Hub or starting any legacy
/// loop. A malformed marker must stop startup rather than select legacy mode.
pub(crate) fn read_marker(home: &Path) -> Result<Option<Marker>> {
    let path = home.join(MARKER);
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.nlink() == 1
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.permissions().mode() & 0o077 == 0,
        "unsafe domain cutover marker"
    );
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 4096, "domain cutover marker too large");
    let marker: Marker = serde_json::from_slice(&bytes).context("invalid domain cutover marker")?;
    ensure!(marker.format == 1, "unsupported domain cutover marker");
    ensure!(
        marker.backup_manifest_sha256.len() == 64
            && marker
                .backup_manifest_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "invalid domain cutover backup digest"
    );
    ensure!(
        marker.data_epoch.len() == 64
            && marker
                .data_epoch
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "invalid domain cutover epoch"
    );
    Ok(Some(marker))
}

fn write_marker(home: &Path, marker: &Marker, initial: bool) -> Result<()> {
    let path = home.join("domain-v2-cutover.json.next");
    if initial {
        ensure!(
            read_marker(home)?.is_none(),
            "domain cutover marker already exists"
        );
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                ensure!(
                    meta.is_file() && meta.nlink() == 1 && meta.uid() == unsafe { libc::geteuid() },
                    "unsafe interrupted cutover marker"
                );
                fs::remove_file(&path)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    file.write_all(&serde_json::to_vec(marker)?)?;
    file.sync_all()?;
    if initial {
        rename_exclusive(&path, &home.join(MARKER))?;
    } else {
        fs::rename(&path, home.join(MARKER))?;
    }
    File::open(home)?.sync_all()?;
    Ok(())
}

/// Publish a fully synced file without replacing a writer's newly-created
/// target between preflight and publication.
fn rename_exclusive(from: &Path, to: &Path) -> Result<()> {
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    #[cfg(target_os = "macos")]
    let status = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(target_os = "linux")]
    let status = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    compile_error!("domain cutover requires an exclusive rename implementation");
    if status != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn file_digest(path: &Path) -> Result<(u64, String)> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && meta.nlink() == 1 && meta.uid() == unsafe { libc::geteuid() },
        "unsafe domain stage file"
    );
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut digest = Sha256::new();
    let mut bytes = 0;
    let mut buffer = [0u8; 65536];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        digest.update(&buffer[..count]);
    }
    Ok((
        bytes,
        digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    ))
}

fn validate_owned_sqlite_files(path: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let candidate = path.with_file_name(format!(
            "{}{suffix}",
            path.file_name()
                .context("SQLite path has no filename")?
                .to_string_lossy()
        ));
        match fs::symlink_metadata(&candidate) {
            Ok(meta) => ensure!(
                meta.is_file() && meta.nlink() == 1 && meta.uid() == unsafe { libc::geteuid() },
                "unsafe cutover SQLite file: {}",
                candidate.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Remove the base before its journals while Prepared fences daemon startup.
/// If interrupted, another Prepared pass removes the orphan journals.
fn remove_owned_sqlite_files(path: &Path) -> Result<()> {
    validate_owned_sqlite_files(path)?;
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let candidate = path.with_file_name(format!(
            "{}{suffix}",
            path.file_name()
                .context("SQLite path has no filename")?
                .to_string_lossy()
        ));
        match fs::remove_file(&candidate) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    File::open(path.parent().context("SQLite path has no parent")?)?.sync_all()?;
    Ok(())
}

fn verify_stage_provenance(
    stage: &Path,
    backup_digest: &str,
    source_seq: u64,
    state: &alondra_core::domain_v2::DomainState,
) -> Result<()> {
    let path = stage.join("domain-v2-stage.json");
    let meta = fs::symlink_metadata(&path)?;
    ensure!(
        meta.is_file()
            && meta.nlink() == 1
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.permissions().mode() & 0o077 == 0,
        "unsafe domain stage provenance"
    );
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 4096, "domain stage provenance too large");
    let provenance: crate::domain_migration::StageProvenance = serde_json::from_slice(&bytes)?;
    ensure!(
        provenance.format == 1
            && provenance.backup_manifest_sha256 == backup_digest
            && provenance.source_seq == source_seq
            && provenance.data_epoch == state.data_epoch.0
            && provenance.domain_state_sha256
                == crate::domain_migration::domain_state_digest(state)?
            && provenance.import_map_sha256
                == crate::domain_migration::import_map_digest(&stage.join("domain-v2.db"))?,
        "stage does not match the verified backup and imported domain state"
    );
    Ok(())
}

fn verify_no_stage_effects(database: &Path) -> Result<()> {
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    for table in [
        "semantic_receipts",
        "local_command_results",
        "command_audit",
        "domain_messages",
        "domain_outbox",
        "delivery_attempts",
    ] {
        let count: i64 =
            connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })?;
        ensure!(
            count == 0,
            "stage contains live command or delivery rows in {table}"
        );
    }
    let invalid: i64 =
        connection.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    ensure!(invalid == 0, "stage has broken foreign keys");
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    ensure!(integrity == "ok", "stage database integrity check failed");
    Ok(())
}

const CONTENT_NEXT: &str = "domain-v2-content-next";

fn clean_content_next(home: &Path) -> Result<()> {
    let directory = home.join(CONTENT_NEXT);
    let meta = match fs::symlink_metadata(&directory) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        meta.is_dir()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.permissions().mode() & 0o077 == 0,
        "unsafe interrupted content staging directory"
    );
    for entry in fs::read_dir(&directory)? {
        let path = entry?.path();
        let meta = fs::symlink_metadata(&path)?;
        ensure!(
            meta.is_file() && meta.nlink() == 1 && meta.uid() == unsafe { libc::geteuid() },
            "unsafe interrupted content staging file"
        );
        fs::remove_file(path)?;
    }
    fs::remove_dir(directory)?;
    File::open(home)?.sync_all()?;
    Ok(())
}

fn promote_content(source: &Path, target: &Path, staging: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(source)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() },
        "unsafe stage content directory"
    );
    match fs::create_dir(target) {
        Ok(()) => fs::set_permissions(target, fs::Permissions::from_mode(0o700))?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let meta = fs::symlink_metadata(target)?;
            ensure!(
                meta.is_dir() && meta.uid() == unsafe { libc::geteuid() },
                "unsafe existing content directory"
            );
        }
        Err(error) => return Err(error.into()),
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            promote_content(&from, &to, staging)?;
        } else {
            let expected = file_digest(&from)?;
            match fs::symlink_metadata(&to) {
                Ok(_) => ensure!(
                    file_digest(&to)? == expected,
                    "existing HOME content differs from stage"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let temporary = staging.join(crate::auth::random_hex(16));
                    crate::domain_migration::copy_regular(&from, &temporary)?;
                    rename_exclusive(&temporary, &to)?;
                    ensure!(
                        file_digest(&to)? == expected,
                        "promoted content differs from stage"
                    );
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    File::open(target)?.sync_all()?;
    Ok(())
}

fn preflight_content(source: &Path, target: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(source)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() },
        "unsafe staged content directory"
    );
    match fs::symlink_metadata(target) {
        Ok(meta) => ensure!(
            meta.is_dir() && meta.uid() == unsafe { libc::geteuid() },
            "unsafe existing content directory"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            preflight_content(&from, &to)?;
        } else {
            let expected = file_digest(&from)?;
            match fs::symlink_metadata(&to) {
                Ok(_) => ensure!(
                    file_digest(&to)? == expected,
                    "existing HOME content differs from stage"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

/// Offline only. The caller must have stopped every old producer and retained
/// the verified backup and old daemon binary. The new daemon must still refuse
/// legacy startup when it sees either marker phase.
pub(crate) fn promote_stage_offline(
    home: &Path,
    backup: &Path,
    expected_backup_digest: &str,
    stage: &Path,
) -> Result<Marker> {
    ensure!(
        home.is_absolute() && backup.is_absolute() && stage.is_absolute(),
        "cutover paths must be absolute"
    );
    let home = home.canonicalize()?;
    let backup = backup.canonicalize()?;
    let stage = stage.canonicalize()?;
    ensure!(
        !backup.starts_with(&home) && !stage.starts_with(&home),
        "cutover sources must be outside HOME"
    );
    ensure!(
        !home.starts_with(&backup) && !home.starts_with(&stage),
        "HOME cannot be inside cutover source"
    );
    let lease = HomeLock::recovery(&home)?;
    let prior = read_marker(&home)?;
    ensure!(
        prior
            .as_ref()
            .is_none_or(|marker| marker.phase == Phase::Prepared),
        "domain cutover already committed"
    );
    if prior.is_some() {
        clean_content_next(&home)?;
    } else {
        ensure!(
            fs::symlink_metadata(home.join(CONTENT_NEXT))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
            "unowned domain content staging directory"
        );
    }
    let manifest =
        crate::domain_migration::verify_backup_expected(&backup, expected_backup_digest)?;
    ensure!(
        manifest.canonical_source_home == home.to_string_lossy(),
        "backup belongs to another HOME"
    );
    crate::domain_migration::verify_current_source_matches_backup(
        &lease,
        &home,
        &backup,
        &manifest,
        prior.as_ref().map(|_| stage.as_path()),
        false,
    )?;
    let staged = crate::domain_runtime::DomainRuntime::open(&stage)?.state()?;
    ensure!(
        staged.revision > 0,
        "stage contains no imported domain state"
    );
    verify_stage_provenance(&stage, expected_backup_digest, manifest.source_seq, &staged)?;
    verify_no_stage_effects(&stage.join("domain-v2.db"))?;
    for revision in staged.document_revisions.values() {
        crate::domain_migration::verify_content(
            &stage,
            &revision.content_reference,
            &revision.content_digest,
        )?;
    }
    for interaction in staged.interactions.values() {
        for attachment in &interaction.attachments {
            crate::domain_migration::verify_content(
                &stage,
                &attachment.content_reference,
                &attachment.content_digest,
            )?;
        }
    }
    if stage.join("files").exists() {
        preflight_content(&stage.join("files"), &home.join("files"))?;
    }
    let marker = Marker {
        format: 1,
        phase: Phase::Prepared,
        backup_manifest_sha256: expected_backup_digest.to_owned(),
        source_seq: manifest.source_seq,
        data_epoch: staged.data_epoch.0.clone(),
    };
    if let Some(prior) = prior {
        ensure!(
            prior == marker,
            "prepared cutover belongs to a different backup or stage"
        );
    } else {
        validate_owned_sqlite_files(&home.join("domain-v2.db"))?;
        ensure!(
            crate::legacy_sidecar_is_empty(&home.join("domain-v2.db"))?,
            "HOME already has domain data"
        );
        write_marker(&home, &marker, true)?;
    }
    // The marker now fences all new daemon starts. Any failure leaves Prepared
    // in place for explicit recovery instead of resuming a stale legacy writer.
    let database = home.join("domain-v2.db");
    let next = home.join("domain-v2.db.next");
    validate_owned_sqlite_files(&database)?;
    if crate::legacy_sidecar_is_empty(&database)? {
        remove_owned_sqlite_files(&database)?;
    }
    if !database.exists() {
        remove_owned_sqlite_files(&next)?;
        crate::domain_migration::sqlite_backup(&stage.join("domain-v2.db"), &next)?;
        rename_exclusive(&next, &database)?;
        File::open(&home)?.sync_all()?;
    }
    verify_no_stage_effects(&home.join("domain-v2.db"))?;
    ensure!(
        crate::domain_migration::import_map_digest(&home.join("domain-v2.db"))?
            == crate::domain_migration::import_map_digest(&stage.join("domain-v2.db"))?,
        "promoted legacy import mapping differs from stage"
    );
    if stage.join("files").exists() {
        let staging = home.join(CONTENT_NEXT);
        fs::DirBuilder::new().mode(0o700).create(&staging)?;
        promote_content(&stage.join("files"), &home.join("files"), &staging)?;
        clean_content_next(&home)?;
    }
    let installed = crate::domain_runtime::DomainRuntime::open(&home)?.state()?;
    ensure!(
        installed == staged,
        "promoted domain state differs from stage"
    );
    for document in installed.document_revisions.values() {
        crate::domain_migration::verify_content(
            &home,
            &document.content_reference,
            &document.content_digest,
        )?;
    }
    for interaction in installed.interactions.values() {
        for attachment in &interaction.attachments {
            crate::domain_migration::verify_content(
                &home,
                &attachment.content_reference,
                &attachment.content_digest,
            )?;
        }
    }
    let mut committed = marker.clone();
    committed.phase = Phase::Committed;
    let interrupted_marker = home.join("domain-v2-cutover.json.next");
    match fs::symlink_metadata(&interrupted_marker) {
        Ok(meta) => {
            ensure!(
                meta.is_file() && meta.nlink() == 1 && meta.uid() == unsafe { libc::geteuid() },
                "unsafe interrupted cutover marker"
            );
            fs::remove_file(&interrupted_marker)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    write_marker(&home, &committed, false)?;
    Ok(committed)
}

/// Reopen a legacy HOME after an interrupted Prepared promotion. Copied
/// content is retained so abort never discards another process's file.
pub(crate) fn abort_prepared_offline(
    home: &Path,
    backup: &Path,
    expected_backup_digest: &str,
) -> Result<()> {
    ensure!(
        home.is_absolute() && backup.is_absolute(),
        "cutover paths must be absolute"
    );
    let home = home.canonicalize()?;
    let backup = backup.canonicalize()?;
    ensure!(
        !backup.starts_with(&home) && !home.starts_with(&backup),
        "backup must be outside HOME"
    );
    let lease = HomeLock::recovery(&home)?;
    let marker = read_marker(&home)?.context("HOME has no prepared domain cutover")?;
    ensure!(
        marker.phase == Phase::Prepared,
        "committed domain cutover cannot be aborted"
    );
    ensure!(
        marker.backup_manifest_sha256 == expected_backup_digest,
        "prepared cutover belongs to a different backup"
    );
    let manifest =
        crate::domain_migration::verify_backup_expected(&backup, expected_backup_digest)?;
    ensure!(
        manifest.canonical_source_home == home.to_string_lossy()
            && manifest.source_seq == marker.source_seq,
        "backup does not match prepared HOME"
    );
    crate::domain_migration::verify_current_source_matches_backup(
        &lease, &home, &backup, &manifest, None, true,
    )?;
    clean_content_next(&home)?;
    let database = home.join("domain-v2.db");
    validate_owned_sqlite_files(&database)?;
    match fs::symlink_metadata(&database) {
        Ok(meta) => {
            ensure!(
                meta.is_file() && meta.nlink() == 1 && meta.uid() == unsafe { libc::geteuid() },
                "unsafe prepared domain database"
            );
            if !crate::legacy_sidecar_is_empty(&database)? {
                verify_no_stage_effects(&database)?;
                let state = crate::domain_runtime::DomainRuntime::open(&home)?.state()?;
                ensure!(
                    state.data_epoch.0 == marker.data_epoch && state.revision > 0,
                    "prepared database does not match cutover marker"
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    validate_owned_sqlite_files(&home.join("domain-v2.db.next"))?;
    for name in ["domain-v2-cutover.json.next"] {
        let path = home.join(name);
        match fs::symlink_metadata(&path) {
            Ok(meta) => ensure!(
                meta.is_file() && meta.nlink() == 1 && meta.uid() == unsafe { libc::geteuid() },
                "unsafe interrupted cutover file"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    remove_owned_sqlite_files(&home.join("domain-v2.db.next"))?;
    for name in ["domain-v2-cutover.json.next"] {
        let path = home.join(name);
        if fs::symlink_metadata(&path).is_ok() {
            fs::remove_file(path)?;
        }
    }
    remove_owned_sqlite_files(&database)?;
    File::open(&home)?.sync_all()?;
    fs::remove_file(home.join(MARKER))?;
    File::open(&home)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_is_bounded_private_and_fail_closed() {
        let home = std::env::temp_dir().join(format!(
            "alondra-domain-cutover-marker-{}",
            crate::auth::random_hex(8)
        ));
        fs::create_dir(&home).unwrap();
        assert!(read_marker(&home).unwrap().is_none());
        let marker = Marker {
            format: 1,
            phase: Phase::Prepared,
            backup_manifest_sha256: "a".repeat(64),
            source_seq: 73,
            data_epoch: "b".repeat(64),
        };
        fs::write(
            home.join("domain-v2-cutover.json.next"),
            b"interrupted initial write",
        )
        .unwrap();
        write_marker(&home, &marker, true).unwrap();
        assert!(!home.join("domain-v2-cutover.json.next").exists());
        assert_eq!(read_marker(&home).unwrap(), Some(marker));
        fs::set_permissions(home.join(MARKER), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_marker(&home).is_err());
        fs::set_permissions(home.join(MARKER), fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(home.join(MARKER), b"malformed").unwrap();
        assert!(read_marker(&home).is_err());
        fs::remove_dir_all(home).unwrap();
    }
}
