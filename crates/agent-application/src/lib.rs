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
use alondra_foundation::{ExecutionStamp, InputId, WorkId};
use alondra_provider_contract::{
    Adapter, AdapterError, DecisionKind, Invocation, Observation, Operation, OrderedObservation,
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
    context: Vec<alondra_store::TaskContextItem>,
}
impl Normalization {
    pub fn new(
        facts: Vec<Observation>,
        checkpoint: Vec<u8>,
    ) -> std::result::Result<Self, AdapterError> {
        if facts.len() > 256 || checkpoint.is_empty() || checkpoint.len() > 65536 {
            return Err(AdapterError::InvalidObservation);
        }
        Ok(Self { facts, checkpoint, context: vec![] })
    }

    /// Public final messages and tool results; private reasoning and transport metadata never
    /// belong here. The application stores these before committing this checkpoint.
    pub fn with_context(mut self, context: Vec<alondra_store::TaskContextItem>) -> std::result::Result<Self, AdapterError> {
        if context.len() > 256 { return Err(AdapterError::InvalidObservation); }
        self.context = context;
        Ok(self)
    }
}
/// Implementations fold from the supplied durable checkpoint without I/O or transport access.
pub trait ObservationNormalizer {
    fn normalize(
        &self,
        batch: &OrderedObservation,
        metadata: &ObservationMetadata<'_, '_>,
    ) -> std::result::Result<Normalization, AdapterError>;
    /// The checkpoint moved past `batch` with no fact and nothing of its evidence read: the
    /// observation is discarded, and the next one folds from where the earlier ones left it.
    fn skip(
        &self,
        batch: &OrderedObservation,
        metadata: &ObservationMetadata<'_, '_>,
    ) -> std::result::Result<Normalization, AdapterError>;
}

/// Normalizes every observation by skipping it: what a discarded observation is staged with.
pub struct Skipping<'normalizer, N>(pub &'normalizer N);
impl<N: ObservationNormalizer> ObservationNormalizer for Skipping<'_, N> {
    fn normalize(
        &self,
        batch: &OrderedObservation,
        metadata: &ObservationMetadata<'_, '_>,
    ) -> std::result::Result<Normalization, AdapterError> {
        self.0.skip(batch, metadata)
    }
    fn skip(
        &self,
        batch: &OrderedObservation,
        metadata: &ObservationMetadata<'_, '_>,
    ) -> std::result::Result<Normalization, AdapterError> {
        self.0.skip(batch, metadata)
    }
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
    ContinuationRequested { work: WorkId, activation: String, text: String },
    TurnEnded {
        turn: TurnId,
        outcome: alondra_provider_contract::TurnOutcome,
    },
    /// Claude will never read `input`: the domain takes it out of what the task waits on and asks
    /// the user whether to send it again.
    InputDropped {
        input: InputId,
    },
    /// The provider's whole account of what runs: the domain ends what its record holds beyond it.
    Reconciled {
        idle: bool,
        live: std::collections::BTreeSet<WorkId>,
    },
    /// Evidence about Claude itself, not about the task: its runtime does not move.
    ModelAnswered,
    /// Claude asks for the decision its host persisted at `epoch`: the domain asks the user or
    /// decides it by its own rules.
    Asked {
        request: String,
        epoch: u64,
        kind: DecisionKind,
        tool_use: Option<String>,
        payload: Vec<u8>,
    },
    /// A worktree of the task, made or entered by its node or a subagent: evidence for the
    /// task's cleanup, its runtime does not move.
    Worktree {
        path: String,
        branch: Option<String>,
        subagent: bool,
    },
    /// The worktree Claude Code names after a subagent that went on in the background.
    SubagentWorktree {
        agent: String,
    },
    /// A command of the task's node opened this pull request.
    PullRequestOpened(alondra_provider_contract::OpenedPullRequest),
    /// A command ran `gh pr create` and its output named no pull request.
    PullRequestNotNamed,
    /// The request at `epoch` is over. An answered one that `continued` lets the turn that asked
    /// go on: the domain records that too.
    Settled {
        epoch: u64,
        answered: bool,
        continued: bool,
    },
    /// How Claude's MCP servers are: evidence about the node, its runtime does not move.
    McpServers {
        servers: Vec<alondra_provider_contract::McpServerReport>,
    },
    McpSignIn {
        server: String,
        sign_in: alondra_provider_contract::McpSignInReport,
    },
    McpToolFailed,
    /// What the provider said of the usage of the binding's account: evidence about the account,
    /// the task's runtime does not move.
    Usage(alondra_provider_contract::UsageReport),
}
impl ObservedFact {
    /// How far the task's runtime moves when the domain records this fact.
    fn runtime_moves(&self) -> usize {
        match self {
            Self::ModelAnswered
            | Self::Worktree { .. }
            | Self::SubagentWorktree { .. }
            | Self::PullRequestOpened(_)
            | Self::PullRequestNotNamed
            | Self::McpServers { .. }
            | Self::McpSignIn { .. }
            | Self::McpToolFailed
            | Self::ContinuationRequested { .. }
            | Self::Reconciled { .. }
            | Self::Usage(_) => 0,
            Self::Settled { answered: true, continued: true, .. } => 2,
            _ => 1,
        }
    }
}

fn domain_fact(fact: &Observation) -> Result<ObservedFact> {
    Ok(ObservedFact::Runtime(match fact {
        Observation::Available => RuntimeFact::ResourceAvailable,
        Observation::TurnStarted { input, turn } => RuntimeFact::TurnStarted {
            turn: turn.clone(),
            trigger: Trigger::Input(input.clone()),
        },
        Observation::TurnResumed { continuation, turn } => RuntimeFact::TurnStarted {
            turn: turn.clone(),
            trigger: Trigger::OwnedContinuation(continuation.clone()),
        },
        Observation::TurnUnprompted { turn } => RuntimeFact::TurnStarted {
            turn: turn.clone(),
            trigger: Trigger::Unprompted,
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
        Observation::ContinuationConsumed { continuation } => RuntimeFact::ContinuationConsumed {
            continuation: continuation.clone(),
        },
        Observation::InputDropped { input } => return Ok(ObservedFact::InputDropped { input: input.clone() }),
        Observation::TurnEnded { turn, outcome } => {
            return Ok(ObservedFact::TurnEnded {
                turn: turn.clone(),
                outcome: outcome.clone(),
            });
        }
        Observation::ContinuationRequested { work, activation, text } => return Ok(ObservedFact::ContinuationRequested { work: work.clone(), activation: activation.clone(), text: text.clone() }),
        Observation::Reconciled { idle, live } => return Ok(ObservedFact::Reconciled { idle: *idle, live: live.clone() }),
        Observation::ModelAnswered => return Ok(ObservedFact::ModelAnswered),
        Observation::Asked { request, epoch, kind, tool_use, payload } => {
            return Ok(ObservedFact::Asked {
                request: request.clone(),
                epoch: *epoch,
                kind: *kind,
                tool_use: tool_use.clone(),
                payload: payload.clone(),
            });
        }
        Observation::WorktreeMade { path, branch, subagent } => {
            return Ok(ObservedFact::Worktree { path: path.clone(), branch: branch.clone(), subagent: *subagent });
        }
        Observation::SubagentWorktree { agent } => return Ok(ObservedFact::SubagentWorktree { agent: agent.clone() }),
        Observation::PullRequestOpened(pr) => return Ok(ObservedFact::PullRequestOpened(pr.clone())),
        Observation::PullRequestNotNamed => return Ok(ObservedFact::PullRequestNotNamed),
        Observation::DecisionSettled { epoch, answered, continued } => {
            return Ok(ObservedFact::Settled { epoch: *epoch, answered: *answered, continued: *continued });
        }
        Observation::McpServers { servers } => return Ok(ObservedFact::McpServers { servers: servers.clone() }),
        Observation::McpSignIn { server, sign_in } => {
            return Ok(ObservedFact::McpSignIn { server: server.clone(), sign_in: sign_in.clone() });
        }
        Observation::McpToolFailed => return Ok(ObservedFact::McpToolFailed),
        Observation::Usage(report) => return Ok(ObservedFact::Usage(report.clone())),
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
        let moved = self.facts.iter().map(ObservedFact::runtime_moves).sum();
        self.settle_moved(transaction, moved)
    }
    /// Settles the observation by the moves the domain recorded of its facts: those it refused
    /// are left out, and the observation still commits.
    pub fn settle_recorded(self, transaction: &mut LifecycleTransaction<'_>, moved: usize) -> Result<ObservationAcceptance> {
        self.settle_moved(transaction, moved)
    }
    fn settle_moved(self, transaction: &mut LifecycleTransaction<'_>, moved: usize) -> Result<ObservationAcceptance> {
        if self.acceptance == ObservationAcceptance::Accepted {
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
        let (previous, normalized, checkpoint, context) = transaction.inspect(|| {
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
            Ok::<_, Error>((previous, facts, normalized.checkpoint, normalized.context))
        })?;
        facts = normalized;
        if facts.iter().any(|fact| matches!(fact, ObservedFact::InputDropped { .. })) {
            // A drop of an input the task no longer waits on, already read or settled, is evidence.
            let lifecycle = transaction.load(batch.execution.task())?;
            facts.retain(|fact| !matches!(fact, ObservedFact::InputDropped { input } if !lifecycle.is_pending(input)));
        }
        transaction.put_normalization_checkpoint(
            &batch.execution,
            batch.cursor,
            previous,
            checkpoint,
        )?;
        if !context.is_empty() {
            transaction.capture_context(&batch.execution, batch.cursor, &context)?;
        }
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
