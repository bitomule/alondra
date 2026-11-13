//! Domain storage lives in its own crate; runtime-dependent fixtures stay with the engine.
pub use alondra_daemon_engine_store::*;
#[cfg(any(test, feature = "test-hooks"))]
pub use owner::admit_through_sdk;
#[cfg(any(test, feature = "test-hooks"))]
#[path = "domain_store/runtime_owner.rs"]
mod owner;
#[cfg(any(test, feature = "test-hooks"))]
pub mod tests {
pub use alondra_daemon_engine_store::tests::*;
#[cfg(test)]
use super::*;
#[test]
fn a_continuation_storage_failure_rolls_back_the_transfer_and_publication() {
    for deferred in [false, true] {
        let (fixture, _root, store, command, _) = continuation_fixture();
        drop(store);
        let runtime = crate::domain_runtime::DomainRuntime::open_at(&fixture.0, &fixture.path()).unwrap();
        let observer = Connection::open(fixture.path()).unwrap();
        if deferred {
            observer.execute_batch("CREATE TABLE missing_parent(id INTEGER PRIMARY KEY);
                CREATE TABLE deferred_child(parent INTEGER REFERENCES missing_parent(id) DEFERRABLE INITIALLY DEFERRED);
                CREATE TRIGGER fail_continuation AFTER INSERT ON semantic_receipts WHEN NEW.command_id='continue'
                BEGIN INSERT INTO deferred_child VALUES(404); END;").unwrap();
        } else {
            observer.execute_batch("CREATE TRIGGER fail_continuation BEFORE INSERT ON task_checklists WHEN NEW.task_id='continued'
                BEGIN SELECT RAISE(ABORT,'injected continuation write failure'); END;").unwrap();
        }
        let tables = ["tasks", "task_checklists", "task_worktree_records", "task_continuations", "semantic_receipts", "command_audit",
            "domain_messages", "domain_outbox", "lifecycle_events"];
        let counts = || tables.map(|table| observer.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get::<_, i64>(0)).unwrap());
        let before_counts = counts();
        let before = runtime.read();
        let changes = runtime.subscribe();
        let error = runtime.execute(&Actor::Orchestrator { generation: 0 }, &command, None, || Ok(())).unwrap_err();
        let StoreError::Storage(cause) = error else { panic!("expected storage failure: {error:?}") };
        let sqlite = cause.downcast_ref::<rusqlite::Error>().or_else(|| {
            match cause.downcast_ref::<alondra_store::Error>() {
                Some(alondra_store::Error::Database(error)) => Some(error),
                _ => None,
            }
        });
        assert!(matches!(sqlite, Some(rusqlite::Error::SqliteFailure(code, _))
            if code.extended_code == if deferred { rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY } else { rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER }), "{cause:#}");
        assert_eq!(counts(), before_counts);
        assert!(Arc::ptr_eq(runtime.read().state(), before.state()), "no partial state is published");
        assert!(!changes.has_changed().unwrap(), "no failed change is announced");
        assert!(runtime.take_acknowledgements().is_empty());
        let mut reopened = DomainStore::open(&fixture.path()).unwrap();
        assert_eq!(&*reopened.load().unwrap(), &**before.state(), "rollback also survives reopening");
    }
}

}
#[cfg(test)]
#[path = "domain_store/runtime_metrics_fold.rs"]
pub(crate) mod metrics_fold;
#[cfg(test)]
#[path = "domain_store/runtime_retention.rs"]
pub(crate) mod retention;
#[cfg(test)]
#[path = "domain_store/runtime_metrics.rs"]
mod metrics;
#[cfg(test)]
mod repository {
pub use alondra_daemon_engine_store::testing::{load, save_journal};
}
#[cfg(test)]
mod lifecycle_cache_tests;
#[cfg(test)]
mod load_tests;
#[cfg(test)]
use {
    alondra_core::domain::*,
    anyhow::Result,
    rusqlite::{Connection, OptionalExtension, Transaction, params},
    serde_json::{Value, json},
    std::{path::Path, sync::Arc},
};
