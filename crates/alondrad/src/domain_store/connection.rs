//! Private SQLite lifecycle. Domain migrations run under the caller's writer reservation.
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

/// Uses SQLite backup so committed WAL pages are included. Call while holding an
/// IMMEDIATE transaction on another connection; never copy an active .db file.
pub(super) fn backup(path: &Path) -> Result<PathBuf> {
    let before = fs::symlink_metadata(path)?;
    ensure!(
        before.is_file() && before.nlink() == 1 && before.uid() == unsafe { libc::geteuid() },
        "unsafe migration source"
    );
    let mut name = path
        .file_name()
        .context("database filename required")?
        .to_os_string();
    name.push(".migration-backups");
    let parent = path.parent().context("database parent required")?;
    let directory = parent.join(name);
    match fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    let dir_meta = fs::symlink_metadata(&directory)?;
    ensure!(
        dir_meta.is_dir()
            && dir_meta.uid() == unsafe { libc::geteuid() }
            && dir_meta.mode() & 0o077 == 0,
        "migration backup directory must be private and owned"
    );
    let generation = directory.join(format!("domain-{}.sqlite", crate::auth::random_hex(16)));
    let output = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&generation)?;
    let source = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let mut target = Connection::open_with_flags(
        &generation,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    target.pragma_update(None, "synchronous", "FULL")?;
    {
        let backup = rusqlite::backup::Backup::new(&source, &mut target)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            match backup.step(512)? {
                rusqlite::backup::StepResult::Done => break,
                rusqlite::backup::StepResult::More if std::time::Instant::now() < deadline => (),
                _ => anyhow::bail!("domain migration backup could not complete"),
            }
        }
    }
    target.pragma_update(None, "journal_mode", "DELETE")?;
    ensure!(
        target.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))? == "ok",
        "backup integrity failed"
    );
    let after = fs::symlink_metadata(path)?;
    let dir_after = fs::symlink_metadata(&directory)?;
    let file_after = fs::symlink_metadata(&generation)?;
    ensure!(
        (before.dev(), before.ino()) == (after.dev(), after.ino())
            && (dir_meta.dev(), dir_meta.ino()) == (dir_after.dev(), dir_after.ino())
            && (output.metadata()?.dev(), output.metadata()?.ino())
                == (file_after.dev(), file_after.ino()),
        "migration paths changed during backup"
    );
    drop(target);
    output.sync_all()?;
    File::open(&directory)?.sync_all()?;
    File::open(parent)?.sync_all()?;
    let marker = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(generation.with_extension("complete"))?;
    marker.sync_all()?;
    File::open(&directory)?.sync_all()?;
    Ok(generation)
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
    fn backup_includes_committed_wal_and_excludes_uncommitted_migration() {
        let fixture = Fixture::new();
        let (mut conn, path) = open(&fixture.path()).unwrap();
        conn.execute_batch("PRAGMA wal_autocheckpoint=0; CREATE TABLE preserved(value TEXT); PRAGMA wal_checkpoint(TRUNCATE); INSERT INTO preserved VALUES('committed');").unwrap();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        tx.execute("INSERT INTO preserved VALUES('uncommitted')", [])
            .unwrap();
        let backup_path = backup(&path).unwrap();
        let copied =
            Connection::open_with_flags(&backup_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(
            copied
                .query_row("SELECT count(*) FROM preserved", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            copied
                .query_row("SELECT value FROM preserved", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "committed"
        );
        assert!(backup_path.with_extension("complete").is_file());
        tx.rollback().unwrap();
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
    fn refuses_leaf_symlink_hardlink_and_nonprivate_backup_directory() {
        let fixture = Fixture::new();
        let original = fixture.0.join("original.sqlite");
        fs::write(&original, "preserved bytes").unwrap();
        symlink(&original, fixture.path()).unwrap();
        assert!(open(&fixture.path()).is_err());
        assert_eq!(fs::read_to_string(&original).unwrap(), "preserved bytes");
        fs::remove_file(fixture.path()).unwrap();
        fs::hard_link(&original, fixture.path()).unwrap();
        assert!(open(&fixture.path()).is_err());
        fs::remove_file(fixture.path()).unwrap();
        let (conn, path) = open(&fixture.path()).unwrap();
        conn.execute_batch("CREATE TABLE x(id INTEGER)").unwrap();
        fs::DirBuilder::new()
            .mode(0o755)
            .create(fixture.0.join("domain.sqlite.migration-backups"))
            .unwrap();
        fs::set_permissions(
            fixture.0.join("domain.sqlite.migration-backups"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(backup(&path).is_err());
    }
}
