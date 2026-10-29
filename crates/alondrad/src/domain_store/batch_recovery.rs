use super::*;

pub(crate) struct UnfinishedBatch {
    pub(crate) run: String,
    pub(crate) ended: bool,
    pub(crate) decision: Option<String>,
    pub(crate) attempts: Vec<DeliveryEvidence>,
}

impl DomainStore {
    pub(crate) fn unfinished_orchestrator_batches(
        &self,
        epoch: &DataEpoch,
        active: Option<&str>,
        retained: Option<&str>,
    ) -> Result<Vec<UnfinishedBatch>> {
        let mut query = self.conn.prepare("SELECT r.run,r.session,r.ended,d.target FROM orchestrator_batch_runs r LEFT JOIN orchestrator_batch_decisions d ON d.run=r.run WHERE r.data_epoch=?1 AND (r.ended=0 OR r.run=?2 OR r.run=?3) ORDER BY r.run")?;
        let rows = query.query_map(params![epoch.0, active, retained], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut batches = Vec::new();
        for row in rows {
            let (run, session, ended, decision) = row?;
            let Some(session) = session else { continue };
            let mut claims = self.conn.prepare("SELECT a.id,a.message_id,a.state,a.version,a.reason FROM orchestrator_deliveries d JOIN delivery_attempts a ON a.id=d.attempt_id WHERE d.session_id=?1 ORDER BY a.id")?;
            let attempts = claims
                .query_map([session], |row| {
                    Ok(DeliveryEvidence {
                        attempt_id: row.get(0)?,
                        message_id: row.get(1)?,
                        state: row.get(2)?,
                        version: row.get::<_, i64>(3)? as u64,
                        reason: row.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            batches.push(UnfinishedBatch {
                run,
                ended,
                decision,
                attempts,
            });
        }
        Ok(batches)
    }

    /// Settles every outstanding claim together. An old attempt cannot overwrite a newer
    /// delivery of the same message, even if a previous reconciliation only partly completed.
    pub(crate) fn reconcile_orchestrator_batch(
        &mut self,
        run: &str,
        continuation: bool,
    ) -> Result<()> {
        #[cfg(test)]
        self.orchestrator_write_fault("reconcile")?;
        let target = if continuation {
            alondra_core::delivery::State::Discarded
        } else {
            alondra_core::delivery::State::Retryable
        };
        self.settle_orchestrator_batch(run, target, Some("orchestrator_storage_recovery"))?;
        Ok(())
    }

    pub(crate) fn settle_orchestrator_batch(
        &mut self,
        run: &str,
        target: alondra_core::delivery::State,
        reason: Option<&str>,
    ) -> Result<bool> {
        use alondra_core::delivery::State;
        let target = match target {
            State::Handed => "handed",
            State::Discarded => "discarded",
            State::Retryable => "retryable",
            _ => anyhow::bail!("batch settlement requires a terminal delivery state"),
        };
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_schema(&tx)?;
        let session: Option<String> = tx
            .query_row(
                "SELECT session FROM orchestrator_batch_runs WHERE run=?1",
                [run],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        if session.is_none() {
            return Ok(false);
        }
        let explicit = reason == Some("explicit_user_reconciliation");
        if explicit {
            tx.execute("INSERT INTO orchestrator_batch_decisions(run,target) VALUES(?1,?2) ON CONFLICT(run) DO UPDATE SET target=excluded.target", params![run, target])?;
        }
        #[cfg(test)]
        crate::domain_orchestrator::faults::database_write(&tx, "delivery_finish")?;
        tx.execute("UPDATE domain_outbox SET state=?2 WHERE message_id IN(SELECT a.message_id FROM delivery_attempts a JOIN orchestrator_deliveries d ON d.attempt_id=a.id JOIN orchestrator_batch_runs r ON r.session=d.session_id WHERE r.run=?1 AND (a.state IN('dispatching','uncertain') OR (?3 AND a.reason='explicit_user_reconciliation' AND a.state IN('discarded','retryable') AND a.state<>?2)) AND NOT EXISTS(SELECT 1 FROM delivery_attempts newer WHERE newer.message_id=a.message_id AND newer.id>a.id))", params![run, target, explicit])?;
        #[cfg(test)]
        crate::domain_orchestrator::faults::database_write(&tx, "delivery_finish")?;
        tx.execute("UPDATE delivery_attempts SET state=?2,version=version+1,reason=?3 WHERE (state IN('dispatching','uncertain') OR (?4 AND reason='explicit_user_reconciliation' AND state IN('discarded','retryable') AND state<>?2)) AND id IN(SELECT d.attempt_id FROM orchestrator_deliveries d JOIN orchestrator_batch_runs r ON r.session=d.session_id WHERE r.run=?1)", params![run, target, reason, explicit])?;
        #[cfg(test)]
        crate::domain_orchestrator::faults::database_write(&tx, "delivery_commit")?;
        tx.commit()?;
        self.note_side_write();
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) fn orchestrator_write_fault(&self, point: &'static str) -> Result<()> {
        if let Some(home) = self.conn.path().map(Path::new).and_then(Path::parent) {
            crate::domain_orchestrator::faults::write(home, point)?;
        }
        Ok(())
    }
}
