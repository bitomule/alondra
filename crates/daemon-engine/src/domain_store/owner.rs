//! The one SQL transaction that stages domain commands, provider observations and the lifecycle
//! journal, and commits them together. Only `LifecycleTransaction::commit` commits it, and only
//! that commit issues the store's seals.
use super::*;
use alondra_agent_application::{InputMetadata, ObservationNormalizer, ObservedFact};
use alondra_domain::{RuntimeFact, TaskLifecycle, Trigger};
use alondra_core::usage::{Share, UsageReading, WindowKind, WindowReading};
use alondra_foundation::ExecutionStamp;
use alondra_provider_contract::{AdapterError, Capability, McpServerReport, McpSignInReport, OrderedObservation, Placement, PreparedCommand, ProviderBinding, TurnOutcome};
use alondra_store::{
    AdapterState, AuthorityScope, CommandEvidence, Committed, CommittedCommand, CommittedControl, ControlAttempt,
    ControlPermit, DispatchAttempt, DispatchPermit, DispatchReceipt, InputPayload, LifecycleTransaction,
    ObservationAcceptance, OwnerGrant, UnresolvedCommand, UnresolvedControl,
};

/// The provider's part of a dispatch: its metadata for an input, and the exact command it sends.
pub(crate) trait Preparer {
    fn stage_input(&mut self, metadata: InputMetadata<'_, '_>) -> std::result::Result<(), AdapterError>;
    fn prepare(&mut self, attempt: &DispatchAttempt, metadata: InputMetadata<'_, '_>) -> std::result::Result<PreparedCommand, AdapterError>;
}

/// What a daemon takes over: claimed commands to reconcile, and controls whose preparation
/// recorded no sendable bytes. Only an ended host lets those unrecorded controls be rejected.
pub(crate) struct Adoption {
    pub(crate) stamp: ExecutionStamp,
    pub(crate) directory: String,
    pub(crate) commands: Vec<UnresolvedCommand>,
    pub(crate) controls: Vec<UnresolvedControl>,
    pub(crate) unrecorded_controls: Vec<i64>,
    pub(crate) unrecorded_commands: Vec<i64>,
}

/// Every step runs inside the journal's guard: an error or a panic in any of them poisons the
/// whole transaction, even when the caller catches it, so it can only roll back.
pub(crate) struct OwnerTransaction<'connection> {
    conn: &'connection Connection,
    cached: &'connection Option<Cached>,
    journal: LifecycleTransaction<'connection>,
    draft: OwnerDraft,
    /// What the host observed that moves the task's lifecycle and the domain refused to record,
    /// with why: the task's record no longer follows its Claude.
    unrecorded: Vec<String>,
}

/// What the commands staged so far add up to. A later replay or no-op never takes back an
/// earlier change, its notification or the outcome another command got.
#[derive(Debug)]
pub(crate) struct OwnerDraft {
    pub(crate) state: Arc<DomainState>,
    pub(crate) outcomes: Vec<StagedOutcome>,
    pub(crate) domain_changed: bool,
    pub(crate) schedule_changed: bool,
    version: i64,
}

#[derive(Debug)]
pub(crate) struct StagedOutcome {
    pub(crate) command_id: CommandId,
    pub(crate) outcome: CommandOutcome,
    pub(crate) replayed: bool,
}

/// Exists only after the real SQL commit returned. Its seals stay inside until the runtime has
/// published the committed state.
#[derive(Debug)]
pub(crate) struct OwnerCommit {
    pub(crate) draft: OwnerDraft,
    technical: Committed,
}
impl OwnerCommit {
    /// Hands out the seals only once readers see exactly the state this transaction committed.
    pub(crate) fn acknowledge(
        self,
        published: &ReadSnapshot,
    ) -> std::result::Result<(OwnerDraft, Committed), StoreError> {
        if !Arc::ptr_eq(published.state(), &self.draft.state) {
            return Err(StoreError::Storage(anyhow::anyhow!(
                "a committed owner transaction is acknowledged only after its state is published"
            )));
        }
        Ok((self.draft, self.technical))
    }
}

impl From<alondra_store::Error> for StoreError {
    fn from(error: alondra_store::Error) -> Self {
        Self::Storage(error.into())
    }
}
/// A fact the domain records for an SDK execution, as its command carries it.
fn observed(fact: &RuntimeFact) -> std::result::Result<RuntimeObservation, StoreError> {
    Ok(match fact {
        RuntimeFact::ResourceAvailable => RuntimeObservation::ResourceAvailable,
        RuntimeFact::TurnStarted { turn, trigger: Trigger::Input(input) } => {
            RuntimeObservation::TurnStarted { turn: turn.as_str().into(), input: input.as_str().into() }
        }
        RuntimeFact::TurnStarted { turn, trigger: Trigger::OwnedContinuation(continuation) } => {
            RuntimeObservation::TurnResumed { turn: turn.as_str().into(), continuation: continuation.as_str().into() }
        }
        RuntimeFact::TurnStarted { turn, trigger: Trigger::Unprompted } => RuntimeObservation::TurnUnprompted { turn: turn.as_str().into() },
        RuntimeFact::InputConsumed { input } => RuntimeObservation::InputConsumed { input: input.as_str().into() },
        RuntimeFact::WorkStarted { turn, work } => RuntimeObservation::WorkStarted { turn: turn.as_str().into(), work: work.as_str().into() },
        RuntimeFact::WorkEnded { work, continuation } => {
            RuntimeObservation::WorkEnded { work: work.as_str().into(), continuation: continuation.as_str().into() }
        }
        RuntimeFact::ContinuationConsumed { continuation } => RuntimeObservation::WorkContinued { continuation: continuation.as_str().into() },
        other => return Err(StoreError::Storage(anyhow::anyhow!("provider observation refused: the domain does not record {other:?} yet"))),
    })
}
/// What a test has the domain refuse, once, as it refuses what it cannot record.
#[cfg(any(test, feature = "test-hooks"))]
#[derive(Clone, Copy, PartialEq)]
pub enum Refuse {
    TurnEnd,
    DecisionRequest,
    InterruptAccepted,
}

#[cfg(any(test, feature = "test-hooks"))]
thread_local! {
    static REFUSE: std::cell::Cell<Option<Refuse>> = const { std::cell::Cell::new(None) };
}

/// The next `what` this thread observes is refused.
#[cfg(any(test, feature = "test-hooks"))]
pub fn refuse_next(what: Refuse) {
    REFUSE.with(|refuse| refuse.set(Some(what)));
}

/// The refusal the test asked for; the run that probes takes it.
#[cfg(any(test, feature = "test-hooks"))]
fn refused_for_test(command: &Command, probe: bool) -> Option<StoreError> {
    let what = match command {
        Command::RecordTurnBoundary { .. } => Refuse::TurnEnd,
        Command::RecordDecisionRequest { .. } => Refuse::DecisionRequest,
        Command::RecordInterruptAccepted { .. } => Refuse::InterruptAccepted,
        _ => return None,
    };
    let refused = REFUSE.with(|refuse| {
        let asked = refuse.get() == Some(what);
        if asked && probe {
            refuse.set(None);
        }
        asked
    });
    refused.then(|| DomainError::new("conflict", "the test refuses it").into())
}

#[cfg(not(any(test, feature = "test-hooks")))]
fn refused_for_test(_: &Command, _: bool) -> Option<StoreError> {
    None
}

fn command_name(command: &Command) -> &'static str {
    match command {
        Command::RecordRuntimeFacts { .. } => "its runtime facts",
        Command::RecordTurnBoundary { .. } => "the end of its turn",
        Command::RecordDroppedInput { .. } => "an input Claude dropped",
        Command::RecordDecisionRequest { .. } => "a request for a decision",
        Command::RecordDecisionSettled { .. } | Command::RecordDecisionAborted { .. } => "the end of a decision",
        Command::RecordTaskWorktree { .. } => "a worktree",
        Command::RecordMcpServers { .. } | Command::RecordMcpSignIn { .. } | Command::ReadMcpStatus { .. } => "its MCP servers",
        _ => "a fact",
    }
}

/// Whether `command` moves the task's lifecycle: refused, the task's record no longer follows
/// what its Claude does.
fn moves_lifecycle(command: &Command) -> bool {
    matches!(command, Command::RecordRuntimeFacts { .. } | Command::RecordTurnBoundary { .. } | Command::RecordDroppedInput { .. }
        | Command::RecordDecisionRequest { .. } | Command::RecordDecisionSettled { .. } | Command::RecordDecisionAborted { .. } | Command::TellNode { .. })
}
fn application_error(error: alondra_agent_application::Error) -> StoreError {
    match error {
        alondra_agent_application::Error::Store(error) => error.into(),
        other => StoreError::Storage(anyhow::anyhow!("provider observation refused: {other:?}")),
    }
}

impl<'connection> OwnerTransaction<'connection> {
    pub(crate) fn domain_state(&self) -> &DomainState { &self.draft.state }
    pub(crate) fn record_settings_projection(&mut self, identity: &super::provider_settings::ProjectionIdentity) -> std::result::Result<(),StoreError> {
        let conn = self.conn;
        self.journal.inspect_mut(|_| super::provider_settings::record(conn,identity).map_err(StoreError::Storage))
    }

    fn begin(
        conn: &'connection Connection,
        cached: &'connection Option<Cached>,
        aggregates: &'connection alondra_store::AggregateCache,
        scope: AuthorityScope,
    ) -> std::result::Result<Self, StoreError> {
        let transaction = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let journal = LifecycleTransaction::attach_cached(transaction, scope, aggregates)?;
        let (state, version) = journal.inspect(|| {
            check_schema(conn)?;
            let (state, _, version) = state_in(cached, conn)?;
            Ok::<_, StoreError>((state, version))
        })?;
        Ok(Self {
            conn,
            cached,
            journal,
            draft: OwnerDraft {
                state,
                outcomes: vec![],
                domain_changed: false,
                schedule_changed: false,
                version,
            },
            unrecorded: vec![],
        })
    }

    pub(crate) fn execution_binding(&self, stamp: &ExecutionStamp) -> std::result::Result<ProviderBinding, StoreError> { Ok(self.journal.execution_binding(stamp)?) }
    pub(crate) fn state(&self) -> &DomainState { &self.draft.state }

    pub(crate) fn retained_context(&self, task: &TaskId, after: Option<&alondra_store::ContextRef>, limit: usize) -> std::result::Result<alondra_store::ContextPage,StoreError> {
        Ok(self.journal.context_page(task,after,limit)?)
    }
    pub(crate) fn pin_context(&mut self, transfer: &str, through: &alondra_store::ContextRef) -> std::result::Result<(),StoreError> {
        Ok(self.journal.pin_context(transfer,through)?)
    }
    pub(crate) fn release_context(&mut self, transfer: &str, task: &TaskId) -> std::result::Result<(),StoreError> {
        Ok(self.journal.release_context(transfer,task)?)
    }

    /// What this transaction's observations moved in Claude and the domain refused to record.
    pub(crate) fn unrecorded(&self) -> &[String] {
        &self.unrecorded
    }

    /// The domain's own guarded command, decided against this transaction's current state.
    /// Documents are refused: their content needs the verified store and lease of `execute_with`.
    pub(crate) fn stage_command(
        &mut self,
        actor: &Actor,
        envelope: &CommandEnvelope,
        attached: Attached<'_>,
    ) -> std::result::Result<&CommandOutcome, StoreError> {
        if let Some(refusal) = refused_for_test(&envelope.command, true) {
            return Err(refusal);
        }
        let (conn, cached) = (self.conn, self.cached);
        let prepared = self.journal.inspect_mut(|journal| {
            if matches!(envelope.command, Command::CreateDocument { .. } | Command::ReviseDocument { .. }) {
                return Err(DomainError::new(
                    "invalid_input",
                    "a document's content is verified and stored outside this transaction",
                )
                .into());
            }
            command::stage(conn, cached, actor, envelope, None, true, attached, journal)
        })?;
        self.draft.record(&envelope.command, &envelope.command_id, prepared.commit);
        Ok(&self.draft.outcomes.last().expect("outcome just recorded").outcome)
    }

    /// Raw provider evidence and its checkpoint, and the facts it was normalized to, recorded by
    /// the domain's own commands in this same transaction: technical facts by RecordRuntimeFacts,
    /// the end of the root turn by RecordTurnBoundary, a decision Claude asks for by
    /// RecordDecisionRequest once its request is kept, and its end by RecordDecisionSettled or
    /// RecordDecisionAborted. Facts the domain does not record yet (a lost connection, a released
    /// resource) refuse the whole observation. `home` is Alondra's, where «allow always» is judged.
    /// With `probe`, each command is first decided on a copy and one the domain refuses is left
    /// out, a request for a decision denied to Claude with why: what Alondra cannot record never
    /// stops the node, and `unrecorded` says what of its lifecycle was left out.
    pub(crate) fn stage_observation<N: ObservationNormalizer>(
        &mut self,
        binding: &ProviderBinding,
        owner_generation: u64,
        batch: &OrderedObservation,
        normalizer: &N,
        home: &std::path::Path,
        probe: bool,
    ) -> std::result::Result<ObservationAcceptance, StoreError> {
        self.stage_observation_with_identity(binding,owner_generation,batch,normalizer,home,probe,None)
    }
    pub(crate) fn stage_observation_with_identity<N:ObservationNormalizer>(&mut self,binding:&ProviderBinding,owner_generation:u64,batch:&OrderedObservation,normalizer:&N,home:&std::path::Path,probe:bool,identity:Option<&crate::sdk_owner::NativeAccountIdentity>)->std::result::Result<ObservationAcceptance,StoreError>{
        let pending = alondra_agent_application::stage_observation(
            &mut self.journal,
            binding,
            owner_generation,
            batch,
            normalizer,
        )
        .map_err(application_error)?;
        let execution = pending.execution().clone();
        let (task_id, generation) = (execution.task().clone(), execution.generation());
        let proven_same_identity=|provider:alondra_core::accounts::Provider,status:&alondra_core::accounts::AccountStatus|identity.is_some_and(|expected|expected.change(status).is_none() && expected.same_known_native_account(provider,status));
        let account_fresh=alondra_core::accounts::Provider::from_driver(binding.driver()).is_none_or(|provider|{
            let account=crate::usage_probe::account_of(binding.credential());let key=alondra_core::accounts::AccountKey::new(provider,account.clone());
            let status=self.draft.state.accounts.iter().find(|status|status.provider==provider && status.id==account);
            if provider==alondra_core::accounts::Provider::Codex{return status.is_some_and(|status|proven_same_identity(provider,status))}
            let generation_matches=binding.credential_generation()==crate::accounts::generation_provider(&self.draft.state,&key);
            (generation_matches || status.is_some_and(|status|proven_same_identity(provider,status)))
                && identity.is_none_or(|expected|status.is_none_or(|status|expected.change(status)!=Some("identity")))
        });
        let same_known_identity=identity.is_some_and(|_|alondra_core::accounts::Provider::from_driver(binding.driver()).is_some_and(|provider|{
            let account=crate::usage_probe::account_of(binding.credential());
            self.draft.state.accounts.iter().find(|status|status.provider==provider && status.id==account).is_some_and(|status|proven_same_identity(provider,status))
        }));
        let cursor = |journal: &LifecycleTransaction<'_>| -> std::result::Result<Option<u64>, StoreError> {
            let lifecycle = journal.load(&task_id)?;
            Ok((lifecycle.execution() == Some(&execution)).then(|| lifecycle.cursor().unwrap_or(0)))
        };
        let before = cursor(&self.journal)?;
        let mut reconciled = false;
        let mut commands = vec![];
        let mut continuations=std::collections::BTreeMap::new();
        let mut read_status = false;
        let mut technical = vec![];
        let flush = |commands: &mut Vec<Command>, technical: &mut Vec<RuntimeObservation>| {
            if !technical.is_empty() {
                commands.push(Command::RecordRuntimeFacts { task_id: task_id.clone(), generation, facts: std::mem::take(technical) });
            }
        };
        for fact in pending.facts() {
            match fact {
                ObservedFact::Runtime(fact) => technical.push(self.journal.inspect(|| observed(fact))?),
                ObservedFact::ContinuationRequested { work, activation, text } => {
                    flush(&mut commands, &mut technical);
                    if activation.is_empty() || activation.len()>4096 || text.trim().is_empty() || text.len()>8192 {
                        return Err(DomainError::new("invalid_input","The native work continuation is invalid").into());
                    }
                    continuations.insert(commands.len(),(work.clone(),activation.clone()));
                    commands.push(Command::TellNode{task_id:task_id.clone(),text:text.clone(), from_user: false, answers: None});
                }
                ObservedFact::ModelAnswered => if account_fresh || same_known_identity{technical.push(RuntimeObservation::ModelAnswered)},
                ObservedFact::TurnEnded { turn, outcome } => {
                    flush(&mut commands, &mut technical);
                    if let TurnOutcome::ApiFailed(failure)=outcome
                        && account_fresh
                        && alondra_core::accounts::Provider::from_driver(binding.driver())==Some(alondra_core::accounts::Provider::Codex)
                        && alondra_core::domain::classify_sdk(&failure.error,failure.status)==Some(alondra_core::domain::SdkFailureKind::Usage)
                    {
                        let account=self.draft.state.tasks.get(&task_id).ok_or_else(||DomainError::new("not_found","Task missing"))?.account_key();
                        let provider=alondra_core::accounts::Provider::from_driver(binding.driver()).ok_or_else(||DomainError::new("unknown_provider","The native quota observation has no supported driver"))?;
                        if provider!=account.provider{return Err(DomainError::new("provider_mismatch","The native quota belongs to another provider").into())}
                        let at=alondra_core::now_millis();
                        let kind=if provider==alondra_core::accounts::Provider::Codex{WindowKind::Other("codex:account:limit".into())}else{WindowKind::Session};
                        let resets_at=failure.resets_at.and_then(|seconds|i64::try_from(seconds).ok()?.checked_mul(1000)).filter(|reset|*reset>at);
                        commands.push(Command::RecordAccountUsage{provider,account:account.account,at,day:crate::node_reports::local_day(at),reading:UsageReading{plan:None,complete:false,windows:vec![WindowReading{kind,used:Share::NONE,exhausted:true,resets_at}]}});
                    }
                    if matches!(outcome,TurnOutcome::Completed)
                        && account_fresh
                        && alondra_core::accounts::Provider::from_driver(binding.driver())==Some(alondra_core::accounts::Provider::Codex)
                    {
                        let account=self.draft.state.tasks.get(&task_id).ok_or_else(||DomainError::new("not_found","Task missing"))?.account_key();
                        let limited=self.draft.state.account_usage.iter().any(|usage|usage.provider==account.provider&&usage.account==account.account
                            &&usage.windows.iter().any(|window|window.reading.kind.is_native_limit_marker()&&window.reading.exhausted));
                        if account.provider==alondra_core::accounts::Provider::Codex && limited {
                            let at=alondra_core::now_millis();
                            commands.push(Command::RecordAccountUsage{provider:account.provider,account:account.account,at,day:crate::node_reports::local_day(at),reading:alondra_core::usage::native_turn_worked()});
                        }
                    }
                    let (failure, failed) = match outcome {
                        TurnOutcome::Completed => (None, None),
                        TurnOutcome::ApiFailed(_) if !account_fresh => (None,Some("stale_native_account_observation".into())),
                        TurnOutcome::ApiFailed(failure) => {
                            (Some(SdkApiFailure { error: failure.error.clone(), status: failure.status, resets_at: failure.resets_at }), None)
                        }
                        TurnOutcome::Failed { code } => (None, Some(code.clone())),
                    };
                    commands.push(Command::RecordTurnBoundary { task_id: task_id.clone(), generation, turn: turn.as_str().into(), failure, failed });
                }
                ObservedFact::InputDropped { input } => {
                    flush(&mut commands, &mut technical);
                    let conn = self.conn;
                    let text = self.journal.inspect(|| {
                        conn.query_row("SELECT body FROM lifecycle_inputs WHERE task_id=?1 AND input_id=?2", rusqlite::params![task_id.0, input.as_str()], |row| row.get::<_, Option<String>>(0))
                            .optional()
                            .map_err(|error| StoreError::Storage(error.into()))
                    })?;
                    let text = text.flatten().ok_or_else(|| StoreError::Storage(anyhow::anyhow!("task {task_id} has no text for its dropped input {}", input.as_str())))?;
                    commands.push(Command::RecordDroppedInput { task_id: task_id.clone(), generation, input: input.as_str().into(), text });
                }
                ObservedFact::Asked { request, epoch, kind, tool_use, payload } => {
                    flush(&mut commands, &mut technical);
                    let conn = self.conn;
                    let prior = self.journal.inspect(|| {
                        conn.prepare("SELECT epoch,tool_use_id,kind,payload FROM lifecycle_decision_requests WHERE task_id=?1 AND generation=?2 AND request_id=?3 AND epoch<?4 ORDER BY epoch")
                            .and_then(|mut query| query.query_map(rusqlite::params![task_id.0, i64::try_from(generation).unwrap_or(i64::MAX), request, i64::try_from(*epoch).unwrap_or(i64::MAX)], |row| {
                                let epoch = row.get::<_, i64>(0)?;
                                let epoch = u64::try_from(epoch).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, epoch))?;
                                Ok((epoch, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?, row.get::<_, Vec<u8>>(3)?))
                            })?.collect::<Result<Vec<_>, _>>())
                            .map_err(|error| StoreError::Storage(error.into()))
                    })?;
                    let plan_identity = *kind == alondra_provider_contract::DecisionKind::Plan || prior.iter().any(|(_, _, kind, _)| kind == "plan");
                    if plan_identity && prior.iter().any(|(_, previous_tool, previous_kind, previous_payload)| {
                        *kind != alondra_provider_contract::DecisionKind::Plan || previous_kind != "plan" || previous_tool != tool_use || previous_payload != payload
                    }) {
                        self.journal.inspect(|| Err::<(), StoreError>(alondra_store::Error::ObservationConflict.into()))?;
                    }
                    let recorded = (*kind == alondra_provider_contract::DecisionKind::Plan).then(|| {
                        prior.iter().find_map(|(previous, ..)| {
                            self.draft.state.lifecycle_journal.get(&task_id)?.iter().rev()
                                .take_while(|record| !matches!(record, alondra_core::domain::LifecycleRecord::Admitted { .. }))
                                .any(|record| match record {
                                    alondra_core::domain::LifecycleRecord::Decided { card, epoch, .. } => epoch == previous
                                        && self.draft.state.holds.get(card) == Some(&alondra_core::domain::Hold::Plan),
                                    alondra_core::domain::LifecycleRecord::DecidedByPolicy { epoch, .. } => epoch == previous,
                                    _ => false,
                                })
                                .then_some(*previous)
                        })
                    }).flatten();
                    let kept = alondra_store::DecisionRequest::new(request.clone(), tool_use.clone(), *kind, payload.clone())?;
                    self.journal.put_decision_request(&execution, *epoch, &kept)?;
                    let (payload, worktree, worktrees) = self.journal.inspect(|| {
                        let payload: serde_json::Value = serde_json::from_slice(payload).map_err(|error| StoreError::Storage(error.into()))?;
                        let worktree = conn
                            .query_row("SELECT dir FROM task_worktrees WHERE task_id=?1", rusqlite::params![task_id.0], |row| row.get::<_, String>(0))
                            .optional()
                            .map_err(|error| StoreError::Storage(error.into()))?;
                        let worktrees = conn
                            .prepare("SELECT path FROM task_worktree_records WHERE task_id=?1 AND \"end\" IS NULL ORDER BY path")
                            .and_then(|mut open| open.query_map(rusqlite::params![task_id.0], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>())
                            .map_err(|error| StoreError::Storage(error.into()))?;
                        Ok::<_, StoreError>((payload, worktree, worktrees))
                    })?;
                    let asked = crate::domain_holds::sdk_decision(self.state().tasks.get(&task_id).map(|task| task.title.as_str()).unwrap_or("Plan"), *kind, &payload, worktree.as_deref(), &worktrees, home);
                    commands.push(Command::RecordDecisionRequest {
                        task_id: task_id.clone(),
                        generation,
                        epoch: *epoch,
                        hold: asked.hold,
                        tool: asked.tool,
                        input: asked.input,
                        mcp_server: asked.mcp_server,
                        prompt: recorded.map_or(asked.prompt, |epoch| alondra_core::domain::DecisionPrompt::RecordedPlan { epoch }),
                    });
                }
                ObservedFact::Worktree { path, branch, subagent } => {
                    flush(&mut commands, &mut technical);
                    let origin = if *subagent { WorktreeOrigin::Subagent } else { WorktreeOrigin::Entered };
                    commands.push(Command::RecordTaskWorktree { task_id: task_id.clone(), path: path.clone(), origin, branch: branch.clone() });
                }
                ObservedFact::SubagentWorktree { agent } => {
                    flush(&mut commands, &mut technical);
                    let conn = self.conn;
                    let own = self.journal.inspect(|| {
                        conn.query_row("SELECT dir FROM task_worktrees WHERE task_id=?1", rusqlite::params![task_id.0], |row| row.get::<_, String>(0))
                            .optional()
                            .map_err(|error| StoreError::Storage(error.into()))
                    })?;
                    let path = own.as_deref().and_then(|own| crate::worktree::subagent_worktree(std::path::Path::new(own), agent));
                    let path = path.ok_or_else(|| StoreError::Storage(anyhow::anyhow!("task {task_id} has no worktree of its own to place subagent {agent}'s")))?;
                    commands.push(Command::RecordTaskWorktree {
                        task_id: task_id.clone(),
                        path: path.display().to_string(),
                        origin: WorktreeOrigin::Subagent,
                        branch: None,
                    });
                }
                ObservedFact::Settled { epoch, answered, .. } => {
                    flush(&mut commands, &mut technical);
                    commands.push(if *answered {
                        Command::RecordDecisionSettled { task_id: task_id.clone(), generation, epoch: *epoch }
                    } else {
                        Command::RecordDecisionAborted { task_id: task_id.clone(), generation, epoch: *epoch }
                    });
                }
                ObservedFact::McpServers { servers } => {
                    flush(&mut commands, &mut technical);
                    let servers: Vec<McpServerState> = servers.iter().map(mcp_server).collect();
                    if self.mcp_changes(&task_id, generation, &servers) {
                        commands.push(Command::RecordMcpServers { task_id: task_id.clone(), generation, servers });
                    }
                }
                ObservedFact::McpSignIn { server, sign_in } => {
                    flush(&mut commands, &mut technical);
                    let sign_in = match sign_in {
                        McpSignInReport::Browser { url, callback_port, state } => McpSignIn::Browser { url: url.clone(), callback_port: *callback_port, state: state.clone() },
                        McpSignInReport::Connected => McpSignIn::Connected,
                        McpSignInReport::Refused { error } => McpSignIn::Refused { error: error.clone() },
                    };
                    commands.push(Command::RecordMcpSignIn { task_id: task_id.clone(), generation, server: server.clone(), sign_in });
                }
                ObservedFact::PullRequestOpened(pr) => {
                    flush(&mut commands, &mut technical);
                    commands.push(Command::NotePrOpened { task_id: task_id.clone(), host: pr.host.clone(), repository: pr.repository.clone(), number: pr.number });
                }
                ObservedFact::PullRequestNotNamed => {
                    flush(&mut commands, &mut technical);
                    commands.push(Command::AskToRegisterPr { task_id: task_id.clone() });
                }
                ObservedFact::McpToolFailed => {
                    flush(&mut commands, &mut technical);
                    if binding.permits(Capability::McpStatus) {
                        read_status = true;
                    }
                }
                ObservedFact::Usage(report) => {
                    if !account_fresh{continue}
                    flush(&mut commands, &mut technical);
                    let Some(provider) = alondra_core::accounts::Provider::from_driver(binding.driver()) else { continue };
                    let account = crate::usage_probe::account_of(binding.credential());
                    let reading = usage_reading(report);
                    let at = alondra_core::now_millis();
                    if !reading.is_empty() && reading.check().is_ok() && self.usage_changes(provider, &account, &reading, at) {
                        let day = crate::node_reports::local_day(at);
                        commands.push(Command::RecordAccountUsage { provider, account, reading, at, day });
                    }
                }
            }
        }
        flush(&mut commands, &mut technical);
        if read_status {
            commands.push(Command::ReadMcpStatus { task_id: task_id.clone() });
        }
        for (index, command) in commands.into_iter().enumerate() {
            if let Command::RecordDecisionSettled { epoch, .. } | Command::RecordDecisionAborted { epoch, .. } = &command {
                let ended = self.draft.state.lifecycle_journal.get(&task_id).is_some_and(|records| records.iter().rev()
                    .take_while(|record| !matches!(record, alondra_core::domain::LifecycleRecord::Admitted { .. }))
                    .any(|record| matches!(record, alondra_core::domain::LifecycleRecord::DecisionSettled { epoch: known } | alondra_core::domain::LifecycleRecord::DecisionAborted { epoch: known, .. } if known == epoch)));
                if ended {
                    reconciled = true;
                    continue;
                }
            }
            let command_id=if let Some((work,activation))=continuations.get(&index){
                let mut live=false;
                for record in self.draft.state.lifecycle_journal.get(&task_id).into_iter().flatten(){
                    match record {
                        alondra_core::domain::LifecycleRecord::Admitted{..}|alondra_core::domain::LifecycleRecord::HostEnded{..}|alondra_core::domain::LifecycleRecord::HostStopped{..}=>live=false,
                        alondra_core::domain::LifecycleRecord::WorkStarted{work:found,..} if found==work.as_str()=>live=true,
                        alondra_core::domain::LifecycleRecord::WorkEnded{work:found,..} if found==work.as_str()=>live=false,
                        _=>{},
                    }
                }
                let lifecycle=self.draft.state.lifecycle_of(&task_id)?;
                live &= lifecycle.execution()==Some(&execution) && matches!(lifecycle.phase(),alondra_domain::TaskPhase::InProgress|alondra_domain::TaskPhase::Delivered);
                if !live {
                    if !probe{return Err(DomainError::new("native_work_mismatch","The native continuation has no matching live work").into())}
                    log!("the native continuation of {task_id} has no matching live work: it commits without another input");
                    continue;
                }
                use sha2::Digest;
                let identity=serde_json::to_vec(&(task_id.0.as_str(),generation,work.as_str(),activation)).map_err(|error|StoreError::Storage(error.into()))?;
                let digest=sha2::Sha256::digest(identity);let key:String=digest.iter().map(|byte|format!("{byte:02x}")).collect();
                format!("native-work-followup-{key}")
            }else{format!("observation:{}:{}:{}:{index}", execution.task(), execution.generation(), batch.cursor)};
            let envelope = CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: self.draft.state.data_epoch.clone(),
                command_id: command_id.as_str().into(),
                expected_revision: 0,
                command,
            };
            if matches!(envelope.command, Command::ReadMcpStatus { .. })
                && self.draft.state.guard(&Actor::System, &CommandEnvelope { expected_revision: self.draft.state.revision, ..envelope.clone() }).is_err()
            {
                continue;
            }
            // What Alondra cannot record never stops the node's owner or its host: a command the
            // domain refuses changed nothing, and the rest of the observation still commits.
            if !probe {
                if let Some(refusal) = refused_for_test(&envelope.command, false) {
                    return Err(refusal);
                }
                let changed = self.stage_command(&Actor::System, &envelope, Attached::None)?.changed;
                reconciled |= !changed && matches!(envelope.command, Command::RecordDecisionRequest { .. });
                continue;
            }
            // What Alondra cannot record never stops the node's owner or its host: each command is
            // first decided on a copy, and one the domain refuses is left out of the observation.
            let Some(refusal) = self.refusal(&envelope) else {
                self.stage_command(&Actor::System, &envelope, Attached::None)?;
                continue;
            };
            if continuations.contains_key(&index) {
                log!("the domain refused the native work continuation of {task_id}: {}: the observation commits without another input",refusal.message);
                continue;
            }
            if let Command::RecordTurnBoundary { task_id, generation, turn, failure, failed } = &envelope.command {
                let technical = CommandEnvelope {
                    command_id: format!("{}:observed-end", envelope.command_id.0).as_str().into(),
                    command: Command::RecordRuntimeFacts {
                        task_id: task_id.clone(), generation: *generation,
                        facts: vec![RuntimeObservation::TurnEnded { turn: turn.clone(), failure: failure.clone(), failed: failed.clone() }],
                    },
                    ..envelope.clone()
                };
                if self.refusal(&technical).is_none() {
                    self.stage_command(&Actor::System, &technical, Attached::None)?;
                    log!("the end of {turn} on {task_id} is recorded without its refused reaction: {}", refusal.message);
                    continue;
                }
            }
            if let Command::RecordDecisionRequest { task_id, generation, epoch, .. } = &envelope.command {
                let denial = CommandEnvelope {
                    command_id: format!("{}:refused", envelope.command_id.0).as_str().into(),
                    command: Command::RefuseDecisionRequest { task_id: task_id.clone(), generation: *generation, epoch: *epoch, reason: refusal.message.clone() },
                    ..envelope.clone()
                };
                if self.refusal(&denial).is_none() {
                    self.stage_command(&Actor::System, &denial, Attached::None)?;
                    log!("Claude's request {epoch} on task {task_id} could not be asked ({}): it is denied with why", refusal.message);
                    continue;
                }
            }
            let what = format!("{} at {}: {}", command_name(&envelope.command), batch.cursor, refusal.message);
            log!("the domain refused what the SDK host of {task_id} observed, {what}: the observation commits without it");
            if moves_lifecycle(&envelope.command) {
                self.unrecorded.push(what);
            }
        }
        if account_fresh && binding.driver() == "codex-app-server" {
            if let Some(message) = alondra_provider_codex::native_message(batch) {
                let account = alondra_core::accounts::AccountKey::new(alondra_core::accounts::Provider::Codex, crate::usage_probe::account_of(binding.credential())).wire_id();
                self.journal.inspect(|| super::codex_metrics::stage(self.conn, &account, &task_id.0, &message, alondra_core::now_millis()).map_err(|error| StoreError::Storage(error.into())))?;
            }
        }
        let accepted = if !probe && !reconciled {
            pending.settle(&mut self.journal).map_err(application_error)?
        } else {
            let moved = match (before, cursor(&self.journal)?) { (Some(before), Some(after)) => after.saturating_sub(before), _ => 0 };
            pending.settle_recorded(&mut self.journal, usize::try_from(moved).unwrap_or(usize::MAX)).map_err(application_error)?
        };
        self.observe_provider_transition(&execution, batch.cursor)?;
        Ok(accepted)
    }

    /// Why the domain would refuse `envelope` now, decided on a copy of this transaction's state.
    pub(crate) fn refusal(&self, envelope: &CommandEnvelope) -> Option<DomainError> {
        if let Some(StoreError::Domain(refusal)) = refused_for_test(&envelope.command, true) {
            return Some(refusal);
        }
        let mut probe = DomainState::clone(&self.draft.state);
        let envelope = CommandEnvelope { expected_revision: probe.revision, ..envelope.clone() };
        probe.execute(&Actor::System, &envelope).err()
    }

    /// Whether `reading` says something new of `account`: the same windows and plan again within
    /// a few minutes change nothing, and a provider event repeats them on every request.
    fn usage_changes(&self, provider: alondra_core::accounts::Provider, account: &alondra_core::accounts::AccountId, reading: &UsageReading, at: alondra_core::Millis) -> bool {
        const FRESH: alondra_core::Millis = 5 * 60_000;
        let Some(known) = self.draft.state.account_usage.iter().find(|usage| usage.provider == provider && usage.account == *account) else { return true };
        let reread = reading.plan.as_ref().is_none_or(|plan| known.plan.as_ref() == Some(plan))
            && reading.windows.iter().all(|read| known.window(&read.kind).is_some_and(|window| window.reading == *read && at - window.measured_at < FRESH));
        !reread
    }

    /// Whether `servers` changes what the task's node holds of its MCP servers: init repeats them
    /// as each prompt starts, and a repeat changes nothing.
    fn mcp_changes(&self, task: &TaskId, generation: u64, servers: &[McpServerState]) -> bool {
        let state = &self.draft.state;
        let node = state.tasks.get(task).and_then(|task| task.agent_id.as_ref()).and_then(|agent| state.mcp.get(agent));
        match node {
            Some(node) if node.generation == generation => node.merged(servers) != node.servers,
            _ => !servers.is_empty(),
        }
    }

    /// Runs `step` on this transaction's connection inside the journal's guard.
    pub(super) fn sql<T>(&mut self, step: impl FnOnce(&Connection) -> anyhow::Result<T>) -> std::result::Result<T, StoreError> {
        let conn = self.conn;
        self.journal.inspect(|| step(conn).map_err(StoreError::Storage))
    }

    /// After `AdmitExecution`, what launches it, in the same transaction: the task's worktree,
    /// any unbound launch text, every pending input's provider metadata and FIFO dispatch, and
    /// the owner's reservation. Bound input payloads remain immutable; all are committed before offer.
    pub(crate) fn launch(
        &mut self,
        task: &TaskId,
        text: &str,
        directory: &str,
        binding: &ProviderBinding,
        provider: &mut impl Preparer,
    ) -> std::result::Result<ExecutionStamp, StoreError> {
        let conn = self.conn;
        self.journal.inspect(|| {
            repository::exec(
                conn,
                "INSERT INTO task_worktrees(task_id,dir,at) VALUES(?1,?2,?3) ON CONFLICT(task_id) DO UPDATE SET dir=excluded.dir,at=excluded.at",
                params![task.0, directory, alondra_core::now_millis()],
            )
        })?;
        let pending: Vec<_> = self.journal.load(task)?.pending_inputs().into_iter().cloned().collect();
        if pending.is_empty() { return Err(alondra_store::Error::MissingInput.into()); }
        self.journal.bind_provider(task, binding)?;
        let execution = self.journal.load(task)?.execution().cloned().ok_or(alondra_store::Error::MissingIntent)?;
        for (at, input) in pending.iter().enumerate() {
            let has_text: bool = self.sql(|connection| Ok(connection.query_row(
                "SELECT body IS NOT NULL FROM lifecycle_inputs WHERE task_id=?1 AND input_id=?2",
                params![task.0, input.as_str()], |row| row.get(0),
            )?))?;
            if !has_text {
                if at != 0 { return Err(alondra_store::Error::MissingInput.into()); }
                self.journal.give_input_text(task, InputPayload::new(input.clone(), text.to_owned())?)?;
            }
            InputMetadata::with_execution(&mut self.journal, &execution, input, |metadata| provider.stage_input(metadata))
                .map_err(application_error)?;
            if at == 0 {
                self.journal.stage_launch(task, &Placement::LocalDirectory(directory.to_owned()), binding)?;
            } else {
                self.journal.stage_input(task, input)?;
            }
        }
        self.journal.reserve_owner(&execution, 0)?;
        if std::path::Path::new(directory).join(".git").is_file() {
            let envelope = CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: self.draft.state.data_epoch.clone(),
                command_id: format!("launch-worktree:{task}:{}", execution.generation()).as_str().into(),
                expected_revision: 0,
                command: Command::RecordTaskWorktree { task_id: task.clone(), path: directory.to_owned(), origin: WorktreeOrigin::Task, branch: None },
            };
            self.stage_command(&Actor::System, &envelope, Attached::None)?;
        }
        Ok(execution)
    }

    pub(crate) fn adapter_state(&self, grant: &OwnerGrant) -> std::result::Result<AdapterState, StoreError> {
        Ok(self.journal.adapter_state(grant)?)
    }

    /// Takes the task's current execution with the next owner generation, for a daemon that is
    /// adopting a host a previous one left running. The commit issues the grant.
    pub(crate) fn adopt(&mut self, task: &TaskId) -> std::result::Result<Adoption, StoreError> {
        let stamp = self.journal.load(task)?.execution().cloned().ok_or(alondra_store::Error::MissingIntent)?;
        let current = self.journal.owner_generation(&stamp)?;
        self.journal.reserve_owner(&stamp, current)?;
        let commands = self.journal.unresolved_commands(task)?;
        let controls = self.journal.unresolved_controls(task)?;
        let unrecorded_controls = self.journal.unrecorded_controls(task)?;
        let unrecorded_commands = self.journal.unrecorded_commands(task)?;
        let conn = self.conn;
        let directory = self.journal.inspect(|| {
            conn.query_row("SELECT dir FROM task_worktrees WHERE task_id=?1", rusqlite::params![task.0], |row| row.get::<_, String>(0))
                .map_err(|error| StoreError::Storage(error.into()))
        })?;
        Ok(Adoption { stamp, directory, commands, controls, unrecorded_controls, unrecorded_commands })
    }

    /// The commit issues again the permit of the task's next unclaimed dispatch.
    pub(crate) fn reissue_permit(&mut self, task: &TaskId) -> std::result::Result<(), StoreError> {
        Ok(self.journal.reissue_permit(task)?)
    }

    /// Resolves a claimed dispatch with the host's answer, fenced by the generation it was
    /// reconciled under. It never invents consumption.
    pub(crate) fn resolve(&mut self, stamp: &ExecutionStamp, intent: i64, owner_generation: u64, receipt: DispatchReceipt) -> std::result::Result<(), StoreError> {
        Ok(self.journal.resolve_dispatch(stamp, intent, owner_generation, receipt)?)
    }

    /// The task's inputs that left for a host and were not consumed, with how their handoff ended.
    pub(crate) fn pending_handoffs(&self, task: &TaskId) -> std::result::Result<Vec<(InputPayload, String)>, StoreError> {
        Ok(self.journal.pending_handoffs(task)?)
    }

    pub(crate) fn normalization_checkpoint(&self, stamp: &alondra_foundation::ExecutionStamp) -> std::result::Result<Option<alondra_store::NormalizationCheckpoint>, StoreError> {
        Ok(self.journal.normalization_checkpoint(stamp)?)
    }

    pub(crate) fn lifecycle(&self, task: &TaskId) -> std::result::Result<std::sync::Arc<TaskLifecycle>, StoreError> {
        Ok(self.journal.load(task)?)
    }

    /// The commit hands back the attempt; a permit some later commit made stale is refused.
    pub(crate) fn claim(&mut self, permit: DispatchPermit) -> std::result::Result<(), StoreError> {
        Ok(self.journal.claim(permit)?)
    }

    /// The provider's metadata for the attempt's input, the command it prepares from it and that
    /// command's evidence, under the owner's generation. The commit hands back the command.
    pub(crate) fn record_command(
        &mut self,
        attempt: DispatchAttempt,
        owner_generation: u64,
        provider: &mut impl Preparer,
    ) -> std::result::Result<(), StoreError> {
        InputMetadata::with_attempt(&mut self.journal, &attempt, |metadata| provider.stage_input(metadata))
            .map_err(application_error)?;
        let command = InputMetadata::with_attempt(&mut self.journal, &attempt, |metadata| provider.prepare(&attempt, metadata))
            .map_err(application_error)?;
        let evidence = CommandEvidence::new(command.bytes().to_vec())?;
        Ok(self.journal.record_owned_command(attempt, owner_generation, evidence)?)
    }

    pub(crate) fn record_receipt(&mut self, command: CommittedCommand, receipt: DispatchReceipt) -> std::result::Result<(), StoreError> {
        Ok(self.journal.record_committed_receipt(command, receipt)?)
    }

    /// The commit hands back the attempt; `false` when the control was revoked instead, an
    /// interrupt whose turn or work already ended. A permit some later commit made stale is refused.
    pub(crate) fn require_native_identity(&self,binding:&ProviderBinding,expected:Option<&crate::sdk_owner::NativeAccountIdentity>)->std::result::Result<(),StoreError>{
        let Some(expected)=expected else{return Ok(())};
        let Some(provider)=alondra_core::accounts::Provider::from_driver(binding.driver()) else{return Ok(())};
        let id=crate::usage_probe::account_of(binding.credential());
        let account=self.state().accounts.iter().find(|account|account.provider==provider && account.id==id);
        if let Some((account,change))=account.and_then(|account|expected.change(account).map(|change|(account,change))){
            return Err(if change=="identity"{DomainError::new("native_account_changed",format!("the account {} now has a different known identity. Sign that account in again with the identity this session started on, then try again, or cancel the task",account.name))}else{DomainError::new("native_billing_policy_changed","Billing policy changed for this account. Restore the original policy or create a new node with an explicit paid account selection")}.into())
        }
        Ok(())
    }
    pub(crate) fn claim_control_for_identity(&mut self,permit:ControlPermit,directory:&std::path::Path)->std::result::Result<bool,StoreError>{
        let kind=self.conn.query_row("SELECT kind,argument FROM lifecycle_controls WHERE id=?1",[permit.id()],|row|Ok((row.get::<_,String>(0)?,row.get::<_,Option<String>>(1)?))).optional().map_err(|error|StoreError::Storage(error.into()))?;
        let Some((kind,argument))=kind else{return Ok(false)};
        let safe=matches!(kind.as_str(),"interrupt"|"force_stop") || (kind=="respond" && argument.as_deref().and_then(|argument|serde_json::from_str::<serde_json::Value>(argument).ok()).and_then(|value|value.get("kind").and_then(serde_json::Value::as_str).map(str::to_owned)).is_some_and(|kind|matches!(kind.as_str(),"deny"|"decline")));
        if !safe{
            let expected=crate::sdk_owner::NativeAccountIdentity::load(directory).map_err(|error|StoreError::Storage(anyhow::anyhow!("{error}")))?;
            let binding=self.execution_binding(permit.execution())?;self.require_native_identity(&binding,expected.as_ref())?;
        }
        self.claim_control(permit)
    }
    pub(crate) fn claim_control(&mut self, permit: ControlPermit) -> std::result::Result<bool, StoreError> {
        Ok(self.journal.claim_control(permit)?)
    }

    /// The exact command the provider prepares for a claimed control, recorded under the owner's
    /// generation once the claim still holds. The commit hands back the command.
    pub(crate) fn record_control(
        &mut self,
        attempt: ControlAttempt,
        owner_generation: u64,
        prepare: impl FnOnce(&ControlAttempt) -> std::result::Result<PreparedCommand, AdapterError>,
    ) -> std::result::Result<(), StoreError> {
        self.journal.fence_control(&attempt)?;
        let command = prepare(&attempt).map_err(|error| StoreError::Storage(anyhow::anyhow!("the control's command was not prepared: {error:?}")))?;
        let evidence = CommandEvidence::new(command.bytes().to_vec())?;
        Ok(self.journal.record_owned_control(attempt, owner_generation, evidence)?)
    }

    pub(crate) fn record_control_receipt(&mut self, control: CommittedControl, receipt: DispatchReceipt) -> std::result::Result<(), StoreError> {
        Ok(self.journal.record_control_receipt(control, receipt)?)
    }

    /// Resolves a claimed control with the host's answer, fenced like `resolve`.
    pub(crate) fn resolve_control(&mut self, stamp: &ExecutionStamp, id: i64, owner_generation: u64, receipt: DispatchReceipt) -> std::result::Result<(), StoreError> {
        Ok(self.journal.resolve_control(stamp, id, owner_generation, receipt)?)
    }

    /// One bounded pass of the raw evidence retention.
    pub(crate) fn retain_evidence(&mut self, limit: usize) -> std::result::Result<alondra_store::EvidenceRetention, StoreError> {
        Ok(self.journal.retain_evidence(limit)?)
    }

    /// Lifecycle fixtures for states no typed owner step produces yet.
    #[cfg(any(test, feature = "test-hooks"))]
    pub(super) fn journal(&mut self) -> &mut LifecycleTransaction<'connection> {
        &mut self.journal
    }

    fn commit(self) -> std::result::Result<OwnerCommit, StoreError> {
        let technical = self.journal.commit()?;
        Ok(OwnerCommit {
            draft: self.draft,
            technical,
        })
    }
}

impl OwnerDraft {
    fn record(&mut self, command: &Command, command_id: &CommandId, commit: Commit) {
        let fresh = !commit.replayed && commit.outcome.changed;
        if fresh {
            self.state = commit.state;
        }
        self.domain_changed |= fresh;
        self.schedule_changed |= fresh && crate::domain_runtime::schedule_command(command);
        self.outcomes.push(StagedOutcome {
            command_id: command_id.clone(),
            outcome: commit.outcome,
            replayed: commit.replayed,
        });
    }
}

impl DomainStore {
    /// Runs `stage` in one SQL transaction and commits it. Nothing is committed or cached when
    /// `stage` fails, when any step failed even if `stage` caught it, or when the commit fails.
    pub(crate) fn own<T>(
        &mut self,
        scope: AuthorityScope,
        stage: impl FnOnce(&mut OwnerTransaction<'_>) -> std::result::Result<T, StoreError>,
    ) -> std::result::Result<(T, OwnerCommit), StoreError> {
        let (value, committed) = {
            let mut owner = OwnerTransaction::begin(&self.conn, &self.cached, &self.aggregates, scope)?;
            let value = stage(&mut owner)?;
            (value, owner.commit()?)
        };
        self.remember(Arc::clone(&committed.draft.state), committed.draft.version);
        Ok((value, committed))
    }
    /// One bounded pass of the raw evidence retention, in its own transaction.
    pub(crate) fn retain_evidence(&mut self, limit: usize) -> std::result::Result<alondra_store::EvidenceRetention, StoreError> {
        let scope = AuthorityScope::new(epoch_of(&self.conn)?)?;
        Ok(self.own(scope, |owner| owner.retain_evidence(limit))?.0)
    }
    /// Gives the disk back up to `pages` of the pages the retention freed, one per step of the
    /// pragma. A database created without incremental vacuum is rebuilt once to get it, and one
    /// whose rows retention shrank in place once they are all done; its write-ahead log is
    /// truncated after the rebuild.
    pub(crate) fn reclaim_space(&self, pages: u32) -> Result<()> {
        let before = self.allocated_space();
        let reclaimed = (|| -> Result<()> {
            let mode: i64 = self.conn.pragma_query_value(None, "auto_vacuum", |row| row.get(0))?;
            if mode == 0 || self.owes_rebuild()? {
                self.conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL; VACUUM;")?;
                self.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
                self.rebuilt()?;
            } else {
                let mut vacuum = self.conn.prepare(&format!("PRAGMA incremental_vacuum({pages})"))?;
                let mut freed = vacuum.query([])?;
                while freed.next()?.is_some() {}
            }
            Ok(())
        })();
        match before.zip(self.allocated_space()) {
            Some((before, after)) => log!("store disk recovery: allocated before {before} bytes, after {after} bytes, released {} bytes, grew {} bytes", before.saturating_sub(after), after.saturating_sub(before)),
            None => log!("store disk recovery: allocated space unavailable"),
        }
        reclaimed
    }

    fn allocated_space(&self) -> Option<u64> {
        use std::os::unix::fs::MetadataExt;
        let path = self.conn.path().filter(|path| !path.is_empty())?;
        let main = std::fs::metadata(path).ok()?.blocks().saturating_mul(512);
        let wal = match std::fs::metadata(format!("{path}-wal")) {
            Ok(metadata) => metadata.blocks().saturating_mul(512),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(_) => return None,
        };
        Some(main.saturating_add(wal))
    }
}

#[cfg(test)]
#[path = "owner_tests.rs"]
pub(super) mod tests;

fn mcp_server(report: &McpServerReport) -> McpServerState {
    McpServerState {
        name: report.name.clone(),
        status: McpStatus::of(&report.status),
        error: report.error.clone(),
        source: report.source.clone(),
        transport: report.transport.clone(),
        url: report.url.clone(),
        traits: report.traits,
    }
}

/// What the provider said of its account's usage, as the domain records it: a partial reading.
fn usage_reading(report: &alondra_provider_contract::UsageReport) -> UsageReading {
    use alondra_provider_contract::UsageWindowKind;
    UsageReading {
        windows: report.windows.iter().map(|window| WindowReading {
            kind: match &window.kind {
                UsageWindowKind::Session => WindowKind::Session,
                UsageWindowKind::Weekly => WindowKind::Weekly,
                UsageWindowKind::Other(name) => WindowKind::Other(name.clone()),
            },
            used: Share::of(f64::from(window.used) / 10_000.0),
            resets_at: window.resets_at,
            exhausted: window.exhausted,
        }).collect(),
        plan: report.plan.clone(),
        complete: false,
    }
}
