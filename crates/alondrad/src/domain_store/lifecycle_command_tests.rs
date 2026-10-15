//! Technical SQL composition: real guarded card commands, not SDK or production cutover proof.
use super::*;
use crate::domain_store::tests::{Fixture, seeded};
use alondra_domain::{Event as LifecycleEvent, RuntimeFact, Trigger};
use alondra_foundation::{ConversationId, HostId, InputId, InstanceId, TurnId};
use alondra_provider_contract::{Capability, Placement, ProviderBinding};
use alondra_store::{AuthorityScope, InputPayload, LifecycleTransaction};
use std::collections::{BTreeMap, BTreeSet};

fn actor() -> Actor {
    Actor::Agent {
        agent_id: "agent-1".into(),
    }
}
fn user() -> Actor {
    Actor::User {
        user_id: "composite-user".into(),
    }
}
fn envelope(store: &mut DomainStore, id: &str, command: Command) -> CommandEnvelope {
    let state = store.load().unwrap();
    CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: id.into(),
        expected_revision: state.revision,
        command,
    }
}
fn question(store: &mut DomainStore) {
    let command = envelope(
        store,
        "composite-question",
        Command::AskQuestion {
            interaction_id: "composite-card".into(),
            scope: Scope::Task {
                task_id: "task-1".into(),
            },
            recipient: Recipient::User,
            body: "Choose the next step".into(),
            attachments: vec![],
            blocking: true,
            options: vec![QuestionOption {
                id: "continue".into(),
                label: "Continue".into(),
                description: None,
            }],
            card: CardSpec::derived(CardKind::Provide, "Choose the next step"),
        },
    );
    store
        .execute_authenticated(&actor(), &command, None)
        .unwrap();
}
fn payload(id: &str) -> InputPayload {
    InputPayload::new(
        InputId::new(id).unwrap(),
        format!("Authorized fixture input {id}"),
    )
    .unwrap()
}
fn setup(released: bool) -> (Fixture, DomainStore) {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    question(&mut store);
    let epoch = store.load().unwrap().data_epoch.clone();
    let mut raw =
        rusqlite::Transaction::new_unchecked(&store.conn, TransactionBehavior::Immediate).unwrap();
    LifecycleTransaction::install(&mut raw).unwrap();
    let mut journal =
        LifecycleTransaction::attach(raw, AuthorityScope::new(epoch).unwrap()).unwrap();
    journal
        .create(&"task-1".into(), payload("initial"))
        .unwrap();
    if released {
        journal
            .apply(
                &"task-1".into(),
                0,
                "fixture-admit",
                LifecycleEvent::Admit {
                    node: "agent-1".into(),
                    conversation: ConversationId::new("composite-conversation").unwrap(),
                    instance: InstanceId::new("composite-resource").unwrap(),
                    host: HostId::new("fixture-service").unwrap(),
                },
            )
            .unwrap();
        let binding = ProviderBinding::new(
            "composite-binding".into(),
            "fixture-service".into(),
            "fixture-instance".into(),
            "fixture-config".into(),
            1,
            1,
            BTreeSet::from([Capability::OfferInput]),
            "fixture-credential-reference".into(),
            1,
        )
        .unwrap();
        journal
            .stage_launch(
                &"task-1".into(),
                &Placement::RemoteWorkspace("fixture/workspace".into()),
                &binding,
            )
            .unwrap();
        // Controlled aggregate fixtures only; these are not native SDK/process observations.
        for (cursor, fact) in [
            RuntimeFact::ResourceAvailable,
            RuntimeFact::TurnStarted {
                turn: TurnId::new("fixture-turn").unwrap(),
                trigger: Trigger::Input(InputId::new("initial").unwrap()),
            },
            RuntimeFact::ResourceReleased {
                problem_card: "composite-card".into(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            let aggregate = journal.load(&"task-1".into()).unwrap();
            journal
                .apply(
                    &"task-1".into(),
                    aggregate.revision(),
                    &format!("fixture-fact-{cursor}"),
                    LifecycleEvent::Runtime {
                        stamp: aggregate.execution().unwrap().clone(),
                        cursor: u64::try_from(cursor).unwrap() + 1,
                        fact,
                    },
                )
                .unwrap();
        }
    } else {
        journal
            .apply(
                &"task-1".into(),
                0,
                "fixture-wait",
                LifecycleEvent::Ask {
                    card: "composite-card".into(),
                },
            )
            .unwrap();
    }
    journal.commit().unwrap();
    (fixture, store)
}
fn answer(store: &mut DomainStore) -> CommandEnvelope {
    envelope(
        store,
        "composite-answer",
        Command::AnswerQuestion {
            interaction_id: "composite-card".into(),
            option_id: "continue".into(),
            inputs: BTreeMap::new(),
            attachments: vec![],
        },
    )
}
fn counts(conn: &Connection) -> [i64; 5] {
    [
        "lifecycle_events",
        "lifecycle_inputs",
        "semantic_receipts",
        "domain_outbox",
        "command_audit",
    ]
    .map(|table| {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    })
}
fn card(conn: &Connection) -> (Option<String>, bool) {
    conn.query_row(
        "SELECT answer,withdrawn FROM interactions WHERE id='composite-card'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap()
}
fn compose(
    store: &mut DomainStore,
    actor: &Actor,
    envelope: &CommandEnvelope,
    retire: bool,
) -> anyhow::Result<(Commit, alondra_store::Committed)> {
    let observer = Connection::open(store.conn.path().expect("fixture database path"))?;
    let before = counts(&observer);
    let cached = Arc::clone(&store.cached.as_ref().unwrap().state);
    let raw = rusqlite::Transaction::new_unchecked(&store.conn, TransactionBehavior::Immediate)?;
    let mut journal =
        LifecycleTransaction::attach(raw, AuthorityScope::new(envelope.data_epoch.clone())?)?;
    journal.put_input(&"task-1".into(), payload("reply"))?;
    let revision = journal.load(&"task-1".into())?.revision();
    journal.apply(
        &"task-1".into(),
        revision,
        "composite-lifecycle",
        if retire {
            LifecycleEvent::RetireReleasedExecution {
                problem_card: "composite-card".into(),
                input: InputId::new("reply")?,
            }
        } else {
            LifecycleEvent::Answer {
                card: "composite-card".into(),
                input: InputId::new("reply")?,
            }
        },
    )?;
    let prepared = stage(
        &store.conn,
        &store.cached,
        actor,
        envelope,
        None,
        false,
        Attached::None,
    )?;
    if !retire {
        journal.apply(
            &"task-1".into(),
            revision + 1,
            "composite-admission",
            LifecycleEvent::Admit {
                node: "agent-1".into(),
                conversation: ConversationId::new("composite-admitted-conversation")?,
                instance: InstanceId::new("composite-admitted-resource")?,
                host: HostId::new("fixture-service")?,
            },
        )?;
        let binding = ProviderBinding::new(
            "composite-binding".into(),
            "fixture-service".into(),
            "fixture-instance".into(),
            "fixture-config".into(),
            1,
            1,
            BTreeSet::from([Capability::OfferInput]),
            "fixture-credential-reference".into(),
            1,
        )
        .map_err(|_| anyhow::anyhow!("fixture binding invalid"))?;
        journal.stage_launch(
            &"task-1".into(),
            &Placement::RemoteWorkspace("fixture/workspace".into()),
            &binding,
        )?;
        journal.stage_input(&"task-1".into(), &InputId::new("reply")?)?;
        let execution = journal.load(&"task-1".into())?.execution().unwrap().clone();
        journal.reserve_owner(&execution, 0)?;
    }
    assert_eq!(
        counts(&observer),
        before,
        "another connection cannot see staged core or lifecycle writes"
    );
    assert!(Arc::ptr_eq(&store.cached.as_ref().unwrap().state, &cached));
    let committed = journal.commit().context("composite lifecycle SQL commit")?;
    assert_ne!(
        counts(&observer),
        before,
        "the real commit precedes returned sealed authority"
    );
    if prepared.remember {
        store.remember(Arc::clone(&prepared.commit.state), prepared.version);
    }
    Ok((prepared.commit, committed))
}
fn assert_reopened_journal(store: &DomainStore, expected: &[&str], has_execution: bool) {
    let epoch = repository::load(&store.conn).unwrap().data_epoch;
    let raw =
        rusqlite::Transaction::new_unchecked(&store.conn, TransactionBehavior::Immediate).unwrap();
    let journal = LifecycleTransaction::attach(raw, AuthorityScope::new(epoch).unwrap()).unwrap();
    let aggregate = journal.load(&"task-1".into()).unwrap();
    assert!(aggregate.cards().is_empty());
    assert_eq!(
        aggregate
            .pending_inputs()
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(aggregate.execution().is_some(), has_execution);
    journal.commit().unwrap();
}
#[test]
fn guarded_answer_and_lifecycle_reply_commit_on_the_same_connection() {
    let (fixture, mut store) = setup(false);
    let envelope = answer(&mut store);
    let before = counts(&store.conn);
    let (outcome, mut committed) = compose(&mut store, &user(), &envelope, false).unwrap();
    assert_eq!(committed.permits.len(), 1);
    assert_eq!(committed.owners.len(), 1);
    assert!(committed.attempts.is_empty());
    assert_eq!(
        committed.permits.pop().unwrap().input(),
        &InputId::new("initial").unwrap()
    );
    assert_eq!(committed.owners.pop().unwrap().generation(), 1);
    assert!(!outcome.replayed);
    assert!(card(&store.conn).0.is_some());
    assert_eq!(counts(&store.conn)[0], before[0] + 2);
    drop(store);
    let reopened = DomainStore::open(&fixture.path()).unwrap();
    assert!(card(&reopened.conn).0.is_some());
    assert_reopened_journal(&reopened, &["initial", "reply"], true);
}
#[test]
fn guarded_withdraw_and_released_resource_retirement_commit_together() {
    let (fixture, mut store) = setup(true);
    let envelope = envelope(
        &mut store,
        "composite-withdraw",
        Command::Withdraw {
            interaction_id: "composite-card".into(),
        },
    );
    compose(&mut store, &actor(), &envelope, true).unwrap();
    assert_eq!(card(&store.conn), (None, true));
    drop(store);
    let reopened = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(card(&reopened.conn), (None, true));
    assert_reopened_journal(&reopened, &["reply"], false);
}
#[test]
fn denied_guard_or_late_core_receipt_failure_rolls_back_lifecycle_and_business_rows() {
    for late_failure in [false, true] {
        let (fixture, mut store) = setup(false);
        let envelope = answer(&mut store);
        let before = counts(&store.conn);
        let cached = Arc::clone(&store.cached.as_ref().unwrap().state);
        if late_failure {
            store.conn.execute_batch("CREATE TRIGGER refuse_composite_receipt BEFORE INSERT ON semantic_receipts WHEN NEW.command_id='composite-answer' BEGIN SELECT RAISE(ABORT,'composite receipt refused'); END").unwrap();
        }
        let error = compose(
            &mut store,
            &if late_failure { user() } else { actor() },
            &envelope,
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains(if late_failure {
            "composite receipt refused"
        } else {
            "intended recipient"
        }));
        assert_eq!(counts(&store.conn), before);
        assert_eq!(card(&store.conn), (None, false));
        assert!(Arc::ptr_eq(&store.cached.as_ref().unwrap().state, &cached));
        drop(store);
        let mut reopened = DomainStore::open(&fixture.path()).unwrap();
        assert_eq!(reopened.load().unwrap(), cached);
        assert_eq!(counts(&reopened.conn), before);
    }
}
#[test]
fn deferred_sql_commit_failure_never_publishes_prepared_core_state() {
    let (fixture, mut store) = setup(false);
    let envelope = answer(&mut store);
    let before = counts(&store.conn);
    let cached = Arc::clone(&store.cached.as_ref().unwrap().state);
    store.conn.execute_batch("CREATE TABLE composite_parent(id INTEGER PRIMARY KEY); CREATE TABLE composite_child(parent INTEGER REFERENCES composite_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER fail_composite_commit AFTER UPDATE OF answer ON interactions WHEN NEW.id='composite-card' BEGIN INSERT INTO composite_child VALUES(404); END").unwrap();
    let error = compose(&mut store, &user(), &envelope, false).unwrap_err();
    assert!(error.to_string().contains("composite lifecycle SQL commit"));
    assert!(matches!(error.downcast_ref::<alondra_store::Error>(),
        Some(alondra_store::Error::Database(rusqlite::Error::SqliteFailure(code, _)))
            if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY));
    assert_eq!(counts(&store.conn), before);
    assert_eq!(card(&store.conn), (None, false));
    assert!(Arc::ptr_eq(&store.cached.as_ref().unwrap().state, &cached));
    assert!(store.conn.is_autocommit());
    drop(store);
    let mut reopened = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(reopened.load().unwrap(), cached);
    assert_eq!(counts(&reopened.conn), before);
}
