//! The copies of the old database a committed migration left, removed once they are no longer a
//! way back.
use super::domain_database_migration::{SOURCE, committed_at};
use alondra_engine_ports::journal::active_path;
use anyhow::Result;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// How long the old database stays as a recovery copy after the move to `domain.db`.
const RECOVERY_WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub fn remove_recovery_copies(home: &Path, now: SystemTime) -> Result<()> {
    let Some(committed) = committed_at(home)? else {
        return Ok(());
    };
    if !active_path(home).exists()
        || now.duration_since(committed).unwrap_or_default() < RECOVERY_WINDOW
    {
        return Ok(());
    }
    let source = SOURCE;
    let retired = format!("{}-retired-", source.trim_end_matches(".db"));
    for entry in fs::read_dir(home)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let copy = name == source
            || name.strip_prefix(source).is_some_and(|rest| ["-wal", "-shm"].contains(&rest) || rest.starts_with(".old-"))
            || name.starts_with(&retired);
        if !copy {
            continue;
        }
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            remove(&entry.path())?;
        }
        log!("removed {name}, a copy kept by an earlier migration");
    }
    Ok(())
}

fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_database_migration::{DATABASE, MARKER};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    struct Home(crate::test_support::Directory);
    impl Home {
        fn new() -> Self {
            let path = crate::test_support::Directory::new();
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
        fn write(&self, name: &str, body: &str) {
            fs::write(self.0.join(name), body).unwrap();
            fs::set_permissions(self.0.join(name), fs::Permissions::from_mode(0o600)).unwrap();
        }
        fn names(&self) -> Vec<String> {
            let mut names: Vec<_> = fs::read_dir(&self.0).unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| !name.ends_with(".lock"))
                .collect();
            names.sort();
            names
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn migrated(home: &Home) {
        home.write(MARKER, &format!(
            r#"{{"format":2,"phase":"committed","stage_id":"{}","source_epoch":"e","source_revision":1,"source_digest":"{}","source_content_digest":"{}"}}"#,
            "c".repeat(24), "d".repeat(64), "f".repeat(64)));
        home.write(DATABASE, "");
        for name in ["domain-v2.db", "domain-v2.db-wal", "domain-v2.db-shm", "domain-v2.db.old-1790689699"] {
            home.write(name, "old");
        }
        fs::create_dir(home.0.join("domain-v2-retired-1790619331")).unwrap();
        home.write("domain-v2-retired-1790619331/domain-v2.db", "old");
    }

    #[test]
    fn copies_left_by_a_migration_committed_a_week_ago_are_removed() {
        let home = Home::new();
        migrated(&home);
        home.write("domain.db.old-1791082509", "kept");

        remove_recovery_copies(&home.0, SystemTime::now() + RECOVERY_WINDOW).unwrap();

        assert_eq!(home.names(), ["database-migration.json", "domain.db", "domain.db.old-1791082509"]);
    }

    #[test]
    fn copies_stay_while_the_migration_is_recent() {
        let home = Home::new();
        migrated(&home);
        let before = home.names();

        remove_recovery_copies(&home.0, SystemTime::now()).unwrap();

        assert_eq!(home.names(), before);
    }

    #[test]
    fn copies_stay_while_the_migration_is_not_committed() {
        let home = Home::new();
        migrated(&home);
        home.write(MARKER, &fs::read_to_string(home.0.join(MARKER)).unwrap().replace("committed", "prepared"));
        let before = home.names();

        remove_recovery_copies(&home.0, SystemTime::now() + RECOVERY_WINDOW).unwrap();

        assert_eq!(home.names(), before);
    }
}
