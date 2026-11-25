//! The root's binaries against what they front: `alondrad --preflight` says what the daemon's own
//! preflight says, and the real `alondra` CLI, run as a node's mod, a hook and Claude run it, speaks
//! to a daemon put together as the root puts it.
use alondra_domain_rules::*;
use alondra_store_sqlite::DomainStore;
use alondra_store_sqlite::testing::*;
use std::fs;

#[path = "binary_smoke/cli_journeys.rs"]
mod cli_journeys;

/// The engine as the root opens it, with the doubles of the Mac, the real launcher of agent hosts,
/// the screens and the gate of a test.
mod engine {
    use alondra_engine_sdk::domain_runtime::DomainRuntime;
    use std::path::Path;

    /// Opens every runtime the server opens on this thread as `open_at` does.
    pub fn wired() {
        alondra_foundation::durability::relax();
        alondra_daemon_engine::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite);
        alondra_daemon_engine::wiring::open_with(open_at);
    }

    /// The runtime as the daemon opens it, with the doubles of the Mac and of GitHub, the screens the root
    /// registers and a gate that is unpaced and launches on the disk the machine watch measured.
    pub fn open_at(home: &Path, database: &Path, files: &dyn alondra_engine_ports::journal::JournalFiles) -> anyhow::Result<DomainRuntime> {
        let runtime = alondrad::wiring::open_at(home, database, files)?;
        alondra_engine_doubles::machine::wire(runtime.ports(), alondra_daemon_server::watcher::claude_home);
        alondra_engine_doubles::github::wire(runtime.ports());
        alondra_agent_runtime::wire(runtime.ports(), Default::default());
        alondra_views::wire(runtime.ports());
        alondra_daemon_server::screens::install(runtime.ports());
        alondra_engine_admission::for_tests(&runtime);
        Ok(runtime)
    }

    /// `open_at` at the home's active database; what the server opens on this thread opens the same.
    pub fn open(home: &Path) -> anyhow::Result<DomainRuntime> {
        wired();
        open_at(home, &alondra_engine_ports::journal::active_path(home), &alondra_store_sqlite::Sqlite)
    }
}

#[test]
fn preflight_reports_a_rejected_journal_event_without_changing_the_home() {
    let fixture = Fixture::new();
    let path = fixture.0.join("domain.db");
    let mut store = DomainStore::open(&path).unwrap();
    seeded(&mut store);
    execute(&mut store, &agent(), "start", Command::StartTask { task_id: "task-1".into() });
    let trigger: String = store.conn.query_row("SELECT sql FROM sqlite_master WHERE name='lifecycle_event_immutable'", [], |row| row.get(0)).unwrap();
    store.conn.execute_batch("DROP TRIGGER lifecycle_event_immutable").unwrap();
    assert_eq!(store.conn.execute("UPDATE lifecycle_events SET event=?1 WHERE task_id='task-1' AND revision=1", [r#"{"kind":"merge_observed","data":{"operation":"PRIVATE_EVENT_CONTENT"}}"#]).unwrap(), 1);
    store.conn.execute_batch(&trigger).unwrap();
    drop(store);
    let before = fs::read(&path).unwrap();
    let config = alondra_core::config::Config::resolve(fixture.0.to_path_buf(), &Default::default(), |_| None, || Some("Test".into()));
    let diagnostic = format!("{:#}", alondra_daemon_server::preflight::run(&config, &alondra_store_sqlite::Sqlite, alondrad::wiring::open, alondrad::wire_machine).unwrap_err());
    let output = std::process::Command::new(std::env::var("FIXTURE_DAEMON").unwrap())
        .arg("--preflight").env_clear().env("HOME", &*fixture.0).env("ALONDRA_HOME", &*fixture.0)
        .env("TMPDIR", &*fixture.0).envs(alondra_daemon_engine::test_support::xcrun_cache()).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("preflight failed: "), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    for text in [&diagnostic, &stderr] {
        for expected in ["CorruptJournal", "task=task-1", "revision=1", "MergeObserved", "InvalidState"] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        for private in ["PRIVATE_EVENT_CONTENT", "Do useful work", fixture.0.to_str().unwrap()] {
            assert!(!text.contains(private), "{text}");
        }
    }
    assert_eq!(fs::read(&path).unwrap(), before, "preflight must leave its source alone");
}
