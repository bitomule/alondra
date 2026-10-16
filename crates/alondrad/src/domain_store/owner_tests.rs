//! One owner transaction on the real domain database: guarded domain commands, provider
//! observations and the lifecycle journal commit together, and the runtime publishes the
//! committed state before it acknowledges. Lifecycle fixtures are not native SDK observations.
use super::*;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::tests::{Fixture, seeded};
use alondra_agent_application::{Normalization, ObservationMetadata};
use alondra_domain::{Event as LifecycleEvent, RuntimeFact, Trigger};
use alondra_foundation::{ConversationId, HostId, InstanceId, TurnId};
use alondra_provider_contract::{AdapterError, Capability, Observation, Placement};
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::mpsc;

const TASK: &str = "task-1";
const CATALOG: &str = include_str!("../../../../fixtures/domain-v2/acceptance.json");

fn agent() -> Actor {
    Actor::Agent {
        agent_id: "agent-1".into(),
    }
}
fn user() -> Actor {
    Actor::User {
        user_id: "composite-user".into(),
    }
}
fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}
fn task() -> TaskId {
    TASK.into()
}
fn envelope(state: &DomainState, id: &str, command: Command) -> CommandEnvelope {
    CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: id.into(),
        expected_revision: 0,
        command,
    }
}
fn ask(id: &str) -> Command {
    Command::AskQuestion {
        interaction_id: id.into(),
        scope: Scope::Task { task_id: task() },
        recipient: Recipient::User,
        body: "Choose the next step".into(),
        attachments: vec![],
        blocking: id == "composite-card",
        options: vec![QuestionOption {
            id: "continue".into(),
            label: "Continue".into(),
            description: None,
        }],
        card: CardSpec::derived(CardKind::Provide, "Choose the next step"),
    }
}
fn answer() -> Command {
    Command::AnswerQuestion {
        interaction_id: "composite-card".into(),
        option_id: "continue".into(),
        inputs: BTreeMap::new(),
        attachments: vec![],
    }
}
fn retitle(title: &str) -> Command {
    Command::RetitleTask {
        task_id: task(),
        title: title.into(),
    }
}
fn schedule() -> Command {
    let catalog: serde_json::Value = serde_json::from_str(CATALOG).unwrap();
    Command::CreateSchedule {
        schedule: serde_json::from_value(catalog["schedule_base"].clone()).unwrap(),
    }
}
fn payload(id: &str) -> InputPayload {
    InputPayload::new(InputId::new(id).unwrap(), format!("Authorized fixture input {id}")).unwrap()
}
fn binding() -> ProviderBinding {
    ProviderBinding::new(
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
    .unwrap()
}
fn batch(execution: &ExecutionStamp, cursor: u64) -> OrderedObservation {
    OrderedObservation {
        owner_generation: 1,
        execution: execution.clone(),
        cursor,
        evidence: format!("raw observation {cursor}").into_bytes(),
    }
}
fn scope_of(state: &DomainState) -> AuthorityScope {
    AuthorityScope::new(state.data_epoch.clone()).unwrap()
}

enum Normalizer {
    Available,
    SwallowsMetadataError,
}
impl ObservationNormalizer for Normalizer {
    fn normalize(
        &self,
        _: &OrderedObservation,
        metadata: &ObservationMetadata<'_, '_>,
    ) -> std::result::Result<Normalization, AdapterError> {
        if let Self::SwallowsMetadataError = self {
            assert!(metadata.lookup_inputs(&vec![b"native".to_vec(); 65]).is_err());
        }
        Normalization::new(vec![Observation::Available], b"normalized".to_vec())
    }
}

fn admit(journal: &mut LifecycleTransaction<'_>, revision: u64, label: &str) -> alondra_store::Result<()> {
    journal.apply(
        &task(),
        revision,
        &format!("fixture-admit-{label}"),
        LifecycleEvent::Admit {
            node: "agent-1".into(),
            conversation: ConversationId::new(format!("conversation-{label}")).unwrap(),
            instance: InstanceId::new(format!("resource-{label}")).unwrap(),
            host: HostId::new("fixture-service").unwrap(),
        },
    )?;
    Ok(())
}
fn launch(journal: &mut LifecycleTransaction<'_>) -> alondra_store::Result<ExecutionStamp> {
    journal.stage_launch(&task(), &Placement::RemoteWorkspace("fixture/workspace".into()), &binding())?;
    let execution = journal.load(&task())?.execution().unwrap().clone();
    journal.reserve_owner(&execution, 0)?;
    Ok(execution)
}

/// The Task waits on its card ("composite-card"), with the lifecycle journal installed.
fn waiting() -> (Fixture, DomainStore) {
    let (fixture, mut store) = installed();
    let scope = scope_of(&store.load().unwrap());
    store
        .own(scope, |owner| {
            let journal = owner.journal();
            journal.create(&task(), payload("initial"))?;
            journal.apply(&task(), 0, "fixture-wait", LifecycleEvent::Ask { card: "composite-card".into() })?;
            Ok(())
        })
        .unwrap();
    (fixture, store)
}
/// The Task runs an execution owned at generation 1 whose provider has observed nothing yet.
fn running() -> (Fixture, DomainStore, ExecutionStamp) {
    let (fixture, mut store) = installed();
    let scope = scope_of(&store.load().unwrap());
    let (execution, _) = store
        .own(scope, |owner| {
            let journal = owner.journal();
            journal.create(&task(), payload("initial"))?;
            admit(journal, 0, "running")?;
            Ok(launch(journal)?)
        })
        .unwrap();
    (fixture, store, execution)
}
/// The running execution's resource was released; its problem card is "composite-card".
fn released() -> (Fixture, DomainStore, ExecutionStamp) {
    let (fixture, mut store, execution) = running();
    let scope = scope_of(&store.load().unwrap());
    store
        .own(scope, |owner| {
            let journal = owner.journal();
            for (cursor, fact) in [
                RuntimeFact::ResourceAvailable,
                RuntimeFact::TurnStarted {
                    turn: TurnId::new("fixture-turn").unwrap(),
                    trigger: Trigger::Input(InputId::new("initial").unwrap()),
                },
                RuntimeFact::ResourceReleased { problem_card: "composite-card".into() },
            ]
            .into_iter()
            .enumerate()
            {
                let aggregate = journal.load(&task())?;
                journal.apply(
                    &task(),
                    aggregate.revision(),
                    &format!("fixture-fact-{cursor}"),
                    LifecycleEvent::Runtime {
                        stamp: execution.clone(),
                        cursor: u64::try_from(cursor).unwrap() + 1,
                        fact,
                    },
                )?;
            }
            Ok(())
        })
        .unwrap();
    (fixture, store, execution)
}
fn installed() -> (Fixture, DomainStore) {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let state = store.load().unwrap();
    store
        .execute_filling_revision(&agent(), &envelope(&state, "composite-question", ask("composite-card")))
        .unwrap();
    let mut install = store.conn.transaction().unwrap();
    LifecycleTransaction::install(&mut install).unwrap();
    install.commit().unwrap();
    (fixture, store)
}
fn runtime(fixture: &Fixture, store: DomainStore) -> DomainRuntime {
    drop(store);
    DomainRuntime::open_at(&fixture.0, &fixture.path()).unwrap()
}

const TABLES: [&str; 8] = [
    "provider_observations",
    "provider_normalization_checkpoints",
    "lifecycle_events",
    "lifecycle_inputs",
    "interactions",
    "semantic_receipts",
    "domain_outbox",
    "command_audit",
];
fn counts(conn: &Connection) -> [i64; 8] {
    TABLES.map(|table| conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap())
}
fn observer(fixture: &Fixture) -> Connection {
    Connection::open(fixture.path()).unwrap()
}
fn card(conn: &Connection, id: &str) -> Option<(Option<String>, bool)> {
    conn.query_row("SELECT answer,withdrawn FROM interactions WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .unwrap()
}
fn poisoned(result: std::result::Result<impl std::fmt::Debug, StoreError>) {
    match result {
        Err(StoreError::Storage(error)) => assert!(
            matches!(error.downcast_ref::<alondra_store::Error>(), Some(alondra_store::Error::Poisoned)),
            "expected Poisoned, got {error:?}"
        ),
        other => panic!("expected Poisoned, got {other:?}"),
    }
}
fn journal_after(store: &mut DomainStore) -> (Vec<String>, bool, usize) {
    let scope = scope_of(&store.load().unwrap());
    store
        .own(scope, |owner| {
            let aggregate = owner.journal().load(&task())?;
            Ok((
                aggregate.pending_inputs().iter().map(|id| id.as_str().to_owned()).collect(),
                aggregate.execution().is_some(),
                aggregate.cards().len(),
            ))
        })
        .unwrap()
        .0
}

// Case 1: atomicity seen from another connection, and readers agree before the acknowledgement.
#[test]
fn card_raw_observation_journal_and_checkpoint_commit_together_and_publish_before_acknowledging() {
    let (fixture, store, execution) = running();
    let runtime = runtime(&fixture, store);
    let observer = observer(&fixture);
    let before = counts(&observer);
    let state = Arc::clone(runtime.read().state());
    let mut changes = runtime.subscribe();
    let ((acceptance, inside), outcomes) = runtime
        .own(scope_of(&state), |owner| {
            let acceptance = owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available)?;
            owner.stage_command(&agent(), &envelope(&state, "owner-ask", ask("owner-card")), Attached::None)?;
            Ok((acceptance, counts(&observer)))
        })
        .unwrap();
    assert_eq!(acceptance, ObservationAcceptance::Accepted);
    assert_eq!(inside, before, "another connection sees nothing of the owner transaction before its commit");
    let after = counts(&observer);
    for (index, table) in TABLES.iter().enumerate().filter(|(_, table)| **table != "lifecycle_inputs") {
        assert!(after[index] > before[index], "{table} committed with the rest of the transaction");
    }
    let acknowledged = runtime.take_acknowledgements();
    assert_eq!(acknowledged.len(), 1);
    assert_eq!(acknowledged[0].observations.len(), 1);
    assert!(acknowledged[0].observations[0].matches_evidence(b"raw observation 1"));
    let published = runtime.read();
    assert_eq!(published.state().revision, outcomes[0].outcome.revision);
    assert!(published.state().interactions.contains_key(&InteractionId::from("owner-card")));
    assert!(changes.has_changed().unwrap(), "the change is notified");
    assert_eq!(*changes.borrow_and_update(), outcomes[0].outcome.revision);
}

// Case 2: a step error the caller catches poisons the owner: nothing commits, publishes or is acknowledged.
#[test]
fn a_caught_step_error_poisons_the_owner_and_nothing_commits_publishes_or_is_acknowledged() {
    for swallowed_by_normalizer in [false, true] {
        let (fixture, store, execution) = running();
        let runtime = runtime(&fixture, store);
        let observer = observer(&fixture);
        let before = counts(&observer);
        let read_before = runtime.read();
        let state = Arc::clone(read_before.state());
        let mut changes = runtime.subscribe();
        let result = runtime.own(scope_of(&state), |owner| {
            if swallowed_by_normalizer {
                let swallowed = owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::SwallowsMetadataError);
                assert!(swallowed.is_err(), "a swallowed metadata error is not accepted");
            } else {
                owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available)?;
                owner.stage_command(&agent(), &envelope(&state, "owner-ask", ask("owner-card")), Attached::None)?;
                let refused = owner.stage_command(&agent(), &envelope(&state, "agent-answer", answer()), Attached::None);
                assert!(refused.is_err(), "only the card's recipient answers it");
            }
            let later = owner.stage_command(&agent(), &envelope(&state, "owner-later", ask("later-card")), Attached::None);
            assert!(later.is_err(), "every later step refuses once one failed");
            Ok(())
        });
        poisoned(result);
        assert_eq!(counts(&observer), before, "nothing of the poisoned transaction is committed");
        assert!(Arc::ptr_eq(runtime.read().state(), read_before.state()), "nothing is published");
        assert!(!changes.has_changed().unwrap(), "nothing is notified");
        assert!(runtime.take_acknowledgements().is_empty(), "nothing is acknowledged");
    }
}

// Case 2, positive controls: the first observation has no checkpoint yet (Ok(None)) and commits;
// an exact command replay returns the stored outcome and still commits.
#[test]
fn a_first_observation_and_an_exact_command_replay_still_commit() {
    let (fixture, store, execution) = running();
    let runtime = runtime(&fixture, store);
    let observer = observer(&fixture);
    let state = Arc::clone(runtime.read().state());
    let command = envelope(&state, "owner-ask", ask("owner-card"));
    let (_, first) = runtime
        .own(scope_of(&state), |owner| {
            owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available)?;
            owner.stage_command(&agent(), &command, Attached::None).map(|_| ())
        })
        .unwrap();
    let committed = counts(&observer);
    let (_, replay) = runtime
        .own(scope_of(&state), |owner| owner.stage_command(&agent(), &command, Attached::None).map(|_| ()))
        .unwrap();
    assert!(!first[0].replayed && replay[0].replayed);
    assert_eq!(replay[0].outcome, first[0].outcome);
    assert_eq!(counts(&observer), committed, "a replay writes no second card, receipt or message");
    assert_eq!(runtime.take_acknowledgements().len(), 2);
}

// Case 3: a commit that fails at COMMIT leaves no seal, no publication and an intact cache.
#[test]
fn a_deferred_commit_failure_issues_no_seal_publication_or_cache() {
    let (fixture, store, execution) = running();
    let runtime = runtime(&fixture, store);
    let observer = observer(&fixture);
    observer.execute_batch("CREATE TABLE composite_parent(id INTEGER PRIMARY KEY); CREATE TABLE composite_child(parent INTEGER REFERENCES composite_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER fail_composite_commit AFTER INSERT ON interactions WHEN NEW.id='owner-card' BEGIN INSERT INTO composite_child VALUES(404); END").unwrap();
    let before = counts(&observer);
    let read_before = runtime.read();
    let state = Arc::clone(read_before.state());
    let error = runtime
        .own(scope_of(&state), |owner| {
            owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available)?;
            owner.stage_command(&agent(), &envelope(&state, "owner-ask", ask("owner-card")), Attached::None).map(|_| ())
        })
        .unwrap_err();
    match error {
        StoreError::Storage(error) => assert!(
            matches!(error.downcast_ref::<alondra_store::Error>(),
                Some(alondra_store::Error::Database(rusqlite::Error::SqliteFailure(code, _)))
                    if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
            "{error:?}"
        ),
        other => panic!("expected the deferred foreign key failure, got {other:?}"),
    }
    assert_eq!(counts(&observer), before);
    assert!(Arc::ptr_eq(runtime.read().state(), read_before.state()), "nothing is published");
    assert!(runtime.take_acknowledgements().is_empty(), "no seal leaves a failed commit");
    let store = runtime.store.lock().unwrap();
    assert!(Arc::ptr_eq(&store.cached.as_ref().unwrap().state, read_before.state()), "the cache keeps the committed state");
    assert!(store.conn.is_autocommit());
}

// Case 4: retirement, its raw observation, card settlement and checkpoint in one transaction.
#[test]
fn a_released_execution_retires_with_its_observation_card_and_checkpoint_and_a_replay_adds_nothing() {
    let (fixture, store, execution) = released();
    let runtime = runtime(&fixture, store);
    let observer = observer(&fixture);
    let state = Arc::clone(runtime.read().state());
    let withdraw = envelope(&state, "composite-withdraw", Command::Withdraw { interaction_id: "composite-card".into() });
    let retire = |owner: &mut OwnerTransaction<'_>| {
        owner.retire_released(
            &execution,
            1,
            1,
            b"retirement observation".to_vec(),
            b"normalized retirement".to_vec(),
            "composite-card".into(),
            payload("continuation"),
        )?;
        owner.stage_command(&agent(), &withdraw, Attached::None).map(|_| ())
    };
    runtime.own(scope_of(&state), retire).unwrap();
    let acknowledged = runtime.take_acknowledgements();
    assert_eq!(acknowledged.len(), 1);
    assert_eq!(acknowledged[0].observations[0].cursor(), 1);
    assert!(acknowledged[0].observations[0].matches_evidence(b"retirement observation"));
    assert_eq!(card(&observer, "composite-card"), Some((None, true)));
    let checkpoint: (i64, Vec<u8>) = observer
        .query_row("SELECT cursor,bytes FROM provider_normalization_checkpoints", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(checkpoint, (1, b"normalized retirement".to_vec()));
    let retired = counts(&observer);

    let late = runtime.own(scope_of(&state), |owner| {
        owner.stage_observation(&binding(), 1, &batch(&execution, 2), &Normalizer::Available)
    });
    assert!(late.is_err(), "a retired execution accepts no new observation");
    let replay = runtime.own(scope_of(&state), retire);
    assert!(replay.is_err(), "the retired observation is not admitted again as current work");
    let (_, withdrawn_again) = runtime
        .own(scope_of(&state), |owner| owner.stage_command(&agent(), &withdraw, Attached::None).map(|_| ()))
        .unwrap();
    assert!(withdrawn_again[0].replayed, "the card settlement replays its stored outcome");
    assert_eq!(counts(&observer), retired, "neither replay adds an effect, card, input or row");

    drop(runtime);
    let mut reopened = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(card(&reopened.conn, "composite-card"), Some((None, true)));
    assert_eq!(journal_after(&mut reopened), (vec!["continuation".to_owned()], false, 0));
}

// Case 5: the commit precedes publication; the acknowledgement waits for publication while the
// writer lock keeps every other writer out.
#[test]
fn publication_follows_the_commit_and_the_acknowledgement_follows_publication() {
    let (fixture, store, execution) = running();
    let runtime = Arc::new(runtime(&fixture, store));
    let observer = observer(&fixture);
    let before = counts(&observer);
    let read_before = runtime.read();
    let state = Arc::clone(read_before.state());
    let revocation = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .open(fixture.0.join("domain-approval.lock"))
        .unwrap();
    let (at_barrier, reached) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let barrier = at_barrier.clone();
    runtime.before_owner_publish(move || {
        barrier.send(None).unwrap();
        released.recv().unwrap();
    });
    let owning = {
        let runtime = Arc::clone(&runtime);
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            let result = runtime.own(scope_of(&state), |owner| {
                owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available)?;
                owner.stage_command(&agent(), &envelope(&state, "owner-ask", ask("owner-card")), Attached::None).map(|_| ())
            });
            at_barrier.send(Some(format!("{result:?}"))).unwrap();
            result
        })
    };
    if let Some(finished) = reached.recv().unwrap() {
        panic!("the owner transaction never reached publication: {finished}");
    }
    assert_ne!(counts(&observer), before, "the SQL commit happened before publication");
    assert!(Arc::ptr_eq(runtime.read().state(), read_before.state()), "readers still see the state before the commit");
    assert!(runtime.take_acknowledgements().is_empty(), "no acknowledgement while readers see the old state");
    // SAFETY: the descriptor stays owned by `revocation` for the call.
    let locked = unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&revocation), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(
        (locked, std::io::Error::last_os_error().raw_os_error()),
        (-1, Some(libc::EWOULDBLOCK)),
        "a revocation waits until the owner transaction has published"
    );
    let (waiting, blocked) = mpsc::channel();
    runtime.store.on_wait(move || {
        let _ = waiting.send(());
    });
    let competing = {
        let runtime = Arc::clone(&runtime);
        let command = envelope(&state, "competing", Command::RegisterAgent { agent_id: "agent-2".into(), name: "Second".into(), at: 0 });
        std::thread::spawn(move || runtime.execute_on_current(&command))
    };
    blocked.recv().unwrap();
    release.send(()).unwrap();
    let (_, outcomes) = owning.join().unwrap().unwrap();
    let acknowledged = runtime.take_acknowledgements();
    assert_eq!(acknowledged.len(), 1);
    assert!(acknowledged[0].observations[0].matches_evidence(b"raw observation 1"));
    let competing = competing.join().unwrap().unwrap();
    assert!(competing.revision > outcomes[0].outcome.revision, "the competing writer committed after the owner published");
    assert_eq!(runtime.read().state().revision, competing.revision);
}

// Case 5: a crash between the commit and publication acknowledges nothing; after a restart the
// exact replay publishes the committed outcome once and acknowledges it.
#[test]
fn a_crash_before_publication_acknowledges_nothing_and_the_replay_acknowledges_once() {
    let (fixture, store, execution) = running();
    let runtime = runtime(&fixture, store);
    let observer = observer(&fixture);
    let read_before = runtime.read();
    let state = Arc::clone(read_before.state());
    let command = envelope(&state, "owner-ask", ask("owner-card"));
    let work = |owner: &mut OwnerTransaction<'_>| {
        let acceptance = owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available)?;
        owner.stage_command(&agent(), &command, Attached::None)?;
        Ok(acceptance)
    };
    runtime.before_owner_publish(|| panic!("publication failed after the commit"));
    let crashed = catch_unwind(AssertUnwindSafe(|| runtime.own(scope_of(&state), work)));
    assert!(crashed.is_err());
    assert!(Arc::ptr_eq(runtime.read().state(), read_before.state()), "nothing was published");
    assert!(runtime.take_acknowledgements().is_empty(), "nothing was acknowledged");
    let committed = counts(&observer);
    assert_eq!(card(&observer, "owner-card"), Some((None, false)), "the transaction itself committed");
    drop(runtime);

    let restarted = DomainRuntime::open_at(&fixture.0, &fixture.path()).unwrap();
    let published = Arc::clone(restarted.read().state());
    assert!(published.interactions.contains_key(&InteractionId::from("owner-card")));
    let (acceptance, outcomes) = restarted.own(scope_of(&published), work).unwrap();
    assert_eq!(acceptance, ObservationAcceptance::Duplicate);
    assert!(outcomes[0].replayed);
    assert_eq!(counts(&observer), committed, "the replay adds no second card, receipt or message");
    let acknowledged = restarted.take_acknowledgements();
    assert_eq!(acknowledged.len(), 1);
    assert!(acknowledged[0].observations[0].matches_evidence(b"raw observation 1"));
    assert_eq!(restarted.read().state().revision, published.revision);
}

// Accumulator 1: a genuine change followed by a no-op keeps the change and its notification.
#[test]
fn a_later_no_op_keeps_the_earlier_change_and_its_notification() {
    let (fixture, store) = installed();
    let runtime = runtime(&fixture, store);
    let observer = observer(&fixture);
    let before = counts(&observer);
    let state = Arc::clone(runtime.read().state());
    let mut changes = runtime.subscribe();
    let (_, outcomes) = runtime
        .own(scope_of(&state), |owner| {
            owner.stage_command(&orchestrator(), &envelope(&state, "rename", retitle("Renamed")), Attached::None)?;
            owner.stage_command(&orchestrator(), &envelope(&state, "rename-again", retitle("Renamed")), Attached::None)?;
            Ok(())
        })
        .unwrap();
    assert!(outcomes[0].outcome.changed && !outcomes[1].outcome.changed);
    assert_eq!(outcomes[1].outcome.revision, outcomes[0].outcome.revision, "the no-op advances no revision");
    let published = runtime.read();
    assert_eq!(published.state().tasks[&task()].title, "Renamed");
    assert_eq!(published.state().revision, outcomes[0].outcome.revision);
    assert!(changes.has_changed().unwrap(), "change notification lost: a later no-op cleared it");
    assert_eq!(*changes.borrow_and_update(), outcomes[0].outcome.revision);
    assert_eq!(counts(&observer)[5], before[5] + 2, "each command keeps its own receipt");
}

// Accumulator 2: a schedule change followed by a no-op and its own exact replay.
#[test]
fn a_schedule_change_keeps_its_notification_through_a_no_op_and_a_replay() {
    let (fixture, store) = installed();
    let runtime = runtime(&fixture, store);
    let observer = observer(&fixture);
    let before = counts(&observer);
    let state = Arc::clone(runtime.read().state());
    let mut schedules = runtime.subscribe_schedules();
    let create = envelope(&state, "schedule", schedule());
    let (_, outcomes) = runtime
        .own(scope_of(&state), |owner| {
            owner.stage_command(&Actor::System, &create, Attached::None)?;
            owner.stage_command(&orchestrator(), &envelope(&state, "same-title", retitle("Goal")), Attached::None)?;
            owner.stage_command(&Actor::System, &create, Attached::None)?;
            Ok(())
        })
        .unwrap();
    assert_eq!(outcomes.iter().map(|o| o.replayed).collect::<Vec<_>>(), [false, false, true]);
    assert!(outcomes[0].outcome.changed && !outcomes[1].outcome.changed);
    assert_eq!(outcomes[2].outcome, outcomes[0].outcome, "the replay returns the stored outcome");
    assert!(schedules.has_changed().unwrap(), "schedule notification lost: a later no-op or replay cleared it");
    assert_eq!(*schedules.borrow_and_update(), outcomes[0].outcome.revision);
    let after = counts(&observer);
    assert_eq!(after[5], before[5] + 2, "the replay writes no second receipt");
    assert_eq!(after[6], before[6] + i64::try_from(outcomes[0].outcome.effects.len()).unwrap(), "the replay queues no effect again");
    assert_eq!(runtime.read().state().schedule.schedules.len(), state.schedule.schedules.len() + 1);
}

// Accumulator 3: two changes and a replay of the first: the final state holds both, and each
// command keeps its own ordered outcome.
#[test]
fn two_changes_and_a_replay_keep_both_changes_and_each_outcome() {
    let (fixture, store) = installed();
    let runtime = runtime(&fixture, store);
    let observer = observer(&fixture);
    let before = counts(&observer);
    let state = Arc::clone(runtime.read().state());
    let first = envelope(&state, "first", retitle("First"));
    let (_, outcomes) = runtime
        .own(scope_of(&state), |owner| {
            owner.stage_command(&orchestrator(), &first, Attached::None)?;
            owner.stage_command(&agent(), &envelope(&state, "second", ask("second-card")), Attached::None)?;
            owner.stage_command(&orchestrator(), &first, Attached::None)?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        outcomes.iter().map(|o| (o.command_id.0.as_str(), o.replayed)).collect::<Vec<_>>(),
        [("first", false), ("second", false), ("first", true)]
    );
    assert!(outcomes[0].outcome.revision < outcomes[1].outcome.revision);
    assert_eq!(outcomes[2].outcome, outcomes[0].outcome);
    let published = runtime.read();
    assert_eq!(published.state().tasks[&task()].title, "First");
    assert!(published.state().interactions.contains_key(&InteractionId::from("second-card")));
    assert_eq!(published.state().revision, outcomes[1].outcome.revision, "the final state is the latest change");
    let after = counts(&observer);
    assert_eq!(after[5], before[5] + 2);
    assert_eq!(after[6], before[6] + i64::try_from(outcomes[1].outcome.effects.len()).unwrap(), "the replay dispatches nothing again");
}

// Accumulator 4: a later failure after two good commands rolls all of them back, caught or not.
#[test]
fn a_late_failure_after_good_commands_rolls_back_every_command() {
    for caught in [false, true] {
        let (fixture, store) = installed();
        let runtime = runtime(&fixture, store);
        let observer = observer(&fixture);
        let before = counts(&observer);
        let read_before = runtime.read();
        let state = Arc::clone(read_before.state());
        let mut changes = runtime.subscribe();
        let result = runtime.own(scope_of(&state), |owner| {
            owner.stage_command(&orchestrator(), &envelope(&state, "first", retitle("First")), Attached::None)?;
            owner.stage_command(&agent(), &envelope(&state, "second", ask("second-card")), Attached::None)?;
            let refused = owner.stage_command(&agent(), &envelope(&state, "agent-answer", answer()), Attached::None);
            if caught {
                assert!(refused.is_err());
                Ok(())
            } else {
                refused.map(|_| ())
            }
        });
        if caught {
            poisoned(result);
        } else {
            assert!(matches!(result, Err(StoreError::Domain(_))), "{result:?}");
        }
        assert_eq!(counts(&observer), before, "no receipt, message or card of the earlier commands remains");
        assert!(Arc::ptr_eq(runtime.read().state(), read_before.state()));
        assert!(!changes.has_changed().unwrap());
        assert!(runtime.take_acknowledgements().is_empty());
    }
}

// The guarded answer and the lifecycle reply, admission and owner commit on one connection.
#[test]
fn guarded_answer_and_lifecycle_reply_commit_on_the_same_connection() {
    let (fixture, mut store) = waiting();
    let before = counts(&store.conn);
    let state = store.load().unwrap();
    let committed = store
        .own(scope_of(&state), |owner| {
            {
                let journal = owner.journal();
                journal.put_input(&task(), payload("reply"))?;
                let revision = journal.load(&task())?.revision();
                journal.apply(
                    &task(),
                    revision,
                    "composite-lifecycle",
                    LifecycleEvent::Answer { card: "composite-card".into(), input: InputId::new("reply").unwrap() },
                )?;
            }
            owner.stage_command(&user(), &envelope(&state, "composite-answer", answer()), Attached::None)?;
            let journal = owner.journal();
            let revision = journal.load(&task())?.revision();
            admit(journal, revision, "admitted")?;
            launch(journal)?;
            journal.stage_input(&task(), &InputId::new("reply").unwrap())?;
            Ok(())
        })
        .map(|(_, commit)| commit.technical)
        .unwrap();
    assert_eq!(committed.permits.len(), 1);
    assert_eq!(committed.permits[0].input(), &InputId::new("initial").unwrap());
    assert_eq!(committed.owners.len(), 1);
    assert_eq!(committed.owners[0].generation(), 1);
    assert!(card(&store.conn, "composite-card").unwrap().0.is_some());
    assert_eq!(counts(&store.conn)[2], before[2] + 2);
    drop(store);
    let mut reopened = DomainStore::open(&fixture.path()).unwrap();
    assert!(card(&reopened.conn, "composite-card").unwrap().0.is_some());
    assert_eq!(journal_after(&mut reopened), (vec!["initial".to_owned(), "reply".to_owned()], true, 0));
}

// A document's content is verified and stored outside the SQL transaction, so the owner refuses it.
#[test]
fn a_document_command_is_refused_and_poisons_the_owner() {
    let (fixture, store) = installed();
    let runtime = runtime(&fixture, store);
    let observer = observer(&fixture);
    let before = counts(&observer);
    let state = Arc::clone(runtime.read().state());
    let document = Command::CreateDocument {
        task_id: task(),
        document_id: "document".into(),
        title: "Plan".into(),
        revision_id: "revision".into(),
        content_digest: "0".repeat(64),
        content_reference: "missing.md".into(),
        role: "plan".into(),
    };
    let result = runtime.own(scope_of(&state), |owner| {
        owner.stage_command(&orchestrator(), &envelope(&state, "rename", retitle("Renamed")), Attached::None)?;
        let refused = owner.stage_command(&agent(), &envelope(&state, "document", document), Attached::None);
        assert!(
            matches!(&refused, Err(StoreError::Domain(error)) if error.code == "invalid_input"),
            "a document command never stages through the owner: {refused:?}"
        );
        Ok(())
    });
    poisoned(result);
    assert_eq!(counts(&observer), before);
}

// The cache an owner commit leaves is exactly what a reload of the database reads.
#[test]
fn the_cache_after_an_owner_commit_is_what_a_reload_reads() {
    let (fixture, store, execution) = running();
    let runtime = runtime(&fixture, store);
    let state = Arc::clone(runtime.read().state());
    runtime
        .own(scope_of(&state), |owner| {
            owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available)?;
            owner.stage_command(&agent(), &envelope(&state, "owner-ask", ask("owner-card")), Attached::None)?;
            owner.stage_command(&orchestrator(), &envelope(&state, "same-title", retitle("Goal")), Attached::None)?;
            Ok(())
        })
        .unwrap();
    let store = runtime.store.lock().unwrap();
    let cached = store.cached.as_ref().unwrap();
    assert!(Arc::ptr_eq(&cached.state, runtime.read().state()), "the cache holds the published state");
    assert_eq!(cached.total_changes, store.conn.total_changes(), "the cache is current for this connection");
    assert_eq!(repository::load(&store.conn).unwrap(), *cached.state, "a reload reads the cached state");
}
