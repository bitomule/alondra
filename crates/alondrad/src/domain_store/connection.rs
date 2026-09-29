//! Private SQLite lifecycle.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub(super) fn open(path: &Path) -> Result<(Connection, PathBuf)> {
    let parent = fs::canonicalize(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    let path = parent.join(path.file_name().context("database filename required")?);
    match fs::symlink_metadata(&path) {
        Ok(meta) => ensure!(
            meta.is_file()
                && meta.nlink() == 1
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.mode() & 0o077 == 0,
            "unsafe domain database path"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)?
                .sync_all()?;
            File::open(&parent)?.sync_all()?;
        }
        Err(error) => return Err(error.into()),
    }
    let before = fs::symlink_metadata(&path)?;
    ensure!(
        before.mode() & 0o077 == 0,
        "domain database must be owner-only"
    );
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let after = fs::symlink_metadata(&path)?;
    ensure!(
        (before.dev(), before.ino()) == (after.dev(), after.ino()),
        "domain database replaced while opening"
    );
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    ensure!(
        conn.pragma_query_value(None, "foreign_keys", |r| r.get::<_, i64>(0))? == 1,
        "foreign keys unavailable"
    );
    Ok((conn, path))
}

/// Moves a database with a foreign schema version aside, with its -wal and -shm, so a
/// fresh store can take its place. The caller must hold no connection to it.
pub(super) fn set_aside(path: &Path, unix_seconds: u64) -> Result<PathBuf> {
    let name = path.file_name().context("database filename required")?;
    let moved = |suffix: &str| {
        let mut file = name.to_os_string();
        file.push(suffix);
        path.with_file_name(file)
    };
    let kept = moved(&format!(".old-{unix_seconds}"));
    ensure!(!kept.exists(), "{} already exists", kept.display());
    for suffix in ["", "-wal", "-shm"] {
        match fs::rename(moved(suffix), moved(&format!(".old-{unix_seconds}{suffix}"))) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => (),
        }
    }
    File::open(path.parent().context("database parent required")?)?.sync_all()?;
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "alondra-domain-sql-{}",
                crate::auth::random_hex(16)
            ));
            fs::DirBuilder::new().mode(0o700).create(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> PathBuf {
            self.0.join("domain.sqlite")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn connection_enforces_real_foreign_keys_and_durability() {
        let fixture = Fixture::new();
        let (conn, _) = open(&fixture.path()).unwrap();
        assert_eq!(fs::metadata(fixture.path()).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            conn.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
                .unwrap(),
            "wal"
        );
        assert_eq!(
            conn.pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        conn.execute_batch("CREATE TABLE parents(id INTEGER PRIMARY KEY); CREATE TABLE children(parent_id INTEGER REFERENCES parents(id));").unwrap();
        assert!(conn.execute("INSERT INTO children VALUES(42)", []).is_err());
    }

    #[test]
    fn rejects_existing_database_readable_by_other_users() {
        let fixture = Fixture::new();
        let path = fixture.path();
        let (conn, _) = open(&path).unwrap();
        drop(conn);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(open(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(open(&path).is_ok());
    }
    #[test]
    fn refuses_leaf_symlink_and_hardlink() {
        let fixture = Fixture::new();
        let original = fixture.0.join("original.sqlite");
        fs::write(&original, "preserved bytes").unwrap();
        symlink(&original, fixture.path()).unwrap();
        assert!(open(&fixture.path()).is_err());
        assert_eq!(fs::read_to_string(&original).unwrap(), "preserved bytes");
        fs::remove_file(fixture.path()).unwrap();
        fs::hard_link(&original, fixture.path()).unwrap();
        assert!(open(&fixture.path()).is_err());
    }
}
