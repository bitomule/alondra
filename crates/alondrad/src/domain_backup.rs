//! The backup a cutover takes of a stopped HOME before it archives the legacy
//! stores. Nothing here reads a legacy record into domain v2: the original
//! event database is a rollback artifact and an archive, never an input.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
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
pub(crate) mod rehearsal;

/// The last sequence the legacy daemon recorded, from its snapshots or its event log.
pub(crate) fn legacy_seq(database: &Path) -> Result<u64> {
    let connection = Connection::open_with_flags(
        canonical_leaf(database)?,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let seq: i64 = connection.query_row(
        "SELECT max(coalesce((SELECT max(seq) FROM event_log),0),coalesce((SELECT max(seq) FROM snapshots),0))",
        [],
        |row| row.get(0),
    )?;
    Ok(u64::try_from(seq)?)
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
/// exclusive HOME lock.
pub(crate) fn backup_home(home: &Path, destination: &Path, binaries: &[&Path]) -> Result<BackupManifest> {
    ensure!(home.is_absolute(), "source HOME must be absolute");
    destination_outside(home, destination)?;
    fs::DirBuilder::new().mode(0o700).create(destination)?;
    let meta = fs::symlink_metadata(destination)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "backup destination must be private and owner-only"
    );
    let source_seq = legacy_seq(&home.join("alondra.db"))?;
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
        canonical_source_home: home.canonicalize()?.to_string_lossy().into_owned(),
        source_seq,
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
        legacy_seq(&home.join("alondra.db"))? == source_seq,
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
