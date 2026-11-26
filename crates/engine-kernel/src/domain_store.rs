//! The journal the runtime speaks to, through the engine's port; runtime-dependent fixtures stay
//! with the engine.
pub use alondra_engine_ports::journal::*;
pub use alondra_engine_ports::journal::{Journal, JournalFiles, OwnerJournal, ReadTransaction};
pub use alondra_engine_ports::journal::account_move::{AccountMove, Plan as AccountMovePlan, Step as AccountMoveStep};
pub use alondra_engine_ports::journal::handover::{Handover, Plan as HandoverPlan, Process as HandoverProcess, Step as HandoverStep, copy_path as handover_copy_path};
pub use alondra_engine_ports::journal::owner::{
    AdapterRefusal, Adoption, DiscardReason, ObservationRefusal, OwnerCommit, OwnerDraft, Preparer, Refuse, StagedOutcome, carried_refusal, refuse_next,
};
pub use alondra_engine_ports::journal::provider_transition;

/// What the journal records of the orchestrator's batches.
pub mod batch_effects {
    pub use alondra_engine_ports::journal::batch::*;
}
pub mod model_metrics {
    pub use alondra_engine_ports::journal::metrics::{Decision, DecisionKind};
}
#[cfg(not(test))]
pub mod metrics_fold {
    pub use alondra_engine_ports::journal::metrics::Spent;
}
pub mod wording {
    pub use alondra_domain_model::orchestrator_wording::{approved_plan_work, orchestrator_text};
    pub use alondra_engine_ports::journal::decisions::{SdkDecision, SdkDecisions};
}
pub mod provider_settings {
    pub use alondra_daemon_engine_model::options::{pending, read_optional, read_options};
    pub use alondra_engine_ports::journal::settings::{PendingProjection, ProjectionIdentity};
}
/// The SQLite journal itself, for the tests that read its tables.
#[cfg(test)]
pub use alondra_store_sqlite::{DomainStore, statement_counts, testing};
pub use owner_fixtures::{admit_through_sdk, admit_through_sdk_with_host};
#[path = "domain_store/runtime_owner.rs"]
mod owner_fixtures;
#[cfg(test)]
mod tests {
use super::*;
use alondra_store_sqlite::testing::continuation_fixture;
#[test]
fn a_continuation_storage_failure_rolls_back_the_transfer_and_publication() {
    for deferred in [false, true] {
        let root = crate::test_support::Directory::new();
        let worktree = alondra_engine_doubles::worktrees::testing::task_worktree(&root, "original");
        alondra_store_sqlite::testing::wire_stores_with(|ports| alondra_engine_doubles::machine::wire(ports, alondra_daemon_support::watcher::claude_home));
        let (fixture, store, command, _) = continuation_fixture(&worktree, &alondra_domain_rules::Rules);
        drop(store);
        let runtime = crate::domain_runtime::DomainRuntime::open_unwired_at(&fixture.0, &fixture.path()).unwrap();
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
        let mut reopened = DomainStore::open(&fixture.path(), &alondra_domain_rules::Rules).unwrap();
        assert_eq!(&*reopened.load().unwrap(), &**before.state(), "rollback also survives reopening");
    }
}

}
#[cfg(test)]
#[path = "domain_store/runtime_metrics_fold.rs"]
pub mod metrics_fold;
#[cfg(test)]
pub use alondra_store_sqlite::retention_fixtures;
#[cfg(test)]
#[path = "domain_store/runtime_retention.rs"]
pub mod retention;
#[cfg(test)]
#[path = "domain_store/runtime_metrics.rs"]
mod metrics;
#[cfg(test)]
mod repository {
pub use alondra_store_sqlite::testing::{load, save_journal};
}
#[cfg(test)]
mod lifecycle_cache_tests;
#[cfg(test)]
use {
    alondra_domain_model::*,
    anyhow::Result,
    rusqlite::{Connection, OptionalExtension, Transaction, params},
    serde_json::{Value, json},
    std::{path::Path, sync::Arc},
};
