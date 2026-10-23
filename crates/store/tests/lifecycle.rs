use alondra_domain::*;
use alondra_foundation::{InteractionId, Revision, TaskId};
use alondra_provider_contract::{Capability, Placement, ProviderBinding};
use alondra_store::*;
use alondra_store::measure;
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};
#[path = "../../test_support.rs"]
mod test_support;

struct Fixture {
    connection: Connection,
    directory: test_support::Directory,
}
// Fixture database owner only; application behavior must use actual guarded core commands.
struct FixtureTransaction<'connection> {
    lifecycle: LifecycleTransaction<'connection>,
    connection: &'connection Connection,
}
impl<'connection> std::ops::Deref for FixtureTransaction<'connection> {
    type Target = LifecycleTransaction<'connection>;
    fn deref(&self) -> &Self::Target {
        &self.lifecycle
    }
}
impl std::ops::DerefMut for FixtureTransaction<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.lifecycle
    }
}
impl FixtureTransaction<'_> {
    fn commit(self) -> alondra_store::Result<Committed> {
        self.lifecycle.commit()
    }
    fn settle_card(
        &mut self,
        task: &TaskId,
        card: &InteractionId,
        settlement: CardSettlement,
    ) -> alondra_store::Result<()> {
        let changed = match settlement {
            CardSettlement::Answer(answer) => self.connection.execute("UPDATE interactions SET answer=?3 WHERE task_id=?1 AND id=?2 AND scope_kind='task' AND answer IS NULL AND withdrawn=0",params![task.0,card.0,answer.0])?,
            CardSettlement::Withdraw => self.connection.execute("UPDATE interactions SET withdrawn=1 WHERE task_id=?1 AND id=?2 AND scope_kind='task' AND answer IS NULL AND withdrawn=0",params![task.0,card.0])?,
        };
        if changed != 1 {
            return Err(Error::InvalidCard);
        }
        Ok(())
    }
}
struct CardAnswer(String);
impl CardAnswer {
    fn from_object(
        value: serde_json::Map<String, serde_json::Value>,
    ) -> alondra_store::Result<Self> {
        let encoded = serde_json::to_string(&value)?;
        if encoded.len() > 4096 {
            return Err(Error::InvalidInput);
        }
        Ok(Self(encoded))
    }
}
enum CardSettlement {
    Answer(CardAnswer),
    Withdraw,
}
fn scope() -> AuthorityScope {
    AuthorityScope::new("epoch".into()).unwrap()
}
fn id(value: &str) -> InputId {
    InputId::new(value).unwrap()
}
fn payload(value: &str) -> InputPayload {
    InputPayload::new(id(value), format!("Full prompt for {value}")).unwrap()
}
fn test_binding() -> ProviderBinding {
    ProviderBinding::new(
        "fixture-binding".into(),
        "fixture-driver".into(),
        "fixture-provider-instance".into(),
        "fixture-config".into(),
        1,
        1,
        BTreeSet::from([Capability::OfferInput]),
        "fixture-credential".into(),
        1,
    )
    .unwrap()
}
fn local(path: &std::path::Path) -> Placement {
    Placement::LocalDirectory(path.to_str().unwrap().to_owned())
}
fn admit(node: &str, instance: &str) -> Event {
    Event::Admit {
        node: node.into(),
        conversation: ConversationId::new(format!("conversation-{node}")).unwrap(),
        instance: InstanceId::new(instance).unwrap(),
        host: HostId::new("host").unwrap(),
    }
}
impl Fixture {
    fn new() -> Self {
        let directory = test_support::Directory::new();
        std::fs::create_dir_all(&*directory).unwrap();
        let mut connection = Connection::open(directory.join("domain.db")).unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        connection
            .execute_batch(include_str!("../../alondrad/src/domain_store/schema.sql"))
            .unwrap();
        connection
            .execute_batch(include_str!(
                "../../alondrad/src/domain_store/schedule_launch_transition.sql"
            ))
            .unwrap();
        // The daemon currently creates this additive table outside schema.sql (TASK_WORKTREES).
        connection.execute_batch("CREATE TABLE task_worktrees(task_id TEXT PRIMARY KEY CHECK(length(task_id)>0),dir TEXT NOT NULL CHECK(length(dir)>0),at INTEGER NOT NULL);").unwrap();
        connection.execute_batch("INSERT INTO domain_meta VALUES(1,'epoch',0,0); INSERT INTO requests(id,body,closed) VALUES('request','Work',0); INSERT INTO agents(id,name) VALUES('node','Node'),('second','Second');").unwrap();
        let mut tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        LifecycleTransaction::install(&mut tx).unwrap();
        tx.commit().unwrap();
        Self {
            connection,
            directory,
        }
    }
    fn add_task(&self, task: &str) {
        self.connection.execute("INSERT INTO tasks(id,request_id,title,state,revision,started,review_prs,archived) VALUES(?1,'request',?1,'not_started',0,0,0,0)", params![task]).unwrap();
        self.connection
            .execute(
                "INSERT INTO task_worktrees VALUES(?1,?2,0)",
                params![task, self.directory.join(task).to_str().unwrap()],
            )
            .unwrap();
    }
    fn transaction(&mut self) -> FixtureTransaction<'_> {
        let transaction =
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate).unwrap();
        FixtureTransaction {
            lifecycle: LifecycleTransaction::attach(transaction, scope()).unwrap(),
            connection: &self.connection,
        }
    }
    fn create(&mut self, task: &str) {
        self.add_task(task);
        let mut tx = self.transaction();
        tx.create(&task.into(), payload("initial")).unwrap();
        tx.commit().unwrap();
    }
    fn launch(&mut self, task: &str) -> DispatchPermit {
        let path = self.directory.join(task);
        let mut tx = self.transaction();
        tx.apply(&task.into(), 0, "admit", admit("node", "instance"))
            .unwrap();
        tx.stage_launch(&task.into(), &local(&path), &test_binding())
            .unwrap();
        tx.commit().unwrap().permits.pop().unwrap()
    }
    fn runtime(&mut self, task: &str, fact: RuntimeFact) -> Revision {
        let mut tx = self.transaction();
        let aggregate = tx.load(&task.into()).unwrap();
        let revision = aggregate.revision();
        tx.apply(
            &task.into(),
            revision,
            &format!("fact-{revision}"),
            Event::Runtime {
                stamp: aggregate.execution().unwrap().clone(),
                cursor: aggregate.cursor().unwrap() + 1,
                fact,
            },
        )
        .unwrap();
        tx.commit().unwrap();
        revision + 1
    }
    fn card(&self, task: &str, card: &str) {
        self.connection.execute("INSERT INTO interactions(id,scope_kind,task_id,author_kind,recipient_kind,body,blocking,kind,revision,ordering,options,withdrawn,card) VALUES(?1,'task',?2,'system','user','What should happen?',1,'question',0,0,'[{\"id\":\"answer\",\"label\":\"Answer\"}]',0,'{\"kind\":\"provide\",\"need\":\"Your decision\"}')", params![card, task]).unwrap();
    }
    fn count(&self, table: &str) -> i64 {
        assert!(
            [
                "lifecycle_tasks",
                "lifecycle_events",
                "lifecycle_dispatches",
                "lifecycle_current_execution",
                "provider_execution_owners",
                "lifecycle_controls",
                "provider_control_commands",
                "provider_binding_controls",
                "lifecycle_decision_requests"
            ]
            .contains(&table)
        );
        self.connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }
}

fn corrupt_while_guard_removed(connection: &Connection, trigger: &str, mutate: impl FnOnce()) {
    let original: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='trigger' AND name=?1",
            [trigger],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute_batch(&format!("DROP TRIGGER {trigger}"))
        .unwrap();
    mutate();
    connection.execute_batch(&original).unwrap();
}

#[test]
fn durable_intent_reopens_with_full_prompt_before_any_dispatch() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let permit = fixture.launch("task");
    drop(permit);
    let path = fixture.directory.join("domain.db");
    fixture.connection = Connection::open(path).unwrap();
    fixture
        .connection
        .execute_batch("PRAGMA foreign_keys=ON")
        .unwrap();
    let mut tx = fixture.transaction();
    tx.recover_prepared(&"task".into()).unwrap();
    let permit = tx.commit().unwrap().permits.pop().unwrap();
    assert_eq!(permit.execution().generation(), 1);
    let mut tx = fixture.transaction();
    tx.claim(permit).unwrap();
    let attempt = tx.commit().unwrap().attempts.pop().unwrap();
    assert_eq!(attempt.input().text(), "Full prompt for initial");
    assert_eq!(attempt.kind(), DispatchKind::Launch);
    assert!(attempt.working_directory().unwrap().ends_with("/task"));
    assert_eq!(attempt.scope(), &scope());
}

#[test]
fn committed_claim_without_receipt_stays_uncertain_and_is_never_reissued() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let permit = fixture.launch("task");
    let mut tx = fixture.transaction();
    tx.claim(permit).unwrap();
    let attempt = tx.commit().unwrap().attempts.pop().unwrap();
    drop(attempt); // A crash may happen before or after the external send.
    let mut tx = fixture.transaction();
    tx.recover_prepared(&"task".into()).unwrap();
    let unresolved = tx.unresolved(&"task".into()).unwrap();
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].input, id("initial"));
    assert!(!unresolved[0].uncertain_receipt);
    assert!(tx.commit().unwrap().permits.is_empty());
}

#[test]
fn handed_receipt_does_not_consume_input_and_uncertain_receipt_never_retries() {
    for receipt in [DispatchReceipt::Handed, DispatchReceipt::Uncertain] {
        let mut fixture = Fixture::new();
        fixture.create("task");
        let permit = fixture.launch("task");
        let mut tx = fixture.transaction();
        tx.claim(permit).unwrap();
        let attempt = tx.commit().unwrap().attempts.pop().unwrap();
        let mut tx = fixture.transaction();
        tx.record_receipt(attempt, receipt).unwrap();
        assert_eq!(
            tx.load(&"task".into()).unwrap().pending_inputs(),
            vec![&id("initial")]
        );
        tx.recover_prepared(&"task".into()).unwrap();
        assert_eq!(
            tx.unresolved(&"task".into()).unwrap().len(),
            usize::from(receipt == DispatchReceipt::Uncertain)
        );
        assert!(tx.commit().unwrap().permits.is_empty());
    }
}

#[test]
fn missing_launch_intent_rolls_back_aggregate_journal_and_binding() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let mut tx = fixture.transaction();
    tx.apply(&"task".into(), 0, "admit", admit("node", "instance"))
        .unwrap();
    assert!(matches!(tx.commit(), Err(Error::MissingIntent)));
    assert_eq!(fixture.count("lifecycle_events"), 0);
    assert_eq!(fixture.count("lifecycle_current_execution"), 0);
    assert_eq!(
        fixture
            .transaction()
            .load(&"task".into())
            .unwrap()
            .revision(),
        0
    );
}

#[test]
fn deferred_commit_failure_emits_no_attempt_and_rolls_back_claim() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let permit = fixture.launch("task");
    let raw = fixture
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    raw.execute(
        "UPDATE tasks SET final_presentation='missing' WHERE id='task'",
        [],
    )
    .unwrap();
    let mut tx = LifecycleTransaction::attach(raw, scope()).unwrap();
    tx.claim(permit).unwrap();
    assert!(matches!(tx.commit(), Err(Error::Database(_))));
    let mut tx = fixture.transaction();
    tx.recover_prepared(&"task".into()).unwrap();
    assert!(tx.unresolved(&"task".into()).unwrap().is_empty());
    assert_eq!(tx.commit().unwrap().permits.len(), 1);
}

#[test]
fn duplicate_command_uses_exact_receipt_identity_and_rejects_conflicting_payload() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    drop(fixture.launch("task"));
    let mut tx = fixture.transaction();
    assert_eq!(
        tx.apply(&"task".into(), 0, "admit", admit("node", "instance"))
            .unwrap(),
        Applied {
            revision: 1,
            replayed: true
        }
    );
    tx.commit().unwrap();
    let mut tx = fixture.transaction();
    assert!(matches!(
        tx.apply(&"task".into(), 1, "admit", admit("node", "instance")),
        Err(Error::CommandConflict)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let mut tx = fixture.transaction();
    assert!(matches!(
        tx.apply(&"task".into(), 0, "admit", admit("node", "other")),
        Err(Error::CommandConflict)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    assert_eq!(fixture.count("lifecycle_events"), 1);
    assert_eq!(fixture.count("lifecycle_dispatches"), 1);
}

#[test]
fn revision_fences_stale_permit_but_recovery_preserves_accepted_input() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let stale = fixture.launch("task");
    fixture.runtime("task", RuntimeFact::ResourceAvailable);
    let mut tx = fixture.transaction();
    assert!(matches!(tx.claim(stale), Err(Error::StalePermit)));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let mut tx = fixture.transaction();
    tx.recover_prepared(&"task".into()).unwrap();
    let fresh = tx.commit().unwrap().permits.pop().unwrap();
    assert_eq!(fresh.revision(), 2);
    let mut tx = fixture.transaction();
    tx.claim(fresh).unwrap();
    assert_eq!(tx.commit().unwrap().attempts.len(), 1);
}

#[test]
fn pause_after_claim_in_same_transaction_cannot_issue_attempt() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let permit = fixture.launch("task");
    fixture.card("task", "pause");
    let mut tx = fixture.transaction();
    tx.claim(permit).unwrap();
    tx.apply(
        &"task".into(),
        1,
        "pause",
        Event::Pause {
            card: "pause".into(),
            owner: Owner::User,
        },
    )
    .unwrap();
    assert!(matches!(tx.commit(), Err(Error::StalePermit)));
    assert_eq!(
        fixture
            .transaction()
            .load(&"task".into())
            .unwrap()
            .revision(),
        1
    );
}

#[test]
fn wrong_domain_epoch_refuses_restore_and_old_permit_claim() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let permit = fixture.launch("task");
    fixture
        .connection
        .execute("UPDATE domain_meta SET data_epoch='new-epoch'", [])
        .unwrap();
    let tx = fixture.connection.transaction().unwrap();
    assert!(matches!(
        LifecycleTransaction::attach(tx, scope()),
        Err(Error::WrongScope)
    ));
    let tx = fixture.connection.transaction().unwrap();
    let tx =
        LifecycleTransaction::attach(tx, AuthorityScope::new("new-epoch".into()).unwrap()).unwrap();
    assert!(matches!(tx.load(&"task".into()), Err(Error::WrongScope)));
    drop(tx);
    let tx = fixture.connection.transaction().unwrap();
    let mut tx =
        LifecycleTransaction::attach(tx, AuthorityScope::new("new-epoch".into()).unwrap()).unwrap();
    assert!(matches!(tx.claim(permit), Err(Error::StalePermit)));
}

#[test]
fn required_cards_use_actual_task_ownership_and_actionability_and_rollback_rejection() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    fixture.create("other");
    fixture.card("other", "wrong-owner");
    fixture.card("task", "answered");
    fixture
        .connection
        .execute(
            "UPDATE interactions SET answer='{}' WHERE id='answered'",
            [],
        )
        .unwrap();
    for card in ["missing", "wrong-owner", "answered"] {
        let mut tx = fixture.transaction();
        assert!(matches!(
            tx.apply(&"task".into(), 0, card, Event::Ask { card: card.into() }),
            Err(Error::InvalidCard)
        ));
        assert!(matches!(tx.commit(), Err(Error::Poisoned)));
        assert_eq!(
            fixture.transaction().load(&"task".into()).unwrap().revision(),
            0
        );
    }
    assert_eq!(fixture.count("lifecycle_events"), 0);
    fixture.card("task", "question");
    let mut tx = fixture.transaction();
    tx.apply(
        &"task".into(),
        0,
        "ask",
        Event::Ask {
            card: "question".into(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    assert!(
        fixture
            .connection
            .execute(
                "UPDATE interactions SET answer='{}' WHERE id='question'",
                []
            )
            .is_err()
    );
    let mut tx = fixture.transaction();
    tx.put_input(&"task".into(), payload("answer")).unwrap();
    tx.apply(
        &"task".into(),
        1,
        "answer",
        Event::Answer {
            card: "question".into(),
            input: id("answer"),
        },
    )
    .unwrap();
    tx.settle_card(
        &"task".into(),
        &"question".into(),
        CardSettlement::Answer(CardAnswer::from_object(serde_json::Map::new()).unwrap()),
    )
    .unwrap();
    tx.commit().unwrap();
}

#[test]
fn duplicate_instance_and_unknown_task_dependency_fail_atomically() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    fixture.create("other");
    drop(fixture.launch("task"));
    let mut tx = fixture.transaction();
    assert!(matches!(
        tx.apply(
            &"other".into(),
            0,
            "admit",
            admit("second-node", "instance")
        ),
        Err(Error::Database(_))
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    assert_eq!(
        fixture.transaction().load(&"other".into()).unwrap().revision(),
        0
    );
    let mut tx = fixture.transaction();
    assert!(matches!(
        tx.apply(
            &"other".into(),
            0,
            "dependency",
            Event::WaitForDependencies {
                dependencies: vec!["missing".into()]
            }
        ),
        Err(Error::Database(_))
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    assert_eq!(
        fixture.transaction().load(&"other".into()).unwrap().revision(),
        0
    );
    assert_eq!(fixture.count("lifecycle_events"), 1);
}

#[test]
fn input_payload_conflict_and_missing_payload_cannot_accept_a_continuation() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    fixture.card("task", "question");
    let mut tx = fixture.transaction();
    assert!(matches!(
        tx.put_input(
            &"task".into(),
            InputPayload::new(id("initial"), "Different bytes").unwrap()
        ),
        Err(Error::InputConflict)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let mut tx = fixture.transaction();
    tx.apply(
        &"task".into(),
        0,
        "ask",
        Event::Ask {
            card: "question".into(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let mut tx = fixture.transaction();
    assert!(matches!(
        tx.apply(
            &"task".into(),
            1,
            "answer",
            Event::Answer {
                card: "question".into(),
                input: id("missing")
            }
        ),
        Err(Error::MissingInput)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    assert_eq!(
        fixture.transaction().load(&"task".into()).unwrap().revision(),
        1
    );
}

#[test]
fn corrupt_journal_cursor_and_execution_projection_fail_closed() {
    for (guard, mutation) in [
        (
            None,
            "UPDATE lifecycle_tasks SET revision=2 WHERE task_id='task'",
        ),
        (
            Some("lifecycle_event_immutable"),
            "UPDATE lifecycle_events SET event=json_set(event,'$.data.instance','forged') WHERE task_id='task'",
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.create("task");
        drop(fixture.launch("task"));
        if let Some(guard) = guard {
            corrupt_while_guard_removed(&fixture.connection, guard, || {
                fixture.connection.execute_batch(mutation).unwrap();
            });
        } else {
            fixture.connection.execute_batch(mutation).unwrap();
        }
        assert!(matches!(
            fixture.transaction().load(&"task".into()),
            Err(Error::CorruptJournal)
        ));
    }
    let mut fixture = Fixture::new();
    fixture.create("task");
    drop(fixture.launch("task"));
    assert!(
        fixture
            .connection
            .execute(
                "UPDATE lifecycle_current_execution SET node_id='second' WHERE task_id='task'",
                []
            )
            .is_err()
    );
}

#[test]
fn launch_requires_exact_owned_worktree_and_no_disabled_fk_connection() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let mut tx = fixture.transaction();
    tx.apply(&"task".into(), 0, "admit", admit("node", "instance"))
        .unwrap();
    assert!(matches!(
        tx.stage_launch(
            &"task".into(),
            &Placement::LocalDirectory("/unowned".into()),
            &test_binding()
        ),
        Err(Error::MissingIntent)
    ));
    drop(tx);
    fixture
        .connection
        .execute_batch("PRAGMA foreign_keys=OFF")
        .unwrap();
    let tx = fixture.connection.transaction().unwrap();
    assert!(matches!(
        LifecycleTransaction::attach(tx, scope()),
        Err(Error::InvalidInput)
    ));
}

#[test]
fn required_delivery_cannot_change_kind_and_replay_checks_semantics() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    drop(fixture.launch("task"));
    fixture.connection.execute("INSERT INTO interactions(id,scope_kind,task_id,author_kind,recipient_kind,body,blocking,kind,revision,ordering,is_final,withdrawn,card) VALUES('delivery','task','task','system','user','Result',0,'delivery',0,0,0,0,'{\"kind\":\"provide\",\"need\":\"Review\"}')", []).unwrap();
    let mut tx = fixture.transaction();
    tx.apply(
        &"task".into(),
        1,
        "present",
        Event::Present {
            delivery: "delivery".into(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let mutation =
        "UPDATE interactions SET kind='question',options='[]',is_final=NULL WHERE id='delivery'";
    assert!(fixture.connection.execute(mutation, []).is_err());
    corrupt_while_guard_removed(
        &fixture.connection,
        "lifecycle_required_delivery_retained",
        || {
            fixture.connection.execute(mutation, []).unwrap();
        },
    );
    assert!(matches!(
        fixture.transaction().load(&"task".into()),
        Err(Error::InvalidCard)
    ));
}

#[test]
fn proven_preoffer_rejection_preserves_original_input_and_requires_explicit_recovery() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let permit = fixture.launch("task");
    let mut tx = fixture.transaction();
    tx.claim(permit).unwrap();
    let attempt = tx.commit().unwrap().attempts.pop().unwrap();
    let mut tx = fixture.transaction();
    tx.record_receipt(attempt, DispatchReceipt::Rejected)
        .unwrap();
    tx.commit().unwrap();
    let mut tx = fixture.transaction();
    let aggregate = tx.load(&"task".into()).unwrap();
    assert_eq!(aggregate.pending_inputs(), vec![&id("initial")]);
    assert_eq!(aggregate.resource(), Some(ResourceEvidence::Reserved));
    tx.recover_prepared(&"task".into()).unwrap();
    assert!(tx.commit().unwrap().permits.is_empty());
    // This slice deliberately exposes no unsafe new-generation retry for that input.
}

#[test]
fn generation_replacement_rejects_old_permit_and_preserves_new_input() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let old = fixture.launch("task");
    fixture.runtime("task", RuntimeFact::ResourceAvailable);
    fixture.runtime(
        "task",
        RuntimeFact::TurnStarted {
            turn: TurnId::new("turn").unwrap(),
            trigger: Trigger::Input(id("initial")),
        },
    );
    fixture.card("task", "recovery");
    fixture.runtime(
        "task",
        RuntimeFact::ResourceReleased {
            problem_card: "recovery".into(),
        },
    );
    let path = fixture.directory.join("task");
    let mut tx = fixture.transaction();
    tx.put_input(&"task".into(), payload("continuation"))
        .unwrap();
    tx.apply(
        &"task".into(),
        4,
        "retire",
        Event::RetireReleasedExecution {
            problem_card: "recovery".into(),
            input: id("continuation"),
        },
    )
    .unwrap();
    tx.settle_card(&"task".into(), &"recovery".into(), CardSettlement::Withdraw)
        .unwrap();
    tx.apply(
        &"task".into(),
        5,
        "replacement",
        admit("node", "replacement-instance"),
    )
    .unwrap();
    tx.stage_launch(&"task".into(), &local(&path), &test_binding())
        .unwrap();
    let fresh = tx.commit().unwrap().permits.pop().unwrap();
    assert_eq!(fresh.execution().generation(), 2);
    let mut tx = fixture.transaction();
    assert!(matches!(tx.claim(old), Err(Error::StalePermit)));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let mut tx = fixture.transaction();
    tx.claim(fresh).unwrap();
    let attempt = tx.commit().unwrap().attempts.pop().unwrap();
    assert_eq!(attempt.input().id(), &id("continuation"));
}

#[test]
fn answering_a_required_card_requires_its_settlement_in_the_same_commit() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    fixture.card("task", "question");
    let mut tx = fixture.transaction();
    tx.apply(
        &"task".into(),
        0,
        "ask",
        Event::Ask {
            card: "question".into(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let mut tx = fixture.transaction();
    tx.put_input(&"task".into(), payload("answer")).unwrap();
    tx.apply(
        &"task".into(),
        1,
        "answer",
        Event::Answer {
            card: "question".into(),
            input: id("answer"),
        },
    )
    .unwrap();
    assert!(matches!(tx.commit(), Err(Error::InvalidCard)));
    assert_eq!(
        fixture
            .transaction()
            .load(&"task".into())
            .unwrap()
            .cards()
            .len(),
        1
    );
}

#[test]
fn uncertain_dispatch_prevents_retiring_a_binding_even_after_exit_observation() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let permit = fixture.launch("task");
    let mut tx = fixture.transaction();
    tx.claim(permit).unwrap();
    drop(tx.commit().unwrap().attempts.pop().unwrap());
    fixture.runtime("task", RuntimeFact::ResourceAvailable);
    fixture.runtime(
        "task",
        RuntimeFact::TurnStarted {
            turn: TurnId::new("turn").unwrap(),
            trigger: Trigger::Input(id("initial")),
        },
    );
    fixture.card("task", "recovery");
    fixture.runtime(
        "task",
        RuntimeFact::ResourceReleased {
            problem_card: "recovery".into(),
        },
    );
    let mut tx = fixture.transaction();
    tx.put_input(&"task".into(), payload("continuation"))
        .unwrap();
    assert!(matches!(
        tx.apply(
            &"task".into(),
            4,
            "retire",
            Event::RetireReleasedExecution {
                problem_card: "recovery".into(),
                input: id("continuation")
            }
        ),
        Err(Error::UnresolvedDispatch)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    assert_eq!(
        fixture
            .transaction()
            .load(&"task".into())
            .unwrap()
            .execution()
            .unwrap()
            .generation(),
        1
    );
}

#[test]
fn free_text_question_with_no_buttons_is_still_actionable() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    fixture.card("task", "question");
    fixture
        .connection
        .execute(
            "UPDATE interactions SET options='[]' WHERE id='question'",
            [],
        )
        .unwrap();
    let mut tx = fixture.transaction();
    tx.apply(
        &"task".into(),
        0,
        "ask",
        Event::Ask {
            card: "question".into(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(
        fixture
            .transaction()
            .load(&"task".into())
            .unwrap()
            .cards()
            .len(),
        1
    );
    assert!(
        fixture
            .connection
            .execute(
                "UPDATE interactions SET card='{\"kind\":\"provide\"}' WHERE id='question'",
                []
            )
            .is_err()
    );
    assert!(
        fixture
            .connection
            .execute(
                "UPDATE interactions SET options='{}' WHERE id='question'",
                []
            )
            .is_err()
    );
}

#[test]
fn replay_preserves_own_background_work_as_working_while_permission_waits() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    drop(fixture.launch("task"));
    fixture.runtime("task", RuntimeFact::ResourceAvailable);
    fixture.runtime(
        "task",
        RuntimeFact::TurnStarted {
            turn: TurnId::new("turn").unwrap(),
            trigger: Trigger::Input(id("initial")),
        },
    );
    fixture.runtime(
        "task",
        RuntimeFact::WorkStarted {
            turn: TurnId::new("turn").unwrap(),
            work: WorkId::new("child").unwrap(),
        },
    );
    fixture.card("task", "permission");
    fixture.runtime(
        "task",
        RuntimeFact::Ask {
            card: "permission".into(),
            callback: CallbackId::new("callback").unwrap(),
            reason: WaitReason::Permission,
        },
    );
    fixture.runtime(
        "task",
        RuntimeFact::TurnEnded {
            turn: TurnId::new("turn").unwrap(),
            unheard_card: "not-created".into(),
        },
    );
    let tx = fixture.transaction();
    let aggregate = tx.load(&"task".into()).unwrap();
    assert!(aggregate.holds_slot());
    assert_eq!(aggregate.activity(), ActivityView::OwnWork);
    assert_eq!(
        aggregate.cards(),
        vec![(&"permission".into(), WaitReason::Permission)]
    );
}

#[test]
fn accepted_replies_offer_in_inbox_order_even_if_intents_are_staged_backwards() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    drop(fixture.launch("task"));
    fixture.runtime("task", RuntimeFact::ResourceAvailable);
    fixture.runtime(
        "task",
        RuntimeFact::TurnStarted {
            turn: TurnId::new("turn").unwrap(),
            trigger: Trigger::Input(id("initial")),
        },
    );
    fixture.card("task", "question-first");
    fixture.card("task", "question-second");
    let mut tx = fixture.transaction();
    let task = TaskId::from("task");
    tx.apply(
        &task,
        3,
        "ask-first",
        Event::Ask {
            card: "question-first".into(),
        },
    )
    .unwrap();
    tx.apply(
        &task,
        4,
        "ask-second",
        Event::Ask {
            card: "question-second".into(),
        },
    )
    .unwrap();
    for (revision, card, input) in [
        (5, "question-first", "reply-first"),
        (6, "question-second", "reply-second"),
    ] {
        tx.put_input(&task, payload(input)).unwrap();
        tx.apply(
            &task,
            revision,
            input,
            Event::Answer {
                card: card.into(),
                input: id(input),
            },
        )
        .unwrap();
        tx.settle_card(
            &task,
            &card.into(),
            CardSettlement::Answer(CardAnswer::from_object(serde_json::Map::new()).unwrap()),
        )
        .unwrap();
    }
    tx.stage_input(&task, &id("reply-second")).unwrap();
    tx.stage_input(&task, &id("reply-first")).unwrap();
    let mut committed = tx.commit().unwrap();
    assert_eq!(
        committed.permits.len(),
        1,
        "later reply must have no independently usable permit"
    );
    let first = committed.permits.pop().unwrap();
    assert_eq!(first.input(), &id("reply-first"));
    let mut tx = fixture.transaction();
    tx.claim(first).unwrap();
    let first = tx.commit().unwrap().attempts.pop().unwrap();
    assert_eq!(first.input().text(), "Full prompt for reply-first");
    assert_eq!(first.kind(), DispatchKind::Input);
    assert_eq!(first.working_directory(), None);
    let mut tx = fixture.transaction();
    tx.recover_prepared(&task).unwrap();
    assert!(
        tx.commit().unwrap().permits.is_empty(),
        "no second claim before first is proven handed/consumed"
    );
    let mut tx = fixture.transaction();
    tx.record_receipt(first, DispatchReceipt::Handed).unwrap();
    let second = tx.commit().unwrap().permits.pop().unwrap();
    assert_eq!(second.input(), &id("reply-second"));
    let mut tx = fixture.transaction();
    tx.claim(second).unwrap();
    let second = tx.commit().unwrap().attempts.pop().unwrap();
    assert_eq!(second.input().text(), "Full prompt for reply-second");
}

#[test]
fn same_business_input_id_on_two_tasks_cannot_cross_bind_dispatch_payloads() {
    let mut fixture = Fixture::new();
    fixture.add_task("first-task");
    fixture.add_task("second-task");
    let first_path = fixture.directory.join("first-task");
    let second_path = fixture.directory.join("second-task");
    let mut tx = fixture.transaction();
    for (task, node, instance, text, path) in [
        (
            "first-task",
            "node",
            "first-instance",
            "First task prompt",
            first_path.as_path(),
        ),
        (
            "second-task",
            "second",
            "second-instance",
            "Second task prompt",
            second_path.as_path(),
        ),
    ] {
        tx.create(
            &task.into(),
            InputPayload::new(id("shared-business-id"), text).unwrap(),
        )
        .unwrap();
        tx.apply(&task.into(), 0, "admit", admit(node, instance))
            .unwrap();
        tx.stage_launch(&task.into(), &local(path), &test_binding())
            .unwrap();
    }
    let permits = tx.commit().unwrap().permits;
    assert_eq!(permits.len(), 2);
    for permit in permits {
        let task = permit.execution().task().clone();
        let mut tx = fixture.transaction();
        tx.claim(permit).unwrap();
        let attempt = tx.commit().unwrap().attempts.pop().unwrap();
        assert_eq!(attempt.execution().task(), &task);
        assert_eq!(attempt.input().id(), &id("shared-business-id"));
        assert_eq!(
            attempt.input().text(),
            if task.0 == "first-task" {
                "First task prompt"
            } else {
                "Second task prompt"
            }
        );
    }
}

fn rust_whitespace() -> String {
    (0..=u32::from(char::MAX))
        .filter_map(char::from_u32)
        .filter(|character| character.is_whitespace())
        .collect()
}

fn unusable_needs() -> Vec<String> {
    vec![
        "\n".into(),
        "\u{a0}".into(),
        "\u{2003}".into(),
        rust_whitespace(),
        "Do\nthis".into(),
        "x".repeat(91),
        "Do\0this".into(),
    ]
}
fn change_need(fixture: &Fixture, need: &str) -> rusqlite::Result<usize> {
    fixture.connection.execute(
        "UPDATE interactions SET card=json_set(card,'$.need',?1) WHERE id='question'",
        params![need],
    )
}
fn require_question(fixture: &mut Fixture) {
    let mut tx = fixture.transaction();
    tx.apply(
        &"task".into(),
        0,
        "ask",
        Event::Ask {
            card: "question".into(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
}

#[test]
fn unusable_need_cannot_satisfy_a_required_card() {
    for need in unusable_needs() {
        let mut fixture = Fixture::new();
        fixture.create("task");
        fixture.card("task", "question");
        change_need(&fixture, &need).unwrap();
        let mut tx = fixture.transaction();
        assert!(
            matches!(
                tx.apply(
                    &"task".into(),
                    0,
                    "ask",
                    Event::Ask {
                        card: "question".into(),
                    },
                ),
                Err(Error::InvalidCard)
            ),
            "unusable need must not stop an open task: {need:?}"
        );
    }
}

#[test]
fn required_card_need_cannot_be_updated_to_unusable_text() {
    for need in unusable_needs() {
        let mut fixture = Fixture::new();
        fixture.create("task");
        fixture.card("task", "question");
        require_question(&mut fixture);
        assert!(
            change_need(&fixture, &need).is_err(),
            "required card mutation must preserve a usable need: {need:?}"
        );
    }
}

#[test]
fn restore_rechecks_required_card_need_after_guard_corruption() {
    for need in unusable_needs() {
        let mut fixture = Fixture::new();
        fixture.create("task");
        fixture.card("task", "question");
        require_question(&mut fixture);
        corrupt_while_guard_removed(
            &fixture.connection,
            "lifecycle_required_card_stays_actionable",
            || {
                change_need(&fixture, &need).unwrap();
            },
        );
        assert!(
            matches!(
                fixture.transaction().load(&"task".into()),
                Err(Error::InvalidCard)
            ),
            "restore must reject an unusable required need: {need:?}"
        );
    }
}

#[test]
fn trimmed_card_need_accepts_unicode_borders_line_endings_and_ninety_characters() {
    for need in [
        "\nDo this\n".into(),
        "\u{a0}\u{2003}Do this\u{2003}\u{a0}".into(),
        format!("\n\u{a0}{}\u{a0}\n", "x".repeat(90)),
        format!(
            "{}{}{}",
            rust_whitespace(),
            "x".repeat(90),
            rust_whitespace()
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.create("task");
        fixture.card("task", "question");
        change_need(&fixture, &need).unwrap();
        require_question(&mut fixture);
        assert_eq!(
            fixture
                .transaction()
                .load(&"task".into())
                .unwrap()
                .cards()
                .len(),
            1
        );
        assert_eq!(change_need(&fixture, &need).unwrap(), 1);
    }
}

#[test]
fn invalid_persisted_identity_preserves_the_domain_error_category() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    fixture.connection.execute(
        "INSERT INTO lifecycle_inputs(task_id,input_id,body) VALUES('task','  ','Retained corrupt input')", [],
    ).unwrap();
    fixture
        .connection
        .execute(
            "UPDATE lifecycle_tasks SET initial_input='  ' WHERE task_id='task'",
            [],
        )
        .unwrap();
    assert!(matches!(
        fixture.transaction().load(&"task".into()),
        Err(Error::Domain(Refusal::EmptyIdentity))
    ));
}

#[test]
fn caught_conflicting_binding_cannot_commit_orphan_identity_or_conversation() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    drop(fixture.launch("task"));
    let alternate = ProviderBinding::new(
        "alternate-binding".into(),
        "alternate-driver".into(),
        "alternate-instance".into(),
        "alternate-config".into(),
        1,
        1,
        BTreeSet::from([Capability::OfferInput]),
        "alternate-credential".into(),
        1,
    )
    .unwrap();
    let mut tx = fixture.transaction();
    assert!(matches!(
        tx.bind_provider(&"task".into(), &alternate),
        Err(Error::BindingConflict)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let database = fixture.directory.join("domain.db");
    drop(fixture.connection);
    let reopened = Connection::open(database).unwrap();
    for table in [
        "provider_bindings",
        "provider_active_bindings",
        "provider_conversations",
    ] {
        let count: i64 = reopened
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 1,
            "caught conflict cannot leave an orphan row in {table}"
        );
    }
    let binding: String = reopened
        .query_row(
            "SELECT binding_id FROM lifecycle_execution_bindings WHERE task_id='task'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(binding, test_binding().id());
    let body: String = reopened
        .query_row(
            "SELECT body FROM lifecycle_inputs WHERE task_id='task' AND input_id='initial'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(body, payload("initial").text());
}

#[test]
fn caught_observation_head_failure_does_not_commit_receipt_without_cursor() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    drop(fixture.launch("task"));
    fixture.connection.execute_batch("CREATE TRIGGER fail_observation_head BEFORE INSERT ON provider_observation_heads BEGIN SELECT RAISE(ABORT, 'fixture head write rejected'); END;").unwrap();
    let mut tx = fixture.transaction();
    let stamp = tx
        .load(&"task".into())
        .unwrap()
        .execution()
        .unwrap()
        .clone();
    let error = tx
        .accept_observation(
            &stamp,
            1,
            CommandEvidence::new(b"actual technical evidence".to_vec()).unwrap(),
        )
        .unwrap_err();
    assert!(
        matches!(&error, Error::Database(rusqlite::Error::SqliteFailure(_, Some(message))) if message == "fixture head write rejected")
    );
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let database = fixture.directory.join("domain.db");
    drop(fixture.connection);
    let reopened = Connection::open(database).unwrap();
    let receipts: i64 = reopened
        .query_row("SELECT count(*) FROM provider_observations", [], |row| {
            row.get(0)
        })
        .unwrap();
    let heads: i64 = reopened
        .query_row(
            "SELECT count(*) FROM provider_observation_heads",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        receipts, 0,
        "failed cursor write must roll back its receipt even when the caller catches the error"
    );
    assert_eq!(heads, 0);
    let revision: i64 = reopened
        .query_row(
            "SELECT revision FROM lifecycle_tasks WHERE task_id='task'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(revision, 1);
}

#[test]
fn owner_grants_are_postcommit_strict_cas_and_old_grants_never_reconnect() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let permit = fixture.launch("task");
    let stamp = permit.execution().clone();
    let mut tx = fixture.transaction();
    tx.reserve_owner(&stamp, 0).unwrap();
    assert!(matches!(
        tx.reserve_owner(&stamp, 0),
        Err(Error::StaleOwner)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    assert_eq!(fixture.count("provider_execution_owners"), 0);
    let mut tx = fixture.transaction();
    tx.reserve_owner(&stamp, 0).unwrap();
    let first = tx.commit().unwrap().owners.pop().unwrap();
    assert_eq!(first.generation(), 1);
    assert_eq!(first.execution(), &stamp);
    assert_eq!(first.binding(), &test_binding());
    let mut tx = fixture.transaction();
    tx.fence_owner(&first).unwrap();
    assert!(matches!(
        tx.reserve_owner(&stamp, 0),
        Err(Error::StaleOwner)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let mut tx = fixture.transaction();
    tx.fence_owner(&first).unwrap();
    tx.reserve_owner(&stamp, 1).unwrap();
    let second = tx.commit().unwrap().owners.pop().unwrap();
    assert_eq!(second.generation(), 2);
    let tx = fixture.transaction();
    assert!(matches!(tx.fence_owner(&first), Err(Error::StaleOwner)));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let tx = fixture.transaction();
    tx.fence_owner(&second).unwrap();
    tx.commit().unwrap();
}

#[test]
fn ownership_after_revocation_allows_control_but_never_new_work() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let permit = fixture.launch("task");
    let stamp = permit.execution().clone();
    let mut tx = fixture.transaction();
    tx.reserve_owner(&stamp, 0).unwrap();
    tx.revoke_provider(&test_binding()).unwrap();
    let mut committed = tx.commit().unwrap();
    let grant = committed.owners.pop().unwrap();
    assert_eq!(
        grant.generation(),
        1,
        "control authority remains available for observation and drain"
    );
    assert!(committed.permits.is_empty());
    let mut tx = fixture.transaction();
    tx.fence_owner(&grant).unwrap();
    assert!(matches!(tx.claim(permit), Err(Error::BindingRevoked)));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let pending = fixture
        .transaction()
        .load(&"task".into())
        .unwrap()
        .pending_inputs()
        .len();
    assert_eq!(
        pending, 1,
        "revocation cannot invent consumption of the original input"
    );
}

#[test]
fn failed_actual_sql_commit_rolls_back_owner_without_emitting_grant() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let stamp = fixture.launch("task").execution().clone();
    let raw = fixture
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    raw.execute(
        "UPDATE tasks SET final_presentation='missing' WHERE id='task'",
        [],
    )
    .unwrap();
    let mut tx = LifecycleTransaction::attach(raw, scope()).unwrap();
    tx.reserve_owner(&stamp, 0).unwrap();
    assert!(matches!(tx.commit(), Err(Error::Database(_))));
    let count: i64 = fixture
        .connection
        .query_row(
            "SELECT count(*) FROM provider_execution_owners",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    let mut tx = fixture.transaction();
    tx.reserve_owner(&stamp, 0).unwrap();
    assert_eq!(tx.commit().unwrap().owners.pop().unwrap().generation(), 1);
}

#[test]
fn missing_execution_binding_after_reopen_never_selects_another_provider() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let stamp = fixture.launch("task").execution().clone();
    corrupt_while_guard_removed(&fixture.connection, "execution_binding_retained", || {
        fixture
            .connection
            .execute(
                "DELETE FROM lifecycle_execution_bindings WHERE task_id='task'",
                [],
            )
            .unwrap();
    });
    let database = fixture.directory.join("domain.db");
    drop(fixture.connection);
    let mut reopened = Connection::open(database).unwrap();
    reopened.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    let tx = LifecycleTransaction::attach(reopened.transaction().unwrap(), scope()).unwrap();
    assert!(matches!(
        tx.execution_binding(&stamp),
        Err(Error::MissingBinding)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let mut tx = LifecycleTransaction::attach(reopened.transaction().unwrap(), scope()).unwrap();
    assert!(matches!(
        tx.reserve_owner(&stamp, 0),
        Err(Error::MissingBinding)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let retained: i64 = reopened
        .query_row("SELECT count(*) FROM provider_bindings", [], |row| {
            row.get(0)
        })
        .unwrap();
    let inputs: i64 = reopened
        .query_row(
            "SELECT count(*) FROM lifecycle_inputs WHERE task_id='task'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let owners: i64 = reopened
        .query_row(
            "SELECT count(*) FROM provider_execution_owners",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retained, 1);
    assert_eq!(inputs, 1);
    assert_eq!(owners, 0);
}

#[test]
fn normalization_checkpoint_cursor_identity_capacity_and_digest_fail_closed() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let stamp = fixture.launch("task").execution().clone();
    let mut tx = fixture.transaction();
    assert!(tx.normalization_checkpoint(&stamp).unwrap().is_none());
    assert!(matches!(
        tx.put_normalization_checkpoint(&stamp, 1, None, b"no raw evidence".to_vec()),
        Err(Error::CheckpointConflict)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let mut tx = fixture.transaction();
    tx.accept_observation(
        &stamp,
        1,
        CommandEvidence::new(b"native evidence one".to_vec()).unwrap(),
    )
    .unwrap();
    tx.put_normalization_checkpoint(&stamp, 1, None, b"causal pending input map".to_vec())
        .unwrap();
    tx.commit().unwrap();
    let mut tx = fixture.transaction();
    tx.put_normalization_checkpoint(&stamp, 1, None, b"causal pending input map".to_vec())
        .unwrap();
    tx.commit().unwrap();
    let mut tx = fixture.transaction();
    assert!(matches!(
        tx.put_normalization_checkpoint(&stamp, 1, Some(1), b"different snapshot".to_vec()),
        Err(Error::CheckpointConflict)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let mut tx = fixture.transaction();
    assert!(matches!(
        tx.put_normalization_checkpoint(&stamp, 0, Some(1), b"old cursor".to_vec()),
        Err(Error::MetadataCapacity)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let mut tx = fixture.transaction();
    assert!(matches!(
        tx.put_normalization_checkpoint(&stamp, 2, Some(1), vec![0; 65537]),
        Err(Error::MetadataCapacity)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let database = fixture.directory.join("domain.db");
    drop(fixture.connection);
    let mut reopened = Connection::open(database).unwrap();
    reopened.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    let tx = LifecycleTransaction::attach(reopened.transaction().unwrap(), scope()).unwrap();
    let snapshot = tx.normalization_checkpoint(&stamp).unwrap().unwrap();
    assert_eq!(snapshot.cursor(), 1);
    assert_eq!(snapshot.bytes(), b"causal pending input map");
    tx.commit().unwrap();
    corrupt_while_guard_removed(&reopened, "normalization_checkpoint_monotonic", || {
        reopened
            .execute(
                "UPDATE provider_normalization_checkpoints SET digest=zeroblob(32)",
                [],
            )
            .unwrap();
    });
    let tx = LifecycleTransaction::attach(reopened.transaction().unwrap(), scope()).unwrap();
    assert!(matches!(
        tx.normalization_checkpoint(&stamp),
        Err(Error::CorruptJournal)
    ));
}

#[test]
fn caught_checkpoint_cas_rejection_cannot_change_durable_snapshot() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let stamp = fixture.launch("task").execution().clone();
    let mut tx = fixture.transaction();
    tx.accept_observation(
        &stamp,
        1,
        CommandEvidence::new(b"evidence one".to_vec()).unwrap(),
    )
    .unwrap();
    tx.put_normalization_checkpoint(&stamp, 1, None, b"snapshot one".to_vec())
        .unwrap();
    tx.accept_observation(
        &stamp,
        2,
        CommandEvidence::new(b"evidence two".to_vec()).unwrap(),
    )
    .unwrap();
    tx.put_normalization_checkpoint(&stamp, 2, Some(1), b"snapshot two".to_vec())
        .unwrap();
    tx.commit().unwrap();
    let mut tx = fixture.transaction();
    tx.accept_observation(
        &stamp,
        3,
        CommandEvidence::new(b"valid new raw head".to_vec()).unwrap(),
    )
    .unwrap();
    tx.commit().unwrap();
    let mut tx = fixture.transaction();
    assert!(matches!(
        tx.put_normalization_checkpoint(&stamp, 3, Some(1), b"stale writer".to_vec()),
        Err(Error::CheckpointConflict)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let database = fixture.directory.join("domain.db");
    drop(fixture.connection);
    let mut reopened = Connection::open(database).unwrap();
    reopened.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    let tx = LifecycleTransaction::attach(reopened.transaction().unwrap(), scope()).unwrap();
    let snapshot = tx.normalization_checkpoint(&stamp).unwrap().unwrap();
    assert_eq!(snapshot.cursor(), 2);
    assert_eq!(snapshot.bytes(), b"snapshot two");
}

#[test]
fn historical_input_bindings_remain_immutable_and_do_not_limit_scoped_state_loading() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let stamp = fixture.launch("task").execution().clone();
    let mut tx = fixture.transaction();
    tx.put_adapter_conversation(
        &stamp,
        AdapterInputMetadata::new(b"native session reference".to_vec()).unwrap(),
    )
    .unwrap();
    tx.put_adapter_input(
        &stamp,
        &id("initial"),
        AdapterInputMetadata::new(b"immutable initial native key".to_vec()).unwrap(),
    )
    .unwrap();
    tx.reserve_owner(&stamp, 0).unwrap();
    let grant = tx.commit().unwrap().owners.pop().unwrap();
    let mut tx = fixture.transaction();
    assert!(matches!(
        tx.put_adapter_input(
            &stamp,
            &id("initial"),
            AdapterInputMetadata::new(b"replacement UUID forbidden".to_vec()).unwrap()
        ),
        Err(Error::InputConflict)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let mut tx = fixture.transaction();
    assert!(matches!(
        tx.put_adapter_conversation(
            &stamp,
            AdapterInputMetadata::new(b"regenerated session forbidden".to_vec()).unwrap()
        ),
        Err(Error::InputConflict)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let mut tx = fixture.transaction();
    for index in 1..256 {
        let input = format!("extra-{index}");
        tx.put_input(&"task".into(), payload(&input)).unwrap();
        tx.put_adapter_input(
            &stamp,
            &id(&input),
            AdapterInputMetadata::new(format!("native-{index}").into_bytes()).unwrap(),
        )
        .unwrap();
    }
    assert_eq!(
        tx.adapter_state(&grant).unwrap().conversation(),
        b"native session reference"
    );
    tx.put_input(&"task".into(), payload("overflow")).unwrap();
    tx.put_adapter_input(
        &stamp,
        &id("overflow"),
        AdapterInputMetadata::new(b"retained overflow key".to_vec()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        tx.adapter_state(&grant).unwrap().conversation(),
        b"native session reference"
    );
    let historical = tx
        .lookup_adapter_inputs(
            &stamp,
            &[b"native-1".to_vec(), b"retained overflow key".to_vec()],
        )
        .unwrap();
    assert_eq!(historical[0].input(), &id("extra-1"));
    assert_eq!(historical[1].input(), &id("overflow"));
    tx.commit().unwrap();
    let tx = fixture.transaction();
    assert!(matches!(
        tx.lookup_adapter_inputs(&stamp, &vec![b"native-1".to_vec(); 65]),
        Err(Error::MetadataCapacity)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let count: i64 = fixture
        .connection
        .query_row("SELECT count(*) FROM provider_input_metadata", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        count, 257,
        "historical identities are retained, not evicted to fit a startup cache"
    );
    let tx = fixture.transaction();
    assert_eq!(
        tx.adapter_input(&stamp, &id("initial")).unwrap().unwrap().bytes(),
        b"immutable initial native key"
    );
    assert_eq!(
        tx.adapter_conversation(&stamp).unwrap().unwrap().bytes(),
        b"native session reference"
    );
    assert_eq!(
        tx.adapter_state(&grant).unwrap().conversation(),
        b"native session reference"
    );
}


fn owned_claim(fixture: &mut Fixture) -> (DispatchAttempt, OwnerGrant) {
    fixture.create("task");
    let permit = fixture.launch("task");
    let stamp = permit.execution().clone();
    let mut tx = fixture.transaction();
    tx.reserve_owner(&stamp, 0).unwrap();
    let owner = tx.commit().unwrap().owners.pop().unwrap();
    let mut tx = fixture.transaction();
    tx.claim(permit).unwrap();
    let attempt = tx.commit().unwrap().attempts.pop().unwrap();
    (attempt, owner)
}

/// A claim whose command bytes are recorded and committed but whose receipt a crash lost: the
/// dispatch stays `claimed` with its exact bytes in `provider_commands`.
fn crashed_claim(fixture: &mut Fixture, bytes: &[u8]) -> OwnerGrant {
    let (attempt, owner) = owned_claim(fixture);
    let mut tx = fixture.transaction();
    tx.record_owned_command(attempt, owner.generation(), CommandEvidence::new(bytes.to_vec()).unwrap())
        .unwrap();
    tx.commit().unwrap();
    owner
}

#[test]
fn an_adopting_owner_reserves_the_generation_over_the_current_one() {
    let mut fixture = Fixture::new();
    let owner = crashed_claim(&mut fixture, b"framed command");
    let stamp = owner.execution().clone();
    let tx = fixture.transaction();
    assert_eq!(tx.owner_generation(&stamp).unwrap(), 1, "the launch owned it as generation 1");
    drop(tx);
    let mut tx = fixture.transaction();
    tx.reserve_owner(&stamp, 1).unwrap();
    let adopted = tx.commit().unwrap().owners.pop().unwrap();
    assert_eq!(adopted.generation(), 2, "adoption reserves the next generation");
    assert_eq!(fixture.transaction().owner_generation(&stamp).unwrap(), 2);
}

#[test]
fn unresolved_commands_return_claimed_sends_with_their_exact_bytes() {
    let mut fixture = Fixture::new();
    let _owner = crashed_claim(&mut fixture, b"the exact framed launch command");
    let tx = fixture.transaction();
    let unresolved = tx.unresolved_commands(&"task".into()).unwrap();
    assert_eq!(unresolved.len(), 1, "one claimed send waits to be reconciled");
    assert_eq!(unresolved[0].input, id("initial"));
    assert_eq!(unresolved[0].command, b"the exact framed launch command");
}

#[test]
fn resolve_dispatch_moves_a_claimed_send_only_under_the_current_owner_and_never_reoffers() {
    let mut fixture = Fixture::new();
    let owner = crashed_claim(&mut fixture, b"framed command");
    let stamp = owner.execution().clone();
    let intent = fixture.transaction().unresolved_commands(&"task".into()).unwrap()[0].intent_id;

    // Adopt as generation 2: the launch's generation 1 is now stale and cannot resolve the send.
    let mut tx = fixture.transaction();
    tx.reserve_owner(&stamp, 1).unwrap();
    let adopted = tx.commit().unwrap().owners.pop().unwrap();
    let mut tx = fixture.transaction();
    assert!(matches!(tx.resolve_dispatch(&stamp, intent, 1, DispatchReceipt::Handed), Err(Error::StaleOwner)));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));

    // Under the adopting generation, resolve as handed: the dispatch is handed, the input stays
    // pending (no invented consumption) and no new permit is offered for it.
    let mut tx = fixture.transaction();
    tx.resolve_dispatch(&stamp, intent, adopted.generation(), DispatchReceipt::Handed).unwrap();
    let committed = tx.commit().unwrap();
    assert!(committed.permits.is_empty(), "a handed send is never offered again");
    let tx = fixture.transaction();
    assert_eq!(tx.load(&"task".into()).unwrap().pending_inputs(), vec![&id("initial")], "a handed send does not consume its input");
    assert!(tx.unresolved_commands(&"task".into()).unwrap().is_empty(), "the claimed send is resolved");
    drop(tx);

    // Resolving it again finds nothing claimed.
    let mut tx = fixture.transaction();
    assert!(matches!(tx.resolve_dispatch(&stamp, intent, adopted.generation(), DispatchReceipt::Handed), Err(Error::StalePermit)));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
}

#[test]
fn an_uncertain_resolution_leaves_the_send_terminal_and_the_input_pending() {
    let mut fixture = Fixture::new();
    let owner = crashed_claim(&mut fixture, b"framed command");
    let stamp = owner.execution().clone();
    let intent = fixture.transaction().unresolved_commands(&"task".into()).unwrap()[0].intent_id;
    let mut tx = fixture.transaction();
    tx.reserve_owner(&stamp, 1).unwrap();
    let adopted = tx.commit().unwrap().owners.pop().unwrap();
    let mut tx = fixture.transaction();
    tx.resolve_dispatch(&stamp, intent, adopted.generation(), DispatchReceipt::Uncertain).unwrap();
    tx.commit().unwrap();
    let tx = fixture.transaction();
    assert!(tx.unresolved_commands(&"task".into()).unwrap().is_empty(), "an uncertain send is no longer claimed");
    assert_eq!(tx.unresolved(&"task".into()).unwrap().iter().filter(|d| d.uncertain_receipt).count(), 1, "it stays uncertain for the recovery card");
    assert_eq!(tx.load(&"task".into()).unwrap().pending_inputs(), vec![&id("initial")], "an uncertain send never invents consumption");
}

#[test]
fn command_commit_value_preserves_exact_claim_and_bytes_only_after_real_commit() {
    let mut fixture = Fixture::new();
    let (attempt, owner) = owned_claim(&mut fixture);
    let independent = Connection::open(fixture.directory.join("domain.db")).unwrap();
    let mut tx = fixture.transaction();
    tx.record_owned_command(
        attempt,
        owner.generation(),
        CommandEvidence::new(b"exact framed command".to_vec()).unwrap(),
    )
    .unwrap();
    let visible: i64 = independent
        .query_row("SELECT count(*) FROM provider_commands", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(visible, 0, "the independent reader cannot see staged bytes");
    let mut committed = tx.commit().unwrap();
    assert_eq!(committed.commands.len(), 1);
    let command = committed.commands.pop().unwrap();
    assert_eq!(command.bytes(), b"exact framed command");
    assert_eq!(command.attempt().input().text(), payload("initial").text());
    assert_eq!(command.attempt().execution(), owner.execution());
    assert_eq!(command.owner_generation(), owner.generation());
    let visible: i64 = independent
        .query_row("SELECT count(*) FROM provider_commands", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        visible, 1,
        "committed bytes are visible before the value can be submitted"
    );
    let mut tx = fixture.transaction();
    tx.fence_committed_command(&command).unwrap();
    tx.record_committed_receipt(command, DispatchReceipt::Handed)
        .unwrap();
    tx.commit().unwrap();
    let tx = fixture.transaction();
    assert_eq!(
        tx.load(&"task".into()).unwrap().pending_inputs(),
        vec![&id("initial")],
        "a technical receipt does not consume input"
    );
}
#[test]
fn deferred_command_commit_failure_releases_no_value_and_rolls_back_bytes() {
    let mut fixture = Fixture::new();
    let (attempt, owner) = owned_claim(&mut fixture);
    let raw =
        Transaction::new_unchecked(&fixture.connection, TransactionBehavior::Immediate).unwrap();
    raw.execute(
        "UPDATE tasks SET final_presentation='missing' WHERE id='task'",
        [],
    )
    .unwrap();
    let mut tx = LifecycleTransaction::attach(raw, scope()).unwrap();
    tx.record_owned_command(
        attempt,
        owner.generation(),
        CommandEvidence::new(b"retained intent".to_vec()).unwrap(),
    )
    .unwrap();
    assert!(
        matches!(tx.commit(), Err(Error::Database(_))),
        "a deferred FK refusal must not release a command value"
    );
    let count: i64 = fixture
        .connection
        .query_row("SELECT count(*) FROM provider_commands", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
    let tx = fixture.transaction();
    assert_eq!(
        tx.unresolved(&"task".into()).unwrap().len(),
        1,
        "the earlier committed claim stays uncertain, never reissued"
    );
}
#[test]
fn observation_commit_value_requires_exact_raw_and_normalization_checkpoint() {
    let mut fixture = Fixture::new();
    let (_attempt, owner) = owned_claim(&mut fixture);
    let mut tx = fixture.transaction();
    tx.accept_owned_observation(
        owner.execution(),
        owner.generation(),
        1,
        CommandEvidence::new(b"observed raw".to_vec()).unwrap(),
    )
    .unwrap();
    assert!(matches!(tx.commit(), Err(Error::CheckpointConflict)));
    let count: i64 = fixture
        .connection
        .query_row("SELECT count(*) FROM provider_observations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        count, 0,
        "missing causal checkpoint rolls back the raw cursor too"
    );
    let mut tx = fixture.transaction();
    tx.accept_owned_observation(
        owner.execution(),
        owner.generation(),
        1,
        CommandEvidence::new(b"observed raw".to_vec()).unwrap(),
    )
    .unwrap();
    tx.put_normalization_checkpoint(owner.execution(), 1, None, b"causal checkpoint".to_vec())
        .unwrap();
    tx.settle_observation(owner.execution(), 1, 0).unwrap();
    let mut committed = tx.commit().unwrap();
    assert_eq!(committed.observations.len(), 1);
    let value = committed.observations.pop().unwrap();
    assert!(value.matches_evidence(b"observed raw"));
    assert!(!value.matches_evidence(b"substituted raw"));
    assert_eq!(value.scope(), &scope());
    assert_eq!(value.execution(), owner.execution());
    assert_eq!(value.binding(), owner.binding());
    assert_eq!(value.owner_generation(), owner.generation());
    assert_eq!(value.cursor(), 1);
    let mut tx = fixture.transaction();
    assert_eq!(
        tx.accept_owned_observation(
            owner.execution(),
            owner.generation(),
            1,
            CommandEvidence::new(b"observed raw".to_vec()).unwrap()
        )
        .unwrap(),
        ObservationAcceptance::Duplicate
    );
    assert_eq!(
        tx.commit().unwrap().observations.len(),
        1,
        "a durable exact replay can yield only SQL acknowledgement evidence"
    );
}
#[test]
fn caught_stale_owner_admission_cannot_emit_observation_commit_value() {
    let mut fixture = Fixture::new();
    let (_attempt, owner) = owned_claim(&mut fixture);
    let mut tx = fixture.transaction();
    assert!(matches!(
        tx.accept_owned_observation(
            owner.execution(),
            owner.generation() + 1,
            1,
            CommandEvidence::new(b"raw".to_vec()).unwrap()
        ),
        Err(Error::StaleOwner)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let count: i64 = fixture
        .connection
        .query_row("SELECT count(*) FROM provider_observations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}
#[test]
fn command_commit_rechecks_revoked_binding_before_releasing_value() {
    let mut fixture = Fixture::new();
    let (attempt, owner) = owned_claim(&mut fixture);
    let mut tx = fixture.transaction();
    tx.record_owned_command(
        attempt,
        owner.generation(),
        CommandEvidence::new(b"command".to_vec()).unwrap(),
    )
    .unwrap();
    tx.revoke_provider(owner.binding()).unwrap();
    assert!(matches!(tx.commit(), Err(Error::BindingRevoked)));
    let count: i64 = fixture
        .connection
        .query_row("SELECT count(*) FROM provider_commands", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn owned_observation_commit_can_finish_cancel_drain_without_reopening_admission() {
    for stale_owner in [false, true] {
        let mut fixture = Fixture::new();
        fixture.create("task");
        let permit = fixture.launch("task");
        let stamp = permit.execution().clone();
        drop(permit);
        let mut tx = fixture.transaction();
        tx.reserve_owner(&stamp, 0).unwrap();
        let owner = tx.commit().unwrap().owners.pop().unwrap();
        fixture.runtime("task", RuntimeFact::ResourceAvailable);
        fixture.runtime(
            "task",
            RuntimeFact::TurnStarted {
                turn: TurnId::new("turn").unwrap(),
                trigger: Trigger::Input(id("initial")),
            },
        );
        let cleanup = OperationId::new("cleanup").unwrap();
        let mut tx = fixture.transaction();
        let revision = tx.load(&"task".into()).unwrap().revision();
        tx.apply(
            &"task".into(),
            revision,
            "cancel",
            Event::Finish {
                intent: FinishIntent::Cancel,
                cleanup: cleanup.clone(),
                post_merge: None,
            },
        )
        .unwrap();
        tx.apply(
            &"task".into(),
            revision + 1,
            "cleanup-ended",
            Event::DrainObserved {
                obligation: DrainObligation::Cleanup(cleanup),
            },
        )
        .unwrap();
        let before_retirement_revision = revision + 2;
        let aggregate = tx.load(&"task".into()).unwrap();
        let remaining = DrainObligation::Execution(stamp.instance().clone());
        assert_eq!(aggregate.phase(), TaskPhase::Finishing);
        assert_eq!(aggregate.drain().collect::<Vec<_>>(), vec![&remaining]);
        tx.commit().unwrap();
        let mut tx = fixture.transaction();
        assert!(tx.unresolved(&"task".into()).unwrap().is_empty());
        assert!(tx.load(&"task".into()).unwrap().pending_inputs().is_empty());
        tx.accept_owned_observation(
            &stamp,
            owner.generation(),
            1,
            CommandEvidence::new(b"retirement observation".to_vec()).unwrap(),
        )
        .unwrap();
        tx.put_normalization_checkpoint(&stamp, 1, None, b"normalized retirement".to_vec())
            .unwrap();
        tx.settle_observation(&stamp, 1, 0).unwrap();
        tx.apply(
            &"task".into(),
            before_retirement_revision,
            "execution-drained",
            Event::DrainObserved {
                obligation: DrainObligation::Execution(stamp.instance().clone()),
            },
        )
        .unwrap();
        if stale_owner {
            let changed = tx.connection.execute(
                "UPDATE provider_execution_owners SET owner_generation=owner_generation+1 WHERE task_id=?1 AND generation=?2 AND binding_id=?3 AND owner_generation=?4",
                params![stamp.task().0, i64::try_from(stamp.generation()).unwrap(), owner.binding().id(), i64::try_from(owner.generation()).unwrap()],
            ).unwrap();
            assert_eq!(
                changed, 1,
                "the current durable owner fence really advances"
            );
        }
        let result = tx.commit();
        let observation = if stale_owner {
            assert!(
                matches!(result, Err(Error::StaleOwner)),
                "retirement must not bypass the current durable owner generation"
            );
            None
        } else {
            assert!(
                result.is_ok(),
                "accepted raw/checkpoint must commit with final cancellation drain: {result:?}"
            );
            let mut committed = result.unwrap();
            assert_eq!(committed.observations.len(), 1);
            assert!(committed.commands.is_empty());
            assert!(committed.attempts.is_empty());
            assert!(committed.permits.is_empty());
            Some(committed.observations.pop().unwrap())
        };
        let database = fixture.directory.join("domain.db");
        drop(fixture.connection);
        let reopened = Connection::open(database).unwrap();
        reopened.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        let rows: i64 = reopened
            .query_row("SELECT count(*) FROM provider_observations", [], |row| {
                row.get(0)
            })
            .unwrap();
        let checkpoints: i64 = reopened
            .query_row(
                "SELECT count(*) FROM provider_normalization_checkpoints",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let durable_owner: i64 = reopened
            .query_row(
                "SELECT owner_generation FROM provider_execution_owners WHERE task_id='task'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            durable_owner,
            i64::try_from(owner.generation()).unwrap(),
            "failed owner CAS is rolled back too"
        );
        let raw = Transaction::new_unchecked(&reopened, TransactionBehavior::Immediate).unwrap();
        let tx = LifecycleTransaction::attach(raw, scope()).unwrap();
        let aggregate = tx.load(&"task".into()).unwrap();
        if stale_owner {
            assert_eq!(rows, 0);
            assert_eq!(checkpoints, 0);
            assert_eq!(aggregate.execution(), Some(&stamp));
            assert_eq!(aggregate.revision(), before_retirement_revision);
            tx.fence_owner(&owner).unwrap();
            assert!(tx.normalization_checkpoint(&stamp).unwrap().is_none());
            assert_eq!(aggregate.phase(), TaskPhase::Finishing);
            assert!(tx.commit().unwrap().observations.is_empty());
        } else {
            assert_eq!(rows, 1);
            assert_eq!(checkpoints, 1);
            assert!(aggregate.execution().is_none());
            assert_eq!(aggregate.phase(), TaskPhase::Cancelled);
            assert!(aggregate.drain().next().is_none());
            let value = observation.unwrap();
            assert_eq!(value.scope(), &scope());
            assert_eq!(value.execution(), &stamp);
            assert_eq!(value.binding(), owner.binding());
            assert_eq!(value.owner_generation(), owner.generation());
            assert_eq!(value.cursor(), 1);
            assert!(value.matches_evidence(b"retirement observation"));
            assert!(!value.matches_evidence(b"different bytes"));
            assert!(tx.commit().unwrap().observations.is_empty());
            let raw =
                Transaction::new_unchecked(&reopened, TransactionBehavior::Immediate).unwrap();
            let mut tx = LifecycleTransaction::attach(raw, scope()).unwrap();
            assert!(
                matches!(
                    tx.accept_owned_observation(
                        &stamp,
                        owner.generation(),
                        2,
                        CommandEvidence::new(b"new late evidence".to_vec()).unwrap()
                    ),
                    Err(Error::StalePermit)
                ),
                "new observation admission still requires the current execution"
            );
            assert!(matches!(tx.commit(), Err(Error::Poisoned)));
            let raw =
                Transaction::new_unchecked(&reopened, TransactionBehavior::Immediate).unwrap();
            let tx = LifecycleTransaction::attach(raw, scope()).unwrap();
            assert!(
                matches!(tx.normalization_checkpoint(&stamp), Err(Error::StalePermit)),
                "the public metadata read does not become a historical admission API"
            );
            assert!(matches!(tx.commit(), Err(Error::Poisoned)));
            let raw =
                Transaction::new_unchecked(&reopened, TransactionBehavior::Immediate).unwrap();
            let mut tx = LifecycleTransaction::attach(raw, scope()).unwrap();
            assert!(
                matches!(
                    tx.put_normalization_checkpoint(
                        &stamp,
                        1,
                        Some(1),
                        b"normalized retirement".to_vec()
                    ),
                    Err(Error::StalePermit)
                ),
                "a later transaction has no pending owned observation for a historical checkpoint"
            );
            assert!(matches!(tx.commit(), Err(Error::Poisoned)));
        }
        drop(reopened);
    }
}

#[test]
fn owned_observation_commit_can_retire_released_execution_with_card_settlement() {
    for stale_owner in [false, true] {
        let mut fixture = Fixture::new();
        fixture.create("task");
        let permit = fixture.launch("task");
        let stamp = permit.execution().clone();
        drop(permit);
        let mut tx = fixture.transaction();
        tx.reserve_owner(&stamp, 0).unwrap();
        let owner = tx.commit().unwrap().owners.pop().unwrap();
        fixture.runtime("task", RuntimeFact::ResourceAvailable);
        fixture.runtime(
            "task",
            RuntimeFact::TurnStarted {
                turn: TurnId::new("turn").unwrap(),
                trigger: Trigger::Input(id("initial")),
            },
        );
        fixture.card("task", "recovery");
        fixture.runtime(
            "task",
            RuntimeFact::ResourceReleased {
                problem_card: "recovery".into(),
            },
        );
        let before_retirement_revision = fixture
            .transaction()
            .load(&"task".into())
            .unwrap()
            .revision();
        let retire = |tx: &mut FixtureTransaction<'_>| {
            assert!(tx.unresolved(&"task".into()).unwrap().is_empty());
            assert!(tx.load(&"task".into()).unwrap().pending_inputs().is_empty());
            tx.accept_owned_observation(
                &stamp,
                owner.generation(),
                1,
                CommandEvidence::new(b"retirement observation".to_vec()).unwrap(),
            )
            .unwrap();
            tx.put_input(&"task".into(), payload("continuation"))
                .unwrap();
            tx.apply(
                &"task".into(),
                before_retirement_revision,
                "retire-released",
                Event::RetireReleasedExecution {
                    problem_card: "recovery".into(),
                    input: id("continuation"),
                },
            )
            .unwrap();
            tx.settle_card(&"task".into(), &"recovery".into(), CardSettlement::Withdraw)
                .unwrap();
        };
        let mut tx = fixture.transaction();
        retire(&mut tx);
        assert!(
            matches!(
                tx.put_normalization_checkpoint(&stamp, 2, None, b"wrong cursor".to_vec()),
                Err(Error::StalePermit)
            ),
            "retirement cannot authorize a checkpoint for an unaccepted cursor"
        );
        assert!(
            matches!(
                tx.put_normalization_checkpoint(&stamp, 1, None, b"normalized retirement".to_vec()),
                Err(Error::Poisoned)
            ),
            "the accepted cursor's own StalePermit fallback cannot clear a caught StalePermit"
        );
        assert!(matches!(tx.commit(), Err(Error::Poisoned)));
        let mut tx = fixture.transaction();
        retire(&mut tx);
        let checkpoint =
            tx.put_normalization_checkpoint(&stamp, 1, None, b"normalized retirement".to_vec());
        assert!(
            checkpoint.is_ok(),
            "same-transaction accepted raw must permit checkpoint after retirement: {checkpoint:?}"
        );
        tx.settle_observation(&stamp, 1, 0).unwrap();
        if stale_owner {
            let changed = tx.connection.execute(
                "UPDATE provider_execution_owners SET owner_generation=owner_generation+1 WHERE task_id=?1 AND generation=?2 AND binding_id=?3 AND owner_generation=?4",
                params![stamp.task().0, i64::try_from(stamp.generation()).unwrap(), owner.binding().id(), i64::try_from(owner.generation()).unwrap()],
            ).unwrap();
            assert_eq!(
                changed, 1,
                "the current durable owner fence really advances"
            );
        }
        let result = tx.commit();
        let observation = if stale_owner {
            assert!(
                matches!(result, Err(Error::StaleOwner)),
                "retirement must not bypass the current durable owner generation"
            );
            None
        } else {
            assert!(
                result.is_ok(),
                "accepted raw/checkpoint must commit with released execution retirement and card settlement: {result:?}"
            );
            let mut committed = result.unwrap();
            assert_eq!(committed.observations.len(), 1);
            assert!(committed.commands.is_empty());
            assert!(committed.attempts.is_empty());
            assert!(committed.permits.is_empty());
            Some(committed.observations.pop().unwrap())
        };
        let database = fixture.directory.join("domain.db");
        drop(fixture.connection);
        let reopened = Connection::open(database).unwrap();
        reopened.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        let rows: i64 = reopened
            .query_row("SELECT count(*) FROM provider_observations", [], |row| {
                row.get(0)
            })
            .unwrap();
        let checkpoints: i64 = reopened
            .query_row(
                "SELECT count(*) FROM provider_normalization_checkpoints",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let durable_owner: i64 = reopened
            .query_row(
                "SELECT owner_generation FROM provider_execution_owners WHERE task_id='task'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            durable_owner,
            i64::try_from(owner.generation()).unwrap(),
            "failed owner CAS is rolled back too"
        );
        let raw = Transaction::new_unchecked(&reopened, TransactionBehavior::Immediate).unwrap();
        let tx = LifecycleTransaction::attach(raw, scope()).unwrap();
        let aggregate = tx.load(&"task".into()).unwrap();
        if stale_owner {
            assert_eq!(rows, 0);
            assert_eq!(checkpoints, 0);
            assert_eq!(aggregate.execution(), Some(&stamp));
            assert_eq!(aggregate.revision(), before_retirement_revision);
            tx.fence_owner(&owner).unwrap();
            assert!(tx.normalization_checkpoint(&stamp).unwrap().is_none());
            assert_eq!(aggregate.resource(), Some(ResourceEvidence::Released));
            let withdrawn: i64 = reopened
                .query_row(
                    "SELECT withdrawn FROM interactions WHERE id='recovery'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                withdrawn, 0,
                "owner failure rolls back recovery card settlement"
            );
            assert_eq!(aggregate.cards().len(), 1);
            assert!(tx.commit().unwrap().observations.is_empty());
        } else {
            assert_eq!(rows, 1);
            assert_eq!(checkpoints, 1);
            assert!(aggregate.execution().is_none());
            assert_eq!(aggregate.pending_inputs(), vec![&id("continuation")]);
            assert!(aggregate.cards().is_empty());
            let withdrawn: i64 = reopened
                .query_row(
                    "SELECT withdrawn FROM interactions WHERE id='recovery'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                withdrawn, 1,
                "the exact recovery card settles in the same commit"
            );
            let value = observation.unwrap();
            assert_eq!(value.scope(), &scope());
            assert_eq!(value.execution(), &stamp);
            assert_eq!(value.binding(), owner.binding());
            assert_eq!(value.owner_generation(), owner.generation());
            assert_eq!(value.cursor(), 1);
            assert!(value.matches_evidence(b"retirement observation"));
            assert!(!value.matches_evidence(b"different bytes"));
            assert!(tx.commit().unwrap().observations.is_empty());
            let raw =
                Transaction::new_unchecked(&reopened, TransactionBehavior::Immediate).unwrap();
            let mut tx = LifecycleTransaction::attach(raw, scope()).unwrap();
            assert!(
                matches!(
                    tx.accept_owned_observation(
                        &stamp,
                        owner.generation(),
                        2,
                        CommandEvidence::new(b"new late evidence".to_vec()).unwrap()
                    ),
                    Err(Error::StalePermit)
                ),
                "new observation admission still requires the current execution"
            );
            assert!(matches!(tx.commit(), Err(Error::Poisoned)));
            let raw =
                Transaction::new_unchecked(&reopened, TransactionBehavior::Immediate).unwrap();
            let tx = LifecycleTransaction::attach(raw, scope()).unwrap();
            assert!(
                matches!(tx.normalization_checkpoint(&stamp), Err(Error::StalePermit)),
                "the public metadata read does not become a historical admission API"
            );
            assert!(matches!(tx.commit(), Err(Error::Poisoned)));
            let raw =
                Transaction::new_unchecked(&reopened, TransactionBehavior::Immediate).unwrap();
            let mut tx = LifecycleTransaction::attach(raw, scope()).unwrap();
            assert!(
                matches!(
                    tx.put_normalization_checkpoint(
                        &stamp,
                        1,
                        Some(1),
                        b"normalized retirement".to_vec()
                    ),
                    Err(Error::StalePermit)
                ),
                "a later transaction has no pending owned observation for a historical checkpoint"
            );
            assert!(matches!(tx.commit(), Err(Error::Poisoned)));
        }
        drop(reopened);
    }
}

#[test]
fn caught_error_after_valid_writes_poisons_commit_and_leaves_no_row() {
    let mut fixture = Fixture::new();
    fixture.add_task("task");
    let mut tx = fixture.transaction();
    tx.create(&"task".into(), payload("initial")).unwrap();
    tx.apply(&"task".into(), 0, "admit", admit("node", "instance"))
        .unwrap();
    assert!(matches!(
        tx.apply(&"task".into(), 0, "admit-again", admit("node", "other")),
        Err(Error::Domain(_))
    ));
    assert!(
        matches!(tx.load(&"task".into()), Err(Error::Poisoned)),
        "every later operation reports the earlier failure"
    );
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let independent = Connection::open(fixture.directory.join("domain.db")).unwrap();
    for table in ["lifecycle_tasks", "lifecycle_inputs", "lifecycle_events"] {
        let rows: i64 = independent
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE task_id='task'"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0, "{table} keeps no row of the poisoned transaction");
    }
}

#[test]
fn caught_error_after_a_valid_write_that_touches_no_task_poisons_commit() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let stamp = fixture.launch("task").execution().clone();
    let mut tx = fixture.transaction();
    tx.put_adapter_conversation(
        &stamp,
        AdapterInputMetadata::new(b"native session reference".to_vec()).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        tx.lookup_adapter_inputs(&stamp, &vec![b"native".to_vec(); 65]),
        Err(Error::MetadataCapacity)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let independent = Connection::open(fixture.directory.join("domain.db")).unwrap();
    let rows: i64 = independent
        .query_row("SELECT count(*) FROM provider_conversation_metadata", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0, "the valid metadata write of the poisoned transaction is not visible");
}

#[test]
fn missing_adapter_metadata_is_a_normal_absence_that_still_commits() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let stamp = fixture.launch("task").execution().clone();
    let mut tx = fixture.transaction();
    assert!(tx.adapter_input(&stamp, &id("initial")).unwrap().is_none());
    assert!(tx.adapter_conversation(&stamp).unwrap().is_none());
    tx.put_adapter_input(
        &stamp,
        &id("initial"),
        AdapterInputMetadata::new(b"first native key".to_vec()).unwrap(),
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(
        fixture
            .transaction()
            .adapter_input(&stamp, &id("initial"))
            .unwrap()
            .unwrap()
            .bytes(),
        b"first native key"
    );
}


#[test]
fn poisoned_journal_rolls_back_even_when_the_caller_asked_to_commit_on_drop() {
    let mut fixture = Fixture::new();
    fixture.add_task("task");
    let mut raw = fixture
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    raw.set_drop_behavior(rusqlite::DropBehavior::Commit);
    let mut tx = LifecycleTransaction::attach(raw, scope()).unwrap();
    tx.create(&"task".into(), payload("initial")).unwrap();
    assert!(matches!(
        tx.put_input(
            &"task".into(),
            InputPayload::new(id("initial"), "Different bytes").unwrap()
        ),
        Err(Error::InputConflict)
    ));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
    let independent = Connection::open(fixture.directory.join("domain.db")).unwrap();
    let rows: i64 = independent
        .query_row("SELECT count(*) FROM lifecycle_tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 0, "the caller's commit-on-drop cannot persist a poisoned journal");
}

fn claude_bg(task: &str) -> Event {
    Event::Admit {
        node: "node".into(),
        conversation: ConversationId::new("node").unwrap(),
        instance: InstanceId::new(format!("{}:{task}:1", alondra_domain::CLAUDE_BG_HOST)).unwrap(),
        host: HostId::new(alondra_domain::CLAUDE_BG_HOST).unwrap(),
    }
}

/// A task of the `claude --bg` backend keeps its journal in the store without any SDK
/// dispatch: it commits with no launch intent or binding, and nothing can dispatch to it.
#[test]
fn a_claude_bg_execution_commits_without_intents_and_nothing_dispatches_to_it() {
    let mut fixture = Fixture::new();
    fixture.add_task("task");
    let path = fixture.directory.join("task");
    let mut tx = fixture.transaction();
    tx.create_without_text(&"task".into(), id("initial")).unwrap();
    tx.apply(&"task".into(), 0, "admit", claude_bg("task")).unwrap();
    let committed = tx.commit().unwrap();
    assert!(committed.permits.is_empty(), "no SDK offer for a claude --bg execution");
    let backend: String = fixture.connection.query_row("SELECT backend FROM lifecycle_executions WHERE task_id='task'", [], |r| r.get(0)).unwrap();
    assert_eq!(backend, "claude_bg");
    let mut tx = fixture.transaction();
    assert!(matches!(tx.stage_launch(&"task".into(), &local(&path), &test_binding()), Err(Error::NotDispatchable)));
    drop(tx);
    let mut tx = fixture.transaction();
    assert!(matches!(tx.bind_provider(&"task".into(), &test_binding()), Err(Error::NotDispatchable)));
    drop(tx);
    let refused = fixture.connection.execute("INSERT INTO lifecycle_dispatches(task_id,generation,input_id,kind,placement_kind,placement,status) VALUES('task',1,'initial','launch','local','/x','prepared')", []);
    assert!(refused.is_err(), "SQL refuses a dispatch to a claude --bg execution too");
}

/// An input the domain named without text gets the text an SDK execution is sent once, only
/// while it is pending for that execution; the SDK can never be offered one without it.
#[test]
fn an_input_without_text_is_given_its_text_once_before_any_sdk_dispatch() {
    let mut fixture = Fixture::new();
    fixture.add_task("task");
    let path = fixture.directory.join("task");
    let mut tx = fixture.transaction();
    tx.create_without_text(&"task".into(), id("initial")).unwrap();
    tx.apply(&"task".into(), 0, "admit", admit("node", "instance")).unwrap();
    assert!(matches!(tx.stage_launch(&"task".into(), &local(&path), &test_binding()), Err(Error::Database(_))));
    drop(tx);
    let mut tx = fixture.transaction();
    tx.create_without_text(&"task".into(), id("initial")).unwrap();
    tx.apply(&"task".into(), 0, "admit", admit("node", "instance")).unwrap();
    tx.give_input_text(&"task".into(), payload("initial")).unwrap();
    tx.give_input_text(&"task".into(), payload("initial")).unwrap();
    assert!(matches!(
        tx.give_input_text(&"task".into(), InputPayload::new(id("initial"), "Another text").unwrap()),
        Err(Error::InputConflict)
    ));
    drop(tx);
    let mut tx = fixture.transaction();
    tx.create_without_text(&"task".into(), id("initial")).unwrap();
    tx.apply(&"task".into(), 0, "admit", admit("node", "instance")).unwrap();
    tx.give_input_text(&"task".into(), payload("initial")).unwrap();
    tx.stage_launch(&"task".into(), &local(&path), &test_binding()).unwrap();
    let committed = tx.commit().unwrap();
    assert_eq!(committed.permits.len(), 1, "with its text the input is offered");
    assert!(fixture.connection.execute("UPDATE lifecycle_inputs SET body='Changed' WHERE task_id='task'", []).is_err(), "a text is never changed");
}

fn read_events(connection: &Connection) -> BTreeMap<TaskId, alondra_store::JournalTask<Event>> {
    alondra_store::read_journal(connection, &scope(), |_, event, _| Ok::<_, alondra_store::Error>(event.clone())).unwrap()
}

/// What the daemon reads: every task's journal, with its events in order.
#[test]
fn the_journal_reads_every_task_with_its_events() {
    let mut fixture = Fixture::new();
    fixture.add_task("task");
    let path = fixture.directory.join("task");
    let mut tx = fixture.transaction();
    tx.create_without_text(&"task".into(), id("initial")).unwrap();
    tx.apply(&"task".into(), 0, "admit", claude_bg("task")).unwrap();
    tx.commit().unwrap();
    let journal = read_events(&fixture.connection);
    assert_eq!(journal.len(), 1);
    let task = &journal[&"task".into()];
    assert_eq!(task.initial, id("initial"));
    assert_eq!(task.entries, vec![claude_bg("task")]);
    assert!(task.texts.is_empty(), "the claude --bg input has no text");
}

/// A launch whose host ended after taking it: its send is uncertain and its input pending.
fn uncertain_launch(fixture: &mut Fixture) -> ExecutionStamp {
    let owner = crashed_claim(fixture, b"framed command");
    let stamp = owner.execution().clone();
    let intent = fixture.transaction().unresolved_commands(&"task".into()).unwrap()[0].intent_id;
    let mut tx = fixture.transaction();
    tx.reserve_owner(&stamp, 1).unwrap();
    let adopted = tx.commit().unwrap().owners.pop().unwrap();
    let mut tx = fixture.transaction();
    tx.resolve_dispatch(&stamp, intent, adopted.generation(), DispatchReceipt::Uncertain).unwrap();
    tx.commit().unwrap();
    stamp
}
fn decide(fixture: &mut Fixture, command: &str, event: Event) -> Committed {
    let mut tx = fixture.transaction();
    let revision = tx.load(&"task".into()).unwrap().revision();
    tx.apply(&"task".into(), revision, command, event).unwrap();
    tx.commit().unwrap()
}

#[test]
fn a_doubtful_send_holds_every_later_send_until_the_user_sends_the_same_text_again() {
    let mut fixture = Fixture::new();
    uncertain_launch(&mut fixture);
    fixture.card("task", "doubt");
    let committed = decide(&mut fixture, "doubt", Event::DoubtHandoff { card: "doubt".into(), input: id("initial") });
    assert!(committed.permits.is_empty(), "nothing goes while the user decides");
    let mut tx = fixture.transaction();
    let revision = tx.load(&"task".into()).unwrap().revision();
    tx.put_input(&"task".into(), InputPayload::new(id("again"), "Fixture prompt").unwrap()).unwrap();
    tx.apply(&"task".into(), revision, "settle", Event::SettleHandoff { card: "doubt".into(), resend: Some(id("again")), unheard_card: "unheard".into() }).unwrap();
    tx.settle_card(&"task".into(), &"doubt".into(), CardSettlement::Answer(CardAnswer(r#"{"option_id":"resend"}"#.into()))).unwrap();
    tx.stage_input(&"task".into(), &id("again")).unwrap();
    let committed = tx.commit().unwrap();
    assert_eq!(committed.permits.iter().map(|permit| permit.input().clone()).collect::<Vec<_>>(), [id("again")],
        "the message sent again starts the session the ended host never reported");
    assert!(fixture.transaction().unresolved(&"task".into()).unwrap().is_empty(), "the uncertain send no longer holds the task");
}

#[test]
fn only_a_handed_or_uncertain_send_can_be_doubted_and_only_the_users_word_releases_it() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    fixture.launch("task");
    let mut tx = fixture.transaction();
    let revision = tx.load(&"task".into()).unwrap().revision();
    assert!(matches!(
        tx.apply(&"task".into(), revision, "doubt", Event::DoubtHandoff { card: "doubt".into(), input: id("initial") }),
        Err(Error::InvalidInput)
    ), "a send that never left is not in doubt");
    drop(tx);

    let mut fixture = Fixture::new();
    uncertain_launch(&mut fixture);
    fixture.card("task", "doubt");
    decide(&mut fixture, "doubt", Event::DoubtHandoff { card: "doubt".into(), input: id("initial") });
    assert_eq!(fixture.transaction().unresolved(&"task".into()).unwrap().len(), 1, "a doubt alone settles nothing");
    let mut tx = fixture.transaction();
    let revision = tx.load(&"task".into()).unwrap().revision();
    tx.apply(&"task".into(), revision, "cancel", Event::Finish { intent: FinishIntent::Cancel, cleanup: OperationId::new("cleanup").unwrap(), post_merge: None }).unwrap();
    tx.settle_card(&"task".into(), &"doubt".into(), CardSettlement::Withdraw).unwrap();
    tx.commit().unwrap();
    assert!(fixture.transaction().unresolved(&"task".into()).unwrap().is_empty(), "cancelling is the user's word on it");
}

// A later reply never overtakes one whose handoff is in doubt: while the user decides about the
// first, the second gets no permit; once the first is taken as received, the second goes.
#[test]
fn a_later_reply_waits_behind_a_doubtful_one_until_the_user_settles_it() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    drop(fixture.launch("task"));
    fixture.runtime("task", RuntimeFact::ResourceAvailable);
    fixture.runtime("task", RuntimeFact::TurnStarted { turn: TurnId::new("turn").unwrap(), trigger: Trigger::Input(id("initial")) });
    for card in ["question-first", "question-second", "doubt"] {
        fixture.card("task", card);
    }
    let task = TaskId::from("task");
    let mut tx = fixture.transaction();
    tx.apply(&task, 3, "ask-first", Event::Ask { card: "question-first".into() }).unwrap();
    tx.apply(&task, 4, "ask-second", Event::Ask { card: "question-second".into() }).unwrap();
    for (revision, card, input) in [(5, "question-first", "reply-first"), (6, "question-second", "reply-second")] {
        tx.put_input(&task, payload(input)).unwrap();
        tx.apply(&task, revision, input, Event::Answer { card: card.into(), input: id(input) }).unwrap();
        tx.settle_card(&task, &card.into(), CardSettlement::Answer(CardAnswer::from_object(serde_json::Map::new()).unwrap())).unwrap();
    }
    tx.stage_input(&task, &id("reply-first")).unwrap();
    tx.stage_input(&task, &id("reply-second")).unwrap();
    let first = tx.commit().unwrap().permits.pop().unwrap();
    let mut tx = fixture.transaction();
    tx.claim(first).unwrap();
    let first = tx.commit().unwrap().attempts.pop().unwrap();
    let mut tx = fixture.transaction();
    tx.record_receipt(first, DispatchReceipt::Handed).unwrap();
    tx.apply(&task, 7, "doubt", Event::DoubtHandoff { card: "doubt".into(), input: id("reply-first") }).unwrap();
    assert!(tx.commit().unwrap().permits.is_empty(), "the second reply waits while the first is in doubt");
    let mut tx = fixture.transaction();
    tx.apply(&task, 8, "settle", Event::SettleHandoff { card: "doubt".into(), resend: None, unheard_card: "unheard".into() }).unwrap();
    tx.settle_card(&task, &"doubt".into(), CardSettlement::Answer(CardAnswer::from_object(serde_json::Map::new()).unwrap())).unwrap();
    let permits = tx.commit().unwrap().permits;
    assert_eq!(permits.iter().map(|permit| permit.input().clone()).collect::<Vec<_>>(), [id("reply-second")]);
}

#[test]
fn a_transaction_replays_each_task_once_and_reads_its_own_applies() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let path = fixture.directory.join("task");
    let before = measure::counts();
    let mut tx = fixture.transaction();
    let held = tx.load(&"task".into()).unwrap();
    tx.apply(&"task".into(), 0, "admit", admit("node", "instance"))
        .unwrap();
    assert_eq!(held.revision(), 0, "an aggregate read before the apply stays as it was read");
    drop(held);
    let admitted = tx.load(&"task".into()).unwrap();
    assert_eq!(
        (admitted.revision(), admitted.execution().is_some()),
        (1, true),
        "a read after the apply sees the apply"
    );
    drop(admitted);
    tx.stage_launch(&"task".into(), &local(&path), &test_binding())
        .unwrap();
    tx.commit().unwrap();
    assert_eq!(
        (measure::counts() - before).aggregate_loads,
        2,
        "one replay before the apply, one for the apply that found the aggregate still held, none for the reads and the commit after it"
    );

    let before = measure::counts();
    let mut tx = fixture.transaction();
    assert_eq!(tx.load(&"task".into()).unwrap().revision(), 1, "the next transaction reads what the last one committed");
    let launched = tx.load(&"task".into()).unwrap();
    let (stamp, cursor) = (launched.execution().unwrap().clone(), launched.cursor().unwrap() + 1);
    drop(launched);
    tx.apply(
        &"task".into(),
        1,
        "fact-1",
        Event::Runtime { stamp, cursor, fact: RuntimeFact::ResourceAvailable },
    )
    .unwrap();
    assert_eq!(tx.load(&"task".into()).unwrap().revision(), 2);
    drop(tx);
    assert_eq!(
        measure::Counts { input_checks: 0, ..measure::counts() - before },
        measure::Counts { transactions: 1, aggregate_loads: 1, event_decodes: 1, input_checks: 0 },
        "a transaction replays the journal of the task it touches once"
    );

    let tx = fixture.transaction();
    assert_eq!(
        tx.load(&"task".into()).unwrap().revision(),
        1,
        "an apply rolled back with its transaction is not read by the next one"
    );
}

#[test]
fn a_replay_looks_up_each_stored_input_once_however_long_it_stays_pending() {
    const TOLD: usize = 400;
    let mut fixture = Fixture::new();
    fixture.create("task");
    fixture.launch("task");
    let task: TaskId = "task".into();
    let mut tx = fixture.transaction();
    for told in 0..TOLD {
        let input = format!("told-{told}");
        tx.put_input(&task, payload(&input)).unwrap();
        let revision = tx.load(&task).unwrap().revision();
        tx.apply(&task, revision, &input, Event::Tell { input: id(&input) }).unwrap();
        tx.stage_input(&task, &id(&input)).unwrap();
    }
    tx.commit().unwrap();
    let stored = u64::try_from(TOLD + 1).unwrap();

    let before = measure::counts();
    let journal = read_events(&fixture.connection);
    let read = measure::counts() - before;
    assert_eq!(journal[&task].entries.len(), TOLD + 1);
    assert!(
        read.input_checks <= stored,
        "reading the journal at startup looked up {} inputs for {stored} stored ones",
        read.input_checks
    );

    let before = measure::counts();
    let tx = fixture.transaction();
    assert_eq!(tx.load(&task).unwrap().pending_inputs().len(), TOLD + 1);
    drop(tx);
    let replayed = measure::counts() - before;
    assert!(
        replayed.input_checks <= stored,
        "replaying the task looked up {} inputs for {stored} stored ones",
        replayed.input_checks
    );
}

#[test]
fn a_transaction_looks_up_each_input_once_however_many_writes_it_makes_on_the_task() {
    const TOLD: usize = 400;
    let mut fixture = Fixture::new();
    fixture.create("task");
    fixture.launch("task");
    let task: TaskId = "task".into();
    let before = measure::counts();
    let mut tx = fixture.transaction();
    for told in 0..TOLD {
        let input = format!("told-{told}");
        tx.put_input(&task, payload(&input)).unwrap();
        let revision = tx.load(&task).unwrap().revision();
        tx.apply(&task, revision, &input, Event::Tell { input: id(&input) }).unwrap();
        tx.stage_input(&task, &id(&input)).unwrap();
    }
    tx.commit().unwrap();
    let written = measure::counts() - before;
    let stored = u64::try_from(TOLD + 1).unwrap();
    assert!(
        written.input_checks <= stored,
        "writing {TOLD} tells in one transaction looked up {} inputs for {stored} stored ones",
        written.input_checks
    );

    let tx = fixture.transaction();
    tx.load(&task).unwrap();
    let loaded = measure::counts();
    for _ in 0..3 {
        assert_eq!(tx.load(&task).unwrap().pending_inputs().len(), TOLD + 1);
    }
    assert_eq!(
        (measure::counts() - loaded).input_checks,
        0,
        "reading a task this transaction already replayed looks up none of its inputs again"
    );
}

fn tamper_after_a_read(
    through_inspect: bool,
    tamper: impl FnOnce(&Connection),
) -> Result<std::sync::Arc<TaskLifecycle>> {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let path = fixture.directory.join("task");
    let mut tx = fixture.transaction();
    tx.apply(&"task".into(), 0, "admit", admit("node", "instance"))
        .unwrap();
    tx.stage_launch(&"task".into(), &local(&path), &test_binding())
        .unwrap();
    tx.commit().unwrap();
    let tx = fixture.transaction();
    assert_eq!(tx.load(&"task".into()).unwrap().pending_inputs(), vec![&id("initial")]);
    let connection = tx.connection;
    if through_inspect {
        tx.inspect(|| {
            tamper(connection);
            Ok::<_, Error>(())
        })
        .unwrap();
    } else {
        tamper(connection);
    }
    tx.load(&"task".into())
}

fn rewrite_the_first_event(connection: &Connection) {
    corrupt_while_guard_removed(connection, "lifecycle_event_immutable", || {
        connection
            .execute("UPDATE lifecycle_events SET event='{}' WHERE task_id='task' AND revision=1", [])
            .unwrap();
    });
}

fn rewrite_the_initial_input(connection: &Connection) {
    connection
        .execute_batch(
            "INSERT INTO lifecycle_inputs(task_id,input_id,body) VALUES('task','other','Another prompt');
             UPDATE lifecycle_tasks SET initial_input='other' WHERE task_id='task';",
        )
        .unwrap();
}

#[test]
fn an_event_rewritten_on_the_same_connection_is_replayed_and_refused() {
    assert!(
        tamper_after_a_read(false, rewrite_the_first_event).is_err(),
        "a journal the store did not write must not be read from an aggregate replayed before it"
    );
}

#[test]
fn a_journal_row_rewritten_without_a_schema_change_is_replayed() {
    let reread = tamper_after_a_read(false, rewrite_the_initial_input).unwrap();
    assert_eq!(
        reread.pending_inputs(),
        vec![&id("other")],
        "a journal write the store did not make must not leave an aggregate replayed before it"
    );
}

#[test]
fn a_journal_row_rewritten_inside_inspect_is_replayed() {
    let reread = tamper_after_a_read(true, rewrite_the_initial_input).unwrap();
    assert_eq!(
        reread.pending_inputs(),
        vec![&id("other")],
        "application work inside inspect must not leave an aggregate replayed before it"
    );
}

fn control_binding(capabilities: impl IntoIterator<Item = Capability>) -> ProviderBinding {
    ProviderBinding::new(
        "control-binding".into(),
        "fixture-driver".into(),
        "control-instance".into(),
        "fixture-config".into(),
        1,
        1,
        capabilities.into_iter().chain([Capability::OfferInput]).collect(),
        "fixture-credential".into(),
        1,
    )
    .unwrap()
}
fn every_control() -> [Capability; 5] {
    [Capability::Interrupt, Capability::SetModel, Capability::SetPermissionMode, Capability::McpStatus, Capability::Compact]
}
fn op(value: &str) -> OperationId {
    OperationId::new(value).unwrap()
}
/// An SDK task whose execution runs its root turn `root` under `binding`.
fn running_sdk(fixture: &mut Fixture, binding: &ProviderBinding) -> ExecutionStamp {
    fixture.create("task");
    let path = fixture.directory.join("task");
    let mut tx = fixture.transaction();
    tx.apply(&"task".into(), 0, "admit", admit("node", "instance")).unwrap();
    tx.stage_launch(&"task".into(), &local(&path), binding).unwrap();
    tx.commit().unwrap();
    fixture.runtime("task", RuntimeFact::ResourceAvailable);
    fixture.runtime("task", RuntimeFact::TurnStarted { turn: TurnId::new("root").unwrap(), trigger: Trigger::Input(id("initial")) });
    fixture.transaction().load(&"task".into()).unwrap().execution().unwrap().clone()
}
fn request(fixture: &mut Fixture, control: &str, kind: Control) -> alondra_store::Result<Committed> {
    let mut tx = fixture.transaction();
    let revision = tx.load(&"task".into())?.revision();
    tx.apply(&"task".into(), revision, &format!("control-{revision}"), Event::RequestControl { control: op(control), kind: kind.clone() })?;
    tx.stage_control(&"task".into(), &op(control), &kind)?;
    tx.commit()
}
fn control_status(fixture: &Fixture, control: &str) -> String {
    fixture.connection.query_row("SELECT status FROM lifecycle_controls WHERE control_id=?1", [control], |row| row.get(0)).unwrap()
}
fn owner(fixture: &mut Fixture, stamp: &ExecutionStamp) -> OwnerGrant {
    let mut tx = fixture.transaction();
    tx.reserve_owner(stamp, 0).unwrap();
    tx.commit().unwrap().owners.pop().unwrap()
}

/// The causal test of the controls: an operation the execution's binding does not permit is
/// refused with no effect at all, not even the domain's record of the request.
#[test]
fn a_control_the_binding_does_not_permit_is_refused_and_writes_nothing() {
    let mut fixture = Fixture::new();
    running_sdk(&mut fixture, &control_binding([]));
    let before = (fixture.count("lifecycle_events"), fixture.count("lifecycle_controls"), fixture.transaction().load(&"task".into()).unwrap().revision());
    for (control, kind) in [
        ("interrupt", Control::Interrupt { turn: Some(TurnId::new("root").unwrap()) }),
        ("model", Control::SetModel { model: "claude-opus".into() }),
        ("mode", Control::SetPermissionMode { mode: PermissionMode::Plan }),
        ("mcp", Control::ReadMcpStatus),
    ] {
        let mut tx = fixture.transaction();
        let revision = tx.load(&"task".into()).unwrap().revision();
        tx.apply(&"task".into(), revision, control, Event::RequestControl { control: op(control), kind: kind.clone() }).unwrap();
        assert!(matches!(tx.stage_control(&"task".into(), &op(control), &kind), Err(Error::UnsupportedCapability)), "{kind:?}");
        assert!(matches!(tx.commit(), Err(Error::Poisoned)));
        assert_eq!((fixture.count("lifecycle_events"), fixture.count("lifecycle_controls"), fixture.transaction().load(&"task".into()).unwrap().revision()), before, "{kind:?} left a trace");
    }
}

#[test]
fn a_control_is_permitted_claimed_recorded_and_receipted_once() {
    let mut fixture = Fixture::new();
    let binding = control_binding(every_control());
    let stamp = running_sdk(&mut fixture, &binding);
    let grant = owner(&mut fixture, &stamp);
    let kind = Control::SetModel { model: "claude-opus".into() };
    let mut committed = request(&mut fixture, "model", kind.clone()).unwrap();
    assert_eq!(committed.control_permits.len(), 1);
    let permit = committed.control_permits.pop().unwrap();
    assert_eq!((permit.control(), permit.execution()), (&op("model"), &stamp));
    assert_eq!(control_status(&fixture, "model"), "prepared");
    let mut tx = fixture.transaction();
    assert!(tx.claim_control(permit).unwrap());
    let mut committed = tx.commit().unwrap();
    assert!(committed.control_permits.is_empty(), "a claimed control is not offered again");
    let attempt = committed.control_attempts.pop().unwrap();
    assert_eq!((attempt.operation(), attempt.binding()), (&ControlOperation::Control(kind), &binding));
    assert_eq!(attempt.command_id(), format!("lifecycle-control-{}", attempt.id()).into());
    let mut tx = fixture.transaction();
    tx.record_owned_control(attempt, grant.generation(), CommandEvidence::new(b"set model".to_vec()).unwrap()).unwrap();
    let control = tx.commit().unwrap().control_commands.pop().unwrap();
    assert_eq!((control.bytes(), control.owner_generation()), (&b"set model"[..], grant.generation()));
    assert_eq!(fixture.count("provider_control_commands"), 1);
    let mut tx = fixture.transaction();
    tx.record_control_receipt(control, DispatchReceipt::Handed).unwrap();
    tx.commit().unwrap();
    assert_eq!(control_status(&fixture, "model"), "handed");
    let tx = fixture.transaction();
    assert_eq!(tx.load(&"task".into()).unwrap().turn(), Some(&TurnId::new("root").unwrap()), "a control changes nothing of the execution");
}

#[test]
fn the_same_control_is_staged_once_and_other_content_under_its_id_conflicts() {
    let mut fixture = Fixture::new();
    running_sdk(&mut fixture, &control_binding(every_control()));
    let kind = Control::SetPermissionMode { mode: PermissionMode::DontAsk };
    request(&mut fixture, "mode", kind.clone()).unwrap();
    let mut tx = fixture.transaction();
    tx.stage_control(&"task".into(), &op("mode"), &kind).unwrap();
    assert_eq!(tx.commit().unwrap().control_permits.len(), 1, "the prepared control is offered again, once");
    assert_eq!(fixture.count("lifecycle_controls"), 1);
    assert!(matches!(request(&mut fixture, "mode", Control::SetPermissionMode { mode: PermissionMode::Plan }), Err(Error::CommandConflict)));
    let mut tx = fixture.transaction();
    assert!(matches!(tx.stage_control(&"task".into(), &op("never-decided"), &Control::ReadMcpStatus), Err(Error::MissingIntent)), "only what the domain recorded is staged");
    drop(tx);
    assert_eq!(fixture.count("lifecycle_controls"), 1);
}

#[test]
fn a_new_execution_revokes_prepared_controls_and_waits_for_claimed_ones() {
    let mut fixture = Fixture::new();
    let stamp = running_sdk(&mut fixture, &control_binding(every_control()));
    let permit = request(&mut fixture, "mcp", Control::ReadMcpStatus).unwrap().control_permits.pop().unwrap();
    let release = |fixture: &mut Fixture| {
        let mut tx = fixture.transaction();
        let revision = tx.load(&"task".into())?.revision();
        tx.put_input(&"task".into(), payload("requeue"))?;
        tx.apply(&"task".into(), revision, "release", Event::ExecutionReleased { input: id("requeue") })?;
        tx.commit()
    };
    let interrupt = request(&mut fixture, "interrupt", Control::Interrupt { turn: Some(TurnId::new("root").unwrap()) }).unwrap();
    let mut tx = fixture.transaction();
    let interrupt = interrupt.control_permits.into_iter().find(|permit| permit.control() == &op("interrupt")).unwrap();
    assert!(tx.claim_control(interrupt).unwrap());
    tx.commit().unwrap();
    assert!(matches!(release(&mut fixture), Err(Error::UnresolvedDispatch)), "a claimed control is resolved before the execution changes");
    let attempt_id: i64 = fixture.connection.query_row("SELECT id FROM lifecycle_controls WHERE control_id='interrupt'", [], |row| row.get(0)).unwrap();
    let grant = owner(&mut fixture, &stamp);
    let mut tx = fixture.transaction();
    tx.resolve_control(&stamp, attempt_id, grant.generation(), DispatchReceipt::Rejected).unwrap();
    tx.commit().unwrap();
    release(&mut fixture).unwrap();
    assert_eq!(control_status(&fixture, "mcp"), "revoked");
    let mut tx = fixture.transaction();
    assert!(matches!(tx.claim_control(permit), Err(Error::StalePermit)));
}

#[test]
fn an_interrupt_whose_turn_ended_is_revoked_instead_of_claimed() {
    let mut fixture = Fixture::new();
    running_sdk(&mut fixture, &control_binding(every_control()));
    let permit = request(&mut fixture, "interrupt", Control::Interrupt { turn: Some(TurnId::new("root").unwrap()) }).unwrap().control_permits.pop().unwrap();
    fixture.card("task", "unheard");
    fixture.runtime("task", RuntimeFact::TurnEnded { turn: TurnId::new("root").unwrap(), unheard_card: "unheard".into() });
    let mut tx = fixture.transaction();
    assert!(!tx.claim_control(permit).unwrap(), "nothing runs to interrupt");
    let committed = tx.commit().unwrap();
    assert!(committed.control_attempts.is_empty() && committed.control_permits.is_empty());
    assert_eq!(control_status(&fixture, "interrupt"), "revoked");
}

#[test]
fn an_adopting_owner_reconciles_a_recorded_control_under_its_own_generation() {
    let mut fixture = Fixture::new();
    let stamp = running_sdk(&mut fixture, &control_binding(every_control()));
    let grant = owner(&mut fixture, &stamp);
    let permit = request(&mut fixture, "mode", Control::SetPermissionMode { mode: PermissionMode::Default }).unwrap().control_permits.pop().unwrap();
    let mut tx = fixture.transaction();
    tx.claim_control(permit).unwrap();
    let attempt = tx.commit().unwrap().control_attempts.pop().unwrap();
    let mut tx = fixture.transaction();
    tx.record_owned_control(attempt, grant.generation(), CommandEvidence::new(b"framed control".to_vec()).unwrap()).unwrap();
    tx.commit().unwrap();
    let unresolved = fixture.transaction().unresolved_controls(&"task".into()).unwrap();
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].command, b"framed control");
    let mut tx = fixture.transaction();
    tx.reserve_owner(&stamp, grant.generation()).unwrap();
    let adopted = tx.commit().unwrap().owners.pop().unwrap();
    let mut tx = fixture.transaction();
    assert!(matches!(tx.resolve_control(&stamp, unresolved[0].id, grant.generation(), DispatchReceipt::Handed), Err(Error::StaleOwner)));
    drop(tx);
    let mut tx = fixture.transaction();
    tx.resolve_control(&stamp, unresolved[0].id, adopted.generation(), DispatchReceipt::Handed).unwrap();
    tx.commit().unwrap();
    assert!(fixture.transaction().unresolved_controls(&"task".into()).unwrap().is_empty());
    assert_eq!(control_status(&fixture, "mode"), "handed");
    let mut tx = fixture.transaction();
    assert!(matches!(tx.resolve_control(&stamp, unresolved[0].id, adopted.generation(), DispatchReceipt::Handed), Err(Error::StalePermit)));
}

#[test]
fn compaction_is_an_input_staged_only_when_the_binding_can_compact() {
    for (binding, permitted) in [(control_binding([Capability::Interrupt]), false), (control_binding(every_control()), true)] {
        let mut fixture = Fixture::new();
        running_sdk(&mut fixture, &binding);
        fixture.card("task", "unheard");
        fixture.runtime("task", RuntimeFact::TurnEnded { turn: TurnId::new("root").unwrap(), unheard_card: "unheard".into() });
        let before = (fixture.count("lifecycle_events"), fixture.count("lifecycle_dispatches"));
        let mut tx = fixture.transaction();
        let revision = tx.load(&"task".into()).unwrap().revision();
        tx.put_input(&"task".into(), InputPayload::new(id("compact"), "/compact").unwrap()).unwrap();
        tx.apply(&"task".into(), revision, "compact", Event::Compact { input: id("compact") }).unwrap();
        if !permitted {
            assert!(matches!(tx.stage_compaction(&"task".into(), &id("compact")), Err(Error::UnsupportedCapability)));
            assert!(matches!(tx.commit(), Err(Error::Poisoned)));
            assert_eq!((fixture.count("lifecycle_events"), fixture.count("lifecycle_dispatches")), before);
            continue;
        }
        tx.stage_compaction(&"task".into(), &id("compact")).unwrap();
        let committed = tx.commit().unwrap();
        assert_eq!(committed.permits.iter().map(|permit| permit.input()).collect::<Vec<_>>(), vec![&id("compact")]);
        let tx = fixture.transaction();
        assert_eq!(tx.load(&"task".into()).unwrap().cards(), vec![(&InteractionId::from("unheard"), WaitReason::Unheard)], "the card stays");
    }
}

#[test]
fn new_capabilities_survive_registration_and_old_bindings_need_no_extra_row() {
    let mut fixture = Fixture::new();
    let binding = control_binding(every_control().into_iter().chain([Capability::Decisions, Capability::BackgroundWork]));
    let stamp = running_sdk(&mut fixture, &binding);
    assert_eq!(fixture.transaction().execution_binding(&stamp).unwrap(), binding);
    assert_eq!(fixture.count("provider_binding_controls"), 1);
    let mut fixture = Fixture::new();
    let stamp = running_sdk(&mut fixture, &control_binding([Capability::Interrupt]));
    assert_eq!(fixture.transaction().execution_binding(&stamp).unwrap(), control_binding([Capability::Interrupt]));
    assert_eq!(fixture.count("provider_binding_controls"), 0);
}

#[test]
fn control_evidence_only_moves_forward_and_is_never_deleted() {
    let mut fixture = Fixture::new();
    let stamp = running_sdk(&mut fixture, &control_binding(every_control()));
    let grant = owner(&mut fixture, &stamp);
    let permit = request(&mut fixture, "mcp", Control::ReadMcpStatus).unwrap().control_permits.pop().unwrap();
    let mut tx = fixture.transaction();
    tx.claim_control(permit).unwrap();
    let attempt = tx.commit().unwrap().control_attempts.pop().unwrap();
    let mut tx = fixture.transaction();
    tx.record_owned_control(attempt, grant.generation(), CommandEvidence::new(b"status".to_vec()).unwrap()).unwrap();
    tx.commit().unwrap();
    let c = &fixture.connection;
    for refused in [
        "UPDATE lifecycle_controls SET status='prepared',claimed_revision=NULL",
        "UPDATE lifecycle_controls SET status='revoked',claimed_revision=NULL",
        "UPDATE lifecycle_controls SET kind='interrupt'",
        "UPDATE lifecycle_controls SET control_id='other'",
        "DELETE FROM lifecycle_controls",
        "UPDATE provider_control_commands SET bytes=x'00'",
        "DELETE FROM provider_control_commands",
        "UPDATE provider_binding_controls SET capabilities=1",
        "DELETE FROM provider_binding_controls",
        "INSERT INTO lifecycle_controls(task_id,generation,control_id,kind,argument,status) VALUES('task',1,'mode','set_permission_mode','bypass_permissions','prepared')",
        "INSERT INTO lifecycle_controls(task_id,generation,control_id,kind,argument,status) VALUES('task',1,'mode','set_permission_mode',NULL,'prepared')",
        "INSERT INTO lifecycle_controls(task_id,generation,control_id,kind,argument,status) VALUES('task',1,'status','mcp_status','x','prepared')",
        "INSERT INTO lifecycle_controls(task_id,generation,control_id,kind,argument,status) VALUES('task',1,'model','set_model',NULL,'prepared')",
        "INSERT INTO lifecycle_controls(task_id,generation,control_id,kind,argument,status,claimed_revision) VALUES('task',1,'claimed','mcp_status',NULL,'prepared',3)",
    ] {
        assert!(c.execute(refused, []).is_err(), "{refused}");
    }
    c.execute("UPDATE lifecycle_controls SET status='uncertain'", []).unwrap();
    assert!(c.execute("UPDATE lifecycle_controls SET status='handed'", []).is_err(), "a receipt is final");
}

#[test]
fn a_claude_bg_execution_is_never_controlled() {
    let mut fixture = Fixture::new();
    fixture.add_task("task");
    let mut tx = fixture.transaction();
    tx.create_without_text(&"task".into(), id("initial")).unwrap();
    tx.apply(&"task".into(), 0, "admit", claude_bg("task")).unwrap();
    tx.commit().unwrap();
    fixture.runtime("task", RuntimeFact::ResourceAvailable);
    fixture.runtime("task", RuntimeFact::TurnStarted { turn: TurnId::new("root").unwrap(), trigger: Trigger::Input(id("initial")) });
    assert!(matches!(request(&mut fixture, "mcp", Control::ReadMcpStatus), Err(Error::NotDispatchable)));
    assert_eq!(fixture.count("lifecycle_controls"), 0);
    assert!(fixture.connection.execute("INSERT INTO lifecycle_controls(task_id,generation,control_id,kind,argument,status) VALUES('task',1,'mcp','mcp_status',NULL,'prepared')", []).is_err());
}

fn decisions() -> impl Iterator<Item = Capability> {
    every_control().into_iter().chain([Capability::Decisions])
}
fn bash_request(epoch: u64) -> DecisionRequest {
    DecisionRequest::new(format!("request-{epoch}"), Some(format!("toolu-{epoch}")), DecisionKind::Permission,
        format!(r#"{{"tool":"Bash","input":{{"command":"make test-{epoch}"}}}}"#).into_bytes()).unwrap()
}
/// Claude asks at `epoch`: the request is kept and its card is required, as the owner does it.
fn ask_decision(fixture: &mut Fixture, stamp: &ExecutionStamp, epoch: u64) -> InteractionId {
    let card = InteractionId(format!("decision-task-{}-{epoch}", stamp.generation()));
    fixture.card("task", &card.0);
    let mut tx = fixture.transaction();
    tx.put_decision_request(stamp, epoch, &bash_request(epoch)).unwrap();
    tx.commit().unwrap();
    fixture.runtime("task", RuntimeFact::Ask { card: card.clone(), callback: decision_callback(stamp.generation(), epoch), reason: WaitReason::Permission });
    card
}
/// The user decides `card` and the response is staged in the same commit.
fn decide_card(fixture: &mut Fixture, card: &InteractionId, epoch: u64, decision: Decision) -> alondra_store::Result<Committed> {
    let mut tx = fixture.transaction();
    let task: TaskId = "task".into();
    let revision = tx.load(&task)?.revision();
    tx.apply(&task, revision, &format!("decide-{revision}"), Event::Decide { card: card.clone(), decision: decision.clone() })?;
    tx.settle_card(&task, card, CardSettlement::Answer(CardAnswer::from_object(serde_json::Map::new()).unwrap()))?;
    tx.stage_response(&task, epoch, &decision)?;
    tx.commit()
}

#[test]
fn a_decision_request_is_kept_once_and_another_at_its_epoch_conflicts() {
    let mut fixture = Fixture::new();
    let stamp = running_sdk(&mut fixture, &control_binding(decisions()));
    let mut tx = fixture.transaction();
    tx.put_decision_request(&stamp, 3, &bash_request(3)).unwrap();
    tx.put_decision_request(&stamp, 3, &bash_request(3)).unwrap();
    tx.commit().unwrap();
    assert_eq!(fixture.count("lifecycle_decision_requests"), 1);
    let mut tx = fixture.transaction();
    assert!(matches!(tx.put_decision_request(&stamp, 3, &bash_request(4)), Err(Error::ObservationConflict)));
    drop(tx);
    for (tool_use, kind) in [(None, DecisionKind::Permission), (Some("toolu".to_owned()), DecisionKind::Elicitation)] {
        assert!(matches!(DecisionRequest::new("request".into(), tool_use, kind, b"{}".to_vec()), Err(Error::InvalidInput)), "{kind:?}");
    }
    assert!(matches!(DecisionRequest::new("request".into(), None, DecisionKind::Elicitation, vec![]), Err(Error::InvalidInput)));
    assert!(DecisionRequest::new("request".into(), None, DecisionKind::Elicitation, b"{}".to_vec()).is_ok());
}

#[test]
fn a_decided_response_is_staged_claimed_and_carries_its_request() {
    let mut fixture = Fixture::new();
    let binding = control_binding(decisions());
    let stamp = running_sdk(&mut fixture, &binding);
    let grant = owner(&mut fixture, &stamp);
    let card = ask_decision(&mut fixture, &stamp, 7);
    let decision = Decision::Deny { message: "The user denied it".into() };
    let mut committed = decide_card(&mut fixture, &card, 7, decision.clone()).unwrap();
    let permit = committed.control_permits.pop().unwrap();
    assert_eq!(permit.control().as_str(), decision_callback(stamp.generation(), 7).as_str());
    let mut tx = fixture.transaction();
    assert!(tx.claim_control(permit).unwrap());
    let attempt = tx.commit().unwrap().control_attempts.pop().unwrap();
    let ControlOperation::Respond(response) = attempt.operation() else { panic!("a response: {:?}", attempt.operation()) };
    assert_eq!((response.epoch(), response.request(), response.decision()), (7, &bash_request(7), &decision));
    let mut tx = fixture.transaction();
    tx.record_owned_control(attempt, grant.generation(), CommandEvidence::new(b"respond".to_vec()).unwrap()).unwrap();
    let control = tx.commit().unwrap().control_commands.pop().unwrap();
    let mut tx = fixture.transaction();
    tx.record_control_receipt(control, DispatchReceipt::Handed).unwrap();
    tx.commit().unwrap();
    assert_eq!(control_status(&fixture, decision_callback(stamp.generation(), 7).as_str()), "handed");
}

#[test]
fn a_policy_decision_is_staged_like_the_users() {
    let mut fixture = Fixture::new();
    let stamp = running_sdk(&mut fixture, &control_binding(decisions()));
    let mut tx = fixture.transaction();
    tx.put_decision_request(&stamp, 4, &bash_request(4)).unwrap();
    tx.commit().unwrap();
    let decision = Decision::Allow;
    let callback = decision_callback(stamp.generation(), 4);
    let mut tx = fixture.transaction();
    let aggregate = tx.load(&"task".into()).unwrap();
    let event = Event::Runtime { stamp: stamp.clone(), cursor: aggregate.cursor().unwrap() + 1, fact: RuntimeFact::AnsweredByPolicy { callback, decision: decision.clone() } };
    tx.apply(&"task".into(), aggregate.revision(), "policy", event).unwrap();
    tx.stage_response(&"task".into(), 4, &decision).unwrap();
    assert_eq!(tx.commit().unwrap().control_permits.len(), 1);
}

/// The causal test of a response's capability: a binding without decisions refuses it before
/// anything is written, the domain's decision included.
#[test]
fn a_response_the_binding_does_not_permit_is_refused_and_writes_nothing() {
    let mut fixture = Fixture::new();
    let stamp = running_sdk(&mut fixture, &control_binding(every_control()));
    let card = ask_decision(&mut fixture, &stamp, 2);
    let before = (fixture.count("lifecycle_events"), fixture.count("lifecycle_controls"));
    assert!(matches!(decide_card(&mut fixture, &card, 2, Decision::Allow), Err(Error::UnsupportedCapability)));
    assert_eq!((fixture.count("lifecycle_events"), fixture.count("lifecycle_controls")), before);
}

/// Only the decision the journal holds, for the request its callback asked at that epoch, is
/// staged: another decision, a stale epoch or a callback nobody decided write nothing.
#[test]
fn a_response_needs_the_decision_the_journal_holds_at_its_epoch() {
    let mut fixture = Fixture::new();
    let stamp = running_sdk(&mut fixture, &control_binding(decisions()));
    let first = ask_decision(&mut fixture, &stamp, 5);
    let _second = ask_decision(&mut fixture, &stamp, 6);
    let task: TaskId = "task".into();
    for (epoch, staged) in [(5, Decision::Deny { message: "No".into() }), (6, Decision::Allow), (4, Decision::Allow)] {
        let mut tx = fixture.transaction();
        let revision = tx.load(&task).unwrap().revision();
        tx.apply(&task, revision, "decide", Event::Decide { card: first.clone(), decision: Decision::Allow }).unwrap();
        assert!(matches!(tx.stage_response(&task, epoch, &staged), Err(Error::MissingIntent)), "epoch {epoch}: {staged:?}");
        drop(tx);
    }
    assert_eq!(fixture.count("lifecycle_controls"), 0);
    decide_card(&mut fixture, &first, 5, Decision::Allow).unwrap();
    let mut tx = fixture.transaction();
    tx.stage_response(&task, 5, &Decision::Allow).unwrap();
    assert!(matches!(tx.stage_response(&task, 5, &Decision::Deny { message: "No".into() }), Err(Error::MissingIntent)));
    drop(tx);
    assert_eq!(fixture.count("lifecycle_controls"), 1);
}

/// A response is sent only while its callback still waits for it: once the request was aborted
/// (an interrupt) or settled, the prepared response is revoked and claims nothing.
#[test]
fn a_response_whose_callback_is_over_is_revoked_instead_of_claimed() {
    for over in ["aborted", "settled"] {
        let mut fixture = Fixture::new();
        let stamp = running_sdk(&mut fixture, &control_binding(decisions()));
        let card = ask_decision(&mut fixture, &stamp, 8);
        let permit = decide_card(&mut fixture, &card, 8, Decision::Allow).unwrap().control_permits.pop().unwrap();
        let callback = decision_callback(stamp.generation(), 8);
        match over {
            "aborted" => fixture.runtime("task", RuntimeFact::CallbackAborted { callback, unheard_card: "unused".into() }),
            _ => fixture.runtime("task", RuntimeFact::CallbackSettled { callback, continuation: decision_continuation(stamp.generation(), 8) }),
        };
        let mut tx = fixture.transaction();
        assert!(!tx.claim_control(permit).unwrap(), "{over}: nothing waits for the response");
        let committed = tx.commit().unwrap();
        assert!(committed.control_attempts.is_empty(), "{over}");
        assert_eq!(control_status(&fixture, decision_callback(stamp.generation(), 8).as_str()), "revoked", "{over}");
    }
}

#[test]
fn decision_evidence_is_immutable_and_a_response_names_its_request() {
    let mut fixture = Fixture::new();
    let stamp = running_sdk(&mut fixture, &control_binding(decisions()));
    let card = ask_decision(&mut fixture, &stamp, 9);
    decide_card(&mut fixture, &card, 9, Decision::Allow).unwrap();
    let c = &fixture.connection;
    for refused in [
        "UPDATE lifecycle_decision_requests SET payload=x'00'",
        "DELETE FROM lifecycle_decision_requests",
        "UPDATE lifecycle_controls SET epoch=10",
        "UPDATE lifecycle_controls SET argument='{\"kind\":\"deny\",\"data\":{\"message\":\"No\"}}'",
        "INSERT INTO lifecycle_controls(task_id,generation,control_id,kind,argument,status) VALUES('task',1,'respond','respond','{}','prepared')",
        "INSERT INTO lifecycle_controls(task_id,generation,control_id,kind,argument,epoch,status) VALUES('task',1,'mode','set_permission_mode','plan',9,'prepared')",
        "INSERT INTO lifecycle_controls(task_id,generation,control_id,kind,argument,epoch,status) VALUES('task',1,'other','respond','not json',9,'prepared')",
        "INSERT INTO lifecycle_controls(task_id,generation,control_id,kind,argument,epoch,status) VALUES('task',1,'other','respond','{}',99,'prepared')",
        "INSERT INTO lifecycle_decision_requests(task_id,generation,epoch,request_id,tool_use_id,kind,payload) VALUES('task',1,20,'r','toolu','elicitation',x'7b7d')",
        "INSERT INTO lifecycle_decision_requests(task_id,generation,epoch,request_id,tool_use_id,kind,payload) VALUES('task',1,21,'r',NULL,'permission',x'7b7d')",
        "INSERT INTO lifecycle_decision_requests(task_id,generation,epoch,request_id,tool_use_id,kind,payload) VALUES('task',1,22,'r','toolu','approval',x'7b7d')",
    ] {
        assert!(c.execute(refused, []).is_err(), "{refused}");
    }
}

#[test]
fn a_claude_bg_execution_asks_for_no_decision() {
    let mut fixture = Fixture::new();
    fixture.add_task("task");
    let mut tx = fixture.transaction();
    tx.create_without_text(&"task".into(), id("initial")).unwrap();
    tx.apply(&"task".into(), 0, "admit", claude_bg("task")).unwrap();
    tx.commit().unwrap();
    let stamp = fixture.transaction().load(&"task".into()).unwrap().execution().unwrap().clone();
    let mut tx = fixture.transaction();
    assert!(matches!(tx.put_decision_request(&stamp, 1, &bash_request(1)), Err(Error::NotDispatchable)));
    drop(tx);
    assert!(fixture.connection.execute("INSERT INTO lifecycle_decision_requests(task_id,generation,epoch,request_id,tool_use_id,kind,payload) VALUES('task',1,1,'r','toolu','permission',x'7b7d')", []).is_err());
}

/// Puts back the narrower checks the two control tables had in `previous`, with every row they
/// hold, as a journal installed by an earlier release still has them.
fn narrow_to(fixture: &mut Fixture, previous: &str) {
    let tx = fixture.connection.transaction().unwrap();
    for name in ["provider_binding_controls", "lifecycle_controls"] {
        let start = previous.find(&format!("CREATE TABLE IF NOT EXISTS {name}(")).unwrap();
        let definition = &previous[start..start + previous[start..].find("\n);").unwrap() + 3];
        tx.execute_batch(&format!(
            "PRAGMA defer_foreign_keys=ON; CREATE TEMP TABLE kept AS SELECT * FROM main.{name}; DROP TABLE main.{name}; {definition} INSERT INTO main.{name} SELECT * FROM temp.kept; DROP TABLE temp.kept;"
        )).unwrap();
    }
    tx.execute_batch(include_str!("../src/controls.sql")).unwrap();
    tx.commit().unwrap();
}
fn controls_rows(fixture: &Fixture) -> Vec<String> {
    let mut rows = vec![];
    for sql in ["SELECT json_array(id,task_id,generation,control_id,kind,argument,epoch,status,claimed_revision) FROM lifecycle_controls ORDER BY id",
        "SELECT json_array(binding_id,capabilities) FROM provider_binding_controls ORDER BY binding_id"] {
        let mut statement = fixture.connection.prepare(sql).unwrap();
        rows.extend(statement.query_map([], |row| row.get::<_, String>(0)).unwrap().map(|row| row.unwrap()));
    }
    rows
}

#[test]
fn a_journal_from_before_the_mcp_controls_keeps_every_control_and_takes_the_new_ones() {
    let mut fixture = Fixture::new();
    running_sdk(&mut fixture, &control_binding(every_control()));
    request(&mut fixture, "mcp", Control::ReadMcpStatus).unwrap();
    narrow_to(&mut fixture, include_str!("../src/controls_before_mcp.sql"));
    let raw = |kind: &str| format!("INSERT INTO lifecycle_controls(task_id,generation,control_id,kind,argument,status) VALUES('task',1,'raw-{kind}','{kind}','linear','prepared')");
    assert!(fixture.connection.execute(&raw("mcp_sign_in"), []).is_err(), "the narrower check refuses an MCP sign-in");
    let before = controls_rows(&fixture);
    assert_eq!(before.len(), 2, "{before:?}");
    let tx = fixture.connection.transaction().unwrap();
    assert!(matches!(LifecycleTransaction::attach(tx, AuthorityScope::new("epoch".into()).unwrap()), Err(Error::UnsupportedJournalFormat)), "an old journal opens only once upgraded");
    let mut tx = fixture.connection.transaction().unwrap();
    LifecycleTransaction::upgrade(&mut tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(controls_rows(&fixture), before, "every row is kept");
    fixture.transaction().commit().unwrap();
    assert!(fixture.connection.execute("DELETE FROM lifecycle_controls", []).is_err(), "the retained-evidence trigger is back");
    for kind in ["mcp_sign_in", "mcp_reconnect"] {
        fixture.connection.execute(&raw(kind), []).unwrap();
    }
    let mut tx = fixture.connection.transaction().unwrap();
    LifecycleTransaction::upgrade(&mut tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(controls_rows(&fixture).len(), 4, "upgrading again changes nothing");
}

#[test]
fn a_journal_from_before_force_stop_keeps_every_control_and_takes_force_stops() {
    let mut fixture = Fixture::new();
    running_sdk(&mut fixture, &control_binding(every_control()));
    request(&mut fixture, "mcp", Control::ReadMcpStatus).unwrap();
    narrow_to(&mut fixture, include_str!("../src/controls_before_force_stop.sql"));
    let force_stop = |id: &str| format!("INSERT INTO lifecycle_controls(task_id,generation,control_id,kind,argument,status) VALUES('task',1,'{id}','force_stop','turn-1','prepared')");
    assert!(fixture.connection.execute(&force_stop("raw-before"), []).is_err(), "the narrower check refuses a force stop");
    let before = controls_rows(&fixture);
    let mut tx = fixture.connection.transaction().unwrap();
    LifecycleTransaction::upgrade(&mut tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(controls_rows(&fixture), before, "every row is kept");
    fixture.transaction().commit().unwrap();
    fixture.connection.execute(&force_stop("raw-after"), []).unwrap();
    assert!(fixture.connection.execute("INSERT INTO lifecycle_controls(task_id,generation,control_id,kind,status) VALUES('task',1,'raw-turnless','force_stop','prepared')", []).is_err(),
        "a force stop always names its turn");
}

#[test]
fn a_journal_from_before_clearing_mcp_sign_ins_keeps_every_control_and_takes_the_sign_outs() {
    let mut fixture = Fixture::new();
    running_sdk(&mut fixture, &control_binding(every_control()));
    request(&mut fixture, "mcp", Control::ReadMcpStatus).unwrap();
    narrow_to(&mut fixture, include_str!("../src/controls_before_clear_auth.sql"));
    let sign_out = |id: &str| format!("INSERT INTO lifecycle_controls(task_id,generation,control_id,kind,argument,status) VALUES('task',1,'{id}','mcp_clear_auth','linear','prepared')");
    assert!(fixture.connection.execute(&sign_out("raw-before"), []).is_err(), "the narrower check refuses a sign-out");
    let before = controls_rows(&fixture);
    let mut tx = fixture.connection.transaction().unwrap();
    LifecycleTransaction::upgrade(&mut tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(controls_rows(&fixture), before, "every row is kept");
    fixture.transaction().commit().unwrap();
    fixture.connection.execute(&sign_out("raw-after"), []).unwrap();
    assert!(fixture.connection.execute("INSERT INTO lifecycle_controls(task_id,generation,control_id,kind,status) VALUES('task',1,'raw-serverless','mcp_clear_auth','prepared')", []).is_err(),
        "a sign-out always names its server");
}

#[test]
fn a_binding_with_every_control_signs_in_and_reconnects_its_mcp_servers() {
    for (name, kind) in [("reconnect", Control::ReconnectMcp { server: "linear".into() }), ("sign-in", Control::StartMcpSignIn { server: "linear".into() }),
        ("sign-out", Control::ClearMcpSignIn { server: "linear".into() })] {
        let mut fixture = Fixture::new();
        let mut capabilities = every_control().to_vec();
        capabilities.extend([Capability::Decisions, Capability::McpReconnect, Capability::McpSignIn, Capability::McpClearAuth]);
        let binding = control_binding(capabilities);
        running_sdk(&mut fixture, &binding);
        let permit = request(&mut fixture, name, kind.clone()).unwrap().control_permits.pop().unwrap();
        let mut tx = fixture.transaction();
        assert!(tx.claim_control(permit).unwrap());
        tx.commit().unwrap();
        assert_eq!(control_status(&fixture, name), "claimed");
        let stored: (String, String) = fixture.connection.query_row("SELECT kind,argument FROM lifecycle_controls WHERE control_id=?1", [name], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(stored.1, "linear", "{kind:?}");
        let mut narrow = Fixture::new();
        running_sdk(&mut narrow, &control_binding(every_control()));
        let mut tx = narrow.transaction();
        let revision = tx.load(&"task".into()).unwrap().revision();
        tx.apply(&"task".into(), revision, name, Event::RequestControl { control: op(name), kind: kind.clone() }).unwrap();
        assert!(matches!(tx.stage_control(&"task".into(), &op(name), &kind), Err(Error::UnsupportedCapability)), "a binding from before the MCP controls is never asked for them");
    }
}

fn observe(fixture: &mut Fixture, grant: &OwnerGrant, cursor: u64, evidence: &[u8]) {
    let mut tx = fixture.transaction();
    tx.accept_owned_observation(grant.execution(), grant.generation(), cursor, CommandEvidence::new(evidence.to_vec()).unwrap()).unwrap();
    tx.put_normalization_checkpoint(grant.execution(), cursor, (cursor > 1).then(|| cursor - 1), b"checkpoint".to_vec()).unwrap();
    tx.settle_observation(grant.execution(), cursor, 0).unwrap();
    tx.commit().unwrap();
}
fn observed(cursor: u64) -> Vec<u8> {
    let mut bytes = format!("observation {cursor} ").into_bytes();
    bytes.resize(16 * 1024, b'x');
    bytes
}
/// Rows of `provider_observations` and the bytes they hold.
fn held(fixture: &Fixture) -> (i64, i64) {
    fixture.connection.query_row("SELECT count(*),coalesce(sum(length(bytes)),0) FROM provider_observations", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
}

#[test]
fn normalized_observations_keep_only_their_digest_and_copies_are_still_told_apart() {
    let mut fixture = Fixture::new();
    let stamp = running_sdk(&mut fixture, &control_binding(every_control()));
    let grant = owner(&mut fixture, &stamp);
    for cursor in 1..=400 {
        observe(&mut fixture, &grant, cursor, &observed(cursor));
    }
    assert_eq!(held(&fixture), (400, 16 * 1024 + 399), "only the checkpoint's observation keeps its bytes");
    let tx = fixture.transaction();
    assert_eq!(tx.normalization_checkpoint(&stamp).unwrap().unwrap().cursor(), 400);
    drop(tx);
    for cursor in [7, 399, 400] {
        let mut tx = fixture.transaction();
        let again = tx.accept_owned_observation(&stamp, grant.generation(), cursor, CommandEvidence::new(observed(cursor)).unwrap()).unwrap();
        assert_eq!(again, ObservationAcceptance::Duplicate, "cursor {cursor}");
        assert_eq!(tx.commit().unwrap().observations.len(), 1);
        let mut tx = fixture.transaction();
        let other = tx.accept_owned_observation(&stamp, grant.generation(), cursor, CommandEvidence::new(observed(cursor + 1)).unwrap());
        assert!(matches!(other, Err(Error::ObservationConflict)), "cursor {cursor}: {other:?}");
    }
    assert_eq!(held(&fixture), (400, 16 * 1024 + 399));
}

#[test]
fn the_journal_refuses_to_release_evidence_not_normalized_or_to_drop_evidence_still_held() {
    let mut fixture = Fixture::new();
    let stamp = running_sdk(&mut fixture, &control_binding(every_control()));
    let grant = owner(&mut fixture, &stamp);
    for cursor in 1..=3 {
        observe(&mut fixture, &grant, cursor, &observed(cursor));
    }
    for refused in [
        "UPDATE provider_observations SET bytes=X'00' WHERE cursor=3",
        "UPDATE provider_observations SET digest=zeroblob(32) WHERE cursor=1",
        "UPDATE provider_observations SET bytes=X'01' WHERE cursor=1",
        "DELETE FROM provider_observations WHERE cursor=3",
    ] {
        assert!(fixture.connection.execute(refused, []).is_err(), "{refused}");
    }
    assert_eq!(held(&fixture), (3, 16 * 1024 + 2));
}

#[test]
fn retention_releases_old_evidence_in_bounded_passes_and_drops_it_once_the_execution_ends() {
    let mut fixture = Fixture::new();
    let stamp = running_sdk(&mut fixture, &control_binding(every_control()));
    owner(&mut fixture, &stamp);
    let binding: String = fixture.connection.query_row("SELECT binding_id FROM lifecycle_execution_bindings WHERE task_id='task'", [], |row| row.get(0)).unwrap();
    for cursor in 1..=40u64 {
        let bytes = observed(cursor);
        let sum: Vec<u8> = { use sha2::Digest; sha2::Sha256::digest(&bytes).to_vec() };
        fixture.connection.execute("INSERT INTO provider_observations(task_id,generation,binding_id,cursor,digest,bytes) VALUES('task',?1,?2,?3,?4,?5)", params![stamp.generation() as i64, binding, cursor as i64, sum, bytes]).unwrap();
    }
    fixture.connection.execute("INSERT INTO provider_observation_heads(task_id,generation,cursor) VALUES('task',?1,40)", params![stamp.generation() as i64]).unwrap();
    fixture.connection.execute("INSERT INTO provider_normalization_checkpoints(task_id,generation,binding_id,cursor,digest,bytes) VALUES('task',?1,?2,40,?3,X'01')", params![stamp.generation() as i64, binding, { use sha2::Digest; sha2::Sha256::digest([1u8]).to_vec() }]).unwrap();
    assert_eq!(held(&fixture), (40, 40 * 16 * 1024), "a journal from before the retention holds every observation");
    assert!(fixture.connection.execute("DELETE FROM provider_observations WHERE cursor=1", []).is_err(), "an observation that holds its bytes is never dropped");
    let mut passes = vec![];
    loop {
        let mut tx = fixture.transaction();
        let pass = tx.retain_evidence(16).unwrap();
        tx.commit().unwrap();
        passes.push(pass);
        if pass.released < 16 {
            break;
        }
    }
    assert_eq!(passes.iter().map(|pass| pass.released).collect::<Vec<_>>(), [16, 16, 7]);
    assert!(passes.iter().all(|pass| pass.removed == 0), "the current execution keeps its digests: {passes:?}");
    assert_eq!(held(&fixture), (40, 16 * 1024 + 39));
    let mut tx = fixture.transaction();
    let revision = tx.load(&"task".into()).unwrap().revision();
    tx.put_input(&"task".into(), payload("requeue")).unwrap();
    tx.apply(&"task".into(), revision, "release", Event::ExecutionReleased { input: id("requeue") }).unwrap();
    tx.commit().unwrap();
    let mut removed = vec![];
    for _ in 0..4 {
        let mut tx = fixture.transaction();
        removed.push(tx.retain_evidence(16).unwrap());
        tx.commit().unwrap();
    }
    assert_eq!(removed.iter().map(|pass| pass.removed).collect::<Vec<_>>(), [16, 16, 7, 0]);
    assert_eq!(held(&fixture), (1, 16 * 1024), "an ended execution keeps only its checkpoint's observation");
    let kept: i64 = fixture.connection.query_row("SELECT cursor FROM provider_observations", [], |row| row.get(0)).unwrap();
    assert_eq!(kept, 40);
}

fn commands(fixture: &Fixture) -> i64 {
    fixture.connection.query_row("SELECT count(*) FROM provider_commands", [], |row| row.get(0)).unwrap()
}
fn retain(fixture: &mut Fixture) -> EvidenceRetention {
    let mut tx = fixture.transaction();
    let pass = tx.retain_evidence(16).unwrap();
    tx.commit().unwrap();
    pass
}

#[test]
fn a_provider_command_is_removed_only_once_its_execution_is_no_longer_current() {
    let mut fixture = Fixture::new();
    let owner = crashed_claim(&mut fixture, b"framed command");
    let stamp = owner.execution().clone();
    assert_eq!(retain(&mut fixture).commands_removed, 0, "a claimed send keeps the bytes it may send again");
    assert!(fixture.connection.execute("DELETE FROM provider_commands", []).is_err());
    let intent = fixture.transaction().unresolved_commands(&"task".into()).unwrap()[0].intent_id;
    let mut tx = fixture.transaction();
    tx.resolve_dispatch(&stamp, intent, owner.generation(), DispatchReceipt::Handed).unwrap();
    tx.commit().unwrap();
    assert_eq!(retain(&mut fixture).commands_removed, 0, "the current execution keeps its commands");
    assert!(fixture.connection.execute("DELETE FROM provider_commands", []).is_err());
    let mut tx = fixture.transaction();
    let revision = tx.load(&"task".into()).unwrap().revision();
    tx.put_input(&"task".into(), payload("requeue")).unwrap();
    tx.apply(&"task".into(), revision, "release", Event::ExecutionReleased { input: id("requeue") }).unwrap();
    tx.commit().unwrap();
    assert_eq!(commands(&fixture), 1);
    assert_eq!(retain(&mut fixture).commands_removed, 1);
    assert_eq!(commands(&fixture), 0);
    assert_eq!(retain(&mut fixture).commands_removed, 0);
}

fn texts(fixture: &Fixture) -> (i64, i64) {
    fixture.connection.query_row("SELECT count(*),count(body) FROM lifecycle_inputs WHERE task_id='task'", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
}

#[test]
fn an_input_lets_go_of_its_text_only_once_its_task_is_done_or_cancelled() {
    let mut fixture = Fixture::new();
    fixture.create("task");
    let mut tx = fixture.transaction();
    tx.put_input(&"task".into(), payload("told")).unwrap();
    tx.commit().unwrap();
    assert_eq!(texts(&fixture), (2, 2));
    assert_eq!(retain(&mut fixture).texts_released, 0, "a task that runs keeps the texts it may still send");
    assert!(fixture.connection.execute("UPDATE lifecycle_inputs SET body=NULL", []).is_err());
    for state in ["done", "cancelled"] {
        fixture.connection.execute("UPDATE tasks SET state=?1 WHERE id='task'", [state]).unwrap();
        assert!(fixture.connection.execute("UPDATE lifecycle_inputs SET body='other' WHERE input_id='told'", []).is_err(), "a text is never rewritten");
    }
    assert_eq!(retain(&mut fixture).texts_released, 2);
    assert_eq!(texts(&fixture), (2, 0), "the inputs stay, without their text");
    assert_eq!(retain(&mut fixture).texts_released, 0);
    let tx = fixture.transaction();
    tx.load(&"task".into()).expect("the journal still reads without the texts");
}
