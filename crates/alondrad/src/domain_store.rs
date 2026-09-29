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

pub(crate) const SCHEMA_VERSION: i64 = 6;
const APPLICATION_ID: i64 = 0x414c4432;
const SCHEMA: &str = include_str!("domain_store/schema.sql");
pub(crate) use bounded::{ProjectionLimit, ProjectionTooLarge};
#[cfg(test)]
pub(crate) use repository::statement_counts;
pub(crate) type TransportIdentity<'a> = (&'a str, &'a str, &'a [u8; 32]);

/// The last state this connection loaded or committed, valid while no other
/// connection committed (`data_version`) and this one wrote nothing outside
/// the paths that refresh it (`total_changes`). Any miss falls back to a full
/// load, so a stale copy is never served.
struct Cached {
    state: DomainState,
    data_version: i64,
    total_changes: u64,
}
pub(crate) struct DomainStore {
    conn: Connection,
    cached: Option<Cached>,
    projection: Option<(DataEpoch, Revision, usize, Vec<u8>)>,
    #[cfg(test)]
    after_commit: Option<Box<dyn FnMut() + Send>>,
}
fn data_version(c: &Connection) -> Result<i64> {
    Ok(c.pragma_query_value(None, "data_version", |r| r.get(0))?)
}
/// The state as of this transaction: the cached copy when it is still current,
/// otherwise a full load. Also returns whether the cache served it and the
/// `data_version` that state corresponds to, read inside the transaction so a
/// commit by another connection after ours can never be absorbed into it.
fn state_in(cached: &Option<Cached>, tx: &Connection) -> Result<(DomainState, bool, i64)> {
    let version = data_version(tx)?;
    let changes = tx.total_changes();
    match cached {
        Some(c) if c.data_version == version && c.total_changes == changes => {
            Ok((c.state.clone(), true, version))
        }
        _ => Ok((repository::load(tx)?, false, version)),
    }
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
    /// Schedule route adapters must check the transport receipt before parsing a
    /// draft or reading current options. A retry returns the original outcome.
    pub(crate) fn transport_receipt(
        &mut self,
        (sender, request_id, digest): TransportIdentity<'_>,
    ) -> std::result::Result<Option<CommandOutcome>, StoreError> {
        let stored: Option<(Vec<u8>, String)> = self.conn.query_row(
            "SELECT digest,result FROM local_command_results WHERE sender=?1 AND request_id=?2",
            params![sender, request_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        match stored {
            Some((previous, _)) if previous != digest => Err(DomainError::new(
                "idempotency_conflict", "request ID reused with different encrypted bytes",
            ).into()),
            Some((_, result)) => Ok(Some(serde_json::from_str(&result)?)),
            None => Ok(None),
        }
    }

    pub(crate) fn open(path: &Path) -> Result<Self> {
        let (mut conn, _) = connection::open(path)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let application: i64 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
        ensure!(version == 0 || version == SCHEMA_VERSION,
            "domain database has an incompatible schema; prepare a fresh cutover store");
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
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        // A process can die after terminal write and before recording its outcome.
        // Never redeliver a claim merely because the daemon restarted.
        tx.execute("UPDATE delivery_attempts SET state='uncertain',version=version+1,reason='daemon_restarted_after_claim' WHERE state='dispatching'",[])?;
        tx.execute(
            "UPDATE domain_outbox SET state='uncertain' WHERE state='dispatching'",
            [],
        )?;
        // A claimed schedule slot is consumed. Restart records uncertainty and
        // never dispatches the same run again solely because the daemon reopened.
        tx.execute("UPDATE schedule_runs SET state='interrupted',error='Alondra stopped while sending this run' WHERE state='launching'", [])?;
        tx.execute("UPDATE schedule_runs SET launch=json_set(launch,'$.state','uncertain','$.error','Alondra stopped after claiming the Claude launch') WHERE json_extract(launch,'$.state')='dispatching'", [])?;
        tx.commit()?;
        let store = Self::from_connection(conn);
        store.integrity_check()?;
        Ok(store)
    }
    fn from_connection(conn: Connection) -> Self {
        Self {
            conn,
            cached: None,
            projection: None,
            #[cfg(test)]
            after_commit: None,
        }
    }
    /// For verifying a prepared stage: no schema creation, no restart recovery
    /// and no write of any kind, so the bytes hashed afterwards are the importer's.
    pub(crate) fn open_readonly(path: &Path) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let parent = std::fs::canonicalize(path.parent().context("database parent required")?)?;
        let path = parent.join(path.file_name().context("database filename required")?);
        let meta = std::fs::symlink_metadata(&path)?;
        ensure!(
            meta.is_file() && meta.nlink() == 1 && meta.uid() == unsafe { libc::geteuid() },
            "unsafe domain database path"
        );
        let conn = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "query_only", "ON")?;
        check_schema(&conn)?;
        Ok(Self::from_connection(conn))
    }
    #[cfg(test)]
    pub(crate) fn on_commit(&mut self, hook: impl FnMut() + Send + 'static) {
        self.after_commit = Some(Box::new(hook));
    }
    #[cfg(test)]
    /// Rows this connection has written since it was opened.
    pub(crate) fn rows_written(&self) -> u64 {
        self.conn.total_changes()
    }
    fn remember(&mut self, state: DomainState, data_version: i64) {
        self.cached = Some(Cached {
            state,
            data_version,
            total_changes: self.conn.total_changes(),
        });
    }
    /// Delivery and receipt rows are not domain state; writing them keeps the cache.
    fn note_side_write(&mut self) {
        if let Some(cached) = &mut self.cached {
            cached.total_changes = self.conn.total_changes();
        }
    }
    pub(crate) fn load(&mut self) -> Result<DomainState> {
        let tx = self.conn.transaction()?;
        check_schema(&tx)?;
        let (state, hit, version) = state_in(&self.cached, &tx)?;
        tx.commit()?;
        if !hit {
            self.remember(state.clone(), version);
        }
        Ok(state)
    }
    /// Offline one-shot cutover. The imported state carries a freshly created
    /// epoch from this empty store; no legacy command or delivery is replayed.
    pub(crate) fn import_initial(&mut self, state: &DomainState,
        links: &[crate::domain_migration::ImportLink], source_seq: u64,
        schedules: &crate::domain_migration::LegacyScheduleInventory) -> Result<()> {
        state.validate()?;
        ensure!(state.schedule == schedules.state,
            "imported Schedule state differs from legacy inventory");
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_schema(&tx)?;
        let (epoch, revision): (String, i64) = tx.query_row(
            "SELECT data_epoch,revision FROM domain_meta WHERE singleton=1", [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(revision == 0 && epoch == state.data_epoch.0,
            "domain import requires an empty store and its fresh data epoch");
        let existing: i64 = tx.query_row(
            "SELECT (SELECT count(*) FROM requests) + (SELECT count(*) FROM schedules)
                + (SELECT count(*) FROM schedule_runs) + (SELECT count(*) FROM schedule_request_tools)
                + (SELECT count(*) FROM legacy_schedule_receipts)
                + (SELECT count(*) FROM schedule_failed_launches)", [], |r| r.get(0))?;
        ensure!(existing == 0, "domain import target already has business rows");
        repository::save(&tx, state)?;
        for (request_id, patterns) in &schedules.request_tools {
            repository::remember_schedule_request_tools(&tx, request_id, patterns)?;
        }
        for receipt in &schedules.legacy_schedule_receipts {
            repository::save_legacy_schedule_receipt(&tx, &repository::LegacyScheduleReceipt {
                sender: receipt.sender.clone(), request_id: receipt.request_id.clone(),
                digest: receipt.digest.clone(), result: receipt.result.clone(),
            })?;
        }
        for request_id in &schedules.failed_launches {
            repository::remember_schedule_failed_launch(&tx, request_id)?;
        }
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
        let version = data_version(&tx)?;
        tx.commit()?;
        self.remember(state.clone(), version);
        self.projection = None;
        Ok(())
    }
    pub(crate) fn legacy_schedule_inventory(
        &mut self,
    ) -> Result<crate::domain_migration::LegacyScheduleInventory> {
        let tx = self.conn.transaction()?;
        check_schema(&tx)?;
        let state = repository::load(&tx)?.schedule;
        let mut request_tools = std::collections::BTreeMap::new();
        for row in tx.prepare("SELECT request_id,patterns FROM schedule_request_tools ORDER BY request_id")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        {
            let (request_id, patterns) = row?;
            request_tools.insert(RequestId(request_id), serde_json::from_str(&patterns)?);
        }
        let mut legacy_schedule_receipts = Vec::new();
        for row in tx.prepare("SELECT sender,request_id,digest,result FROM legacy_schedule_receipts ORDER BY sender,request_id")?
            .query_map([], |r| Ok(crate::domain_migration::LegacyScheduleReceiptRow {
                sender: r.get(0)?, request_id: r.get(1)?, digest: r.get(2)?, result: r.get(3)?,
            }))?
        {
            legacy_schedule_receipts.push(row?);
        }
        let mut failed_launches = std::collections::BTreeSet::new();
        for row in tx.prepare("SELECT request_id FROM schedule_failed_launches ORDER BY request_id")?
            .query_map([], |r| r.get::<_, String>(0))?
        {
            failed_launches.insert(RequestId(row?));
        }
        tx.commit()?;
        Ok(crate::domain_migration::LegacyScheduleInventory {
            state, request_tools, legacy_schedule_receipts, failed_launches,
        })
    }
    pub(crate) fn project(&mut self) -> Result<DomainProjection> {
        Ok(self.load()?.projection())
    }
    pub(crate) fn schedule_request_tools(&mut self, request_id: &RequestId) -> Result<Option<Vec<String>>> {
        let tx = self.conn.transaction()?;
        check_schema(&tx)?;
        let patterns = repository::load_schedule_request_tools(&tx, request_id)?;
        tx.commit()?;
        Ok(patterns)
    }
    pub(crate) fn was_schedule_launch_failed(&self, request_id: &RequestId) -> Result<bool> {
        check_schema(&self.conn)?;
        repository::was_schedule_launch_failed(&self.conn, request_id)
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
        self.execute_envelope(actor, envelope, transport, false)
    }
    /// Producers have no protocol read for the cursor: an expected_revision of
    /// zero is filled with the current revision under the writer lock, while
    /// the semantic receipt still keys on the bytes the producer sent.
    pub(crate) fn execute_filling_revision(
        &mut self,
        actor: &Actor,
        envelope: &CommandEnvelope,
    ) -> std::result::Result<Commit, StoreError> {
        self.execute_envelope(actor, envelope, None, true)
    }
    fn execute_envelope(
        &mut self,
        actor: &Actor,
        envelope: &CommandEnvelope,
        transport: Option<TransportIdentity<'_>>,
        fill_revision: bool,
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
        let (state, hit, version) = state_in(&self.cached, &tx)?;
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
                if !hit {
                    self.remember(state.clone(), version);
                }
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
                repository::exec(
                    &tx,
                    "INSERT INTO local_command_results VALUES(?1,?2,?3,?4)",
                    params![sender, id, ciphertext_digest.as_slice(), result],
                )?;
            }
            tx.commit()?;
            self.remember(state.clone(), version);
            return Ok(Commit {
                state,
                outcome,
                replayed: true,
            });
        }
        let filled;
        let envelope = if fill_revision && envelope.expected_revision == 0 {
            filled = CommandEnvelope {
                expected_revision: state.revision,
                ..envelope.clone()
            };
            &filled
        } else {
            envelope
        };
        let mut next = state.clone();
        let outcome = next.execute(actor, envelope)?;
        if outcome.changed {
            repository::save_changes(&tx, &state, &next)?;
            if let Command::RecordScheduleRunFailed { run_id, .. } = &envelope.command
                && let Some(request_id) = &next.schedule.runs[run_id].request_id
            {
                repository::remember_schedule_failed_launch(&tx, request_id)?;
            }
            let at = alondra_core::now_millis();
            repository::exec(&tx, "INSERT INTO command_audit(revision,actor,command_id,data_epoch,command,at) VALUES(?1,?2,?3,?4,?5,?6)",params![outcome.revision as i64,actor_key,envelope.command_id.0,envelope.data_epoch.0,String::from_utf8(canonical::bytes(&envelope.command)?).context("command encoding")?,at])?;
            for request in next.requests.keys().filter(|id| !state.requests.contains_key(*id)) {
                repository::exec(&tx, "UPDATE requests SET created_at=?1 WHERE id=?2", params![at, request.0])?;
            }
            for (id, task) in next.tasks.iter().filter(|(id, task)| state.tasks.get(*id).map(|before| before.state) != Some(task.state)) {
                repository::exec(&tx, "INSERT INTO task_history(revision,task_id,state,summary) VALUES(?1,?2,?3,?4)",
                    params![outcome.revision as i64, id.0, serde_json::to_value(task.state)?.as_str().unwrap_or_default(), history_summary(&envelope.command)])?;
            }
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
                repository::exec(&tx, "INSERT INTO domain_messages(revision,ordinal,recipient_kind,agent_id,task_id,interaction_id,message) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![outcome.revision as i64,index as i64,kind,agent,task,interaction,message])?;
                if matches!(effect, DomainEffect::NotifyUser { .. }) {
                    repository::exec(&tx, "INSERT INTO domain_message_user_targets(message_id) VALUES(?1)", [tx.last_insert_rowid()])?;
                }
                repository::exec(
                    &tx,
                    "INSERT INTO domain_outbox(message_id,state) VALUES(?1,'pending')",
                    [tx.last_insert_rowid()],
                )?;
            }
        } else {
            ensure_no_effects(&outcome)?;
        }
        let result = serde_json::to_string(&outcome)?;
        repository::exec(&tx, "INSERT INTO semantic_receipts(actor,command_id,data_epoch,digest,result) VALUES(?1,?2,?3,?4,?5)",params![actor_key,envelope.command_id.0,envelope.data_epoch.0,digest.as_slice(),result])?;
        if let Some((sender, id, ciphertext_digest)) = transport {
            repository::exec(
                &tx,
                "INSERT INTO local_command_results VALUES(?1,?2,?3,?4)",
                params![sender, id, ciphertext_digest.as_slice(), result],
            )?;
        }
        tx.commit()?;
        #[cfg(test)]
        if let Some(hook) = &mut self.after_commit {
            hook();
        }
        self.remember(next.clone(), version);
        Ok(Commit {
            state: next,
            outcome,
            replayed: false,
        })
    }
}
/// What a Task's history says about the step, when the command that moved it carried words.
fn history_summary(command: &Command) -> String {
    let words = match command {
        Command::PresentResult { body, .. } | Command::AskQuestion { body, .. } | Command::RequestAction { body, .. } => body,
        _ => return String::new(),
    };
    words.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default().chars().take(200).collect()
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
/// One step of a Task's history.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct HistoryStep {
    pub at: i64,
    pub state: String,
    pub summary: String,
}

impl DomainStore {
    /// When each Task last entered its state.
    pub(crate) fn task_state_times(&self) -> Result<std::collections::BTreeMap<TaskId, i64>> {
        let mut query = self.conn.prepare("SELECT h.task_id,a.at FROM task_history h JOIN command_audit a ON a.revision=h.revision
            WHERE h.revision=(SELECT max(revision) FROM task_history WHERE task_id=h.task_id)")?;
        let rows = query.query_map([], |row| Ok((TaskId(row.get(0)?), row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub(crate) fn request_times(&self) -> Result<std::collections::BTreeMap<RequestId, i64>> {
        let mut query = self.conn.prepare("SELECT id,created_at FROM requests")?;
        let rows = query.query_map([], |row| Ok((RequestId(row.get(0)?), row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    /// When the commits of these revisions were made.
    pub(crate) fn revision_times(&self, revisions: &[Revision]) -> Result<std::collections::BTreeMap<Revision, i64>> {
        let mut query = self.conn.prepare("SELECT at FROM command_audit WHERE revision=?1")?;
        let mut times = std::collections::BTreeMap::new();
        for revision in revisions {
            if let Some(at) = query.query_row([*revision as i64], |row| row.get(0)).optional()? {
                times.insert(*revision, at);
            }
        }
        Ok(times)
    }
    pub(crate) fn task_history(&self, task_id: &TaskId) -> Result<Vec<HistoryStep>> {
        let mut query = self.conn.prepare("SELECT a.at,h.state,h.summary FROM task_history h JOIN command_audit a ON a.revision=h.revision
            WHERE h.task_id=?1 ORDER BY h.revision")?;
        let rows = query.query_map([&task_id.0], |row| Ok(HistoryStep { at: row.get(0)?, state: row.get(1)?, summary: row.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
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
        self.note_side_write();
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
        self.note_side_write();
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
        self.note_side_write();
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
        self.note_side_write();
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
#[path = "domain_store/acceptance_tests.rs"]
mod acceptance_tests;
#[cfg(test)]
#[path = "domain_store/schedule_acceptance_tests.rs"]
mod schedule_acceptance_tests;
