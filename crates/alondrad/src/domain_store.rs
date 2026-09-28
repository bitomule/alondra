//! Relational authority for domain v2. The legacy event store remains technical
//! evidence only; these rows are never reconstructed by legacy event replay.
mod bounded;
mod canonical;
mod connection;
mod repository;

use alondra_core::domain_v2::*;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

const SCHEMA_VERSION: i64 = 3;
const APPLICATION_ID: i64 = 0x414c4432;
const SCHEMA: &str = include_str!("domain_store/schema.sql");
pub(crate) use bounded::{ProjectionLimit, ProjectionTooLarge};
pub(crate) type TransportIdentity<'a> = (&'a str, &'a str, &'a [u8; 32]);

pub(crate) struct DomainStore {
    conn: Connection,
}
#[derive(Debug)]
pub(crate) enum StoreError {
    Domain(DomainError),
    Storage(anyhow::Error),
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Domain(e) => e.fmt(f),
            Self::Storage(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for StoreError {}
impl From<anyhow::Error> for StoreError {
    fn from(e: anyhow::Error) -> Self {
        Self::Storage(e)
    }
}
impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Storage(e.into())
    }
}
impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        Self::Storage(e.into())
    }
}
impl From<DomainError> for StoreError {
    fn from(e: DomainError) -> Self {
        Self::Domain(e)
    }
}
#[derive(Debug)]
pub(crate) struct Commit {
    pub state: DomainState,
    pub outcome: CommandOutcome,
    pub replayed: bool,
}

impl DomainStore {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let (mut conn, path) = connection::open(path)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let application: i64 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
        ensure!(
            version <= SCHEMA_VERSION,
            "domain schema is newer than this daemon"
        );
        if version == 0 {
            let existing: i64 = tx.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get(0),
            )?;
            ensure!(
                existing == 0 && application == 0,
                "refusing nonempty unversioned domain database"
            );
            tx.execute_batch(SCHEMA)?;
            tx.execute("INSERT INTO domain_meta(singleton,data_epoch,revision,orchestrator_generation) VALUES(1,?1,0,0)",[crate::auth::random_hex(32)])?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
        } else {
            ensure!(
                application == APPLICATION_ID,
                "not an Alondra domain database"
            );
        }
        if version == 1 || version == 2 {
            connection::backup(&path)?;
        }
        if version < 2 {
            tx.execute_batch("ALTER TABLE domain_meta ADD COLUMN migration_note TEXT;")?;
            // v1 predates attachment rows. Build the relation in the same transaction
            // after a coherent WAL-aware backup, before publishing user_version=2.
            tx.execute_batch(include_str!("domain_store/attachments_v2.sql"))?;
        }
        if version == 1 || version == 2 {
            // Add a logical User target without rebuilding the message table,
            // whose ids are referenced by the outbox and delivery attempts.
            tx.execute_batch(include_str!("domain_store/user_targets_v3.sql"))?;
        }
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        // A process can die after terminal write and before recording its outcome.
        // Never redeliver a claim merely because the daemon restarted.
        tx.execute("UPDATE delivery_attempts SET state='uncertain',version=version+1,reason='daemon_restarted_after_claim' WHERE state='dispatching'",[])?;
        tx.execute(
            "UPDATE domain_outbox SET state='uncertain' WHERE state='dispatching'",
            [],
        )?;
        tx.commit()?;
        let store = Self { conn };
        store.integrity_check()?;
        Ok(store)
    }
    pub(crate) fn load(&mut self) -> Result<DomainState> {
        let tx = self.conn.transaction()?;
        check_schema(&tx)?;
        let state = repository::load(&tx)?;
        tx.commit()?;
        Ok(state)
    }
    /// Offline one-shot cutover. The imported state carries a freshly created
    /// epoch from this empty store; no legacy command or delivery is replayed.
    pub(crate) fn import_initial(&mut self, state: &DomainState,
        links: &[crate::domain_migration::ImportLink], source_seq: u64) -> Result<()> {
        state.validate()?;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_schema(&tx)?;
        let (epoch, revision): (String, i64) = tx.query_row(
            "SELECT data_epoch,revision FROM domain_meta WHERE singleton=1", [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(revision == 0 && epoch == state.data_epoch.0,
            "domain import requires an empty store and its fresh data epoch");
        let existing: i64 = tx.query_row("SELECT count(*) FROM requests", [], |r| r.get(0))?;
        ensure!(existing == 0, "domain import target already has business rows");
        repository::save(&tx, state)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS legacy_import_map (
            source_kind TEXT NOT NULL,source_id TEXT NOT NULL,role TEXT NOT NULL,
            target_kind TEXT NOT NULL,target_id TEXT NOT NULL,
            source_digest TEXT NOT NULL CHECK(length(source_digest)=64),source_seq INTEGER NOT NULL,
            PRIMARY KEY(source_kind,source_id,role),UNIQUE(target_kind,target_id,role)
        );")?;
        for link in links {
            tx.execute("INSERT INTO legacy_import_map(source_kind,source_id,role,target_kind,target_id,source_digest,source_seq)
                VALUES(?1,?2,?3,?4,?5,?6,?7)", params![link.source_kind,link.source_id,link.role,
                link.target_kind,link.target_id,link.source_digest,i64::try_from(source_seq)?])?;
        }
        let mapped: i64 = tx.query_row("SELECT count(*) FROM legacy_import_map", [], |r|r.get(0))?;
        ensure!(mapped as usize == links.len(), "legacy mapping count changed during import");
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn project(&mut self) -> Result<DomainProjection> {
        Ok(self.load()?.projection())
    }
    pub(crate) fn integrity_check(&self) -> Result<()> {
        ensure!(
            self.conn
                .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?
                == "ok",
            "domain integrity check failed"
        );
        let violations: i64 =
            self.conn
                .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                    r.get(0)
                })?;
        ensure!(violations == 0, "domain foreign key check failed");
        Ok(())
    }
    pub(crate) fn execute_authenticated(
        &mut self,
        actor: &Actor,
        envelope: &CommandEnvelope,
        transport: Option<TransportIdentity<'_>>,
    ) -> std::result::Result<Commit, StoreError> {
        let actor_key = String::from_utf8(canonical::bytes(&match actor {
            Actor::Orchestrator { .. } => serde_json::json!({"kind":"orchestrator"}),
            _ => serde_json::to_value(actor)?,
        })?)
        .context("actor encoding")?;
        let digest = canonical::digest(envelope)
            .map_err(|e| DomainError::new("invalid_input", e.to_string()))?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_schema(&tx)?;
        let state = repository::load(&tx)?;
        if envelope.protocol_version != PROTOCOL_VERSION {
            return Err(
                DomainError::new("incompatible_protocol", "domain protocol 2 is required").into(),
            );
        }
        if envelope.data_epoch != state.data_epoch {
            return Err(DomainError::new("stale_epoch", "domain epoch changed").into());
        }
        if let Actor::Orchestrator { generation } = actor {
            if *generation != state.orchestrator_generation {
                return Err(DomainError::new(
                    "stale_generation",
                    "orchestrator generation changed",
                )
                .into());
            }
        }
        if let Some((sender, id, ciphertext_digest)) = transport {
            let stored:Option<(Vec<u8>,String)>=tx.query_row("SELECT digest,result FROM local_command_results WHERE sender=?1 AND request_id=?2",params![sender,id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((previous, result)) = stored {
                if previous != ciphertext_digest {
                    return Err(DomainError::new(
                        "idempotency_conflict",
                        "request ID reused with different encrypted bytes",
                    )
                    .into());
                }
                let outcome = serde_json::from_str(&result)?;
                tx.commit()?;
                return Ok(Commit {
                    state,
                    outcome,
                    replayed: true,
                });
            }
        }
        let stored:Option<(Vec<u8>,String)>=tx.query_row("SELECT digest,result FROM semantic_receipts WHERE actor=?1 AND command_id=?2 AND data_epoch=?3",params![actor_key,envelope.command_id.0,envelope.data_epoch.0],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((previous, result)) = stored {
            if previous != digest {
                return Err(DomainError::new(
                    "idempotency_conflict",
                    "command ID reused with different payload",
                )
                .into());
            }
            let outcome = serde_json::from_str(&result)?;
            if let Some((sender, id, ciphertext_digest)) = transport {
                tx.execute(
                    "INSERT INTO local_command_results VALUES(?1,?2,?3,?4)",
                    params![sender, id, ciphertext_digest.as_slice(), result],
                )?;
            }
            tx.commit()?;
            return Ok(Commit {
                state,
                outcome,
                replayed: true,
            });
        }
        let mut next = state.clone();
        let outcome = next.execute(actor, envelope)?;
        if outcome.changed {
            repository::save(&tx, &next)?;
            tx.execute("INSERT INTO command_audit(revision,actor,command_id,data_epoch,command) VALUES(?1,?2,?3,?4,?5)",params![outcome.revision as i64,actor_key,envelope.command_id.0,envelope.data_epoch.0,String::from_utf8(canonical::bytes(&envelope.command)?).context("command encoding")?])?;
            for (index, effect) in outcome.effects.iter().enumerate() {
                let (kind, agent, task, interaction, message) = match effect {
                    DomainEffect::NotifyUser {
                        task_id,
                        interaction_id,
                    } => (
                        "orchestrator",
                        None,
                        task_id.as_ref().map(|x| x.0.as_str()),
                        Some(interaction_id.0.as_str()),
                        "A domain interaction is ready for your review.",
                    ),
                    DomainEffect::NotifyAgent {
                        agent_id,
                        task_id,
                        interaction_id,
                        message,
                    } => (
                        "agent",
                        Some(agent_id.0.as_str()),
                        task_id.as_ref().map(|x| x.0.as_str()),
                        interaction_id.as_ref().map(|x| x.0.as_str()),
                        message.as_str(),
                    ),
                    DomainEffect::NotifyOrchestrator { interaction_id } => (
                        "orchestrator",
                        None,
                        None,
                        Some(interaction_id.0.as_str()),
                        "A domain interaction requires your response.",
                    ),
                };
                tx.execute("INSERT INTO domain_messages(revision,ordinal,recipient_kind,agent_id,task_id,interaction_id,message) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![outcome.revision as i64,index as i64,kind,agent,task,interaction,message])?;
                if matches!(effect, DomainEffect::NotifyUser { .. }) {
                    tx.execute("INSERT INTO domain_message_user_targets(message_id) VALUES(?1)", [tx.last_insert_rowid()])?;
                }
                tx.execute(
                    "INSERT INTO domain_outbox(message_id,state) VALUES(?1,'pending')",
                    [tx.last_insert_rowid()],
                )?;
            }
        } else {
            ensure_no_effects(&outcome)?;
        }
        let result = serde_json::to_string(&outcome)?;
        tx.execute("INSERT INTO semantic_receipts(actor,command_id,data_epoch,digest,result) VALUES(?1,?2,?3,?4,?5)",params![actor_key,envelope.command_id.0,envelope.data_epoch.0,digest.as_slice(),result])?;
        if let Some((sender, id, ciphertext_digest)) = transport {
            tx.execute(
                "INSERT INTO local_command_results VALUES(?1,?2,?3,?4)",
                params![sender, id, ciphertext_digest.as_slice(), result],
            )?;
        }
        tx.commit()?;
        Ok(Commit {
            state: next,
            outcome,
            replayed: false,
        })
    }
}
fn check_schema(conn: &Connection) -> Result<()> {
    ensure!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))? == SCHEMA_VERSION
            && conn.pragma_query_value(None, "application_id", |r| r.get::<_, i64>(0))?
                == APPLICATION_ID,
        "domain schema changed while daemon was running"
    );
    Ok(())
}
fn ensure_no_effects(outcome: &CommandOutcome) -> Result<()> {
    ensure!(
        outcome.effects.is_empty(),
        "unchanged command produced effects"
    );
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct OutboxMessage {
    pub id: i64,
    pub recipient_kind: String,
    pub agent_id: Option<String>,
    pub task_id: Option<String>,
    pub interaction_id: Option<String>,
    pub message: String,
    pub state: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DeliveryClaim {
    pub attempt_id: i64,
    pub version: u64,
    pub message: OutboxMessage,
}
impl DomainStore {
    pub(crate) fn pending_messages(&self) -> Result<Vec<OutboxMessage>> {
        let mut q=self.conn.prepare("SELECT m.id,CASE WHEN u.message_id IS NOT NULL THEN 'user' ELSE m.recipient_kind END,m.agent_id,m.task_id,m.interaction_id,m.message,o.state FROM domain_messages m JOIN domain_outbox o ON o.message_id=m.id LEFT JOIN domain_message_user_targets u ON u.message_id=m.id WHERE o.state IN('pending','retryable') ORDER BY m.id")?;
        Ok(q.query_map([], |r| {
            Ok(OutboxMessage {
                id: r.get(0)?,
                recipient_kind: r.get(1)?,
                agent_id: r.get(2)?,
                task_id: r.get(3)?,
                interaction_id: r.get(4)?,
                message: r.get(5)?,
                state: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
    }
    pub(crate) fn claim_message(&mut self, id: i64) -> Result<Option<DeliveryClaim>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_schema(&tx)?;
        let message=tx.query_row("SELECT m.id,CASE WHEN u.message_id IS NOT NULL THEN 'user' ELSE m.recipient_kind END,m.agent_id,m.task_id,m.interaction_id,m.message,o.state FROM domain_messages m JOIN domain_outbox o ON o.message_id=m.id LEFT JOIN domain_message_user_targets u ON u.message_id=m.id WHERE m.id=?1 AND o.state IN('pending','retryable')",[id],|r|Ok(OutboxMessage{id:r.get(0)?,recipient_kind:r.get(1)?,agent_id:r.get(2)?,task_id:r.get(3)?,interaction_id:r.get(4)?,message:r.get(5)?,state:r.get(6)?})).optional()?;
        let Some(message) = message else {
            return Ok(None);
        };
        tx.execute("INSERT INTO delivery_attempts(message_id,state,version,reason) VALUES(?1,'dispatching',1,NULL)",[id])?;
        let attempt_id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE domain_outbox SET state='dispatching' WHERE message_id=?1",
            [id],
        )?;
        tx.commit()?;
        Ok(Some(DeliveryClaim {
            attempt_id,
            version: 1,
            message,
        }))
    }
    pub(crate) fn finish_message(
        &mut self,
        attempt_id: i64,
        version: u64,
        state: alondra_core::delivery::State,
        reason: Option<&str>,
    ) -> Result<bool> {
        use alondra_core::delivery::State;
        let status = match state {
            State::Uncertain => "uncertain",
            State::Handed => "handed",
            State::Retryable => "retryable",
            State::Discarded => "discarded",
            State::Dispatching => anyhow::bail!("finish cannot dispatch"),
        };
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_schema(&tx)?;
        let message:Option<i64>=tx.query_row("SELECT message_id FROM delivery_attempts WHERE id=?1 AND version=?2 AND state='dispatching'",params![attempt_id,i64::try_from(version).context("delivery version overflow")?],|r|r.get(0)).optional()?;
        let Some(id) = message else { return Ok(false) };
        tx.execute("UPDATE delivery_attempts SET state=?3,version=version+1,reason=?4 WHERE id=?1 AND version=?2",params![attempt_id,i64::try_from(version).context("delivery version overflow")?,status,reason])?;
        tx.execute(
            "UPDATE domain_outbox SET state=?2 WHERE message_id=?1",
            params![id, status],
        )?;
        tx.commit()?;
        Ok(true)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DeliveryEvidence {
    pub attempt_id: i64,
    pub message_id: i64,
    pub state: String,
    pub version: u64,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DeliveryEvidenceItem {
    pub attempt_id: i64,
    pub message_id: i64,
    pub state: String,
    pub current_state: String,
    pub version: u64,
    pub reason: Option<String>,
    pub recipient_kind: String,
    pub task_id: Option<String>,
    pub interaction_id: Option<String>,
}
impl DomainStore {
    /// One bounded page of technical evidence. The message body remains in the
    /// domain projection; this API carries only IDs and delivery state.
    pub(crate) fn delivery_evidence_page(&self, after: i64) -> Result<Vec<DeliveryEvidenceItem>> {
        let mut q = self.conn.prepare(r#"SELECT a.id,a.message_id,a.state,o.state,a.version,a.reason,
            CASE WHEN u.message_id IS NOT NULL THEN 'user' ELSE m.recipient_kind END,
            m.task_id,m.interaction_id FROM delivery_attempts a
            JOIN domain_messages m ON m.id=a.message_id
            JOIN domain_outbox o ON o.message_id=a.message_id
            LEFT JOIN domain_message_user_targets u ON u.message_id=a.message_id
            WHERE a.id>?1 ORDER BY a.id LIMIT 101"#)?;
        Ok(q.query_map([after], |r| Ok(DeliveryEvidenceItem {
            attempt_id: r.get(0)?, message_id: r.get(1)?, state: r.get(2)?,
            current_state: r.get(3)?, version: r.get::<_, i64>(4)? as u64,
            reason: r.get(5)?, recipient_kind: r.get(6)?, task_id: r.get(7)?,
            interaction_id: r.get(8)?,
        }))?.collect::<rusqlite::Result<_>>()?)
    }

    /// Resolve exactly one uncertain attempt with the same durable transport
    /// receipt used by domain commands. No ambiguous batch is retried.
    pub(crate) fn resolve_uncertain_authenticated(
        &mut self, attempt_id: i64, expected_version: u64, epoch: &str,
        retry: bool, transport: TransportIdentity<'_>,
    ) -> std::result::Result<Value, StoreError> {
        let (sender, request_id, ciphertext_digest) = transport;
        let version = i64::try_from(expected_version)
            .map_err(|_| DomainError::new("invalid_input", "delivery version overflow"))?;
        if version < 1 || expected_version >= MAX_REVISION {
            return Err(DomainError::new("invalid_input", "invalid delivery version").into());
        }
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_schema(&tx)?;
        let current_epoch: String = tx.query_row(
            "SELECT data_epoch FROM domain_meta WHERE singleton=1", [], |r| r.get(0))?;
        if current_epoch != epoch {
            return Err(DomainError::new("stale_epoch", "domain epoch changed").into());
        }
        let previous: Option<(Vec<u8>, String)> = tx.query_row(
            "SELECT digest,result FROM local_command_results WHERE sender=?1 AND request_id=?2",
            params![sender, request_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        if let Some((digest, result)) = previous {
            if digest != ciphertext_digest {
                return Err(DomainError::new("idempotency_conflict", "request ID reused with different encrypted bytes").into());
            }
            return Ok(serde_json::from_str(&result)?);
        }
        let message_id: Option<i64> = tx.query_row(
            "SELECT a.message_id FROM delivery_attempts a JOIN domain_outbox o ON o.message_id=a.message_id \
             WHERE a.id=?1 AND a.version=?2 AND a.state='uncertain' AND o.state='uncertain'",
            params![attempt_id, version], |r| r.get(0)).optional()?;
        let Some(message_id) = message_id else {
            return Err(DomainError::new("delivery_uncertain", "uncertain delivery changed; refresh evidence").into());
        };
        let status = if retry { "retryable" } else { "discarded" };
        tx.execute("UPDATE delivery_attempts SET state=?3,version=version+1,reason='explicit_user_reconciliation' \
            WHERE id=?1 AND version=?2 AND state='uncertain'", params![attempt_id,version,status])?;
        tx.execute("UPDATE domain_outbox SET state=?2 WHERE message_id=?1 AND state='uncertain'",
            params![message_id,status])?;
        let result = json!({"attempt_id":attempt_id,"message_id":message_id,
            "version":expected_version+1,"state":status});
        tx.execute("INSERT INTO local_command_results(sender,request_id,digest,result) VALUES(?1,?2,?3,?4)",
            params![sender,request_id,ciphertext_digest.as_slice(),serde_json::to_string(&result)?])?;
        tx.commit()?;
        Ok(result)
    }

    pub(crate) fn delivery_evidence(&self) -> Result<Vec<DeliveryEvidence>> {
        let mut q = self.conn.prepare(
            "SELECT id,message_id,state,version,reason FROM delivery_attempts ORDER BY id",
        )?;
        Ok(q.query_map([], |r| {
            Ok(DeliveryEvidence {
                attempt_id: r.get(0)?,
                message_id: r.get(1)?,
                state: r.get(2)?,
                version: r.get::<_, i64>(3)? as u64,
                reason: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
    }
    /// Legacy store-level fixture. Production resolution uses the authenticated
    /// transport receipt above so a retry cannot repeat the decision.
    #[cfg(test)]
    pub(crate) fn resolve_uncertain(
        &mut self,
        attempt_id: i64,
        expected_version: u64,
        retry: bool,
    ) -> Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_schema(&tx)?;
        let message:Option<i64>=tx.query_row("SELECT message_id FROM delivery_attempts WHERE id=?1 AND version=?2 AND state='uncertain'",params![attempt_id,i64::try_from(expected_version).context("delivery version overflow")?],|r|r.get(0)).optional()?;
        let Some(id) = message else { return Ok(false) };
        let status = if retry { "retryable" } else { "discarded" };
        tx.execute("UPDATE delivery_attempts SET state=?3,version=version+1,reason='explicit_user_reconciliation' WHERE id=?1 AND version=?2",params![attempt_id,i64::try_from(expected_version).context("delivery version overflow")?,status])?;
        tx.execute(
            "UPDATE domain_outbox SET state=?2 WHERE message_id=?1",
            params![id, status],
        )?;
        tx.commit()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
