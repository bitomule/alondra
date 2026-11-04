//! The domain store as alondrad's preflight reads it.
use alondra_daemon_engine::domain_store::tests::*;
use alondra_daemon_engine::domain_store::*;
use alondra_core::domain::*;
use std::fs;

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
    let diagnostic = format!("{:#}", crate::preflight::run(&config).unwrap_err());
    let output = std::process::Command::new(std::env::var("FIXTURE_DAEMON").unwrap())
        .arg("--preflight").env_clear().env("HOME", &*fixture.0).env("ALONDRA_HOME", &*fixture.0)
        .env("TMPDIR", &*fixture.0).output().unwrap();
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
