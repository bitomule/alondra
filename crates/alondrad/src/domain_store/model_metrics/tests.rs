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
