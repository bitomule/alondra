//! `alondrad --preflight`: the startup this binary would do, run against a private snapshot of
//! HOME so a running daemon, its HOME lock and its data are never touched. An SDK backend
//! configuration in HOME is checked as this binary would admit it.
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use alondra_core::config::Config;
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

const DATABASES: [&str; 5] = [
    "alondra.db",
    "domain-v2.db",
    "domain.db",
    "domain.db.next",
    "schedules.db",
];

pub fn run(config: &Config) -> Result<String> {
    run_for(config, &crate::options::user_home())
}

/// `run` for a user whose HOME is `user_home`, where a production SDK backend runs Claude.
fn run_for(config: &Config, user_home: &Path) -> Result<String> {
    crate::domain_cutover::refuse_legacy_home(&config.home)?;
    alondra_core::home_lock::ensure_ready(&config.home)?;
    let backend = crate::sdk_owner::backend::load(&config.home, config.sandbox, user_home)
        .context("the SDK backend configuration is not one this build admits")?;
    let scratch = Scratch::new()?;
    snapshot(&config.home, &scratch.0)?;
    let mut copy = config.clone();
    copy.home = scratch.0.clone();
    let started = start(&copy)?;
    Ok(match backend {
        Some(backend) => format!("{started}; SDK backend configuration {} admitted", backend.configuration_digest()),
        None => started,
    })
}

/// The same canonical bootstrap `run` performs, on a HOME it may freely write.
fn start(config: &Config) -> Result<String> {
    crate::home_renames::apply(&config.home)?;
    let (_, _source_fence) = crate::domain_database_migration::migrate_and_fence(&config.home)?;
    crate::retire_empty_older_sidecar(&config.home)?;
    crate::ensure_domain_protocol(&config.home)?;
    let _domain = crate::domain_runtime::DomainRuntime::open(&config.home)?;
    crate::domain_bootstrap::DomainDaemon::open(&config.home)?;
    Ok("canonical domain state starts".into())
}

/// A consistent copy of each database, read through SQLite so a live writer's WAL is included.
fn snapshot(home: &Path, into: &Path) -> Result<()> {
    let home = &home
        .canonicalize()
        .with_context(|| format!("reading {}", home.display()))?;
    crate::domain_database_migration::validate_preflight_source(home)?;
    for name in DATABASES {
        let source = home.join(name);
        match std::fs::symlink_metadata(&source) {
            Ok(meta) => anyhow::ensure!(meta.is_file(), "{name} must be a regular file"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        }
        let from = open_source(&source).with_context(|| format!("reading {name}"))?;
        let mut to = Connection::open(into.join(name))?;
        std::fs::set_permissions(into.join(name), std::fs::Permissions::from_mode(0o600))?;
        rusqlite::backup::Backup::new(&from, &mut to)?
            .run_to_completion(4096, std::time::Duration::ZERO, None)
            .with_context(|| format!("copying {name}"))?;
    }
    for source in crate::domain_database_migration::preflight_staging_files(home)? {
        copy_private_file(&source, &into.join(source.file_name().unwrap()))?;
    }
    let marker = [crate::domain_cutover::MARKER, crate::domain_cutover::PREVIOUS_MARKER]
        .map(|name| home.join(name))
        .into_iter()
        .find(|path| path.exists());
    if let Some(marker) = marker {
        std::fs::copy(&marker, into.join(crate::domain_cutover::MARKER))?;
    }
    let migration_marker = home.join(crate::domain_database_migration::MARKER);
    if migration_marker.exists() {
        std::fs::copy(
            &migration_marker,
            into.join(crate::domain_database_migration::MARKER),
        )?;
    }
    Ok(())
}

/// A WAL database with no daemon running has no `-wal` and no `-shm`, and SQLite cannot open it
/// read-only in a folder it may not write: with no `-wal` nobody is writing and the file alone is
/// the whole database, so it is opened as immutable. With a live writer it is opened as usual.
fn open_source(source: &Path) -> rusqlite::Result<Connection> {
    let mut wal = source.as_os_str().to_owned();
    wal.push("-wal");
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    if Path::new(&wal).exists() {
        return Connection::open_with_flags(source, flags);
    }
    let uri = format!("file:{}?immutable=1", source.display());
    Connection::open_with_flags(uri, flags | OpenFlags::SQLITE_OPEN_URI)
}

fn copy_private_file(source: &Path, target: &Path) -> Result<()> {
    use std::io::copy;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let metadata = std::fs::symlink_metadata(source)?;
    anyhow::ensure!(
        metadata.is_file()
            && metadata.nlink() == 1
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o077 == 0,
        "unsafe staging file in preflight HOME"
    );
    let mut from = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(source)?;
    let mut to = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(target)?;
    copy(&mut from, &mut to)?;
    to.sync_all()?;
    Ok(())
}

struct Scratch(PathBuf);

impl Scratch {
    #[expect(clippy::disallowed_methods, reason = "Production Scratch owns and removes the directory in Drop.")]
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "alondrad-preflight-{}-{}",
            std::process::id(),
            crate::auth::random_hex(8)
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        match path.canonicalize() {
            Ok(canonical) => Ok(Self(canonical)),
            Err(e) => {
                let _ = std::fs::remove_dir(&path);
                Err(e.into())
            }
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn home(_name: &str) -> (crate::test_support::Directory, Config) {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        let config = Config::resolve(
            home.to_path_buf(),
            &Default::default(),
            |_| None,
            || Some("Test".into()),
        );
        (home, config)
    }

    /// Every file with its bytes, except SQLite's shared-memory index and an empty WAL, which a
    /// reader of a WAL database creates and which hold no data.
    fn contents(home: &Path) -> BTreeMap<String, Vec<u8>> {
        std::fs::read_dir(home)
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|e| e.file_type().unwrap().is_file())
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    std::fs::read(e.path()).unwrap(),
                )
            })
            .filter(|(name, bytes)| {
                !name.ends_with("-shm") && !(name.ends_with("-wal") && bytes.is_empty())
            })
            .collect()
    }

    #[test]
    fn a_legacy_home_is_refused_without_the_lock_and_left_alone() {
        let (home, config) = home("legacy");
        let hub = crate::hub::Hub::open(config.clone()).unwrap();
        hub.append(alondra_core::Event::Joined {
            session_id: "s1".into(),
        })
        .unwrap();
        let lock = alondra_core::home_lock::HomeLock::recovery(&home).unwrap();
        let before = contents(&home);
        let refused = run(&config).unwrap_err();
        drop(lock);
        assert!(
            format!("{refused:#}").contains("still holds legacy data"),
            "{refused:#}"
        );
        assert_eq!(
            contents(&home),
            before,
            "preflight must not write to the HOME it checks"
        );
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }

    fn committed_domain_home(name: &str) -> (crate::test_support::Directory, Config) {
        let (home, config) = home(name);
        let path = home.join("domain-v2.db");
        let _store = crate::domain_store::DomainStore::open(&path).unwrap();
        let marker = crate::domain_cutover::Marker {
            format: 1,
            phase: crate::domain_cutover::Phase::Committed,
            backup_manifest_sha256: "a".repeat(64),
            source_seq: 0,
            data_epoch: "b".repeat(64),
        };
        std::fs::write(
            home.join(crate::domain_cutover::MARKER),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(
            home.join(crate::domain_cutover::MARKER),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        drop(_store);
        (home, config)
    }

    #[test]
    fn preflight_admits_a_production_sdk_backend_and_refuses_one_whose_artifact_changed() {
        let (home, config) = committed_domain_home("sdk-backend");
        let user = home.join("user");
        let artifact = home.join("artifact");
        std::fs::write(&artifact, b"artifact").unwrap();
        let sha256 = crate::sdk_owner::backend::digest(&artifact).unwrap();
        let backend = serde_json::json!({
            "node": artifact, "node_sha256": sha256, "host": artifact, "host_sha256": sha256, "native": artifact, "native_sha256": sha256,
            "host_environment": {}, "native_environment": {"HOME": user}, "state_root": home.join("state"), "io_timeout_seconds": 30,
        });
        std::fs::write(home.join(crate::sdk_owner::backend::FILE), backend.to_string()).unwrap();
        let before = contents(&home);
        let admitted = run_for(&config, &user).unwrap();
        assert!(admitted.contains("SDK backend configuration") && admitted.contains("admitted"), "{admitted}");
        assert_eq!(contents(&home), before, "preflight must not write to the HOME it checks");
        std::fs::write(&artifact, b"another artifact").unwrap();
        let refused = format!("{:#}", run_for(&config, &user).unwrap_err());
        assert!(refused.contains("does not have the SHA256"), "{refused}");
    }

    #[test]
    fn preflight_migrates_a_markerless_database_only_in_its_snapshot() {
        let (home, config) = committed_domain_home("migration-markerless");
        let before = contents(&home);
        assert!(
            run(&config)
                .unwrap()
                .contains("canonical domain state starts")
        );
        assert_eq!(contents(&home), before);
        assert!(!home.join("domain.db").exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn preflight_resumes_prepared_migration_only_in_its_snapshot() {
        let (home, config) = committed_domain_home("migration-prepared");
        crate::domain_database_migration::prepare_test_migration(&home);
        let before = contents(&home);
        assert!(
            run(&config)
                .unwrap()
                .contains("canonical domain state starts")
        );
        assert_eq!(contents(&home), before);
        assert!(!home.join("domain.db").exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn preflight_accepts_committed_migration_only_in_its_snapshot() {
        let (home, config) = committed_domain_home("migration-committed");
        assert!(crate::domain_database_migration::migrate(&home).unwrap());
        let before = contents(&home);
        assert!(
            run(&config)
                .unwrap()
                .contains("canonical domain state starts")
        );
        assert_eq!(contents(&home), before);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn preflight_rejects_a_prepared_source_changed_after_its_marker() {
        let (home, config) = committed_domain_home("migration-changed-source");
        crate::domain_database_migration::prepare_test_migration(&home);
        Connection::open(home.join("domain-v2.db"))
            .unwrap()
            .execute("UPDATE domain_meta SET revision=revision+1", [])
            .unwrap();
        let before = contents(&home);
        assert!(run(&config).is_err());
        assert_eq!(contents(&home), before);
        std::fs::remove_dir_all(home).unwrap();
    }

    /// 13 Oct: with the daemon down, the upgrade rehearsal could not read the live domain.db (WAL,
    /// no -wal or -shm) from its read-only sandbox: "unable to open database file".
    #[test]
    fn a_wal_database_with_no_daemon_is_read_from_a_folder_it_may_not_write() {
        let (home, _config) = home("wal");
        let source = home.join("domain.db");
        {
            let db = Connection::open(&source).unwrap();
            db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t(v); INSERT INTO t VALUES (42);").unwrap();
        }
        assert!(!home.join("domain.db-wal").exists() && !home.join("domain.db-shm").exists());
        std::fs::set_permissions(&*home, std::fs::Permissions::from_mode(0o500)).unwrap();
        let read = open_source(&source).and_then(|db| db.query_row("SELECT v FROM t", [], |row| row.get::<_, i64>(0)));
        std::fs::set_permissions(&*home, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(read.unwrap(), 42);
        assert!(!home.join("domain.db-shm").exists(), "the live folder is left as it was");
    }
}
