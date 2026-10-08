use super::*;
use std::collections::BTreeSet;
use std::os::unix::fs::DirBuilderExt;
use std::{fs, path::PathBuf};
pub(super) struct Fixture(pub(super) crate::test_support::Directory);
impl Fixture {
    pub(super) fn new() -> Self {
        let p = crate::test_support::Directory::new();
        fs::DirBuilder::new().mode(0o700).create(&p).unwrap();
        Self(p)
    }
    pub(super) fn path(&self) -> PathBuf {
        self.0.join("domain.sqlite")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn attachment(id: &str) -> AttachmentRef {
    AttachmentRef {
        id: id.into(),
        name: format!("{id}.png"),
        content_digest: "a".repeat(64),
        content_reference: format!("file:///synthetic/{id}.png"),
        bytes: 12,
    }
}
fn user() -> Actor {
    Actor::User {
        user_id: "user-1".into(),
    }
}
fn agent() -> Actor {
    Actor::Agent {
        agent_id: "agent-1".into(),
    }
}
fn envelope(store: &mut DomainStore, id: &str, command: Command) -> CommandEnvelope {
    let s = store.load().unwrap();
    CommandEnvelope {
        protocol_version: 2,
        data_epoch: s.data_epoch.clone(),
        command_id: id.into(),
        expected_revision: s.revision,
        command,
    }
}
fn execute(store: &mut DomainStore, actor: &Actor, id: &str, command: Command) -> Commit {
    let e = envelope(store, id, command);
    let committed = store.execute_authenticated(actor, &e, None).unwrap();
    assert_eq!(
        store.load().unwrap(),
        committed.state,
        "SQL roundtrip for {id}"
    );
    committed
}
pub(super) fn seeded(store: &mut DomainStore) {
    execute(
        store,
        &Actor::Orchestrator { generation: 0 },
        "request",
        Command::CreateRequest {
            request_id: "request-1".into(),
            body: "Do useful work".into(), auto_merge: false, maintenance: false, message: false,
        },
    );
    execute(
        store,
        &Actor::System,
        "agent",
        Command::RegisterAgent {
            agent_id: "agent-1".into(),
            name: "Fixture".into(),
        },
    );
    execute(
        store,
        &user(),
        "task",
        Command::CreateTask {
            task_id: "task-1".into(),
            request_id: "request-1".into(),
            title: "Goal".into(),
            agent_id: Some("agent-1".into()),
            review_prs: true,
            dependencies: BTreeSet::new(),
            stack: None,
        },
    );
}
fn present(store: &mut DomainStore) {
    execute(
        store,
        &agent(),
        "present",
        Command::PresentResult {
            task_id: "task-1".into(),
            interaction_id: "delivery-1".into(),
            body: "Finished the answer".into(),
            targets: vec![DeliveryTarget::Text {
                text: "The answer".into(),
            }],
            is_final: true,
            nothing_to_review: false,
            attachments: vec![], card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
        },
    );
}
fn acceptance(store: &mut DomainStore) -> CommandEnvelope {
    envelope(
        store,
        "accept",
        Command::AcceptDelivery {
            presentation_id: "delivery-1".into(),
            targets: vec![DeliveryTarget::Text {
                text: "The answer".into(),
            }],
        },
    )
}
fn feedback(store: &mut DomainStore) -> CommandEnvelope {
    envelope(
        store,
        "feedback",
        Command::SubmitFeedback {
            attachments: vec![],
            presentation_id: "delivery-1".into(),
            targets: vec![DeliveryTarget::Text {
                text: "The answer".into(),
            }],
            body: "Shorter, please".into(),
            instruction: Instruction::Changes,
        },
    )
}
fn count(store: &DomainStore, table: &str) -> i64 {
    store
        .conn
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn reads_share_one_snapshot_until_a_commit_replaces_it() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let first = store.load().unwrap();
    assert!(Arc::ptr_eq(&first, &store.load().unwrap()), "a read without a write copies nothing");
    let committed = execute(
        &mut store,
        &Actor::System,
        "info",
        Command::PublishInformation {
            interaction_id: "info-1".into(),
            scope: Scope::Task { task_id: "task-1".into() },
            recipient: Recipient::User,
            body: "Progress".into(),
            attachments: vec![],
            card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Know, "Read the update"),
        },
    );
    let after = store.load().unwrap();
    assert!(Arc::ptr_eq(&after, &committed.state), "readers get the committed state itself");
    assert_eq!(after.revision, first.revision + 1);
    assert!(!first.interactions.contains_key(&InteractionId::from("info-1")), "an earlier snapshot never changes");
}

#[test]
fn sql_restart_roundtrip_receipts_and_read_has_no_communication() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    execute(
        &mut store,
        &Actor::System,
        "info",
        Command::PublishInformation {
            interaction_id: "info-1".into(),
            scope: Scope::Task {
                task_id: "task-1".into(),
            },
            recipient: Recipient::User,
            body: "Progress".into(),
            attachments: vec![attachment("info-file")], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Know, "Read the update"),
        },
    );
    let task_before = store.load().unwrap().tasks[&TaskId::from("task-1")].clone();
    execute(
        &mut store,
        &user(),
        "read",
        Command::MarkRead {
            interaction_id: "info-1".into(),
        },
    );
    assert_eq!(
        store.load().unwrap().tasks[&TaskId::from("task-1")],
        task_before
    );
    assert_eq!(count(&store, "domain_messages"), 1);
    assert_eq!(count(&store, "domain_message_user_targets"), 1);
    assert_eq!(count(&store, "domain_outbox"), 1);
    assert_eq!(count(&store, "delivery_attempts"), 0);
    present(&mut store);
    let e = feedback(&mut store);
    let committed = store
        .execute_authenticated(&user(), &e, Some(("phone", "request-x", &[1; 32])))
        .unwrap();
    assert_eq!(count(&store, "domain_messages"), 3);
    assert_eq!(count(&store, "domain_outbox"), 3);
    let audit_count = count(&store, "command_audit");
    let semantic_count = count(&store, "semantic_receipts");
    let transport_count = count(&store, "local_command_results");
    let pending = store.pending_messages().unwrap();
    let expected = store.load().unwrap();
    drop(store);
    let mut restarted = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(restarted.load().unwrap(), expected);
    let recovered = restarted
        .execute_authenticated(&user(), &e, Some(("phone", "request-x", &[1; 32])))
        .unwrap();
    assert!(recovered.replayed);
    assert_eq!(recovered.outcome, committed.outcome);
    assert_eq!(restarted.pending_messages().unwrap(), pending);
    assert_eq!(count(&restarted, "command_audit"), audit_count);
    assert_eq!(count(&restarted, "semantic_receipts"), semantic_count);
    assert_eq!(count(&restarted, "local_command_results"), transport_count);
    assert_eq!(count(&restarted, "domain_messages"), 3);
    let reciphered = restarted
        .execute_authenticated(&user(), &e, Some(("phone", "request-y", &[2; 32])))
        .unwrap();
    assert!(reciphered.replayed);
    assert_eq!(restarted.pending_messages().unwrap(), pending);
    assert_eq!(count(&restarted, "command_audit"), audit_count);
    assert_eq!(count(&restarted, "semantic_receipts"), semantic_count);
    assert_eq!(count(&restarted, "local_command_results"), transport_count + 1);
    assert_eq!(count(&restarted, "domain_messages"), 3);
    assert!(
        matches!(restarted.execute_authenticated(&user(),&e,Some(("phone","request-x",&[3;32]))),Err(StoreError::Domain(e)) if e.code=="idempotency_conflict")
    );
    let mut wrong = e.clone();
    wrong.command = Command::MarkRead {
        interaction_id: "info-1".into(),
    };
    assert!(
        matches!(restarted.execute_authenticated(&user(),&wrong,None),Err(StoreError::Domain(e)) if e.code=="idempotency_conflict")
    );
    restarted.integrity_check().unwrap();
}

#[test]
fn receipt_failure_rolls_back_decision_audit_outbox_and_revision() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    present(&mut store);
    let before = store.load().unwrap();
    let audits = count(&store, "command_audit");
    let receipts = count(&store, "semantic_receipts");
    let transport_receipts = count(&store, "local_command_results");
    let pending = store.pending_messages().unwrap();
    let e = feedback(&mut store);
    store.conn.execute_batch("CREATE TRIGGER fail_receipt BEFORE INSERT ON local_command_results BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;").unwrap();
    assert!(matches!(
        store.execute_authenticated(&user(), &e, Some(("phone", "op", &[1; 32]))),
        Err(StoreError::Storage(_))
    ));
    assert_eq!(store.load().unwrap(), before);
    assert_eq!(count(&store, "domain_messages"), 1);
    assert_eq!(count(&store, "domain_outbox"), 1);
    assert_eq!(count(&store, "command_audit"), audits);
    assert_eq!(count(&store, "semantic_receipts"), receipts);
    assert_eq!(count(&store, "local_command_results"), transport_receipts);
    assert_eq!(count(&store, "delivery_attempts"), 0);
    assert_eq!(store.pending_messages().unwrap(), pending);
    store
        .conn
        .execute_batch("DROP TRIGGER fail_receipt")
        .unwrap();
    drop(store);
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(store.load().unwrap(), before);
    assert_eq!(store.pending_messages().unwrap(), pending);
    store
        .execute_authenticated(&user(), &e, Some(("phone", "op", &[1; 32])))
        .unwrap();
    assert_eq!(count(&store, "domain_messages"), 2);
    assert_eq!(count(&store, "command_audit"), audits + 1);
    assert_eq!(count(&store, "semantic_receipts"), receipts + 1);
    assert_eq!(count(&store, "local_command_results"), transport_receipts + 1);
}

#[test]
fn replay_returns_the_first_outcome() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let mut e = envelope(
        &mut store,
        "producer-info",
        Command::PublishInformation {
            attachments: vec![],
            interaction_id: "info-1".into(),
            scope: Scope::Task { task_id: "task-1".into() },
            recipient: Recipient::User,
            body: "Progress".into(), card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Know, "Read the update"),
        },
    );
    e.expected_revision = 0;
    let revision = store.load().unwrap().revision;
    let first = store.execute_filling_revision(&Actor::System, &e).unwrap();
    assert!(!first.replayed && first.outcome.changed);
    assert_eq!(first.outcome.revision, revision + 1);
    let replay = store.execute_filling_revision(&Actor::System, &e).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.outcome, first.outcome);
    assert_eq!(store.load().unwrap().revision, revision + 1);
    let mut user_zero = envelope(
        &mut store,
        "user-zero",
        Command::CreateRequest { request_id: "request-2".into(), body: "More".into(), auto_merge: false, maintenance: false, message: false },
    );
    user_zero.expected_revision = 0;
    assert!(matches!(
        store.execute_authenticated(&user(), &user_zero, None),
        Err(StoreError::Domain(error)) if error.code == "stale_revision"
    ), "only producers get a server-filled revision");
}

#[test]
fn two_connections_compete_by_revision_and_domain_rejection_writes_nothing() {
    let fixture = Fixture::new();
    let mut first = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut first);
    present(&mut first);
    let mut second = DomainStore::open(&fixture.path()).unwrap();
    let e = feedback(&mut first);
    let mut competing = e.clone();
    competing.command_id = "other-command".into();
    first.execute_authenticated(&user(), &e, None).unwrap();
    let before = second.load().unwrap();
    let receipts = count(&second, "semantic_receipts");
    assert!(
        matches!(second.execute_authenticated(&Actor::User{user_id:"other-user".into()},&competing,None),Err(StoreError::Domain(e)) if e.code=="stale_revision")
    );
    assert_eq!(second.load().unwrap(), before);
    assert_eq!(count(&second, "semantic_receipts"), receipts);
    assert_eq!(count(&second, "domain_messages"), 2);
    let mut old_epoch = e.clone();
    old_epoch.data_epoch = "retired-epoch".into();
    assert!(
        matches!(second.execute_authenticated(&user(),&old_epoch,None),Err(StoreError::Domain(e)) if e.code=="stale_epoch")
    );
}

#[test]
fn lifetime_pr_sql_constraints_reject_second_pr_reparent_delete_and_bad_targets() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    execute(
        &mut store,
        &agent(),
        "pr",
        Command::RegisterPr {
            task_id: "task-1".into(),
            pr_id: "pr-1".into(),
            host: "github.com".into(),
            repository: "owner/repository".into(),
            number: 1,
            head: "aaa".into(),
            base: "main".into(),
            allowed_methods: vec![MergeMethod::Squash],
        },
    );
    store
        .conn
        .execute(
            "UPDATE pull_requests SET state='closed' WHERE id='pr-1'",
            [],
        )
        .unwrap();
    assert!(
        store
            .conn
            .execute("DELETE FROM pull_requests WHERE id='pr-1'", [])
            .is_err()
    );
    assert!(
        store
            .conn
            .execute(
                "UPDATE pull_requests SET task_id='other-task' WHERE id='pr-1'",
                []
            )
            .is_err()
    );
    assert!(store.conn.execute("INSERT INTO pull_requests SELECT 'pr-2',task_id,host,repository,2,head,base,state,merged_head,allowed_methods FROM pull_requests WHERE id='pr-1'",[]).is_err());
    assert!(
        store
            .conn
            .execute(
                "UPDATE tasks SET final_presentation='missing-presentation' WHERE id='task-1'",
                []
            )
            .is_err()
    );
    assert!(
        store
            .conn
            .execute("INSERT INTO interaction_reads VALUES('missing','user')", [])
            .is_err()
    );
    store.integrity_check().unwrap();
}

#[test]
fn claimed_delivery_restart_is_uncertain_until_explicit_reconciliation() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    present(&mut store);
    let e = feedback(&mut store);
    store.execute_authenticated(&user(), &e, None).unwrap();
    let pending = store.pending_messages().unwrap();
    assert_eq!(pending.len(), 2);
    let agent_message = pending.iter().find(|m| m.recipient_kind == "agent").unwrap();
    let claim = store.claim_message(agent_message.id).unwrap().unwrap();
    assert!(store.claim_message(agent_message.id).unwrap().is_none());
    drop(store);
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(store.pending_messages().unwrap().len(), 1);
    let evidence = store.delivery_evidence().unwrap();
    assert_eq!(evidence[0].state, "uncertain");
    assert_eq!(evidence[0].version, 2);
    assert!(
        !store
            .finish_message(
                claim.attempt_id,
                claim.version,
                alondra_core::delivery::State::Handed,
                None
            )
            .unwrap()
    );
    assert!(!store.resolve_uncertain(claim.attempt_id, 1, true).unwrap());
    assert!(store.resolve_uncertain(claim.attempt_id, 2, true).unwrap());
    let retried = store.claim_message(agent_message.id).unwrap().unwrap();
    assert_ne!(claim.attempt_id, retried.attempt_id);
    assert!(
        store
            .finish_message(
                retried.attempt_id,
                retried.version,
                alondra_core::delivery::State::Handed,
                None
            )
            .unwrap()
    );
    assert_eq!(store.pending_messages().unwrap().len(), 1);
    let evidence = store.delivery_evidence().unwrap();
    assert_eq!(evidence.len(), 2);
    assert_eq!(evidence[1].state, "handed");
}

#[test]
fn encrypted_resolution_has_a_durable_receipt_and_fences_stale_or_changed_decisions() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    present(&mut store);
    let e = feedback(&mut store);
    store.execute_authenticated(&user(), &e, None).unwrap();
    let agent_message = store.pending_messages().unwrap().into_iter()
        .find(|message| message.recipient_kind == "agent").unwrap();
    let claim = store.claim_message(agent_message.id).unwrap().unwrap();
    drop(store);
    let mut restarted = DomainStore::open(&fixture.path()).unwrap();
    let epoch = restarted.load().unwrap().data_epoch.0.clone();
    let page = restarted.delivery_evidence_page(0).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].state, "uncertain");
    assert_eq!(page[0].recipient_kind, "agent");
    assert_eq!(page[0].current_state, "uncertain");
    assert_eq!(page[0].version, 2);
    assert!(restarted.delivery_evidence_page(claim.attempt_id).unwrap().is_empty());
    let digest = [17u8; 32];
    let different = [19u8; 32];
    let first = restarted.resolve_uncertain_authenticated(claim.attempt_id, 2, &epoch, true,
        ("phone-a", "work-a", &digest)).unwrap();
    assert_eq!(first["state"], "retryable");
    assert_eq!(first["version"], 3);
    assert_eq!(first, restarted.resolve_uncertain_authenticated(claim.attempt_id, 2, &epoch,
        true, ("phone-a", "work-a", &digest)).unwrap());
    assert!(matches!(restarted.resolve_uncertain_authenticated(claim.attempt_id, 2, &epoch,
        false, ("phone-a", "work-a", &different)), Err(StoreError::Domain(error))
        if error.code == "idempotency_conflict"));
    assert!(matches!(restarted.resolve_uncertain_authenticated(claim.attempt_id, 2, &epoch,
        false, ("phone-a", "work-b", &different)), Err(StoreError::Domain(error))
        if error.code == "delivery_uncertain"));
    let followup = restarted.claim_message(agent_message.id).unwrap().unwrap();
    drop(restarted);
    let mut restarted = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(restarted.delivery_evidence_page(followup.attempt_id - 1).unwrap()[0].state,
        "uncertain");
    let discarded = restarted.resolve_uncertain_authenticated(followup.attempt_id, 2, &epoch,
        false, ("phone-a", "work-c", &[23u8; 32])).unwrap();
    assert_eq!(discarded["state"], "discarded");
    assert!(restarted.pending_messages().unwrap().iter().all(|m| m.id != agent_message.id));
}

#[test]
fn a_legacy_writer_is_rejected_and_a_current_store_keeps_its_rows() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let expected = store.load().unwrap();
    assert!(crate::store::Store::open(&fixture.path()).is_err());
    drop(store);
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(store.load().unwrap(), expected);
    assert_eq!(store.conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), SCHEMA_VERSION);
    assert!(!fixture.0.read_dir().unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains(".old-")));
}

#[test]
fn an_unsupported_schema_is_refused_without_replacing_the_database() {
    use std::os::unix::fs::MetadataExt;
    for journal in ["wal", "delete"] {
        for other in [1, 7, 8, SCHEMA_VERSION + 1] {
            let fixture = Fixture::new();
            let mut store = DomainStore::open(&fixture.path()).unwrap();
            let command = envelope(&mut store, "old", Command::CreateRequest {
                request_id: "old".into(), body: "Made before".into(),
                auto_merge: false, maintenance: false, message: false,
            });
            store.execute_authenticated(&user(), &command, None).unwrap();
            let expected = store.load().unwrap();
            store.conn.pragma_update(None, "user_version", other).unwrap();
            store.conn.pragma_update(None, "journal_mode", journal).unwrap();
            drop(store);
            let before = fs::metadata(fixture.path()).unwrap();
            let bytes = fs::read(fixture.path()).unwrap();
            let error = match DomainStore::open(&fixture.path()) {
                Ok(_) => panic!("schema {other} must not activate an empty replacement"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("unsupported domain database schema"), "{error:#}");
            let after = fs::metadata(fixture.path()).unwrap();
            assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
            assert_eq!(fs::read(fixture.path()).unwrap(), bytes);
            assert!(!fixture.0.read_dir().unwrap().any(|entry|
                entry.unwrap().file_name().to_string_lossy().contains(".old-")));
            let preserved = Connection::open(&fixture.path()).unwrap();
            assert_eq!(preserved.pragma_query_value(None, "user_version", |row|
                row.get::<_, i64>(0)).unwrap(), other);
            assert_eq!(preserved.query_row("SELECT body FROM requests", [], |row|
                row.get::<_, String>(0)).unwrap(), "Made before");
            preserved.pragma_update(None, "user_version", SCHEMA_VERSION).unwrap();
            drop(preserved);
            let mut reopened = DomainStore::open(&fixture.path()).unwrap();
            assert_eq!(reopened.load().unwrap(), expected);
            let retry = reopened.execute_authenticated(&user(), &command, None).unwrap();
            assert_eq!(retry.state, expected, "the original semantic receipt survives refusal");
        }
    }
}


#[test]
#[ignore = "internal isolated crash fixture"]
fn crash_domain_transaction_fixture() {
    let Some(path) = std::env::var_os("ALONDRA_CRASH_DOMAIN") else { return; };
    let connection = Connection::open(PathBuf::from(path)).unwrap();
    connection.pragma_update(None, "cache_size", 1).unwrap();
    connection.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    if std::env::var_os("ALONDRA_UNCOMMITTED_SCHEMA").is_some() {
        connection.pragma_update(None, "user_version", 99).unwrap();
    }
    connection.execute_batch("UPDATE requests SET body='Uncommitted';
        UPDATE domain_meta SET data_epoch='uncommitted',revision=revision+100;
        DELETE FROM semantic_receipts;
        INSERT INTO requests(id,body,closed) VALUES('uncommitted',hex(zeroblob(524288)),0);").unwrap();
    unsafe { libc::_exit(0); }
}

#[test]
fn crash_recovery_preserves_committed_work_and_receipts_before_schema_admission() {
    use std::os::unix::fs::MetadataExt;
    for journal in ["WAL", "DELETE"] {
        for (committed_schema, uncommitted_schema) in [(SCHEMA_VERSION, false), (99, false), (SCHEMA_VERSION, true)] {
            let fixture = Fixture::new();
            let mut store = DomainStore::open(&fixture.path()).unwrap();
            seeded(&mut store);
            let command = envelope(&mut store, "retained-command", Command::CreateRequest {
                request_id: "retained".into(), body: "Committed work".into(),
                auto_merge: false, maintenance: false, message: false,
            });
            let committed = store.execute_authenticated(&user(), &command, None).unwrap();
            let expected = committed.state;
            let receipt_count: i64 = store.conn.query_row("SELECT count(*) FROM semantic_receipts", [], |r| r.get(0)).unwrap();
            store.conn.pragma_update(None, "user_version", committed_schema).unwrap();
            store.conn.pragma_update(None, "journal_mode", journal).unwrap();
            drop(store);
            let before = fs::metadata(fixture.path()).unwrap();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child.args(["--ignored", "--exact", "domain_store::tests::crash_domain_transaction_fixture"])
                .env("ALONDRA_CRASH_DOMAIN", fixture.path()).env("HOME", &fixture.0)
                .env_remove("CLAUDE_HOME").env_remove("CLAUDE_CONFIG_DIR")
                .env_remove("ALONDRA_UNCOMMITTED_SCHEMA");
            if uncommitted_schema { child.env("ALONDRA_UNCOMMITTED_SCHEMA", "1"); }
            let child = child.output().unwrap();
            assert!(child.status.success(), "{}", String::from_utf8_lossy(&child.stderr));
            let sidecar = fixture.path().with_file_name(if journal == "DELETE" { "domain.sqlite-journal" } else { "domain.sqlite-wal" });
            assert!(fs::metadata(&sidecar).unwrap().len() > 0, "crash did not retain its transaction journal");
            let opened = DomainStore::open(&fixture.path());
            if committed_schema == SCHEMA_VERSION {
                let mut recovered = opened.unwrap();
                assert_eq!(recovered.load().unwrap(), expected, "compatible stores must recover automatically");
                drop(recovered);
            } else {
                let error = opened.err().expect("unsupported committed schema activated a store");
                assert!(error.to_string().contains("unsupported domain database schema 99"), "{error:#}");
            }
            let after = fs::metadata(fixture.path()).unwrap();
            assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
            assert!(!fixture.0.read_dir().unwrap().any(|entry| entry.unwrap().file_name().to_string_lossy().contains(".old-")));
            let preserved = Connection::open(&fixture.path()).unwrap();
            assert_eq!(preserved.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), committed_schema);
            assert_eq!(preserved.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0)).unwrap(), "ok");
            assert_eq!(preserved.query_row("SELECT body FROM requests WHERE id='retained'", [], |r| r.get::<_, String>(0)).unwrap(), "Committed work");
            assert_eq!(preserved.query_row("SELECT count(*) FROM requests WHERE id='uncommitted'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
            assert_eq!(preserved.query_row("SELECT count(*) FROM semantic_receipts", [], |r| r.get::<_, i64>(0)).unwrap(), receipt_count);
            preserved.pragma_update(None, "user_version", SCHEMA_VERSION).unwrap();
            drop(preserved);
            let mut recovered = DomainStore::open(&fixture.path()).unwrap();
            assert_eq!(recovered.load().unwrap(), expected);
            let retry = recovered.execute_authenticated(&user(), &command, None).unwrap();
            assert_eq!(retry.state, expected, "recovery retained the original command receipt");
            assert_eq!(recovered.load().unwrap().revision, expected.revision);
        }
    }
}

#[test]
fn schedule_schema_enforces_run_relations_and_survives_restart() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let before = store.load().unwrap();
    let mut upgraded = store;
    assert_eq!(upgraded.load().unwrap(), before);
    assert_eq!(upgraded.conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), SCHEMA_VERSION);
    upgraded.conn.execute_batch("INSERT INTO schedules
        (id,title,text,project,options,disallowed_tools,frequency,hour,minute,weekday,interval,start_at,enabled,next_run,revision,deleted_at)
        VALUES ('schedule-1','Daily work','Do work',NULL,'{}','[]','daily',9,30,7,0,-1,1,2000000,2,NULL);
        INSERT INTO requests(id,body,closed) VALUES ('request-2','Older scheduled work',0);
        INSERT INTO schedule_request_tools(request_id,patterns) VALUES ('request-2','[\"Bash\"]');").unwrap();
    assert!(upgraded.conn.execute("UPDATE schedules SET enabled=0 WHERE id='schedule-1'", []).is_err(),
        "paused schedules cannot retain a due time");
    assert!(upgraded.conn.execute_batch("INSERT INTO schedules SELECT 'bad-weekly',title,text,project,options,disallowed_tools,'weekly',hour,minute,NULL,interval,start_at,enabled,next_run,revision,deleted_at FROM schedules WHERE id='schedule-1'").is_err());
    assert!(upgraded.conn.execute_batch("INSERT INTO schedules SELECT 'bad-once',title,text,project,options,disallowed_tools,'once',hour,minute,weekday,interval,NULL,enabled,next_run,revision,deleted_at FROM schedules WHERE id='schedule-1'").is_err());
    assert!(upgraded.conn.execute_batch("INSERT INTO schedules SELECT 'bad-interval',title,text,project,options,disallowed_tools,'every_hours',hour,minute,weekday,NULL,1000,enabled,next_run,revision,deleted_at FROM schedules WHERE id='schedule-1'").is_err());
    let launch = serde_json::to_string(&ScheduleLaunch {
        intent: ScheduleLaunchIntent {
            name: "scheduled/run-1".into(), cwd: "/synthetic/Projects".into(),
            prompt: "Do work".into(),
            options_json: serde_json::to_string(&alondra_core::options::OptionsFile::default()
                .resolve(None, &Default::default()).unwrap()).unwrap(),
        },
        state: ScheduleLaunchState::Pending, claimed_at: None, finished_at: None,
        session_id: None, agent_id: None, error: None,
    }).unwrap();
    let valid_run = format!(r#"INSERT INTO schedule_runs
        (id,schedule_id,origin,claim_revision,due_at,claimed_at,finished_at,state,request_id,task_id,error,definition_snapshot,launch)
        VALUES ('run-1','schedule-1','due',2,1000,1001,1002,'sent','request-1','task-1',NULL,'{{"title":"Daily work","text":"Do work","project":null,"options":{{}},"disallowed_tools":[]}}','{launch}')"#);
    assert!(upgraded.conn.execute_batch(&valid_run.replace("'request-1'", "'request-2'")).is_err(),
        "Task must belong to the run Request");
    assert!(upgraded.conn.execute_batch(&valid_run.replace("'due',2", "'due',5")).is_err(),
        "a run cannot claim a future Schedule revision");
    assert!(upgraded.conn.execute_batch(&valid_run.replace("\"state\":\"pending\"", "\"state\":\"bogus\"")).is_err(),
        "unknown technical launch states are rejected");
    upgraded.conn.execute_batch(&valid_run).unwrap();
    upgraded.conn.execute("UPDATE schedules SET revision=3 WHERE id='schedule-1'", []).unwrap();
    upgraded.conn.execute_batch(r#"INSERT INTO schedule_runs
        (id,schedule_id,origin,claim_revision,due_at,claimed_at,finished_at,state,request_id,task_id,error,definition_snapshot)
        VALUES ('run-2','schedule-1','manual',3,NULL,2000,NULL,'launching',NULL,NULL,NULL,'{"title":"Daily work","text":"Do work","project":null,"options":{},"disallowed_tools":[]}')"#).unwrap();
    assert!(upgraded.conn.execute_batch(r#"INSERT INTO schedule_runs
        (id,schedule_id,origin,claim_revision,due_at,claimed_at,finished_at,state,request_id,task_id,error,definition_snapshot)
        VALUES ('run-3','schedule-1','manual',4,NULL,2001,NULL,'launching',NULL,NULL,NULL,'{"title":"Daily work","text":"Do work","project":null,"options":{},"disallowed_tools":[]}')"#).is_err(),
        "one schedule cannot have simultaneous claims");
    assert!(upgraded.conn.execute("UPDATE schedule_runs SET error='premature' WHERE id='run-2'", []).is_err());
    upgraded.conn.execute("UPDATE schedule_runs SET state='failed',finished_at=2002,error='launch failed' WHERE id='run-2'", []).unwrap();
    assert!(upgraded.conn.execute("UPDATE schedule_runs SET state='launching',finished_at=NULL,error=NULL WHERE id='run-2'", []).is_err(),
        "a terminal run cannot lose its failure evidence");
    upgraded.conn.execute("UPDATE schedules SET revision=4 WHERE id='schedule-1'", []).unwrap();
    upgraded.conn.execute_batch(r#"INSERT INTO schedule_runs
        (id,schedule_id,origin,claim_revision,due_at,claimed_at,finished_at,state,request_id,task_id,error,definition_snapshot)
        VALUES ('run-3','schedule-1','manual',4,NULL,3000,NULL,'launching',NULL,NULL,NULL,'{"title":"Daily work","text":"Do work","project":null,"options":{},"disallowed_tools":[]}')"#).unwrap();
    assert!(upgraded.conn.execute("DELETE FROM schedules WHERE id='schedule-1'", []).is_err());
    assert!(upgraded.conn.execute("UPDATE schedules SET frequency='weekly' WHERE id='schedule-1'", []).is_err(),
        "an irrelevant legacy weekday may not become an invalid weekly rule");
    assert!(upgraded.conn.execute("UPDATE schedule_runs SET task_id=NULL WHERE id='run-1'", []).is_err());
    assert!(upgraded.conn.execute("UPDATE schedule_request_tools SET patterns='[]' WHERE request_id='request-2'", []).is_err());
    upgraded.conn.execute_batch("INSERT INTO legacy_schedule_receipts(sender,request_id,digest,result)
        VALUES ('phone','schedule-command',X'00','{\"ok\":true}');").unwrap();
    upgraded.integrity_check().unwrap();
    drop(upgraded);
    let mut reopened = DomainStore::open(&fixture.path()).unwrap();
    let reloaded = reopened.load().unwrap();
    assert_eq!(reloaded.tasks, before.tasks);
    assert_eq!(reloaded.requests.len(), 2);
    assert!(reloaded.requests.contains_key(&RequestId::from("request-2")));
    assert_eq!(count(&reopened, "schedules"), 1);
    assert_eq!(count(&reopened, "schedule_runs"), 3);
    let (state, reason): (String, String) = reopened.conn.query_row(
        "SELECT state,error FROM schedule_runs WHERE id='run-3'", [],
        |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(state, "interrupted");
    assert!(reason.contains("stopped"));
    assert_eq!(count(&reopened, "schedule_request_tools"), 1);
    assert_eq!(count(&reopened, "legacy_schedule_receipts"), 1);
}

#[test]
fn restart_preserves_existing_outbox_and_user_notifications_claim_normally() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    present(&mut store);
    let before = store.load().unwrap();
    let old_message_id = store.pending_messages().unwrap()[0].id;
    let existing_targets = count(&store, "domain_message_user_targets");
    drop(store);
    let mut upgraded = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(upgraded.load().unwrap(), before);
    assert_eq!(upgraded.pending_messages().unwrap()[0].id, old_message_id);
    assert_eq!(count(&upgraded, "domain_message_user_targets"), existing_targets);
    execute(
        &mut upgraded,
        &Actor::System,
        "after-upgrade-info",
        Command::PublishInformation {
            interaction_id: "info-after-upgrade".into(),
            scope: Scope::Task {
                task_id: "task-1".into(),
            },
            recipient: Recipient::User,
            body: "Ready".into(),
            attachments: vec![], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Know, "Read the update"),
        },
    );
    let user_message = upgraded
        .pending_messages()
        .unwrap()
        .into_iter()
        .find(|message| message.interaction_id.as_deref() == Some("info-after-upgrade"))
        .unwrap();
    assert_eq!(user_message.interaction_id.as_deref(), Some("info-after-upgrade"));
    assert_eq!(user_message.task_id.as_deref(), Some("task-1"));
    assert_eq!(count(&upgraded, "domain_message_user_targets"), existing_targets + 1);
    let claim = upgraded.claim_message(user_message.id).unwrap().unwrap();
    assert_eq!(claim.message.recipient_kind, "user");
    assert!(upgraded
        .finish_message(
            claim.attempt_id,
            claim.version,
            alondra_core::delivery::State::Handed,
            None,
        )
        .unwrap());
    upgraded.integrity_check().unwrap();
}

#[test]
fn a_withdrawn_question_a_retitled_task_and_a_closed_requests_answer_survive_restart() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    execute(&mut store, &agent(), "question", Command::AskQuestion {
        attachments: vec![], interaction_id: "question-1".into(), scope: Scope::Task { task_id: "task-1".into() },
        recipient: Recipient::User, body: "Ship?".into(), blocking: true,
        options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None, inputs: vec![] }], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
    });
    execute(&mut store, &agent(), "withdraw", Command::Withdraw { interaction_id: "question-1".into() });
    let orchestrator = Actor::Orchestrator { generation: 0 };
    execute(&mut store, &orchestrator, "retitle", Command::RetitleTask { task_id: "task-1".into(), title: "Editor colours".into() });
    execute(&mut store, &user(), "request-2", Command::CreateRequest { request_id: "request-2".into(), body: "Is main green?".into(), auto_merge: false, maintenance: false, message: false });
    execute(&mut store, &orchestrator, "close-2", Command::CloseRequest { request_id: "request-2".into(),
        answer: Some("Main is green".into()), withdrawn: false, title: Some("CI status".into()) });
    execute(&mut store, &user(), "request-3", Command::CreateRequest { request_id: "request-3".into(), body: "Never mind".into(), auto_merge: false, maintenance: false, message: false });
    execute(&mut store, &user(), "close-3", Command::CloseRequest { request_id: "request-3".into(), answer: None, withdrawn: true, title: None });
    let before = store.load().unwrap();
    drop(store);
    let reopened = DomainStore::open(&fixture.path()).unwrap().load().unwrap();
    assert_eq!(reopened, before);
    assert!(reopened.interactions[&"question-1".into()].withdrawn);
    assert_eq!(reopened.tasks[&"task-1".into()].title, "Editor colours");
    assert_eq!(reopened.requests[&"request-2".into()].answer.as_deref(), Some("Main is green"));
    assert!(reopened.requests[&"request-3".into()].withdrawn);
}

#[test]
fn work_declared_after_the_merge_survives_a_restart_and_leaves_no_row_once_it_ends() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    present(&mut store);
    let accept = acceptance(&mut store);
    store.execute_authenticated(&user(), &accept, None).unwrap();
    let declare = |reason: Option<&str>| Command::DeclarePostMergeWork { task_id: "task-1".into(), reason: reason.map(str::to_owned) };
    execute(&mut store, &agent(), "declare", declare(Some("release.sh publishes 0.2.54")));
    drop(store);
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    let work = store.load().unwrap().tasks[&"task-1".into()].wrap_up.post_merge_work.clone().unwrap();
    assert_eq!(work.reason, "release.sh publishes 0.2.54");
    execute(&mut store, &agent(), "finished", declare(None));
    assert_eq!(count(&store, "task_post_merge_work"), 0);
}

#[test]
fn a_requests_auto_merge_choice_survives_restart() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    execute(&mut store, &user(), "auto", Command::CreateRequest { request_id: "auto".into(), body: "Merge alone".into(), auto_merge: true, maintenance: false, message: false });
    execute(&mut store, &user(), "manual", Command::CreateRequest { request_id: "manual".into(), body: "Ask me".into(), auto_merge: false, maintenance: false, message: false });
    drop(store);
    let reopened = DomainStore::open(&fixture.path()).unwrap().load().unwrap();
    assert_eq!((reopened.requests[&"auto".into()].auto_merge, reopened.requests[&"manual".into()].auto_merge), (true, false));
}

#[test]
fn restart_preserves_decisions_documents_and_relations() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    execute(
        &mut store,
        &agent(),
        "doc",
        Command::CreateDocument {
            task_id: "task-1".into(),
            document_id: "doc-1".into(),
            title: "Design".into(),
            revision_id: "revision-1".into(),
            content_digest: "a".repeat(64),
            content_reference: "file:///synthetic/design.md".into(),
            role: "design".into(),
        },
    );
    execute(
        &mut store,
        &agent(),
        "question",
        Command::AskQuestion {
            interaction_id: "question-1".into(),
            scope: Scope::Task {
                task_id: "task-1".into(),
            },
            recipient: Recipient::User,
            body: "Which option?".into(),
            blocking: true,
            attachments: vec![attachment("question-file")],
            options: vec![QuestionOption {
                id: "continue".into(),
                label: "Continue".into(),
                description: None,
                inputs: vec![],
            }], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
        },
    );
    execute(
        &mut store,
        &user(),
        "answer",
        Command::AnswerQuestion {
            interaction_id: "question-1".into(),
            option_id: "continue".into(),
            inputs: Default::default(),
            attachments: vec![attachment("answer-file")],
        },
    );
    execute(
        &mut store,
        &agent(),
        "pr",
        Command::RegisterPr {
            task_id: "task-1".into(),
            pr_id: "pr-1".into(),
            host: "github.com".into(),
            repository: "owner/repository".into(),
            number: 1,
            head: "aaa".into(),
            base: "main".into(),
            allowed_methods: vec![MergeMethod::Squash],
        },
    );
    execute(
        &mut store,
        &agent(),
        "link-question",
        Command::LinkInteractionSubject {
            interaction_id: "question-1".into(),
            subject: InteractionSubject::PullRequest {
                pr_id: "pr-1".into(),
            },
        },
    );
    execute(
        &mut store,
        &agent(),
        "present",
        Command::PresentResult {
            task_id: "task-1".into(),
            interaction_id: "delivery-1".into(),
            body: "PR and document".into(),
            attachments: vec![attachment("delivery-file")],
            targets: vec![
                DeliveryTarget::PullRequest {
                    pr_id: "pr-1".into(),
                    head: "aaa".into(),
                },
                DeliveryTarget::DocumentRevision {
                    document_id: "doc-1".into(),
                    revision_id: "revision-1".into(),
                },
            ],
            is_final: true, nothing_to_review: false, card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
        },
    );
    execute(
        &mut store,
        &user(),
        "authorize",
        Command::AuthorizeMerge {
            authorization_id: "authorization-1".into(),
            presentation_id: "delivery-1".into(),
            pr_id: "pr-1".into(),
            expected_head: "aaa".into(),
            method: MergeMethod::Squash,
        },
    );
    execute(
        &mut store,
        &user(),
        "decide-document",
        Command::DecideDocumentReview {
            task_id: "task-1".into(),
            document_id: "doc-1".into(),
            revision_id: "revision-1".into(),
            accepted: true,
        },
    );
    let before = store.load().unwrap();
    assert_eq!(before.document_reviews[0].decisions.len(), 1);
    assert!(matches!(&before.interactions[&InteractionId::from("delivery-1")].payload,
        InteractionPayload::Delivery { acceptance: Some(acceptance), .. } if acceptance.valid));
    assert_eq!(
        before.pull_requests[&PrId::from("pr-1")]
            .review
            .as_ref()
            .unwrap()
            .decisions
            .len(),
        1
    );
    let audits = count(&store, "command_audit");
    let receipts = count(&store, "semantic_receipts");
    let pending = store.pending_messages().unwrap();
    let user_targets = count(&store, "domain_message_user_targets");
    drop(store);
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(store.load().unwrap(), before);
    assert_eq!(store.pending_messages().unwrap(), pending);
    assert_eq!(count(&store, "command_audit"), audits);
    assert_eq!(count(&store, "semantic_receipts"), receipts);
    assert_eq!(count(&store, "domain_message_user_targets"), user_targets);
    store.integrity_check().unwrap();
    assert!(
        store
            .conn
            .execute(
                "UPDATE document_revisions SET content_digest=?1 WHERE id='revision-1'",
                ["b".repeat(64)]
            )
            .is_err()
    );
    assert!(store.conn.execute("UPDATE delivery_targets SET document_id='missing-document' WHERE kind='document_revision'",[]).is_err());
    let state = store.load().unwrap();
    assert_eq!(state, before);
}

#[test]
fn attachments_of_interaction_answer_feedback_and_delivery_survive_restart() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    execute(
        &mut store,
        &agent(),
        "question",
        Command::AskQuestion {
            interaction_id: "question-1".into(),
            scope: Scope::Task {
                task_id: "task-1".into(),
            },
            recipient: Recipient::User,
            body: "What?".into(),
            blocking: true,
            options: vec![QuestionOption {
                id: "yes".into(),
                label: "Yes".into(),
                description: None,
                inputs: vec![],
            }],
            attachments: vec![attachment("question-file")], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
        },
    );
    execute(
        &mut store,
        &user(),
        "answer",
        Command::AnswerQuestion {
            interaction_id: "question-1".into(),
            option_id: "yes".into(),
            inputs: Default::default(),
            attachments: vec![attachment("answer-file")],
        },
    );
    execute(
        &mut store,
        &agent(),
        "present",
        Command::PresentResult {
            task_id: "task-1".into(),
            interaction_id: "delivery-1".into(),
            body: "Result".into(),
            targets: vec![DeliveryTarget::Text {
                text: "Result".into(),
            }],
            is_final: true,
            nothing_to_review: false,
            attachments: vec![attachment("delivery-file")], card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
        },
    );
    execute(
        &mut store,
        &user(),
        "feedback",
        Command::SubmitFeedback {
            presentation_id: "delivery-1".into(),
            targets: vec![DeliveryTarget::Text {
                text: "Result".into(),
            }],
            body: "Please revise".into(),
            instruction: Instruction::Changes,
            attachments: vec![attachment("feedback-file")],
        },
    );
    assert_eq!(count(&store, "interaction_attachments"), 4);
    let state = store.load().unwrap();
    drop(store);
    let mut reopened = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(reopened.load().unwrap(), state);
    let q = &state.interactions[&InteractionId::from("question-1")];
    assert_eq!(q.attachments[0].id, "question-file");
    let InteractionPayload::Question {
        answer: Some(a), ..
    } = &q.payload
    else {
        panic!("missing answer")
    };
    assert_eq!(a.attachments[0].id, "answer-file");
    let d = &state.interactions[&InteractionId::from("delivery-1")];
    assert_eq!(d.attachments[0].id, "delivery-file");
    let InteractionPayload::Delivery { feedback, .. } = &d.payload else {
        panic!("missing delivery")
    };
    assert_eq!(feedback[0].attachments[0].id, "feedback-file");
}

#[test]
fn second_user_read_is_noop_with_independent_receipt_and_zero_delivery() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    execute(
        &mut store,
        &Actor::System,
        "info",
        Command::PublishInformation {
            interaction_id: "info-1".into(),
            scope: Scope::Task {
                task_id: "task-1".into(),
            },
            recipient: Recipient::User,
            body: "Info".into(),
            attachments: vec![], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Know, "Read the update"),
        },
    );
    execute(
        &mut store,
        &user(),
        "read-1",
        Command::MarkRead {
            interaction_id: "info-1".into(),
        },
    );
    let before = store.load().unwrap();
    let receipts = count(&store, "semantic_receipts");
    let audit = count(&store, "command_audit");
    let e = envelope(
        &mut store,
        "read-2",
        Command::MarkRead {
            interaction_id: "info-1".into(),
        },
    );
    let other = Actor::User {
        user_id: "user-2".into(),
    };
    let committed = store.execute_authenticated(&other, &e, None).unwrap();
    assert!(!committed.outcome.changed);
    assert_eq!(store.load().unwrap(), before);
    assert_eq!(count(&store, "semantic_receipts"), receipts + 1);
    assert_eq!(count(&store, "command_audit"), audit);
    assert_eq!(count(&store, "domain_messages"), 1);
}

#[test]
fn closed_request_reopen_rejection_survives_sql_restart() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let mut store = DomainStore::open(&path).unwrap();
    seeded(&mut store);
    execute(
        &mut store,
        &user(),
        "cancel-task",
        Command::CancelTask {
            task_id: "task-1".into(),
        },
    );
    execute(
        &mut store,
        &user(),
        "close-request",
        Command::CloseRequest { request_id: "request-1".into(), answer: None, withdrawn: false, title: None },
    );
    let before = store.load().unwrap();
    let receipts = count(&store, "semantic_receipts");
    let audit = count(&store, "command_audit");
    let attempt = envelope(
        &mut store,
        "reopen-closed-request",
        Command::ReopenTask {
            task_id: "task-1".into(),
        },
    );
    let result = store.execute_authenticated(&user(), &attempt, None);
    assert!(matches!(result, Err(StoreError::Domain(error)) if error.code == "not_found"));
    assert_eq!(store.load().unwrap(), before);
    assert_eq!(count(&store, "semantic_receipts"), receipts);
    assert_eq!(count(&store, "command_audit"), audit);
    drop(store);
    let mut reopened = DomainStore::open(&path).unwrap();
    assert_eq!(reopened.load().unwrap(), before);
    let result = reopened.execute_authenticated(&user(), &attempt, None);
    assert!(matches!(result, Err(StoreError::Domain(error)) if error.code == "not_found"));
    assert_eq!(reopened.load().unwrap(), before);
}

fn document_package(store: &mut DomainStore, pr: bool) -> Vec<DeliveryTarget> {
    execute(
        store,
        &agent(),
        "document",
        Command::CreateDocument {
            task_id: "task-1".into(),
            document_id: "doc-1".into(),
            title: "Design".into(),
            revision_id: "revision-1".into(),
            content_digest: "a".repeat(64),
            content_reference: "file:///synthetic/design.md".into(),
            role: "design".into(),
        },
    );
    let mut targets = vec![DeliveryTarget::DocumentRevision {
        document_id: "doc-1".into(),
        revision_id: "revision-1".into(),
    }];
    if pr {
        execute(
            store,
            &agent(),
            "pr",
            Command::RegisterPr {
                task_id: "task-1".into(),
                pr_id: "pr-1".into(),
                host: "github.com".into(),
                repository: "owner/repository".into(),
                number: 1,
                head: "aaa".into(),
                base: "main".into(),
                allowed_methods: vec![MergeMethod::Squash, MergeMethod::Rebase],
            },
        );
        targets.push(DeliveryTarget::PullRequest {
            pr_id: "pr-1".into(),
            head: "aaa".into(),
        });
    } else {
        targets.push(DeliveryTarget::Text {
            text: "The answer".into(),
        });
    }
    execute(
        store,
        &agent(),
        "present",
        Command::PresentResult {
            task_id: "task-1".into(),
            interaction_id: "delivery-1".into(),
            body: "Finished the design".into(),
            targets: targets.clone(),
            is_final: true, nothing_to_review: false,
            attachments: vec![], card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
        },
    );
    targets
}

#[test]
fn acceptance_from_a_snapshot_older_than_a_rejection_is_stale() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let targets = document_package(&mut store, false);
    let reject = envelope(
        &mut store,
        "reject-on-device-a",
        Command::DecideDocumentReview {
            task_id: "task-1".into(),
            document_id: "doc-1".into(),
            revision_id: "revision-1".into(),
            accepted: false,
        },
    );
    let accept = envelope(
        &mut store,
        "accept-on-device-b",
        Command::AcceptDelivery {
            presentation_id: "delivery-1".into(),
            targets,
        },
    );
    store.execute_authenticated(&user(), &reject, None).unwrap();
    let rejected = store.load().unwrap();
    assert!(matches!(
        store.execute_authenticated(&user(), &accept, None),
        Err(StoreError::Domain(error)) if error.code == "stale_revision"
    ));
    assert_eq!(store.load().unwrap(), rejected);
}

#[test]
fn second_merge_authorization_from_the_same_snapshot_is_stale() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    document_package(&mut store, true);
    let authorize = |store: &mut DomainStore, id: &str, method| {
        envelope(
            store,
            id,
            Command::AuthorizeMerge {
                authorization_id: id.into(),
                presentation_id: "delivery-1".into(),
                pr_id: "pr-1".into(),
                expected_head: "aaa".into(),
                method,
            },
        )
    };
    let squash = authorize(&mut store, "squash-on-device-a", MergeMethod::Squash);
    let rebase = authorize(&mut store, "rebase-on-device-b", MergeMethod::Rebase);
    store.execute_authenticated(&user(), &squash, None).unwrap();
    let authorized = store.load().unwrap();
    assert!(matches!(
        store.execute_authenticated(&user(), &rebase, None),
        Err(StoreError::Domain(error)) if error.code == "stale_revision"
    ));
    assert_eq!(store.load().unwrap(), authorized);
    let kept = &authorized.merge_authorizations[&"squash-on-device-a".into()];
    assert!(kept.valid && kept.method == MergeMethod::Squash);
}

#[test]
fn a_commit_landing_right_after_ours_is_never_served_from_the_cache() {
    let fixture = Fixture::new();
    let mut first = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut first);
    let mut second = DomainStore::open(&fixture.path()).unwrap();
    let foreign = envelope(
        &mut second,
        "foreign-request",
        Command::CreateRequest {
            request_id: "request-2".into(),
            body: "Written by another connection".into(), auto_merge: false, maintenance: false, message: false,
        },
    );
    let mut foreign_writer = Some((second, foreign));
    first.on_commit(move || {
        if let Some((mut second, foreign)) = foreign_writer.take() {
            let mut foreign = foreign;
            foreign.expected_revision += 1;
            second.execute_authenticated(&user(), &foreign, None).unwrap();
        }
    });
    let own = envelope(
        &mut first,
        "own-request",
        Command::CreateRequest {
            request_id: "request-3".into(),
            body: "Written by this connection".into(), auto_merge: false, maintenance: false, message: false,
        },
    );
    first.execute_authenticated(&user(), &own, None).unwrap();
    let served = first.load().unwrap();
    let persisted = DomainStore::open_readonly(&fixture.path())
        .unwrap()
        .load()
        .unwrap();
    assert!(persisted.requests.contains_key(&"request-2".into()));
    assert_eq!(served, persisted);
}

#[test]
fn a_new_final_reports_the_acceptance_it_superseded_and_feedback_does_not() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    present(&mut store);
    let e = acceptance(&mut store);
    let accepted_at = store
        .execute_authenticated(&user(), &e, None)
        .unwrap()
        .outcome
        .revision;
    let mut represent = envelope(
        &mut store,
        "represent-without-reading",
        Command::PresentResult {
            task_id: "task-1".into(),
            interaction_id: "delivery-2".into(),
            body: "A different answer".into(),
            targets: vec![DeliveryTarget::Text {
                text: "Another answer".into(),
            }],
            is_final: true,
            nothing_to_review: false,
            attachments: vec![], card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
        },
    );
    represent.expected_revision = 0;
    let superseded = SupersededAcceptance {
        task_id: "task-1".into(),
        presentation_id: "delivery-1".into(),
        user_id: "user-1".into(),
        accepted_at,
    };
    let committed = store.execute_filling_revision(&agent(), &represent).unwrap();
    assert_eq!(committed.outcome.superseded_acceptances, vec![superseded.clone()]);
    assert_eq!(store.load().unwrap().projection().superseded_acceptances, vec![superseded]);
    let audited: String = store
        .conn
        .query_row(
            "SELECT r.result FROM command_audit a JOIN semantic_receipts r
               USING(actor,command_id,data_epoch) WHERE a.revision=?1",
            [committed.outcome.revision as i64],
            |row| row.get(0),
        )
        .unwrap();
    assert!(audited.contains("\"superseded_acceptances\""), "{audited}");

    let accept_again = envelope(
        &mut store,
        "accept-new-final",
        Command::AcceptDelivery {
            presentation_id: "delivery-2".into(),
            targets: vec![DeliveryTarget::Text {
                text: "Another answer".into(),
            }],
        },
    );
    store.execute_authenticated(&user(), &accept_again, None).unwrap();
    assert!(store.load().unwrap().projection().superseded_acceptances.is_empty());
    let feedback = envelope(
        &mut store,
        "user-withdraws",
        Command::SubmitFeedback {
            attachments: vec![],
            presentation_id: "delivery-2".into(),
            targets: vec![DeliveryTarget::Text {
                text: "Another answer".into(),
            }],
            body: "Not quite".into(),
            instruction: Instruction::Changes,
        },
    );
    let withdrawn = store.execute_authenticated(&user(), &feedback, None).unwrap();
    assert!(withdrawn.outcome.superseded_acceptances.is_empty());
    assert!(store.load().unwrap().projection().superseded_acceptances.is_empty());
}

#[test]
fn persisting_a_removed_entry_is_refused_instead_of_leaving_its_row() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let before = store.load().unwrap();
    let mut after = DomainState::clone(&before);
    after.agents.remove(&"agent-1".into());
    after.revision += 1;
    let tx = store.conn.transaction().unwrap();
    let error = repository::save_changes(&tx, &before, &after).unwrap_err();
    assert!(error.to_string().contains("cannot remove agent"), "{error}");
}

#[test]
fn a_request_and_what_the_user_chose_for_its_launch_commit_together_or_not_at_all() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    let launch = RequestLaunch {
        project: Some("App".into()),
        options: alondra_core::options::OptionsFile::default().resolve(Some("App"), &Default::default()).unwrap(),
        issue: Some("me/app#3".into()),
        issue_text: Some("Crash on launch".into()),
        attachments: vec![attachment("att-1")],
    };
    let create = |store: &mut DomainStore, id: &str, request: &str, body: &str| {
        let mut e = envelope(store, id, Command::CreateRequest { request_id: request.into(), body: body.into(), auto_merge: false, maintenance: false, message: false });
        e.expected_revision = 0;
        store.execute_launching(&user(), &e, None, &launch)
    };
    assert!(matches!(create(&mut store, "empty", "request-1", "  "), Err(StoreError::Domain(_))), "a refused request records no launch");
    assert!(store.request_launch(&"request-1".into()).unwrap().is_none());
    assert!(store.load().unwrap().requests.is_empty());
    let not_a_request = envelope(&mut store, "other", Command::CloseRequest { request_id: "request-1".into(), answer: None, withdrawn: false, title: None });
    assert!(matches!(store.execute_launching(&user(), &not_a_request, None, &launch), Err(StoreError::Domain(_))), "only a new Request has a launch");

    let first = create(&mut store, "first", "request-1", "Ship it").unwrap();
    assert!(!first.replayed && first.outcome.changed);
    assert_eq!(store.request_launch(&"request-1".into()).unwrap().as_ref(), Some(&launch));
    assert_eq!(store.request_launch_summaries().unwrap()[&"request-1".into()], (Some("App".to_owned()), Some("auto".to_owned())));
    let again = create(&mut store, "first", "request-1", "Ship it").unwrap();
    assert!(again.replayed, "the same command again is the same request");
    execute(&mut store, &user(), "plain", Command::CreateRequest { request_id: "request-2".into(), body: "Plain".into(), auto_merge: false, maintenance: false, message: false });
    assert!(store.request_launch(&"request-2".into()).unwrap().is_none(), "a plain request keeps no launch");

    assert!(store.conn.execute("UPDATE request_launch SET project='Other'", []).is_err(), "what was asked for is immutable");
    assert!(store.conn.execute("DELETE FROM request_launch", []).is_err(), "and retained");
    let expected = store.load().unwrap();
    drop(store);
    let mut reopened = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(reopened.load().unwrap(), expected);
    assert_eq!(reopened.request_launch(&"request-1".into()).unwrap().as_ref(), Some(&launch), "the launch survives a restart");
}

#[test]
fn a_tasks_model_survives_restart_and_a_store_made_before_models_still_opens() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let launched = alondra_core::domain::TaskModel { model: "opus".into(), chosen_by: alondra_core::domain::ModelChooser::Orchestrator };
    execute(&mut store, &user(), "model", Command::AssignTask { task_id: "task-1".into(), agent_id: "agent-1".into(), model: Some(launched.clone()) });
    drop(store);
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(store.load().unwrap().tasks[&"task-1".into()].model, Some(launched));

    store.conn.execute_batch("DROP TABLE task_models").unwrap();
    drop(store);
    let mut before_models = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(before_models.load().unwrap().tasks[&"task-1".into()].model, None, "the old store opens with the task as it was");
    assert_eq!(before_models.conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), SCHEMA_VERSION);
    assert!(!fixture.0.read_dir().unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains(".old-")), "nothing was moved aside");
}

#[test]
fn a_store_made_before_wrap_ups_launch_plans_and_closings_opens_in_place() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    execute(&mut store, &user(), "cancel", Command::CancelTask { task_id: "task-1".into() });
    store.begin_task_closing(&"task-1".into(), 5).unwrap();
    store.conn.execute_batch("DROP TABLE task_wrap_ups; DROP TABLE task_relations; DROP TABLE task_closings; DROP TABLE auto_launches; DROP TABLE task_launch_plans; DROP TABLE inherited_worktrees; DROP TABLE maintenance_requests").unwrap();
    drop(store);
    let mut reopened = DomainStore::open(&fixture.path()).unwrap();
    for table in ["task_wrap_ups", "task_relations", "task_closings", "auto_launches", "task_launch_plans", "inherited_worktrees", "maintenance_requests"] {
        let count: i64 = reopened.conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0, "{table} is created empty");
    }
    assert!(reopened.load().unwrap().tasks[&"task-1".into()].wrap_up.is_default(), "the task decodes with the default wrap-up");
    assert_eq!(reopened.conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), SCHEMA_VERSION);
    assert!(!fixture.0.read_dir().unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains(".old-")), "nothing was moved aside");
}

#[test]
fn a_maintenance_message_stays_one_after_a_restart() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    execute(&mut store, &user(), "maintenance", Command::CreateRequest {
        request_id: "request-m".into(), body: "Launch two more".into(), auto_merge: false, maintenance: true, message: false });
    drop(store);
    let state = DomainStore::open(&fixture.path()).unwrap().load().unwrap();
    assert!(state.requests[&"request-m".into()].maintenance);
    assert!(!state.requests[&"request-1".into()].maintenance);
}

#[test]
fn a_message_to_the_orchestrator_stays_one_after_a_restart() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    execute(&mut store, &user(), "message", Command::CreateRequest {
        request_id: "request-q".into(), body: "Is main green?".into(), auto_merge: false, maintenance: false, message: true });
    execute(&mut store, &user(), "maintenance", Command::CreateRequest {
        request_id: "request-m".into(), body: "Launch two more".into(), auto_merge: false, maintenance: true, message: false });
    drop(store);
    let state = DomainStore::open(&fixture.path()).unwrap().load().unwrap();
    assert!(state.requests[&"request-q".into()].message);
    assert!(state.requests[&"request-m".into()].message, "maintenance is a message too");
    assert!(!state.requests[&"request-1".into()].message);
}

#[test]
fn a_store_made_before_messages_were_marked_marks_the_ones_the_message_route_made() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let routed = "request-0123456789abcdef0123456789abcdef";
    for (id, maintenance) in [(routed, false), ("request-m", true), ("5f1c2c1e-6d55-4a2e-9a77-0b3c1d7e9f10", false), ("request-0123456789abcdef0123456789abcdeg", false)] {
        execute(&mut store, &user(), id, Command::CreateRequest {
            request_id: id.into(), body: "Text".into(), auto_merge: false, maintenance, message: false });
    }
    store.conn.execute_batch("DROP TABLE message_requests").unwrap();
    drop(store);
    let state = DomainStore::open(&fixture.path()).unwrap().load().unwrap();
    let marked: BTreeSet<&str> = state.requests.values().filter(|r| r.message).map(|r| r.id.0.as_str()).collect();
    assert_eq!(marked, BTreeSet::from([routed, "request-m"]));
}

#[test]
fn task_relations_a_verdict_and_the_split_review_states_survive_a_restart() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    execute(&mut store, &Actor::System, "agent-2", Command::RegisterAgent { agent_id: "agent-2".into(), name: "Reviewer".into() });
    execute(&mut store, &user(), "request-title", Command::TitleRequest { request_id: "request-1".into(), title: "Useful work".into() });
    execute(&mut store, &user(), "task-2", Command::CreateTask {
        task_id: "task-2".into(), request_id: "request-1".into(), title: "Review".into(), agent_id: Some("agent-2".into()),
        review_prs: false, dependencies: BTreeSet::new(), stack: None,
    });
    execute(&mut store, &user(), "relations", Command::SetRelations {
        task_id: "task-2".into(), delivered_dependencies: BTreeSet::from(["task-1".into()]), blocks: BTreeSet::from(["task-1".into()]),
    });
    present(&mut store);
    assert_eq!(store.load().unwrap().tasks[&"task-1".into()].state, TaskState::AwaitingBlocker);
    let reviewer = Actor::Agent { agent_id: "agent-2".into() };
    execute(&mut store, &reviewer, "start-review", Command::StartTask { task_id: "task-2".into() });
    execute(&mut store, &reviewer, "present-review", Command::PresentResult {
        task_id: "task-2".into(), interaction_id: "delivery-2".into(), body: "Reviewed".into(),
        targets: vec![DeliveryTarget::Text { text: "The review".into() }], is_final: true, nothing_to_review: false, attachments: vec![],
        card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
    });
    execute(&mut store, &reviewer, "changes", Command::RecordVerdict {
        task_id: "task-2".into(), presentation_id: "delivery-2".into(), verdict: Verdict::ChangesRequested { text: "Fix it".into(), serious_findings: Some(1) },
    });
    drop(store);
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    let state = store.load().unwrap();
    let review = &state.tasks[&"task-2".into()];
    assert_eq!((review.blocks.len(), review.delivered_dependencies.len()), (1, 1));
    assert_eq!(review.verdict, Some(Verdict::ChangesRequested { text: "Fix it".into(), serious_findings: Some(1) }));
    assert_eq!(state.tasks[&"task-1".into()].review_rounds, 1, "the round is counted on the task under review");
    assert_eq!(state.tasks[&"task-1".into()].state, TaskState::Running, "the work went back to its node");

    let pushes = |store: &DomainStore, card: &str| -> i64 {
        store.conn.query_row(
            "SELECT count(*) FROM domain_messages m JOIN domain_message_user_targets t ON t.message_id=m.id WHERE m.interaction_id=?1",
            [card], |r| r.get(0),
        ).unwrap()
    };
    assert_eq!(pushes(&store, "delivery-1"), 0, "no push while a review holds the delivery back");
    execute(&mut store, &agent(), "present-again", Command::PresentResult {
        task_id: "task-1".into(), interaction_id: "delivery-1b".into(), body: "Fixed".into(),
        targets: vec![DeliveryTarget::Text { text: "The answer".into() }], is_final: true, nothing_to_review: false, attachments: vec![],
        card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
    });
    execute(&mut store, &reviewer, "present-review-again", Command::PresentResult {
        task_id: "task-2".into(), interaction_id: "delivery-2b".into(), body: "Reviewed again".into(),
        targets: vec![DeliveryTarget::Text { text: "The review".into() }], is_final: true, nothing_to_review: false, attachments: vec![],
        card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
    });
    execute(&mut store, &reviewer, "approve", Command::RecordVerdict {
        task_id: "task-2".into(), presentation_id: "delivery-2b".into(),
        verdict: Verdict::Approved { summary: "Fine".into(), open_issues: false, serious_findings: 0 },
    });
    assert_eq!(pushes(&store, "delivery-1b"), 1, "one push when the review lets the delivery through");
    drop(store);
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(store.load().unwrap().tasks[&"task-1".into()].state, TaskState::ForYourReview);
    execute(&mut store, &user(), "rename", Command::RetitleTask { task_id: "task-1".into(), title: "Goal, renamed".into() });
    assert_eq!(pushes(&store, "delivery-1b"), 1, "a restart and an unrelated command never repeat it");
    assert_eq!(pushes(&store, "delivery-1"), 0);
}

#[test]
fn a_tasks_wrap_up_survives_a_restart() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    execute(&mut store, &user(), "cancel", Command::CancelTask { task_id: "task-1".into() });
    execute(&mut store, &Actor::System, "end", Command::RecordNodeEnd { task_id: "task-1".into(), end: NodeEnd::KeptByUser, agent_id: None });
    drop(store);
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    let wrap_up = store.load().unwrap().tasks[&"task-1".into()].wrap_up.clone();
    assert_eq!(wrap_up, WrapUp { asked_at: None, cleaned_at: None, close_cancelled: true, node: Some(NodeEnd::KeptByUser), post_merge_work: None });

    execute(&mut store, &user(), "reopen", Command::ReopenTask { task_id: "task-1".into() });
    drop(store);
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    assert!(store.load().unwrap().tasks[&"task-1".into()].wrap_up.is_default(), "a reset is written back, not left stale");
}

#[test]
fn task_closings_keep_their_wait_failures_and_card_across_a_restart() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let mut row = store.begin_task_closing(&"task-1".into(), 100).unwrap();
    assert_eq!((row.since, row.failures), (100, 0));
    row.failures = 2;
    row.last_error = Some("busy".into());
    row.next_attempt = 900;
    row.attempted_cleaned_at = Some(7);
    row.rm_started_at = Some(150);
    row.problem_id = Some("problem-close-task-1-ab".into());
    store.save_task_closing(&row).unwrap();
    drop(store);
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(store.begin_task_closing(&"task-1".into(), 5_000).unwrap(), row, "the wait is not restarted");
    assert_eq!(store.task_closings().unwrap(), vec![row]);
    store.forget_task_closing(&"task-1".into()).unwrap();
    assert_eq!(store.task_closing(&"task-1".into()).unwrap(), None);
}

fn plan() -> crate::launch_plans::LaunchPlan {
    crate::launch_plans::LaunchPlan {
        name: "app/next".into(), cwd: "/synthetic/App".into(), mission: None, model: Some("sonnet".into()), effort: None,
        approach: None, reason: Some("clear".into()), role: Some(crate::launch_plans::TaskRole::Implementation),
    }
}

fn planned_task(id: &str, request: &str, epoch: &alondra_core::domain::DataEpoch) -> CommandEnvelope {
    CommandEnvelope {
        protocol_version: 2, data_epoch: epoch.clone(), command_id: format!("plan-{id}").into(), expected_revision: 0,
        command: Command::CreateTask { task_id: id.into(), request_id: request.into(), title: "Next".into(), agent_id: None,
            review_prs: false, dependencies: Default::default(), stack: None },
    }
}

#[test]
fn a_launch_plan_is_stored_with_its_task_in_one_transaction() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    execute(&mut store, &user(), "title", Command::TitleRequest { request_id: "request-1".into(), title: "Work".into() });
    let epoch = store.load().unwrap().data_epoch.clone();
    let envelope = |id: &str, request: &str| planned_task(id, request, &epoch);

    let refused = store.execute_planned(&user(), &envelope("next-1", "no-such-request"), &plan());
    assert!(matches!(refused, Err(StoreError::Domain(_))));
    assert_eq!(store.launch_plan(&"next-1".into()).unwrap(), None, "a refused command stores no plan");

    store.execute_planned(&user(), &envelope("next-1", "request-1"), &plan()).unwrap();
    assert_eq!(store.launch_plan(&"next-1".into()).unwrap(), Some(plan()));
    assert_eq!(store.task_roles().unwrap().get(&TaskId::from("next-1")).map(String::as_str), Some("implementation"));

    let other = crate::launch_plans::LaunchPlan { name: "app/other".into(), ..plan() };
    let replay = store.execute_planned(&user(), &envelope("next-1", "request-1"), &other).unwrap();
    assert!(replay.replayed);
    assert_eq!(store.launch_plan(&"next-1".into()).unwrap(), Some(plan()), "a replay does not write the plan again");
    drop(store);
    let reopened = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(reopened.launch_plan(&"next-1".into()).unwrap(), Some(plan()), "the plan survives a restart");
}

#[test]
fn an_automatic_launch_is_claimed_once_per_task() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let task: TaskId = "task-1".into();
    assert!(store.claim_auto_launch(&task, 1, false).unwrap());
    assert!(!store.claim_auto_launch(&task, 2, false).unwrap(), "taken");
    assert!(!store.claim_auto_launch(&task, 3, true).unwrap(), "a launching row is not retaken");
    store.set_auto_launch(&task, AutoLaunchState::Blocked, Some(AutoLaunchReason::UsageExhausted), Some("later"), 4).unwrap();
    assert!(!store.claim_auto_launch(&task, 5, false).unwrap(), "blocked stays blocked without a retry");
    assert!(store.claim_auto_launch(&task, 6, true).unwrap());
    assert_eq!(store.auto_launch(&task).unwrap().unwrap().state, AutoLaunchState::Launching);
    drop(store);
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(store.auto_launches().unwrap().len(), 1, "the claim survives a restart");
    store.forget_auto_launch(&task).unwrap();
    assert_eq!(store.auto_launch(&task).unwrap(), None);
}

#[test]
fn the_orchestrators_session_survives_a_restart_and_a_new_one_replaces_it() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(store.load().unwrap().orchestrator_session, None);
    execute(&mut store, &Actor::System, "bind-1", Command::BindOrchestratorSession { session_id: "session-1".into() });
    execute(&mut store, &Actor::System, "bind-2", Command::BindOrchestratorSession { session_id: "session-2".into() });
    drop(store);
    let mut reopened = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(reopened.load().unwrap().orchestrator_session, Some("session-2".into()));
}
