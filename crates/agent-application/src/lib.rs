#![forbid(unsafe_code)]
//! Worker technical controller over an existing Task and the durable journal.
//!
//! Production binding requires daemon-owned domain commands and commit operations.
//! Observations needing that application boundary remain unacknowledged.
use alondra_domain::{Event, RuntimeFact, TaskLifecycle, Trigger};
use alondra_foundation::{ExecutionStamp, InputId, TaskId};
use alondra_provider_contract::{
    Adapter, AdapterError, Handoff, Invocation, Observation, Operation, OrderedObservation,
    Placement,
};
use alondra_store::{
    AdapterInputMetadata, AdapterState, AuthorityScope, CommandEvidence, DispatchAttempt,
    DispatchKind, DispatchPermit, DispatchReceipt, InputPayload, LifecycleTransaction,
    ObservationAcceptance, OwnerGrant,
};
use rusqlite::{Connection, TransactionBehavior};
use std::collections::VecDeque;

#[derive(Debug)]
pub enum Error {
    Store(alondra_store::Error),
    Adapter(AdapterError),
    BindingMismatch,
    RequiresDomainCommand,
    ObservationCapacity,
}
impl From<alondra_store::Error> for Error {
    fn from(value: alondra_store::Error) -> Self {
        Self::Store(value)
    }
}
impl From<AdapterError> for Error {
    fn from(value: AdapterError) -> Self {
        Self::Adapter(value)
    }
}
type Result<T> = std::result::Result<T, Error>;

/// Adapter-owned metadata is scoped by the application to one exact execution/input.
/// An adapter cannot use this handle to write business state or select another Task.
pub struct InputMetadata<'borrow, 'connection> {
    transaction: &'borrow mut LifecycleTransaction<'connection>,
    execution: &'borrow ExecutionStamp,
    input: &'borrow InputId,
}
impl InputMetadata<'_, '_> {
    pub fn execution(&self) -> &ExecutionStamp {
        self.execution
    }
    pub fn input_id(&self) -> &InputId {
        self.input
    }
    pub fn put_input(&mut self, bytes: Vec<u8>) -> alondra_store::Result<()> {
        self.transaction.put_adapter_input(
            self.execution,
            self.input,
            AdapterInputMetadata::new(bytes)?,
        )
    }
    pub fn input(&self) -> alondra_store::Result<Vec<u8>> {
        Ok(self
            .transaction
            .adapter_input(self.execution, self.input)?
            .bytes()
            .to_vec())
    }
    pub fn put_conversation(&mut self, bytes: Vec<u8>) -> alondra_store::Result<()> {
        self.transaction
            .put_adapter_conversation(self.execution, AdapterInputMetadata::new(bytes)?)
    }
    pub fn conversation(&self) -> alondra_store::Result<Vec<u8>> {
        Ok(self
            .transaction
            .adapter_conversation(self.execution)?
            .bytes()
            .to_vec())
    }
}

/// Read-only native mapping and causal checkpoint access for one received execution.
pub struct ObservationMetadata<'borrow, 'connection> {
    transaction: &'borrow LifecycleTransaction<'connection>,
    execution: &'borrow ExecutionStamp,
    cursor: u64,
}
impl ObservationMetadata<'_, '_> {
    pub fn execution(&self) -> &ExecutionStamp {
        self.execution
    }
    pub fn cursor(&self) -> u64 {
        self.cursor
    }
    pub fn checkpoint(
        &self,
    ) -> alondra_store::Result<Option<alondra_store::NormalizationCheckpoint>> {
        self.transaction.normalization_checkpoint(self.execution)
    }
    pub fn lookup_inputs(
        &self,
        keys: &[Vec<u8>],
    ) -> alondra_store::Result<Vec<alondra_store::AdapterInputBinding>> {
        self.transaction.lookup_adapter_inputs(self.execution, keys)
    }
}
pub struct Normalization {
    facts: Vec<Observation>,
    checkpoint: Vec<u8>,
}
impl Normalization {
    pub fn new(
        facts: Vec<Observation>,
        checkpoint: Vec<u8>,
    ) -> std::result::Result<Self, AdapterError> {
        if facts.len() > 256 || checkpoint.is_empty() || checkpoint.len() > 65536 {
            return Err(AdapterError::InvalidObservation);
        }
        Ok(Self { facts, checkpoint })
    }
}
/// Implementations fold from the supplied durable checkpoint without I/O or transport access.
pub trait ObservationNormalizer {
    fn normalize(
        &self,
        batch: &OrderedObservation,
        metadata: &ObservationMetadata<'_, '_>,
    ) -> std::result::Result<Normalization, AdapterError>;
}

pub trait DurableAdapter: Adapter {
    type Normalizer: ObservationNormalizer;
    fn normalizer(&self) -> &Self::Normalizer;
    fn owner_generation(&self) -> std::result::Result<u64, AdapterError>;
    fn stage_input(
        &mut self,
        metadata: InputMetadata<'_, '_>,
    ) -> std::result::Result<(), AdapterError>;
    fn load_input(
        &mut self,
        metadata: InputMetadata<'_, '_>,
    ) -> std::result::Result<Self::NativeInput, AdapterError>;
    fn install_owner(
        &mut self,
        grant: OwnerGrant,
        state: AdapterState,
    ) -> std::result::Result<(), AdapterError>;
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
        self.adapter.stage_input(InputMetadata {
            transaction: &mut transaction,
            execution: &execution,
            input: &input_id,
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
        let native = self.adapter.load_input(InputMetadata {
            transaction: &mut transaction,
            execution: attempt.execution(),
            input: attempt.input().id(),
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
        if self.adapter.owner_generation()? != batch.owner_generation {
            return Err(alondra_store::Error::StaleOwner.into());
        }
        let raw = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(alondra_store::Error::from)?;
        let mut transaction = LifecycleTransaction::attach(raw, self.scope.clone())?;
        if transaction.execution_binding(&batch.execution)? != *self.adapter.binding() {
            return Err(Error::BindingMismatch);
        }
        transaction.fence_observation_owner(&batch.execution, batch.owner_generation)?;
        let acceptance = transaction.accept_observation(
            &batch.execution,
            batch.cursor,
            CommandEvidence::new(batch.evidence.clone())?,
        )?;
        if acceptance == ObservationAcceptance::Accepted {
            let metadata = ObservationMetadata {
                transaction: &transaction,
                execution: &batch.execution,
                cursor: batch.cursor,
            };
            let previous = metadata.checkpoint()?.map(|checkpoint| checkpoint.cursor());
            let normalized = self.adapter.normalizer().normalize(batch, &metadata)?;
            for (index, fact) in normalized
                .facts
                .iter()
                .map(domain_fact)
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .enumerate()
            {
                let aggregate = transaction.load(batch.execution.task())?;
                let cursor = aggregate
                    .cursor()
                    .and_then(|cursor| cursor.checked_add(1))
                    .ok_or(alondra_store::Error::InvalidInput)?;
                transaction.apply(
                    batch.execution.task(),
                    aggregate.revision(),
                    &format!(
                        "adapter-{}-{}-{index}",
                        batch.execution.generation(),
                        batch.cursor
                    ),
                    Event::Runtime {
                        stamp: batch.execution.clone(),
                        cursor,
                        fact,
                    },
                )?;
            }
            transaction.put_normalization_checkpoint(
                &batch.execution,
                batch.cursor,
                previous,
                normalized.checkpoint,
            )?;
        }
        if transaction
            .normalization_checkpoint(&batch.execution)?
            .is_none_or(|checkpoint| checkpoint.cursor() < batch.cursor)
        {
            return Err(alondra_store::Error::CheckpointConflict.into());
        }
        self.permits.extend(transaction.commit()?.permits);
        self.adapter.acknowledge(&batch.execution, batch.cursor)?;
        self.pending = None;
        Ok(true)
    }
    pub fn load(&mut self, task: &TaskId) -> Result<TaskLifecycle> {
        let raw = self
            .connection
            .transaction()
            .map_err(alondra_store::Error::from)?;
        let transaction = LifecycleTransaction::attach(raw, self.scope.clone())?;
        Ok(transaction.load(task)?)
    }
}
fn invocation(attempt: &DispatchAttempt) -> Invocation<'_> {
    Invocation {
        binding: attempt.binding(),
        execution: attempt.execution(),
        command: attempt.command_id(),
        input: attempt.input().id(),
        text: attempt.input().text(),
        operation: match attempt.kind() {
            DispatchKind::Launch => Operation::Launch,
            DispatchKind::Input => Operation::Input,
        },
        placement: attempt.placement(),
    }
}
fn domain_fact(fact: &Observation) -> Result<RuntimeFact> {
    Ok(match fact {
        Observation::Available => RuntimeFact::ResourceAvailable,
        Observation::TurnStarted { input, turn } => RuntimeFact::TurnStarted {
            turn: turn.clone(),
            trigger: Trigger::Input(input.clone()),
        },
        Observation::InputConsumed { input } => RuntimeFact::InputConsumed {
            input: input.clone(),
        },
        Observation::WorkStarted { turn, work } => RuntimeFact::WorkStarted {
            turn: turn.clone(),
            work: work.clone(),
        },
        Observation::WorkEnded { work, continuation } => RuntimeFact::WorkEnded {
            work: work.clone(),
            continuation: continuation.clone(),
        },
        Observation::TurnEnded { .. }
        | Observation::TransportLost
        | Observation::ResourceReleased => return Err(Error::RequiresDomainCommand),
    })
}
