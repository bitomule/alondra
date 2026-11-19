//! Startup moves of HOME files to their current names, removal of the copies earlier migrations
//! left under the old ones once they are no longer a way back, and of files no reader uses any
//! more.
use alondra_core::clock;
use alondra_engine_ports::admission;
use crate::domain_cutover;
use alondra_engine_ports::journal::JournalFiles;
use anyhow::Result;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

/// The plan's limits as the node mods measured them, and the Mac-wide usage pause, with their
/// half-written copies: each account's usage now lives in the domain, and the pause in memory.
const RETIRED: [&str; 4] = ["usage.json", "usage.next", "usage-exhausted.json", "usage-exhausted.next"];

/// Run before anything reads HOME. Without the HOME lock (another daemon owns it)
/// nothing moves; readers still accept the old names. `records` are the previous and current names
/// of the records kept by the crates above the engine, moved first; the journal removes the copies
/// its migrations left once they are no longer a way back.
pub fn apply(home: &Path, records: &[(&str, &str)], files: &dyn JournalFiles) -> Result<()> {
    if !home.exists() {
        return Ok(());
    }
    let Ok(_lock) = alondra_core::home_lock::HomeLock::recovery(home) else {
        return Ok(());
    };
    for (previous, current) in records {
        rename(home, previous, current)?;
    }
    rename(home, domain_cutover::PREVIOUS_MARKER, domain_cutover::MARKER)?;
    remove(&home.join(admission::PREVIOUS_FILE).with_extension("next"))?;
    rename(home, admission::PREVIOUS_FILE, admission::FILE)?;
    for name in RETIRED {
        remove(&home.join(name))?;
    }
    files.remove_recovery_copies(home, clock::system_time())
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

    const ORCHESTRATOR_RECORD: &[(&str, &str)] = &[("orchestrator-v2.json", "orchestrator.json")];

    /// A journal that left no recovery copies to remove.
    struct NoCopies;
    impl JournalFiles for NoCopies {
        fn open(&self, _: &Path, _: Option<std::sync::Arc<dyn alondra_engine_ports::journal::decisions::SdkDecisions>>) -> Result<Box<dyn alondra_engine_ports::journal::Journal>> { unreachable!() }
        fn open_readonly(&self, _: &Path) -> Result<Box<dyn alondra_engine_ports::journal::Journal>> { unreachable!() }
        fn migrate_and_fence(&self, _: &Path) -> Result<(bool, Option<Box<dyn std::any::Any + Send>>)> { unreachable!() }
        fn validate_preflight_source(&self, _: &Path) -> Result<()> { unreachable!() }
        fn preflight_staging_files(&self, _: &Path) -> Result<Vec<std::path::PathBuf>> { unreachable!() }
        fn migration_marker(&self) -> &'static str { unreachable!() }
        fn remove_recovery_copies(&self, _: &Path, _: std::time::SystemTime) -> Result<()> { Ok(()) }
    }

    fn session_of(home: &Home) -> String {
        let record: serde_json::Value = serde_json::from_slice(&fs::read(home.0.join("orchestrator.json")).unwrap()).unwrap();
        record["session_id"].as_str().unwrap().to_owned()
    }

    const ORCHESTRATOR: &str = r#"{"session_id":"d6ec6dac-9169-4dc0-ab34-0e05a07d7417","active_run":null,"last_run_id":null,"last_error":null,"retry_at":null,"consecutive_failures":0,"cancel_requested":false,"pending_resolutions":[]}"#;

    fn cutover_marker() -> String {
        format!(r#"{{"format":1,"phase":"committed","backup_manifest_sha256":"{}","source_seq":0,"data_epoch":"{}"}}"#, "a".repeat(64), "b".repeat(64))
    }

    #[test]
    fn the_orchestrator_record_moves_to_its_new_name() {
        let home = Home::new();
        home.write("orchestrator-v2.json", ORCHESTRATOR);
        home.write("orchestrator-v2.json.next", ORCHESTRATOR);

        apply(&home.0, ORCHESTRATOR_RECORD, &NoCopies).unwrap();

        assert_eq!(home.names(), ["orchestrator.json"]);
        assert_eq!(session_of(&home), "d6ec6dac-9169-4dc0-ab34-0e05a07d7417");
    }

    #[test]
    fn a_record_already_under_the_new_name_wins() {
        let home = Home::new();
        home.write("orchestrator-v2.json", ORCHESTRATOR);
        home.write("orchestrator.json", &ORCHESTRATOR.replace("d6ec6dac", "0000aaaa"));

        apply(&home.0, ORCHESTRATOR_RECORD, &NoCopies).unwrap();

        assert_eq!(home.names(), ["orchestrator.json"]);
        assert!(session_of(&home).starts_with("0000aaaa"));
    }

    #[test]
    fn the_cutover_marker_reads_under_either_name_and_moves_to_the_new_one() {
        let home = Home::new();
        home.write("domain-v2-cutover.json", &cutover_marker());
        assert!(domain_cutover::read_marker(&home.0).unwrap().is_some());

        apply(&home.0, ORCHESTRATOR_RECORD, &NoCopies).unwrap();

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

        apply(&home.0, ORCHESTRATOR_RECORD, &NoCopies).unwrap();

        assert_eq!(home.names(), ["device-usage.json"], "another file named for usage stays");
    }
}
