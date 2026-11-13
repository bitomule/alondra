use super::*;
use crate::tests::{Fixture, seeded};

/// The journal over a new transaction on the store's connection, as every write path opens it.
fn journal(store: &DomainStore) -> alondra_store::LifecycleTransaction<'_> {
    let transaction = rusqlite::Transaction::new_unchecked(&store.conn, TransactionBehavior::Immediate).unwrap();
    let scope = alondra_store::AuthorityScope::new(epoch_of(&store.conn).unwrap()).unwrap();
    alondra_store::LifecycleTransaction::attach(transaction, scope).unwrap()
}

fn user() -> Actor {
    Actor::User {
        user_id: "transaction-user".into(),
    }
}

fn information(store: &mut DomainStore) -> CommandEnvelope {
    let state = store.load().unwrap();
    CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: "transaction-information".into(),
        expected_revision: state.revision,
        command: Command::PublishInformation {
            interaction_id: "transaction-card".into(),
            scope: Scope::Task {
                task_id: "task-1".into(),
            },
            recipient: Recipient::User,
            body: "The work reached a checkpoint".into(),
            attachments: vec![],
            card: CardSpec::derived(CardKind::Know, "Read the checkpoint"),
        },
    }
}

fn rows(connection: &Connection, table: &str) -> i64 {
    assert!(
        [
            "interactions",
            "semantic_receipts",
            "local_command_results",
            "domain_outbox",
            "command_audit",
            "command_outer_witness"
        ]
        .contains(&table)
    );
    connection
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn counts(connection: &Connection) -> [i64; 5] {
    [
        "interactions",
        "semantic_receipts",
        "local_command_results",
        "domain_outbox",
        "command_audit",
    ]
    .map(|table| rows(connection, table))
}

#[test]
fn staged_command_does_not_commit_or_publish_and_drop_rolls_back_every_effect() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let envelope = information(&mut store);
    let before = store.load().unwrap();
    let reader = Connection::open(fixture.path()).unwrap();
    let before_counts = counts(&reader);
    let mut transaction = journal(&store);
    let staged = stage(
        &store.conn,
        &store.cached, &store.protected,
        &Actor::System,
        &envelope,
        Some(("phone", "transaction-op", &[7; 32])),
        false,
        Attached::None, &mut transaction,
    )
    .unwrap();
    assert!(!staged.commit.replayed);
    assert!(
        staged
            .commit
            .state
            .interactions
            .contains_key(&"transaction-card".into())
    );
    assert!(!store.conn.is_autocommit());
    assert!(Arc::ptr_eq(&store.cached.as_ref().unwrap().state, &before));
    assert_eq!(
        counts(&reader),
        before_counts,
        "a separate connection sees no staged card, receipt, audit or outbox"
    );
    assert_eq!(rows(&store.conn, "interactions"), before_counts[0] + 1);
    drop(staged);
    drop(transaction);
    assert_eq!(counts(&reader), before_counts);
    assert_eq!(store.load().unwrap(), before);
    drop(reader);
    drop(store);
    let mut reopened = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(reopened.load().unwrap(), before);
    assert_eq!(counts(&reopened.conn), before_counts);
}

#[test]
fn replay_transport_receipt_waits_for_the_callers_commit() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let envelope = information(&mut store);
    let first = store
        .execute_authenticated(
            &Actor::System,
            &envelope,
            Some(("phone", "first-op", &[1; 32])),
        )
        .unwrap();
    let reader = Connection::open(fixture.path()).unwrap();
    let before = counts(&reader);
    let cached = Arc::clone(&store.cached.as_ref().unwrap().state);
    let mut transaction = journal(&store);
    let staged = stage(
        &store.conn,
        &store.cached, &store.protected,
        &Actor::System,
        &envelope,
        Some(("phone", "replayed-op", &[2; 32])),
        false,
        Attached::None, &mut transaction,
    )
    .unwrap();
    assert!(staged.commit.replayed);
    assert_eq!(staged.commit.outcome, first.outcome);
    assert_eq!(rows(&store.conn, "local_command_results"), before[2] + 1);
    assert_eq!(counts(&reader), before);
    assert!(!store.conn.is_autocommit());
    assert!(Arc::ptr_eq(&store.cached.as_ref().unwrap().state, &cached));
    drop(transaction);
    assert_eq!(counts(&reader), before);
    assert_eq!(store.load().unwrap(), first.state);
    let mut transaction = journal(&store);
    let transported = stage(
        &store.conn,
        &store.cached, &store.protected,
        &Actor::System,
        &envelope,
        Some(("phone", "first-op", &[1; 32])),
        false,
        Attached::None, &mut transaction,
    )
    .unwrap();
    assert!(transported.commit.replayed);
    assert_eq!(transported.commit.outcome, first.outcome);
    assert!(
        !transported.remember,
        "an unchanged transport replay retains the cache hit"
    );
    assert!(!store.conn.is_autocommit());
    transaction.commit().unwrap();
    assert_eq!(counts(&reader), before);
}

#[test]
fn rejected_command_leaves_the_callers_transaction_owned_and_rollbackable() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let mut envelope = information(&mut store);
    envelope.expected_revision += 1;
    store
        .conn
        .execute_batch("CREATE TABLE command_outer_witness(value INTEGER NOT NULL)")
        .unwrap();
    let reader = Connection::open(fixture.path()).unwrap();
    let before = counts(&reader);
    let mut transaction = journal(&store);
    store
        .conn
        .execute("INSERT INTO command_outer_witness VALUES(1)", [])
        .unwrap();
    assert!(
        matches!(stage(&store.conn, &store.cached, &store.protected, &user(), &envelope, None, false, Attached::None, &mut transaction), Err(StoreError::Domain(error)) if error.code == "stale_revision")
    );
    assert!(!store.conn.is_autocommit());
    assert_eq!(rows(&store.conn, "command_outer_witness"), 1);
    assert_eq!(rows(&reader, "command_outer_witness"), 0);
    drop(transaction);
    assert_eq!(rows(&reader, "command_outer_witness"), 0);
    assert_eq!(counts(&reader), before);
}

#[test]
fn receipt_failure_after_core_writes_cannot_escape_the_outer_rollback() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let envelope = information(&mut store);
    store.conn.execute_batch("CREATE TRIGGER reject_staged_receipt BEFORE INSERT ON semantic_receipts BEGIN SELECT RAISE(ABORT,'fixture receipt refusal'); END").unwrap();
    let reader = Connection::open(fixture.path()).unwrap();
    let before = counts(&reader);
    let cached = Arc::clone(&store.cached.as_ref().unwrap().state);
    let mut transaction = journal(&store);
    let error = stage(
        &store.conn,
        &store.cached, &store.protected,
        &Actor::System,
        &envelope,
        None,
        false,
        Attached::None, &mut transaction,
    )
    .unwrap_err();
    assert!(error.to_string().contains("fixture receipt refusal"));
    assert_eq!(
        rows(&store.conn, "interactions"),
        before[0] + 1,
        "the refusal occurs after actual core persistence"
    );
    assert!(rows(&store.conn, "domain_outbox") > before[3]);
    assert!(!store.conn.is_autocommit());
    assert_eq!(counts(&reader), before);
    assert!(Arc::ptr_eq(&store.cached.as_ref().unwrap().state, &cached));
    drop(transaction);
    assert_eq!(counts(&reader), before);
    assert_eq!(store.load().unwrap(), cached);
}

#[test]
fn failed_nested_begin_does_not_commit_or_destroy_the_existing_transaction() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let envelope = information(&mut store);
    let reader = Connection::open(fixture.path()).unwrap();
    let before = counts(&reader);
    let mut transaction = journal(&store);
    assert!(
        rusqlite::Transaction::new_unchecked(&store.conn, TransactionBehavior::Immediate).is_err()
    );
    assert!(!store.conn.is_autocommit());
    let staged = stage(
        &store.conn,
        &store.cached, &store.protected,
        &Actor::System,
        &envelope,
        None,
        false,
        Attached::None, &mut transaction,
    )
    .unwrap();
    assert!(!staged.commit.replayed);
    assert_eq!(counts(&reader), before);
    drop(transaction);
    assert_eq!(counts(&reader), before);
}

#[test]
fn multiple_guarded_commands_commit_together_and_publish_only_the_final_state() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let first_envelope = information(&mut store);
    let before = store.load().unwrap();
    let reader = Connection::open(fixture.path()).unwrap();
    let before_counts = counts(&reader);
    let mut transaction = journal(&store);
    let first = stage(
        &store.conn,
        &store.cached, &store.protected,
        &Actor::System,
        &first_envelope,
        None,
        false,
        Attached::None, &mut transaction,
    )
    .unwrap();
    let mut second_envelope = first_envelope.clone();
    second_envelope.command_id = "second-transaction-information".into();
    second_envelope.expected_revision = first.commit.state.revision;
    if let Command::PublishInformation {
        interaction_id,
        body,
        ..
    } = &mut second_envelope.command
    {
        *interaction_id = "second-transaction-card".into();
        *body = "The next checkpoint".into();
    } else {
        panic!("the fixture stages information commands");
    }
    let second = stage(
        &store.conn,
        &store.cached, &store.protected,
        &Actor::System,
        &second_envelope,
        None,
        false,
        Attached::None, &mut transaction,
    )
    .unwrap();
    assert_eq!(second.commit.outcome.revision, before.revision + 2);
    assert!(
        second
            .commit
            .state
            .interactions
            .contains_key(&"transaction-card".into())
    );
    assert!(
        second
            .commit
            .state
            .interactions
            .contains_key(&"second-transaction-card".into())
    );
    assert_eq!(counts(&reader), before_counts);
    assert!(Arc::ptr_eq(&store.cached.as_ref().unwrap().state, &before));
    transaction.commit().unwrap();
    store.remember(Arc::clone(&second.commit.state), second.version);
    let after = counts(&reader);
    assert_eq!(after[0], before_counts[0] + 2);
    assert_eq!(after[1], before_counts[1] + 2);
    assert_eq!(after[3], before_counts[3] + 2);
    assert_eq!(after[4], before_counts[4] + 2);
    assert_eq!(store.load().unwrap(), second.commit.state);
    drop(reader);
    drop(store);
    let mut reopened = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(reopened.load().unwrap(), second.commit.state);
}

#[test]
fn deferred_commit_failure_rolls_back_staged_core_effects_without_publication() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let envelope = information(&mut store);
    let before = store.load().unwrap();
    store.conn.execute_batch("CREATE TABLE command_commit_witness(parent TEXT REFERENCES requests(id) DEFERRABLE INITIALLY DEFERRED)").unwrap();
    let reader = Connection::open(fixture.path()).unwrap();
    let before_counts = counts(&reader);
    let mut transaction = journal(&store);
    store
        .conn
        .execute(
            "INSERT INTO command_commit_witness VALUES('missing-request')",
            [],
        )
        .unwrap();
    let staged = stage(
        &store.conn,
        &store.cached, &store.protected,
        &Actor::System,
        &envelope,
        None,
        false,
        Attached::None, &mut transaction,
    )
    .unwrap();
    assert!(
        staged
            .commit
            .state
            .interactions
            .contains_key(&"transaction-card".into())
    );
    assert_eq!(counts(&reader), before_counts);
    let error = transaction.commit().unwrap_err();
    assert!(
        matches!(error, alondra_store::Error::Database(rusqlite::Error::SqliteFailure(ref failure, _)) if failure.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY)
    );
    assert!(store.conn.is_autocommit());
    assert!(Arc::ptr_eq(&store.cached.as_ref().unwrap().state, &before));
    assert_eq!(counts(&reader), before_counts);
    assert_eq!(store.load().unwrap(), before);
}

#[test]
fn a_new_command_that_changes_nothing_is_refused_and_a_repeated_one_answers_its_first_effect() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let information = information(&mut store);
    store.execute_authenticated(&Actor::System, &information, None).unwrap();
    let read = |store: &mut DomainStore, id: &str| {
        let state = store.load().unwrap();
        CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(),
            expected_revision: state.revision, command: Command::MarkRead { interaction_id: "transaction-card".into() },
        }
    };
    let sent = read(&mut store, "read-1");
    let first = store.execute_authenticated(&user(), &sent, None).unwrap();
    assert!(!first.replayed && first.outcome.changed);
    assert!(!first.outcome.effect.is_empty(), "the first read says what it changed");
    let receipts = rows(&store.conn, "semantic_receipts");
    let again = store.execute_authenticated(&user(), &sent, None).unwrap();
    assert!(again.replayed, "the same command is answered as already recorded");
    assert_eq!(again.outcome.effect, first.outcome.effect, "with the effect it had the first time");
    assert_eq!(rows(&store.conn, "semantic_receipts"), receipts);
    let before = store.load().unwrap();
    let another = read(&mut store, "read-2");
    let Err(StoreError::Domain(refused)) = store.execute_authenticated(&user(), &another, None) else { panic!("a new read of a read card was accepted") };
    assert_eq!(refused.code, "no_effect", "{refused}");
    assert!(refused.message.starts_with("mark_read changed nothing"), "{}", refused.message);
    assert_eq!(rows(&store.conn, "semantic_receipts"), receipts, "a refused command leaves no receipt");
    assert_eq!(store.load().unwrap(), before);
}
