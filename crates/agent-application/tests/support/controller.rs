//! Test-only SQL owner used by component fixtures; no production coordinator or publication claim.
use alondra_agent_application::{
    DurableAdapter, Error, InputMetadata, PendingObservation, invocation, stage_observation,
};
use alondra_domain::{Event, TaskLifecycle};
use alondra_foundation::TaskId;
use alondra_provider_contract::{Handoff, OrderedObservation, Placement};
use alondra_store::{
    AuthorityScope, CommandEvidence, DispatchAttempt, DispatchPermit, DispatchReceipt,
    InputPayload, LifecycleTransaction,
};
use rusqlite::{Connection, TransactionBehavior};
use std::collections::VecDeque;
type Result<T> = std::result::Result<T, alondra_agent_application::Error>;

/// What the domain does with an accepted observation's facts, for fixtures that run the store
/// without it: each technical fact becomes the next runtime event of the execution, then the
/// observation is settled. The end of a turn and a decision need the domain's, so they are refused; that
/// the model answered, or how the MCP servers are, moves no task, so nothing records it.
pub fn record_facts(
    transaction: &mut LifecycleTransaction<'_>,
    pending: PendingObservation,
) -> Result<alondra_store::ObservationAcceptance> {
    use alondra_agent_application::ObservedFact;
    let stamp = pending.execution().clone();
    for (index, fact) in pending.facts().iter().enumerate() {
        let fact = match fact {
            ObservedFact::Runtime(fact) => fact,
            ObservedFact::ModelAnswered
            | ObservedFact::Worktree { .. }
            | ObservedFact::SubagentWorktree { .. }
            | ObservedFact::PullRequestOpened(_)
            | ObservedFact::PullRequestNotNamed
            | ObservedFact::McpServers { .. }
            | ObservedFact::McpSignIn { .. }
            | ObservedFact::McpToolFailed
            | ObservedFact::Usage(_) => continue,
            ObservedFact::ContinuationRequested { .. } | ObservedFact::TurnEnded { .. } | ObservedFact::InputDropped { .. } | ObservedFact::Asked { .. } | ObservedFact::Settled { .. } => {
                return Err(Error::RequiresDomainCommand);
            }
        };
        let aggregate = transaction.load(stamp.task())?;
        let cursor = aggregate.cursor().unwrap_or(0) + 1;
        transaction.apply(
            stamp.task(),
            aggregate.revision(),
            &format!("fixture-fact-{}-{cursor}-{index}", stamp.generation()),
            Event::Runtime { stamp: stamp.clone(), cursor, fact: fact.clone() },
        )?;
    }
    pending.settle(transaction)
}
/// Owns the submitting adapter and serializes its command/observation lane with mutable access.
/// The adapter must fence its external effect against its committed owner grant as well.
pub struct WorkerController<A: DurableAdapter> {
    connection: Connection,
    scope: AuthorityScope,
    adapter: A,
    pending: Option<OrderedObservation>,
    permits: VecDeque<DispatchPermit>,
    task: Option<TaskId>,
}
impl<A: DurableAdapter> WorkerController<A> {
    pub fn open(mut connection: Connection, scope: AuthorityScope, adapter: A) -> Result<Self> {
        let mut transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(alondra_store::Error::from)?;
        LifecycleTransaction::install(&mut transaction)?;
        transaction.commit().map_err(alondra_store::Error::from)?;
        Ok(Self {
            connection,
            scope,
            adapter,
            pending: None,
            permits: VecDeque::new(),
            task: None,
        })
    }
    /// The core Task must already exist; this does not create a pre-Task orchestrator owner.
    pub fn launch(
        &mut self,
        task: &TaskId,
        input: InputPayload,
        admission: Event,
        placement: Placement,
    ) -> Result<()> {
        if self.task.is_some() || !matches!(admission, Event::Admit { .. }) {
            return Err(Error::BindingMismatch);
        }
        let raw = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(alondra_store::Error::from)?;
        let mut transaction = LifecycleTransaction::attach(raw, self.scope.clone())?;
        let input_id = input.id().clone();
        transaction.create(task, input)?;
        transaction.apply(task, 0, "worker-admission", admission)?;
        transaction.bind_provider(task, self.adapter.binding())?;
        let execution = transaction
            .load(task)?
            .execution()
            .cloned()
            .ok_or(alondra_store::Error::MissingIntent)?;
        InputMetadata::with_execution(&mut transaction, &execution, &input_id, |metadata| {
            self.adapter.stage_input(metadata)
        })?;
        transaction.stage_launch(task, &placement, self.adapter.binding())?;
        transaction.reserve_owner(&execution, 0)?;
        let mut committed = transaction.commit()?;
        let owner = committed
            .owners
            .pop()
            .ok_or(alondra_store::Error::StaleOwner)?;
        let raw = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(alondra_store::Error::from)?;
        let transaction = LifecycleTransaction::attach(raw, self.scope.clone())?;
        let state = transaction.adapter_state(&owner)?;
        transaction.commit()?;
        self.task = Some(task.clone());
        self.adapter.install_owner(owner, state)?;
        self.permits.extend(committed.permits);
        Ok(())
    }
    pub fn next_permit(&mut self) -> Option<DispatchPermit> {
        self.permits.pop_front()
    }
    pub fn claim(&mut self, permit: DispatchPermit) -> Result<DispatchAttempt> {
        let raw = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(alondra_store::Error::from)?;
        let mut transaction = LifecycleTransaction::attach(raw, self.scope.clone())?;
        transaction.claim(permit)?;
        transaction
            .commit()?
            .attempts
            .pop()
            .ok_or(alondra_store::Error::MissingIntent.into())
    }
    pub fn dispatch(&mut self, attempt: DispatchAttempt) -> Result<Handoff> {
        if attempt.binding() != self.adapter.binding() {
            return Err(Error::BindingMismatch);
        }
        let owner_generation = self.adapter.owner_generation()?;
        let raw = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(alondra_store::Error::from)?;
        let mut transaction = LifecycleTransaction::attach(raw, self.scope.clone())?;
        transaction.fence_attempt(&attempt)?;
        transaction.fence_observation_owner(attempt.execution(), owner_generation)?;
        let native = InputMetadata::with_attempt(&mut transaction, &attempt, |metadata| {
            self.adapter.load_input(metadata)
        })?;
        let command = self.adapter.prepare(invocation(&attempt), native)?;
        transaction.record_command(&attempt, CommandEvidence::new(command.bytes().to_vec())?)?;
        transaction.commit()?;
        let raw = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(alondra_store::Error::from)?;
        let transaction = LifecycleTransaction::attach(raw, self.scope.clone())?;
        transaction.fence_attempt(&attempt)?;
        transaction.fence_observation_owner(attempt.execution(), owner_generation)?;
        transaction.commit()?;
        // SQLite is no longer held while the adapter waits on external I/O.
        let handoff = self
            .adapter
            .submit(invocation(&attempt), &command)
            .unwrap_or(Handoff::Uncertain);
        let raw = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(alondra_store::Error::from)?;
        let mut transaction = LifecycleTransaction::attach(raw, self.scope.clone())?;
        transaction.record_receipt(
            attempt,
            match handoff {
                Handoff::Offered => DispatchReceipt::Handed,
                Handoff::RefusedBeforeOffer => DispatchReceipt::Rejected,
                Handoff::Uncertain => DispatchReceipt::Uncertain,
            },
        )?;
        self.permits.extend(transaction.commit()?.permits);
        Ok(handoff)
    }
    /// Accepted evidence and normalized aggregate facts commit before the host cursor is ACKed.
    pub fn observe(&mut self) -> Result<bool> {
        if self.pending.is_none() {
            self.pending = self.adapter.next()?;
        }
        let Some(batch) = self.pending.as_ref() else {
            return Ok(false);
        };
        if batch.evidence.is_empty() || batch.evidence.len() > 1_048_576 {
            return Err(Error::ObservationCapacity);
        }
        let raw = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(alondra_store::Error::from)?;
        let mut transaction = LifecycleTransaction::attach(raw, self.scope.clone())?;
        let pending = stage_observation(
            &mut transaction,
            self.adapter.binding(),
            self.adapter.owner_generation()?,
            batch,
            self.adapter.normalizer(),
        )?;
        record_facts(&mut transaction, pending)?;
        self.permits.extend(transaction.commit()?.permits);
        self.adapter.acknowledge(&batch.execution, batch.cursor)?;
        self.pending = None;
        Ok(true)
    }
    pub fn load(&mut self, task: &TaskId) -> Result<std::sync::Arc<TaskLifecycle>> {
        let raw = self
            .connection
            .transaction()
            .map_err(alondra_store::Error::from)?;
        let transaction = LifecycleTransaction::attach(raw, self.scope.clone())?;
        Ok(transaction.load(task)?)
    }
}
