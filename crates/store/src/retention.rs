//! How long raw provider observations keep their bytes. The normalizer reads an observation only
//! while its checkpoint has not passed it; after that only its digest is read, to answer a copy the
//! host sends again, and only while its execution is its task's current one.
//!
//! A provider command is read only while its dispatch is claimed, to tell a copy apart and to send
//! it again after a crash, and no claim outlives its execution. An input's text is read only to
//! send it to the current execution; once its task is done or cancelled, replaying the journal
//! needs only the input's id.
use super::*;

/// What one bounded retention pass changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EvidenceRetention {
    /// Normalized observations that kept only their digest.
    pub released: usize,
    /// Digests of executions that are no longer their task's current one, removed.
    pub removed: usize,
    /// Commands of executions that are no longer their task's current one, removed.
    pub commands_removed: usize,
    /// Texts of inputs of done or cancelled tasks, let go.
    pub texts_released: usize,
}

impl LifecycleTransaction<'_> {
    /// Releases up to `limit` normalized observations still holding their bytes, and removes up
    /// to `limit` released ones of executions no longer current. The checkpoint's observation and
    /// the ones not normalized yet keep their bytes. Removes up to `limit` commands of executions
    /// no longer current, and lets go of up to `limit` input texts of ended tasks.
    pub fn retain_evidence(&mut self, limit: usize) -> Result<EvidenceRetention> {
        staging!(self, {
            let limit = i64::try_from(limit).map_err(|_| Error::InvalidInput)?;
            let released = self.transaction.execute(
                "UPDATE provider_observations SET bytes=?1 WHERE rowid IN(
                   SELECT o.rowid FROM provider_observations o JOIN provider_normalization_checkpoints c ON c.task_id=o.task_id AND c.generation=o.generation
                   WHERE o.cursor<c.cursor AND (length(o.bytes)>1 OR o.bytes IS NOT ?1) LIMIT ?2)",
                params![NORMALIZED_EVIDENCE, limit],
            )?;
            let removed = self.transaction.execute(
                "DELETE FROM provider_observations WHERE rowid IN(
                   SELECT o.rowid FROM provider_observations o WHERE length(o.bytes)=1 AND o.bytes=?1
                   AND NOT EXISTS(SELECT 1 FROM lifecycle_current_execution c WHERE c.task_id=o.task_id AND c.generation=o.generation) LIMIT ?2)",
                params![NORMALIZED_EVIDENCE, limit],
            )?;
            let commands_removed = self.transaction.execute(
                "DELETE FROM provider_commands WHERE intent_id IN(
                   SELECT p.intent_id FROM provider_commands p
                   WHERE NOT EXISTS(SELECT 1 FROM lifecycle_current_execution c WHERE c.task_id=p.task_id AND c.generation=p.generation)
                   AND NOT EXISTS(SELECT 1 FROM lifecycle_dispatches d WHERE d.id=p.intent_id AND d.status='claimed') LIMIT ?1)",
                [limit],
            )?;
            let texts_released = self.transaction.execute(
                "UPDATE lifecycle_inputs SET body=NULL WHERE rowid IN(
                   SELECT i.rowid FROM lifecycle_inputs i JOIN tasks t ON t.id=i.task_id
                   WHERE i.body IS NOT NULL AND t.state IN('done','cancelled') LIMIT ?1)",
                [limit],
            )?;
            Ok(EvidenceRetention { released, removed, commands_removed, texts_released })
        })
    }
}
