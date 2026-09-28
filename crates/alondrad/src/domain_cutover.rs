//! Offline promotion of a rehearsed domain image. The marker fences legacy
//! startup before any staged bytes enter HOME; it is committed only after the
//! new database and content have been verified. The startup fence is added by
//! the v2-only bootstrap integration; this module alone does not enforce it.
//! No live cutover is invoked by the CLI until the v2-only server and clients
//! are ready.
#[cfg(test)]
use alondra_core::home_lock::HomeLock;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
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
    let path = home.join(if initial {
        MARKER
    } else {
        "domain-v2-cutover.json.next"
    });
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    file.write_all(&serde_json::to_vec(marker)?)?;
    file.sync_all()?;
    if !initial {
        fs::rename(&path, home.join(MARKER))?;
    }
    File::open(home)?.sync_all()?;
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

fn promote_content(source: &Path, target: &Path) -> Result<()> {
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
            promote_content(&from, &to)?;
        } else {
            let expected = file_digest(&from)?;
            if to.exists() {
                ensure!(
                    file_digest(&to)? == expected,
                    "existing HOME content differs from stage"
                );
            } else {
                crate::domain_migration::copy_regular(&from, &to)?;
                ensure!(
                    file_digest(&to)? == expected,
                    "promoted content differs from stage"
                );
            }
        }
    }
    File::open(target)?.sync_all()?;
    Ok(())
}

/// Offline only. The caller must have stopped every old producer and retained
/// the verified backup and old daemon binary. The new daemon must still refuse
/// legacy startup when it sees either marker phase.
#[cfg(test)]
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
    let _lease = HomeLock::recovery(&home)?;
    ensure!(
        read_marker(&home)?.is_none(),
        "domain cutover already started"
    );
    ensure!(
        !home.join("domain-v2.db").exists(),
        "HOME already has a domain database"
    );
    let manifest =
        crate::domain_migration::verify_backup_expected(&backup, expected_backup_digest)?;
    ensure!(
        manifest.canonical_source_home == home.to_string_lossy(),
        "backup belongs to another HOME"
    );
    ensure!(
        crate::domain_migration::load_legacy_readonly(&home.join("alondra.db"))?.seq
            == manifest.source_seq,
        "legacy state advanced after backup"
    );
    let staged = crate::domain_runtime::DomainRuntime::open(&stage)?.state()?;
    ensure!(
        staged.revision > 0,
        "stage contains no imported domain state"
    );
    let marker = Marker {
        format: 1,
        phase: Phase::Prepared,
        backup_manifest_sha256: expected_backup_digest.to_owned(),
        source_seq: manifest.source_seq,
        data_epoch: staged.data_epoch.0.clone(),
    };
    write_marker(&home, &marker, true)?;
    // The marker now fences all new daemon starts. Any failure leaves Prepared
    // in place for explicit recovery instead of resuming a stale legacy writer.
    crate::domain_migration::sqlite_backup(
        &stage.join("domain-v2.db"),
        &home.join("domain-v2.db"),
    )?;
    if stage.join("files").exists() {
        promote_content(&stage.join("files"), &home.join("files"))?;
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
    write_marker(&home, &committed, false)?;
    Ok(committed)
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
        write_marker(&home, &marker, true).unwrap();
        assert_eq!(read_marker(&home).unwrap(), Some(marker));
        fs::set_permissions(home.join(MARKER), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_marker(&home).is_err());
        fs::set_permissions(home.join(MARKER), fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(home.join(MARKER), b"malformed").unwrap();
        assert!(read_marker(&home).is_err());
        fs::remove_dir_all(home).unwrap();
    }
}
