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
pub(crate) mod rehearsal;
mod schedules;
#[cfg(test)]
pub(crate) mod synthetic;
pub(crate) use import::{ImportLink, ImportReport, materialize_legacy};
pub(crate) use schedules::{LegacyScheduleInventory, LegacyScheduleReceiptRow};

#[derive(Debug, Default, Serialize)]
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

/// A delivery the legacy daemon had not settled: the importer conserves it.
pub(crate) fn unresolved_delivery(attempt: &alondra_core::delivery::Attempt) -> bool {
    use alondra_core::delivery::State;
    matches!(attempt.state, State::Dispatching | State::Uncertain | State::Retryable)
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

pub(crate) const BACKUP_DATABASES: [&str; 4] = [
    "alondra.db",
    "schedules.db",
    "service-receipts.sqlite",
    "service-upload-attempts.sqlite",
];
pub(crate) const BACKUP_FILES: [&str; 14] = [
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
];
pub(crate) const BACKUP_TREES: [&str; 4] = ["service-keys", "files", "candidates", "soak"];
/// The installed binaries a rollback reinstalls; kept in the backup, never restored into HOME.
pub(crate) const LEGACY_BINARIES: &str = "legacy-bin";

/// Copy what a rollback needs from a stopped HOME into a new private
/// directory: each SQLite store through the backup API, configuration, keys
/// and stored files, with a manifest of every byte. The caller holds the
/// exclusive HOME lock. `recorded_home` is the HOME the legacy events were
/// written in; absolute attachment paths are resolved against it.
pub(crate) fn backup_home(
    home: &Path, recorded_home: &Path, destination: &Path, binaries: &[&Path],
) -> Result<BackupManifest> {
    ensure!(home.is_absolute() && recorded_home.is_absolute(), "source HOME must be absolute");
    destination_outside(home, destination)?;
    ensure_legacy_schedules_readable(home)?;
    fs::DirBuilder::new().mode(0o700).create(destination)?;
    let meta = fs::symlink_metadata(destination)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "backup destination must be private and owner-only"
    );
    let view = load_legacy_readonly(&home.join("alondra.db"))?;
    let mut files = Vec::new();
    for name in BACKUP_DATABASES {
        let source = home.join(name);
        if source.exists() {
            let target = destination.join(name);
            sqlite_backup(&source, &target)?;
            files.push(describe_file(destination, &target)?);
        }
    }
    for name in BACKUP_FILES {
        let source = home.join(name);
        if source.exists() {
            let target = destination.join(name);
            copy_regular(&source, &target)?;
            files.push(describe_file(destination, &target)?);
        }
    }
    for directory in BACKUP_TREES {
        let source = home.join(directory);
        if source.exists() {
            copy_tree(&source, &destination.join(directory), destination, &mut files)?;
        }
    }
    if !binaries.is_empty() {
        fs::DirBuilder::new().mode(0o700).create(destination.join(LEGACY_BINARIES))?;
    }
    for binary in binaries {
        let target = destination.join(LEGACY_BINARIES).join(binary.file_name().context("binary has no name")?);
        copy_regular(&binary.canonicalize()?, &target)?;
        files.push(describe_file(destination, &target)?);
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = BackupManifest {
        format: 1,
        source_home: recorded_home.to_string_lossy().into_owned(),
        canonical_source_home: home.canonicalize()?.to_string_lossy().into_owned(),
        source_seq: view.seq,
        files,
    };
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(destination.join("manifest.json"))?;
    output.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
    output.sync_all()?;
    File::open(destination)?.sync_all()?;
    ensure!(
        load_legacy_readonly(&home.join("alondra.db"))?.seq == view.seq,
        "legacy writer advanced during the backup"
    );
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

/// Set by the cutover rehearsal on a copy of a live HOME: the live HOME's path.
pub(crate) const RECORDED_HOME: &str = "ALONDRA_CUTOVER_RECORDED_HOME";

fn materialize_with_schedules(
    view: &View, source: &import::Source<'_>, target_home: &Path, epoch: d::DataEpoch,
) -> Result<(d::DomainState, ImportReport, LegacyScheduleInventory)> {
    let (mut state, mut imported) = materialize_legacy(view, source, target_home, epoch)?;
    // A backup already holds a rollback-journal copy. A rehearsal straight
    // from a live HOME snapshots its WAL database the same way first.
    let (schedules, dropped) = if source.home.join("manifest.json").exists()
        || !source.home.join("schedules.db").exists()
    {
        schedules::read_legacy_schedules_counted(source.home, view, &state)?
    } else {
        let snapshot = std::env::temp_dir().join(format!(
            "alondra-domain-schedule-snapshot-{}",
            crate::auth::random_hex(8)
        ));
        fs::DirBuilder::new().mode(0o700).create(&snapshot)?;
        let result = sqlite_backup(&source.home.join("schedules.db"), &snapshot.join("schedules.db"))
            .and_then(|()| schedules::read_legacy_schedules_counted(&snapshot, view, &state));
        fs::remove_dir_all(&snapshot)?;
        result?
    };
    imported.schedule_links_dropped = dropped.links;
    imported.schedule_runs_dropped = dropped.runs;
    state.schedule = schedules.state.clone();
    state.validate().map_err(anyhow::Error::new)?;
    Ok((state, imported, schedules))
}

/// Import the work in flight from `source_home` into a new private stage.
/// Nothing is written to the source; the stage is left for inspection.
fn import_into(source: &import::Source<'_>, target_home: &Path) -> Result<(d::DomainState, ImportReport, LegacyInventory, usize)> {
    destination_outside(source.home, target_home)?;
    ensure_legacy_schedules_readable(source.home)?;
    fs::DirBuilder::new().mode(0o700).create(target_home)?;
    let view = load_legacy_readonly(&source.home.join("alondra.db"))?;
    let legacy = inventory(&view);
    let mut store = crate::domain_store::DomainStore::open(&target_home.join("domain-v2.db"))?;
    let epoch = store.load()?.data_epoch;
    let (state, imported, schedules) = materialize_with_schedules(&view, source, target_home, epoch)?;
    store.import_initial(&state, &imported.links, view.seq, &schedules)?;
    store.integrity_check()?;
    let loaded = store.load()?;
    ensure!(loaded == state, "domain import SQL roundtrip changed business state");
    let projection = store.project_bounded(crate::service_overview::MAX_BYTES)?;
    for revision in loaded.document_revisions.values() {
        verify_content(target_home, &revision.content_reference, &revision.content_digest)?;
    }
    for interaction in loaded.interactions.values() {
        for attachment in &interaction.attachments {
            verify_content(target_home, &attachment.content_reference, &attachment.content_digest)?;
        }
    }
    Ok((loaded, imported, legacy, projection.len()))
}

/// Import a verified cutover backup into `stage`.
pub(crate) fn import_backup(backup: &Path, recorded_home: &Path, stage: &Path) -> Result<(d::DomainState, ImportReport)> {
    let digest = backup_manifest_digest(backup)?;
    verify_backup_expected(backup, &digest)?;
    let (state, imported, _, _) = import_into(&import::Source { home: backup, recorded_home }, stage)?;
    verify_backup_expected(backup, &digest)?;
    Ok((state, imported))
}

/// Rehearse the import against a source HOME and a brand-new target HOME.
pub(crate) fn rehearse(source_home: &Path, target_home: &Path) -> Result<Rehearsal> {
    let (state, imported, source, projection_bytes) =
        import_into(&import::Source { home: source_home, recorded_home: source_home }, target_home)?;
    Ok(Rehearsal { source, imported, projection_bytes, new_data_epoch: state.data_epoch.0 })
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

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::pr;
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
        let error = backup_home(&source, &source, &backup, &[]).unwrap_err().to_string();
        assert!(error.contains("unknown table"), "{error}");
        assert!(!backup.exists(), "a refused cutover must leave no backup");
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
    fn one_command_cutover_imports_work_in_flight_and_rolls_back_exactly() {
        let root = std::env::temp_dir().join(format!(
            "alondra-domain-migration-{}",
            crate::auth::random_hex(8)
        ));
        let source = root.join("source");
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
        let backup = root.join("backup");
        let legacy = source.join("legacy-daemon");
        {
            let held = alondra_core::home_lock::HomeLock::recovery(&source).unwrap();
            let error = crate::domain_cutover::refuse_legacy_home(&source).unwrap_err().to_string();
            assert!(error.contains("still holds legacy data"), "{error}");
            drop(held);
        }
        let wait = crate::domain_cutover::LockWait { attempts: 1, interval: Duration::ZERO };
        let result = crate::domain_cutover::cutover_offline(&source, &backup, &source, &[&legacy], &wait, |_| ())
            .unwrap();
        assert!(!result.resumed && !result.already_committed);
        assert!(result.rollback.contains("--rollback-domain-v2"));
        let imported = result.imported.unwrap();
        assert_eq!(imported.source_seq, 2, "committed WAL tail must be imported");
        assert_eq!((imported.requests, imported.skipped.requests), (2, 1), "the closed Request stays behind");
        assert_eq!((imported.tasks, imported.derived_pr_tasks), (2, 1));
        assert_eq!(imported.agents, 1, "only assignees of work in flight become Agents");
        assert_eq!(imported.prs, 2);
        assert_eq!(imported.stacked_prs, vec![(2, 1)],
            "a PR train is reported, not converted into a stack dependency");
        assert_eq!((imported.questions_reopened, imported.questions_unread, imported.skipped.questions), (1, 1, 1),
            "the assignee's open question stays answerable, another session's arrives unread, the answered one stays behind");
        assert_eq!((imported.documents, imported.reports_unread), (1, 1));
        assert_eq!(imported.final_presentations, 2, "task-1 and its derived PR Task keep their review context");
        assert_eq!((imported.attachments, imported.attachment_size_discrepancies), (2, 2));
        crate::domain_cutover::refuse_legacy_home(&source).unwrap();
        assert_eq!(result.marker.phase, crate::domain_cutover::Phase::Committed);
        assert_eq!(crate::domain_cutover::read_marker(&source).unwrap().unwrap(), result.marker);
        assert!(!source.join("alondra.db").exists() && !source.join("schedules.db").exists());
        let archive = source.join(crate::domain_cutover::ARCHIVE);
        assert_eq!(load_legacy_readonly(&archive.join("alondra.db")).unwrap().seq, 2,
            "the legacy store stays in HOME, renamed");
        assert!(archive.join("schedules.db").exists());
        assert_eq!(fs::read(backup.join(LEGACY_BINARIES).join("legacy-daemon")).unwrap(), b"old compatible daemon");
        let bad_attachment = alondra_core::model::Attachment {
            name: "question.txt".into(),
            source_path: "/synthetic/question.txt".into(),
            stored_path: Some("/foreign/files/question.txt".into()),
            bytes: Some(16),
        };
        assert!(import::stored_file(&import::Source { home: &backup, recorded_home: &source }, &bad_attachment).is_err());
        let again = crate::domain_cutover::cutover_offline(&source, &backup, &source, &[&legacy], &wait, |_| ()).unwrap();
        assert!(again.already_committed, "a second run only reports the committed cutover");
        {
            use alondra_core::domain_v2::*;
            let runtime = crate::domain_runtime::DomainRuntime::open(&source).unwrap();
            let state = runtime.state().unwrap();
            assert_eq!(state.agents.len(), 1);
            assert_eq!(state.agents[&AgentId("session-1".into())].name, "Assigned node");
            let reopened = &state.interactions[&InteractionId("legacy-question-1".into())];
            assert!(matches!(reopened.payload, InteractionPayload::Question { answer: None, .. }));
            assert_eq!(reopened.author, Author::Agent { agent_id: "session-1".into() });
            assert!(reopened.blocking);
            assert!(matches!(
                &state.interactions[&InteractionId("legacy-question-3".into())].payload,
                InteractionPayload::Information { read_by } if read_by.is_empty()
            ));
            assert!(!state.interactions.contains_key(&InteractionId("legacy-question-2".into())));
            let task = &state.tasks[&TaskId("task-1".into())];
            assert_eq!(task.state, TaskState::NeedsYou, "derived at import from the reopened blocking question");
            assert_eq!(task.final_presentation, Some(InteractionId("legacy-final-task-1".into())));
            assert!(task.documents.contains_key(&DocumentId("legacy-document-1".into())));
            let presentation = &state.interactions[&InteractionId("legacy-final-task-1".into())];
            assert!(matches!(&presentation.payload, InteractionPayload::Delivery { targets, is_final: true, acceptance: None, .. }
                if targets == &vec![
                    DeliveryTarget::PullRequest { pr_id: "legacy-pr-1".into(), head: format!("{:040x}", 1) },
                    DeliveryTarget::DocumentRevision { document_id: "legacy-document-1".into(), revision_id: "legacy-document-revision-1".into() },
                ]));
            assert_eq!(state.tasks[&TaskId("legacy-pr-task-2".into())].state, TaskState::InReview);
            assert_eq!(state.tasks[&TaskId("task-sched".into())].state, TaskState::NotStarted);
            let inventory = crate::domain_store::DomainStore::open(&source.join("domain-v2.db")).unwrap()
                .legacy_schedule_inventory().unwrap();
            assert_eq!(inventory.counts(), (2, 1, 1, 1, 1), "schedules, runs, tools, receipts and failed launches are imported");
            assert_eq!(inventory.state.runs.values().next().unwrap().task_id, Some(TaskId("task-sched".into())));
            let envelope = |id: &str, command| CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: state.data_epoch.clone(),
                command_id: id.into(),
                expected_revision: runtime.state().unwrap().revision,
                command,
            };
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
        }
        let reference = rehearsal::sqlite_content_digest(&backup.join("alondra.db")).unwrap();
        let original = fs::read(backup.join("files/question.txt")).unwrap();
        fs::write(backup.join("files/question.txt"), b"tampered").unwrap();
        assert!(crate::domain_cutover::rollback_offline(&source, &backup, &wait, |_| ()).is_err(),
            "a changed backup cannot restore HOME");
        assert!(crate::domain_cutover::read_marker(&source).unwrap().is_some());
        fs::write(backup.join("files/question.txt"), &original).unwrap();
        let aside = crate::domain_cutover::rollback_offline(&source, &backup, &wait, |_| ()).unwrap();
        assert!(crate::domain_cutover::read_marker(&source).unwrap().is_none());
        assert_eq!(rehearsal::sqlite_content_digest(&source.join("alondra.db")).unwrap(), reference);
        assert_eq!(load_legacy_readonly(&source.join("alondra.db")).unwrap().seq, 2);
        assert!(!source.join("domain-v2.db").exists() && !archive.exists());
        assert!(aside.join("domain-v2.db").exists() && aside.join(crate::domain_cutover::ARCHIVE).join("alondra.db").exists(),
            "what v2 wrote is set aside, not deleted");
        assert!(!source.join(LEGACY_BINARIES).exists(), "retained binaries are not restored into HOME");
        assert_eq!(fs::read(source.join("tui-device")).unwrap(), b"private device identity");
        assert_eq!(fs::read(source.join("candidates/candidate")).unwrap(), b"candidate daemon");
        assert!(crate::domain_cutover::refuse_legacy_home(&source).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduled_run_links_the_original_task_when_its_request_split_into_pr_tasks() {
        let root = std::env::temp_dir().join(format!(
            "alondra-schedule-split-{}",
            crate::auth::random_hex(8)
        ));
        let source = root.join("source");
        let stage = root.join("stage");
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::DirBuilder::new().mode(0o700).create(&source).unwrap();
        let mut view = View::default();
        view.seq = 1;
        view.board.recorded_since = Some(1);
        view.nodes.insert(
            "session-1".into(),
            alondra_core::model::Node::new("session-1", 1),
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
                "state":"in_review","state_since":1,"created_at":1,"recorded":true,
                "assignee":{"session_id":"session-1"},"prs":[1,2]
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
            item.task = Some("task-sched".into());
            item.head_sha = Some(format!("{number:040x}"));
            item.head = Some(format!("feature-{number}"));
            item.base = Some("main".into());
            item.merge_methods = vec![pr::MergeMethod::Merge];
            view.prs.insert(number, item);
        }
        let db = Connection::open(source.join("alondra.db")).unwrap();
        db.execute_batch("CREATE TABLE event_log(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,session TEXT,body TEXT NOT NULL); CREATE TABLE snapshots(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,body TEXT NOT NULL);").unwrap();
        db.execute(
            "INSERT INTO snapshots(seq,at,body) VALUES(1,1,?1)",
            [serde_json::to_string(&view).unwrap()],
        )
        .unwrap();
        drop(db);
        let schedules = Connection::open(source.join("schedules.db")).unwrap();
        schedules.execute_batch("CREATE TABLE schedules(id TEXT PRIMARY KEY,body TEXT NOT NULL);").unwrap();
        let body = json!({
            "id":"sched-sent","title":"Nightly review","text":"Review the project","project":null,
            "options":{},"disallowed_tools":[],"frequency":"daily","hour":9,"minute":0,
            "weekday":null,"interval":null,"start_at":null,"enabled":true,"next_run":2000,
            "last_run":1000,"last_request":"request-sched","last_error":null,"status":"sent",
            "revision":3
        });
        schedules
            .execute(
                "INSERT INTO schedules(id,body) VALUES('sched-sent',?1)",
                [body.to_string()],
            )
            .unwrap();
        drop(schedules);

        let result = rehearse(&source, &stage).unwrap();
        assert_eq!(result.imported.derived_pr_tasks, 1);
        let mut domain =
            crate::domain_store::DomainStore::open(&stage.join("domain-v2.db")).unwrap();
        let imported = domain.load().unwrap();
        let split = &imported.tasks[&d::TaskId("legacy-pr-task-2".into())];
        assert_eq!(split.request_id, d::RequestId("request-sched".into()));
        let inventory = domain.legacy_schedule_inventory().unwrap();
        let run = &inventory.state.runs[&d::ScheduleRunId("legacy-last-sched-sent".into())];
        assert_eq!(run.request_id, Some(d::RequestId("request-sched".into())));
        assert_eq!(run.task_id, Some(d::TaskId("task-sched".into())));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn import_reports_running_tasks_that_d02_holds_on_a_dependency_in_review() {
        let root = std::env::temp_dir().join(format!(
            "alondra-import-d02-{}",
            crate::auth::random_hex(8)
        ));
        let source = root.join("source");
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::DirBuilder::new().mode(0o700).create(&source).unwrap();
        let mut view = View::default();
        view.seq = 1;
        view.board.recorded_since = Some(1);
        view.board.requests.insert(
            "request-1".into(),
            serde_json::from_value(json!({"id":"request-1","text":"Two steps","created_at":1}))
                .unwrap(),
        );
        for (id, state, depends_on) in [
            ("task-upstream", "in_review", json!([])),
            ("task-downstream", "running", json!(["task-upstream"])),
            ("task-free", "running", json!([])),
        ] {
            view.board.tasks.insert(
                id.into(),
                serde_json::from_value(json!({
                    "id":id,"request":"request-1","title":id,"state":state,
                    "state_since":1,"created_at":1,"recorded":true,"depends_on":depends_on
                }))
                .unwrap(),
            );
        }
        let db = Connection::open(source.join("alondra.db")).unwrap();
        db.execute_batch("CREATE TABLE event_log(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,session TEXT,body TEXT NOT NULL); CREATE TABLE snapshots(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,body TEXT NOT NULL);").unwrap();
        db.execute(
            "INSERT INTO snapshots(seq,at,body) VALUES(1,1,?1)",
            [serde_json::to_string(&view).unwrap()],
        )
        .unwrap();
        drop(db);

        let result = rehearse(&source, &root.join("stage")).unwrap();
        assert_eq!(result.imported.d02_waiting_dependency, vec!["task-downstream".to_owned()]);
        fs::remove_dir_all(root).unwrap();
    }
}
