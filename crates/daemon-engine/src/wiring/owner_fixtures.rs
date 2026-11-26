//! The domain stores and runtimes an SDK node's owner works on, as the owner's tests here and in
//! the crates above build them. They read the SQLite journal itself, so no production crate
//! compiles them: each test binary that uses them includes this file (`#[path]`).
//! One owner transaction on the real domain database: guarded domain commands, provider
//! observations and the lifecycle journal commit together, and the runtime publishes the
//! committed state before it acknowledges. Lifecycle fixtures are not native SDK observations.
use alondra_engine_kernel::domain_store::*;
use alondra_store_sqlite::DomainStore;
use alondra_domain_model::*;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use alondra_agent_application::{InputMetadata, ObservationNormalizer, ObservedFact};
use alondra_domain::{RuntimeFact, TaskLifecycle, Trigger};
use alondra_core::usage::{Share, UsageReading, WindowKind, WindowReading};
use alondra_provider_contract::{McpServerReport, McpSignInReport, OrderedObservation, PreparedCommand, ProviderBinding, TurnOutcome};
use alondra_store::{
    AdapterState, AuthorityScope, CommandEvidence, Committed, CommittedCommand, CommittedControl, ControlAttempt,
    ControlPermit, DispatchAttempt, DispatchPermit, DispatchReceipt, LifecycleTransaction,
    ObservationAcceptance, OwnerGrant, UnresolvedCommand, UnresolvedControl,
};
use alondra_engine_ports::journal::owner::ObservationRefusal;
use alondra_engine_kernel::domain_runtime::DomainRuntime;
use alondra_store_sqlite::testing::{Fixture, seeded};
use alondra_agent_application::{Normalization, ObservationMetadata};
use alondra_foundation::{ContinuationId, ExecutionStamp, InputId, TurnId, WorkId};
use alondra_store::InputPayload;
use alondra_provider_contract::{AdapterError, Capability, Observation, Placement};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::mpsc;

pub(crate) use alondra_engine_kernel::domain_store::admit_through_sdk;


pub const TASK: &str = "task-1";

/// No fixture here asks for a decision, so no «allow always» is ever judged under a home.
pub fn no_home() -> &'static std::path::Path {
    std::path::Path::new("/nonexistent")
}

pub fn agent() -> Actor {
    Actor::Agent {
        agent_id: "agent-1".into(),
    }
}
pub fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}
pub fn task() -> TaskId {
    TASK.into()
}
pub fn envelope(state: &DomainState, id: &str, command: Command) -> CommandEnvelope {
    CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: id.into(),
        expected_revision: 0,
        command,
    }
}
pub fn ask(id: &str) -> Command {
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
pub fn answer() -> Command {
    Command::AnswerQuestion {
        interaction_id: "composite-card".into(),
        option_id: "continue".into(),
        inputs: BTreeMap::new(),
        attachments: vec![],
    }
}
pub fn binding() -> ProviderBinding {
    binding_with(BTreeSet::from([Capability::OfferInput]))
}
pub fn binding_with(capabilities: BTreeSet<Capability>) -> ProviderBinding {
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
pub fn batch(execution: &ExecutionStamp, cursor: u64) -> OrderedObservation {
    OrderedObservation {
        owner_generation: 1,
        execution: execution.clone(),
        cursor,
        evidence: format!("raw observation {cursor}").into_bytes(),
    }
}
pub fn scope_of(state: &DomainState) -> AuthorityScope {
    AuthorityScope::new(state.data_epoch.clone()).unwrap()
}

pub enum Normalizer {
    Available,
    SwallowsMetadataError,
    Released,
    /// The resource is up, a turn takes `input` and ends, after starting work of its own if
    /// `work`.
    Turn { input: InputId, work: bool },
    Facts(Vec<Observation>),
    /// Refuses every observation, as the Claude adapter refused a command it found no input for.
    Refuses,
}
impl ObservationNormalizer for Normalizer {
    fn skip(&self, _: &OrderedObservation, _: &ObservationMetadata<'_, '_>) -> std::result::Result<Normalization, AdapterError> {
        Normalization::new(vec![], b"normalized".to_vec())
    }
    fn normalize(
        &self,
        _: &OrderedObservation,
        metadata: &ObservationMetadata<'_, '_>,
    ) -> std::result::Result<Normalization, AdapterError> {
        if let Self::SwallowsMetadataError = self {
            assert!(metadata.lookup_inputs(&vec![b"native".to_vec(); 65]).is_err());
        }
        if let Self::Refuses = self {
            return Err(AdapterError::Storage("MissingAdapterInput".into()));
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
pub fn admit(label: &str) -> Command {
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
pub fn launch_with(journal: &mut LifecycleTransaction<'_>, binding: &ProviderBinding) -> alondra_store::Result<ExecutionStamp> {
    journal.stage_launch(&task(), &Placement::RemoteWorkspace("fixture/workspace".into()), binding)?;
    let execution = journal.load(&task())?.execution().unwrap().clone();
    journal.reserve_owner(&execution, 0)?;
    Ok(execution)
}

/// The Task runs an execution the SDK host owns at generation 1, whose provider has observed
/// nothing yet: its `claude --bg` node was released and the domain admitted the SDK's.
pub fn running() -> (Fixture, DomainStore, ExecutionStamp) {
    running_with(&binding())
}
pub fn running_with(binding: &ProviderBinding) -> (Fixture, DomainStore, ExecutionStamp) {
    let (fixture, mut store) = installed();
    let state = store.load().unwrap();
    let release = Command::ReleaseTask { task_id: task(), reason: "The SDK host takes it".into(), relaunch: alondra_domain_model::Relaunch::ByItself, cwd: None };
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
pub fn installed() -> (Fixture, DomainStore) {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path(), &alondra_domain_rules::Rules).unwrap();
    seeded(&mut store);
    let state = store.load().unwrap();
    store
        .execute_filling_revision(&agent(), &envelope(&state, "composite-question", ask("composite-card")))
        .unwrap();
    (fixture, store)
}
pub fn runtime(fixture: &Fixture, store: DomainStore) -> DomainRuntime {
    drop(store);
    alondra_daemon_engine::wiring::open_at(&fixture.0, &fixture.path()).unwrap()
}

pub const TABLES: [&str; 8] = [
    "provider_observations",
    "provider_normalization_checkpoints",
    "lifecycle_events",
    "lifecycle_inputs",
    "interactions",
    "semantic_receipts",
    "domain_outbox",
    "command_audit",
];
pub fn counts(conn: &Connection) -> [i64; 8] {
    TABLES.map(|table| conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap())
}
/// The daemon restarts: the runtime closes and opens again on the same database.
pub fn runtime_reopened(fixture: &Fixture, runtime: DomainRuntime) -> DomainRuntime {
    drop(runtime);
    alondra_daemon_engine::wiring::open_at(&fixture.0, &fixture.path()).unwrap()
}

pub fn observer(fixture: &Fixture) -> Connection {
    Connection::open(fixture.path()).unwrap()
}
pub fn card(conn: &Connection, id: &str) -> Option<(Option<String>, bool)> {
    conn.query_row("SELECT answer,withdrawn FROM interactions WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .unwrap()
}
pub fn journal_after(store: &mut DomainStore) -> (Vec<String>, bool, usize) {
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

pub fn pending_input(store: &mut DomainStore) -> InputId {
    let (inputs, _, _) = journal_after(store);
    InputId::new(inputs[0].clone()).unwrap()
}
/// The case the orchestrator gets when turn `turn` of the running execution ends as a stop.
pub fn stall_case_of(turn: &str) -> InteractionId {
    alondra_domain_model::stalled_card_id(&task(), 2, turn)
}
/// Ends the first turn of the running execution with nothing that explains its next step (the
/// fixture's question withdrawn first), which wakes its node, ends empty the turns of its other
/// wakes, and returns the normalizer of the turn the last wake opens, `turn-2`, ending the same way.
pub fn woken(store: &mut DomainStore, execution: &ExecutionStamp) -> Normalizer {
    let state = store.load().unwrap();
    let withdraw = Command::Withdraw { interaction_id: "composite-card".into() };
    store.execute_filling_revision(&agent(), &envelope(&state, "withdraw-composite", withdraw)).unwrap();
    let input = pending_input(store);
    let state = store.load().unwrap();
    store
        .own(scope_of(&state), |owner| owner.stage_observation(&binding(), 1, &batch(execution, 1), &Normalizer::Turn { input, work: false }, no_home(), false))
        .unwrap();
    let empty = |store: &mut DomainStore, turn: &str| {
        let turn = TurnId::new(turn).unwrap();
        Normalizer::Facts(vec![
            Observation::TurnStarted { input: pending_input(store), turn: turn.clone() },
            Observation::TurnEnded { turn, outcome: alondra_provider_contract::TurnOutcome::Completed },
        ])
    };
    for cursor in 2..WOKEN_CURSOR {
        let normalizer = empty(store, &format!("turn-woken-{cursor}"));
        let state = store.load().unwrap();
        store.own(scope_of(&state), |owner| owner.stage_observation(&binding(), 1, &batch(execution, cursor), &normalizer, no_home(), false)).unwrap();
    }
    empty(store, "turn-2")
}
/// The cursor of the observation `woken` returns: the turns of the node's other wakes came before.
pub const WOKEN_CURSOR: u64 = alondra_domain::EMPTY_TURN_WAKES as u64 + 1;

/// The running execution as t-main-red-accounts-email-lowercase stood on 2026-11-07 at 02:08:
/// its first turn left two works behind, both ended while no turn was open, and the second turn,
/// which the first notification opened, started work of its own. Returns the runtime and that
/// turn, still open, with the second notification waiting.
pub fn resumed_with_a_notification_waiting() -> (Fixture, DomainRuntime, ExecutionStamp, TurnId) {
    let (fixture, mut store, execution) = running();
    let state = store.load().unwrap();
    let withdraw = Command::Withdraw { interaction_id: "composite-card".into() };
    store.execute_filling_revision(&agent(), &envelope(&state, "withdraw-composite", withdraw)).unwrap();
    let input = pending_input(&mut store);
    let runtime = runtime(&fixture, store);
    let (first, second) = (TurnId::new("turn-1").unwrap(), TurnId::new("turn-2").unwrap());
    let work = |id: &str| WorkId::new(id).unwrap();
    let notified = |id: &str| ContinuationId::new(format!("claude-notification:{id}")).unwrap();
    let observations = [
        vec![
            Observation::Available,
            Observation::TurnStarted { input, turn: first.clone() },
            Observation::WorkStarted { turn: first.clone(), work: work("work-a") },
            Observation::WorkStarted { turn: first.clone(), work: work("work-b") },
            Observation::TurnEnded { turn: first, outcome: alondra_provider_contract::TurnOutcome::Completed },
        ],
        vec![Observation::WorkEnded { work: work("work-a"), continuation: notified("a") }],
        vec![Observation::WorkEnded { work: work("work-b"), continuation: notified("b") }],
        vec![
            Observation::TurnResumed { continuation: notified("a"), turn: second.clone() },
            Observation::WorkStarted { turn: second.clone(), work: work("work-c") },
        ],
    ];
    for (at, facts) in (1..).zip(observations) {
        let state = Arc::clone(runtime.read().state());
        runtime.own(scope_of(&state), |owner| owner.stage_observation(&binding(), 1, &batch(&execution, at), &Normalizer::Facts(facts), no_home(), false)).unwrap();
    }
    (fixture, runtime, execution, second)
}

pub fn cursor_of(runtime: &DomainRuntime) -> u64 {
    runtime.read().state().lifecycle_of(&task()).unwrap().cursor().unwrap()
}


pub fn discarded(fixture: &Fixture) -> Vec<(i64, String)> {
    let observer = observer(fixture);
    let mut rows = observer.prepare("SELECT cursor,reason FROM provider_discarded_observations WHERE task_id=?1 ORDER BY cursor").unwrap();
    rows.query_map([TASK], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().collect::<Result<_, _>>().unwrap()
}
