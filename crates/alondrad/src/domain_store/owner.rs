//! The one SQL transaction that stages domain commands, provider observations and the lifecycle
//! journal, and commits them together. Only `LifecycleTransaction::commit` commits it, and only
//! that commit issues the store's seals.
use super::*;
use alondra_agent_application::{InputMetadata, ObservationNormalizer, ObservedFact};
use alondra_domain::{RuntimeFact, TaskLifecycle, Trigger};
use alondra_foundation::ExecutionStamp;
use alondra_provider_contract::{AdapterError, OrderedObservation, Placement, PreparedCommand, ProviderBinding};
use alondra_store::{
    AdapterState, AuthorityScope, CommandEvidence, Committed, CommittedCommand, DispatchAttempt, DispatchPermit,
    DispatchReceipt, InputPayload, LifecycleTransaction, ObservationAcceptance, OwnerGrant,
};

/// The provider's part of a dispatch: its metadata for an input, and the exact command it sends.
pub(crate) trait Preparer {
    fn stage_input(&mut self, metadata: InputMetadata<'_, '_>) -> std::result::Result<(), AdapterError>;
    fn prepare(&mut self, attempt: &DispatchAttempt, metadata: InputMetadata<'_, '_>) -> std::result::Result<PreparedCommand, AdapterError>;
}

/// Every step runs inside the journal's guard: an error or a panic in any of them poisons the
/// whole transaction, even when the caller catches it, so it can only roll back.
pub(crate) struct OwnerTransaction<'connection> {
    conn: &'connection Connection,
    cached: &'connection Option<Cached>,
    journal: LifecycleTransaction<'connection>,
    draft: OwnerDraft,
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
        RuntimeFact::InputConsumed { input } => RuntimeObservation::InputConsumed { input: input.as_str().into() },
        RuntimeFact::WorkStarted { turn, work } => RuntimeObservation::WorkStarted { turn: turn.as_str().into(), work: work.as_str().into() },
        RuntimeFact::WorkEnded { work, continuation } => {
            RuntimeObservation::WorkEnded { work: work.as_str().into(), continuation: continuation.as_str().into() }
        }
        other => return Err(StoreError::Storage(anyhow::anyhow!("provider observation refused: the domain does not record {other:?} yet"))),
    })
}
fn application_error(error: alondra_agent_application::Error) -> StoreError {
    match error {
        alondra_agent_application::Error::Store(error) => error.into(),
        other => StoreError::Storage(anyhow::anyhow!("provider observation refused: {other:?}")),
    }
}

impl<'connection> OwnerTransaction<'connection> {
    fn begin(
        conn: &'connection Connection,
        cached: &'connection Option<Cached>,
        scope: AuthorityScope,
    ) -> std::result::Result<Self, StoreError> {
        let transaction = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let journal = LifecycleTransaction::attach(transaction, scope)?;
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
        })
    }

    /// The domain's own guarded command, decided against this transaction's current state.
    /// Documents are refused: their content needs the verified store and lease of `execute_with`.
    pub(crate) fn stage_command(
        &mut self,
        actor: &Actor,
        envelope: &CommandEnvelope,
        attached: Attached<'_>,
    ) -> std::result::Result<&CommandOutcome, StoreError> {
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
    /// the end of the root turn by RecordTurnBoundary. Facts the domain does not record yet (a
    /// lost connection, a released resource) refuse the whole observation.
    pub(crate) fn stage_observation<N: ObservationNormalizer>(
        &mut self,
        binding: &ProviderBinding,
        owner_generation: u64,
        batch: &OrderedObservation,
        normalizer: &N,
    ) -> std::result::Result<ObservationAcceptance, StoreError> {
        let pending = alondra_agent_application::stage_observation(
            &mut self.journal,
            binding,
            owner_generation,
            batch,
            normalizer,
        )
        .map_err(application_error)?;
        let execution = pending.execution().clone();
        let mut commands = vec![];
        let mut technical = vec![];
        for fact in pending.facts() {
            match fact {
                ObservedFact::Runtime(fact) => technical.push(self.journal.inspect(|| observed(fact))?),
                ObservedFact::TurnEnded(turn) => {
                    if !technical.is_empty() {
                        commands.push(Command::RecordRuntimeFacts { task_id: execution.task().clone(), generation: execution.generation(), facts: std::mem::take(&mut technical) });
                    }
                    commands.push(Command::RecordTurnBoundary { task_id: execution.task().clone(), generation: execution.generation(), turn: turn.as_str().into() });
                }
            }
        }
        if !technical.is_empty() {
            commands.push(Command::RecordRuntimeFacts { task_id: execution.task().clone(), generation: execution.generation(), facts: technical });
        }
        for (index, command) in commands.into_iter().enumerate() {
            let envelope = CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: self.draft.state.data_epoch.clone(),
                command_id: format!("observation:{}:{}:{}:{index}", execution.task(), execution.generation(), batch.cursor).as_str().into(),
                expected_revision: 0,
                command,
            };
            self.stage_command(&Actor::System, &envelope, Attached::None)?;
        }
        pending.settle(&mut self.journal).map_err(application_error)
    }

    /// After `AdmitExecution`, what launches it, in the same transaction: the task's worktree,
    /// the text of its first input with the provider's metadata for it, the launch's dispatch in
    /// that worktree and the owner's reservation. The commit issues the launch permit and grant.
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
        let first = self.journal.load(task)?.pending_inputs().first().map(|input| (*input).clone()).ok_or(alondra_store::Error::MissingInput)?;
        self.journal.give_input_text(task, InputPayload::new(first.clone(), text.to_owned())?)?;
        self.journal.bind_provider(task, binding)?;
        let execution = self.journal.load(task)?.execution().cloned().ok_or(alondra_store::Error::MissingIntent)?;
        InputMetadata::with_execution(&mut self.journal, &execution, &first, |metadata| provider.stage_input(metadata))
            .map_err(application_error)?;
        self.journal.stage_launch(task, &Placement::LocalDirectory(directory.to_owned()), binding)?;
        self.journal.reserve_owner(&execution, 0)?;
        Ok(execution)
    }

    pub(crate) fn adapter_state(&self, grant: &OwnerGrant) -> std::result::Result<AdapterState, StoreError> {
        Ok(self.journal.adapter_state(grant)?)
    }

    /// Takes the task's current execution with the next owner generation, for a daemon that is
    /// adopting a host a previous one left running. The commit issues the grant; the returned
    /// dispatches are the ones a crash left claimed, with the exact bytes to re-send to the host.
    pub(crate) fn adopt(&mut self, task: &TaskId) -> std::result::Result<(ExecutionStamp, String, Vec<alondra_store::UnresolvedCommand>), StoreError> {
        let stamp = self.journal.load(task)?.execution().cloned().ok_or(alondra_store::Error::MissingIntent)?;
        let current = self.journal.owner_generation(&stamp)?;
        self.journal.reserve_owner(&stamp, current)?;
        let unresolved = self.journal.unresolved_commands(task)?;
        let conn = self.conn;
        let directory = self.journal.inspect(|| {
            conn.query_row("SELECT dir FROM task_worktrees WHERE task_id=?1", rusqlite::params![task.0], |row| row.get::<_, String>(0))
                .map_err(|error| StoreError::Storage(error.into()))
        })?;
        Ok((stamp, directory, unresolved))
    }

    /// Resolves a claimed dispatch with the host's answer, fenced by the generation it was
    /// reconciled under. It never invents consumption.
    pub(crate) fn resolve(&mut self, stamp: &ExecutionStamp, intent: i64, owner_generation: u64, receipt: DispatchReceipt) -> std::result::Result<(), StoreError> {
        Ok(self.journal.resolve_dispatch(stamp, intent, owner_generation, receipt)?)
    }

    pub(crate) fn lifecycle(&self, task: &TaskId) -> std::result::Result<TaskLifecycle, StoreError> {
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

    /// Lifecycle fixtures for states no typed owner step produces yet.
    #[cfg(test)]
    fn journal(&mut self) -> &mut LifecycleTransaction<'connection> {
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
            let mut owner = OwnerTransaction::begin(&self.conn, &self.cached, scope)?;
            let value = stage(&mut owner)?;
            (value, owner.commit()?)
        };
        self.remember(Arc::clone(&committed.draft.state), committed.draft.version);
        Ok((value, committed))
    }
}

#[cfg(test)]
#[path = "owner_tests.rs"]
mod tests;
