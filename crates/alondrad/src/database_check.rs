//! A physical audit of a private HOME snapshot, independent of daemon startup.
use std::{fs::{self, OpenOptions}, os::unix::fs::{DirBuilderExt, OpenOptionsExt}, path::{Path, PathBuf}};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags};

pub(crate) fn run(path: &Path) -> Result<()> {
    let parent = fs::canonicalize(path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")))?;
    let path = parent.join(path.file_name().context("database filename required")?);
    let lock = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW)
        .open(parent.join("service-home.lock"))
        .context("database check requires the existing service-home.lock beside the database; use a stopped HOME or its private copy")?;
    ensure!(lock.metadata()?.is_file(), "HOME lock must be a regular file");
    lock.try_lock().context("Alondra HOME is busy; check a stopped HOME or its private copy")?;
    let scratch = Scratch::new()?;
    let copy = scratch.0.join("database");
    copy_file(&path, &copy)?;
    for suffix in ["-wal", "-journal"] {
        let mut source = path.as_os_str().to_owned();
        source.push(suffix);
        let mut target = copy.as_os_str().to_owned();
        target.push(suffix);
        match copy_file(Path::new(&source), Path::new(&target)) {
            Ok(()) => {},
            Err(error) if error.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {},
            Err(error) => return Err(error),
        }
    }
    drop(lock);
    let mut connection = Connection::open_with_flags(
        &copy,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    ).with_context(|| format!("opening the private copy of {}", path.display()))?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.pragma_update(None, "query_only", true)?;
    let snapshot = connection.transaction()?;
    audit(&snapshot).with_context(|| format!("checking {}", path.display()))?;
    snapshot.commit()?;
    Ok(())
}

fn copy_file(source: &Path, target: &Path) -> Result<()> {
    let mut source = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(source)?;
    ensure!(source.metadata()?.is_file(), "database and journal must be regular files");
    let mut target = OpenOptions::new().write(true).create_new(true).mode(0o600).open(target)?;
    std::io::copy(&mut source, &mut target)?;
    Ok(())
}

struct Scratch(PathBuf);
impl Scratch {
    #[expect(clippy::disallowed_methods, reason = "The diagnostic owns and removes this private directory in Drop.")]
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("alondrad-database-check-{}-{}", std::process::id(), crate::auth::random_hex(8)));
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

pub(crate) fn audit(connection: &Connection) -> Result<()> {
    let result: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    ensure!(result == "ok", "database integrity check failed: {result}");
    let violations: i64 = connection.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| row.get(0))?;
    ensure!(violations == 0, "database foreign key check failed: {violations} violation(s)");
    Ok(())
}
