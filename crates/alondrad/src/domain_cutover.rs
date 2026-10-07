//! The one-command cutover of a stopped HOME to domain v2, and its rollback.
//! The marker fences daemon startup before any legacy store moves; it is
//! committed only after the empty domain database exists and the legacy stores
//! are archived. Nothing from the legacy data is imported.
use alondra_core::home_lock::HomeLock;
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
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

fn canonical_parent(path: &Path) -> Result<std::path::PathBuf> {
    let parent = path
        .parent()
        .context("path has no parent")?
        .canonicalize()?;
    Ok(parent.join(path.file_name().context("path has no name")?))
}

/// Where the legacy stores stay after the cutover, renamed inside HOME.
pub(crate) const ARCHIVE: &str = "legacy-v1";
pub(crate) const LEGACY_STORES: [&str; 2] = ["alondra.db", "schedules.db"];

/// A daemon built for domain v2 must not run a HOME that still holds legacy
/// data. It refuses before taking the HOME lock, so the cutover can take it.
pub(crate) fn refuse_legacy_home(home: &Path) -> Result<()> {
    for store in LEGACY_STORES {
        let path = home.join(store);
        match legacy_store(&path) {
            LegacyStore::NoData => (),
            LegacyStore::Empty => log!("{} holds no tables, so it is not legacy data; startup ignores it", path.display()),
            LegacyStore::Data => bail!(
                "this HOME still holds legacy data in {}; it was left untouched. Restore or migrate it explicitly before starting the domain daemon",
                path.display()
            ),
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq)]
enum LegacyStore {
    NoData,
    Empty,
    Data,
}

/// A legacy store with any recorded event or snapshot. A file with no tables
/// and no journal beside it is what opening a missing path with SQLite leaves
/// behind. Anything else that cannot be read counts as data, so an unknown
/// store is never started over.
fn legacy_store(path: &Path) -> LegacyStore {
    let bytes = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return LegacyStore::NoData,
        Ok(meta) if meta.is_file() => meta.len(),
        _ => return LegacyStore::Data,
    };
    let journaled = ["-wal", "-journal"].iter().any(|suffix| {
        let mut journal = path.as_os_str().to_owned();
        journal.push(suffix);
        fs::symlink_metadata(Path::new(&journal)).is_ok_and(|meta| meta.len() > 0)
    });
    if journaled {
        return LegacyStore::Data;
    }
    if bytes == 0 {
        return LegacyStore::Empty;
    }
    let read = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .and_then(|store| {
        let tables: i64 = store.query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))?;
        if tables == 0 {
            return Ok(LegacyStore::Empty);
        }
        let data: bool = store.query_row(
            "SELECT EXISTS(SELECT 1 FROM event_log) OR EXISTS(SELECT 1 FROM snapshots)",
            [],
            |row| row.get(0),
        )?;
        Ok(if data { LegacyStore::Data } else { LegacyStore::NoData })
    });
    read.unwrap_or(LegacyStore::Data)
}

/// How long the cutover waits for a running daemon to release HOME.
pub(crate) struct LockWait {
    pub attempts: u32,
    pub interval: std::time::Duration,
}

impl LockWait {
    pub(crate) const LIVE: LockWait = LockWait {
        attempts: 600,
        interval: std::time::Duration::from_secs(1),
    };
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
                return Err(anyhow::Error::new(error).context(format!(
                    "HOME is still in use after {} attempts; stop its daemon",
                    wait.attempts
                )));
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
    pub rollback: String,
}

fn rollback_command(backup: &Path) -> String {
    format!("alondrad --rollback-domain-v2 {}", backup.display())
}

fn sqlite_trio(name: &str) -> [String; 4] {
    [
        name.to_owned(),
        format!("{name}-wal"),
        format!("{name}-shm"),
        format!("{name}-journal"),
    ]
}

/// Moves the legacy stores and their journals into HOME's archive. Renames
/// only: the archived files are the original bytes.
fn archive_legacy_stores(home: &Path) -> Result<()> {
    let archive = home.join(ARCHIVE);
    match fs::DirBuilder::new().mode(0o700).create(&archive) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let meta = fs::symlink_metadata(&archive)?;
            ensure!(
                meta.is_dir() && meta.uid() == unsafe { libc::geteuid() },
                "unsafe legacy archive"
            );
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

/// The whole cutover of a stopped HOME in one command: wait for the daemon to
/// release HOME, copy it coherently into `backup`, archive the legacy stores
/// inside HOME, start an empty v2 store and commit the marker. Nothing legacy
/// is imported. Re-running after an interruption resumes; after a commit it
/// only reports.
pub(crate) fn cutover_offline(
    home: &Path,
    backup: &Path,
    legacy_binaries: &[&Path],
    wait: &LockWait,
    progress: impl FnMut(u32),
) -> Result<Cutover> {
    ensure!(
        home.is_absolute() && backup.is_absolute(),
        "cutover paths must be absolute"
    );
    let home = home.canonicalize()?;
    let backup = &canonical_parent(backup)?;
    let _lock = exclusive_lock(&home, wait, progress)?;
    let rollback = rollback_command(backup);
    match read_marker(&home)? {
        Some(marker) if marker.phase == Phase::Committed => {
            crate::domain_backup::verify_backup_expected(backup, &marker.backup_manifest_sha256)
                .context("HOME was cut over with another backup")?;
            return Ok(Cutover {
                marker,
                resumed: false,
                already_committed: true,
                rollback,
            });
        }
        Some(marker) => {
            crate::domain_backup::verify_backup_expected(backup, &marker.backup_manifest_sha256)
                .context("an interrupted cutover belongs to another backup; roll it back first")?;
            ensure!(
                crate::domain_runtime::DomainRuntime::open_at(&home, &home.join("domain-v2.db"))
                    .and_then(|runtime| runtime.state())
                    .is_ok_and(|state| state.data_epoch.0 == marker.data_epoch),
                "the interrupted cutover's domain store is missing; roll it back with {rollback}"
            );
            archive_legacy_stores(&home)?;
            let mut committed = marker.clone();
            committed.phase = Phase::Committed;
            write_marker(&home, &committed, false)?;
            return Ok(Cutover {
                marker: committed,
                resumed: true,
                already_committed: false,
                rollback,
            });
        }
        None => (),
    }
    ensure!(
        home.join("alondra.db").exists(),
        "HOME holds no legacy store to cut over"
    );
    let database = home.join("domain-v2.db");
    validate_owned_sqlite_files(&database)?;
    ensure!(
        !database.exists() || crate::replaceable_cutover_sidecar_is_empty(&database)?,
        "HOME already has domain data"
    );
    ensure!(
        !home.join(ARCHIVE).exists(),
        "HOME already has a legacy archive"
    );
    let manifest = crate::domain_backup::backup_home(&home, backup, legacy_binaries)?;
    let digest = crate::domain_backup::backup_manifest_digest(backup)?;
    remove_owned_sqlite_files(&database)?;
    let data_epoch = crate::domain_runtime::DomainRuntime::open_at(&home, &database)?
        .state()?
        .data_epoch
        .0
        .clone();
    let marker = Marker {
        format: 1,
        phase: Phase::Prepared,
        backup_manifest_sha256: digest,
        source_seq: manifest.source_seq,
        data_epoch,
    };
    write_marker(&home, &marker, true)?;
    archive_legacy_stores(&home)?;
    let mut committed = marker;
    committed.phase = Phase::Committed;
    write_marker(&home, &committed, false)?;
    Ok(Cutover {
        marker: committed,
        resumed: false,
        already_committed: false,
        rollback,
    })
}

fn absolute(path: &Path) -> Result<std::path::PathBuf> {
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    })
}

fn waiting(attempt: u32) {
    if attempt == 1 || attempt % 10 == 0 {
        eprintln!(
            "waiting for the daemon to release HOME (attempt {attempt} of {}); stop it to continue",
            LockWait::LIVE.attempts
        );
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
        let name = binary
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        steps.push_str(&format!(
            "   install -m 755 {} \"$(realpath \"$(command -v {name})\")\"\n",
            backup
                .join(crate::domain_backup::LEGACY_BINARIES)
                .join(&name)
                .display()
        ));
    }
    steps.push_str(&format!(
        "   brew pin alondra\n4. Restart: launchctl kickstart -k gui/$(id -u)/{label}"
    ));
    steps
}

pub(crate) fn cutover_cli(backup: &Path, binaries: &[&Path]) -> Result<()> {
    let config = alondra_core::config::Config::load()
        .map_err(|error| anyhow::anyhow!("config.toml unreadable: {error}"))?;
    let backup = absolute(backup)?;
    eprintln!(
        "Cutting {} over to domain v2; backup in {}",
        config.home.display(),
        backup.display()
    );
    let result = cutover_offline(&config.home, &backup, binaries, &LockWait::LIVE, waiting)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    eprintln!(
        "Domain v2 started empty; the legacy data is archived in {} (legacy seq {}). To roll back:\n{}",
        config.home.join(ARCHIVE).display(),
        result.marker.source_seq,
        rollback_steps(&backup, binaries, &config.service_label)
    );
    Ok(())
}

pub(crate) fn rollback_cli(backup: &Path) -> Result<()> {
    let config = alondra_core::config::Config::load()
        .map_err(|error| anyhow::anyhow!("config.toml unreadable: {error}"))?;
    let aside = rollback_offline(&config.home, &absolute(backup)?, &LockWait::LIVE, waiting)?;
    eprintln!(
        "Legacy HOME restored. What domain v2 wrote is kept in {}; start the legacy daemon.",
        aside.display()
    );
    Ok(())
}

/// Puts back exactly what `backup` copied, sets aside what v2 wrote in
/// `domain-v2-rolled-back-<time>` and removes the marker last. A committed
/// HOME whose daemon ran may have sent messages or merged PRs: those effects
/// are not undone.
pub(crate) fn rollback_offline(
    home: &Path,
    backup: &Path,
    wait: &LockWait,
    progress: impl FnMut(u32),
) -> Result<std::path::PathBuf> {
    ensure!(
        home.is_absolute() && backup.is_absolute(),
        "rollback paths must be absolute"
    );
    let home = home.canonicalize()?;
    let backup = &backup.canonicalize()?;
    let _lock = exclusive_lock(&home, wait, progress)?;
    let marker = read_marker(&home)?.context("HOME has no domain cutover to roll back")?;
    let manifest =
        crate::domain_backup::verify_backup_expected(backup, &marker.backup_manifest_sha256)
            .context("backup does not belong to this cutover")?;
    ensure!(
        manifest.source_seq == marker.source_seq,
        "backup does not match the cutover marker"
    );
    let aside = home.join(format!(
        "domain-v2-rolled-back-{}",
        alondra_core::now_millis()
    ));
    fs::DirBuilder::new().mode(0o700).create(&aside)?;
    let set_aside = |name: &str| -> Result<()> {
        let from = home.join(name);
        if fs::symlink_metadata(&from).is_ok() {
            let to = aside.join(name);
            if let Some(parent) = to.parent() {
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(parent)?;
            }
            rename_exclusive(&from, &to)?;
        }
        Ok(())
    };
    for name in sqlite_trio("domain-v2.db")
        .iter()
        .chain(&sqlite_trio("domain-v2.db.next"))
    {
        set_aside(name)?;
    }
    set_aside(ARCHIVE)?;
    for file in manifest
        .files
        .iter()
        .filter(|file| !file.path.starts_with(crate::domain_backup::LEGACY_BINARIES))
    {
        let names = if crate::domain_backup::BACKUP_DATABASES.contains(&file.path.as_str()) {
            sqlite_trio(&file.path).to_vec()
        } else {
            vec![file.path.clone()]
        };
        for name in &names {
            set_aside(name)?;
        }
        let target = home.join(&file.path);
        if let Some(parent) = target.parent() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        crate::domain_backup::copy_regular(&backup.join(&file.path), &target)?;
    }
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

    /// Holds the shared HOME lock like a daemon that keeps restarting, until told to stop.
    #[test]
    fn lock_holder_child() {
        let (Some(home), Some(signals)) = (
            std::env::var_os("ALONDRA_TEST_HOLD_HOME"),
            std::env::var_os("ALONDRA_TEST_HOLD_SIGNALS"),
        ) else {
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

    fn legacy_home(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "alondra-cutover-{name}-{}",
            crate::auth::random_hex(8)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let home = root.join("home");
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let events = Connection::open(home.join("alondra.db")).unwrap();
        events
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE event_log(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,session TEXT,body TEXT NOT NULL);
                 CREATE TABLE snapshots(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,body TEXT NOT NULL);
                 INSERT INTO event_log(seq,at,kind,body) VALUES(1,1,'note','{}'),(2,2,'note','{}');",
            )
            .unwrap();
        let schedules = Connection::open(home.join("schedules.db")).unwrap();
        schedules
            .execute_batch(
                "CREATE TABLE schedules(id TEXT); INSERT INTO schedules VALUES('nightly');",
            )
            .unwrap();
        fs::write(home.join("config.toml"), "name = \"Test\"\n").unwrap();
        fs::write(home.join("devices.json"), "[]").unwrap();
        (root, home)
    }

    fn quick() -> LockWait {
        LockWait {
            attempts: 1,
            interval: std::time::Duration::ZERO,
        }
    }

    fn legacy_digests(home: &Path) -> Vec<String> {
        ["alondra.db", "schedules.db"]
            .iter()
            .map(|name| {
                crate::domain_backup::rehearsal::sqlite_content_digest(&home.join(name)).unwrap()
            })
            .collect()
    }

    #[test]
    fn the_cutover_waits_for_a_restarting_daemon_to_release_home() {
        let (root, home) = legacy_home("lock");
        let signals = root.join("signals");
        fs::create_dir(&signals).unwrap();
        let mut holder = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "domain_cutover::tests::lock_holder_child",
                "--nocapture",
            ])
            .env("ALONDRA_TEST_HOLD_HOME", &home)
            .env("ALONDRA_TEST_HOLD_SIGNALS", &signals)
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        while !signals.join("held").exists() {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(60),
                "holder never took the lock"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let three = LockWait {
            attempts: 3,
            interval: std::time::Duration::from_millis(1),
        };
        let mut reported = vec![];
        let error = cutover_offline(&home, &root.join("refused"), &[], &three, |attempt| {
            reported.push(attempt)
        })
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("still in use after 3 attempts"),
            "{error:#}"
        );
        assert_eq!(reported, vec![1, 2], "every failed attempt is reported");
        assert!(
            !root.join("refused").exists()
                && home.join("alondra.db").exists()
                && read_marker(&home).unwrap().is_none()
        );

        let patient = LockWait {
            attempts: 60_000,
            interval: std::time::Duration::from_millis(1),
        };
        let mut waited = 0;
        let cutover = cutover_offline(&home, &root.join("backup"), &[], &patient, |_| {
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
    fn the_cutover_starts_the_domain_store_empty_and_archives_the_legacy_data_without_importing_it() {
        let (root, home) = legacy_home("empty");
        let before = legacy_digests(&home);
        assert!(refuse_legacy_home(&home).is_err());
        let backup = root.join("backup");
        let cutover = cutover_offline(&home, &backup, &[], &quick(), |_| ()).unwrap();
        assert_eq!(cutover.marker.phase, Phase::Committed);
        assert_eq!(cutover.marker.source_seq, 2);
        assert_eq!(read_marker(&home).unwrap().unwrap(), cutover.marker);

        let state =
            crate::domain_runtime::DomainRuntime::open_at(&home, &home.join("domain-v2.db"))
                .unwrap()
                .state()
                .unwrap();
        assert_eq!(state.data_epoch.0, cutover.marker.data_epoch);
        assert_eq!(state.revision, 0);
        assert!(
            state.requests.is_empty()
                && state.tasks.is_empty()
                && state.agents.is_empty()
                && state.interactions.is_empty()
                && state.document_revisions.is_empty()
                && state.schedule.schedules.is_empty(),
            "nothing from the legacy stores reaches domain v2"
        );

        let archive = home.join(ARCHIVE);
        for name in ["alondra.db", "schedules.db"] {
            assert!(!home.join(name).exists(), "{name} left in HOME");
            assert!(archive.join(name).exists(), "{name} not archived");
        }
        let archived = ["alondra.db", "schedules.db"].map(|name| {
            crate::domain_backup::rehearsal::sqlite_content_digest(&archive.join(name)).unwrap()
        });
        assert_eq!(
            archived.to_vec(),
            before,
            "the archive holds the original rows"
        );
        assert!(home.join("config.toml").exists() && home.join("devices.json").exists());
        refuse_legacy_home(&home).unwrap();

        let again = cutover_offline(&home, &backup, &[], &quick(), |_| ()).unwrap();
        assert!(again.already_committed && !again.resumed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_interrupted_cutover_resumes_from_its_backup_and_empty_store() {
        let (root, home) = legacy_home("resume");
        let backup = root.join("backup");
        let manifest = crate::domain_backup::backup_home(&home, &backup, &[]).unwrap();
        let epoch =
            crate::domain_runtime::DomainRuntime::open_at(&home, &home.join("domain-v2.db"))
                .unwrap()
                .state()
                .unwrap()
                .data_epoch
                .0
                .clone();
        let prepared = Marker {
            format: 1,
            phase: Phase::Prepared,
            backup_manifest_sha256: crate::domain_backup::backup_manifest_digest(&backup).unwrap(),
            source_seq: manifest.source_seq,
            data_epoch: epoch.clone(),
        };
        write_marker(&home, &prepared, true).unwrap();
        assert!(
            refuse_legacy_home(&home).is_err(),
            "prepared cutover with legacy files still present fails closed"
        );
        let resumed = cutover_offline(&home, &backup, &[], &quick(), |_| ()).unwrap();
        assert!(resumed.resumed);
        assert_eq!(resumed.marker.phase, Phase::Committed);
        assert_eq!(resumed.marker.data_epoch, epoch);
        assert!(
            !home.join("alondra.db").exists() && home.join(ARCHIVE).join("alondra.db").exists()
        );
        assert_eq!(
            crate::domain_runtime::DomainRuntime::open_at(&home, &home.join("domain-v2.db"))
                .unwrap()
                .state()
                .unwrap()
                .revision,
            0
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rollback_restores_the_legacy_home_and_sets_the_empty_domain_store_aside() {
        let (root, home) = legacy_home("rollback");
        let before = legacy_digests(&home);
        let backup = root.join("backup");
        cutover_offline(&home, &backup, &[], &quick(), |_| ()).unwrap();
        let (other_root, other_home) = legacy_home("other");
        let stranger = other_root.join("backup");
        crate::domain_backup::backup_home(&other_home, &stranger, &[]).unwrap();
        assert!(
            rollback_offline(&home, &stranger, &quick(), |_| ()).is_err(),
            "another backup must not restore this HOME"
        );
        assert!(read_marker(&home).unwrap().is_some());

        let aside = rollback_offline(&home, &backup, &quick(), |_| ()).unwrap();
        assert!(read_marker(&home).unwrap().is_none());
        assert_eq!(legacy_digests(&home), before);
        assert!(
            aside.join("domain-v2.db").exists() && aside.join(ARCHIVE).join("alondra.db").exists()
        );
        assert!(!home.join(ARCHIVE).exists() && !home.join("domain-v2.db").exists());
        assert!(refuse_legacy_home(&home).is_err());
        fs::remove_dir_all(other_root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_empty_or_tableless_legacy_store_does_not_block_startup() {
        let root = std::env::temp_dir().join(format!("alondra-empty-legacy-{}", crate::auth::random_hex(8)));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let root = root.canonicalize().unwrap();
        fs::write(root.join("alondra.db"), b"").unwrap();
        refuse_legacy_home(&root).unwrap();
        fs::remove_file(root.join("alondra.db")).unwrap();
        Connection::open(root.join("schedules.db")).unwrap().execute_batch("PRAGMA user_version = 1").unwrap();
        assert!(fs::metadata(root.join("schedules.db")).unwrap().len() > 0);
        refuse_legacy_home(&root).unwrap();
        assert!(root.join("schedules.db").exists(), "an empty store is left where it is");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_empty_store_with_a_journal_and_unreadable_bytes_still_block_startup_naming_the_file() {
        let root = std::env::temp_dir().join(format!("alondra-journal-legacy-{}", crate::auth::random_hex(8)));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::write(root.join("alondra.db"), b"").unwrap();
        fs::write(root.join("alondra.db-wal"), b"uncheckpointed frames").unwrap();
        let error = refuse_legacy_home(&root).unwrap_err().to_string();
        assert!(error.contains(&root.join("alondra.db").display().to_string()), "{error}");
        fs::remove_file(root.join("alondra.db-wal")).unwrap();
        fs::write(root.join("alondra.db"), b"not sqlite").unwrap();
        assert!(refuse_legacy_home(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
