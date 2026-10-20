//! The marker a cutover of a HOME to the domain store left, and the refusal to start on a HOME
//! that still holds legacy data. Nothing from the legacy data is imported.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub(crate) const MARKER: &str = "domain-cutover.json";
/// The marker's name before it was renamed; startup moves it to `MARKER`.
pub(crate) const PREVIOUS_MARKER: &str = "domain-v2-cutover.json";

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

/// A malformed marker must stop startup rather than be taken for a HOME that was never cut over.
pub(crate) fn read_marker(home: &Path) -> Result<Option<Marker>> {
    let open = |name: &str| OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(home.join(name));
    let file = match open(MARKER).or_else(|error| match error.kind() {
        std::io::ErrorKind::NotFound => open(PREVIOUS_MARKER),
        _ => Err(error),
    }) {
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

const LEGACY_STORES: [&str; 2] = ["alondra.db", "schedules.db"];

/// A daemon must not run a HOME that still holds legacy data. It refuses before
/// taking the HOME lock and leaves the data as it is.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;

    #[test]
    fn marker_is_bounded_private_and_fail_closed() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!(
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
        fs::write(home.join(MARKER), serde_json::to_vec(&marker).unwrap()).unwrap();
        fs::set_permissions(home.join(MARKER), fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_marker(&home).unwrap(), Some(marker));
        fs::set_permissions(home.join(MARKER), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_marker(&home).is_err());
        fs::set_permissions(home.join(MARKER), fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(home.join(MARKER), b"malformed").unwrap();
        assert!(read_marker(&home).is_err());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn an_empty_or_tableless_legacy_store_does_not_block_startup() {
        let scratch_directory = crate::test_support::Directory::new();
        let root = scratch_directory.parent().unwrap().join(format!("alondra-empty-legacy-{}", crate::auth::random_hex(8)));
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
        let scratch_directory = crate::test_support::Directory::new();
        let root = scratch_directory.parent().unwrap().join(format!("alondra-journal-legacy-{}", crate::auth::random_hex(8)));
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
