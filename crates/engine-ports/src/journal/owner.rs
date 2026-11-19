//! The values of an owner transaction: what it stages, commits and takes over.
use super::{Commit, StoreError};
use alondra_agent_application::{InputMetadata};
use alondra_domain_rules::{Command, CommandId, CommandOutcome, DomainState};
use alondra_foundation::ExecutionStamp;
use alondra_provider_contract::{AdapterError, PreparedCommand};
use alondra_domain_rules::ReadSnapshot;
use alondra_store::{Committed, DispatchAttempt, UnresolvedCommand, UnresolvedControl};
use std::sync::Arc;

/// The provider's part of a dispatch: its metadata for an input, and the exact command it sends.
pub trait Preparer {
    fn stage_input(&mut self, metadata: InputMetadata<'_, '_>) -> std::result::Result<(), AdapterError>;
    fn prepare(&mut self, attempt: &DispatchAttempt, metadata: InputMetadata<'_, '_>) -> std::result::Result<PreparedCommand, AdapterError>;
}

impl<P: Preparer + ?Sized> Preparer for &mut P {
    fn stage_input(&mut self, metadata: InputMetadata<'_, '_>) -> std::result::Result<(), AdapterError> {
        (**self).stage_input(metadata)
    }
    fn prepare(&mut self, attempt: &DispatchAttempt, metadata: InputMetadata<'_, '_>) -> std::result::Result<PreparedCommand, AdapterError> {
        (**self).prepare(attempt, metadata)
    }
}

/// What a daemon takes over: claimed commands to reconcile, and controls whose preparation
/// recorded no sendable bytes. Only an ended host lets those unrecorded controls be rejected.
pub struct Adoption {
    pub stamp: ExecutionStamp,
    pub directory: String,
    pub commands: Vec<UnresolvedCommand>,
    pub controls: Vec<UnresolvedControl>,
    pub unrecorded_controls: Vec<i64>,
    pub unrecorded_commands: Vec<i64>,
    pub receipt_candidates: Vec<alondra_store::ReceiptCandidate>,
}

/// What the commands staged so far add up to. A later replay or no-op never takes back an
/// earlier change, its notification or the outcome another command got.
#[derive(Debug)]
pub struct OwnerDraft {
    pub state: Arc<DomainState>,
    pub outcomes: Vec<StagedOutcome>,
    pub domain_changed: bool,
    pub schedule_changed: bool,
    pub version: i64,
}

#[derive(Debug)]
pub struct StagedOutcome {
    pub command_id: CommandId,
    pub outcome: CommandOutcome,
    pub replayed: bool,
}

/// Exists only after the real SQL commit returned. Its seals stay inside until the runtime has
/// published the committed state.
#[derive(Debug)]
pub struct OwnerCommit {
    pub draft: OwnerDraft,
    pub technical: Committed,
}

impl OwnerCommit {
    /// Hands out the seals only once readers see exactly the state this transaction committed.
    pub fn acknowledge(
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

/// Why the owner cannot record an observation, kept typed so that whether it is discarded is
/// decided on its kind.
#[derive(Debug)]
pub enum ObservationRefusal {
    Provider(alondra_agent_application::Error),
    /// A fact the domain has no command for, or one that names what the task does not hold.
    Unrecordable(String),
}

impl std::fmt::Display for ObservationRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(error) => write!(f, "provider observation refused: {error:?}"),
            Self::Unrecordable(what) => write!(f, "provider observation refused: {what}"),
        }
    }
}

impl std::error::Error for ObservationRefusal {}

impl OwnerDraft {
    pub fn record(&mut self, command: &Command, command_id: &CommandId, commit: Commit) {
        let fresh = !commit.replayed && commit.outcome.changed;
        if fresh {
            self.state = commit.state;
        }
        self.domain_changed |= fresh;
        self.schedule_changed |= fresh && super::schedule_command(command);
        self.outcomes.push(StagedOutcome {
            command_id: command_id.clone(),
            outcome: commit.outcome,
            replayed: commit.replayed,
        });
    }
}

/// Why an observation was discarded. Its code is what the table keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscardReason {
    /// The domain recorded its facts, but not as far as the observation says they move the task.
    Unsettled,
    /// It holds a fact the domain has no command for, or one that names what the task lacks.
    Unrecordable,
    /// The provider's adapter refused it, with the kind of its refusal.
    AdapterRefused(AdapterRefusal),
    /// Its evidence is larger than an observation may be.
    Capacity,
    /// The domain refused it even with what it cannot record left out.
    DomainRefused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterRefusal {
    InvalidObservation,
    Unsupported,
    UnknownInput,
    Storage,
    Protocol,
}

impl DiscardReason {
    /// The reason an observation that failed with `error` is discarded for, or `None` when the
    /// failure is not the observation's own: another owner, a conflict of evidence, the disk.
    pub fn of(error: &StoreError) -> Option<Self> {
        let StoreError::Storage(error) = error else { return Some(Self::DomainRefused) };
        if matches!(error.downcast_ref::<alondra_store::Error>(), Some(alondra_store::Error::UnsettledObservation)) {
            return Some(Self::Unsettled);
        }
        use alondra_agent_application::Error;
        Some(match error.downcast_ref::<ObservationRefusal>()? {
            ObservationRefusal::Unrecordable(_) | ObservationRefusal::Provider(Error::RequiresDomainCommand) => Self::Unrecordable,
            ObservationRefusal::Provider(Error::ObservationCapacity) => Self::Capacity,
            ObservationRefusal::Provider(Error::Adapter(adapter)) => Self::AdapterRefused(match adapter {
                AdapterError::Disconnected => return None,
                AdapterError::InvalidObservation => AdapterRefusal::InvalidObservation,
                AdapterError::Unsupported => AdapterRefusal::Unsupported,
                AdapterError::UnknownInput => AdapterRefusal::UnknownInput,
                AdapterError::Storage(_) => AdapterRefusal::Storage,
                AdapterError::Protocol(_) => AdapterRefusal::Protocol,
            }),
            ObservationRefusal::Provider(Error::Store(_) | Error::BindingMismatch) => return None,
        })
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Unsettled => "unsettled",
            Self::Unrecordable => "unrecordable",
            Self::AdapterRefused(AdapterRefusal::InvalidObservation) => "adapter_invalid_observation",
            Self::AdapterRefused(AdapterRefusal::Unsupported) => "adapter_unsupported",
            Self::AdapterRefused(AdapterRefusal::UnknownInput) => "adapter_unknown_input",
            Self::AdapterRefused(AdapterRefusal::Storage) => "adapter_storage",
            Self::AdapterRefused(AdapterRefusal::Protocol) => "adapter_protocol",
            Self::Capacity => "capacity",
            Self::DomainRefused => "domain_refused",
        }
    }
}

/// What a test has the domain refuse, once, as it refuses what it cannot record.
#[derive(Clone, Copy, PartialEq)]
pub enum Refuse {
    TurnEnd,
    DecisionRequest,
    InterruptAccepted,
    /// The provider's adapter refuses the next observation, as one it cannot read.
    Observation,
}

thread_local! {
    static REFUSE: std::cell::Cell<Option<Refuse>> = const { std::cell::Cell::new(None) };
}

/// The next `what` this thread observes is refused.
pub fn refuse_next(what: Refuse) {
    REFUSE.with(|refuse| refuse.set(Some(what)));
}

/// Whether the test asked this thread to refuse `what`; the run that takes it clears it.
pub fn take_refusal(what: Refuse, take: bool) -> bool {
    REFUSE.with(|refuse| {
        let asked = refuse.get() == Some(what);
        if asked && take {
            refuse.set(None);
        }
        asked
    })
}

/// What this thread was asked to refuse, handed to a thread it starts, which refuses it instead.
pub fn carried_refusal() -> impl FnOnce() + Send {
    let what = REFUSE.with(|refuse| refuse.replace(None));
    move || if let Some(what) = what { refuse_next(what) }
}

impl From<alondra_store::Error> for StoreError {
    fn from(error: alondra_store::Error) -> Self {
        Self::Storage(error.into())
    }
}

impl From<ObservationRefusal> for StoreError {
    fn from(refusal: ObservationRefusal) -> Self {
        Self::Storage(refusal.into())
    }
}
