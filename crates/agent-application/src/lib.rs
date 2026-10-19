#![forbid(unsafe_code)]
//! Scoped technical staging over a transaction borrowed from the application database owner.
//!
//! Production binding requires daemon-owned domain commands and commit operations.
//! Observations needing that application boundary remain unacknowledged.
//! ```
//! use alondra_agent_application::InputMetadata;
//! fn stage(
//!     transaction: &mut alondra_store::LifecycleTransaction<'_>,
//!     execution: &alondra_foundation::ExecutionStamp,
//!     input: &alondra_foundation::InputId,
//! ) {
//!     let _ = InputMetadata::with_execution(transaction, execution, input, |metadata| {
//!         Ok(metadata.execution().clone())
//!     });
//! }
//! ```
//! Adapter metadata is never handed out outside its poisoning guard:
//! ```compile_fail,E0599
//! use alondra_agent_application::InputMetadata;
//! let _ = InputMetadata::for_execution;
//! ```
//! ```compile_fail,E0432
//! use alondra_agent_application::WorkerController;
//! ```
use alondra_domain::{RuntimeFact, Trigger, TurnId};
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
impl<'connection> InputMetadata<'_, 'connection> {
    /// The handle exists only inside this guard: adapter errors or panics, including its own
    /// decoding, poison the transaction even when the caller catches them.
    pub fn with_execution<T>(
        transaction: &mut LifecycleTransaction<'connection>,
        execution: &ExecutionStamp,
        input: &InputId,
        work: impl FnOnce(InputMetadata<'_, 'connection>) -> std::result::Result<T, AdapterError>,
    ) -> Result<T> {
        transaction.inspect_mut(|transaction| {
            let aggregate = transaction.load(execution.task())?;
            if aggregate.execution() != Some(execution) || !aggregate.pending_inputs().contains(&input)
            {
                return Err(Error::BindingMismatch);
            }
            transaction.execution_binding(execution)?;
            Ok(work(InputMetadata {
                transaction,
                execution,
                input,
            })?)
        })
    }
    pub fn with_attempt<T>(
        transaction: &mut LifecycleTransaction<'connection>,
        attempt: &DispatchAttempt,
        work: impl FnOnce(InputMetadata<'_, 'connection>) -> std::result::Result<T, AdapterError>,
    ) -> Result<T> {
        transaction.fence_attempt(attempt)?;
        Self::with_execution(transaction, attempt.execution(), attempt.input().id(), work)
    }
}
impl<'connection> InputMetadata<'_, 'connection> {
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
        let metadata = self.transaction.inspect(|| AdapterInputMetadata::new(bytes))?;
        self.transaction.put_adapter_input(self.execution, self.input, metadata)
    }
    pub fn input(&self) -> alondra_store::Result<Option<Vec<u8>>> {
        Ok(self
            .transaction
            .adapter_input(self.execution, self.input)?
            .map(|value| value.bytes().to_vec()))
    }
    pub fn put_conversation(&mut self, bytes: Vec<u8>) -> alondra_store::Result<()> {
        let metadata = self.transaction.inspect(|| AdapterInputMetadata::new(bytes))?;
        self.transaction.put_adapter_conversation(self.execution, metadata)
    }
    pub fn conversation(&self) -> alondra_store::Result<Option<Vec<u8>>> {
        Ok(self
            .transaction
            .adapter_conversation(self.execution)?
            .map(|value| value.bytes().to_vec()))
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
/// A normalized fact for the domain to record. The end of the root turn carries no card: which
/// card it requires, if any, is the domain's decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservedFact {
    Runtime(RuntimeFact),
    TurnEnded {
        turn: TurnId,
        failure: Option<alondra_provider_contract::ApiFailure>,
    },
    /// Evidence about Claude itself, not about the task: its runtime does not move.
    ModelAnswered,
}
impl ObservedFact {
    fn moves_runtime(&self) -> bool {
        !matches!(self, Self::ModelAnswered)
    }
}

fn domain_fact(fact: &Observation) -> Result<ObservedFact> {
    Ok(ObservedFact::Runtime(match fact {
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
        Observation::TurnEnded { turn, failure } => {
            return Ok(ObservedFact::TurnEnded {
                turn: turn.clone(),
                failure: failure.clone(),
            });
        }
        Observation::ModelAnswered => return Ok(ObservedFact::ModelAnswered),
        Observation::TransportLost | Observation::ResourceReleased => return Err(Error::RequiresDomainCommand),
    }))
}

/// An accepted observation whose normalized facts the domain still has to record. It exists only
/// after the raw evidence, its owner, binding and cursor were checked and its checkpoint staged;
/// the owner records its facts through a domain command and then settles it, once. Until it is
/// settled the transaction cannot commit.
/// ```compile_fail,E0277
/// fn duplicate<T: Clone>() {}
/// duplicate::<alondra_agent_application::PendingObservation>();
/// ```
#[derive(Debug)]
#[must_use = "an accepted observation must be settled, or its transaction cannot commit"]
pub struct PendingObservation {
    acceptance: ObservationAcceptance,
    execution: ExecutionStamp,
    cursor: u64,
    facts: Vec<ObservedFact>,
}
impl PendingObservation {
    pub fn acceptance(&self) -> ObservationAcceptance {
        self.acceptance
    }
    pub fn execution(&self) -> &ExecutionStamp {
        &self.execution
    }
    /// The facts the domain records, in order. A duplicate of an observation already recorded has
    /// none: they were recorded with it.
    pub fn facts(&self) -> &[ObservedFact] {
        &self.facts
    }
    /// The domain recorded every fact: the task's runtime moved by exactly that many.
    pub fn settle(self, transaction: &mut LifecycleTransaction<'_>) -> Result<ObservationAcceptance> {
        if self.acceptance == ObservationAcceptance::Accepted {
            let moved = self.facts.iter().filter(|fact| fact.moves_runtime()).count();
            transaction.settle_observation(&self.execution, self.cursor, moved)?;
        }
        Ok(self.acceptance)
    }
}

/// Accepts the raw observation and stages its checkpoint; the facts it was normalized to are the
/// domain's to record. A lost transport or a released resource returns RequiresDomainCommand
/// before anything else: the domain does not record them yet.
/// The owner must abort the whole transaction on error; this helper stages no independent commit.
pub fn stage_observation<N: ObservationNormalizer>(
    transaction: &mut LifecycleTransaction<'_>,
    binding: &alondra_provider_contract::ProviderBinding,
    owner_generation: u64,
    batch: &OrderedObservation,
    normalizer: &N,
) -> Result<PendingObservation> {
    transaction.inspect(|| {
        if batch.evidence.is_empty() || batch.evidence.len() > 1_048_576 {
            return Err(Error::ObservationCapacity);
        }
        if owner_generation != batch.owner_generation {
            return Err(alondra_store::Error::StaleOwner.into());
        }
        if transaction.execution_binding(&batch.execution)? != *binding {
            return Err(Error::BindingMismatch);
        }
        Ok::<_, Error>(())
    })?;
    let acceptance = transaction.accept_owned_observation(
        &batch.execution,
        batch.owner_generation,
        batch.cursor,
        transaction.inspect(|| CommandEvidence::new(batch.evidence.clone()))?,
    )?;
    let mut facts = vec![];
    if acceptance == ObservationAcceptance::Accepted {
        let (previous, normalized, checkpoint) = transaction.inspect(|| {
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
            Ok::<_, Error>((previous, facts, normalized.checkpoint))
        })?;
        facts = normalized;
        transaction.put_normalization_checkpoint(
            &batch.execution,
            batch.cursor,
            previous,
            checkpoint,
        )?;
    }
    transaction.inspect(|| {
        if transaction
            .normalization_checkpoint(&batch.execution)?
            .is_none_or(|checkpoint| checkpoint.cursor() < batch.cursor)
        {
            return Err(alondra_store::Error::CheckpointConflict.into());
        }
        Ok(PendingObservation {
            acceptance,
            execution: batch.execution.clone(),
            cursor: batch.cursor,
            facts,
        })
    })
}
