//! `alondrad --preflight`: the startup this binary would do, run against a private snapshot of
//! HOME so a running daemon, its HOME lock and its data are never touched.
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use alondra_core::config::Config;
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

const DATABASES: [&str; 3] = ["alondra.db", "domain-v2.db", "schedules.db"];

pub fn run(config: &Config) -> Result<String> {
    alondra_core::home_lock::ensure_ready(&config.home)?;
    let scratch = Scratch::new()?;
    snapshot(&config.home, &scratch.0)?;
    let mut copy = config.clone();
    copy.home = scratch.0.clone();
    start(&copy)
}

/// The same selection and bootstrap `run` performs, on a HOME it may freely write.
fn start(config: &Config) -> Result<String> {
    crate::retire_empty_older_sidecar(&config.home)?;
    match crate::protocol_mode(&config.home)? {
        crate::ProtocolMode::Legacy => {
            let hub = crate::hub::Hub::open(config.clone(), false)?;
            Ok(format!("legacy state starts: {} nodes", hub.read(|v| v.nodes.len())))
        }
        crate::ProtocolMode::Prepared => anyhow::bail!("domain cutover is prepared but not committed"),
        crate::ProtocolMode::Committed(epoch) => {
            crate::domain_bootstrap::V2Only::open(&config.home, &epoch)?;
            Ok("domain v2 state starts".into())
        }
    }
}

/// A consistent copy of each database, read through SQLite so a live writer's WAL is included.
fn snapshot(home: &Path, into: &Path) -> Result<()> {
    let home = &home.canonicalize().with_context(|| format!("reading {}", home.display()))?;
    for name in DATABASES {
        let source = home.join(name);
        match std::fs::symlink_metadata(&source) {
            Ok(meta) => anyhow::ensure!(meta.is_file(), "{name} must be a regular file"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        }
        let from = Connection::open_with_flags(&source, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW)
            .with_context(|| format!("reading {name}"))?;
        let mut to = Connection::open(into.join(name))?;
        std::fs::set_permissions(into.join(name), std::fs::Permissions::from_mode(0o600))?;
        rusqlite::backup::Backup::new(&from, &mut to)?
            .run_to_completion(4096, std::time::Duration::ZERO, None)
            .with_context(|| format!("copying {name}"))?;
    }
    let marker = home.join(crate::domain_cutover::MARKER);
    if marker.exists() {
        std::fs::copy(&marker, into.join(crate::domain_cutover::MARKER))?;
    }
    Ok(())
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("alondrad-preflight-{}-{}", std::process::id(), crate::auth::random_hex(8)));
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

    fn home(name: &str) -> (PathBuf, Config) {
        let home = std::env::temp_dir().join(format!("alondra-preflight-{name}-{}", crate::auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        let config = Config::resolve(home.clone(), &Default::default(), |_| None, || Some("Test".into()));
        (home, config)
    }

    /// Every file with its bytes, except SQLite's shared-memory index and an empty WAL, which a
    /// reader of a WAL database creates and which hold no data.
    fn contents(home: &Path) -> BTreeMap<String, Vec<u8>> {
        std::fs::read_dir(home).unwrap().map(|e| e.unwrap())
            .filter(|e| e.file_type().unwrap().is_file())
            .map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap()))
            .filter(|(name, bytes)| !name.ends_with("-shm") && !(name.ends_with("-wal") && bytes.is_empty()))
            .collect()
    }

    #[test]
    fn a_startable_home_passes_and_nothing_in_it_changes() {
        let (home, config) = home("ok");
        let hub = crate::hub::Hub::open(config.clone(), false).unwrap();
        hub.append(alondra_core::Event::Joined { session_id: "s1".into() }).unwrap();
        let lock = alondra_core::home_lock::HomeLock::recovery(&home).unwrap();
        let before = contents(&home);
        let passed = run(&config).unwrap();
        drop(lock);
        assert!(passed.contains("legacy state starts"), "{passed}");
        assert_eq!(contents(&home), before, "preflight must not write to the HOME it checks");
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn stored_state_that_fails_the_integrity_audit_is_refused_and_left_alone() {
        let (home, config) = home("audit");
        drop(crate::hub::Hub::open(config.clone(), false).unwrap());
        let mut view = alondra_core::View::default();
        view.nodes.insert("s1".into(), alondra_core::Node::new("other", 1));
        let db = Connection::open(home.join("alondra.db")).unwrap();
        db.execute("INSERT INTO snapshots(seq,at,body) VALUES(1000,1,?1)", [serde_json::to_string(&view).unwrap()]).unwrap();
        drop(db);
        let before = contents(&home);
        let refused = run(&config).unwrap_err();
        assert!(format!("{refused:#}").contains("integrity audit"), "{refused:#}");
        assert_eq!(contents(&home), before);
        std::fs::remove_dir_all(home).unwrap();
    }
}
