//! Crash-safe migration from the former database filename to the canonical one.
//!
//! The SQLite backup API copies a consistent view, including committed WAL
//! frames. The old database remains in place as a private recovery copy after
//! the new copy has been verified and a committed marker is durable. Attachments are deliberately
//! left in place: their references are stored in this database and remain
//! valid when the database is copied to its canonical filename.
use anyhow::{Context, Result, ensure};
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) const DATABASE: &str = "domain.db";
const SOURCE: &str = "domain-v2.db";
const NEXT: &str = "domain.db.next";
pub(crate) const MARKER: &str = "database-migration.json";
const APPLICATION_ID: i64 = 0x414c4432;

pub(crate) fn active_path(home: &Path) -> std::path::PathBuf {
    let canonical = home.join(DATABASE);
    #[cfg(test)]
    if !canonical.exists() {
        // Existing unit fixtures predate the filename migration.
        return home.join(SOURCE);
    }
    #[cfg(not(test))]
    {
        let committed = crate::domain_cutover::read_marker(home)
            .ok()
            .flatten()
            .is_some_and(|marker| marker.phase == crate::domain_cutover::Phase::Committed);
        if !committed && !home.join(MARKER).exists() {
            return home.join(SOURCE);
        }
    }
    canonical
}

/// A private preflight snapshot uses SQLite backup, so its source file bytes
/// differ from the live HOME even though the logical database is identical.
/// Rebind only the snapshot's physical digest; retain the logical digest.
pub(crate) fn validate_preflight_source(home: &Path) -> Result<()> {
    let Some(marker) = read_marker(home)? else {
        return Ok(());
    };
    let source = home.join(SOURCE);
    if path_exists(&source) {
        let (epoch, revision) = inspect_database(&source)?;
        ensure!(
            epoch == marker.source_epoch && revision == marker.source_revision,
            "source database metadata changed after migration preparation"
        );
        ensure!(
            logical_database_digest(&source)? == marker.source_content_digest,
            "source database content changed after migration preparation"
        );
    }
    Ok(())
}

pub(crate) fn preflight_staging_files(home: &Path) -> Result<Vec<PathBuf>> {
    let Some(marker) = read_marker(home)? else {
        return Ok(Vec::new());
    };
    if marker.phase != Phase::Prepared {
        return Ok(Vec::new());
    }
    let stage = stage_path(home, &marker.stage_id);
    let mut files = vec![stage.clone(), owner_path(&stage)];
    files.extend([sibling(&stage, "-wal"), sibling(&stage, "-journal")]);
    files.retain(|path| path_exists(path));
    for path in &files {
        validate_regular(path).context("unsafe staging file in preflight HOME")?;
    }
    Ok(files)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Prepared,
    Committed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    format: u32,
    phase: Phase,
    stage_id: String,
    source_epoch: String,
    source_revision: i64,
    source_digest: String,
    source_content_digest: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StageOwner {
    stage_id: String,
    source_digest: String,
    source_content_digest: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Checkpoint {
    Prepared,
    BackupPage,
    BackupComplete,
    Published,
    Committed,
    Cleaned,
}

/// Migrate only a HOME that has already completed the domain cutover. A
/// markerless legacy HOME is left for the legacy protocol selector to handle.
pub(crate) fn migrate(home: &Path) -> Result<bool> {
    let (migrated, _fence) = migrate_and_fence(home)?;
    Ok(migrated)
}

pub(crate) fn migrate_and_fence(home: &Path) -> Result<(bool, Option<SourceFence>)> {
    let source = home.join(SOURCE);
    let migration_marker_exists = home.join(MARKER).exists();
    if !migration_marker_exists {
        let cutover = crate::domain_cutover::read_marker(home)?;
        if !cutover.is_some_and(|marker| marker.phase == crate::domain_cutover::Phase::Committed) {
            return Ok((false, None));
        }
        ensure!(
            path_exists(&source),
            "committed domain source database is missing before filename migration"
        );
    }
    let _lock = alondra_core::home_lock::HomeLock::recovery(home)
        .context("database migration needs exclusive access to HOME")?;
    alondra_core::home_lock::ensure_ready(home)?;
    let fence = if path_exists(&source) {
        Some(SourceFence::acquire_with_timeout(
            &source,
            Duration::from_secs(5),
        )?)
    } else {
        None
    };
    let migrated = migrate_locked(home, |_| Ok(()))?;
    Ok((migrated, fence))
}

/// A SQLite RESERVED write lock on the retained source prevents binaries that
/// still use the old filename from changing it after the canonical copy exists.
/// The serving daemon holds this fence for its lifetime.
pub(crate) struct SourceFence {
    _connection: Connection,
}

impl SourceFence {
    fn acquire_with_timeout(path: &Path, timeout: Duration) -> Result<Self> {
        let path = canonical_sqlite_path(path)?;
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(timeout)?;
        connection
            .execute_batch("BEGIN IMMEDIATE")
            .context("cannot acquire source database write fence")?;
        Ok(Self {
            _connection: connection,
        })
    }
}

#[cfg(test)]
pub(crate) fn prepare_test_migration(home: &Path) {
    let result = migrate_locked(home, |phase| {
        if phase == Checkpoint::Prepared {
            anyhow::bail!("test stop after prepared marker");
        }
        Ok(())
    });
    assert!(result.is_err());
}

fn migrate_locked(
    home: &Path,
    mut checkpoint: impl FnMut(Checkpoint) -> Result<()>,
) -> Result<bool> {
    let source = home.join(SOURCE);
    let target = home.join(DATABASE);
    let mut marker = match read_marker(home)? {
        Some(marker) => marker,
        None => {
            if !path_exists(&source) {
                return Ok(false);
            }
            let cutover = crate::domain_cutover::read_marker(home)?;
            ensure!(
                cutover.is_some_and(|m| m.phase == crate::domain_cutover::Phase::Committed),
                "refusing database rename before the domain cutover is committed"
            );
            ensure!(
                !path_exists(&target),
                "both database names exist without a migration marker; refusing to choose one"
            );
            let (epoch, revision) = inspect_database(&source)?;
            let source_digest = database_digest(&source)?;
            let source_content_digest = logical_database_digest(&source)?;
            let marker = Marker {
                format: 2,
                phase: Phase::Prepared,
                stage_id: crate::auth::random_hex(12),
                source_epoch: epoch,
                source_revision: revision,
                source_digest,
                source_content_digest,
            };
            write_marker(home, &marker)?;
            checkpoint(Checkpoint::Prepared)?;
            marker
        }
    };
    ensure!(marker.format == 2, "unsupported database migration marker");

    match marker.phase {
        Phase::Prepared => {
            if path_exists(&target) {
                // The process may have stopped after publishing the verified
                // copy but before committing its marker. Never overwrite it.
                let (target_epoch, target_revision) = inspect_database(&target)
                    .context("published database does not verify; preserving both copies")?;
                ensure!(
                    target_epoch == marker.source_epoch
                        && target_revision == marker.source_revision,
                    "published database metadata does not match the prepared source; preserving both copies"
                );
                ensure!(
                    path_exists(&source),
                    "prepared source is missing; preserving published database"
                );
                let (source_epoch, source_revision) = inspect_database(&source)
                    .context("prepared database source is invalid; preserving both copies")?;
                ensure!(
                    source_epoch == marker.source_epoch
                        && source_revision == marker.source_revision,
                    "database source changed after preparation; preserving both copies"
                );
                ensure!(
                    logical_database_digest(&source)? == marker.source_content_digest,
                    "database source content changed after preparation; preserving both copies"
                );
                ensure!(
                    logical_database_digest(&target)? == marker.source_content_digest,
                    "published database content differs from prepared source; preserving both copies"
                );
                checkpoint(Checkpoint::Published)?;
            } else {
                ensure_stage_id(&marker.stage_id)?;
                ensure!(
                    !path_exists(&home.join(NEXT)),
                    "unrecognized database staging file exists; preserving it"
                );
                let (source_epoch, source_revision) = inspect_database(&source)
                    .context("prepared database migration source is unavailable or invalid")?;
                ensure!(
                    source_epoch == marker.source_epoch
                        && source_revision == marker.source_revision,
                    "database migration source changed after preparation; preserving both files"
                );
                ensure!(
                    logical_database_digest(&source)? == marker.source_content_digest,
                    "database source content changed after preparation; preserving both files"
                );
                let mut stage = stage_path(home, &marker.stage_id);
                ensure_stage_owner(home, &marker)?;
                if path_exists(&stage) {
                    let valid = validate_stage(&stage, &marker).is_ok();
                    if !valid {
                        quarantine_stage(home, &marker)?;
                        marker.stage_id = crate::auth::random_hex(12);
                        write_marker(home, &marker)?;
                        stage = stage_path(home, &marker.stage_id);
                        ensure_stage_owner(home, &marker)?;
                    }
                }
                if !path_exists(&stage) {
                    backup_database_with_progress(&source, &stage, || {
                        checkpoint(Checkpoint::BackupPage)
                    })?;
                }
                validate_stage(&stage, &marker)
                    .context("staged database is invalid; preserving it and the source")?;
                let (source_epoch, source_revision) = inspect_database(&source)
                    .context("source database changed while copying; preserving both files")?;
                ensure!(
                    source_epoch == marker.source_epoch
                        && source_revision == marker.source_revision,
                    "source database changed while copying; preserving both files"
                );
                ensure!(
                    logical_database_digest(&source)? == marker.source_content_digest,
                    "source database content changed while copying; preserving both files"
                );
                checkpoint(Checkpoint::BackupComplete)?;
                rename_exclusive(&stage, &target)?;
                sync_dir(home)?;
                checkpoint(Checkpoint::Published)?;
            }
            marker.phase = Phase::Committed;
            write_marker(home, &marker)?;
            checkpoint(Checkpoint::Committed)?;
        }
        Phase::Committed => {
            let (target_epoch, target_revision) = inspect_database(&target)
                .context("committed database migration target is unavailable or invalid")?;
            ensure!(
                target_epoch == marker.source_epoch && target_revision >= marker.source_revision,
                "committed database does not match its migration marker; preserving source"
            );
            if path_exists(&source) {
                ensure!(
                    logical_database_digest(&source)? == marker.source_content_digest,
                    "retained source database content changed after migration; preserving both files"
                );
            }
        }
    }

    // Keep the original database as a private recovery copy. The staging name
    // disappears only when atomically published; unknown files are preserved.
    sync_dir(home)?;
    checkpoint(Checkpoint::Cleaned)?;
    Ok(true)
}

fn inspect_database(path: &Path) -> Result<(String, i64)> {
    validate_regular(path)?;
    let path = canonical_sqlite_path(path)?;
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .with_context(|| {
        format!(
            "opening {} read-only",
            path.file_name().unwrap_or_default().to_string_lossy()
        )
    })?;
    let app_id: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let schema: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    ensure!(
        app_id == APPLICATION_ID && schema == crate::domain_store::SCHEMA_VERSION,
        "database has an unexpected application or schema version"
    );
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    ensure!(integrity == "ok", "database integrity check failed");
    let foreign_key_violation: bool = conn
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .next()?
        .is_some();
    ensure!(
        !foreign_key_violation,
        "database has a foreign-key violation"
    );
    let (epoch, revision): (String, i64) = conn.query_row(
        "SELECT data_epoch,revision FROM domain_meta WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        epoch.len() == 64 && epoch.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid database epoch"
    );
    ensure!(revision >= 0, "invalid database revision");
    Ok((epoch, revision))
}

fn database_digest(path: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    for suffix in ["", "-wal", "-journal"] {
        let candidate = sibling(path, suffix);
        digest.update(suffix.as_bytes());
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) => {
                validate_regular(&candidate)?;
                if metadata.len() == 0 {
                    // SQLite may create/remove an empty WAL or rollback journal
                    // while opening the retained source. It carries no state.
                    digest.update([0]);
                    continue;
                }
                digest.update([1]);
                let mut file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(candidate)?;
                let mut buffer = [0_u8; 64 * 1024];
                loop {
                    let count = file.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    digest.update(&buffer[..count]);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => digest.update([0]),
            Err(error) => return Err(error.into()),
        }
    }
    let mut hex = String::with_capacity(64);
    for byte in digest.finalize() {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(hex)
}

fn logical_database_digest(path: &Path) -> Result<String> {
    use rusqlite::types::ValueRef;

    validate_regular(path)?;
    let path = canonical_sqlite_path(path)?;
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let mut digest = Sha256::new();
    let schema = {
        let mut statement =
            conn.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name")?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (kind, name, table, sql) in &schema {
        digest_field(&mut digest, 1, kind.as_bytes());
        digest_field(&mut digest, 2, name.as_bytes());
        digest_field(&mut digest, 3, table.as_bytes());
        digest_field(&mut digest, 4, sql.as_deref().unwrap_or("").as_bytes());
    }

    for (kind, table, _, _) in schema.iter().filter(|(kind, _, _, _)| kind == "table") {
        let identifier = format!("\"{}\"", table.replace('"', "\"\""));
        let mut statement = conn.prepare(&format!("SELECT * FROM {identifier}"))?;
        let columns = statement.column_count();
        let mut rows = statement.query([])?;
        let mut row_digests = Vec::new();
        while let Some(row) = rows.next()? {
            let mut row_digest = Sha256::new();
            for index in 0..columns {
                match row.get_ref(index)? {
                    ValueRef::Null => digest_field(&mut row_digest, 0, &[]),
                    ValueRef::Integer(value) => {
                        digest_field(&mut row_digest, 1, &value.to_be_bytes())
                    }
                    ValueRef::Real(value) => {
                        digest_field(&mut row_digest, 2, &value.to_bits().to_be_bytes())
                    }
                    ValueRef::Text(value) => digest_field(&mut row_digest, 3, value),
                    ValueRef::Blob(value) => digest_field(&mut row_digest, 4, value),
                }
            }
            row_digests.push(row_digest.finalize().to_vec());
        }
        row_digests.sort();
        digest_field(&mut digest, 5, kind.as_bytes());
        digest_field(&mut digest, 6, table.as_bytes());
        digest_field(&mut digest, 7, &(row_digests.len() as u64).to_be_bytes());
        for row_digest in row_digests {
            digest_field(&mut digest, 8, &row_digest);
        }
    }

    let mut hex = String::with_capacity(64);
    for byte in digest.finalize() {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(hex)
}

fn digest_field(digest: &mut Sha256, tag: u8, bytes: &[u8]) {
    digest.update([tag]);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn stage_path(home: &Path, stage_id: &str) -> PathBuf {
    home.join(format!("{NEXT}.{stage_id}"))
}

fn owner_path(stage: &Path) -> PathBuf {
    let mut name = stage.file_name().unwrap_or_default().to_os_string();
    name.push(".owner");
    stage.with_file_name(name)
}

fn read_stage_owner(path: &Path) -> Result<StageOwner> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.nlink() == 1
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o077 == 0,
        "unsafe staging ownership record; preserving it"
    );
    let mut bytes = Vec::new();
    file.take(2049).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 2048, "staging ownership record is too large");
    Ok(
        serde_json::from_slice(&bytes)
            .context("invalid staging ownership record; preserving it")?,
    )
}

fn interrupted_path(home: &Path, stage_id: &str) -> PathBuf {
    home.join(format!("domain.db.interrupted.{stage_id}"))
}

fn ensure_stage_id(stage_id: &str) -> Result<()> {
    ensure!(
        stage_id.len() == 24
            && stage_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "invalid stage ID in database migration marker"
    );
    Ok(())
}

fn ensure_stage_owner(home: &Path, marker: &Marker) -> Result<()> {
    let stage = stage_path(home, &marker.stage_id);
    let owner = owner_path(&stage);
    match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&owner)
    {
        Ok(file) => {
            let metadata = file.metadata()?;
            ensure!(
                metadata.is_file()
                    && metadata.nlink() == 1
                    && metadata.uid() == unsafe { libc::geteuid() }
                    && metadata.permissions().mode() & 0o077 == 0,
                "unsafe staging ownership record; preserving it"
            );
            drop(file);
            let existing = read_stage_owner(&owner)?;
            ensure!(
                existing.stage_id == marker.stage_id
                    && existing.source_digest == marker.source_digest
                    && existing.source_content_digest == marker.source_content_digest,
                "staging ownership record does not match marker; preserving it"
            );
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ensure!(
                !stage.exists(),
                "staging database has no ownership record; preserving it"
            );
            let record = StageOwner {
                stage_id: marker.stage_id.clone(),
                source_digest: marker.source_digest.clone(),
                source_content_digest: marker.source_content_digest.clone(),
            };
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&owner)?;
            use std::io::Write;
            file.write_all(&serde_json::to_vec(&record)?)?;
            file.sync_all()?;
            sync_dir(home)
        }
        Err(error) => Err(error.into()),
    }
}

fn validate_stage(stage: &Path, marker: &Marker) -> Result<()> {
    let (epoch, revision) = inspect_database(stage)?;
    ensure!(
        epoch == marker.source_epoch && revision == marker.source_revision,
        "staged database metadata differs from prepared source"
    );
    ensure!(
        logical_database_digest(stage)? == marker.source_content_digest,
        "staged database content differs from prepared source"
    );
    Ok(())
}

fn quarantine_stage(home: &Path, marker: &Marker) -> Result<()> {
    let stage = stage_path(home, &marker.stage_id);
    validate_regular(&stage).context("cannot prove ownership of invalid staging database")?;
    let owner = owner_path(&stage);
    let quarantine = interrupted_path(home, &marker.stage_id);
    let quarantine_owner = owner_path(&quarantine);
    ensure!(
        !path_exists(&quarantine) && !path_exists(&quarantine_owner),
        "interrupted staging destination already exists; preserving all files"
    );
    rename_exclusive(&stage, &quarantine)?;
    if path_exists(&owner) {
        rename_exclusive(&owner, &quarantine_owner)?;
    }
    sync_dir(home)
}

fn canonical_sqlite_path(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().unwrap_or(Path::new(".")).canonicalize()?;
    Ok(parent.join(path.file_name().context("SQLite filename is missing")?))
}

fn backup_database(source: &Path, target: &Path) -> Result<()> {
    backup_database_with_progress(source, target, || Ok(()))
}

fn backup_database_with_progress(
    source: &Path,
    target: &Path,
    mut progress: impl FnMut() -> Result<()>,
) -> Result<()> {
    ensure!(
        !path_exists(target),
        "database migration staging file already exists"
    );
    let source = canonical_sqlite_path(source)?;
    let target = canonical_sqlite_path(target)?;
    let from = Connection::open_with_flags(
        &source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&target)?
        .sync_all()?;
    let mut to = Connection::open_with_flags(
        &target,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let backup = Backup::new(&from, &mut to)?;
    loop {
        match backup.step(1)? {
            StepResult::Done => break,
            StepResult::More => progress()?,
            StepResult::Busy | StepResult::Locked => {
                std::thread::sleep(Duration::from_millis(10));
            }
            other => anyhow::bail!("unexpected SQLite backup result: {other:?}"),
        }
    }
    drop(backup);
    to.pragma_update(None, "journal_mode", "DELETE")?;
    drop(to);
    drop(from);
    File::open(target)?.sync_all()?;
    Ok(())
}

fn validate_regular(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file()
            && metadata.nlink() == 1
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o077 == 0,
        "unsafe database migration file: {}",
        path.display()
    );
    Ok(())
}

fn read_marker(home: &Path) -> Result<Option<Marker>> {
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
        "unsafe database migration marker"
    );
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 4096, "database migration marker too large");
    let marker: Marker =
        serde_json::from_slice(&bytes).context("invalid database migration marker")?;
    ensure!(marker.format == 2, "unsupported database migration marker");
    ensure_stage_id(&marker.stage_id)?;
    ensure!(
        marker.source_digest.len() == 64
            && marker
                .source_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid source database digest in migration marker"
    );
    ensure!(
        marker.source_content_digest.len() == 64
            && marker
                .source_content_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid source content digest in migration marker"
    );
    Ok(Some(marker))
}

fn write_marker(home: &Path, marker: &Marker) -> Result<()> {
    let next = home.join(format!("{MARKER}.{}.tmp", crate::auth::random_hex(12)));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&next)?;
    use std::io::Write;
    file.write_all(&serde_json::to_vec(marker)?)?;
    file.sync_all()?;
    fs::rename(next, home.join(MARKER))?;
    sync_dir(home)
}

fn rename_exclusive(from: &Path, to: &Path) -> Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
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
    compile_error!("database migration requires exclusive rename support");
    if status != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn sync_dir(home: &Path) -> Result<()> {
    File::open(home)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
#[path = "domain_database_migration_tests.rs"]
mod tests;
