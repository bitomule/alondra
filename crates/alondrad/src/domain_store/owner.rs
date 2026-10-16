//! The one SQL transaction that stages domain commands, provider observations and the lifecycle
//! journal, and commits them together. Only `LifecycleTransaction::commit` commits it, and only
//! that commit issues the store's seals.
#![cfg_attr(not(test), expect(dead_code, reason = "the SDK host drives it from the vertical slice"))]
use super::*;
use alondra_agent_application::ObservationNormalizer;
use alondra_foundation::{ExecutionStamp, InputId};
use alondra_provider_contract::{OrderedObservation, ProviderBinding};
use alondra_store::{
    AuthorityScope, CommandEvidence, Committed, InputPayload, LifecycleTransaction,
    ObservationAcceptance,
};

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
        let prepared = self.journal.inspect(|| {
            if matches!(envelope.command, Command::CreateDocument { .. } | Command::ReviseDocument { .. }) {
                return Err(DomainError::new(
                    "invalid_input",
                    "a document's content is verified and stored outside this transaction",
                )
                .into());
            }
            command::stage(conn, cached, actor, envelope, None, true, attached)
        })?;
        self.draft.record(&envelope.command, &envelope.command_id, prepared.commit);
        Ok(&self.draft.outcomes.last().expect("outcome just recorded").outcome)
    }

    /// Raw provider evidence, its normalized facts and checkpoint.
    pub(crate) fn stage_observation<N: ObservationNormalizer>(
        &mut self,
        binding: &ProviderBinding,
        owner_generation: u64,
        batch: &OrderedObservation,
        normalizer: &N,
    ) -> std::result::Result<ObservationAcceptance, StoreError> {
        alondra_agent_application::stage_observation(
            &mut self.journal,
            binding,
            owner_generation,
            batch,
            normalizer,
        )
        .map_err(application_error)
    }

    /// Accepts the raw observation that reports the released resource, then retires its
    /// execution with the continuation that waits for the next one. The problem card is settled
    /// by a guarded command in this same transaction, or the commit refuses.
    pub(crate) fn retire_released(
        &mut self,
        execution: &ExecutionStamp,
        owner_generation: u64,
        cursor: u64,
        evidence: Vec<u8>,
        checkpoint: Vec<u8>,
        problem_card: alondra_foundation::InteractionId,
        continuation: InputPayload,
    ) -> std::result::Result<(), StoreError> {
        let task = execution.task();
        let evidence = self.journal.inspect(|| CommandEvidence::new(evidence))?;
        let previous = self
            .journal
            .normalization_checkpoint(execution)?
            .map(|checkpoint| checkpoint.cursor());
        self.journal.accept_owned_observation(execution, owner_generation, cursor, evidence)?;
        let input: InputId = continuation.id().clone();
        self.journal.put_input(task, continuation)?;
        let revision = self.journal.load(task)?.revision();
        self.journal.apply(
            task,
            revision,
            &format!("retire-{}-{cursor}", execution.generation()),
            alondra_domain::Event::RetireReleasedExecution { problem_card, input },
        )?;
        self.journal
            .put_normalization_checkpoint(execution, cursor, previous, checkpoint)?;
        Ok(())
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
