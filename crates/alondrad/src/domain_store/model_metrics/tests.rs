use super::*;
use crate::domain_store::tests::{Fixture, seeded};
use alondra_core::domain::*;

fn store() -> (Fixture, DomainStore) {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    (fixture, store)
}

fn sonnet_launch() -> Decision {
    Decision {
        task_id: "task-1".into(), request_id: "request-1".into(), agent_id: Some("agent-1".into()), at: 0, kind: DecisionKind::Launch,
        model: Some("sonnet".into()), effort: Some("medium".into()), approach: Some(Approach::Solo),
        chosen_by: ModelChooser::Orchestrator, reason: Some("why task-1".into()), title: "Title task-1".into(),
        provider: None, tags: None,
    }
}

#[test]
fn decisions_can_be_neither_changed_nor_removed() {
    let (_fixture, mut store) = store();
    store.record_model_decision(&sonnet_launch()).unwrap();
    let update = store.conn.execute("UPDATE model_decisions SET model='opus'", []).unwrap_err().to_string();
    assert!(update.contains("model decisions are immutable"), "{update}");
    let delete = store.conn.execute("DELETE FROM model_decisions", []).unwrap_err().to_string();
    assert!(delete.contains("model decisions are retained"), "{delete}");
    assert_eq!(store.model_decision_rows().unwrap().len(), 1);
}

#[test]
fn a_store_made_before_routing_gains_provider_and_tags_empty_for_its_old_rows() {
    let (fixture, mut store) = store();
    store.record_model_decision(&sonnet_launch()).unwrap();
    store.conn.execute_batch("DROP TRIGGER immutable_model_decision; DROP TRIGGER retain_model_decision;
        ALTER TABLE model_decisions DROP COLUMN provider; ALTER TABLE model_decisions DROP COLUMN task_kind;
        ALTER TABLE model_decisions DROP COLUMN areas; ALTER TABLE model_decisions DROP COLUMN risks;
        ALTER TABLE task_metrics DROP COLUMN provider; ALTER TABLE task_metrics DROP COLUMN kind;").unwrap();
    drop(store);

    let mut reopened = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(reopened.model_decision_routing().unwrap(), [(None, None, None, None)], "the old row keeps its place, without provider or tags");
    let tags = serde_json::from_value(serde_json::json!({"kind": "migration", "risks": ["data_migration", "concurrency"]})).unwrap();
    reopened.record_model_decision(&Decision { provider: Some(alondra_core::accounts::Provider::Codex), tags: Some(tags), ..sonnet_launch() }).unwrap();
    assert_eq!(reopened.model_decision_routing().unwrap()[1],
        (Some("codex".into()), Some("migration".into()), None, Some(r#"["data_migration","concurrency"]"#.into())));
    let task_metrics: i64 = reopened.conn.query_row("SELECT count(*) FROM pragma_table_info('task_metrics') WHERE name IN('provider','kind')", [], |r| r.get(0)).unwrap();
    assert_eq!(task_metrics, 2);
}

#[test]
fn a_store_made_before_model_metrics_opens_in_place_and_keeps_its_rows() {
    let (fixture, mut store) = store();
    store.record_model_decision(&sonnet_launch()).unwrap();
    let before = store.load().unwrap();
    store.conn.execute_batch("DROP TABLE model_decisions; CREATE TABLE IF NOT EXISTS node_costs(session_id TEXT PRIMARY KEY);").unwrap();
    drop(store);

    let mut reopened = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(reopened.load().unwrap(), before, "the domain is as it was");
    assert_eq!(reopened.conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), crate::domain_store::SCHEMA_VERSION);
    assert!(!fixture.0.read_dir().unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains(".old-")), "nothing was moved aside");
    assert!(reopened.model_decision_rows().unwrap().is_empty());
    let node_costs: i64 = reopened.conn.query_row("SELECT count(*) FROM sqlite_master WHERE name='node_costs'", [], |r| r.get(0)).unwrap();
    assert_eq!(node_costs, 0, "the table that never got a cost is gone");
    reopened.record_model_decision(&sonnet_launch()).unwrap();
    assert!(reopened.conn.execute("DELETE FROM model_decisions", []).is_err(), "the recreated table is guarded too");
    drop(reopened);

    let again = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(again.model_decision_rows().unwrap().len(), 1, "a reopen keeps recorded decisions");
}
