//! The one-command cutover of a stopped HOME to domain v2, and its rollback.
//! The marker fences daemon startup before any legacy store moves; it is
//! committed only after the new database and content have been verified.
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

fn staged_state(stage: &Path) -> Result<alondra_core::domain_v2::DomainState> {
    crate::domain_store::DomainStore::open_readonly(&stage.join("domain-v2.db"))?.load()
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

/// Where the imported store waits until it is promoted: beside the backup,
/// outside HOME, so the backup directory holds exactly the verified copy.
fn stage_of(backup: &Path) -> Result<std::path::PathBuf> {
    let name = backup.file_name().context("backup directory has no name")?;
    Ok(backup.with_file_name(format!("{}.stage", name.to_string_lossy())))
}

fn canonical_parent(path: &Path) -> Result<std::path::PathBuf> {
    let parent = path.parent().context("path has no parent")?.canonicalize()?;
    Ok(parent.join(path.file_name().context("path has no name")?))
}

/// Where the legacy stores stay after the cutover, renamed inside HOME.
pub(crate) const ARCHIVE: &str = "legacy-v1";
const LEGACY_STORES: [&str; 2] = ["alondra.db", "schedules.db"];

/// A daemon built for domain v2 must not run a HOME that still holds legacy
/// data. It refuses before taking the HOME lock, so the cutover can take it.
pub(crate) fn refuse_legacy_home(home: &Path) -> Result<()> {
    if read_marker(home)?.is_some() {
        return Ok(());
    }
    ensure!(
        !legacy_data(&home.join("alondra.db")),
        "this HOME still holds legacy data; cut it over first with `alondrad --cutover-domain-v2` (see docs/domain-v2-live-cutover.md)"
    );
    Ok(())
}

/// A legacy store with any recorded event or snapshot. One that cannot be
/// read as such counts as data, so an unknown store is never started over.
fn legacy_data(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
        Ok(_) => (),
    }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW)
        .and_then(|store| store.query_row(
            "SELECT EXISTS(SELECT 1 FROM event_log) OR EXISTS(SELECT 1 FROM snapshots)", [], |row| row.get(0)))
        .unwrap_or(true)
}

/// How long the cutover waits for a running daemon to release HOME.
pub(crate) struct LockWait {
    pub attempts: u32,
    pub interval: std::time::Duration,
}

impl LockWait {
    pub(crate) const LIVE: LockWait = LockWait { attempts: 600, interval: std::time::Duration::from_secs(1) };
}

fn exclusive_lock(home: &Path, wait: &LockWait, mut progress: impl FnMut(u32)) -> Result<HomeLock> {
    for attempt in 1..=wait.attempts {
        match HomeLock::recovery(home) {
            Ok(lock) => return Ok(lock),
            Err(_) if attempt < wait.attempts => {
                progress(attempt);
                std::thread::sleep(wait.interval);
            }
            Err(error) => {
                return Err(anyhow::Error::new(error)
                    .context(format!("HOME is still in use after {} attempts; stop its daemon", wait.attempts)));
            }
        }
    }
    anyhow::bail!("no attempt to take the HOME lock was allowed")
}

#[derive(Debug, Serialize)]
pub(crate) struct Cutover {
    pub marker: Marker,
    pub resumed: bool,
    pub already_committed: bool,
    pub imported: Option<crate::domain_migration::ImportReport>,
    pub rollback: String,
}

fn rollback_command(backup: &Path) -> String {
    format!("alondrad --rollback-domain-v2 {}", backup.display())
}

fn sqlite_trio(name: &str) -> [String; 4] {
    [name.to_owned(), format!("{name}-wal"), format!("{name}-shm"), format!("{name}-journal")]
}

/// Moves the legacy stores and their journals into HOME's archive. Renames
/// only: the archived files are the original bytes.
fn archive_legacy_stores(home: &Path) -> Result<()> {
    let archive = home.join(ARCHIVE);
    match fs::DirBuilder::new().mode(0o700).create(&archive) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let meta = fs::symlink_metadata(&archive)?;
            ensure!(meta.is_dir() && meta.uid() == unsafe { libc::geteuid() }, "unsafe legacy archive");
        }
        Err(error) => return Err(error.into()),
    }
    for store in LEGACY_STORES {
        validate_owned_sqlite_files(&home.join(store))?;
        for name in sqlite_trio(store) {
            if fs::symlink_metadata(home.join(&name)).is_ok() {
                rename_exclusive(&home.join(&name), &archive.join(&name))?;
            }
        }
    }
    File::open(&archive)?.sync_all()?;
    File::open(home)?.sync_all()?;
    Ok(())
}

fn promote(home: &Path, stage: &Path) -> Result<alondra_core::domain_v2::DomainState> {
    let staged = staged_state(stage)?;
    verify_no_stage_effects(&stage.join("domain-v2.db"))?;
    let database = home.join("domain-v2.db");
    let installed = if database.exists() && !crate::replaceable_cutover_sidecar_is_empty(&database)? {
        crate::domain_runtime::DomainRuntime::open(home)?.state()?
    } else {
        remove_owned_sqlite_files(&database)?;
        let next = home.join("domain-v2.db.next");
        remove_owned_sqlite_files(&next)?;
        crate::domain_migration::sqlite_backup(&stage.join("domain-v2.db"), &next)?;
        rename_exclusive(&next, &database)?;
        File::open(home)?.sync_all()?;
        crate::domain_runtime::DomainRuntime::open(home)?.state()?
    };
    ensure!(installed == staged, "HOME holds another domain store than the stage");
    clean_content_next(home)?;
    if stage.join("files").exists() {
        let staging = home.join(CONTENT_NEXT);
        fs::DirBuilder::new().mode(0o700).create(&staging)?;
        promote_content(&stage.join("files"), &home.join("files"), &staging)?;
        clean_content_next(home)?;
    }
    for revision in installed.document_revisions.values() {
        crate::domain_migration::verify_content(home, &revision.content_reference, &revision.content_digest)?;
    }
    for interaction in installed.interactions.values() {
        for attachment in &interaction.attachments {
            crate::domain_migration::verify_content(home, &attachment.content_reference, &attachment.content_digest)?;
        }
    }
    Ok(installed)
}

/// The whole cutover of a stopped HOME in one command: wait for the daemon to
/// release HOME, copy it coherently into `backup`, import the work in flight,
/// archive the legacy stores inside HOME, install the v2 store and commit the
/// marker. Re-running after an interruption resumes; after a commit it only
/// reports. `recorded_home` is where the legacy events were written (HOME
/// itself, except when rehearsing on a copy).
pub(crate) fn cutover_offline(
    home: &Path,
    backup: &Path,
    recorded_home: &Path,
    legacy_binaries: &[&Path],
    wait: &LockWait,
    progress: impl FnMut(u32),
) -> Result<Cutover> {
    ensure!(home.is_absolute() && backup.is_absolute(), "cutover paths must be absolute");
    let home = home.canonicalize()?;
    let backup = &canonical_parent(backup)?;
    let _lock = exclusive_lock(&home, wait, progress)?;
    let stage = stage_of(backup)?;
    let rollback = rollback_command(backup);
    match read_marker(&home)? {
        Some(marker) if marker.phase == Phase::Committed => {
            crate::domain_migration::verify_backup_expected(backup, &marker.backup_manifest_sha256)
                .context("HOME was cut over with another backup")?;
            return Ok(Cutover { marker, resumed: false, already_committed: true, imported: None, rollback });
        }
        Some(marker) => {
            crate::domain_migration::verify_backup_expected(backup, &marker.backup_manifest_sha256)
                .context("an interrupted cutover belongs to another backup; roll it back first")?;            ensure!(
                staged_state(&stage).is_ok_and(|state| state.data_epoch.0 == marker.data_epoch),
                "the interrupted cutover's stage is missing; roll it back with {rollback}"
            );
            archive_legacy_stores(&home)?;
            promote(&home, &stage)?;
            let mut committed = marker.clone();
            committed.phase = Phase::Committed;
            write_marker(&home, &committed, false)?;
            fs::remove_dir_all(&stage)?;
            return Ok(Cutover { marker: committed, resumed: true, already_committed: false, imported: None, rollback });
        }
        None => (),
    }
    ensure!(home.join("alondra.db").exists(), "HOME holds no legacy store to cut over");
    validate_owned_sqlite_files(&home.join("domain-v2.db"))?;
    ensure!(
        !home.join("domain-v2.db").exists() || crate::replaceable_cutover_sidecar_is_empty(&home.join("domain-v2.db"))?,
        "HOME already has domain data"
    );
    ensure!(!home.join(ARCHIVE).exists(), "HOME already has a legacy archive");
    ensure!(!stage.exists(), "a stage from an earlier attempt is in the way: {}", stage.display());
    let manifest = crate::domain_migration::backup_home(&home, recorded_home, backup, legacy_binaries)?;
    let digest = crate::domain_migration::backup_manifest_digest(backup)?;
    let (staged, imported) = crate::domain_migration::import_backup(backup, recorded_home, &stage)?;
    ensure!(staged.revision > 0 && imported.source_seq == manifest.source_seq, "stage does not match the backup");
    let marker = Marker {
        format: 1,
        phase: Phase::Prepared,
        backup_manifest_sha256: digest,
        source_seq: manifest.source_seq,
        data_epoch: staged.data_epoch.0.clone(),
    };
    write_marker(&home, &marker, true)?;
    archive_legacy_stores(&home)?;
    promote(&home, &stage)?;
    let mut committed = marker;
    committed.phase = Phase::Committed;
    write_marker(&home, &committed, false)?;
    fs::remove_dir_all(&stage)?;
    Ok(Cutover { marker: committed, resumed: false, already_committed: false, imported: Some(imported), rollback })
}

fn absolute(path: &Path) -> Result<std::path::PathBuf> {
    Ok(if path.is_absolute() { path.to_owned() } else { std::env::current_dir()?.join(path) })
}

fn waiting(attempt: u32) {
    if attempt == 1 || attempt % 10 == 0 {
        eprintln!("waiting for the daemon to release HOME (attempt {attempt} of {}); stop it to continue",
            LockWait::LIVE.attempts);
    }
}

/// The exact steps that undo this cutover: restore the copy, then put the
/// retained binaries back where they were installed and restart the service.
fn rollback_steps(backup: &Path, binaries: &[&Path], label: &str) -> String {
    let mut steps = format!(
        "1. Stop the daemon: launchctl kill SIGTERM gui/$(id -u)/{label}\n\
         2. Restore the legacy HOME: alondrad --rollback-domain-v2 {}\n\
         3. Put the previous binaries back and keep Homebrew from upgrading them:\n",
        backup.display()
    );
    for binary in binaries {
        let name = binary.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        steps.push_str(&format!(
            "   install -m 755 {} \"$(realpath \"$(command -v {name})\")\"\n",
            backup.join(crate::domain_migration::LEGACY_BINARIES).join(&name).display()
        ));
    }
    steps.push_str(&format!("   brew pin alondra\n4. Restart: launchctl kickstart -k gui/$(id -u)/{label}"));
    steps
}

pub(crate) fn cutover_cli(backup: &Path, binaries: &[&Path]) -> Result<()> {
    let config = alondra_core::config::Config::load().map_err(|error| anyhow::anyhow!("config.toml unreadable: {error}"))?;
    let backup = absolute(backup)?;
    let recorded = std::env::var_os(crate::domain_migration::RECORDED_HOME)
        .map_or_else(|| config.home.clone(), std::path::PathBuf::from);
    eprintln!("Cutting {} over to domain v2; backup in {}", config.home.display(), backup.display());
    let result = cutover_offline(&config.home, &backup, &recorded, binaries, &LockWait::LIVE, waiting)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    eprintln!("Domain v2 committed (legacy seq {}). To roll back:\n{}",
        result.marker.source_seq, rollback_steps(&backup, binaries, &config.service_label));
    Ok(())
}

pub(crate) fn rollback_cli(backup: &Path) -> Result<()> {
    let config = alondra_core::config::Config::load().map_err(|error| anyhow::anyhow!("config.toml unreadable: {error}"))?;
    let aside = rollback_offline(&config.home, &absolute(backup)?, &LockWait::LIVE, waiting)?;
    eprintln!("Legacy HOME restored. What domain v2 wrote is kept in {}; start the legacy daemon.", aside.display());
    Ok(())
}

/// Puts back exactly what `backup` copied, sets aside what v2 wrote in
/// `domain-v2-rolled-back-<time>` and removes the marker last. A committed
/// HOME whose daemon ran may have sent messages or merged PRs: those effects
/// are not undone.
pub(crate) fn rollback_offline(home: &Path, backup: &Path, wait: &LockWait, progress: impl FnMut(u32)) -> Result<std::path::PathBuf> {
    ensure!(home.is_absolute() && backup.is_absolute(), "rollback paths must be absolute");
    let home = home.canonicalize()?;
    let backup = &backup.canonicalize()?;
    let _lock = exclusive_lock(&home, wait, progress)?;
    let marker = read_marker(&home)?.context("HOME has no domain cutover to roll back")?;
    let manifest = crate::domain_migration::verify_backup_expected(backup, &marker.backup_manifest_sha256)
        .context("backup does not belong to this cutover")?;
    ensure!(manifest.source_seq == marker.source_seq, "backup does not match the cutover marker");
    let aside = home.join(format!("domain-v2-rolled-back-{}", alondra_core::now_millis()));
    fs::DirBuilder::new().mode(0o700).create(&aside)?;
    let set_aside = |name: &str| -> Result<()> {
        let from = home.join(name);
        if fs::symlink_metadata(&from).is_ok() {
            let to = aside.join(name);
            if let Some(parent) = to.parent() {
                fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
            }
            rename_exclusive(&from, &to)?;
        }
        Ok(())
    };
    for name in sqlite_trio("domain-v2.db").iter().chain(&sqlite_trio("domain-v2.db.next")) {
        set_aside(name)?;
    }
    set_aside(ARCHIVE)?;
    for file in manifest.files.iter().filter(|file| !file.path.starts_with(crate::domain_migration::LEGACY_BINARIES)) {
        let names = if crate::domain_migration::BACKUP_DATABASES.contains(&file.path.as_str()) {
            sqlite_trio(&file.path).to_vec()
        } else {
            vec![file.path.clone()]
        };
        for name in &names {
            set_aside(name)?;
        }
        let target = home.join(&file.path);
        if let Some(parent) = target.parent() {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
        }
        crate::domain_migration::copy_regular(&backup.join(&file.path), &target)?;
    }
    clean_content_next(&home)?;
    File::open(&home)?.sync_all()?;
    set_aside(MARKER)?;
    File::open(&home)?.sync_all()?;
    Ok(aside)
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

    #[test]
    fn promote_reads_the_stage_without_writing_it() {
        let stage = std::env::temp_dir().join(format!(
            "alondra-domain-cutover-stage-{}",
            crate::auth::random_hex(8)
        ));
        fs::DirBuilder::new().mode(0o700).create(&stage).unwrap();
        let database = stage.join("domain-v2.db");
        let epoch = crate::domain_store::DomainStore::open(&database)
            .unwrap()
            .load()
            .unwrap()
            .data_epoch;
        let imported = fs::read(&database).unwrap();
        assert_eq!(staged_state(&stage).unwrap().data_epoch, epoch);
        assert_eq!(
            fs::read(&database).unwrap(),
            imported,
            "the stage hashed for provenance must hold only the importer's writes"
        );
        fs::remove_dir_all(stage).unwrap();
    }

    /// Holds the shared HOME lock like a daemon that keeps restarting, until told to stop.
    #[test]
    fn lock_holder_child() {
        let (Some(home), Some(signals)) = (std::env::var_os("ALONDRA_TEST_HOLD_HOME"), std::env::var_os("ALONDRA_TEST_HOLD_SIGNALS")) else {
            return;
        };
        let signals = std::path::PathBuf::from(signals);
        let lock = HomeLock::shared(Path::new(&home)).unwrap();
        fs::write(signals.join("held"), b"").unwrap();
        while !signals.join("release").exists() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        drop(lock);
    }

    fn live_shaped(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("alondra-cutover-{name}-{}", crate::auth::random_hex(8)))
            .to_path_buf();
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let home = root.join("home");
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        crate::domain_migration::synthetic::write_live_shaped_home(&home);
        (root, home)
    }

    #[test]
    fn the_cutover_waits_for_a_restarting_daemon_to_release_home() {
        let (root, home) = live_shaped("lock");
        let signals = root.join("signals");
        fs::create_dir(&signals).unwrap();
        let mut holder = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "domain_cutover::tests::lock_holder_child", "--nocapture"])
            .env("ALONDRA_TEST_HOLD_HOME", &home)
            .env("ALONDRA_TEST_HOLD_SIGNALS", &signals)
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        while !signals.join("held").exists() {
            assert!(started.elapsed() < std::time::Duration::from_secs(60), "holder never took the lock");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let quick = LockWait { attempts: 3, interval: std::time::Duration::from_millis(1) };
        let mut reported = vec![];
        let error = cutover_offline(&home, &root.join("refused"), &home, &[], &quick, |attempt| reported.push(attempt))
            .unwrap_err();
        assert!(format!("{error:#}").contains("still in use after 3 attempts"), "{error:#}");
        assert_eq!(reported, vec![1, 2], "every failed attempt is reported");
        assert!(!root.join("refused").exists() && home.join("alondra.db").exists() && read_marker(&home).unwrap().is_none());

        let patient = LockWait { attempts: 60_000, interval: std::time::Duration::from_millis(1) };
        let mut waited = 0;
        let cutover = cutover_offline(&home, &root.join("backup"), &home, &[], &patient, |_| {
            waited += 1;
            if waited == 1 {
                fs::write(signals.join("release"), b"").unwrap();
            }
        })
        .unwrap();
        assert!(waited >= 1, "the cutover had to wait for the holder");
        assert!(holder.wait().unwrap().success());
        assert_eq!(cutover.marker.phase, Phase::Committed);
        refuse_legacy_home(&home).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_interrupted_cutover_resumes_from_its_backup_and_stage() {
        let (root, home) = live_shaped("resume");
        let backup = root.join("backup");
        let stage = stage_of(&backup).unwrap();
        let manifest = crate::domain_migration::backup_home(&home, &home, &backup, &[]).unwrap();
        let (staged, _) = crate::domain_migration::import_backup(&backup, &home, &stage).unwrap();
        let prepared = Marker {
            format: 1,
            phase: Phase::Prepared,
            backup_manifest_sha256: crate::domain_migration::backup_manifest_digest(&backup).unwrap(),
            source_seq: manifest.source_seq,
            data_epoch: staged.data_epoch.0.clone(),
        };
        write_marker(&home, &prepared, true).unwrap();
        archive_legacy_stores(&home).unwrap();
        let error = refuse_legacy_home(&home);
        assert!(error.is_ok(), "a Prepared HOME is fenced by protocol mode, not treated as legacy");
        let wait = LockWait { attempts: 1, interval: std::time::Duration::ZERO };
        let resumed = cutover_offline(&home, &backup, &home, &[], &wait, |_| ()).unwrap();
        assert!(resumed.resumed);
        assert_eq!(resumed.marker.phase, Phase::Committed);
        assert_eq!(crate::domain_runtime::DomainRuntime::open(&home).unwrap().state().unwrap(), staged);
        assert!(!stage.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
