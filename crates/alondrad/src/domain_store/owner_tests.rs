//! One owner transaction on the real domain database: guarded domain commands, provider
//! observations and the lifecycle journal commit together, and the runtime publishes the
//! committed state before it acknowledges. Lifecycle fixtures are not native SDK observations.
use super::*;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::tests::{Fixture, seeded};
use alondra_agent_application::{Normalization, ObservationMetadata};
use alondra_foundation::{ExecutionStamp, InputId, TurnId, WorkId};
use alondra_store::InputPayload;
use alondra_provider_contract::{AdapterError, Capability, Observation, Placement};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::mpsc;

const TASK: &str = "task-1";

/// No fixture here asks for a decision, so no «allow always» is ever judged under a home.
fn no_home() -> &'static std::path::Path {
    std::path::Path::new("/nonexistent")
}
const CATALOG: &str = include_str!("../../../../fixtures/domain-v2/acceptance.json");

fn agent() -> Actor {
    Actor::Agent {
        agent_id: "agent-1".into(),
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
fn binding() -> ProviderBinding {
    binding_with(BTreeSet::from([Capability::OfferInput]))
}
fn binding_with(capabilities: BTreeSet<Capability>) -> ProviderBinding {
    ProviderBinding::new(
        "composite-binding".into(),
        "fixture-service".into(),
        "fixture-instance".into(),
        "fixture-config".into(),
        1,
        1,
        capabilities,
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
pub(in crate::domain_store) fn scope_of(state: &DomainState) -> AuthorityScope {
    AuthorityScope::new(state.data_epoch.clone()).unwrap()
}

enum Normalizer {
    Available,
    SwallowsMetadataError,
    Released,
    /// The resource is up, a turn takes `input` and ends, after starting work of its own if
    /// `work`.
    Turn { input: InputId, work: bool },
    Facts(Vec<Observation>),
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
        let facts = match self {
            Self::Released => vec![Observation::ResourceReleased],
            Self::Turn { input, work } => {
                let turn = TurnId::new("turn-1").unwrap();
                let mut facts = vec![Observation::Available, Observation::TurnStarted { input: input.clone(), turn: turn.clone() }];
                if *work {
                    facts.push(Observation::WorkStarted { turn: turn.clone(), work: WorkId::new("subagent").unwrap() });
                }
                facts.push(Observation::TurnEnded { turn, outcome: alondra_provider_contract::TurnOutcome::Completed });
                facts
            }
            Self::Facts(facts) => facts.clone(),
            _ => vec![Observation::Available],
        };
        Normalization::new(facts, b"normalized".to_vec())
    }
}

/// The SDK host takes the task: the domain admits an execution of its own.
fn admit(label: &str) -> Command {
    Command::AdmitExecution { provider: alondra_core::accounts::Provider::Claude,
        task_id: task(),
        agent_id: "agent-1".into(),
        conversation: format!("conversation-{label}"),
        instance: format!("resource-{label}"),
        host: "fixture-service".into(),
        model: None,
        account: None,
        leaves_limit: None,
    }
}
fn launch(journal: &mut LifecycleTransaction<'_>) -> alondra_store::Result<ExecutionStamp> {
    launch_with(journal, &binding())
}
fn launch_with(journal: &mut LifecycleTransaction<'_>, binding: &ProviderBinding) -> alondra_store::Result<ExecutionStamp> {
    journal.stage_launch(&task(), &Placement::RemoteWorkspace("fixture/workspace".into()), binding)?;
    let execution = journal.load(&task())?.execution().unwrap().clone();
    journal.reserve_owner(&execution, 0)?;
    Ok(execution)
}

/// The Task runs an execution the SDK host owns at generation 1, whose provider has observed
/// nothing yet: its `claude --bg` node was released and the domain admitted the SDK's.
pub(in crate::domain_store) fn running() -> (Fixture, DomainStore, ExecutionStamp) {
    running_with(&binding())
}
fn running_with(binding: &ProviderBinding) -> (Fixture, DomainStore, ExecutionStamp) {
    let (fixture, mut store) = installed();
    let state = store.load().unwrap();
    let release = Command::ReleaseTask { task_id: task(), reason: "The SDK host takes it".into() };
    store.execute_filling_revision(&Actor::System, &envelope(&state, "fixture-release", release)).unwrap();
    let state = store.load().unwrap();
    let (execution, _) = store
        .own(scope_of(&state), |owner| {
            owner.stage_command(&Actor::System, &envelope(&state, "fixture-admit", admit("running")), Attached::None)?;
            let journal = owner.journal();
            let pending = journal.load(&task())?.pending_inputs()[0].clone();
            journal.give_input_text(&task(), InputPayload::new(pending, "Authorized fixture input").unwrap())?;
            Ok(launch_with(journal, binding)?)
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
/// The daemon restarts: the runtime closes and opens again on the same database.
fn runtime_reopened(fixture: &Fixture, runtime: DomainRuntime) -> DomainRuntime {
    drop(runtime);
    DomainRuntime::open_at(&fixture.0, &fixture.path()).unwrap()
}

#[test]
fn a_late_plan_observation_after_approval_commits_without_reopening_the_card() {
    use alondra_provider_contract::DecisionKind;
    let binding = binding_with(BTreeSet::from([Capability::OfferInput, Capability::Decisions]));
    let (fixture, store, execution) = running_with(&binding);
    let runtime = runtime(&fixture, store);
    let state = runtime.state().unwrap();
    runtime.execute_fixture(&Actor::User { user_id: "fixture-user".into() }, envelope(&state, "answer-fixture-question", answer())).unwrap();
    let input = runtime.read().state().lifecycle_of(&task()).unwrap().pending_inputs()[0].clone();
    let request = Observation::Asked {
        request: "native-plan".into(), epoch: 1, kind: DecisionKind::Plan, tool_use: Some("plan-item".into()),
        payload: serde_json::to_vec(&serde_json::json!({"tool":"plan","input":{"plan":"- [ ] Implement the change\n- [ ] Verify the change"}})).unwrap(),
    };
    let observe = |cursor, facts| {
        let state = runtime.state().unwrap();
        runtime.own(scope_of(&state), |owner| {
            let acceptance = owner.stage_observation(&binding, 1, &batch(&execution, cursor), &Normalizer::Facts(facts), no_home(), false)?;
            assert!(owner.unrecorded().is_empty(), "reconciliation leaves no lifecycle failure");
            Ok(acceptance)
        }).unwrap()
    };
    observe(1, vec![Observation::Available, Observation::TurnStarted { input, turn: TurnId::new("plan-turn").unwrap() }, request.clone()]);
    let card = alondra_core::domain::decision_card_id(&task(), execution.generation(), 1);
    let state = runtime.state().unwrap();
    assert!(state.interactions.contains_key(&card), "the native plan is presented: {:?}", state.lifecycle_journal[&task()]);
    runtime.execute_fixture(&Actor::User { user_id: "fixture-user".into() }, envelope(&state, "approve-native-plan", Command::AnswerQuestion {
        interaction_id: card.clone(), option_id: Hold::APPROVE.into(), inputs: BTreeMap::new(), attachments: vec![],
    })).unwrap();
    let before = runtime.state().unwrap();
    runtime.take_acknowledgements();
    let (acceptance, _) = observe(2, vec![request.clone()]);
    assert_eq!(acceptance, ObservationAcceptance::Accepted);
    assert_eq!(runtime.take_acknowledgements().iter().map(|committed| committed.observations.len()).sum::<usize>(), 1, "the late observation is acknowledged after publication");
    let after = runtime.state().unwrap();
    assert_eq!(after.interactions, before.interactions);
    assert_eq!(after.checklists, before.checklists);
    assert_eq!(after.lifecycle_journal, before.lifecycle_journal);
    observe(3, vec![Observation::DecisionSettled { epoch: 1, answered: true, continued: true }]);
    let settled = runtime.state().unwrap();
    observe(4, vec![request.clone()]);
    assert_eq!(runtime.read().state().lifecycle_journal, settled.lifecycle_journal, "an ended decision is never asked again");
    observe(5, vec![Observation::DecisionSettled { epoch: 1, answered: true, continued: true }, Observation::DecisionSettled { epoch: 1, answered: false, continued: false }]);
    assert_eq!(runtime.read().state().lifecycle_journal, settled.lifecycle_journal, "duplicate or contradictory ends cannot change a settled decision");
    let mut changed_request = request;
    let Observation::Asked { payload, .. } = &mut changed_request else { unreachable!() };
    *payload = serde_json::to_vec(&serde_json::json!({"tool":"plan","input":{"plan":"- [ ] A different plan cannot reuse this identity"}})).unwrap();
    let before = counts(&observer(&fixture));
    let state = runtime.state().unwrap();
    assert!(runtime.own(scope_of(&state), |owner| owner.stage_observation(&binding, 1, &batch(&execution, 6), &Normalizer::Facts(vec![changed_request]), no_home(), false)).is_err());
    assert_eq!(counts(&observer(&fixture)), before, "a changed request rolls back evidence and checkpoint");
    assert_eq!(observer(&fixture).query_row("SELECT count(*) FROM lifecycle_decision_requests WHERE task_id=?1", [TASK], |row| row.get::<_, i64>(0)).unwrap(), 1);
}
#[test]
fn a_native_plan_repeated_at_a_new_epoch_reuses_its_exact_approved_decision() {
    use alondra_provider_contract::DecisionKind;
    let binding = binding_with(BTreeSet::from([Capability::OfferInput, Capability::Decisions]));
    let (fixture, store, execution) = running_with(&binding);
    let runtime = runtime(&fixture, store);
    let state = runtime.state().unwrap();
    runtime.execute_fixture(&Actor::User { user_id: "fixture-user".into() }, envelope(&state, "answer-fixture-question", answer())).unwrap();
    let input = runtime.read().state().lifecycle_of(&task()).unwrap().pending_inputs()[0].clone();
    let requested = |epoch| Observation::Asked {
        request: "same-native-plan".into(), epoch, kind: DecisionKind::Plan, tool_use: Some("same-native-item".into()),
        payload: serde_json::to_vec(&serde_json::json!({"tool":"plan","input":{"plan":"- [ ] Implement the change\n- [ ] Verify the change"}})).unwrap(),
    };
    let observe = |cursor, facts| {
        let state = runtime.state().unwrap();
        runtime.own(scope_of(&state), |owner| {
            let acceptance = owner.stage_observation(&binding, 1, &batch(&execution, cursor), &Normalizer::Facts(facts), no_home(), false)?;
            assert!(owner.unrecorded().is_empty());
            Ok(acceptance)
        }).unwrap()
    };
    observe(1, vec![Observation::Available, Observation::TurnStarted { input, turn: TurnId::new("plan-turn").unwrap() }, requested(1)]);
    let original = alondra_core::domain::decision_card_id(&task(), execution.generation(), 1);
    let state = runtime.state().unwrap();
    runtime.execute_fixture(&Actor::User { user_id: "fixture-user".into() }, envelope(&state, "approve-original-plan", Command::AnswerQuestion {
        interaction_id: original.clone(), option_id: Hold::APPROVE.into(), inputs: BTreeMap::new(), attachments: vec![],
    })).unwrap();
    let approved = runtime.state().unwrap();
    runtime.take_acknowledgements();
    observe(2, vec![requested(2)]);
    let replayed = runtime.state().unwrap();
    assert_eq!(replayed.interactions, approved.interactions, "a new transport epoch cannot present the same native plan again");
    assert_eq!(replayed.checklists, approved.checklists);
    assert!(!replayed.interactions.contains_key(&alondra_core::domain::decision_card_id(&task(), execution.generation(), 2)));
    assert!(replayed.lifecycle_journal[&task()].iter().any(|record| matches!(record,
        LifecycleRecord::DecidedByPolicy { epoch: 2, decision: DecisionRecord::Allow })));
    assert_eq!(runtime.take_acknowledgements().iter().map(|committed| committed.observations.len()).sum::<usize>(), 1);
    let database = observer(&fixture);
    let responses: Vec<String> = database.prepare("SELECT argument FROM lifecycle_controls WHERE task_id=?1 AND kind='respond' AND epoch IN(1,2) ORDER BY epoch").unwrap()
        .query_map([TASK], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0], responses[1], "the second callback receives the exact recorded decision");
    observe(3, vec![Observation::DecisionSettled { epoch: 1, answered: true, continued: true }, Observation::DecisionSettled { epoch: 2, answered: true, continued: true }]);
    observe(4, vec![requested(4)]);
    assert_eq!(runtime.read().state().interactions, approved.interactions, "settling the original callback does not erase its approved native identity");
    assert_eq!(runtime.read().state().checklists, approved.checklists);
    assert_eq!(database.query_row("SELECT argument FROM lifecycle_controls WHERE task_id=?1 AND epoch=4", [TASK], |row| row.get::<_, String>(0)).unwrap(), responses[0]);
    for damage in 0..3 {
        let mut changed = requested(5);
        let Observation::Asked { tool_use, kind, payload, .. } = &mut changed else { unreachable!() };
        match damage {
            0 => *payload = serde_json::to_vec(&serde_json::json!({"tool":"plan","input":{"plan":"- [ ] This changed plan requires a new identity"}})).unwrap(),
            1 => *tool_use = Some("different-native-item".into()),
            _ => *kind = DecisionKind::Permission,
        }
        runtime.take_acknowledgements();
        let before = counts(&database);
        let requests_before: i64 = database.query_row("SELECT count(*) FROM lifecycle_decision_requests WHERE task_id=?1", [TASK], |row| row.get(0)).unwrap();
        let state = runtime.state().unwrap();
        assert!(runtime.own(scope_of(&state), |owner| owner.stage_observation(&binding, 1, &batch(&execution, 5), &Normalizer::Facts(vec![changed]), no_home(), false)).is_err());
        assert_eq!(counts(&database), before, "conflicting native plan identity rolls back evidence, request and checkpoint");
        assert_eq!(database.query_row("SELECT count(*) FROM lifecycle_decision_requests WHERE task_id=?1", [TASK], |row| row.get::<_, i64>(0)).unwrap(), requests_before);
        assert!(runtime.take_acknowledgements().is_empty(), "rejected evidence is never acknowledged");
        assert_eq!(runtime.read().state().interactions, approved.interactions);
    }
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
            let acceptance = owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available, no_home(), false)?;
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
    assert_eq!(outcomes.len(), 2, "the observation's facts and the card: two domain commands");
    assert!(matches!(outcomes[0].command_id.0.as_str(), id if id.starts_with("observation:")));
    let last = outcomes.last().unwrap();
    assert_eq!(published.state().revision, last.outcome.revision);
    assert!(published.state().interactions.contains_key(&InteractionId::from("owner-card")));
    assert!(published.state().tasks[&task()].started, "the observed resource starts the task through its journal");
    assert!(changes.has_changed().unwrap(), "the change is notified");
    assert_eq!(*changes.borrow_and_update(), last.outcome.revision);
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
                let swallowed = owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::SwallowsMetadataError, no_home(), false);
                assert!(swallowed.is_err(), "a swallowed metadata error is not accepted");
            } else {
                owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available, no_home(), false)?;
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
            owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available, no_home(), false)?;
            owner.stage_command(&agent(), &command, Attached::None).map(|_| ())
        })
        .unwrap();
    let committed = counts(&observer);
    let (_, replay) = runtime
        .own(scope_of(&state), |owner| owner.stage_command(&agent(), &command, Attached::None).map(|_| ()))
        .unwrap();
    let first = first.last().unwrap();
    assert!(!first.replayed && replay[0].replayed);
    assert_eq!(replay[0].outcome, first.outcome);
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
            owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available, no_home(), false)?;
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

// Case 4: a released resource is not a fact the domain records yet, so nothing retires the
// execution behind its back: the observation is refused, and its raw evidence, checkpoint and
// journal stay as they were, with nothing acknowledged.
#[test]
fn a_released_resource_is_refused_and_nothing_of_its_observation_commits() {
    let (fixture, store, execution) = running();
    let runtime = runtime(&fixture, store);
    let observer = observer(&fixture);
    let before = counts(&observer);
    let state = Arc::clone(runtime.read().state());
    let refused = runtime.own(scope_of(&state), |owner| {
        owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Released, no_home(), false)
    });
    assert!(refused.is_err(), "a released resource is refused");
    assert_eq!(counts(&observer), before);
    assert!(runtime.take_acknowledgements().is_empty());
    assert_eq!(runtime.read().state().revision, state.revision);
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
                owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available, no_home(), false)?;
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
        std::thread::spawn(move || runtime.execute_as_system(command))
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
        let acceptance = owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available, no_home(), false)?;
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
            owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available, no_home(), false)?;
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

// An accepted observation whose facts the domain never recorded cannot commit: dropping its
// pending observation leaves nothing to acknowledge.
#[test]
fn an_observation_whose_facts_the_domain_did_not_record_cannot_commit() {
    let (fixture, store, execution) = running();
    let runtime = runtime(&fixture, store);
    let observer = observer(&fixture);
    let before = counts(&observer);
    let state = Arc::clone(runtime.read().state());
    let unrecorded = runtime.own(scope_of(&state), |owner| {
        let pending = alondra_agent_application::stage_observation(owner.journal(), &binding(), 1, &batch(&execution, 1), &Normalizer::Available)
            .map_err(|error| StoreError::Storage(anyhow::anyhow!("{error:?}")))?;
        assert_eq!(pending.facts().len(), 1, "the observation was normalized to one fact");
        drop(pending);
        Ok(())
    });
    match unrecorded {
        Err(StoreError::Storage(error)) => assert!(
            matches!(error.downcast_ref::<alondra_store::Error>(), Some(alondra_store::Error::UnsettledObservation)),
            "{error:?}"
        ),
        other => panic!("expected UnsettledObservation, got {other:?}"),
    }
    assert_eq!(counts(&observer), before);
    assert!(runtime.take_acknowledgements().is_empty());
}

fn pending_input(store: &mut DomainStore) -> InputId {
    let (inputs, _, _) = journal_after(store);
    InputId::new(inputs[0].clone()).unwrap()
}
/// The card the end of turn "turn-1" of the running execution would ask with; its `claude --bg`
/// node was released, so the SDK's execution is the second.
fn turn_ended_card() -> InteractionId {
    turn_ended_card_of("turn-1")
}
fn turn_ended_card_of(turn: &str) -> InteractionId {
    InteractionId::from(format!("{TURN_ENDED_PREFIX}{TASK}-2-{turn}").as_str())
}
/// The case the orchestrator gets when turn `turn` of the running execution ends as a stop.
fn stall_case_of(turn: &str) -> InteractionId {
    alondra_core::domain::stalled_card_id(&task(), 2, turn)
}
/// Ends the first turn of the running execution with nothing that explains its next step (the
/// fixture's question withdrawn first), which wakes its node, and returns the normalizer of the turn that wake opens, `turn-2`, ending the same way.
fn woken(store: &mut DomainStore, execution: &ExecutionStamp) -> Normalizer {
    let state = store.load().unwrap();
    let withdraw = Command::Withdraw { interaction_id: "composite-card".into() };
    store.execute_filling_revision(&agent(), &envelope(&state, "withdraw-composite", withdraw)).unwrap();
    let input = pending_input(store);
    let state = store.load().unwrap();
    store
        .own(scope_of(&state), |owner| owner.stage_observation(&binding(), 1, &batch(execution, 1), &Normalizer::Turn { input, work: false }, no_home(), false))
        .unwrap();
    let turn = TurnId::new("turn-2").unwrap();
    Normalizer::Facts(vec![
        Observation::TurnStarted { input: pending_input(store), turn: turn.clone() },
        Observation::TurnEnded { turn, outcome: alondra_provider_contract::TurnOutcome::Completed },
    ])
}

// 3d: a new task's turn ends with nothing that explains its next step, and so does the turn the
// wake opens. The raw observation, its facts, the case for the orchestrator, the journal and the
// checkpoint commit together; readers see the case before the acknowledgement; the same
// observation again adds nothing, and no card asks the user.
#[test]
fn a_turn_end_commits_its_one_card_with_the_raw_observation_and_is_acknowledged_after_publication() {
    let (fixture, mut store, execution) = running();
    let normalizer = woken(&mut store, &execution);
    let runtime = runtime(&fixture, store);
    let observer = observer(&fixture);
    let state = Arc::clone(runtime.read().state());
    let card_id = stall_case_of("turn-2");
    let (acceptance, outcomes) = runtime
        .own(scope_of(&state), |owner| owner.stage_observation(&binding(), 1, &batch(&execution, 2), &normalizer, no_home(), false))
        .unwrap();
    assert_eq!(acceptance, ObservationAcceptance::Accepted);
    assert_eq!(outcomes.len(), 2, "the technical facts and the end of the turn");
    let published = runtime.read();
    assert!(published.state().interactions.contains_key(&card_id), "readers see the case");
    assert_eq!(published.state().tasks[&task()].final_presentation, None, "a native result is not a delivery");
    assert_eq!(card(&observer, card_id.0.as_str()), Some((None, false)), "the case committed with the observation");
    let acknowledged = runtime.take_acknowledgements();
    assert_eq!(acknowledged.len(), 1);
    assert!(acknowledged[0].observations[0].matches_evidence(b"raw observation 2"));
    let committed = counts(&observer);

    let (again, replayed) = runtime
        .own(scope_of(&state), |owner| owner.stage_observation(&binding(), 1, &batch(&execution, 2), &normalizer, no_home(), false))
        .unwrap();
    assert_eq!(again, ObservationAcceptance::Duplicate);
    assert!(replayed.is_empty(), "a duplicate records nothing");
    assert_eq!(counts(&observer), committed, "no second card, fact or row");
    let count = |pattern: &str| observer.query_row("SELECT count(*) FROM interactions WHERE id LIKE ?1", [pattern], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!((count("stalled-%"), count("turn-ended-%")), (1, 0), "one case, and no card for the user");
}

/// A stopped node's case the orchestrator leaves unsettled comes back to it once, and after that
/// Alondra closes the task itself; the count of stops and the closing outlive a restart, and no
/// card ever asks the user.
#[test]
fn a_stop_left_unsettled_is_reminded_once_then_closed_and_both_outlive_a_restart() {
    use alondra_core::domain::{STALL_ATTEMPTS, StallClosing};
    use alondra_core::orchestrator_cards::STALL_NOTICE_PREFIX;
    let (fixture, mut store, execution) = running();
    let normalizer = woken(&mut store, &execution);
    let runtime = runtime(&fixture, store);
    let state = Arc::clone(runtime.read().state());
    runtime.own(scope_of(&state), |owner| owner.stage_observation(&binding(), 1, &batch(&execution, 2), &normalizer, no_home(), false)).unwrap();
    let case = stall_case_of("turn-2");
    crate::orchestrator_inbox::settle(&runtime);
    let state = runtime.state().unwrap();
    assert!(state.interactions.contains_key(&InteractionId::from(format!("{STALL_NOTICE_PREFIX}{case}").as_str())), "the orchestrator is reminded");
    assert!(state.open(&state.interactions[&case]) && !state.tasks[&task()].over(), "it gets one more go first");
    assert_eq!(state.tasks[&task()].stalls, 1);
    assert!(STALL_ATTEMPTS > state.tasks[&task()].stalls);
    let runtime = runtime_reopened(&fixture, runtime);
    let state = runtime.state().unwrap();
    assert_eq!(state.tasks[&task()].stalls, 1, "the count outlives the restart");
    assert!(state.open(&state.interactions[&case]), "and so does the case");
    crate::orchestrator_inbox::settle(&runtime);
    let runtime = runtime_reopened(&fixture, runtime);
    let state = runtime.state().unwrap();
    assert_eq!(state.tasks[&task()].stall_closing, Some(StallClosing::Unsettled), "Alondra closed it, and that outlives the restart");
    assert!(state.tasks[&task()].over());
    let observer = observer(&fixture);
    let asked: i64 = observer.query_row("SELECT count(*) FROM interactions WHERE task_id=?1 AND recipient_kind='user' AND (id LIKE 'turn-ended-%' OR id LIKE 'stalled-%' OR id LIKE 'problem-%')",
        [TASK], |r| r.get(0)).unwrap();
    assert_eq!(asked, 0, "no card ever asked the user");
}

/// The orchestrator's instruction settles the case for good: no reminder follows it.
#[test]
fn a_stop_the_orchestrator_settles_gets_no_reminder() {
    use alondra_core::orchestrator_cards::STALL_NOTICE_PREFIX;
    let (fixture, mut store, execution) = running();
    let normalizer = woken(&mut store, &execution);
    let runtime = runtime(&fixture, store);
    let state = Arc::clone(runtime.read().state());
    runtime.own(scope_of(&state), |owner| owner.stage_observation(&binding(), 1, &batch(&execution, 2), &normalizer, no_home(), false)).unwrap();
    let state = Arc::clone(runtime.read().state());
    let settle = Command::SettleStall { case_id: stall_case_of("turn-2"), task_id: task(), outcome: alondra_core::domain::StallOutcome::Instruct { text: "Present it".into() } };
    let fresh = CommandEnvelope { expected_revision: state.revision, ..envelope(&state, "orchestrator-instructs", settle) };
    runtime.execute(&Actor::Orchestrator { generation: state.orchestrator_generation }, &fresh, None, || Ok(())).unwrap();
    crate::orchestrator_inbox::settle(&runtime);
    assert!(!runtime.state().unwrap().interactions.keys().any(|id| id.0.starts_with(STALL_NOTICE_PREFIX)));
}

// 3d: a turn that ends behind work of its own asks nothing, and the work keeps its slot.
#[test]
fn a_turn_end_behind_work_of_its_own_commits_no_card_and_keeps_the_slot() {
    let (fixture, mut store, execution) = running();
    let input = pending_input(&mut store);
    let runtime = runtime(&fixture, store);
    let state = Arc::clone(runtime.read().state());
    runtime
        .own(scope_of(&state), |owner| owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Turn { input, work: true }, no_home(), false))
        .unwrap();
    assert!(!runtime.read().state().interactions.contains_key(&turn_ended_card()));
    drop(runtime);
    let mut reopened = DomainStore::open(&fixture.path()).unwrap();
    let scope = scope_of(&reopened.load().unwrap());
    let (holds, _) = reopened.own(scope, |owner| Ok(owner.journal().load(&task())?.holds_slot())).unwrap();
    assert!(holds, "the work still running keeps the slot");
}

// 3d: the card cannot be created after the raw observation was accepted: nothing of the
// observation, its facts, the journal or the checkpoint commits, and nothing is acknowledged.
#[test]
fn a_turn_end_whose_card_cannot_be_made_lets_nothing_escape() {
    let (fixture, mut store, execution) = running();
    let input = pending_input(&mut store);
    let state = store.load().unwrap();
    store.execute_filling_revision(&Actor::System, &envelope(&state, "taken-card-id", ask(turn_ended_card().0.as_str()))).unwrap();
    let runtime = runtime(&fixture, store);
    let observer = observer(&fixture);
    let before = counts(&observer);
    let state = Arc::clone(runtime.read().state());
    let failed = runtime.own(scope_of(&state), |owner| {
        owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Turn { input, work: false }, no_home(), false)
    });
    assert!(failed.is_err(), "the turn end is refused");
    assert_eq!(counts(&observer), before);
    assert!(runtime.take_acknowledgements().is_empty());
    assert_eq!(runtime.read().state().revision, state.revision);
}

// 3d and 4: the orchestrator settles the case: the lifecycle lets the case go before it is settled,
// in one commit with the dispatch of what the task is told, on the normal command path.
#[test]
fn settling_the_case_of_a_stopped_node_tells_the_task_how_to_go_on_in_one_commit() {
    let (fixture, mut store, execution) = running();
    let normalizer = woken(&mut store, &execution);
    let state = store.load().unwrap();
    store
        .own(scope_of(&state), |owner| owner.stage_observation(&binding(), 1, &batch(&execution, 2), &normalizer, no_home(), false))
        .unwrap();
    let state = store.load().unwrap();
    let instruct = Command::SettleStall {
        case_id: stall_case_of("turn-2"),
        task_id: task(),
        outcome: alondra_core::domain::StallOutcome::Instruct { text: "Add the tests first".into() },
    };
    let ordinary = store.execute_filling_revision(&orchestrator(), &envelope(&state, "instruct", instruct)).unwrap();
    let permits = &ordinary.sealed.as_ref().unwrap().permits;
    assert_eq!(permits.len(), 1, "the commit hands the owner the dispatch of what the task is told");
    let prepared: String = store.conn
        .query_row("SELECT d.status FROM lifecycle_dispatches d JOIN lifecycle_inputs i USING(task_id,input_id) WHERE d.task_id=?1 AND d.kind='input' AND i.body = 'Add the tests first'", [TASK], |r| r.get(0))
        .unwrap();
    assert_eq!(prepared, "prepared");
    assert!(card(&store.conn, stall_case_of("turn-2").0.as_str()).unwrap().0.is_some(), "the case is settled");
    let held: i64 = store.conn.query_row("SELECT count(*) FROM lifecycle_cards", [], |r| r.get(0)).unwrap();
    assert_eq!(held, 0, "the lifecycle no longer requires it");
    drop(store);
    let mut reopened = DomainStore::open(&fixture.path()).unwrap();
    let (pending, _, cards) = journal_after(&mut reopened);
    assert_eq!((pending.len(), cards), (1, 0), "the task waits to be told, with no card left");
}

/// Opt-in, for trying the card in the real TUI: `ALONDRA_TURN_END_DB` names the domain database
/// of a stopped sandbox daemon. A test task gets an SDK execution whose turn ended with nothing
/// that explains its next step, through the real owner path.
#[test]
#[ignore = "opt-in: needs ALONDRA_TURN_END_DB, the database of a stopped sandbox daemon"]
fn a_sandbox_task_whose_sdk_turn_ended_without_a_next_step() {
    let path = PathBuf::from(std::env::var("ALONDRA_TURN_END_DB").expect("ALONDRA_TURN_END_DB names a sandbox database"));
    assert!(!path.components().any(|part| part.as_os_str() == ".alondra"), "never the user's own database");
    let mut store = DomainStore::open(&path).unwrap();
    let state = store.load().unwrap();
    let id = |name: &str| format!("{name}-{}", state.revision);
    let task: TaskId = id("sdk-turn").as_str().into();
    let request: RequestId = id("sdk-turn-request").as_str().into();
    let node: AgentId = id("sdk-node").as_str().into();
    let run = |store: &mut DomainStore, actor: &Actor, name: &str, command: Command| {
        let state = store.load().unwrap();
        store.execute_filling_revision(actor, &envelope(&state, &id(name), command)).unwrap();
    };
    run(&mut store, &Actor::System, "register", Command::RegisterAgent { agent_id: node.clone(), name: "SDK node".into(), at: 0 });
    run(&mut store, &orchestrator(), "request", Command::CreateRequest { request_id: request.clone(), body: "Try the turn-end card".into(), auto_merge: false, maintenance: false, message: false });
    run(&mut store, &orchestrator(), "title", Command::TitleRequest { request_id: request.clone(), title: "Turn-end card".into() });
    run(&mut store, &orchestrator(), "task", Command::CreateTask {
        plan_items: None, continues: None, task_id: task.clone(), request_id: request, title: "Write the release notes".into(), agent_id: None,
        review_prs: false, dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    let state = store.load().unwrap();
    let admit = Command::AdmitExecution { provider: alondra_core::accounts::Provider::Claude, task_id: task.clone(), agent_id: node, conversation: id("conversation"), instance: id("instance"), host: "fixture-service".into(), model: None, account: None, leaves_limit: None };
    let (execution, _) = store
        .own(scope_of(&state), |owner| {
            owner.stage_command(&Actor::System, &envelope(&state, &id("admit"), admit), Attached::None)?;
            let journal = owner.journal();
            let pending = journal.load(&task)?.pending_inputs()[0].clone();
            journal.give_input_text(&task, InputPayload::new(pending, "Write the release notes").unwrap())?;
            journal.stage_launch(&task, &Placement::RemoteWorkspace("sandbox/workspace".into()), &binding())?;
            let execution = journal.load(&task)?.execution().unwrap().clone();
            journal.reserve_owner(&execution, 0)?;
            Ok(execution)
        })
        .unwrap();
    let state = store.load().unwrap();
    let input = InputId::new(state.lifecycle_journal[&task].iter().find_map(|record| match record {
        LifecycleRecord::Planned { input } => Some(input.clone()),
        _ => None,
    }).unwrap()).unwrap();
    let observation = OrderedObservation { owner_generation: 1, execution, cursor: 1, evidence: b"sandbox observation".to_vec() };
    store.own(scope_of(&state), |owner| owner.stage_observation(&binding(), 1, &observation, &Normalizer::Turn { input, work: false }, no_home(), false)).unwrap();
}

fn mcp_report(status: &str) -> Observation {
    Observation::McpServers { servers: vec![alondra_provider_contract::McpServerReport {
        name: "linear".into(), status: status.into(), error: None, source: Some("user".into()),
        transport: Some("http".into()), url: Some("https://mcp.linear.app/mcp".into()), traits: alondra_foundation::McpTraits { scope: alondra_foundation::McpScope::User, origin: alondra_foundation::McpOrigin::UserSettings, connect: alondra_foundation::McpConnect::SignIn, reconnect: true, sign_out: true, remove: true },
    }] }
}
/// Every byte under `root`, for whether something was ever written there.
fn written_anywhere(root: &std::path::Path, needle: &[u8]) -> Vec<PathBuf> {
    let mut found = vec![];
    for entry in std::fs::read_dir(root).unwrap().map(Result::unwrap) {
        let path = entry.path();
        if path.is_dir() {
            found.extend(written_anywhere(&path, needle));
        } else if std::fs::read(&path).unwrap().windows(needle.len()).any(|window| window == needle) {
            found.push(path);
        }
    }
    found
}

// How a node's MCP servers are and the sign-in its Claude opened are facts of its observations; a
// report init repeats changes nothing, and a failed MCP call reads the status only where the
// binding can. The browser's return reaches Claude's listener once, and its code is kept nowhere.
#[test]
fn a_node_s_mcp_servers_and_sign_in_are_recorded_and_its_return_reaches_only_claude_s_listener() {
    use crate::mcp_sign_in::{SignInError, complete};
    use alondra_provider_contract::McpSignInReport;
    use std::io::{BufRead, BufReader, Write};
    const CODE: &str = "one-time-code-7f3a9c";
    let (fixture, store, execution) = running();
    let runtime = runtime(&fixture, store);
    let observe = |cursor: u64, facts: Vec<Observation>| {
        let state = Arc::clone(runtime.read().state());
        runtime.own(scope_of(&state), |owner| owner.stage_observation(&binding(), 1, &batch(&execution, cursor), &Normalizer::Facts(facts), no_home(), false)).unwrap();
    };
    let agent = AgentId::from("agent-1");
    observe(1, vec![Observation::Available, mcp_report("needs-auth")]);
    assert_eq!(runtime.read().state().mcp[&agent].servers[0].status, McpStatus::NeedsAuth);
    let revision = runtime.read().state().revision;
    observe(2, vec![mcp_report("needs-auth")]);
    assert_eq!(runtime.read().state().revision, revision, "init repeats the servers as each prompt starts: nothing changes");
    observe(3, vec![Observation::McpToolFailed]);
    let controls: i64 = Connection::open(fixture.path()).unwrap().query_row("SELECT count(*) FROM lifecycle_controls", [], |row| row.get(0)).unwrap();
    assert_eq!(controls, 0, "a binding without MCP status reads none, and the observation still commits");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    observe(4, vec![Observation::McpSignIn { server: "linear".into(), sign_in: McpSignInReport::Browser {
        url: "https://linear.app/oauth/authorize?state=s1".into(), callback_port: Some(port), state: Some("s1".into()),
    } }]);
    let back = |state: &str| format!("http://localhost:{port}/callback?code={CODE}&state={state}");
    assert!(matches!(complete(&runtime, &task(), "linear", &back("another")), Err(SignInError::Refused(_))), "another sign-in's return");
    assert!(matches!(complete(&runtime, &task(), "linear", &format!("http://localhost:{}/callback?code={CODE}&state=s1", port + 1)), Err(SignInError::Refused(_))), "another listener");
    assert_eq!(complete(&runtime, &task(), "linear", "https://evil.example.com/callback?code=c&state=s1"), Err(SignInError::NotAReturn));
    let (heard, received) = mpsc::channel();
    let claude = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").unwrap();
        heard.send(line.trim_end().to_owned()).unwrap();
    });
    complete(&runtime, &task(), "linear", &back("s1")).unwrap();
    claude.join().unwrap();
    assert_eq!(received.recv().unwrap(), format!("GET /callback?code={CODE}&state=s1 HTTP/1.1"));
    assert!(runtime.read().state().mcp[&agent].sign_ins.is_empty(), "the sign-in is no longer open");
    assert!(matches!(complete(&runtime, &task(), "linear", &back("s1")), Err(SignInError::Refused(_))), "a return is taken once");
    drop(runtime);
    assert_eq!(written_anywhere(&fixture.0, CODE.as_bytes()), Vec::<PathBuf>::new(), "the one-time code is written nowhere");
}

#[test]
fn a_failed_mcp_call_reads_the_status_where_the_binding_can() {
    let mcp = binding_with(BTreeSet::from([Capability::OfferInput, Capability::McpStatus]));
    let (fixture, store, execution) = running_with(&mcp);
    let runtime = runtime(&fixture, store);
    let observe = |cursor: u64, facts: Vec<Observation>| {
        let state = Arc::clone(runtime.read().state());
        runtime.own(scope_of(&state), |owner| owner.stage_observation(&mcp, 1, &batch(&execution, cursor), &Normalizer::Facts(facts), no_home(), false)).unwrap();
    };
    let reads = || {
        runtime.read().state().lifecycle_journal[&task()].iter().filter(|record| matches!(record,
            LifecycleRecord::ControlRequested { operation, .. } if format!("{operation:?}").starts_with("ReadMcpStatus"))).count()
    };
    observe(1, vec![Observation::Available, mcp_report("needs-auth")]);
    assert_eq!(reads(), 0);
    observe(2, vec![Observation::McpToolFailed]);
    assert_eq!(reads(), 1, "the failed call asks the host for the servers' status");
}

#[test]
fn the_retention_gives_the_disk_back_what_normalized_observations_held() {
    let (fixture, store, execution) = running();
    let runtime = runtime(&fixture, store);
    for cursor in 1..=60 {
        let state = Arc::clone(runtime.read().state());
        let mut observed = batch(&execution, cursor);
        observed.evidence.resize(32 * 1024, b'x');
        runtime.own(scope_of(&state), |owner| owner.stage_observation(&binding(), 1, &observed, &Normalizer::Facts(vec![]), no_home(), false)).unwrap();
    }
    let held: i64 = observer(&fixture).query_row("SELECT sum(length(bytes)) FROM provider_observations", [], |row| row.get(0)).unwrap();
    assert_eq!(held, 32 * 1024 + 59, "only the checkpoint's observation keeps its bytes");
    let pages = |name: &str| observer(&fixture).pragma_query_value(None, name, |row| row.get::<_, i64>(0)).unwrap();
    let before = pages("page_count");
    assert!(pages("freelist_count") > 0);
    assert_eq!(runtime.retain(alondra_core::now_millis()).unwrap().evidence, alondra_store::EvidenceRetention::default());
    assert_eq!(pages("auto_vacuum"), 2, "the store vacuums incrementally from now on");
    assert_eq!(pages("freelist_count"), 0);
    assert!(pages("page_count") < before, "{} pages, {before} before", pages("page_count"));
    runtime.retain(alondra_core::now_millis()).unwrap();
    assert_eq!(pages("auto_vacuum"), 2);
}

// A task that waits for an MCP server goes on once its own node reports it connected, after the
// user signed in through the card's routes away from the Mac: Connect opens one sign-in and
// hands every device the same page while Claude still waits for it, and where the browser came
// back to reaches Claude's listener in memory. Neither that address nor its one-time code is
// kept anywhere under Alondra's HOME.
#[test]
fn a_task_waiting_for_an_mcp_server_goes_on_after_the_sign_in_and_its_return_is_kept_nowhere() {
    use crate::mcp_waits::{Opened, connect, give_back, reconnect};
    use alondra_provider_contract::McpSignInReport;
    use std::io::{BufRead, BufReader, Write};
    const CODE: &str = "one-time-code-5be1d4";
    let mcp = binding_with(BTreeSet::from([Capability::OfferInput, Capability::McpStatus, Capability::McpReconnect, Capability::McpSignIn]));
    let (fixture, store, execution) = running_with(&mcp);
    let runtime = Arc::new(runtime(&fixture, store));
    let cursor = std::sync::atomic::AtomicU64::new(0);
    let observe = |facts: Vec<Observation>| {
        let state = Arc::clone(runtime.read().state());
        let at = cursor.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        runtime.own(scope_of(&state), |owner| owner.stage_observation(&mcp, 1, &batch(&execution, at), &Normalizer::Facts(facts), no_home(), false)).unwrap();
    };
    let controls = |kind: &str| -> usize {
        runtime.read().state().lifecycle_journal[&task()].iter().filter(|record| matches!(record,
            LifecycleRecord::ControlRequested { operation, .. } if format!("{operation:?}").starts_with(kind))).count()
    };
    observe(vec![Observation::Available, mcp_report("needs-auth")]);
    let state = runtime.read().state().clone();
    let wait = CommandEnvelope { expected_revision: state.revision, ..envelope(&state, "wait-linear", Command::WaitForMcp { task_id: task(), server: "linear".into() }) };
    runtime.execute(&agent(), &wait, None, || Ok(())).unwrap();
    let card = crate::mcp_waits::Target::Card(runtime.read().state().mcp_waiting()[0].card.clone());
    let tokio = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let opening = tokio.spawn({
        let (runtime, card) = (runtime.clone(), card.clone());
        async move { connect(&runtime, &card).await }
    });
    while controls("StartMcpSignIn") == 0 {
        assert!(!opening.is_finished(), "Connect asked for no sign-in: {:?}", tokio.block_on(opening));
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    observe(vec![Observation::McpSignIn { server: "linear".into(), sign_in: McpSignInReport::Browser {
        url: "https://linear.app/oauth/authorize?state=s1".into(), callback_port: Some(port), state: Some("s1".into()),
    } }]);
    let page = Opened::Browser { server: "linear".into(), url: "https://linear.app/oauth/authorize?state=s1".into(), port: Some(port) };
    assert_eq!(tokio.block_on(opening).unwrap(), Ok(page.clone()));
    assert_eq!(tokio.block_on(connect(&runtime, &card)), Ok(page), "another device gets the same page while Claude waits for it");
    assert_eq!(controls("StartMcpSignIn"), 1, "one sign-in for every device");

    let address = format!("http://localhost:{port}/callback?code={CODE}&state=s1");
    let refused = tokio.block_on(give_back(&runtime, &card, "https://evil.example.com/callback?code=c&state=s1".into()));
    assert_eq!(refused.unwrap_err().code, "invalid_input");
    let claude = std::thread::spawn(move || loop {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
        if line.is_empty() {
            continue;
        }
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").unwrap();
        return line.trim_end().to_owned();
    });
    let watching = tokio.spawn(crate::mcp_waits::run(runtime.clone()));
    tokio.block_on(give_back(&runtime, &card, address.clone())).unwrap();
    assert_eq!(claude.join().unwrap(), format!("GET /callback?code={CODE}&state=s1 HTTP/1.1"));
    let mut changes = runtime.subscribe();
    let asked = tokio.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while controls("ReconnectMcp") == 0 {
                changes.changed().await.unwrap();
            }
        }).await
    });
    assert!(asked.is_ok(), "Claude connects the server only after it answered the browser: the node must connect it again");
    watching.abort();
    assert_eq!(runtime.read().state().mcp_waiting().len(), 1, "a return is no fact that the server connected");
    observe(vec![mcp_report("connected")]);
    assert!(runtime.read().state().mcp_waiting().is_empty(), "its own node reported it connected");
    assert!(matches!(runtime.read().state().lifecycle_journal[&task()].last(),
        Some(LifecycleRecord::Resumed { text: Some(text), .. }) if text.starts_with("linear is connected")));
    assert_eq!(reconnect(&runtime, &card).unwrap_err().code, "conflict", "nothing waits any more");
    drop(tokio);
    drop(runtime);
    for kept in [CODE, address.as_str(), "callback?code"] {
        assert_eq!(written_anywhere(&fixture.0, kept.as_bytes()), Vec::<PathBuf>::new(), "{kept} is written nowhere");
    }
}

// The MCP servers screen acts on a server no task waits for: Connect opens its sign-in on the
// node that uses it, and Sign out has Claude forget its sign-in to it. A server only Alondra's
// screen names, by an opaque target, is the one the node reported.
#[test]
fn the_servers_screen_signs_in_to_and_out_of_a_server_no_task_waits_for() {
    use crate::mcp_waits::{Opened, Target, connect, reconnect, sign_out};
    use alondra_core::domain::McpServerKey;
    use alondra_provider_contract::McpSignInReport;
    let mcp = binding_with(BTreeSet::from([Capability::OfferInput, Capability::McpStatus, Capability::McpReconnect, Capability::McpSignIn,
        Capability::McpClearAuth]));
    let (fixture, store, execution) = running_with(&mcp);
    let runtime = Arc::new(runtime(&fixture, store));
    let cursor = std::sync::atomic::AtomicU64::new(0);
    let observe = |facts: Vec<Observation>| {
        let state = Arc::clone(runtime.read().state());
        let at = cursor.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        runtime.own(scope_of(&state), |owner| owner.stage_observation(&mcp, 1, &batch(&execution, at), &Normalizer::Facts(facts), no_home(), false)).unwrap();
    };
    let controls = |kind: &str| -> usize {
        runtime.read().state().lifecycle_journal[&task()].iter().filter(|record| matches!(record,
            LifecycleRecord::ControlRequested { operation, .. } if format!("{operation:?}").starts_with(kind))).count()
    };
    let tokio = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let unknown = Target::of("mcp-server-0000000000000000").unwrap();
    assert_eq!(tokio.block_on(connect(&runtime, &unknown)).unwrap_err().code, "not_found", "a server no node reported");
    observe(vec![Observation::Available, mcp_report("needs-auth")]);
    assert!(runtime.read().state().mcp_waiting().is_empty(), "no task waits for it");
    let server = Target::of(&runtime.read().state().mcp_nodes()[0].servers.iter().map(McpServerKey::of).next().unwrap().target()).unwrap();
    assert_eq!(sign_out(&runtime, &server).unwrap_err().code, "conflict", "no sign-in to forget while it needs one");

    let opening = tokio.spawn({
        let (runtime, server) = (runtime.clone(), server.clone());
        async move { connect(&runtime, &server).await }
    });
    while controls("StartMcpSignIn") == 0 {
        assert!(!opening.is_finished(), "Connect asked for no sign-in: {:?}", tokio.block_on(opening));
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    observe(vec![Observation::McpSignIn { server: "linear".into(), sign_in: McpSignInReport::Browser {
        url: "https://linear.app/oauth/authorize?state=s1".into(), callback_port: None, state: None,
    } }]);
    assert_eq!(tokio.block_on(opening).unwrap(), Ok(Opened::Browser { server: "linear".into(), url: "https://linear.app/oauth/authorize?state=s1".into(), port: None }));
    observe(vec![mcp_report("connected")]);
    assert_eq!(tokio.block_on(connect(&runtime, &server)), Ok(Opened::Connected { server: "linear".into() }), "nothing to open once it is connected");
    assert_eq!(reconnect(&runtime, &server), Ok(1));

    assert_eq!(sign_out(&runtime, &server), Ok("linear".into()));
    assert!(matches!(runtime.read().state().lifecycle_journal[&task()].last(),
        Some(LifecycleRecord::ControlRequested { operation: alondra_core::domain::ControlRecord::ClearMcpSignIn { server }, .. }) if server == "linear"));
}

// 21 Oct, in the sandbox: a node said it waits for Linear and was told its Claude had no such
// server. A write outside the domain had made the store load the state again from its rows, which
// never hold what the nodes reported of their MCP servers. A reload keeps it.
#[test]
fn what_a_node_reported_of_its_mcp_servers_outlives_a_reload_of_the_state_from_its_rows() {
    let (_fixture, mut store, execution) = running();
    let state = store.load().unwrap();
    store.own(scope_of(&state), |owner| owner.stage_observation(&binding(), 1, &batch(&execution, 1),
        &Normalizer::Facts(vec![Observation::Available, mcp_report("needs-auth")]), no_home(), false)).unwrap();
    let before = store.load().unwrap();
    assert_eq!(before.mcp[&AgentId::from("agent-1")].servers[0].name, "linear");
    store.conn.execute("UPDATE domain_meta SET revision = revision WHERE singleton = 1", []).unwrap();
    let after = store.load().unwrap();
    assert!(!Arc::ptr_eq(&before, &after), "the store loaded the state again from its rows");
    assert_eq!(after.mcp, before.mcp, "the reload kept what the node reported");
}

// What Claude says of its usage during a turn belongs to the account of the binding that ran the
// turn; the same reading again soon after changes nothing.
#[test]
fn a_usage_observation_is_recorded_for_the_account_of_the_binding_that_produced_it() {
    use alondra_core::usage::{Share, WindowKind};
    use alondra_provider_contract::{UsageReport, UsageWindow, UsageWindowKind};
    let native_binding=ProviderBinding::new("composite-binding".into(),"claude-agent-sdk".into(),"fixture-instance".into(),"fixture-config".into(),1,1,
        BTreeSet::from([Capability::OfferInput]),"fixture-credential-reference".into(),1).unwrap();
    let (fixture, store, execution) = running_with(&native_binding);
    let runtime = runtime(&fixture, store);
    let state=runtime.read();
    runtime.execute_as_system(envelope(state.state(),"native-account",Command::RecordAccounts {accounts:vec![alondra_core::accounts::AccountStatus {
        provider:alondra_core::accounts::Provider::Claude,id:alondra_core::accounts::AccountId::parse("fixture-credential-reference").unwrap(),name:"Native account".into(),state:alondra_core::accounts::AccountState::SignedIn,native_identity: None, email:None,plan:None,billing:Default::default(),sign_ins: 0, profile: None,
    }],reserve:alondra_core::usage::default_reserve()})).unwrap();
    drop(state);
    let observe = |cursor: u64, facts: Vec<Observation>| {
        let state = Arc::clone(runtime.read().state());
        runtime.own(scope_of(&state), |owner| owner.stage_observation(&native_binding, 1, &batch(&execution, cursor), &Normalizer::Facts(facts), no_home(), false)).unwrap();
    };
    let week = |used: u16, exhausted: bool| Observation::Usage(UsageReport {
        windows: vec![UsageWindow { kind: UsageWindowKind::Weekly, used, resets_at: Some(4_102_444_800_000), exhausted }], plan: None,
    });
    observe(1, vec![Observation::Available, week(5_300, false)]);
    let usage = runtime.read().state().account_usage.clone();
    assert_eq!(usage.iter().map(|u| u.account.as_str()).collect::<Vec<_>>(), ["fixture-credential-reference"], "the binding's credential names the account");
    assert_eq!(usage[0].window(&WindowKind::Weekly).map(|w| w.reading.used), Some(Share::of(0.53)));
    let revision = runtime.read().state().revision;
    observe(2, vec![week(5_300, false)]);
    assert_eq!(runtime.read().state().revision, revision, "the same reading again changes nothing");
    observe(3, vec![week(10_000, true), Observation::Usage(UsageReport { windows: vec![], plan: Some("max".into()) })]);
    let usage = runtime.read().state().account_usage[0].clone();
    assert!(usage.window(&WindowKind::Weekly).unwrap().reading.exhausted, "the week ran out");
    assert_eq!(usage.day_start.map(|start| start.weekly), Some(Share::of(0.53)), "today still starts from its first reading");
    assert_eq!(usage.plan.as_deref(), Some("max"));
}

#[test]
fn a_previous_native_credentials_generation_cannot_change_the_new_accounts_auth_or_quota(){
    use alondra_core::accounts::{AccountId,AccountState,AccountStatus,Provider};
    use alondra_core::usage::WindowKind;
    use alondra_provider_contract::{ApiFailure,TurnOutcome,UsageReport,UsageWindow,UsageWindowKind};
    let native=ProviderBinding::new("composite-binding".into(),"claude-agent-sdk".into(),"fixture-instance".into(),"fixture-config".into(),1,1,BTreeSet::from([Capability::OfferInput]),"default".into(),1).unwrap();
    let (fixture,store,execution)=running_with(&native);let runtime=runtime(&fixture,store);
    let publish=|email:&str,signed_in:bool,id:&str|{
        let state=runtime.read();let account=AccountStatus{provider:Provider::Claude,id:AccountId::default_account(),name:"Native default".into(),state:if signed_in{AccountState::SignedIn}else{AccountState::SignedOut},native_identity: None, email:Some(email.into()),plan:None,billing:Default::default(),sign_ins: 0, profile: None};
        runtime.execute_as_system(envelope(state.state(),id,Command::RecordAccounts{accounts:vec![account],reserve:alondra_core::usage::default_reserve()})).unwrap();
    };
    publish("first@example.test",true,"source-native-account");
    let input=runtime.read().state().lifecycle_of(&task()).unwrap().pending_inputs()[0].clone();let turn=TurnId::new("native-first-turn").unwrap();
    let observe=|cursor:u64,facts:Vec<Observation>|{let state=Arc::clone(runtime.read().state());runtime.own(scope_of(&state),|owner|owner.stage_observation(&native,1,&batch(&execution,cursor),&Normalizer::Facts(facts),no_home(),false)).unwrap();};
    observe(1,vec![Observation::Available,Observation::TurnStarted{input,turn:turn.clone()}]);
    publish("first@example.test",false,"source-native-logout");publish("replacement@example.test",true,"replacement-native-login");publish("replacement@example.test",false,"replacement-native-logout");
    assert!(crate::accounts::generation_provider(runtime.read().state(),&alondra_core::accounts::AccountKey::new(Provider::Claude,AccountId::default_account()))>native.credential_generation());
    observe(2,vec![Observation::ModelAnswered,Observation::Usage(UsageReport{windows:vec![UsageWindow{kind:UsageWindowKind::Weekly,used:10_000,resets_at:None,exhausted:true}],plan:None}),Observation::TurnEnded{turn,outcome:TurnOutcome::ApiFailed(ApiFailure{error:"authentication_failed".into(),status:Some(401),resets_at:None})}]);
    let state=runtime.read();assert_eq!(state.state().accounts[0].email.as_deref(),Some("replacement@example.test"));assert_eq!(state.state().accounts[0].state,AccountState::SignedOut);
    assert!(!state.state().account_usage.iter().any(|usage|usage.provider==Provider::Claude && usage.account.is_default() && usage.window(&WindowKind::Weekly).is_some_and(|window|window.reading.exhausted)));
    assert!(state.state().lifecycle_of(&task()).unwrap().turn().is_none(),"the previous task turn still commits");
    assert_eq!(state.state().lifecycle_of(&task()).unwrap().cursor(),Some(3),"Available, TurnStarted and TurnEnded remain three distinct lifecycle facts");
    let normalized:i64=runtime.store.lock().unwrap().conn.query_row("SELECT cursor FROM provider_normalization_checkpoints WHERE task_id=?1 AND generation=?2",rusqlite::params![TASK,i64::try_from(execution.generation()).unwrap()],|row|row.get(0)).unwrap();
    assert_eq!(normalized,2,"the two raw observations still have their exact committed normalization checkpoint");
}

#[test]
fn a_live_native_cell_continuation_is_one_durable_input_without_retiring_own_work() {
    let (fixture,mut store,execution)=running();let input=pending_input(&mut store);let runtime=runtime(&fixture,store);let work=WorkId::new("owned-code-cell").unwrap();let turn=TurnId::new("cell-origin").unwrap();
    let observe=|cursor,facts|{let state=runtime.state().unwrap();runtime.own(scope_of(&state),|owner|owner.stage_observation(&binding(),1,&batch(&execution,cursor),&Normalizer::Facts(facts),no_home(),false)).unwrap().0};
    observe(1,vec![Observation::Available,Observation::TurnStarted{input,turn:turn.clone()},Observation::WorkStarted{turn:turn.clone(),work:work.clone()}]);
    observe(2,vec![Observation::TurnEnded{turn,outcome:alondra_provider_contract::TurnOutcome::Completed}]);
    let state=runtime.state().unwrap();runtime.execute_as_system(envelope(&state,"already-queued",Command::TellNode{task_id:task(),text:"Preserve this existing input".into(), from_user: false, answers: None})).unwrap();
    let before=runtime.state().unwrap();let pending=before.lifecycle_of(&task()).unwrap().pending_inputs().iter().map(|input|input.as_str().to_owned()).collect::<Vec<_>>();
    let before_runtime_cursor=before.lifecycle_of(&task()).unwrap().cursor();
    let before_told=before.lifecycle_journal[&task()].iter().filter(|record|matches!(record,alondra_core::domain::LifecycleRecord::Told{..})).count();
    let continuation=Observation::ContinuationRequested{work:work.clone(),activation:"process/root/origin/exec".into(),text:"Use functions.wait for the existing cell".into()};
    assert_eq!(observe(3,vec![continuation.clone()]),ObservationAcceptance::Accepted);
    let state=runtime.state().unwrap();let after=state.lifecycle_of(&task()).unwrap().pending_inputs().iter().map(|input|input.as_str().to_owned()).collect::<Vec<_>>();assert_eq!(&after[..pending.len()],pending.as_slice());assert_eq!(after.len(),pending.len()+1);
    assert_eq!(state.lifecycle_of(&task()).unwrap().cursor(),before_runtime_cursor,"Tell queues a real input and domain event; it is not a new native runtime fact");
    assert_eq!(state.lifecycle_journal[&task()].iter().filter(|record|matches!(record,alondra_core::domain::LifecycleRecord::Told{..})).count(),before_told+1,"the zero cursor delta cannot skip the actual durable Tell");
    assert!(!state.lifecycle_journal[&task()].iter().any(|record|matches!(record,alondra_core::domain::LifecycleRecord::WorkEnded{work:ended,..} if ended==work.as_str())));
    let before=counts(&observer(&fixture));assert_eq!(observe(3,vec![continuation]),ObservationAcceptance::Duplicate);assert_eq!(counts(&observer(&fixture)),before);
    let told:i64=observer(&fixture).query_row("SELECT count(*) FROM command_audit WHERE command_id LIKE 'native-work-followup-%'",[],|row|row.get(0)).unwrap();assert_eq!(told,1);
    let state=runtime.state().unwrap();let before=counts(&observer(&fixture));let error=runtime.own(scope_of(&state),|owner|owner.stage_observation(&binding(),1,&batch(&execution,4),&Normalizer::Facts(vec![Observation::ContinuationRequested{work:WorkId::new("unknown-cell").unwrap(),activation:"unknown".into(),text:"Do not invent a follow-up".into()}]),no_home(),false));assert!(error.is_err());assert_eq!(counts(&observer(&fixture)),before,"an unowned cell cannot advance raw evidence/checkpoint or queue input");
    let state=runtime.state().unwrap();let accepted=runtime.own(scope_of(&state),|owner|owner.stage_observation(&binding(),1,&batch(&execution,4),&Normalizer::Facts(vec![Observation::ContinuationRequested{work:WorkId::new("unknown-cell").unwrap(),activation:"unknown".into(),text:"Do not invent a follow-up".into()}]),no_home(),true)).unwrap().0;assert_eq!(accepted,ObservationAcceptance::Accepted,"a late continuation is acknowledged with no input after explicit domain refusal");
    let before_terminal=runtime.state().unwrap().lifecycle_of(&task()).unwrap().cursor().unwrap();
    observe(5,vec![Observation::WorkEnded{work:work.clone(),continuation:alondra_foundation::ContinuationId::new("finished-cell").unwrap()}]);
    assert_eq!(runtime.state().unwrap().lifecycle_of(&task()).unwrap().cursor(),Some(before_terminal+1),"positive native terminal evidence still advances the exact native cursor");
    let late=Normalizer::Facts(vec![Observation::ContinuationRequested{work,activation:"late-original-cell".into(),text:"The finished cell needs no follow-up".into()}]);
    let state=runtime.state().unwrap();let before=counts(&observer(&fixture));assert!(runtime.own(scope_of(&state),|owner|owner.stage_observation(&binding(),1,&batch(&execution,6),&late,no_home(),false)).is_err());assert_eq!(counts(&observer(&fixture)),before);
    let state=runtime.state().unwrap();assert_eq!(runtime.own(scope_of(&state),|owner|owner.stage_observation(&binding(),1,&batch(&execution,6),&late,no_home(),true)).unwrap().0,ObservationAcceptance::Accepted);
    drop(runtime);let reopened=DomainRuntime::open_at(&fixture.0,&fixture.path()).unwrap();let state=reopened.state().unwrap();let remaining=state.lifecycle_of(&task()).unwrap().pending_inputs().iter().map(|input|input.as_str().to_owned()).collect::<Vec<_>>();assert_eq!(remaining,after,"the real durable input queue survives restart without a second store");
}
