//! Startup moves of HOME files to their current names, removal of the copies earlier migrations
//! left under the old ones once they are no longer a way back, and of files no reader uses any
//! more.
use crate::{admission, domain_cutover, domain_database_migration as database, domain_orchestrator};
use anyhow::Result;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// How long the old database stays as a recovery copy after the move to `domain.db`.
const RECOVERY_WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// The plan's limits as the node mods measured them, and the Mac-wide usage pause, with their
/// half-written copies: each account's usage now lives in the domain, and the pause in memory.
const RETIRED: [&str; 4] = ["usage.json", "usage.next", "usage-exhausted.json", "usage-exhausted.next"];

/// Run before anything reads HOME. Without the HOME lock (another daemon owns it)
/// nothing moves; readers still accept the old names.
pub fn apply(home: &Path) -> Result<()> {
    if !home.exists() {
        return Ok(());
    }
    let Ok(_lock) = alondra_core::home_lock::HomeLock::recovery(home) else {
        return Ok(());
    };
    rename(home, domain_orchestrator::PREVIOUS_FILE, domain_orchestrator::FILE)?;
    rename(home, domain_cutover::PREVIOUS_MARKER, domain_cutover::MARKER)?;
    remove(&home.join(admission::PREVIOUS_FILE).with_extension("next"))?;
    rename(home, admission::PREVIOUS_FILE, admission::FILE)?;
    for name in RETIRED {
        remove(&home.join(name))?;
    }
    remove_recovery_copies(home, SystemTime::now())
}

fn rename(home: &Path, from: &str, to: &str) -> Result<()> {
    remove(&home.join(format!("{from}.next")))?;
    let from = home.join(from);
    if !from.exists() {
        return Ok(());
    }
    if home.join(to).exists() {
        remove(&from)?;
    } else {
        fs::rename(&from, home.join(to))?;
    }
    fs::File::open(home)?.sync_all()?;
    Ok(())
}

fn remove_recovery_copies(home: &Path, now: SystemTime) -> Result<()> {
    let Some(committed) = database::committed_at(home)? else {
        return Ok(());
    };
    if !database::active_path(home).exists()
        || now.duration_since(committed).unwrap_or_default() < RECOVERY_WINDOW
    {
        return Ok(());
    }
    let source = database::SOURCE;
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

    const ORCHESTRATOR: &str = r#"{"session_id":"d6ec6dac-9169-4dc0-ab34-0e05a07d7417","active_run":null,"last_run_id":null,"last_error":null,"retry_at":null,"consecutive_failures":0,"cancel_requested":false,"pending_resolutions":[]}"#;

    fn cutover_marker() -> String {
        format!(r#"{{"format":1,"phase":"committed","backup_manifest_sha256":"{}","source_seq":0,"data_epoch":"{}"}}"#, "a".repeat(64), "b".repeat(64))
    }

    fn migrated(home: &Home) {
        home.write(database::MARKER, &format!(
            r#"{{"format":2,"phase":"committed","stage_id":"{}","source_epoch":"e","source_revision":1,"source_digest":"{}","source_content_digest":"{}"}}"#,
            "c".repeat(24), "d".repeat(64), "f".repeat(64)));
        home.write(database::DATABASE, "");
        for name in ["domain-v2.db", "domain-v2.db-wal", "domain-v2.db-shm", "domain-v2.db.old-1790689699"] {
            home.write(name, "old");
        }
        fs::create_dir(home.0.join("domain-v2-retired-1790619331")).unwrap();
        home.write("domain-v2-retired-1790619331/domain-v2.db", "old");
    }

    #[test]
    fn the_orchestrator_record_moves_to_its_new_name() {
        let home = Home::new();
        home.write("orchestrator-v2.json", ORCHESTRATOR);
        home.write("orchestrator-v2.json.next", ORCHESTRATOR);

        apply(&home.0).unwrap();

        assert_eq!(home.names(), ["orchestrator.json"]);
        assert_eq!(domain_orchestrator::load(&home.0).unwrap().session_id.as_deref(), Some("d6ec6dac-9169-4dc0-ab34-0e05a07d7417"));
    }

    #[test]
    fn a_record_already_under_the_new_name_wins() {
        let home = Home::new();
        home.write("orchestrator-v2.json", ORCHESTRATOR);
        home.write("orchestrator.json", &ORCHESTRATOR.replace("d6ec6dac", "0000aaaa"));

        apply(&home.0).unwrap();

        assert_eq!(home.names(), ["orchestrator.json"]);
        assert!(domain_orchestrator::load(&home.0).unwrap().session_id.unwrap().starts_with("0000aaaa"));
    }

    #[test]
    fn the_cutover_marker_reads_under_either_name_and_moves_to_the_new_one() {
        let home = Home::new();
        home.write("domain-v2-cutover.json", &cutover_marker());
        assert!(domain_cutover::read_marker(&home.0).unwrap().is_some());

        apply(&home.0).unwrap();

        assert_eq!(home.names(), ["domain-cutover.json"]);
        assert_eq!(domain_cutover::read_marker(&home.0).unwrap().unwrap().phase, domain_cutover::Phase::Committed);
    }

    #[test]
    fn the_usage_files_of_an_earlier_release_are_removed() {
        let home = Home::new();
        home.write("usage.json", r#"{"windows":[{"kind":"seven_day","percent_used":53.0,"resets_at":1790553600000}],"measured_at":1790192202000}"#);
        home.write("usage.next", "{");
        home.write("usage-exhausted.json", r#"{"exhausted":{"until":1790202000000,"since":1790192202000},"announced":true}"#);
        home.write("usage-exhausted.next", "{");
        home.write("device-usage.json", r#"{"tracking_since_ms":0}"#);

        apply(&home.0).unwrap();

        assert_eq!(home.names(), ["device-usage.json"], "another file named for usage stays");
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

        apply(&home.0).unwrap();

        assert_eq!(home.names(), before);
    }

    #[test]
    fn copies_stay_while_the_migration_is_not_committed() {
        let home = Home::new();
        migrated(&home);
        home.write(database::MARKER, &fs::read_to_string(home.0.join(database::MARKER)).unwrap().replace("committed", "prepared"));
        let before = home.names();

        remove_recovery_copies(&home.0, SystemTime::now() + RECOVERY_WINDOW).unwrap();

        assert_eq!(home.names(), before);
    }
}
