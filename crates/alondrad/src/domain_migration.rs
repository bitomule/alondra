//! Offline domain cutover preparation. Nothing here executes a legacy event or
//! a queued effect against a real producer. The original event database is an
//! audit source and rollback artifact, never the authority of domain v2.
use alondra_core::{Event, View};
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
pub(crate) use import::{ImportLink, ImportReport, materialize_legacy};

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

#[derive(Debug, Serialize, Deserialize)]
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

/// Rehearse against a source copy and a brand-new target HOME. The function
/// never writes the legacy source, and leaves the target for human inspection.
pub(crate) fn rehearse(source_home: &Path, target_home: &Path) -> Result<Rehearsal> {
    destination_outside(source_home, target_home)?;
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
    let (state, imported) = materialize_legacy(&view, source_home, target_home, epoch.clone())?;
    store.import_initial(&state, &imported.links, view.seq)?;
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
                "prs":[1,2]
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
            item.base = Some("main".into());
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
        view.reports.insert(
            1,
            serde_json::from_value(json!({
                "id":1,"session_id":"session-1","node_name":null,
                "title":"Legacy document","body":"Document contents","kind":"document",
                "created_at":1,"task":"task-1"
            }))
            .unwrap(),
        );
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
        assert_eq!(result.source.requests, 2);
        assert_eq!(result.source.tasks, 1);
        assert_eq!(result.imported.split_pr_tasks, 1);
        assert_eq!(result.imported.formal_prs, 2);
        assert_eq!(result.imported.questions, 2);
        assert_eq!(result.imported.request_closures, 1);
        assert_eq!(result.imported.request_attachments, 1);
        assert_eq!(result.imported.open_questions_requiring_reconciliation, 1);
        assert_eq!(result.imported.documents, 1);
        assert_eq!(result.imported.attachments, 2);
        assert_eq!(result.imported.attachment_size_discrepancies, 2);
        let mut domain =
            crate::domain_store::DomainStore::open(&target.join("domain-v2.db")).unwrap();
        let imported = domain.load().unwrap();
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
        assert!(matches!(
            imported.interactions
                [&alondra_core::domain_v2::InteractionId("legacy-question-1".into())]
                .payload,
            alondra_core::domain_v2::InteractionPayload::Information { .. }
        ));
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
            domain.import_initial(&imported, &[], 2).is_err(),
            "a populated target cannot be reimported"
        );
        drop(domain);
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
        let marker = crate::domain_cutover::promote_stage_offline(
            &source,
            &backup,
            &manifest_digest,
            &target,
        )
        .unwrap();
        assert_eq!(marker.phase, crate::domain_cutover::Phase::Committed);
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
}
