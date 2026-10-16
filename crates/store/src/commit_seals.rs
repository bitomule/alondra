//! Commit values attest only to SQL persistence, never application publication or policy.
use super::*;

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
#[derive(Debug)]
pub struct CommittedCommand {
    attempt: DispatchAttempt,
    owner_generation: u64,
    evidence: CommandEvidence,
}
impl CommittedCommand {
    pub fn attempt(&self) -> &DispatchAttempt {
        &self.attempt
    }
    pub fn owner_generation(&self) -> u64 {
        self.owner_generation
    }
    pub fn bytes(&self) -> &[u8] {
        self.evidence.bytes()
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
#[derive(Debug)]
pub struct CommittedObservation {
    scope: AuthorityScope,
    execution: ExecutionStamp,
    binding: ProviderBinding,
    owner_generation: u64,
    cursor: u64,
    digest: Vec<u8>,
}
impl CommittedObservation {
    pub fn scope(&self) -> &AuthorityScope {
        &self.scope
    }
    pub fn execution(&self) -> &ExecutionStamp {
        &self.execution
    }
    pub fn binding(&self) -> &ProviderBinding {
        &self.binding
    }
    pub fn owner_generation(&self) -> u64 {
        self.owner_generation
    }
    pub fn cursor(&self) -> u64 {
        self.cursor
    }
    pub fn matches_evidence(&self, raw: &[u8]) -> bool {
        !raw.is_empty() && raw.len() <= MAX_PAYLOAD && evidence_digest(raw) == self.digest
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
        self.fence_attempt(&attempt)?;
        self.fence_observation_owner(attempt.execution(), owner_generation)?;
        let retained = CommandEvidence::new(evidence.bytes().to_vec())?;
        self.record_command(&attempt, evidence)?;
        self.commands.push(CommittedCommand {
            attempt,
            owner_generation,
            evidence: retained,
        });
        Ok(())
    }
    pub fn fence_committed_command(&self, command: &CommittedCommand) -> Result<()> {
        self.fence_attempt(&command.attempt)?;
        self.fence_observation_owner(command.attempt.execution(), command.owner_generation)?;
        if self.command_evidence(&command.attempt)?.bytes() != command.bytes() {
            return Err(Error::CommandConflict);
        }
        Ok(())
    }
    /// Receipt recording must still work after a possible offer whose authority was revoked.
    pub fn record_committed_receipt(
        &mut self,
        command: CommittedCommand,
        receipt: DispatchReceipt,
    ) -> Result<()> {
        self.record_receipt(command.attempt, receipt)
    }
    /// The commit also requires the durable normalization checkpoint to cover this raw cursor.
    pub fn accept_owned_observation(
        &mut self,
        execution: &ExecutionStamp,
        owner_generation: u64,
        cursor: u64,
        evidence: CommandEvidence,
    ) -> Result<ObservationAcceptance> {
        self.fence_observation_owner(execution, owner_generation)?;
        let binding = self.execution_binding(execution)?;
        let digest = evidence_digest(evidence.bytes());
        let acceptance = self.accept_observation(execution, cursor, evidence)?;
        if !self
            .observations
            .iter()
            .any(|prior| prior.execution == *execution && prior.cursor == cursor)
        {
            self.observations.push(CommittedObservation {
                scope: self.scope.clone(),
                execution: execution.clone(),
                binding,
                owner_generation,
                cursor,
                digest,
            });
        }
        Ok(acceptance)
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
    fn verify_retained_observation(&self, observation: &CommittedObservation) -> Result<()> {
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
        if digest != observation.digest || !observation.matches_evidence(&bytes) {
            return Err(Error::ObservationConflict);
        }
        Ok(())
    }
    pub(super) fn verify_observation_commit(
        &self,
        observation: &CommittedObservation,
    ) -> Result<()> {
        self.verify_retained_observation(observation)?;
        if self
            .retained_normalization_checkpoint(&observation.execution, &observation.binding)?
            .is_none_or(|checkpoint| checkpoint.cursor() < observation.cursor)
        {
            return Err(Error::CheckpointConflict);
        }
        Ok(())
    }
}
