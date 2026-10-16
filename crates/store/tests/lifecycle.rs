use alondra_domain::*;
use alondra_foundation::{InteractionId, Revision, TaskId};
use alondra_provider_contract::{Capability, Placement, ProviderBinding};
use alondra_store::*;
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use std::collections::BTreeSet;
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
                "lifecycle_current_execution"
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
    assert!(matches!(
        tx.apply(&"task".into(), 1, "admit", admit("node", "instance")),
        Err(Error::CommandConflict)
    ));
    assert!(matches!(
        tx.apply(&"task".into(), 0, "admit", admit("node", "other")),
        Err(Error::CommandConflict)
    ));
    tx.commit().unwrap();
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
    let mut tx =
        LifecycleTransaction::attach(tx, AuthorityScope::new("new-epoch".into()).unwrap()).unwrap();
    assert!(matches!(tx.load(&"task".into()), Err(Error::WrongScope)));
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
    let mut tx = fixture.transaction();
    for card in ["missing", "wrong-owner", "answered"] {
        assert!(matches!(
            tx.apply(&"task".into(), 0, card, Event::Ask { card: card.into() }),
            Err(Error::InvalidCard)
        ));
        assert_eq!(tx.load(&"task".into()).unwrap().revision(), 0);
    }
    tx.commit().unwrap();
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
fn duplicate_node_and_unknown_task_dependency_fail_atomically() {
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
            admit("node", "second-instance")
        ),
        Err(Error::Database(_))
    ));
    assert_eq!(tx.load(&"other".into()).unwrap().revision(), 0);
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
    assert_eq!(tx.load(&"other".into()).unwrap().revision(), 0);
    tx.commit().unwrap();
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
    tx.apply(
        &"task".into(),
        0,
        "ask",
        Event::Ask {
            card: "question".into(),
        },
    )
    .unwrap();
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
    assert_eq!(tx.load(&"task".into()).unwrap().revision(), 1);
    tx.commit().unwrap();
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
    assert_eq!(
        tx.load(&"task".into())
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
    tx.commit().unwrap();
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
    tx.commit().unwrap();
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
        "failed cursor write must roll back its receipt even when the outer transaction commits"
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
    tx.reserve_owner(&stamp, 1).unwrap();
    let second = tx.commit().unwrap().owners.pop().unwrap();
    assert_eq!(second.generation(), 2);
    let tx = fixture.transaction();
    assert!(matches!(tx.fence_owner(&first), Err(Error::StaleOwner)));
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
    assert!(tx.commit().unwrap().attempts.is_empty());
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
    let mut tx = LifecycleTransaction::attach(reopened.transaction().unwrap(), scope()).unwrap();
    assert!(matches!(
        tx.execution_binding(&stamp),
        Err(Error::MissingBinding)
    ));
    assert!(matches!(
        tx.reserve_owner(&stamp, 0),
        Err(Error::MissingBinding)
    ));
    tx.commit().unwrap();
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
    assert!(matches!(
        tx.put_normalization_checkpoint(&stamp, 1, Some(1), b"different snapshot".to_vec()),
        Err(Error::CheckpointConflict)
    ));
    assert!(matches!(
        tx.put_normalization_checkpoint(&stamp, 0, Some(1), b"old cursor".to_vec()),
        Err(Error::MetadataCapacity)
    ));
    assert!(matches!(
        tx.put_normalization_checkpoint(&stamp, 2, Some(1), vec![0; 65537]),
        Err(Error::MetadataCapacity)
    ));
    tx.commit().unwrap();
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
    tx.commit().unwrap();
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
    assert!(matches!(
        tx.put_adapter_input(
            &stamp,
            &id("initial"),
            AdapterInputMetadata::new(b"replacement UUID forbidden".to_vec()).unwrap()
        ),
        Err(Error::InputConflict)
    ));
    assert!(matches!(
        tx.put_adapter_conversation(
            &stamp,
            AdapterInputMetadata::new(b"regenerated session forbidden".to_vec()).unwrap()
        ),
        Err(Error::InputConflict)
    ));
    tx.reserve_owner(&stamp, 0).unwrap();
    let grant = tx.commit().unwrap().owners.pop().unwrap();
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
    assert!(matches!(
        tx.lookup_adapter_inputs(&stamp, &vec![b"native-1".to_vec(); 65]),
        Err(Error::MetadataCapacity)
    ));
    tx.commit().unwrap();
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
        tx.adapter_input(&stamp, &id("initial")).unwrap().bytes(),
        b"immutable initial native key"
    );
    assert_eq!(
        tx.adapter_conversation(&stamp).unwrap().bytes(),
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
    assert!(tx.commit().unwrap().observations.is_empty());
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
        let mut tx = LifecycleTransaction::attach(raw, scope()).unwrap();
        let aggregate = tx.load(&"task".into()).unwrap();
        if stale_owner {
            assert_eq!(rows, 0);
            assert_eq!(checkpoints, 0);
            assert_eq!(aggregate.execution(), Some(&stamp));
            assert_eq!(aggregate.revision(), before_retirement_revision);
            tx.fence_owner(&owner).unwrap();
            assert!(tx.normalization_checkpoint(&stamp).unwrap().is_none());
            assert_eq!(aggregate.phase(), TaskPhase::Finishing);
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
            assert!(
                matches!(tx.normalization_checkpoint(&stamp), Err(Error::StalePermit)),
                "the public metadata read does not become a historical admission API"
            );
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
        }
        assert!(tx.commit().unwrap().observations.is_empty());
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
        assert!(
            matches!(
                tx.put_normalization_checkpoint(&stamp, 2, None, b"wrong cursor".to_vec()),
                Err(Error::StalePermit)
            ),
            "retirement cannot authorize a checkpoint for an unaccepted cursor"
        );
        let checkpoint =
            tx.put_normalization_checkpoint(&stamp, 1, None, b"normalized retirement".to_vec());
        assert!(
            checkpoint.is_ok(),
            "same-transaction accepted raw must permit checkpoint after retirement: {checkpoint:?}"
        );
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
        let mut tx = LifecycleTransaction::attach(raw, scope()).unwrap();
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
            assert!(
                matches!(tx.normalization_checkpoint(&stamp), Err(Error::StalePermit)),
                "the public metadata read does not become a historical admission API"
            );
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
        }
        assert!(tx.commit().unwrap().observations.is_empty());
        drop(reopened);
    }
}
