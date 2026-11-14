//! Later definitive receipts supplement a dispatch's immutable historical uncertainty.
use super::*;
use alondra_provider_contract::{Handoff, HandoffProof};

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS provider_receipt_proofs(
 intent_id INTEGER NOT NULL,task_id TEXT NOT NULL,generation INTEGER NOT NULL,binding_id TEXT NOT NULL,
 command_digest BLOB NOT NULL CHECK(length(command_digest)=32),
 proof_digest BLOB NOT NULL CHECK(length(proof_digest)=32),
 outcome TEXT NOT NULL CHECK(outcome IN('handed','rejected')),
 evidence BLOB NOT NULL CHECK(length(evidence) BETWEEN 1 AND 1048576),
 owner_generation INTEGER NOT NULL CHECK(owner_generation BETWEEN 1 AND 9007199254740991),
 PRIMARY KEY(intent_id,proof_digest),
 FOREIGN KEY(intent_id,task_id,generation) REFERENCES lifecycle_dispatches(id,task_id,generation),
 FOREIGN KEY(task_id,generation,binding_id) REFERENCES lifecycle_execution_bindings(task_id,generation,binding_id)
);
CREATE TRIGGER IF NOT EXISTS provider_receipt_matches_command BEFORE INSERT ON provider_receipt_proofs
 WHEN NOT EXISTS(SELECT 1 FROM lifecycle_dispatches d JOIN provider_commands c ON c.intent_id=d.id
 WHERE d.id=NEW.intent_id AND d.task_id=NEW.task_id AND d.generation=NEW.generation AND d.status='uncertain'
 AND c.task_id=NEW.task_id AND c.generation=NEW.generation AND c.binding_id=NEW.binding_id AND c.digest=NEW.command_digest)
 BEGIN SELECT RAISE(ABORT,'receipt proof requires its uncertain recorded command'); END;
CREATE TRIGGER IF NOT EXISTS provider_receipt_immutable BEFORE UPDATE ON provider_receipt_proofs
 BEGIN SELECT RAISE(ABORT,'receipt proof is immutable'); END;
CREATE TRIGGER IF NOT EXISTS provider_receipt_retained BEFORE DELETE ON provider_receipt_proofs
 BEGIN SELECT RAISE(ABORT,'receipt proof is retained'); END;
CREATE VIEW IF NOT EXISTS lifecycle_dispatch_certainty AS
 SELECT d.id,d.task_id,d.generation,d.input_id,d.kind,d.placement_kind,d.placement,
 CASE WHEN d.status='uncertain' THEN coalesce(
 (SELECT CASE WHEN min(p.outcome)=max(p.outcome) THEN min(p.outcome) ELSE 'uncertain' END
 FROM provider_receipt_proofs p WHERE p.intent_id=d.id),d.status) ELSE d.status END AS status,d.claimed_revision
 FROM lifecycle_dispatches d;";

/// An uncertain dispatch with retained command bytes that an adapter can examine without sending.
#[derive(Debug)]
pub struct ReceiptCandidate {
    pub intent_id: i64,
    pub input: InputId,
    pub command: Vec<u8>,
}

impl LifecycleTransaction<'_> {
    pub fn receipt_candidates(&self, task: &TaskId) -> Result<Vec<ReceiptCandidate>> {
        staging!(self, {
            let aggregate = self.load(task)?;
            let stamp = aggregate.execution().ok_or(Error::MissingIntent)?;
            let mut statement = self.transaction.prepare(
                "SELECT d.id,d.input_id,c.bytes,c.digest FROM lifecycle_dispatches d
                 JOIN lifecycle_dispatch_certainty effective ON effective.id=d.id
                 JOIN provider_commands c ON c.intent_id=d.id AND c.task_id=d.task_id AND c.generation=d.generation
                 WHERE d.task_id=?1 AND d.generation=?2 AND d.status='uncertain' AND effective.status='uncertain' ORDER BY d.id",
            )?;
            let rows = statement.query_map(params![task.0, sql_number(stamp.generation())?], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, Vec<u8>>(2)?, row.get::<_, Vec<u8>>(3)?))
            })?;
            rows.map(|row| {
                let (intent_id, input, command, digest) = row?;
                if command.is_empty() || command.len() > MAX_PAYLOAD || evidence_digest(&command) != digest {
                    return Err(Error::CorruptJournal);
                }
                Ok(ReceiptCandidate { intent_id, input: InputId::new(input)?, command })
            }).collect()
        })
    }

    /// Appends evidence under the current owner; contradictions remain uncertain in the read view.
    /// `false` means the exact evidence was already retained. Neither case consumes the input.
    pub fn record_handoff_proof(&mut self, owner: &OwnerGrant, intent_id: i64, proof: &HandoffProof) -> Result<bool> {
        staging!(self, {
            self.fence_owner(owner)?;
            let stamp = owner.execution();
            let row: Option<(String, Vec<u8>, Vec<u8>)> = self.transaction.query_row(
                "SELECT d.status,c.bytes,c.digest FROM lifecycle_dispatches d JOIN provider_commands c ON c.intent_id=d.id
                 WHERE d.id=?1 AND d.task_id=?2 AND d.generation=?3 AND c.task_id=d.task_id AND c.generation=d.generation AND c.binding_id=?4",
                params![intent_id, stamp.task().0, sql_number(stamp.generation())?, owner.binding().id()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).optional()?;
            let (status, bytes, command_digest) = row.ok_or(Error::MissingCommand)?;
            if status != "uncertain" { return Err(Error::StalePermit); }
            if evidence_digest(&bytes) != command_digest { return Err(Error::CorruptJournal); }
            if bytes != proof.command() { return Err(Error::CommandConflict); }
            let outcome = match proof.receipt() {
                Handoff::Offered => "handed",
                Handoff::RefusedBeforeOffer => "rejected",
                Handoff::Uncertain => return Err(Error::InvalidInput),
            };
            let proof_digest = evidence_digest(&[outcome.as_bytes(), &[0], proof.evidence()].concat());
            let inserted = self.transaction.execute(
                "INSERT INTO provider_receipt_proofs(intent_id,task_id,generation,binding_id,command_digest,proof_digest,outcome,evidence,owner_generation)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(intent_id,proof_digest) DO NOTHING",
                params![intent_id, stamp.task().0, sql_number(stamp.generation())?, owner.binding().id(), command_digest, proof_digest,
                    outcome, proof.evidence(), sql_number(owner.generation())?],
            )?;
            let (stored, stored_outcome): (Vec<u8>, String) = self.transaction.query_row(
                "SELECT evidence,outcome FROM provider_receipt_proofs WHERE intent_id=?1 AND proof_digest=?2",
                params![intent_id, proof_digest], |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if stored != proof.evidence() || stored_outcome != outcome { return Err(Error::CorruptJournal); }
            if inserted != 0 { self.touched.insert(stamp.task().clone()); }
            Ok(inserted != 0)
        })
    }
}
