#![forbid(unsafe_code)]
//! Transactional TaskLifecycle journal port. No production daemon binding is installed here.
mod codec;
mod commit_seals;
pub use commit_seals::{CommittedCommand, CommittedObservation};
mod journal_format;
mod persistence;
mod provider_binding;

use alondra_domain::{
    Event, ExecutionStamp, InputId, Refusal, ResourceEvidence, TaskLifecycle, TaskPhase, WaitReason,
};
use alondra_foundation::{DataEpoch, Revision, TaskId};
use alondra_provider_contract::{Capability, Placement, ProviderBinding};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::collections::BTreeSet;
use std::path::Path;

pub type Result<T> = std::result::Result<T, Error>;
const MAX_PAYLOAD: usize = 1_048_576;

#[derive(Debug)]
pub enum Error {
    Database(rusqlite::Error),
    Codec(serde_json::Error),
    Domain(Refusal),
    InvalidInput,
    WrongScope,
    MissingTask,
    CorruptJournal,
    UnsupportedJournalFormat,
    MissingBinding,
    BindingConflict,
    BindingRevoked,
    StaleOwner,
    UnsupportedCapability,
    MissingAdapterInput,
    MissingCommand,
    ObservationConflict,
    ObservationGap,
    CheckpointConflict,
    MetadataCapacity,
    CommandConflict,
    InputConflict,
    MissingInput,
    MissingIntent,
    StalePermit,
    OfferOrder,
    UnresolvedDispatch,
    InvalidCard,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(value: rusqlite::Error) -> Self {
        Self::Database(value)
    }
}
impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::Codec(value)
    }
}
impl From<Refusal> for Error {
    fn from(value: Refusal) -> Self {
        Self::Domain(value)
    }
}
impl From<alondra_foundation::IdentityError> for Error {
    fn from(value: alondra_foundation::IdentityError) -> Self {
        Self::Domain(value.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorityScope {
    epoch: DataEpoch,
}
impl AuthorityScope {
    pub fn new(epoch: DataEpoch) -> Result<Self> {
        identity(&epoch.0)?;
        Ok(Self { epoch })
    }
    pub fn epoch(&self) -> &DataEpoch {
        &self.epoch
    }
}

#[derive(Debug)]
pub struct InputPayload {
    id: InputId,
    text: String,
}
impl InputPayload {
    pub fn new(id: InputId, text: impl Into<String>) -> Result<Self> {
        let text = text.into();
        if text.is_empty()
            || text.len() > MAX_PAYLOAD - 65_536
            || text.contains('\0')
            || serde_json::to_vec(&text)?.len() > MAX_PAYLOAD - 65_536
        {
            return Err(Error::InvalidInput);
        }
        identity(id.as_str())?;
        Ok(Self { id, text })
    }
    pub fn id(&self) -> &InputId {
        &self.id
    }
    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchKind {
    Launch,
    Input,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchReceipt {
    /// Authenticated ordered handoff (Offered/Applied), never admission alone.
    Handed,
    /// Authenticated refusal proving the host never offered this input.
    Rejected,
    Uncertain,
}

/// A committed intent permits one fenced claim in safe Rust. The execution stamp alone
/// grants no claim authority; the host must separately enforce process ownership.
///
/// Issued permits can be inspected and moved into the claim transaction:
///
/// ```
/// use alondra_foundation::{ExecutionStamp, InputId};
/// use alondra_store::{AuthorityScope, DispatchPermit};
/// fn forward(permit: DispatchPermit) -> DispatchPermit {
///     let _: u64 = permit.revision();
///     let _: &ExecutionStamp = permit.execution();
///     let _: &InputId = permit.input();
///     let _: &AuthorityScope = permit.scope();
///     permit
/// }
/// ```
///
/// External code cannot construct a permit from correlation data:
///
/// ```compile_fail,E0451
/// use alondra_foundation::{ExecutionStamp, InputId};
/// use alondra_store::{AuthorityScope, DispatchPermit};
/// fn fabricate(scope: AuthorityScope, stamp: ExecutionStamp, input: InputId) -> DispatchPermit {
///     DispatchPermit { id: 1, scope, revision: 1, stamp, input }
/// }
/// ```
///
/// An issued permit cannot be duplicated through Clone:
///
/// ```compile_fail,E0277
/// use alondra_store::DispatchPermit;
/// fn requires_clone<T: Clone>() {}
/// requires_clone::<DispatchPermit>();
/// ```
#[derive(Debug)]
pub struct DispatchPermit {
    id: i64,
    scope: AuthorityScope,
    revision: Revision,
    stamp: ExecutionStamp,
    input: InputId,
}
impl DispatchPermit {
    pub fn revision(&self) -> Revision {
        self.revision
    }
    pub fn execution(&self) -> &ExecutionStamp {
        &self.stamp
    }
    pub fn input(&self) -> &InputId {
        &self.input
    }
    pub fn scope(&self) -> &AuthorityScope {
        &self.scope
    }
}

/// An attempt authorizes dispatch only after its claim transaction commits. A transport
/// ACK is never input consumption. Safe Rust callers can inspect and move the attempt:
///
/// ```
/// use alondra_foundation::ExecutionStamp;
/// use alondra_store::{DispatchAttempt, DispatchKind, InputPayload};
/// fn forward(attempt: DispatchAttempt) -> DispatchAttempt {
///     let _: i64 = attempt.intent_id();
///     let _: &ExecutionStamp = attempt.execution();
///     let _: &InputPayload = attempt.input();
///     let _: DispatchKind = attempt.kind();
///     attempt
/// }
/// ```
///
/// External code cannot rebuild an attempt with another durable intent identity:
///
/// ```compile_fail,E0451
/// use alondra_store::DispatchAttempt;
/// fn replace_identity(attempt: DispatchAttempt) -> DispatchAttempt {
///     DispatchAttempt { id: 2, ..attempt }
/// }
/// ```
///
/// An issued attempt cannot be duplicated through Clone:
///
/// ```compile_fail,E0277
/// use alondra_store::DispatchAttempt;
/// fn requires_clone<T: Clone>() {}
/// requires_clone::<DispatchAttempt>();
/// ```
#[derive(Debug)]
pub struct DispatchAttempt {
    id: i64,
    scope: AuthorityScope,
    revision: Revision,
    stamp: ExecutionStamp,
    payload: DispatchPayload,
    binding: ProviderBinding,
}
#[derive(Debug)]
enum DispatchPayload {
    Launch {
        input: InputPayload,
        placement: Placement,
    },
    Input(InputPayload),
}
impl DispatchAttempt {
    pub fn intent_id(&self) -> i64 {
        self.id
    }
    pub fn command_id(&self) -> alondra_foundation::CommandId {
        format!("lifecycle-dispatch-{}", self.id).into()
    }
    pub fn revision(&self) -> Revision {
        self.revision
    }
    pub fn execution(&self) -> &ExecutionStamp {
        &self.stamp
    }
    pub fn input(&self) -> &InputPayload {
        match &self.payload {
            DispatchPayload::Launch { input, .. } | DispatchPayload::Input(input) => input,
        }
    }
    pub fn kind(&self) -> DispatchKind {
        match self.payload {
            DispatchPayload::Launch { .. } => DispatchKind::Launch,
            DispatchPayload::Input(_) => DispatchKind::Input,
        }
    }
    pub fn placement(&self) -> Option<&Placement> {
        match &self.payload {
            DispatchPayload::Launch { placement, .. } => Some(placement),
            DispatchPayload::Input(_) => None,
        }
    }
    pub fn working_directory(&self) -> Option<&str> {
        match self.placement() {
            Some(Placement::LocalDirectory(directory)) => Some(directory),
            _ => None,
        }
    }
    pub fn binding(&self) -> &ProviderBinding {
        &self.binding
    }
    pub fn scope(&self) -> &AuthorityScope {
        &self.scope
    }
}

#[derive(Debug)]
pub struct Committed {
    pub permits: Vec<DispatchPermit>,
    pub attempts: Vec<DispatchAttempt>,
    pub owners: Vec<OwnerGrant>,
    pub commands: Vec<CommittedCommand>,
    pub observations: Vec<CommittedObservation>,
}
/// Committed lane ownership; correlation data alone cannot create or renew a grant.
///
/// ```
/// use alondra_store::OwnerGrant;
/// fn inspect(grant: OwnerGrant) -> OwnerGrant {
///     let _: u64 = grant.generation();
///     grant
/// }
/// ```
///
/// ```compile_fail,E0451
/// use alondra_store::OwnerGrant;
/// fn forge(grant: OwnerGrant) -> OwnerGrant {
///     OwnerGrant { generation: grant.generation() + 1, ..grant }
/// }
/// ```
///
/// ```compile_fail,E0277
/// use alondra_store::OwnerGrant;
/// fn require_clone<T: Clone>() {}
/// require_clone::<OwnerGrant>();
/// ```
#[derive(Debug)]
pub struct OwnerGrant {
    scope: AuthorityScope,
    execution: ExecutionStamp,
    binding: ProviderBinding,
    generation: u64,
}
impl OwnerGrant {
    pub fn scope(&self) -> &AuthorityScope {
        &self.scope
    }
    pub fn execution(&self) -> &ExecutionStamp {
        &self.execution
    }
    pub fn binding(&self) -> &ProviderBinding {
        &self.binding
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Applied {
    pub revision: Revision,
    pub replayed: bool,
}
#[derive(Debug)]
pub struct UnresolvedDispatch {
    pub intent_id: i64,
    pub generation: u64,
    pub input: InputId,
    pub uncertain_receipt: bool,
}

/// Owns a caller-created transaction on domain.db. Drop rolls back every staged effect.
/// Business card settlement belongs to guarded application commands on the same transaction.
///
/// The journal exposes aggregate reads and a real commit boundary:
/// ```
/// use alondra_store::LifecycleTransaction;
/// let _ = LifecycleTransaction::load;
/// let _ = LifecycleTransaction::commit;
/// ```
///
/// It does not expose a second card writer:
/// ```compile_fail,E0599
/// use alondra_store::LifecycleTransaction;
/// let _ = LifecycleTransaction::settle_card;
/// ```
/// The caller must install this port instead of independent legacy task/node writes at cutover.
pub struct LifecycleTransaction<'connection> {
    transaction: Transaction<'connection>,
    scope: AuthorityScope,
    touched: BTreeSet<TaskId>,
    claimed: Vec<DispatchAttempt>,
    owners: Vec<OwnerGrant>,
    cleared_cards: BTreeSet<(TaskId, alondra_foundation::InteractionId)>,
    commands: Vec<CommittedCommand>,
    observations: Vec<CommittedObservation>,
}
impl<'connection> LifecycleTransaction<'connection> {
    pub fn install(transaction: &mut Transaction<'_>) -> Result<()> {
        require_foreign_keys(transaction)?;
        let savepoint = transaction.savepoint()?;
        journal_format::install(&savepoint)?;
        savepoint.commit()?;
        Ok(())
    }
    pub fn attach(transaction: Transaction<'connection>, scope: AuthorityScope) -> Result<Self> {
        require_foreign_keys(&transaction)?;
        journal_format::verify(&transaction)?;
        check_epoch(&transaction, &scope)?;
        Ok(Self {
            transaction,
            scope,
            touched: BTreeSet::new(),
            claimed: vec![],
            owners: vec![],
            cleared_cards: BTreeSet::new(),
            commands: vec![],
            observations: vec![],
        })
    }
    pub fn scope(&self) -> &AuthorityScope {
        &self.scope
    }
    pub fn load(&self, task: &TaskId) -> Result<TaskLifecycle> {
        persistence::load(&self.transaction, &self.scope, task)
    }
    pub fn create(&mut self, task: &TaskId, initial: InputPayload) -> Result<()> {
        identity(&task.0)?;
        let _ = TaskLifecycle::planned(task.clone(), initial.id.clone())?;
        let savepoint = self.transaction.savepoint()?;
        savepoint.execute(
            "INSERT INTO lifecycle_tasks(task_id,epoch,initial_input,revision) VALUES(?1,?2,?3,0)",
            params![task.0, self.scope.epoch.0, initial.id.as_str()],
        )?;
        savepoint.execute(
            "INSERT INTO lifecycle_inputs(task_id,input_id,body) VALUES(?1,?2,?3)",
            params![task.0, initial.id.as_str(), initial.text],
        )?;
        savepoint.commit()?;
        self.touched.insert(task.clone());
        Ok(())
    }
    pub fn put_input(&mut self, task: &TaskId, input: InputPayload) -> Result<()> {
        self.load(task)?;
        let existing: Option<String> = self
            .transaction
            .query_row(
                "SELECT body FROM lifecycle_inputs WHERE task_id=?1 AND input_id=?2",
                params![task.0, input.id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing != input.text {
                return Err(Error::InputConflict);
            }
        } else {
            self.transaction.execute(
                "INSERT INTO lifecycle_inputs(task_id,input_id,body) VALUES(?1,?2,?3)",
                params![task.0, input.id.as_str(), input.text],
            )?;
        }
        self.touched.insert(task.clone());
        Ok(())
    }
    pub fn apply(
        &mut self,
        task: &TaskId,
        expected_revision: Revision,
        command_id: &str,
        event: Event,
    ) -> Result<Applied> {
        identity(command_id)?;
        check_epoch(&self.transaction, &self.scope)?;
        let encoded = codec::encode(&event)?;
        let mut aggregate = self.load(task)?;
        let prior: Option<(Revision, String)> = self
            .transaction
            .query_row(
                "SELECT revision,event FROM lifecycle_events WHERE task_id=?1 AND command_id=?2",
                params![task.0, command_id],
                |row| Ok((read_number(row, 0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((revision, prior)) = prior {
            if revision.checked_sub(1) != Some(expected_revision) || prior != encoded {
                return Err(Error::CommandConflict);
            }
            self.touched.insert(task.clone());
            return Ok(Applied {
                revision,
                replayed: true,
            });
        }
        let old_execution = aggregate.execution().cloned();
        let old_cards: Vec<_> = aggregate
            .cards()
            .into_iter()
            .map(|(card, _)| card.clone())
            .collect();
        aggregate.apply(expected_revision, event)?;
        if old_execution.as_ref() != aggregate.execution() {
            let unresolved: bool = self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM lifecycle_dispatches WHERE task_id=?1 AND status IN('claimed','uncertain'))", params![task.0], |row| row.get(0))?;
            if unresolved {
                return Err(Error::UnresolvedDispatch);
            }
        }
        let cleared: Vec<_> = old_cards
            .into_iter()
            .filter(|card| {
                !aggregate
                    .cards()
                    .iter()
                    .any(|(current, _)| *current == card)
            })
            .collect();
        let savepoint = self.transaction.savepoint()?;
        persistence::require_inputs(&savepoint, &aggregate)?;
        persistence::save_relations(&savepoint, &aggregate)?;
        savepoint.execute(
            "INSERT INTO lifecycle_events(task_id,revision,command_id,event) VALUES(?1,?2,?3,?4)",
            params![
                task.0,
                crate::sql_number(aggregate.revision())?,
                command_id,
                encoded
            ],
        )?;
        savepoint.execute(
            "UPDATE lifecycle_tasks SET revision=?2 WHERE task_id=?1",
            params![task.0, crate::sql_number(aggregate.revision())?],
        )?;
        // A prepared intent has not crossed the host boundary; obsolete bindings can revoke it.
        savepoint.execute("UPDATE lifecycle_dispatches SET status='revoked' WHERE task_id=?1 AND status='prepared' AND generation IS NOT ?2",
            params![task.0, aggregate.execution().map(|stamp| crate::sql_number(stamp.generation())).transpose()?])?;
        savepoint.commit()?;
        for card in cleared {
            self.cleared_cards.insert((task.clone(), card));
        }
        self.touched.insert(task.clone());
        Ok(Applied {
            revision: aggregate.revision(),
            replayed: false,
        })
    }
    pub fn bind_provider(&mut self, task: &TaskId, binding: &ProviderBinding) -> Result<()> {
        let aggregate = self.load(task)?;
        let stamp = aggregate.execution().ok_or(Error::MissingIntent)?;
        let savepoint = self.transaction.savepoint()?;
        provider_binding::activate_initial(&savepoint, binding)?;
        provider_binding::bind(&savepoint, stamp, binding)?;
        savepoint.commit()?;
        self.touched.insert(task.clone());
        Ok(())
    }
    pub fn execution_binding(&self, stamp: &ExecutionStamp) -> Result<ProviderBinding> {
        if self.load(stamp.task())?.execution() != Some(stamp) {
            return Err(Error::StalePermit);
        }
        provider_binding::for_execution(&self.transaction, stamp)
    }
    pub fn reserve_owner(&mut self, stamp: &ExecutionStamp, expected_previous: u64) -> Result<()> {
        if self.owners.iter().any(|owner| owner.execution == *stamp) {
            return Err(Error::StaleOwner);
        }
        let binding = self.execution_binding(stamp)?;
        let next = expected_previous.checked_add(1).ok_or(Error::StaleOwner)?;
        let next_sql = sql_number(next)?;
        let changed = if expected_previous == 0 {
            self.transaction.execute("INSERT INTO provider_execution_owners(task_id,generation,binding_id,owner_generation) VALUES(?1,?2,?3,?4) ON CONFLICT(task_id,generation) DO NOTHING",params![stamp.task().0,sql_number(stamp.generation())?,binding.id(),next_sql])?
        } else {
            self.transaction.execute("UPDATE provider_execution_owners SET owner_generation=?4 WHERE task_id=?1 AND generation=?2 AND binding_id=?3 AND owner_generation=?5",params![stamp.task().0,sql_number(stamp.generation())?,binding.id(),next_sql,sql_number(expected_previous)?])?
        };
        if changed != 1 {
            return Err(Error::StaleOwner);
        }
        self.owners.push(OwnerGrant {
            scope: self.scope.clone(),
            execution: stamp.clone(),
            binding,
            generation: next,
        });
        Ok(())
    }
    pub fn fence_owner(&self, grant: &OwnerGrant) -> Result<()> {
        if grant.scope != self.scope || self.execution_binding(&grant.execution)? != grant.binding {
            return Err(Error::StaleOwner);
        }
        let current:Option<u64>=self.transaction.query_row("SELECT owner_generation FROM provider_execution_owners WHERE task_id=?1 AND generation=?2 AND binding_id=?3",params![grant.execution.task().0,sql_number(grant.execution.generation())?,grant.binding.id()],|row|read_number(row,0)).optional()?;
        if current != Some(grant.generation) {
            return Err(Error::StaleOwner);
        }
        Ok(())
    }
    pub fn fence_observation_owner(
        &self,
        stamp: &ExecutionStamp,
        owner_generation: u64,
    ) -> Result<()> {
        let binding = self.execution_binding(stamp)?;
        self.fence_retained_observation_owner(stamp, &binding, owner_generation)
    }
    fn fence_retained_observation_owner(
        &self,
        stamp: &ExecutionStamp,
        binding: &ProviderBinding,
        owner_generation: u64,
    ) -> Result<()> {
        let current:Option<u64>=self.transaction.query_row("SELECT owner_generation FROM provider_execution_owners WHERE task_id=?1 AND generation=?2 AND binding_id=?3",params![stamp.task().0,sql_number(stamp.generation())?,binding.id()],|row|read_number(row,0)).optional()?;
        if current != Some(owner_generation) || owner_generation == 0 {
            return Err(Error::StaleOwner);
        }
        Ok(())
    }
    pub fn revoke_provider(&mut self, binding: &ProviderBinding) -> Result<()> {
        provider_binding::revoke(&self.transaction, binding)
    }
    pub fn stage_launch(
        &mut self,
        task: &TaskId,
        placement: &Placement,
        binding: &ProviderBinding,
    ) -> Result<()> {
        let value = match placement {
            Placement::LocalDirectory(value) | Placement::RemoteWorkspace(value) => value,
        };
        if value.trim().is_empty() || value.len() > 4096 || value.contains('\0') {
            return Err(Error::InvalidInput);
        }
        if let Placement::LocalDirectory(directory) = placement {
            if !Path::new(directory).is_absolute() {
                return Err(Error::InvalidInput);
            }
            require_directory(&self.transaction, task, directory)?;
        }
        let aggregate = self.load(task)?;
        if aggregate.resource() != Some(ResourceEvidence::Reserved) {
            return Err(Error::MissingIntent);
        }
        let input = aggregate
            .pending_inputs()
            .first()
            .copied()
            .ok_or(Error::MissingInput)?;
        let stamp = aggregate.execution().ok_or(Error::MissingIntent)?;
        let savepoint = self.transaction.savepoint()?;
        provider_binding::activate_initial(&savepoint, binding)?;
        provider_binding::bind(&savepoint, stamp, binding)?;
        Self::stage(&savepoint, &aggregate, input, Some(placement))?;
        savepoint.commit()?;
        self.touched.insert(task.clone());
        Ok(())
    }
    pub fn stage_input(&mut self, task: &TaskId, input: &InputId) -> Result<()> {
        let aggregate = self.load(task)?;
        if !aggregate.pending_inputs().contains(&input) {
            return Err(Error::MissingInput);
        }
        let stamp = aggregate.execution().ok_or(Error::MissingIntent)?;
        let binding = provider_binding::for_execution(&self.transaction, stamp)?;
        provider_binding::require_active(&self.transaction, &binding)?;
        if !binding.permits(Capability::OfferInput) {
            return Err(Error::UnsupportedCapability);
        }
        Self::stage(&self.transaction, &aggregate, input, None)?;
        self.touched.insert(task.clone());
        Ok(())
    }
    fn stage(
        connection: &Connection,
        aggregate: &TaskLifecycle,
        input: &InputId,
        placement: Option<&Placement>,
    ) -> Result<()> {
        let stamp = aggregate.execution().ok_or(Error::MissingIntent)?;
        let (kind, placement_kind, value) = match placement {
            Some(Placement::LocalDirectory(value)) => {
                ("launch", Some("local"), Some(value.as_str()))
            }
            Some(Placement::RemoteWorkspace(value)) => {
                ("launch", Some("remote"), Some(value.as_str()))
            }
            None => ("input", None, None),
        };
        let existing:Option<(String,Option<String>,Option<String>)>=connection.query_row("SELECT kind,placement_kind,placement FROM lifecycle_dispatches WHERE task_id=?1 AND generation=?2 AND input_id=?3",params![aggregate.task().0,sql_number(stamp.generation())?,input.as_str()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
        if let Some((old_kind, old_placement, old_value)) = existing {
            if old_kind != kind
                || old_placement.as_deref() != placement_kind
                || old_value.as_deref() != value
            {
                return Err(Error::InputConflict);
            }
            return Ok(());
        }
        connection.execute("INSERT INTO lifecycle_dispatches(task_id,generation,input_id,kind,placement_kind,placement,status) VALUES(?1,?2,?3,?4,?5,?6,'prepared')",params![aggregate.task().0,sql_number(stamp.generation())?,input.as_str(),kind,placement_kind,value])?;
        Ok(())
    }
    pub fn put_adapter_input(
        &mut self,
        stamp: &ExecutionStamp,
        input: &InputId,
        metadata: AdapterInputMetadata,
    ) -> Result<()> {
        let aggregate = self.load(stamp.task())?;
        if aggregate.execution() != Some(stamp) {
            return Err(Error::StalePermit);
        }
        let binding = provider_binding::for_execution(&self.transaction, stamp)?;
        let savepoint = self.transaction.savepoint()?;
        savepoint.execute("INSERT INTO provider_input_metadata(task_id,binding_id,conversation_id,input_id,metadata) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(task_id,binding_id,conversation_id,input_id) DO NOTHING",params![stamp.task().0,binding.id(),stamp.conversation().as_str(),input.as_str(),metadata.bytes()])?;
        let stored:Vec<u8>=savepoint.query_row("SELECT metadata FROM provider_input_metadata WHERE task_id=?1 AND binding_id=?2 AND conversation_id=?3 AND input_id=?4",params![stamp.task().0,binding.id(),stamp.conversation().as_str(),input.as_str()],|row|row.get(0))?;
        if stored != metadata.bytes() {
            return Err(Error::InputConflict);
        }
        savepoint.commit()?;
        Ok(())
    }
    pub fn adapter_input(
        &self,
        stamp: &ExecutionStamp,
        input: &InputId,
    ) -> Result<AdapterInputMetadata> {
        let binding = provider_binding::for_execution(&self.transaction, stamp)?;
        let bytes:Option<Vec<u8>>=self.transaction.query_row("SELECT metadata FROM provider_input_metadata WHERE task_id=?1 AND binding_id=?2 AND conversation_id=?3 AND input_id=?4",params![stamp.task().0,binding.id(),stamp.conversation().as_str(),input.as_str()],|row|row.get(0)).optional()?;
        AdapterInputMetadata::new(bytes.ok_or(Error::MissingAdapterInput)?)
    }
    pub fn fence_attempt(&self, attempt: &DispatchAttempt) -> Result<()> {
        if attempt.scope != self.scope {
            return Err(Error::StalePermit);
        }
        let aggregate = self.load(attempt.execution().task())?;
        if aggregate.revision() != attempt.revision()
            || aggregate.execution() != Some(attempt.execution())
            || !dispatchable(&aggregate)
            || !aggregate.pending_inputs().contains(&&attempt.input().id)
        {
            return Err(Error::StalePermit);
        }
        if provider_binding::for_execution(&self.transaction, attempt.execution())?
            != attempt.binding
        {
            return Err(Error::BindingConflict);
        }
        provider_binding::require_active(&self.transaction, &attempt.binding)
    }
    pub fn record_command(
        &mut self,
        attempt: &DispatchAttempt,
        command: CommandEvidence,
    ) -> Result<()> {
        self.fence_attempt(attempt)?;
        let digest = evidence_digest(command.bytes());
        let savepoint = self.transaction.savepoint()?;
        savepoint.execute("INSERT INTO provider_commands(intent_id,task_id,generation,binding_id,digest,bytes) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(intent_id) DO NOTHING",params![attempt.id,attempt.stamp.task().0,sql_number(attempt.stamp.generation())?,attempt.binding.id(),digest,command.bytes()])?;
        let (stored,stored_digest):(Vec<u8>,Vec<u8>)=savepoint.query_row("SELECT bytes,digest FROM provider_commands WHERE intent_id=?1 AND task_id=?2 AND generation=?3 AND binding_id=?4",params![attempt.id,attempt.stamp.task().0,sql_number(attempt.stamp.generation())?,attempt.binding.id()],|row|Ok((row.get(0)?,row.get(1)?)))?;
        if evidence_digest(&stored) != stored_digest {
            return Err(Error::CorruptJournal);
        }
        if stored != command.bytes() {
            return Err(Error::CommandConflict);
        }
        savepoint.commit()?;
        self.touched.insert(attempt.stamp.task().clone());
        Ok(())
    }
    pub fn command_evidence(&self, attempt: &DispatchAttempt) -> Result<CommandEvidence> {
        let row:Option<(Vec<u8>,Vec<u8>)>=self.transaction.query_row("SELECT bytes,digest FROM provider_commands WHERE intent_id=?1 AND task_id=?2 AND generation=?3 AND binding_id=?4",params![attempt.id,attempt.stamp.task().0,sql_number(attempt.stamp.generation())?,attempt.binding.id()],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        let (bytes, digest) = row.ok_or(Error::MissingCommand)?;
        if evidence_digest(&bytes) != digest {
            return Err(Error::CorruptJournal);
        }
        CommandEvidence::new(bytes)
    }
    pub fn accept_observation(
        &mut self,
        stamp: &ExecutionStamp,
        cursor: u64,
        evidence: CommandEvidence,
    ) -> Result<ObservationAcceptance> {
        let aggregate = self.load(stamp.task())?;
        if aggregate.execution() != Some(stamp) {
            return Err(Error::StalePermit);
        }
        let binding = provider_binding::for_execution(&self.transaction, stamp)?;
        let prior:Option<(Vec<u8>,Vec<u8>)>=self.transaction.query_row("SELECT bytes,digest FROM provider_observations WHERE task_id=?1 AND generation=?2 AND cursor=?3 AND binding_id=?4",params![stamp.task().0,sql_number(stamp.generation())?,sql_number(cursor)?,binding.id()],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        if let Some((bytes, digest)) = prior {
            if bytes != evidence.bytes() || evidence_digest(&bytes) != digest {
                return Err(Error::ObservationConflict);
            }
            return Ok(ObservationAcceptance::Duplicate);
        }
        let head: Option<u64> = self
            .transaction
            .query_row(
                "SELECT cursor FROM provider_observation_heads WHERE task_id=?1 AND generation=?2",
                params![stamp.task().0, sql_number(stamp.generation())?],
                |row| read_number(row, 0),
            )
            .optional()?;
        if head.unwrap_or(0).checked_add(1) != Some(cursor) {
            return Err(Error::ObservationGap);
        }
        let savepoint = self.transaction.savepoint()?;
        savepoint.execute("INSERT INTO provider_observations(task_id,generation,binding_id,cursor,digest,bytes) VALUES(?1,?2,?3,?4,?5,?6)",params![stamp.task().0,sql_number(stamp.generation())?,binding.id(),sql_number(cursor)?,evidence_digest(evidence.bytes()),evidence.bytes()])?;
        savepoint.execute("INSERT INTO provider_observation_heads(task_id,generation,cursor) VALUES(?1,?2,?3) ON CONFLICT(task_id,generation) DO UPDATE SET cursor=excluded.cursor",params![stamp.task().0,sql_number(stamp.generation())?,sql_number(cursor)?])?;
        savepoint.commit()?;
        self.touched.insert(stamp.task().clone());
        Ok(ObservationAcceptance::Accepted)
    }
    pub fn put_adapter_conversation(
        &mut self,
        stamp: &ExecutionStamp,
        metadata: AdapterInputMetadata,
    ) -> Result<()> {
        let aggregate = self.load(stamp.task())?;
        if aggregate.execution() != Some(stamp) {
            return Err(Error::StalePermit);
        }
        let binding = provider_binding::for_execution(&self.transaction, stamp)?;
        let savepoint = self.transaction.savepoint()?;
        savepoint.execute("INSERT INTO provider_conversation_metadata(task_id,binding_id,conversation_id,metadata) VALUES(?1,?2,?3,?4) ON CONFLICT(task_id,binding_id,conversation_id) DO NOTHING",params![stamp.task().0,binding.id(),stamp.conversation().as_str(),metadata.bytes()])?;
        let stored:Vec<u8>=savepoint.query_row("SELECT metadata FROM provider_conversation_metadata WHERE task_id=?1 AND binding_id=?2 AND conversation_id=?3",params![stamp.task().0,binding.id(),stamp.conversation().as_str()],|row|row.get(0))?;
        if stored != metadata.bytes() {
            return Err(Error::InputConflict);
        }
        savepoint.commit()?;
        Ok(())
    }
    pub fn adapter_conversation(&self, stamp: &ExecutionStamp) -> Result<AdapterInputMetadata> {
        let binding = provider_binding::for_execution(&self.transaction, stamp)?;
        let bytes:Option<Vec<u8>>=self.transaction.query_row("SELECT metadata FROM provider_conversation_metadata WHERE task_id=?1 AND binding_id=?2 AND conversation_id=?3",params![stamp.task().0,binding.id(),stamp.conversation().as_str()],|row|row.get(0)).optional()?;
        AdapterInputMetadata::new(bytes.ok_or(Error::MissingAdapterInput)?)
    }
    /// Reissues only unclaimed intents against current facts; claimed/uncertain work needs receipts.
    pub fn adapter_state(&self, grant: &OwnerGrant) -> Result<AdapterState> {
        self.fence_owner(grant)?;
        let stamp = grant.execution();
        let conversation = self.adapter_conversation(stamp)?;
        Ok(AdapterState {
            scope: grant.scope().clone(),
            execution: grant.execution().clone(),
            binding: grant.binding().clone(),
            owner_generation: grant.generation(),
            conversation,
            checkpoint: self.normalization_checkpoint(stamp)?,
        })
    }
    pub fn lookup_adapter_inputs(
        &self,
        stamp: &ExecutionStamp,
        keys: &[Vec<u8>],
    ) -> Result<Vec<AdapterInputBinding>> {
        if keys.len() > 64 || keys.iter().any(|key| key.is_empty() || key.len() > 4096) {
            return Err(Error::MetadataCapacity);
        }
        let binding = self.execution_binding(stamp)?;
        let mut statement=self.transaction.prepare("SELECT input_id,metadata FROM provider_input_metadata WHERE task_id=?1 AND binding_id=?2 AND conversation_id=?3 AND metadata=?4")?;
        let mut inputs = Vec::with_capacity(keys.len());
        for key in keys {
            let row: Option<(String, Vec<u8>)> = statement
                .query_row(
                    params![
                        stamp.task().0,
                        binding.id(),
                        stamp.conversation().as_str(),
                        key
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let (input, bytes) = row.ok_or(Error::MissingAdapterInput)?;
            inputs.push(AdapterInputBinding {
                input: InputId::new(input)?,
                metadata: AdapterInputMetadata::new(bytes)?,
            });
        }
        Ok(inputs)
    }
    pub fn normalization_checkpoint(
        &self,
        stamp: &ExecutionStamp,
    ) -> Result<Option<NormalizationCheckpoint>> {
        let binding = self.execution_binding(stamp)?;
        self.retained_normalization_checkpoint(stamp, &binding)
    }
    fn retained_normalization_checkpoint(
        &self,
        stamp: &ExecutionStamp,
        binding: &ProviderBinding,
    ) -> Result<Option<NormalizationCheckpoint>> {
        let row:Option<(u64,Vec<u8>,Vec<u8>)>=self.transaction.query_row("SELECT cursor,bytes,digest FROM provider_normalization_checkpoints WHERE task_id=?1 AND generation=?2 AND binding_id=?3",params![stamp.task().0,sql_number(stamp.generation())?,binding.id()],|row|Ok((read_number(row,0)?,row.get(1)?,row.get(2)?))).optional()?;
        row.map(|(cursor,bytes,digest)| {
            if bytes.is_empty() || bytes.len()>65536 || evidence_digest(&bytes)!=digest {return Err(Error::CorruptJournal);}
            let exists:bool=self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM provider_observations WHERE task_id=?1 AND generation=?2 AND binding_id=?3 AND cursor=?4)",params![stamp.task().0,sql_number(stamp.generation())?,binding.id(),sql_number(cursor)?],|row|row.get(0))?;
            if !exists {return Err(Error::CorruptJournal);}
            Ok(NormalizationCheckpoint{cursor,bytes})
        }).transpose()
    }
    pub fn put_normalization_checkpoint(
        &mut self,
        stamp: &ExecutionStamp,
        cursor: u64,
        expected_previous: Option<u64>,
        bytes: Vec<u8>,
    ) -> Result<()> {
        if cursor == 0 || bytes.is_empty() || bytes.len() > 65536 {
            return Err(Error::MetadataCapacity);
        }
        let binding = match self.execution_binding(stamp) {
            Ok(binding) => binding,
            Err(Error::StalePermit) => {
                self.checkpoint_binding_for_accepted_observation(stamp, cursor)?
            }
            Err(error) => return Err(error),
        };
        let previous = self.retained_normalization_checkpoint(stamp, &binding)?;
        if let Some(previous) = &previous {
            if previous.cursor == cursor {
                return if previous.bytes == bytes {
                    Ok(())
                } else {
                    Err(Error::CheckpointConflict)
                };
            }
            if previous.cursor > cursor {
                return Err(Error::CheckpointConflict);
            }
        }
        if previous.as_ref().map(|previous| previous.cursor) != expected_previous {
            return Err(Error::CheckpointConflict);
        }
        let head: Option<u64> = self
            .transaction
            .query_row(
                "SELECT cursor FROM provider_observation_heads WHERE task_id=?1 AND generation=?2",
                params![stamp.task().0, sql_number(stamp.generation())?],
                |row| read_number(row, 0),
            )
            .optional()?;
        if head != Some(cursor) {
            return Err(Error::CheckpointConflict);
        }
        let changed = if let Some(previous) = expected_previous {
            self.transaction.execute("UPDATE provider_normalization_checkpoints SET cursor=?4,digest=?5,bytes=?6 WHERE task_id=?1 AND generation=?2 AND binding_id=?3 AND cursor=?7",params![stamp.task().0,sql_number(stamp.generation())?,binding.id(),sql_number(cursor)?,evidence_digest(&bytes),bytes,sql_number(previous)?])?
        } else {
            self.transaction.execute("INSERT INTO provider_normalization_checkpoints(task_id,generation,binding_id,cursor,digest,bytes) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(task_id,generation) DO NOTHING",params![stamp.task().0,sql_number(stamp.generation())?,binding.id(),sql_number(cursor)?,evidence_digest(&bytes),bytes])?
        };
        if changed != 1 {
            return Err(Error::CheckpointConflict);
        }
        Ok(())
    }
    pub fn recover_prepared(&mut self, task: &TaskId) -> Result<()> {
        self.load(task)?;
        self.touched.insert(task.clone());
        Ok(())
    }
    pub fn unresolved(&self, task: &TaskId) -> Result<Vec<UnresolvedDispatch>> {
        self.load(task)?;
        let mut statement = self.transaction.prepare("SELECT id,generation,input_id,status FROM lifecycle_dispatches WHERE task_id=?1 AND status IN('claimed','uncertain') ORDER BY id")?;
        let rows = statement.query_map(params![task.0], |row| {
            Ok((
                row.get(0)?,
                read_number(row, 1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (intent_id, generation, input, status) = row?;
            Ok(UnresolvedDispatch {
                intent_id,
                generation,
                input: InputId::new(input)?,
                uncertain_receipt: status == "uncertain",
            })
        })
        .collect()
    }
    pub fn claim(&mut self, permit: DispatchPermit) -> Result<()> {
        if permit.scope != self.scope {
            return Err(Error::StalePermit);
        }
        let aggregate = self.load(permit.stamp.task())?;
        if aggregate.revision() != permit.revision
            || aggregate.execution() != Some(&permit.stamp)
            || !dispatchable(&aggregate)
            || !aggregate.pending_inputs().contains(&&permit.input)
        {
            return Err(Error::StalePermit);
        }
        if next_offer(&self.transaction, &aggregate)?.map(|(id, _)| id) != Some(permit.id) {
            return Err(Error::OfferOrder);
        }
        let record: Option<(String, Option<String>, Option<String>, String, String)> = self.transaction.query_row(
            "SELECT d.kind,d.placement_kind,d.placement,i.body,d.status FROM lifecycle_dispatches d JOIN lifecycle_inputs i USING(task_id,input_id) WHERE d.id=?1 AND d.task_id=?2 AND d.generation=?3 AND d.input_id=?4",
            params![permit.id, permit.stamp.task().0, crate::sql_number(permit.stamp.generation())?, permit.input.as_str()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))).optional()?;
        let (kind, placement_kind, placement_value, text, status) =
            record.ok_or(Error::StalePermit)?;
        let binding = provider_binding::for_execution(&self.transaction, &permit.stamp)?;
        provider_binding::require_active(&self.transaction, &binding)?;
        if kind == "input" && !binding.permits(Capability::OfferInput) {
            return Err(Error::UnsupportedCapability);
        }
        if status != "prepared"
            || (kind == "input" && aggregate.resource() != Some(ResourceEvidence::Available))
        {
            return Err(Error::StalePermit);
        }
        let input = InputPayload::new(permit.input, text)?;
        let payload = match (kind.as_str(), placement_kind.as_deref(), placement_value) {
            ("launch", Some("local"), Some(directory)) => {
                require_directory(&self.transaction, permit.stamp.task(), &directory)?;
                DispatchPayload::Launch {
                    input,
                    placement: Placement::LocalDirectory(directory),
                }
            }
            ("launch", Some("remote"), Some(workspace)) => DispatchPayload::Launch {
                input,
                placement: Placement::RemoteWorkspace(workspace),
            },
            ("input", None, None) => DispatchPayload::Input(input),
            _ => return Err(Error::CorruptJournal),
        };
        let changed = self.transaction.execute("UPDATE lifecycle_dispatches SET status='claimed',claimed_revision=?2 WHERE id=?1 AND status='prepared'", params![permit.id, crate::sql_number(permit.revision)?])?;
        if changed != 1 {
            return Err(Error::StalePermit);
        }
        self.touched.insert(permit.stamp.task().clone());
        self.claimed.push(DispatchAttempt {
            id: permit.id,
            scope: self.scope.clone(),
            revision: permit.revision,
            stamp: permit.stamp,
            payload,
            binding,
        });
        Ok(())
    }
    pub fn record_receipt(
        &mut self,
        attempt: DispatchAttempt,
        receipt: DispatchReceipt,
    ) -> Result<()> {
        if attempt.scope != self.scope {
            return Err(Error::StalePermit);
        }
        check_epoch(&self.transaction, &self.scope)?;
        let changed = self.transaction.execute("UPDATE lifecycle_dispatches SET status=?2 WHERE id=?1 AND task_id=?3 AND generation=?4 AND input_id=?5 AND claimed_revision=?6 AND status='claimed'",
            params![attempt.id, match receipt { DispatchReceipt::Handed => "handed", DispatchReceipt::Rejected => "rejected", DispatchReceipt::Uncertain => "uncertain" },
            attempt.stamp.task().0, crate::sql_number(attempt.stamp.generation())?, attempt.input().id.as_str(), crate::sql_number(attempt.revision)?])?;
        if changed != 1 {
            return Err(Error::StalePermit);
        }
        self.touched.insert(attempt.stamp.task().clone());
        Ok(())
    }
    pub fn commit(self) -> Result<Committed> {
        check_epoch(&self.transaction, &self.scope)?;
        for attempt in &self.claimed {
            self.fence_attempt(attempt)?;
        }
        for owner in &self.owners {
            self.fence_owner(owner)?;
        }
        for command in &self.commands {
            self.fence_committed_command(command)?;
        }
        for observation in &self.observations {
            self.verify_observation_commit(observation)?;
        }
        for (task, card) in &self.cleared_cards {
            let aggregate = self.load(task)?;
            if aggregate
                .cards()
                .iter()
                .any(|(current, _)| *current == card)
            {
                continue;
            }
            let settled: bool = self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM interactions WHERE task_id=?1 AND id=?2 AND (answer IS NOT NULL OR withdrawn<>0))", params![task.0, card.0], |row| row.get(0))?;
            if !settled {
                return Err(Error::InvalidCard);
            }
        }
        let mut permits = vec![];
        for task in &self.touched {
            let aggregate = self.load(task)?;
            persistence::require_inputs(&self.transaction, &aggregate)?;
            persistence::require_intents(&self.transaction, &aggregate)?;
            if matches!(
                aggregate.phase(),
                TaskPhase::Completed | TaskPhase::Cancelled
            ) && !self.unresolved(task)?.is_empty()
            {
                return Err(Error::UnresolvedDispatch);
            }
            if !dispatchable(&aggregate) {
                continue;
            }
            let stamp = aggregate.execution().ok_or(Error::MissingIntent)?;
            let binding = provider_binding::for_execution(&self.transaction, stamp)?;
            if let Some((id, input)) = next_offer(&self.transaction, &aggregate)? {
                match provider_binding::require_active(&self.transaction, &binding) {
                    Ok(()) => {}
                    Err(Error::BindingRevoked) => continue,
                    Err(error) => return Err(error),
                }
                permits.push(DispatchPermit {
                    id,
                    scope: self.scope.clone(),
                    revision: aggregate.revision(),
                    stamp: stamp.clone(),
                    input,
                });
            }
        }
        self.transaction.commit()?;
        Ok(Committed {
            permits,
            attempts: self.claimed,
            owners: self.owners,
            commands: self.commands,
            observations: self.observations,
        })
    }
}

fn identity(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 256 || value.contains('\0') {
        Err(Error::InvalidInput)
    } else {
        Ok(())
    }
}
fn require_foreign_keys(connection: &Connection) -> Result<()> {
    if connection.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))? != 1 {
        return Err(Error::InvalidInput);
    }
    Ok(())
}
fn check_epoch(connection: &Connection, scope: &AuthorityScope) -> Result<()> {
    let epoch: String = connection.query_row(
        "SELECT data_epoch FROM domain_meta WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    if epoch != scope.epoch.0 {
        return Err(Error::WrongScope);
    }
    Ok(())
}
fn dispatchable(aggregate: &TaskLifecycle) -> bool {
    matches!(
        aggregate.resource(),
        Some(ResourceEvidence::Reserved | ResourceEvidence::Available)
    ) && !matches!(
        aggregate.phase(),
        TaskPhase::Finishing | TaskPhase::Completed | TaskPhase::Cancelled
    ) && !aggregate
        .cards()
        .iter()
        .any(|(_, reason)| *reason == WaitReason::Pause)
}

fn require_directory(connection: &Connection, task: &TaskId, directory: &str) -> Result<()> {
    let exact: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_worktrees WHERE task_id=?1 AND dir=?2)",
        params![task.0, directory],
        |row| row.get(0),
    )?;
    if !exact {
        return Err(Error::MissingIntent);
    }
    Ok(())
}

fn next_offer(
    connection: &Connection,
    aggregate: &TaskLifecycle,
) -> Result<Option<(i64, InputId)>> {
    let stamp = aggregate.execution().ok_or(Error::MissingIntent)?;
    for input in aggregate.pending_inputs() {
        let row: Option<(i64, String, String)> = connection.query_row("SELECT id,status,kind FROM lifecycle_dispatches WHERE task_id=?1 AND generation=?2 AND input_id=?3", params![aggregate.task().0, crate::sql_number(stamp.generation())?, input.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()?;
        let (id, status, kind) = row.ok_or(Error::MissingIntent)?;
        match status.as_str() {
            "handed" => continue,
            "prepared"
                if kind == "launch"
                    || aggregate.resource() == Some(ResourceEvidence::Available) =>
            {
                return Ok(Some((id, input.clone())));
            }
            _ => return Ok(None),
        }
    }
    Ok(None)
}

fn sql_number(value: u64) -> Result<i64> {
    if value > alondra_foundation::MAX_REVISION {
        return Err(Error::InvalidInput);
    }
    i64::try_from(value).map_err(|_| Error::InvalidInput)
}
fn read_number(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value)
        .ok()
        .filter(|value| *value <= alondra_foundation::MAX_REVISION)
        .ok_or(rusqlite::Error::IntegralValueOutOfRange(index, value))
}

#[cfg(test)]
mod numeric_tests {
    use super::*;
    #[test]
    fn sqlite_numbers_never_truncate_or_accept_out_of_range_journal_metadata() {
        assert_eq!(sql_number(0).unwrap(), 0);
        assert_eq!(
            sql_number(alondra_foundation::MAX_REVISION).unwrap(),
            9_007_199_254_740_991_i64
        );
        assert!(matches!(
            sql_number(alondra_foundation::MAX_REVISION + 1),
            Err(Error::InvalidInput)
        ));
        assert!(matches!(sql_number(u64::MAX), Err(Error::InvalidInput)));
        let connection = Connection::open_in_memory().unwrap();
        for invalid in [-1_i64, 9_007_199_254_740_992_i64] {
            assert!(
                connection
                    .query_row("SELECT ?1", params![invalid], |row| read_number(row, 0))
                    .is_err()
            );
        }
        assert_eq!(
            connection
                .query_row("SELECT 9007199254740991", [], |row| read_number(row, 0))
                .unwrap(),
            alondra_foundation::MAX_REVISION
        );
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct AdapterInputMetadata {
    bytes: Vec<u8>,
}
impl AdapterInputMetadata {
    pub fn new(bytes: Vec<u8>) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(Error::InvalidInput);
        }
        Ok(Self { bytes })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
#[derive(Debug)]
pub struct AdapterState {
    scope: AuthorityScope,
    execution: ExecutionStamp,
    binding: ProviderBinding,
    owner_generation: u64,
    conversation: AdapterInputMetadata,
    checkpoint: Option<NormalizationCheckpoint>,
}
impl AdapterState {
    /// Checks snapshot provenance only; current SQL ownership still requires a transaction fence.
    pub fn validate_owner(&self, grant: &OwnerGrant) -> Result<()> {
        if self.scope != *grant.scope()
            || self.execution != *grant.execution()
            || self.binding != *grant.binding()
            || self.owner_generation != grant.generation()
        {
            return Err(Error::StaleOwner);
        }
        Ok(())
    }
    pub fn conversation(&self) -> &[u8] {
        self.conversation.bytes()
    }
    pub fn checkpoint(&self) -> Option<&NormalizationCheckpoint> {
        self.checkpoint.as_ref()
    }
}
#[derive(Debug)]
pub struct AdapterInputBinding {
    input: InputId,
    metadata: AdapterInputMetadata,
}
impl AdapterInputBinding {
    pub fn input(&self) -> &InputId {
        &self.input
    }
    pub fn metadata(&self) -> &[u8] {
        self.metadata.bytes()
    }
}
#[derive(Debug)]
pub struct NormalizationCheckpoint {
    cursor: u64,
    bytes: Vec<u8>,
}
impl NormalizationCheckpoint {
    pub fn cursor(&self) -> u64 {
        self.cursor
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
#[derive(Debug)]
pub struct CommandEvidence(Vec<u8>);
impl CommandEvidence {
    pub fn new(bytes: Vec<u8>) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_PAYLOAD {
            return Err(Error::InvalidInput);
        }
        Ok(Self(bytes))
    }
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum ObservationAcceptance {
    Accepted,
    Duplicate,
}
fn evidence_digest(bytes: &[u8]) -> Vec<u8> {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).to_vec()
}
