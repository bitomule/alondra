use super::*;
use std::collections::BTreeSet;
use std::os::unix::fs::DirBuilderExt;
use std::{fs, path::PathBuf};
pub(super) struct Fixture(pub(super) PathBuf);
impl Fixture {
    pub(super) fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "alondra-domain-store-{}",
            crate::auth::random_hex(16)
        ));
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
        data_epoch: s.data_epoch,
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
        &user(),
        "request",
        Command::CreateRequest {
            request_id: "request-1".into(),
            body: "Do useful work".into(),
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
            attachments: vec![],
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
fn count(store: &DomainStore, table: &str) -> i64 {
    store
        .conn
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn sql_restart_roundtrip_receipts_and_read_has_no_communication() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    execute(
        &mut store,
        &agent(),
        "info",
        Command::PublishInformation {
            interaction_id: "info-1".into(),
            scope: Scope::Task {
                task_id: "task-1".into(),
            },
            recipient: Recipient::User,
            body: "Progress".into(),
            attachments: vec![attachment("info-file")],
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
    let e = acceptance(&mut store);
    let committed = store
        .execute_authenticated(&user(), &e, Some(("phone", "request-x", &[1; 32])))
        .unwrap();
    assert_eq!(count(&store, "domain_messages"), 3);
    assert_eq!(count(&store, "domain_outbox"), 3);
    let expected = store.load().unwrap();
    drop(store);
    let mut restarted = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(restarted.load().unwrap(), expected);
    let recovered = restarted
        .execute_authenticated(&user(), &e, Some(("phone", "request-x", &[1; 32])))
        .unwrap();
    assert!(recovered.replayed);
    assert_eq!(recovered.outcome, committed.outcome);
    assert_eq!(count(&restarted, "domain_messages"), 3);
    let reciphered = restarted
        .execute_authenticated(&user(), &e, Some(("phone", "request-y", &[2; 32])))
        .unwrap();
    assert!(reciphered.replayed);
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
    let e = acceptance(&mut store);
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
    store
        .conn
        .execute_batch("DROP TRIGGER fail_receipt")
        .unwrap();
    store
        .execute_authenticated(&user(), &e, Some(("phone", "op", &[1; 32])))
        .unwrap();
    assert_eq!(count(&store, "domain_messages"), 2);
}

#[test]
fn two_connections_compete_by_revision_and_domain_rejection_writes_nothing() {
    let fixture = Fixture::new();
    let mut first = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut first);
    present(&mut first);
    let mut second = DomainStore::open(&fixture.path()).unwrap();
    let e = acceptance(&mut first);
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
    let e = acceptance(&mut store);
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
fn future_schema_and_legacy_writer_are_rejected_and_migration_preserves_rows() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let expected = store.load().unwrap();
    assert!(crate::store::Store::open(&fixture.path()).is_err());
    store
        .conn
        .execute_batch("DROP TABLE domain_message_user_targets; DROP TABLE interaction_attachments; ALTER TABLE domain_meta DROP COLUMN migration_note; PRAGMA user_version=1;")
        .unwrap();
    drop(store);
    let mut migrated = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(migrated.load().unwrap(), expected);
    assert_eq!(count(&migrated, "interaction_attachments"), 0);
    assert_eq!(count(&migrated, "domain_message_user_targets"), 0);
    assert!(fixture.0.join("domain.sqlite.migration-backups").is_dir());
    migrated
        .conn
        .pragma_update(None, "user_version", 4)
        .unwrap();
    drop(migrated);
    assert!(DomainStore::open(&fixture.path()).is_err());
}

#[test]
fn v2_migration_preserves_existing_outbox_and_user_notifications_claim_normally() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    present(&mut store);
    let before = store.load().unwrap();
    let old_message_id = store.pending_messages().unwrap()[0].id;
    // A version 2 database had no User-target relation. Its existing outbox
    // and delivery references must survive the append-only schema upgrade.
    store
        .conn
        .execute_batch("DROP TABLE domain_message_user_targets; PRAGMA user_version=2;")
        .unwrap();
    drop(store);
    let mut upgraded = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(upgraded.load().unwrap(), before);
    assert_eq!(upgraded.pending_messages().unwrap()[0].id, old_message_id);
    assert_eq!(count(&upgraded, "domain_message_user_targets"), 0);
    execute(
        &mut upgraded,
        &agent(),
        "after-upgrade-info",
        Command::PublishInformation {
            interaction_id: "info-after-upgrade".into(),
            scope: Scope::Task {
                task_id: "task-1".into(),
            },
            recipient: Recipient::User,
            body: "Ready".into(),
            attachments: vec![],
        },
    );
    let user_message = upgraded
        .pending_messages()
        .unwrap()
        .into_iter()
        .find(|message| message.recipient_kind == "user")
        .unwrap();
    assert_eq!(user_message.interaction_id.as_deref(), Some("info-after-upgrade"));
    assert_eq!(user_message.task_id.as_deref(), Some("task-1"));
    assert_eq!(count(&upgraded, "domain_message_user_targets"), 1);
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
fn documents_questions_pr_reviews_and_authorizations_roundtrip_relations() {
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
                inputs: vec![],
            }],
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
            is_final: true,
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
    let before = store.load().unwrap();
    assert_eq!(before.document_reviews[0].decisions.len(), 1);
    assert_eq!(
        before.pull_requests[&PrId::from("pr-1")]
            .review
            .as_ref()
            .unwrap()
            .decisions
            .len(),
        1
    );
    drop(store);
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(store.load().unwrap(), before);
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
                inputs: vec![],
            }],
            attachments: vec![attachment("question-file")],
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
            attachments: vec![attachment("delivery-file")],
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
        &agent(),
        "info",
        Command::PublishInformation {
            interaction_id: "info-1".into(),
            scope: Scope::Task {
                task_id: "task-1".into(),
            },
            recipient: Recipient::User,
            body: "Info".into(),
            attachments: vec![],
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
