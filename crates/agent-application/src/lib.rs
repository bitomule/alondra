#![forbid(unsafe_code)]
//! Scoped technical staging over a transaction borrowed from the application database owner.
//!
//! Production binding requires daemon-owned domain commands and commit operations.
//! Observations needing that application boundary remain unacknowledged.
//! ```
//! use alondra_agent_application::InputMetadata;
//! let _ = InputMetadata::for_execution;
//! ```
//! ```compile_fail,E0432
//! use alondra_agent_application::WorkerController;
//! ```
use alondra_domain::{Event, RuntimeFact, Trigger};
use alondra_foundation::{ExecutionStamp, InputId};
use alondra_provider_contract::{
    Adapter, AdapterError, Invocation, Observation, Operation, OrderedObservation,
};
use alondra_store::{
    AdapterInputMetadata, AdapterState, CommandEvidence, DispatchAttempt, DispatchKind,
    LifecycleTransaction, ObservationAcceptance, OwnerGrant,
};

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
impl<'borrow, 'connection> InputMetadata<'borrow, 'connection> {
    pub fn for_execution(
        transaction: &'borrow mut LifecycleTransaction<'connection>,
        execution: &'borrow ExecutionStamp,
        input: &'borrow InputId,
    ) -> Result<Self> {
        let aggregate = transaction.load(execution.task())?;
        if aggregate.execution() != Some(execution) || !aggregate.pending_inputs().contains(&input)
        {
            return Err(Error::BindingMismatch);
        }
        transaction.execution_binding(execution)?;
        Ok(Self {
            transaction,
            execution,
            input,
        })
    }
    pub fn for_attempt(
        transaction: &'borrow mut LifecycleTransaction<'connection>,
        attempt: &'borrow DispatchAttempt,
    ) -> Result<Self> {
        transaction.fence_attempt(attempt)?;
        Self::for_execution(transaction, attempt.execution(), attempt.input().id())
    }

    pub fn scope(&self) -> &alondra_store::AuthorityScope {
        self.transaction.scope()
    }
    pub fn binding(&self) -> alondra_store::Result<alondra_provider_contract::ProviderBinding> {
        self.transaction.execution_binding(self.execution)
    }
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

pub fn invocation(attempt: &DispatchAttempt) -> Invocation<'_> {
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

/// Stages only technical facts. The real database owner commits and later decides when to ACK.
/// Terminal observations return RequiresDomainCommand before any acknowledgement can be issued.
/// The owner must abort the whole transaction on error; this helper stages no independent commit.
pub fn stage_observation<N: ObservationNormalizer>(
    transaction: &mut LifecycleTransaction<'_>,
    binding: &alondra_provider_contract::ProviderBinding,
    owner_generation: u64,
    batch: &OrderedObservation,
    normalizer: &N,
) -> Result<ObservationAcceptance> {
    if batch.evidence.is_empty() || batch.evidence.len() > 1_048_576 {
        return Err(Error::ObservationCapacity);
    }
    if owner_generation != batch.owner_generation {
        return Err(alondra_store::Error::StaleOwner.into());
    }
    if transaction.execution_binding(&batch.execution)? != *binding {
        return Err(Error::BindingMismatch);
    }
    let acceptance = transaction.accept_owned_observation(
        &batch.execution,
        batch.owner_generation,
        batch.cursor,
        CommandEvidence::new(batch.evidence.clone())?,
    )?;
    if acceptance == ObservationAcceptance::Accepted {
        let metadata = ObservationMetadata {
            transaction,
            execution: &batch.execution,
            cursor: batch.cursor,
        };
        let previous = metadata.checkpoint()?.map(|checkpoint| checkpoint.cursor());
        let normalized = normalizer.normalize(batch, &metadata)?;
        let facts = normalized
            .facts
            .iter()
            .map(domain_fact)
            .collect::<Result<Vec<_>>>()?;
        for (index, fact) in facts.into_iter().enumerate() {
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
    Ok(acceptance)
}
