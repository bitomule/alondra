//! Offline domain cutover preparation. Nothing here executes a legacy event or
//! a queued effect against a real producer. The original event database is an
//! audit source and rollback artifact, never the authority of domain v2.
use alondra_core::{Event, View, domain_v2 as d};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn canonical_leaf(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .context("database parent missing")?
        .canonicalize()?;
    Ok(parent.join(path.file_name().context("database filename missing")?))
}
fn destination_outside(home: &Path, destination: &Path) -> Result<()> {
    let home = home.canonicalize()?;
    let parent = destination
        .parent()
        .context("destination parent missing")?
        .canonicalize()?;
    ensure!(
        !parent.starts_with(home),
        "migration destination must be outside source HOME"
    );
    Ok(())
}

mod import;
mod schedules;
#[cfg(test)]
mod synthetic;
pub(crate) use import::{ImportLink, ImportReport, materialize_legacy};
pub(crate) use schedules::{LegacyScheduleInventory, LegacyScheduleReceiptRow, read_legacy_schedules_readonly};

#[derive(Debug, Serialize)]
pub(crate) struct LegacyInventory {
    pub source_seq: u64,
    pub requests: usize,
    pub tasks: usize,
    pub questions: usize,
    pub reports: usize,
    pub prs: usize,
    pub messages: usize,
    pub attachments: usize,
    pub open_questions: usize,
    pub open_prs: usize,
    pub active_tasks: usize,
    pub multiple_pr_tasks: Vec<String>,
}

pub(crate) fn inventory(view: &View) -> LegacyInventory {
    use alondra_core::{model::QuestionStatus, pr::PrState, tasks::TaskState};
    LegacyInventory {
        source_seq: view.seq,
        requests: view.board.requests.len(),
        tasks: view.board.tasks.len(),
        questions: view.questions.len(),
        reports: view.reports.len(),
        prs: view.prs.len(),
        messages: view.messages.len(),
        attachments: view
            .board
            .requests
            .values()
            .map(|v| v.attachments.len())
            .sum::<usize>()
            + view
                .questions
                .values()
                .map(|v| v.attachments.len())
                .sum::<usize>()
            + view
                .reports
                .values()
                .map(|v| v.attachments.len())
                .sum::<usize>()
            + view
                .messages
                .values()
                .map(|v| v.attachments.len())
                .sum::<usize>(),
        open_questions: view
            .questions
            .values()
            .filter(|v| v.status == QuestionStatus::Open)
            .count(),
        open_prs: view
            .prs
            .values()
            .filter(|v| v.state == PrState::Open)
            .count(),
        active_tasks: view
            .board
            .tasks
            .values()
            .filter(|v| !matches!(v.state, TaskState::Done | TaskState::Cancelled))
            .count(),
        multiple_pr_tasks: view
            .board
            .tasks
            .values()
            .filter(|v| v.prs.len() > 1)
            .map(|v| v.id.clone())
            .collect(),
    }
}

/// Read snapshot and tail inside one SQLite read transaction. A live writer may
/// advance later, so cutover also fences writers with HomeLock before this read.
pub(crate) fn load_legacy_readonly(path: &Path) -> Result<View> {
    let path = canonical_leaf(path)?;
    let meta = fs::symlink_metadata(&path)?;
    ensure!(
        meta.is_file() && meta.nlink() == 1 && meta.uid() == unsafe { libc::geteuid() },
        "unsafe legacy database path"
    );
    let db = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.execute_batch("PRAGMA query_only=ON; BEGIN")?;
    let snapshot: Option<(i64, String)> = db
        .query_row(
            "SELECT seq,body FROM snapshots ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (after, mut view) = match snapshot {
        Some((seq, body)) => (
            seq,
            serde_json::from_str::<View>(&body).context("legacy snapshot unreadable")?,
        ),
        None => (0, View::default()),
    };
    ensure!(
        after >= 0 && view.seq == after as u64,
        "invalid legacy snapshot sequence"
    );
    view.backfill_created_at();
    view.backfill_delivery_ids();
    view.backfill_tasks();
    let mut q = db.prepare("SELECT seq,at,body FROM event_log WHERE seq>?1 ORDER BY seq")?;
    let mut rows = q.query([after])?;
    while let Some(row) = rows.next()? {
        let seq: i64 = row.get(0)?;
        let at: i64 = row.get(1)?;
        let body: String = row.get(2)?;
        ensure!(seq > 0, "invalid legacy event sequence");
        let event: Event = serde_json::from_str(&body)
            .with_context(|| format!("legacy event {seq} unreadable"))?;
        view.apply(seq as u64, at, &event);
    }
    drop(rows);
    drop(q);
    db.execute_batch("COMMIT")?;
    let issues = view.integrity_issues();
    ensure!(
        issues.is_empty(),
        "legacy state integrity failed: {}",
        issues.join("; ")
    );
    Ok(view)
}

fn ensure_legacy_delivery_reconciled(view: &View) -> Result<()> {
    use alondra_core::delivery::State;
    let unresolved: Vec<_> = view
        .delivery_attempts
        .values()
        .filter(|attempt| {
            matches!(
                attempt.state,
                State::Dispatching | State::Uncertain | State::Retryable
            )
        })
        .map(|attempt| attempt.id)
        .collect();
    ensure!(
        unresolved.is_empty(),
        "legacy deliveries require explicit reconciliation before migration: {unresolved:?}"
    );
    Ok(())
}

/// The legacy schedule store is imported at cutover. Before any backup is
/// taken, refuse a source whose schedules.db is unsafe, corrupt or holds
/// tables the importer does not know; rows themselves are welcome.
pub(crate) fn ensure_legacy_schedules_readable(home: &Path) -> Result<()> {
    let path = home.join("schedules.db");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            for sidecar in ["schedules.db-wal", "schedules.db-shm", "schedules.db-journal"] {
                match fs::symlink_metadata(home.join(sidecar)) {
                    Ok(_) => anyhow::bail!("schedules.db is missing but {sidecar} exists"),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.is_file() && metadata.nlink() == 1 && metadata.uid() == unsafe { libc::geteuid() },
        "unsafe schedules.db path"
    );
    let path = canonical_leaf(&path)?;
    let database = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    database.execute_batch("PRAGMA query_only=ON; BEGIN")?;
    let integrity: String = database.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    ensure!(integrity == "ok", "schedules.db integrity check failed");
    let mut tables = database.prepare(
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
    )?;
    let names = tables.query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(tables);
    for name in names {
        ensure!(
            matches!(
                name.as_str(),
                "schedules" | "local_command_results" | "schedule_failed_launches" | "schedule_launch_tools"
            ),
            "schedules.db contains an unknown table: {name}"
        );
    }
    database.execute_batch("COMMIT")?;
    Ok(())
}

/// The promoted store must carry exactly the schedule rows, run links, tool
/// policies, failed launches and receipts that the verified backup holds.
pub(crate) fn verify_staged_schedules(backup: &Path, stage: &Path) -> Result<()> {
    let view = load_legacy_readonly(&backup.join("alondra.db"))?;
    let mut store = crate::domain_store::DomainStore::open(&stage.join("domain-v2.db"))?;
    let staged = store.load()?;
    let expected = read_legacy_schedules_readonly(backup, &view, &staged)?;
    let imported = store.legacy_schedule_inventory()?;
    ensure!(
        imported == expected,
        "staged schedule import differs from the verified backup: staged {:?}, backup {:?}",
        imported.counts(),
        expected.counts()
    );
    Ok(())
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BackupFile {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BackupManifest {
    pub format: u32,
    pub source_home: String,
    pub canonical_source_home: String,
    pub source_seq: u64,
    pub files: Vec<BackupFile>,
}

/// Caller must have stopped the daemon, jobs and legacy producers. The
/// exclusive HOME lock makes daemon startup impossible during the backup.
/// The destination must be new and private; no secret enters public output.
pub(crate) fn backup_offline(
    home: &Path,
    destination: &Path,
    legacy_daemon: &Path,
) -> Result<BackupManifest> {
    let lock = alondra_core::home_lock::HomeLock::recovery(home)?;
    backup_locked(&lock, home, destination, legacy_daemon)
}

fn backup_locked(
    _lock: &alondra_core::home_lock::HomeLock,
    home: &Path,
    destination: &Path,
    legacy_daemon: &Path,
) -> Result<BackupManifest> {
    ensure!(home.is_absolute(), "source HOME must be absolute");
    destination_outside(home, destination)?;
    fs::DirBuilder::new().mode(0o700).create(destination)?;
    let meta = fs::symlink_metadata(destination)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "backup destination must be private and owner-only"
    );
    let view = load_legacy_readonly(&home.join("alondra.db"))?;
    ensure_legacy_delivery_reconciled(&view)?;
    let mut files = Vec::new();
    for name in [
        "alondra.db",
        "schedules.db",
        "service-receipts.sqlite",
        "service-upload-attempts.sqlite",
    ] {
        let source = home.join(name);
        if source.exists() {
            let target = destination.join(name);
            sqlite_backup(&source, &target)?;
            files.push(describe_file(destination, &target)?);
        }
    }
    for name in [
        "config.toml",
        "service.toml",
        "service.toml.staged",
        "options.toml",
        "config.toml.pre-loopback-service-20260926",
        "devices.json",
        "push.json",
        "tui-key.pem",
        "tui-device",
        "terminal-service.json",
        "legacy-questions.json",
        "flagged-reports.jsonl",
        "rejected-reports.jsonl",
        "brew-keg-path",
    ] {
        let source = home.join(name);
        if source.exists() {
            let target = destination.join(name);
            copy_regular(&source, &target)?;
            files.push(describe_file(destination, &target)?);
        }
    }
    let legacy_binary = destination.join("legacy-alondrad");
    copy_regular(legacy_daemon, &legacy_binary)?;
    files.push(describe_file(destination, &legacy_binary)?);
    for directory in ["service-keys", "files", "candidates", "soak"] {
        let source = home.join(directory);
        if source.exists() {
            copy_tree(
                &source,
                &destination.join(directory),
                destination,
                &mut files,
            )?;
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = BackupManifest {
        format: 1,
        source_home: home.to_string_lossy().into_owned(),
        canonical_source_home: home.canonicalize()?.to_string_lossy().into_owned(),
        source_seq: view.seq,
        files,
    };
    let content = serde_json::to_vec_pretty(&manifest)?;
    let marker = destination.join("manifest.json");
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&marker)?;
    output.write_all(&content)?;
    output.sync_all()?;
    File::open(destination)?.sync_all()?;
    verify_backup(destination)?;
    Ok(manifest)
}

pub(crate) fn sqlite_backup(source: &Path, target: &Path) -> Result<()> {
    let source = canonical_leaf(source)?;
    let target = canonical_leaf(target)?;
    let meta = fs::symlink_metadata(&source)?;
    ensure!(
        meta.is_file() && meta.nlink() == 1 && meta.uid() == unsafe { libc::geteuid() },
        "unsafe SQLite source"
    );
    OpenOptions::new()
        .write(true)
        .read(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&target)?;
    let from = Connection::open_with_flags(
        &source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let mut to = Connection::open_with_flags(
        &target,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    to.pragma_update(None, "synchronous", "FULL")?;
    let deadline = Instant::now() + Duration::from_secs(120);
    {
        let copy = rusqlite::backup::Backup::new(&from, &mut to)?;
        loop {
            match copy.step(512)? {
                rusqlite::backup::StepResult::Done => break,
                rusqlite::backup::StepResult::More if Instant::now() < deadline => (),
                _ => anyhow::bail!("SQLite backup did not complete"),
            }
        }
    }
    to.pragma_update(None, "journal_mode", "DELETE")?;
    let integrity: String = to.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    ensure!(integrity == "ok", "SQLite backup integrity failure");
    drop(to);
    File::open(&target)?.sync_all()?;
    Ok(())
}

pub(crate) fn copy_regular(source: &Path, target: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(source)?;
    ensure!(
        meta.is_file() && meta.nlink() == 1 && meta.uid() == unsafe { libc::geteuid() },
        "unsafe backup source"
    );
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(if meta.mode() & 0o111 == 0 {
            0o600
        } else {
            0o700
        })
        .custom_flags(libc::O_NOFOLLOW)
        .open(target)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    Ok(())
}

fn copy_tree(source: &Path, target: &Path, root: &Path, files: &mut Vec<BackupFile>) -> Result<()> {
    let meta = fs::symlink_metadata(source)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() },
        "unsafe backup directory"
    );
    fs::DirBuilder::new().mode(0o700).create(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&from, &to, root, files)?;
        } else {
            copy_regular(&from, &to)?;
            files.push(describe_file(root, &to)?);
        }
    }
    File::open(target)?.sync_all()?;
    Ok(())
}

fn describe_file(root: &Path, path: &Path) -> Result<BackupFile> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = 0;
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        bytes += count as u64;
    }
    Ok(BackupFile {
        path: path.strip_prefix(root)?.to_string_lossy().into_owned(),
        bytes,
        sha256: hex(&hash.finalize()),
    })
}

pub(crate) fn verify_backup(root: &Path) -> Result<BackupManifest> {
    let data = fs::read(root.join("manifest.json"))?;
    let manifest: BackupManifest = serde_json::from_slice(&data)?;
    ensure!(
        manifest.format == 1,
        "unsupported migration backup manifest"
    );
    let mut expected = BTreeSet::from(["manifest.json".to_owned()]);
    for item in &manifest.files {
        let path = Path::new(&item.path);
        ensure!(
            path.components()
                .all(|component| matches!(component, std::path::Component::Normal(_))),
            "unsafe backup manifest path"
        );
        ensure!(
            expected.insert(item.path.clone()),
            "duplicate backup manifest path"
        );
        let observed = describe_file(root, &root.join(path))?;
        ensure!(
            observed.bytes == item.bytes && observed.sha256 == item.sha256,
            "backup file changed: {}",
            item.path
        );
    }
    let mut actual = BTreeSet::new();
    collect_backup_files(root, root, &mut actual)?;
    ensure!(
        actual == expected,
        "backup contains unlisted or missing files"
    );
    Ok(manifest)
}

fn collect_backup_files(root: &Path, directory: &Path, files: &mut BTreeSet<String>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        ensure!(
            meta.uid() == unsafe { libc::geteuid() },
            "unsafe backup owner"
        );
        if meta.is_dir() {
            collect_backup_files(root, &path, files)?;
        } else {
            ensure!(
                meta.is_file() && meta.nlink() == 1,
                "unsafe backup file type"
            );
            files.insert(path.strip_prefix(root)?.to_string_lossy().into_owned());
        }
    }
    Ok(())
}

pub(crate) fn backup_manifest_digest(root: &Path) -> Result<String> {
    let data = fs::read(root.join("manifest.json"))?;
    Ok(hex(&Sha256::digest(data)))
}

pub(crate) fn verify_backup_expected(root: &Path, expected_digest: &str) -> Result<BackupManifest> {
    ensure!(
        expected_digest.len() == 64
            && expected_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "invalid expected backup digest"
    );
    ensure!(
        backup_manifest_digest(root)? == expected_digest,
        "backup manifest digest mismatch"
    );
    verify_backup(root)
}

/// Recopy every preserved source under the same exclusive HOME lease used by
/// promotion. The legacy sequence alone cannot detect changed snapshots,
/// technical journals, configuration or stored content.
pub(crate) fn verify_current_source_matches_backup(
    lock: &alondra_core::home_lock::HomeLock,
    home: &Path,
    backup: &Path,
    expected: &BackupManifest,
    stage: Option<&Path>,
    allow_unlisted_files: bool,
) -> Result<()> {
    let check = std::env::temp_dir().join(format!(
        "alondra-domain-current-check-{}",
        crate::auth::random_hex(8)
    ));
    let result = (|| {
        let current = backup_locked(lock, home, &check, &backup.join("legacy-alondrad"))?;
        let expected_files: std::collections::BTreeMap<_, _> = expected
            .files
            .iter()
            .map(|item| (item.path.as_str(), item))
            .collect();
        let current_files: std::collections::BTreeMap<_, _> = current
            .files
            .iter()
            .map(|item| (item.path.as_str(), item))
            .collect();
        let originals_match = expected_files.iter().all(|(path, item)| {
            current_files
                .get(path)
                .is_some_and(|observed| *observed == *item)
        });
        let allowed_extra = if let Some(stage) = stage {
            let mut staged = Vec::new();
            if stage.join("files").exists() {
                collect_stage_files(stage, &stage.join("files"), &mut staged)?;
            }
            staged
                .into_iter()
                .map(|item| (item.path, (item.bytes, item.sha256)))
                .collect::<std::collections::BTreeMap<_, _>>()
        } else {
            Default::default()
        };
        let extras_match = current_files
            .iter()
            .filter(|(path, _)| !expected_files.contains_key(*path))
            .all(|(path, item)| {
                allow_unlisted_files
                    || allowed_extra
                        .get(*path)
                        .is_some_and(|(bytes, sha)| *bytes == item.bytes && *sha == item.sha256)
            });
        ensure!(
            current.canonical_source_home == expected.canonical_source_home
                && current.source_seq == expected.source_seq
                && originals_match
                && extras_match,
            "current HOME differs from verified cutover backup; prepare a fresh backup and stage"
        );
        Ok(())
    })();
    if check.exists() {
        fs::remove_dir_all(&check)?;
    }
    result
}

fn collect_stage_files(stage: &Path, directory: &Path, files: &mut Vec<BackupFile>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_stage_files(stage, &path, files)?;
        } else {
            files.push(describe_file(stage, &path)?);
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub(crate) struct Rehearsal {
    pub source: LegacyInventory,
    pub imported: ImportReport,
    pub projection_bytes: usize,
    pub new_data_epoch: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct CutoverPreparation {
    pub backup_manifest_sha256: String,
    pub backup_files: usize,
    pub rehearsal: Rehearsal,
}

/// Binds a staged image to the exact verified backup from which it was made.
/// This file stays outside the live HOME and contains no source data or secrets.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StageProvenance {
    pub format: u32,
    pub backup_manifest_sha256: String,
    pub source_seq: u64,
    pub data_epoch: String,
    pub domain_state_sha256: String,
    pub import_map_sha256: String,
}

pub(crate) fn domain_state_digest(state: &alondra_core::domain_v2::DomainState) -> Result<String> {
    Ok(hex(&Sha256::digest(serde_json::to_vec(state)?)))
}

pub(crate) fn import_map_digest(database: &Path) -> Result<String> {
    let database = canonical_leaf(database)?;
    let connection = Connection::open_with_flags(
        &database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let mut statement = connection.prepare("SELECT source_kind,source_id,role,target_kind,target_id,source_digest,source_seq FROM legacy_import_map ORDER BY source_kind,source_id,role")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(hex(&Sha256::digest(serde_json::to_vec(&rows)?)))
}

/// Prepare the exact cutover image under one exclusive HOME lease. This does
/// not swap the business database, start v2, or dispatch any external effect.
/// The returned manifest digest must be recorded outside the private backup.
pub(crate) fn prepare_cutover_offline(
    home: &Path,
    backup: &Path,
    stage: &Path,
    legacy_daemon: &Path,
) -> Result<CutoverPreparation> {
    destination_outside(home, backup)?;
    destination_outside(home, stage)?;
    let lock = alondra_core::home_lock::HomeLock::recovery(home)?;
    ensure_legacy_schedules_readable(home)?;
    let manifest = backup_locked(&lock, home, backup, legacy_daemon)?;
    let digest = backup_manifest_digest(backup)?;
    let rehearsal = rehearse(backup, stage)?;
    verify_backup_expected(backup, &digest)?;
    ensure!(
        rehearsal.source.source_seq == manifest.source_seq,
        "staged domain does not match backup source sequence"
    );
    ensure!(
        load_legacy_readonly(&home.join("alondra.db"))?.seq == manifest.source_seq,
        "legacy writer advanced during cutover preparation"
    );
    Ok(CutoverPreparation {
        backup_manifest_sha256: digest,
        backup_files: manifest.files.len(),
        rehearsal,
    })
}

fn materialize_with_schedules(
    view: &View, source_home: &Path, target_home: &Path, epoch: d::DataEpoch,
) -> Result<(d::DomainState, ImportReport, LegacyScheduleInventory)> {
    let (mut state, imported) = materialize_legacy(view, source_home, target_home, epoch)?;
    // A verified backup already holds a rollback-journal copy. A rehearsal
    // straight from a HOME snapshots the live WAL database the same way first.
    let schedules = if source_home.join("manifest.json").exists()
        || !source_home.join("schedules.db").exists()
    {
        read_legacy_schedules_readonly(source_home, view, &state)?
    } else {
        let snapshot = std::env::temp_dir().join(format!(
            "alondra-domain-schedule-snapshot-{}",
            crate::auth::random_hex(8)
        ));
        fs::DirBuilder::new().mode(0o700).create(&snapshot)?;
        let result = sqlite_backup(&source_home.join("schedules.db"), &snapshot.join("schedules.db"))
            .and_then(|()| read_legacy_schedules_readonly(&snapshot, view, &state));
        fs::remove_dir_all(&snapshot)?;
        result?
    };
    state.schedule = schedules.state.clone();
    state.validate().map_err(anyhow::Error::new)?;
    Ok((state, imported, schedules))
}

/// Rehearse against a source copy and a brand-new target HOME. The function
/// never writes the legacy source, and leaves the target for human inspection.
pub(crate) fn rehearse(source_home: &Path, target_home: &Path) -> Result<Rehearsal> {
    destination_outside(source_home, target_home)?;
    ensure_legacy_schedules_readable(source_home)?;
    let backup_digest = if source_home.join("manifest.json").exists() {
        let digest = backup_manifest_digest(source_home)?;
        verify_backup_expected(source_home, &digest)?;
        Some(digest)
    } else {
        None
    };
    fs::DirBuilder::new().mode(0o700).create(target_home)?;
    let view = load_legacy_readonly(&source_home.join("alondra.db"))?;
    ensure_legacy_delivery_reconciled(&view)?;
    let source = inventory(&view);
    let mut store = crate::domain_store::DomainStore::open(&target_home.join("domain-v2.db"))?;
    let epoch = store.load()?.data_epoch;
    let (state, imported, schedules) =
        materialize_with_schedules(&view, source_home, target_home, epoch.clone())?;
    store.import_initial(&state, &imported.links, view.seq, &schedules)?;
    store.integrity_check()?;
    let loaded = store.load()?;
    ensure!(
        loaded == state,
        "domain import SQL roundtrip changed business state"
    );
    let projection = store.project_bounded(crate::service_overview::MAX_BYTES)?;
    for revision in loaded.document_revisions.values() {
        verify_content(
            target_home,
            &revision.content_reference,
            &revision.content_digest,
        )?;
    }
    for interaction in loaded.interactions.values() {
        for attachment in &interaction.attachments {
            verify_content(
                target_home,
                &attachment.content_reference,
                &attachment.content_digest,
            )?;
        }
    }
    if let Some(digest) = backup_digest {
        verify_backup_expected(source_home, &digest)?;
        let provenance = StageProvenance {
            format: 1,
            backup_manifest_sha256: digest,
            source_seq: view.seq,
            data_epoch: epoch.0.clone(),
            domain_state_sha256: domain_state_digest(&loaded)?,
            import_map_sha256: import_map_digest(&target_home.join("domain-v2.db"))?,
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(target_home.join("domain-v2-stage.json"))?;
        file.write_all(&serde_json::to_vec(&provenance)?)?;
        file.sync_all()?;
        File::open(target_home)?.sync_all()?;
    }
    Ok(Rehearsal {
        source,
        imported,
        projection_bytes: projection.len(),
        new_data_epoch: epoch.0,
    })
}

pub(crate) fn verify_content(home: &Path, reference: &str, digest: &str) -> Result<()> {
    let path = Path::new(reference);
    ensure!(
        path.components()
            .all(|component| matches!(component, std::path::Component::Normal(_))),
        "unsafe imported content reference"
    );
    let observed = describe_file(home, &home.join(path))?;
    ensure!(
        observed.sha256 == digest,
        "imported content digest mismatch"
    );
    Ok(())
}

/// Validate rollback by restoring into a separate empty HOME. The real HOME
/// remains untouched; operational rollback requires an older compatible binary
/// and explicit reconciliation of any external effects made after cutover.
pub(crate) fn restore_rehearsal(
    backup: &Path,
    expected_digest: &str,
    target_home: &Path,
) -> Result<LegacyInventory> {
    let manifest = verify_backup_expected(backup, expected_digest)?;
    fs::DirBuilder::new().mode(0o700).create(target_home)?;
    for file in &manifest.files {
        let relative = Path::new(&file.path);
        let target = target_home.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        copy_regular(&backup.join(relative), &target)?;
    }
    let inventory = inventory(&load_legacy_readonly(&target_home.join("alondra.db"))?);
    ensure!(
        inventory.source_seq == manifest.source_seq,
        "rollback rehearsal lost legacy tail"
    );
    Ok(inventory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::{domain_v2::PrId, pr};
    use serde_json::json;

    #[test]
    fn populated_schedule_store_is_readable_and_unsafe_or_unknown_stores_are_refused() {
        let root = std::env::temp_dir().join(format!(
            "alondra-schedule-cutover-{}",
            crate::auth::random_hex(8)
        ));
        let source = root.join("source");
        let stage = root.join("stage");
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::DirBuilder::new().mode(0o700).create(&source).unwrap();
        ensure_legacy_schedules_readable(&source).unwrap();

        let database = Connection::open(source.join("schedules.db")).unwrap();
        database.execute_batch("CREATE TABLE schedules(id TEXT PRIMARY KEY,body TEXT NOT NULL);
            CREATE TABLE local_command_results(sender TEXT,request_id TEXT,digest BLOB,result TEXT);
            CREATE TABLE schedule_failed_launches(request TEXT PRIMARY KEY);
            CREATE TABLE schedule_launch_tools(request TEXT PRIMARY KEY,patterns TEXT NOT NULL);
            INSERT INTO schedules(id,body) VALUES('disabled','{\"enabled\":false}');
            INSERT INTO local_command_results(sender,request_id,digest,result) VALUES('phone','old',X'00','{}');
            INSERT INTO schedule_failed_launches(request) VALUES('request-1');
            INSERT INTO schedule_launch_tools(request,patterns) VALUES('request-1','[]');").unwrap();
        ensure_legacy_schedules_readable(&source).unwrap();
        database.execute("CREATE TABLE unexpected_schedule_data(value TEXT)", []).unwrap();
        assert!(ensure_legacy_schedules_readable(&source).unwrap_err().to_string().contains("unknown table"));
        let error = rehearse(&source, &stage).unwrap_err().to_string();
        assert!(error.contains("unknown table"), "{error}");
        assert!(!stage.exists(), "failed rehearsal must leave no stage");
        let backup = root.join("backup");
        let error = prepare_cutover_offline(&source, &backup, &stage, &source.join("legacy-daemon"))
            .unwrap_err().to_string();
        assert!(error.contains("unknown table"), "{error}");
        assert!(!backup.exists(), "failed preparation must leave no backup");
        drop(database);
        fs::remove_file(source.join("schedules.db")).unwrap();
        fs::write(source.join("schedules.db"), b"not a SQLite database").unwrap();
        assert!(ensure_legacy_schedules_readable(&source).is_err());
        fs::remove_file(source.join("schedules.db")).unwrap();
        for sidecar in ["schedules.db-wal", "schedules.db-shm", "schedules.db-journal"] {
            fs::write(source.join(sidecar), b"orphaned SQLite sidecar").unwrap();
            let error = ensure_legacy_schedules_readable(&source).unwrap_err().to_string();
            assert!(error.contains(sidecar), "{error}");
            let error = rehearse(&source, &stage).unwrap_err().to_string();
            assert!(error.contains(sidecar), "{error}");
            assert!(!stage.exists(), "failed rehearsal must leave no stage");
            fs::remove_file(source.join(sidecar)).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backup_rehearsal_and_rollback_preserve_tail_cards_and_content() {
        let mut unresolved = View::default();
        for state in [
            alondra_core::delivery::State::Dispatching,
            alondra_core::delivery::State::Uncertain,
            alondra_core::delivery::State::Retryable,
        ] {
            unresolved.delivery_attempts.insert(
                1,
                alondra_core::delivery::Attempt {
                    id: 1,
                    session_id: "old-session".into(),
                    channel: alondra_core::delivery::Channel::Terminal,
                    message_ids: vec![],
                    question_id: None,
                    state,
                    version: 1,
                    reason: None,
                    created_at: 1,
                    updated_at: 1,
                    resolved_from_version: None,
                },
            );
            assert!(ensure_legacy_delivery_reconciled(&unresolved).is_err());
        }
        unresolved.delivery_attempts.get_mut(&1).unwrap().state =
            alondra_core::delivery::State::Discarded;
        ensure_legacy_delivery_reconciled(&unresolved).unwrap();
        let root = std::env::temp_dir().join(format!(
            "alondra-domain-migration-{}",
            crate::auth::random_hex(8)
        ));
        let source = root.join("source");
        let backup = root.join("backup");
        let target = root.join("target");
        let restored = root.join("restored");
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::DirBuilder::new().mode(0o700).create(&source).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(source.join("files"))
            .unwrap();
        fs::write(source.join("files/question.txt"), b"legacy attachment").unwrap();
        fs::write(source.join("tui-device"), b"private device identity").unwrap();
        fs::write(
            source.join("service.toml.staged"),
            b"staged service configuration",
        )
        .unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(source.join("candidates"))
            .unwrap();
        fs::write(source.join("candidates/candidate"), b"candidate daemon").unwrap();
        fs::write(source.join("legacy-daemon"), b"old compatible daemon").unwrap();
        fs::set_permissions(
            source.join("legacy-daemon"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let mut view = View::default();
        view.seq = 1;
        view.board.recorded_since = Some(1);
        view.apply(1, 1, &Event::Orchestrator(alondra_core::orchestrator::Transition::Enabled {
            session_id: Some("orchestrator-session".into()),
        }));
        let mut assigned_node = alondra_core::model::Node::new("session-1", 1);
        assigned_node.name = Some("Assigned node".into());
        view.nodes.insert(assigned_node.session_id.clone(), assigned_node);
        let mut idle_node = alondra_core::model::Node::new("idle-session", 1);
        idle_node.name = Some("Idle node".into());
        idle_node.pid = Some(123);
        idle_node.alive = false;
        view.nodes.insert(idle_node.session_id.clone(), idle_node);
        let mut orchestrator_node = alondra_core::model::Node::new("orchestrator-session", 1);
        orchestrator_node.is_orchestrator = true;
        view.nodes.insert(orchestrator_node.session_id.clone(), orchestrator_node);
        view.board.requests.insert(
            "request-1".into(),
            serde_json::from_value(json!({
                "id":"request-1","text":"Do the migration","created_at":1,
                "attachments":[{"name":"question.txt","source_path":"/synthetic/question.txt",
                    "stored_path":source.join("files/question.txt").to_str().unwrap(),"bytes":16}]
            }))
            .unwrap(),
        );
        view.board.requests.insert(
            "request-closed".into(),
            serde_json::from_value(json!({
                "id":"request-closed","text":"What happened?","created_at":1,
                "closed":{"answer":"The historical answer","withdrawn":false,"at":2}
            }))
            .unwrap(),
        );
        view.board.tasks.insert(
            "task-1".into(),
            serde_json::from_value(json!({
                "id":"task-1","request":"request-1","title":"Migration Task",
                "state":"in_review","state_since":1,"created_at":1,"recorded":true,
                "assignee":{"session_id":"session-1"},
                "prs":[1,2],"final_report":1
            }))
            .unwrap(),
        );
        view.board.requests.insert(
            "request-sched".into(),
            serde_json::from_value(json!({
                "id":"request-sched","text":"Scheduled mission","created_at":1
            }))
            .unwrap(),
        );
        view.board.tasks.insert(
            "task-sched".into(),
            serde_json::from_value(json!({
                "id":"task-sched","request":"request-sched","title":"Scheduled Task",
                "state":"not_started","state_since":1,"created_at":1,"recorded":true
            }))
            .unwrap(),
        );
        for number in [1, 2] {
            let mut item = pr::PullRequest::opened(
                number,
                &format!("https://github.com/owner/repo/pull/{number}"),
                "session-1",
                None,
                1,
            )
            .unwrap();
            item.task = Some("task-1".into());
            item.head_sha = Some(format!("{number:040x}"));
            item.head = Some(format!("feature-{number}"));
            item.base = Some(if number == 2 { "feature-1".into() } else { "main".into() });
            item.merge_methods = vec![pr::MergeMethod::Merge];
            view.prs.insert(number, item);
        }
        view.questions.insert(
            1,
            serde_json::from_value(json!({
                "id":1,"session_id":"session-1","node_name":null,"title":"Which option?",
                "blocking":true,"origin":"ask","status":"open","asked_at":1,
                "task":"task-1","attachments":[{"name":"question.txt",
                    "source_path":"/synthetic/question.txt",
                    "stored_path":source.join("files/question.txt").to_str().unwrap(),"bytes":16}]
            }))
            .unwrap(),
        );
        view.questions.insert(
            2,
            serde_json::from_value(json!({
                "id":2,"session_id":"session-1","node_name":null,"title":"Pick one",
                "blocking":false,"origin":"ask","status":"answered","asked_at":1,
                "task":"task-1","options":[{"label":"Choice A"}],
                "answer":{"text":"","option":"Choice A","by":"user","at":2}
            }))
            .unwrap(),
        );
        view.questions.insert(
            3,
            serde_json::from_value(json!({
                "id":3,"session_id":"idle-session","node_name":null,"title":"Still there?",
                "blocking":false,"origin":"ask","status":"open","asked_at":1,"task":"task-1"
            }))
            .unwrap(),
        );
        view.reports.insert(
            1,
            serde_json::from_value(json!({
                "id":1,"session_id":"session-1","node_name":null,
                "title":"Legacy document","body":"Document contents","kind":"document",
                "created_at":1,"task":"task-1"
            }))
            .unwrap(),
        );
        let schedules = Connection::open(source.join("schedules.db")).unwrap();
        schedules.execute_batch("CREATE TABLE schedules(id TEXT PRIMARY KEY,body TEXT NOT NULL);
            CREATE TABLE local_command_results(sender TEXT NOT NULL,request_id TEXT NOT NULL,digest BLOB NOT NULL,result TEXT NOT NULL,PRIMARY KEY(sender,request_id));
            CREATE TABLE schedule_failed_launches(request TEXT PRIMARY KEY);
            CREATE TABLE schedule_launch_tools(request TEXT PRIMARY KEY,patterns TEXT NOT NULL);
            INSERT INTO local_command_results(sender,request_id,digest,result) VALUES('phone','sched-old',zeroblob(32),'{}');
            INSERT INTO schedule_failed_launches(request) VALUES('request-sched');
            INSERT INTO schedule_launch_tools(request,patterns) VALUES('request-sched','[\"Bash\"]');").unwrap();
        for (id, status) in [("sched-sent", "sent"), ("sched-never", "never_run")] {
            let sent = status == "sent";
            let body = json!({
                "id":id,"title":"Nightly review","text":"Review the project","project":null,
                "options":{},"disallowed_tools":[],"frequency":"daily","hour":9,"minute":0,
                "weekday":null,"interval":null,"start_at":null,"enabled":sent,
                "next_run":if sent { Some(2000) } else { None },
                "last_run":if sent { Some(1000) } else { None },
                "last_request":if sent { Some("request-sched") } else { None },
                "last_error":null,"status":status,"revision":if sent { 3 } else { 1 }
            });
            schedules.execute("INSERT INTO schedules(id,body) VALUES(?1,?2)",
                rusqlite::params![id, body.to_string()]).unwrap();
        }
        drop(schedules);
        let db = Connection::open(source.join("alondra.db")).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE event_log(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,session TEXT,body TEXT NOT NULL); CREATE TABLE snapshots(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,body TEXT NOT NULL);").unwrap();
        db.execute(
            "INSERT INTO snapshots(seq,at,body) VALUES(1,1,?1)",
            [serde_json::to_string(&view).unwrap()],
        )
        .unwrap();
        let event = Event::Clock {
            machine_strained: false,
        };
        db.execute(
            "INSERT INTO event_log(seq,at,kind,session,body) VALUES(2,2,'clock',NULL,?1)",
            [serde_json::to_string(&event).unwrap()],
        )
        .unwrap();
        drop(db);
        let source_view = load_legacy_readonly(&source.join("alondra.db")).unwrap();
        assert_eq!(source_view.seq, 2, "committed WAL tail must be replayed");
        let corrupt = Connection::open(source.join("alondra.db")).unwrap();
        corrupt
            .execute(
                "INSERT INTO snapshots(seq,at,body) VALUES(3,3,?1)",
                [serde_json::to_string(&view).unwrap()],
            )
            .unwrap();
        assert!(load_legacy_readonly(&source.join("alondra.db")).is_err());
        corrupt
            .execute("DELETE FROM snapshots WHERE seq=3", [])
            .unwrap();
        drop(corrupt);
        let forbidden_stage = source.join("inside");
        assert!(rehearse(&source, &forbidden_stage).is_err());
        assert!(
            !forbidden_stage.exists(),
            "rehearsal cannot mutate source HOME"
        );
        let prepared =
            prepare_cutover_offline(&source, &backup, &target, &source.join("legacy-daemon"))
                .unwrap();
        let manifest_digest = prepared.backup_manifest_sha256;
        let manifest = verify_backup_expected(&backup, &manifest_digest).unwrap();
        assert_eq!(manifest.source_seq, 2);
        let result = prepared.rehearsal;
        assert_eq!(result.source.requests, 3);
        assert_eq!(result.source.tasks, 2);
        assert_eq!(result.imported.agents, 2, "Task assignee and idle node are distinct Agents");
        assert_eq!(result.imported.split_pr_tasks, 1);
        assert_eq!(result.imported.formal_prs, 2);
        assert_eq!(result.imported.questions, 3);
        assert_eq!(result.imported.request_closures, 1);
        assert_eq!(result.imported.request_attachments, 1);
        assert_eq!(result.imported.open_questions_requiring_reconciliation, 1,
            "an open question from a dead session stays historical");
        assert_eq!(result.imported.open_questions_reopened, 1,
            "an open question from a live session stays answerable");
        assert_eq!(result.imported.final_presentations, 2,
            "task-1 and its split PR Task keep their review context");
        assert_eq!(result.imported.stacked_prs, vec![(2, 1)],
            "a PR train is reported, not converted into a stack dependency");
        assert_eq!(result.imported.documents, 1);
        assert_eq!(result.imported.attachments, 2);
        assert_eq!(result.imported.attachment_size_discrepancies, 2);
        let mut domain =
            crate::domain_store::DomainStore::open(&target.join("domain-v2.db")).unwrap();
        let provenance = Connection::open(target.join("domain-v2.db")).unwrap();
        let agent_session: String = provenance.query_row("SELECT source_id FROM legacy_import_map
            WHERE source_kind='session' AND role='agent_conversation' AND target_kind='agent'
            AND target_id='session-1'", [], |row| row.get(0)).unwrap();
        assert_eq!(agent_session, "session-1");
        let idle_session: String = provenance.query_row("SELECT source_id FROM legacy_import_map
            WHERE source_kind='session' AND role='agent_conversation' AND target_kind='agent'
            AND target_id='idle-session'", [], |row| row.get(0)).unwrap();
        assert_eq!(idle_session, "idle-session");
        let node_rows: i64 = provenance.query_row("SELECT COUNT(*) FROM legacy_import_map
            WHERE source_kind='node' AND role='entity' AND target_kind='agent'", [], |row| row.get(0)).unwrap();
        assert_eq!(node_rows, 2, "Orchestrator is not imported as an Agent");
        let orchestrator_session: String = provenance.query_row("SELECT source_id FROM legacy_import_map
            WHERE source_kind='session' AND role='orchestrator_conversation' AND target_kind='orchestrator'
            AND target_id='generation-0'", [], |row| row.get(0)).unwrap();
        assert_eq!(orchestrator_session, "orchestrator-session");
        let imported = domain.load().unwrap();
        assert_eq!(imported.agents.len(), 2);
        assert_eq!(imported.agents[&alondra_core::domain_v2::AgentId("session-1".into())].name, "Assigned node");
        let idle = &imported.agents[&alondra_core::domain_v2::AgentId("idle-session".into())];
        assert_eq!(idle.name, "Idle node");
        assert!(!idle.connected, "legacy PID must not become v2 connection state");
        assert!(
            imported
                .interactions
                .values()
                .any(|i| i.body.contains("The historical answer"))
        );
        assert!(
            imported
                .interactions
                .values()
                .any(|i| i.body.contains("\"option\":\"Choice A\""))
        );
        {
            use alondra_core::domain_v2::*;
            let reopened = &imported.interactions[&InteractionId("legacy-question-1".into())];
            assert!(matches!(reopened.payload, InteractionPayload::Question { answer: None, .. }));
            assert_eq!(reopened.author, Author::Agent { agent_id: "session-1".into() });
            assert!(reopened.blocking);
            assert!(matches!(
                imported.interactions[&InteractionId("legacy-question-3".into())].payload,
                InteractionPayload::Information { .. }
            ));
            let task = &imported.tasks[&TaskId("task-1".into())];
            assert_eq!(task.state, TaskState::NeedsYou, "derived at import from the reopened blocking question");
            assert_eq!(task.final_presentation, Some(InteractionId("legacy-final-task-1".into())));
            assert!(task.documents.contains_key(&DocumentId("legacy-document-1".into())));
            assert!(imported.document_reviews.iter().any(|r| r.task_id.0 == "task-1" && r.document_id.0 == "legacy-document-1"));
            let presentation = &imported.interactions[&InteractionId("legacy-final-task-1".into())];
            assert_eq!(presentation.author, Author::Agent { agent_id: "session-1".into() });
            assert!(matches!(&presentation.payload, InteractionPayload::Delivery { targets, is_final: true, acceptance: None, .. }
                if targets == &vec![
                    DeliveryTarget::PullRequest { pr_id: "legacy-pr-1".into(), head: format!("{:040x}", 1) },
                    DeliveryTarget::DocumentRevision { document_id: "legacy-document-1".into(), revision_id: "legacy-document-revision-1".into() },
                ]));
            assert_eq!(imported.tasks[&TaskId("legacy-pr-task-2".into())].state, TaskState::InReview);
            assert_eq!(imported.tasks[&TaskId("task-sched".into())].state, TaskState::NotStarted);
            let stacked: String = provenance.query_row("SELECT target_id FROM legacy_import_map
                WHERE source_kind='pr' AND source_id='2' AND role='stacked_on'", [], |row| row.get(0)).unwrap();
            assert_eq!(stacked, "legacy-pr-1");
            let inventory = domain.legacy_schedule_inventory().unwrap();
            assert_eq!(inventory.counts(), (2, 1, 1, 1, 1), "schedules, runs, tools, receipts and failed launches are imported");
            let run = inventory.state.runs.values().next().unwrap();
            assert_eq!(run.task_id, Some(TaskId("task-sched".into())));
        }
        let bad_attachment = alondra_core::model::Attachment {
            name: "question.txt".into(),
            source_path: "/synthetic/question.txt".into(),
            stored_path: Some("/foreign/files/question.txt".into()),
            bytes: Some(16),
        };
        assert!(import::stored_file(&backup, &bad_attachment).is_err());
        let original_bytes = fs::read(backup.join("files/question.txt")).unwrap();
        fs::write(backup.join("files/question.txt"), b"temporary substitution").unwrap();
        assert!(
            import::read_stored_bytes(&backup, &view.questions[&1].attachments[0]).is_err(),
            "consumed bytes must match the backup manifest even if the file is later restored"
        );
        fs::write(backup.join("files/question.txt"), &original_bytes).unwrap();
        verify_backup_expected(&backup, &manifest_digest).unwrap();
        assert_eq!(imported.pull_requests.len(), 2);
        assert!(
            imported
                .pull_requests
                .contains_key(&PrId("legacy-pr-2".into()))
        );
        assert!(
            domain.import_initial(&imported, &[], 2, &LegacyScheduleInventory::default()).is_err(),
            "a populated target cannot be reimported"
        );
        drop(domain);
        drop(crate::domain_runtime::DomainRuntime::open(&source).unwrap());
        assert!(crate::legacy_sidecar_is_empty(&source.join("domain-v2.db")).unwrap());
        assert_eq!(
            crate::domain_runtime::DomainRuntime::open(&target)
                .unwrap()
                .state()
                .unwrap()
                .pull_requests
                .len(),
            2,
            "the runtime must open the same filename populated by migration"
        );
        let schedule_bytes = fs::read(source.join("schedules.db")).unwrap();
        let late_schedules = Connection::open(source.join("schedules.db")).unwrap();
        late_schedules.execute_batch("INSERT INTO schedules(id,body) VALUES('late','{\"enabled\":true}');").unwrap();
        drop(late_schedules);
        let error = crate::domain_cutover::promote_stage_offline(
            &source,
            &backup,
            &manifest_digest,
            &target,
        )
        .unwrap_err().to_string();
        assert!(error.contains("current HOME differs from verified cutover backup"), "{error}");
        assert!(crate::domain_cutover::read_marker(&source).unwrap().is_none());
        fs::write(source.join("schedules.db"), &schedule_bytes).unwrap();
        let staged_schedules = Connection::open(target.join("domain-v2.db")).unwrap();
        staged_schedules.execute("DELETE FROM schedule_failed_launches", []).unwrap();
        let error = crate::domain_cutover::promote_stage_offline(
            &source,
            &backup,
            &manifest_digest,
            &target,
        )
        .unwrap_err().to_string();
        assert!(error.contains("staged schedule import differs"), "{error}");
        staged_schedules.execute("INSERT INTO schedule_failed_launches(request_id) VALUES('request-sched')", []).unwrap();
        drop(staged_schedules);
        let provenance = target.join("domain-v2-stage.json");
        let original_provenance = fs::read(&provenance).unwrap();
        fs::write(&provenance, b"mismatched backup").unwrap();
        assert!(
            crate::domain_cutover::promote_stage_offline(
                &source,
                &backup,
                &manifest_digest,
                &target,
            )
            .is_err(),
            "a stage without verified provenance must not fence HOME"
        );
        assert!(
            crate::domain_cutover::read_marker(&source)
                .unwrap()
                .is_none()
        );
        fs::write(&provenance, original_provenance).unwrap();
        {
            let connection = Connection::open(target.join("domain-v2.db")).unwrap();
            connection.execute("INSERT INTO local_command_results(sender,request_id,digest,result) VALUES('test','unreconciled',?1,'{}')", [vec![0u8;32]]).unwrap();
        }
        assert!(
            crate::domain_cutover::promote_stage_offline(
                &source,
                &backup,
                &manifest_digest,
                &target,
            )
            .is_err(),
            "a stage with technical receipts must not be promoted"
        );
        assert!(
            crate::domain_cutover::read_marker(&source)
                .unwrap()
                .is_none()
        );
        Connection::open(target.join("domain-v2.db"))
            .unwrap()
            .execute("DELETE FROM local_command_results WHERE sender='test'", [])
            .unwrap();
        let live_file = source.join("files/question.txt");
        let live_bytes = fs::read(&live_file).unwrap();
        fs::write(&live_file, b"changed without an event").unwrap();
        assert!(
            crate::domain_cutover::promote_stage_offline(
                &source,
                &backup,
                &manifest_digest,
                &target,
            )
            .is_err(),
            "changed HOME content with the same event sequence must be rejected"
        );
        assert!(
            crate::domain_cutover::read_marker(&source)
                .unwrap()
                .is_none()
        );
        fs::write(&live_file, live_bytes).unwrap();
        let prepared_marker = crate::domain_cutover::Marker {
            format: 1,
            phase: crate::domain_cutover::Phase::Prepared,
            backup_manifest_sha256: manifest_digest.clone(),
            source_seq: manifest.source_seq,
            data_epoch: imported.data_epoch.0.clone(),
        };
        let marker_path = source.join(crate::domain_cutover::MARKER);
        fs::write(&marker_path, serde_json::to_vec(&prepared_marker).unwrap()).unwrap();
        fs::set_permissions(&marker_path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(source.join("domain-v2.db.next"), b"interrupted copy").unwrap();
        crate::domain_cutover::abort_prepared_offline(&source, &backup, &manifest_digest).unwrap();
        assert!(
            crate::domain_cutover::read_marker(&source)
                .unwrap()
                .is_none()
        );
        assert!(!source.join("domain-v2.db.next").exists());
        drop(crate::domain_runtime::DomainRuntime::open(&source).unwrap());
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("domain_migration::tests::crash_after_empty_sidecar_wal")
            .env("ALONDRA_CUTOVER_WAL_CHILD", source.join("domain-v2.db"))
            .status()
            .unwrap();
        assert!(
            child.success(),
            "crashed SQLite child must exit after durable WAL write"
        );
        assert!(source.join("domain-v2.db-wal").exists());
        fs::write(&marker_path, serde_json::to_vec(&prepared_marker).unwrap()).unwrap();
        fs::set_permissions(&marker_path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(source.join("domain-v2.db.next"), b"interrupted copy").unwrap();
        fs::write(
            source.join("domain-v2-cutover.json.next"),
            b"interrupted marker",
        )
        .unwrap();
        let interrupted_content = source.join("domain-v2-content-next");
        fs::create_dir(&interrupted_content).unwrap();
        fs::set_permissions(&interrupted_content, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(interrupted_content.join("partial-copy"), b"partial").unwrap();
        let marker = crate::domain_cutover::promote_stage_offline(
            &source,
            &backup,
            &manifest_digest,
            &target,
        )
        .unwrap();
        assert_eq!(marker.phase, crate::domain_cutover::Phase::Committed);
        assert!(!source.join("domain-v2.db-wal").exists());
        assert!(!interrupted_content.exists());
        assert!(
            crate::domain_cutover::abort_prepared_offline(&source, &backup, &manifest_digest)
                .is_err(),
            "committed HOME cannot be reopened as legacy"
        );
        assert_eq!(marker.source_seq, 2);
        assert_eq!(
            crate::domain_cutover::read_marker(&source)
                .unwrap()
                .unwrap(),
            marker
        );
        assert_eq!(
            crate::domain_runtime::DomainRuntime::open(&source)
                .unwrap()
                .state()
                .unwrap(),
            imported,
            "the promoted HOME must load the staged domain without losing legacy config"
        );
        {
            use alondra_core::domain_v2::*;
            assert_eq!(
                crate::domain_store::DomainStore::open(&source.join("domain-v2.db")).unwrap()
                    .legacy_schedule_inventory().unwrap().counts(),
                (2, 1, 1, 1, 1)
            );
            let runtime = crate::domain_runtime::DomainRuntime::open(&source).unwrap();
            let before = runtime.state().unwrap();
            let envelope = |id: &str, command| CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: before.data_epoch.clone(),
                command_id: id.into(),
                expected_revision: runtime.state().unwrap().revision,
                command,
            };
            runtime.execute(&Actor::Agent { agent_id: "session-1".into() }, &envelope("live-info",
                Command::PublishInformation {
                    attachments: vec![],
                    interaction_id: "live-info".into(),
                    scope: Scope::Task { task_id: "task-1".into() },
                    recipient: Recipient::User,
                    body: "Still working after cutover".into(),
                }), None, || Ok(())).unwrap();
            assert_eq!(runtime.state().unwrap().tasks, before.tasks,
                "the first live command must not rewrite imported Task states or revisions");
            let answered = runtime.execute(&Actor::User { user_id: "device:phone".into() }, &envelope("answer",
                Command::AnswerQuestion {
                    attachments: vec![],
                    interaction_id: "legacy-question-1".into(),
                    option_id: String::new(),
                    inputs: std::collections::BTreeMap::from([("text".to_string(), InputValue::Text("Go ahead".into()))]),
                }), None, || Ok(())).unwrap();
            assert_eq!(answered.effects, vec![DomainEffect::NotifyAgent {
                agent_id: "session-1".into(),
                task_id: Some("task-1".into()),
                interaction_id: Some("legacy-question-1".into()),
                message: "Question answered".into(),
            }]);
            assert_eq!(runtime.state().unwrap().tasks[&TaskId("task-1".into())].state, TaskState::InReview);
        }
        assert_eq!(
            fs::read(source.join("tui-device")).unwrap(),
            b"private device identity",
            "promotion must preserve the existing identity and configuration"
        );
        assert!(
            crate::domain_cutover::promote_stage_offline(
                &source,
                &backup,
                &manifest_digest,
                &target,
            )
            .is_err(),
            "a committed HOME cannot be promoted twice"
        );
        assert_eq!(
            restore_rehearsal(&backup, &manifest_digest, &restored)
                .unwrap()
                .source_seq,
            2
        );
        assert_ne!(
            fs::metadata(restored.join("legacy-alondrad"))
                .unwrap()
                .mode()
                & 0o111,
            0
        );
        assert_eq!(
            fs::read(restored.join("service.toml.staged")).unwrap(),
            b"staged service configuration"
        );
        assert_eq!(
            fs::read(restored.join("tui-device")).unwrap(),
            b"private device identity"
        );
        assert_eq!(
            fs::read(restored.join("candidates/candidate")).unwrap(),
            b"candidate daemon"
        );
        fs::write(backup.join("files/unlisted.txt"), b"unmanifested").unwrap();
        assert!(verify_backup_expected(&backup, &manifest_digest).is_err());
        fs::remove_file(backup.join("files/unlisted.txt")).unwrap();
        fs::write(backup.join("files/question.txt"), b"tampered").unwrap();
        assert!(
            verify_backup(&backup).is_err(),
            "backup verification must detect changed attachment"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn crash_after_empty_sidecar_wal() {
        let Some(path) = std::env::var_os("ALONDRA_CUTOVER_WAL_CHILD") else {
            return;
        };
        let wal = Connection::open(Path::new(&path)).unwrap();
        wal.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
             CREATE TABLE abandoned_wal_test(value INTEGER);
             DROP TABLE abandoned_wal_test;",
        )
        .unwrap();
        assert!(Path::new(&path).with_file_name("domain-v2.db-wal").exists());
        unsafe { libc::_exit(0) };
    }
}
