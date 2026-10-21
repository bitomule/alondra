//! Commit values attest only to SQL persistence, never application publication or policy.
use super::*;
use std::collections::BTreeMap;

/// What a transaction staged; only `LifecycleTransaction::commit`, after the SQL commit
/// returned, turns it into the public value.
#[derive(Debug)]
pub(super) struct StagedCommand {
    attempt: DispatchAttempt,
    owner_generation: u64,
    evidence: CommandEvidence,
}
#[derive(Debug)]
pub(super) struct StagedObservation {
    scope: AuthorityScope,
    execution: ExecutionStamp,
    binding: ProviderBinding,
    owner_generation: u64,
    cursor: u64,
    digest: Vec<u8>,
    /// The runtime cursor of the task before this observation, while the domain has not yet
    /// recorded the facts it was normalized to.
    unsettled: Option<u64>,
}
impl StagedCommand {
    pub(super) fn seal(self) -> CommittedCommand {
        CommittedCommand { staged: self }
    }
}
impl StagedObservation {
    pub(super) fn seal(self) -> CommittedObservation {
        CommittedObservation { staged: self }
    }
    fn matches_evidence(&self, raw: &[u8]) -> bool {
        !raw.is_empty() && raw.len() <= MAX_PAYLOAD && evidence_digest(raw) == self.digest
    }
}

/// Exact command evidence issued only after the owning SQL transaction commits.
/// ```
/// use alondra_store::CommittedCommand;
/// fn inspect(value: CommittedCommand) -> CommittedCommand { let _ = value.bytes(); value }
/// ```
/// ```compile_fail,E0451
/// use alondra_store::CommittedCommand;
/// fn reconstruct(value: CommittedCommand) -> CommittedCommand { CommittedCommand { ..value } }
/// ```
/// ```compile_fail,E0277
/// use alondra_store::CommittedCommand;
/// fn requires_clone<T: Clone>() {}
/// requires_clone::<CommittedCommand>();
/// ```
/// The staged draft is not nameable outside the store, so nothing else can seal it:
/// ```compile_fail,E0432
/// use alondra_store::StagedCommand;
/// ```
#[derive(Debug)]
pub struct CommittedCommand {
    staged: StagedCommand,
}
impl CommittedCommand {
    pub fn attempt(&self) -> &DispatchAttempt {
        &self.staged.attempt
    }
    pub fn owner_generation(&self) -> u64 {
        self.staged.owner_generation
    }
    pub fn bytes(&self) -> &[u8] {
        self.staged.evidence.bytes()
    }
}

/// A committed raw observation and checkpoint, not a certificate of client publication.
/// ```
/// use alondra_store::CommittedObservation;
/// fn inspect(value: CommittedObservation) -> CommittedObservation { let _ = value.cursor(); value }
/// ```
/// ```compile_fail,E0451
/// use alondra_store::CommittedObservation;
/// fn reconstruct(value: CommittedObservation) -> CommittedObservation { CommittedObservation { ..value } }
/// ```
/// ```compile_fail,E0277
/// use alondra_store::CommittedObservation;
/// fn requires_clone<T: Clone>() {}
/// requires_clone::<CommittedObservation>();
/// ```
/// ```compile_fail,E0432
/// use alondra_store::StagedObservation;
/// ```
#[derive(Debug)]
pub struct CommittedObservation {
    staged: StagedObservation,
}
impl CommittedObservation {
    pub fn scope(&self) -> &AuthorityScope {
        &self.staged.scope
    }
    pub fn execution(&self) -> &ExecutionStamp {
        &self.staged.execution
    }
    pub fn binding(&self) -> &ProviderBinding {
        &self.staged.binding
    }
    pub fn owner_generation(&self) -> u64 {
        self.staged.owner_generation
    }
    pub fn cursor(&self) -> u64 {
        self.staged.cursor
    }
    pub fn matches_evidence(&self, raw: &[u8]) -> bool {
        self.staged.matches_evidence(raw)
    }
}

impl LifecycleTransaction<'_> {
    /// Consumes the claim; no submitted-command value escapes before commit succeeds.
    pub fn record_owned_command(
        &mut self,
        attempt: DispatchAttempt,
        owner_generation: u64,
        evidence: CommandEvidence,
    ) -> Result<()> {
        staging!(self, {
            self.fence_attempt(&attempt)?;
            self.fence_observation_owner(attempt.execution(), owner_generation)?;
            let retained = CommandEvidence::new(evidence.bytes().to_vec())?;
            self.record_command(&attempt, evidence)?;
            self.commands.push(StagedCommand {
                attempt,
                owner_generation,
                evidence: retained,
            });
            Ok(())
        })
    }
    pub fn fence_committed_command(&self, command: &CommittedCommand) -> Result<()> {
        self.fence_staged_command(&command.staged)
    }
    pub(super) fn fence_staged_command(&self, command: &StagedCommand) -> Result<()> {
        staging!(self, {
            self.fence_attempt(&command.attempt)?;
            self.fence_observation_owner(command.attempt.execution(), command.owner_generation)?;
            if self.command_evidence(&command.attempt)?.bytes() != command.evidence.bytes() {
                return Err(Error::CommandConflict);
            }
            Ok(())
        })
    }
    /// Receipt recording must still work after a possible offer whose authority was revoked.
    pub fn record_committed_receipt(
        &mut self,
        command: CommittedCommand,
        receipt: DispatchReceipt,
    ) -> Result<()> {
        staging!(self, {
            self.record_receipt(command.staged.attempt, receipt)
        })
    }
    /// The commit also requires the durable normalization checkpoint to cover this raw cursor.
    pub fn accept_owned_observation(
        &mut self,
        execution: &ExecutionStamp,
        owner_generation: u64,
        cursor: u64,
        evidence: CommandEvidence,
    ) -> Result<ObservationAcceptance> {
        staging!(self, {
            self.fence_observation_owner(execution, owner_generation)?;
            let binding = self.execution_binding(execution)?;
            let digest = evidence_digest(evidence.bytes());
            let before = self.load(execution.task())?.cursor().unwrap_or(0);
            let acceptance = self.accept_observation(execution, cursor, evidence)?;
            if !self
                .observations
                .iter()
                .any(|prior| prior.execution == *execution && prior.cursor == cursor)
            {
                self.observations.push(StagedObservation {
                    scope: self.scope.clone(),
                    execution: execution.clone(),
                    binding,
                    owner_generation,
                    cursor,
                    digest,
                    unsettled: (acceptance == ObservationAcceptance::Accepted).then_some(before),
                });
            }
            Ok(acceptance)
        })
    }
    /// The domain recorded the `facts` an accepted observation was normalized to: the task's
    /// runtime cursor moved by exactly that many facts since the observation was accepted, or the
    /// observation brought none and its execution was retired by a decision of the domain. Until
    /// then the transaction cannot commit.
    pub fn settle_observation(&mut self, execution: &ExecutionStamp, cursor: u64, facts: usize) -> Result<()> {
        staging!(self, {
            let now = self.load(execution.task())?;
            let observation = self.observations.iter_mut()
                .find(|observation| observation.execution == *execution && observation.cursor == cursor)
                .ok_or(Error::StalePermit)?;
            let before = observation.unsettled.ok_or(Error::ObservationConflict)?;
            let moved = u64::try_from(facts).map_err(|_| Error::InvalidInput)?;
            let current = now.execution() == Some(execution);
            if (current && now.cursor().unwrap_or(0) != before + moved) || (!current && moved != 0) {
                return Err(Error::UnsettledObservation);
            }
            observation.unsettled = None;
            Ok(())
        })
    }
    pub(super) fn checkpoint_binding_for_accepted_observation(
        &self,
        execution: &ExecutionStamp,
        cursor: u64,
    ) -> Result<ProviderBinding> {
        let observation = self.observations.iter().find(|observation| {
            observation.scope == self.scope
                && observation.execution == *execution
                && observation.cursor == cursor
        }).ok_or(Error::StalePermit)?;
        self.verify_retained_observation(observation)?;
        Ok(observation.binding.clone())
    }
    fn verify_retained_observation(&self, observation: &StagedObservation) -> Result<()> {
        if observation.scope != self.scope
            || provider_binding::for_execution(&self.transaction, &observation.execution)?
                != observation.binding
        {
            return Err(Error::ObservationConflict);
        }
        self.fence_retained_observation_owner(
            &observation.execution,
            &observation.binding,
            observation.owner_generation,
        )?;
        let raw:Option<(Vec<u8>,Vec<u8>)>=self.transaction.query_row(
            "SELECT bytes,digest FROM provider_observations WHERE task_id=?1 AND generation=?2 AND binding_id=?3 AND cursor=?4 AND length(bytes) BETWEEN 1 AND 1048576 AND length(digest)=32",
            params![observation.execution.task().0,sql_number(observation.execution.generation())?,observation.binding.id(),sql_number(observation.cursor)?],
            |row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        let (bytes, digest) = raw.ok_or(Error::ObservationConflict)?;
        let released = bytes == NORMALIZED_EVIDENCE;
        if digest != observation.digest || !(released || observation.matches_evidence(&bytes)) {
            return Err(Error::ObservationConflict);
        }
        Ok(())
    }
    /// Leaves only the digest of every observation of this transaction's executions that its
    /// checkpoint is now past, from the one before the first it staged: earlier ones already have.
    pub(super) fn release_normalized_evidence(&self) -> Result<()> {
        let mut from = BTreeMap::<(&TaskId, u64), u64>::new();
        for observation in &self.observations {
            let key = (observation.execution.task(), observation.execution.generation());
            let first = observation.cursor.saturating_sub(1).max(1);
            from.entry(key).and_modify(|cursor| *cursor = (*cursor).min(first)).or_insert(first);
        }
        let mut release = self.transaction.prepare_cached(
            "UPDATE provider_observations SET bytes=?4 WHERE task_id=?1 AND generation=?2 AND cursor>=?3 AND (length(bytes)>1 OR bytes IS NOT ?4)
             AND cursor<(SELECT cursor FROM provider_normalization_checkpoints c WHERE c.task_id=?1 AND c.generation=?2)",
        )?;
        for ((task, generation), cursor) in from {
            release.execute(params![task.0, sql_number(generation)?, sql_number(cursor)?, NORMALIZED_EVIDENCE])?;
        }
        Ok(())
    }
    pub(super) fn verify_observation_commit(
        &self,
        observation: &StagedObservation,
    ) -> Result<()> {
        self.verify_retained_observation(observation)?;
        if self
            .retained_normalization_checkpoint(&observation.execution, &observation.binding)?
            .is_none_or(|checkpoint| checkpoint.cursor() < observation.cursor)
        {
            return Err(Error::CheckpointConflict);
        }
        if observation.unsettled.is_some() {
            return Err(Error::UnsettledObservation);
        }
        Ok(())
    }
}
