//! Relational authority for domain v2. The legacy event store remains technical
//! evidence only; these rows are never reconstructed by legacy event replay.
mod canonical;
mod connection;
#[cfg(test)]
mod live_read_trace;
pub(crate) mod model_metrics;
mod repository;

use alondra_core::domain::*;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

pub(crate) const SCHEMA_VERSION: i64 = 4;
pub(crate) const ORCHESTRATOR_MESSAGE_TTL_MS: i64 = 24 * 60 * 60 * 1_000;
const APPLICATION_ID: i64 = 0x414c4432;
const SCHEDULE_LAUNCH_TRANSITION: &str = include_str!("domain_store/schedule_launch_transition.sql");
const SCHEMA: &str = concat!(
    include_str!("domain_store/schema.sql"),
    include_str!("domain_store/schedule_launch_transition.sql")
);
/// Created on every open instead of with the schema, so a store made before models were recorded
/// gains it without being moved aside.
const TASK_MODELS: &str = "CREATE TABLE IF NOT EXISTS task_models(task_id TEXT PRIMARY KEY REFERENCES tasks(id), model TEXT NOT NULL CHECK(length(model)>0), chosen_by TEXT NOT NULL CHECK(chosen_by IN('orchestrator','user')));";
const MODEL_METRICS: &str = include_str!("domain_store/model_metrics.sql");
/// How many turn ends in a row Alondra held back for a Task, since its node last delivered or
/// asked (`marker`, the order of that card). Kept outside the domain state, like task_models.
/// Domains written before the node machine kept a `connected` flag per agent. A connected agent
/// was at work; one with an open task is being brought back; the rest are closed. The first
/// listing of real sessions after the daemon starts corrects what the flag got wrong.
const AGENT_PHASES: &str = "ALTER TABLE agents ADD COLUMN phase TEXT NOT NULL DEFAULT '{\"kind\":\"closed\"}' CHECK(json_valid(phase) AND json_type(phase,'$.kind')='text');\
ALTER TABLE agents ADD COLUMN since INTEGER NOT NULL DEFAULT 0;\
UPDATE agents SET phase=CASE WHEN connected=1 THEN '{\"kind\":\"working\"}' \
 WHEN EXISTS(SELECT 1 FROM tasks t WHERE t.agent_id=agents.id AND t.archived=0 AND t.state NOT IN('done','cancelled')) THEN '{\"kind\":\"starting\",\"reviving\":true}' \
 ELSE '{\"kind\":\"closed\"}' END;\
ALTER TABLE agents DROP COLUMN connected;";
/// The turns of a node a Claude API error ended in a row, the first and the last, and the card that
/// asks the user once Alondra stops sending it back by itself. Kept outside the domain state,
/// like stop_blocks.
const API_FAILURES: &str = "CREATE TABLE IF NOT EXISTS api_failures(agent_id TEXT PRIMARY KEY, failures INTEGER NOT NULL CHECK(failures BETWEEN 0 AND 255), first_at INTEGER NOT NULL, last_at INTEGER NOT NULL, card TEXT);";
const STOP_BLOCKS: &str = "CREATE TABLE IF NOT EXISTS stop_blocks(task_id TEXT PRIMARY KEY, blocks INTEGER NOT NULL CHECK(blocks>=0), marker INTEGER NOT NULL);";
/// What became of a task's node, kept beside the task; created on every open like task_models.
const TASK_WRAP_UPS: &str = "CREATE TABLE IF NOT EXISTS task_wrap_ups(\
 task_id TEXT PRIMARY KEY REFERENCES tasks(id),\
 asked_at INTEGER CHECK(asked_at IS NULL OR asked_at BETWEEN 0 AND 9007199254740991),\
 cleaned_at INTEGER CHECK(cleaned_at IS NULL OR cleaned_at BETWEEN 0 AND 9007199254740991),\
 close_cancelled INTEGER NOT NULL CHECK(close_cancelled IN(0,1)),\
 node_end TEXT CHECK(node_end IS NULL OR node_end IN('closed','gone','kept_by_user','users_own')));";
/// What a task relates to beyond its dependencies, and its verdict on the tasks it blocks, kept
/// beside the task; created on every open like task_wrap_ups.
const TASK_RELATIONS: &str = "CREATE TABLE IF NOT EXISTS task_relations(\
 task_id TEXT PRIMARY KEY REFERENCES tasks(id),\
 delivered_dependencies TEXT NOT NULL CHECK(json_valid(delivered_dependencies)),\
 blocks TEXT NOT NULL CHECK(json_valid(blocks)),\
 verdict TEXT CHECK(verdict IS NULL OR json_valid(verdict)),\
 review_rounds INTEGER NOT NULL CHECK(review_rounds BETWEEN 0 AND 255));";
/// The Requests sent as maintenance messages, kept beside them; created on every open like
/// task_relations.
const ORCHESTRATOR_SESSION: &str = "CREATE TABLE IF NOT EXISTS orchestrator_session(\
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),\
 session_id TEXT NOT NULL CHECK(length(session_id)>0));";
const ORCHESTRATOR_DELIVERIES: &str = "CREATE TABLE IF NOT EXISTS orchestrator_deliveries(\
 attempt_id INTEGER PRIMARY KEY REFERENCES delivery_attempts(id),\
 session_id TEXT NOT NULL CHECK(length(session_id)>0));";
const MAINTENANCE_REQUESTS: &str = "CREATE TABLE IF NOT EXISTS maintenance_requests(request_id TEXT PRIMARY KEY REFERENCES requests(id));";
/// The Requests sent as messages to the orchestrator, kept beside them; created on every open like
/// task_relations. When it is first created, the messages from before it are the maintenance ones and
/// those with the `request-` + 32 hex id that only the orchestrator message route makes.
const MESSAGE_REQUESTS: &str = "CREATE TABLE IF NOT EXISTS message_requests(request_id TEXT PRIMARY KEY REFERENCES requests(id));";
const MESSAGE_REQUESTS_BEFORE: &str = "INSERT OR IGNORE INTO message_requests(request_id) \
 SELECT request_id FROM maintenance_requests UNION \
 SELECT id FROM requests WHERE length(id)=40 AND substr(id,1,8)='request-' AND substr(id,9) NOT GLOB '*[^0-9a-f]*';";
/// What a node says it still does after its PR merged, kept beside the task's wrap-up; created on
/// every open like task_relations.
const TASK_POST_MERGE_WORK: &str = "CREATE TABLE IF NOT EXISTS task_post_merge_work(\
 task_id TEXT PRIMARY KEY REFERENCES tasks(id),\
 reason TEXT NOT NULL,\
 declared_at INTEGER NOT NULL CHECK(declared_at BETWEEN 0 AND 9007199254740991));";
/// A task's priority other than Normal, kept beside the task; created on every open like task_relations.
const TASK_PRIORITIES: &str = "CREATE TABLE IF NOT EXISTS task_priorities(\
 task_id TEXT PRIMARY KEY REFERENCES tasks(id),\
 priority TEXT NOT NULL CHECK(priority IN('high','normal','low')));";
/// Each task's scope, the points of the last plan the user approved, and the points each final
/// delivery marked done; created on every open like task_relations.
const TASK_CHECKLISTS: &str = "CREATE TABLE IF NOT EXISTS task_checklists(\
 task_id TEXT PRIMARY KEY REFERENCES tasks(id),\
 plan_interaction_id TEXT NOT NULL REFERENCES interactions(id),\
 items TEXT NOT NULL CHECK(json_valid(items)));\
 CREATE TABLE IF NOT EXISTS delivery_checklists(\
 interaction_id TEXT PRIMARY KEY REFERENCES interactions(id),\
 marks TEXT NOT NULL CHECK(json_valid(marks)));";
/// Launches waiting at the gate for a free slot (not domain state): what to launch, who asked and
/// since when. See docs/fleet-pacing.md.
const LAUNCH_QUEUE: &str = "CREATE TABLE IF NOT EXISTS launch_queue(\
 task_id TEXT PRIMARY KEY,\
 body TEXT NOT NULL CHECK(json_valid(body)),\
 origin TEXT NOT NULL,\
 enqueued_at INTEGER NOT NULL);";
/// The closer's own bookkeeping (not domain state): when it first saw a task due for closing, its
/// failures and the card it raised.
const TASK_CLOSINGS: &str = "CREATE TABLE IF NOT EXISTS task_closings(\
 task_id TEXT PRIMARY KEY REFERENCES tasks(id),\
 since INTEGER NOT NULL,\
 failures INTEGER NOT NULL DEFAULT 0 CHECK(failures>=0),\
 last_error TEXT,\
 next_attempt INTEGER NOT NULL DEFAULT 0,\
 attempted_cleaned_at INTEGER,\
 rm_started_at INTEGER,\
 ready_since INTEGER,\
 problem_id TEXT);";
/// The closer's watch over work a node declared after the merge (not domain state): which
/// declaration it watches, since when, when it asked the node whether it still runs, and the card
/// it raised when the node did not answer.
const POST_MERGE_CHECKS: &str = "CREATE TABLE IF NOT EXISTS post_merge_checks(\
 task_id TEXT PRIMARY KEY REFERENCES tasks(id),\
 declared_at INTEGER NOT NULL,\
 since INTEGER NOT NULL,\
 asked_at INTEGER,\
 problem_id TEXT,\
 close_now INTEGER NOT NULL DEFAULT 0 CHECK(close_now IN(0,1)));";
const TASK_CLOSING_SELECT_ONE: &str = "SELECT task_id,since,failures,last_error,next_attempt,attempted_cleaned_at,rm_started_at,problem_id,ready_since FROM task_closings WHERE task_id=?1";
const TASK_CLOSING_SELECT_ALL: &str = "SELECT task_id,since,failures,last_error,next_attempt,attempted_cleaned_at,rm_started_at,problem_id,ready_since FROM task_closings ORDER BY task_id";
/// What the closer remembers about work a node declared after the merge, across restarts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PostMergeCheck {
    pub task_id: TaskId,
    /// The revision of the declaration this watch is about.
    pub declared_at: i64,
    /// Milliseconds: when the closer first saw that declaration.
    pub since: i64,
    /// Milliseconds: when Alondra asked the node whether it still runs.
    pub asked_at: Option<i64>,
    /// The card asking the user whether to close the node.
    pub problem_id: Option<String>,
    /// The user chose to close the node anyway.
    pub close_now: bool,
}
/// What the closer remembers about closing one task's node, across restarts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskClosing {
    pub task_id: TaskId,
    /// Milliseconds: when the closer first saw the task due for closing.
    pub since: i64,
    pub failures: u32,
    pub last_error: Option<String>,
    /// Milliseconds: no attempt before this.
    pub next_attempt: i64,
    /// The task's `wrap_up.cleaned_at` seen at the last failed attempt.
    pub attempted_cleaned_at: Option<i64>,
    /// Milliseconds: `claude rm` was started and its result not recorded.
    pub rm_started_at: Option<i64>,
    /// Milliseconds: when the closer first found the node ready to close but still working.
    pub ready_since: Option<i64>,
    /// The Problem card raised after three failures.
    pub problem_id: Option<String>,
}
impl TaskClosing {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            task_id: TaskId(row.get(0)?),
            since: row.get(1)?,
            failures: row.get(2)?,
            last_error: row.get(3)?,
            next_attempt: row.get(4)?,
            attempted_cleaned_at: row.get(5)?,
            rm_started_at: row.get(6)?,
            ready_since: row.get(8)?,
            problem_id: row.get(7)?,
        })
    }
}
/// How Alondra starts a task by itself, given with the task and stored with it.
const TASK_LAUNCH_PLANS: &str = "CREATE TABLE IF NOT EXISTS task_launch_plans(\
 task_id TEXT PRIMARY KEY REFERENCES tasks(id),\
 plan TEXT NOT NULL CHECK(json_valid(plan)),\
 role TEXT CHECK(role IS NULL OR role IN('design','implementation','review')),\
 at INTEGER NOT NULL);";
/// One automatic launch per task ever: the row is the claim, written before `claude` runs.
const AUTO_LAUNCHES: &str = "CREATE TABLE IF NOT EXISTS auto_launches(\
 task_id TEXT PRIMARY KEY REFERENCES tasks(id),\
 state TEXT NOT NULL CHECK(state IN('launching','launched','blocked')),\
 reason TEXT CHECK(reason IS NULL OR reason IN('usage_exhausted','workspace_untrusted','no_plan','launch_failed')),\
 detail TEXT,\
 at INTEGER NOT NULL);";
/// A start that failed and that Alondra tries again by itself: how many times so far and when next.
const START_RETRIES: &str = "CREATE TABLE IF NOT EXISTS start_retries(\
 task_id TEXT PRIMARY KEY REFERENCES tasks(id),\
 attempts INTEGER NOT NULL CHECK(attempts > 0),\
 retry_at INTEGER NOT NULL);";
/// Why the last launch of a task that has not started was refused, whoever asked for it, so its
/// state says it. `detail` is the folder Claude Code does not trust, or the error.
const LAUNCH_REFUSALS: &str = "CREATE TABLE IF NOT EXISTS launch_refusals(\
 task_id TEXT PRIMARY KEY REFERENCES tasks(id),\
 reason TEXT NOT NULL CHECK(reason IN('usage_exhausted','disk_low','workspace_untrusted','launch_failed')),\
 detail TEXT,\
 at INTEGER NOT NULL);";
/// The worktree a node took over from the node it replaced: Alondra may delete it with the node,
/// as it may the worktree Claude Code made for a node, never a folder it was only launched in.
const INHERITED_WORKTREES: &str = "CREATE TABLE IF NOT EXISTS inherited_worktrees(\
 agent_id TEXT PRIMARY KEY CHECK(length(agent_id)>0),\
 dir TEXT NOT NULL CHECK(length(dir)>0),\
 from_agent TEXT NOT NULL,\
 at INTEGER NOT NULL);";
/// The worktree a task's nodes work in, kept after they end so the next node of the task starts
/// there instead of the repository's main checkout.
const TASK_WORKTREES: &str = "CREATE TABLE IF NOT EXISTS task_worktrees(\
 task_id TEXT PRIMARY KEY CHECK(length(task_id)>0),\
 dir TEXT NOT NULL CHECK(length(dir)>0),\
 at INTEGER NOT NULL);";

const EVENTS_VIEW: &str = "DROP VIEW IF EXISTS events;\
 CREATE VIEW events AS SELECT revision AS seq,0 AS at,'domain' AS kind,NULL AS session,command AS body FROM command_audit;";

/// What a command carries into the transaction that commits it.
pub(crate) enum Attached<'a> {
    None,
    /// What the user chose for the launch of the Request a `CreateRequest` makes.
    Request(&'a RequestLaunch),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutoLaunchState {
    Launching,
    Launched,
    Blocked,
}
impl AutoLaunchState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Launching => "launching",
            Self::Launched => "launched",
            Self::Blocked => "blocked",
        }
    }
    fn parse(text: &str) -> Self {
        match text {
            "launched" => Self::Launched,
            "blocked" => Self::Blocked,
            _ => Self::Launching,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutoLaunchReason {
    UsageExhausted,
    WorkspaceUntrusted,
    LaunchFailed,
}
impl AutoLaunchReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::UsageExhausted => "usage_exhausted",
            Self::WorkspaceUntrusted => "workspace_untrusted",
            Self::LaunchFailed => "launch_failed",
        }
    }
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "usage_exhausted" => Self::UsageExhausted,
            "workspace_untrusted" => Self::WorkspaceUntrusted,
            "launch_failed" => Self::LaunchFailed,
            _ => return None,
        })
    }
}

/// A launch waiting at the gate: the launch as asked, who asked and since when.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct QueuedLaunch {
    pub task_id: TaskId,
    pub body: serde_json::Value,
    pub origin: String,
    pub enqueued_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AutoLaunch {
    pub task_id: TaskId,
    pub state: AutoLaunchState,
    pub reason: Option<AutoLaunchReason>,
    pub detail: Option<String>,
    pub at: i64,
}
impl AutoLaunch {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let state: String = row.get(1)?;
        let reason: Option<String> = row.get(2)?;
        Ok(Self {
            task_id: TaskId(row.get(0)?),
            state: AutoLaunchState::parse(&state),
            reason: reason.as_deref().and_then(AutoLaunchReason::parse),
            detail: row.get(3)?,
            at: row.get(4)?,
        })
    }
}
#[cfg(test)]
pub(crate) use repository::statement_counts;
pub(crate) type TransportIdentity<'a> = (&'a str, &'a str, &'a [u8; 32]);

/// What the user chose when asking for new work, kept with the Request it created.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RequestLaunch {
    pub project: Option<String>,
    pub options: alondra_core::options::Resolved,
    pub issue: Option<String>,
    pub issue_text: Option<String>,
    pub attachments: Vec<AttachmentRef>,
}

/// The last state this connection loaded or committed, valid while no other
/// connection committed (`data_version`) and this one wrote nothing outside
/// the paths that refresh it (`total_changes`). Any miss falls back to a full
/// load, so a stale copy is never served.
struct Cached {
    state: Arc<DomainState>,
    data_version: i64,
    total_changes: u64,
}
pub(crate) struct DomainStore {
    conn: Connection,
    #[cfg(test)]
    live_read_trace: Option<live_read_trace::Sink>,
    cached: Option<Cached>,
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
fn state_in(cached: &Option<Cached>, tx: &Connection) -> Result<(Arc<DomainState>, bool, i64)> {
    let version = data_version(tx)?;
    let changes = tx.total_changes();
    match cached {
        Some(c) if c.data_version == version && c.total_changes == changes => {
            Ok((Arc::clone(&c.state), true, version))
        }
        _ => Ok((Arc::new(repository::load(tx)?), false, version)),
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
    pub state: Arc<DomainState>,
    pub outcome: CommandOutcome,
    pub replayed: bool,
}

impl DomainStore {
    #[cfg(test)]
    pub(crate) fn begin_live_read_trace(&mut self) {
        assert!(self.live_read_trace.is_none(), "live read trace already active");
        self.live_read_trace = Some(Default::default());
    }
    #[cfg(test)]
    pub(crate) fn end_live_read_trace(&mut self) -> Vec<live_read_trace::LiveStoreRead> {
        let sink = self.live_read_trace.take().expect("live read trace not active");
        let events = sink.lock().expect("live read trace poisoned").clone();
        events
    }
    #[cfg(test)]
    fn trace_live_read(&self, getter: &'static str) -> Option<live_read_trace::Read> {
        self.live_read_trace.as_ref().map(|sink| live_read_trace::Read::new(sink.clone(), getter))
    }
    /// Return only a command that already has a durable semantic receipt for this exact actor,
    /// command id and epoch. Route adapters use this to recover opaque fields for an idempotent
    /// retry without reopening a completed authorization to a new command.
    pub(crate) fn receipted_command(
        &self,
        actor: &Actor,
        command_id: &CommandId,
        data_epoch: &DataEpoch,
    ) -> std::result::Result<Option<Command>, StoreError> {
        let actor_key = String::from_utf8(canonical::bytes(&match actor {
            Actor::Orchestrator { .. } => serde_json::json!({"kind":"orchestrator"}),
            _ => serde_json::to_value(actor)?,
        })?)
        .context("actor encoding")?;
        let command: Option<String> = self.conn.query_row(
            "SELECT a.command FROM command_audit a JOIN semantic_receipts r USING(actor,command_id,data_epoch) WHERE a.actor=?1 AND a.command_id=?2 AND a.data_epoch=?3",
            params![actor_key, command_id.0, data_epoch.0],
            |row| row.get(0),
        ).optional()?;
        command.map(|command| serde_json::from_str(&command).map_err(StoreError::from)).transpose()
    }

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
        ensure!(version == 0 || version == SCHEMA_VERSION,
            "unsupported domain database schema {version}; this build requires {SCHEMA_VERSION}; database preserved at {}",
            path.display());
        let application: i64 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
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
        let flagged: bool = tx.query_row("SELECT count(*) FROM pragma_table_info('agents') WHERE name='connected'", [], |r| r.get::<_, i64>(0))? == 1;
        if flagged {
            tx.execute_batch(AGENT_PHASES)?;
        }
        tx.execute_batch(TASK_MODELS)?;
        tx.execute_batch(MODEL_METRICS)?;
        tx.execute_batch(STOP_BLOCKS)?;
        tx.execute_batch(API_FAILURES)?;
        tx.execute_batch(TASK_WRAP_UPS)?;
        tx.execute_batch(TASK_RELATIONS)?;
        tx.execute_batch(MAINTENANCE_REQUESTS)?;
        let messages_known: bool = tx.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='message_requests'", [], |r| r.get::<_, i64>(0),
        )? == 1;
        tx.execute_batch(MESSAGE_REQUESTS)?;
        if !messages_known {
            tx.execute_batch(MESSAGE_REQUESTS_BEFORE)?;
        }
        tx.execute_batch(ORCHESTRATOR_SESSION)?;
        tx.execute_batch(ORCHESTRATOR_DELIVERIES)?;
        tx.execute_batch(TASK_POST_MERGE_WORK)?;
        tx.execute_batch(TASK_PRIORITIES)?;
        tx.execute_batch(TASK_CHECKLISTS)?;
        tx.execute_batch(LAUNCH_QUEUE)?;
        tx.execute_batch(TASK_CLOSINGS)?;
        tx.execute_batch(POST_MERGE_CHECKS)?;
        tx.execute_batch(TASK_LAUNCH_PLANS)?;
        tx.execute_batch(AUTO_LAUNCHES)?;
        tx.execute_batch(START_RETRIES)?;
        tx.execute_batch(LAUNCH_REFUSALS)?;
        tx.execute_batch(INHERITED_WORKTREES)?;
        tx.execute_batch(TASK_WORKTREES)?;
        tx.execute_batch(EVENTS_VIEW)?;
        if version != 0 {
            tx.execute_batch("DROP TRIGGER IF EXISTS schedule_launch_transition;")?;
            tx.execute_batch(SCHEDULE_LAUNCH_TRANSITION)?;
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
        store.conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, false)?;
        Ok(store)
    }
    fn from_connection(conn: Connection) -> Self {
        Self {
            conn,
            cached: None,
            #[cfg(test)]
            live_read_trace: None,
            #[cfg(test)]
            after_commit: None,
        }
    }
    /// No schema creation, no restart recovery and no write of any kind.
    #[cfg(test)]
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
    fn remember(&mut self, state: Arc<DomainState>, data_version: i64) {
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
    pub(crate) fn load(&mut self) -> Result<Arc<DomainState>> {
        let tx = self.conn.transaction()?;
        check_schema(&tx)?;
        let (state, hit, version) = state_in(&self.cached, &tx)?;
        tx.commit()?;
        if !hit {
            self.remember(Arc::clone(&state), version);
        }
        Ok(state)
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
        self.execute_envelope(actor, envelope, transport, false, Attached::None)
    }
    /// A `CreateRequest` that also records what the user chose for its launch, in the same
    /// transaction.
    pub(crate) fn execute_launching(
        &mut self,
        actor: &Actor,
        envelope: &CommandEnvelope,
        transport: Option<TransportIdentity<'_>>,
        launch: &RequestLaunch,
    ) -> std::result::Result<Commit, StoreError> {
        if !matches!(envelope.command, Command::CreateRequest { .. }) {
            return Err(DomainError::new("invalid_input", "only a new Request has a launch").into());
        }
        self.execute_envelope(actor, envelope, transport, true, Attached::Request(launch))
    }
    /// The project and effort each Request was asked for with, by Request.
    pub(crate) fn request_launch_summaries(&self) -> Result<std::collections::BTreeMap<RequestId, (Option<String>, Option<String>)>> {
        #[cfg(test)]
        let mut trace = self.trace_live_read("request_launch_summaries");
        let mut query = self.conn.prepare("SELECT request_id,project,json_extract(options,'$.effort') FROM request_launch")?;
        #[cfg(test)]
        if let Some(trace) = &mut trace { trace.statement(); }
        let rows = query.query_map([], |row| { #[cfg(test)] if let Some(trace) = &mut trace { trace.row(); } Ok((RequestId(row.get(0)?), (row.get(1)?, row.get(2)?))) })?;
        let values = rows.collect::<rusqlite::Result<_>>()?;
        #[cfg(test)]
        if let Some(trace) = &mut trace { trace.complete(); }
        Ok(values)
    }
    pub(crate) fn request_launch(&mut self, request_id: &RequestId) -> Result<Option<RequestLaunch>> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let launch = repository::load_request_launch(&tx, request_id)?;
        tx.commit()?;
        Ok(launch)
    }
    /// Producers have no protocol read for the cursor: an expected_revision of
    /// zero is filled with the current revision under the writer lock, while
    /// the semantic receipt still keys on the bytes the producer sent.
    pub(crate) fn execute_filling_revision(
        &mut self,
        actor: &Actor,
        envelope: &CommandEnvelope,
    ) -> std::result::Result<Commit, StoreError> {
        self.execute_envelope(actor, envelope, None, true, Attached::None)
    }
    /// A `CreateTask` that also stores how Alondra starts the Task, in the same transaction.
    fn execute_envelope(
        &mut self,
        actor: &Actor,
        envelope: &CommandEnvelope,
        transport: Option<TransportIdentity<'_>>,
        fill_revision: bool,
        attached: Attached<'_>,
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
                    self.remember(Arc::clone(&state), version);
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
            self.remember(Arc::clone(&state), version);
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
        let mut next = DomainState::clone(&state);
        let outcome = next.execute(actor, envelope)?;
        if outcome.changed {
            repository::save_changes(&tx, &state, &next)?;
            if let Command::RecordScheduleRunFailed { run_id, .. } = &envelope.command
                && let Some(request_id) = &next.schedule.runs[run_id].request_id
            {
                repository::remember_schedule_failed_launch(&tx, request_id)?;
            }
            match (&attached, &envelope.command) {
                (Attached::Request(launch), Command::CreateRequest { request_id, .. }) => {
                    repository::remember_request_launch(&tx, request_id, launch)?;
                }
                (_, Command::RecordScheduleRunSent { run_id, request, launch, .. }) => {
                    if let Ok(options) = serde_json::from_str::<alondra_core::options::Resolved>(&launch.options_json) {
                        let project = next.schedule.runs.get(run_id).and_then(|run| run.definition_snapshot.as_ref()).and_then(|definition| definition.project.clone());
                        repository::remember_request_launch(&tx, &request.id, &RequestLaunch {
                            project, options, issue: None, issue_text: None, attachments: vec![],
                        })?;
                    }
                }
                (_, Command::CreateTask { task_id, launch: Some(plan), .. }) => {
                    repository::exec(
                        &tx,
                        "INSERT INTO task_launch_plans(task_id,plan,role,at) VALUES(?1,?2,?3,?4)",
                        params![task_id.0, serde_json::to_string(plan)?, plan.role.map(|role| role.as_str()), alondra_core::now_millis()],
                    )?;
                }
                _ => {}
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
                        "A domain interaction is ready for your review.".to_owned(),
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
                        message.clone(),
                    ),
                    DomainEffect::NotifyOrchestrator { interaction_id: Some(interaction_id), .. } => (
                        "orchestrator",
                        None,
                        None,
                        Some(interaction_id.0.as_str()),
                        "A domain interaction requires your response.".to_owned(),
                    ),
                    DomainEffect::NotifyOrchestrator { interaction_id: None, request_id } => (
                        "orchestrator",
                        None,
                        None,
                        None,
                        match request_id {
                            Some(request) if next.requests.get(request).is_some_and(|r| r.maintenance) => format!(
                                "Maintenance message {request} (sent with alondra send): act on it, then close it with close_request; it gets no card for the user."),
                            Some(request) => format!("The user created Request {request}: turn it into Tasks, or answer it and close it."),
                            None => "The domain has something for you.".to_owned(),
                        },
                    ),
                    DomainEffect::ScheduleLaunchFailed { task_id, title, reason } => (
                        "orchestrator",
                        None,
                        Some(task_id.0.as_str()),
                        None,
                        format!("The scheduled Task “{title}” ({task_id}) could not start: {reason} It stays not started with no node and nothing retries it: tell the user why and ask before starting it again."),
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
        let next = Arc::new(next);
        self.remember(Arc::clone(&next), version);
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
        #[cfg(test)]
        let mut trace = self.trace_live_read("task_state_times");
        let mut query = self.conn.prepare("SELECT h.task_id,a.at FROM task_history h JOIN command_audit a ON a.revision=h.revision
            WHERE h.revision=(SELECT max(revision) FROM task_history WHERE task_id=h.task_id)")?;
        #[cfg(test)]
        if let Some(trace) = &mut trace { trace.statement(); }
        let rows = query.query_map([], |row| { #[cfg(test)] if let Some(trace) = &mut trace { trace.row(); } Ok((TaskId(row.get(0)?), row.get(1)?)) })?;
        let values = rows.collect::<rusqlite::Result<_>>()?;
        #[cfg(test)]
        if let Some(trace) = &mut trace { trace.complete(); }
        Ok(values)
    }
    pub(crate) fn request_times(&self) -> Result<std::collections::BTreeMap<RequestId, i64>> {
        #[cfg(test)]
        let mut trace = self.trace_live_read("request_times");
        let mut query = self.conn.prepare("SELECT id,created_at FROM requests")?;
        #[cfg(test)]
        if let Some(trace) = &mut trace { trace.statement(); }
        let rows = query.query_map([], |row| { #[cfg(test)] if let Some(trace) = &mut trace { trace.row(); } Ok((RequestId(row.get(0)?), row.get(1)?)) })?;
        let values = rows.collect::<rusqlite::Result<_>>()?;
        #[cfg(test)]
        if let Some(trace) = &mut trace { trace.complete(); }
        Ok(values)
    }
    /// When the commits of these revisions were made.
    pub(crate) fn revision_times(&self, revisions: &[Revision]) -> Result<std::collections::BTreeMap<Revision, i64>> {
        #[cfg(test)]
        let mut trace = self.trace_live_read("revision_times");
        let mut times = std::collections::BTreeMap::new();
        if revisions.is_empty() {
            self.conn.prepare("SELECT at FROM command_audit WHERE revision=?1")?;
        }
        for revisions in revisions.chunks(512) {
            let marks = vec!["?"; revisions.len()].join(",");
            let mut query = self.conn.prepare(&format!(
                "SELECT revision,at FROM command_audit WHERE revision IN ({marks})"
            ))?;
            #[cfg(test)]
            if let Some(trace) = &mut trace { trace.statement(); }
            let rows = query.query_map(
                rusqlite::params_from_iter(revisions.iter().map(|revision| *revision as i64)),
                |row| {
                    #[cfg(test)]
                    if let Some(trace) = &mut trace { trace.row(); }
                    Ok((row.get::<_, i64>(0)? as Revision, row.get::<_, i64>(1)?))
                },
            )?;
            for row in rows {
                let (revision, at) = row?;
                times.insert(revision, at);
            }
        }
        #[cfg(test)]
        if let Some(trace) = &mut trace { trace.complete(); }
        Ok(times)
    }
    pub(crate) fn api_failures(&self) -> Result<std::collections::BTreeMap<AgentId, crate::api_failures::Streak>> {
        let mut statement = self.conn.prepare("SELECT agent_id,failures,first_at,last_at,card FROM api_failures")?;
        let rows = statement.query_map([], |row| Ok((AgentId(row.get(0)?), crate::api_failures::Streak {
            failures: row.get(1)?, first_at: row.get(2)?, last_at: row.get(3)?, card: row.get::<_, Option<String>>(4)?.map(InteractionId),
        })))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub(crate) fn set_api_failures(&self, agent: &AgentId, streak: Option<&crate::api_failures::Streak>) -> Result<()> {
        match streak {
            None => self.conn.execute("DELETE FROM api_failures WHERE agent_id=?1", [&agent.0])?,
            Some(streak) => self.conn.execute(
                "INSERT INTO api_failures(agent_id,failures,first_at,last_at,card) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(agent_id) DO UPDATE SET failures=excluded.failures,first_at=excluded.first_at,last_at=excluded.last_at,card=excluded.card",
                params![agent.0, streak.failures, streak.first_at, streak.last_at, streak.card.as_ref().map(|card| card.0.as_str())])?,
        };
        Ok(())
    }
    pub(crate) fn stop_blocks(&self, task_id: &TaskId) -> Result<Option<(u32, i64)>> {
        Ok(self.conn.query_row("SELECT blocks,marker FROM stop_blocks WHERE task_id=?1", [&task_id.0],
            |row| Ok((row.get(0)?, row.get(1)?))).optional()?)
    }
    pub(crate) fn set_stop_blocks(&self, task_id: &TaskId, blocks: u32, marker: i64) -> Result<()> {
        self.conn.execute("INSERT INTO stop_blocks(task_id,blocks,marker) VALUES(?1,?2,?3) ON CONFLICT(task_id) DO UPDATE SET blocks=excluded.blocks,marker=excluded.marker",
            params![task_id.0, blocks, marker])?;
        Ok(())
    }
    pub(crate) fn launch_plan(&self, task_id: &TaskId) -> Result<Option<crate::launch_plans::LaunchPlan>> {
        let plan: Option<String> = self.conn.query_row("SELECT plan FROM task_launch_plans WHERE task_id=?1", [&task_id.0], |row| row.get(0)).optional()?;
        plan.map(|plan| serde_json::from_str(&plan).context("stored launch plan")).transpose()
    }
    pub(crate) fn set_launch_plan(&mut self, task_id: &TaskId, plan: &crate::launch_plans::LaunchPlan, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO task_launch_plans(task_id,plan,role,at) VALUES(?1,?2,?3,?4) ON CONFLICT(task_id) DO UPDATE SET plan=excluded.plan,role=excluded.role,at=excluded.at",
            params![task_id.0, serde_json::to_string(plan)?, plan.role.map(|role| role.as_str()), now])?;
        self.note_side_write();
        Ok(())
    }
    pub(crate) fn inherited_worktree(&self, agent_id: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT dir FROM inherited_worktrees WHERE agent_id=?1", [agent_id], |row| row.get(0)).optional()?)
    }
    pub(crate) fn set_inherited_worktree(&mut self, agent_id: &str, dir: &str, from_agent: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO inherited_worktrees(agent_id,dir,from_agent,at) VALUES(?1,?2,?3,?4) ON CONFLICT(agent_id) DO UPDATE SET dir=excluded.dir,from_agent=excluded.from_agent,at=excluded.at",
            params![agent_id, dir, from_agent, now])?;
        self.note_side_write();
        Ok(())
    }
    pub(crate) fn task_worktree(&self, task_id: &TaskId) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT dir FROM task_worktrees WHERE task_id=?1", [&task_id.0], |row| row.get(0)).optional()?)
    }
    pub(crate) fn task_worktrees(&self) -> Result<Vec<(TaskId, String)>> {
        let mut query = self.conn.prepare("SELECT task_id,dir FROM task_worktrees ORDER BY task_id")?;
        let rows = query.query_map([], |row| Ok((TaskId(row.get(0)?), row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub(crate) fn set_task_worktree(&mut self, task_id: &TaskId, dir: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO task_worktrees(task_id,dir,at) VALUES(?1,?2,?3) ON CONFLICT(task_id) DO UPDATE SET dir=excluded.dir,at=excluded.at",
            params![task_id.0, dir, now])?;
        self.note_side_write();
        Ok(())
    }
    pub(crate) fn forget_inherited_worktree(&mut self, agent_id: &str) -> Result<()> {
        if self.conn.execute("DELETE FROM inherited_worktrees WHERE agent_id=?1", [agent_id])? > 0 {
            self.note_side_write();
        }
        Ok(())
    }
    /// When each task with a launch plan got it.
    pub(crate) fn planned_tasks(&self) -> Result<std::collections::BTreeMap<TaskId, i64>> {
        let mut query = self.conn.prepare("SELECT task_id,at FROM task_launch_plans")?;
        let rows = query.query_map([], |row| Ok((TaskId(row.get(0)?), row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    /// Every task a launch plan names a role for.
    pub(crate) fn task_roles(&self) -> Result<std::collections::BTreeMap<TaskId, String>> {
        let mut query = self.conn.prepare("SELECT task_id,role FROM task_launch_plans WHERE role IS NOT NULL")?;
        let rows = query.query_map([], |row| Ok((TaskId(row.get(0)?), row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub(crate) fn auto_launch(&self, task_id: &TaskId) -> Result<Option<AutoLaunch>> {
        Ok(self.conn.query_row("SELECT task_id,state,reason,detail,at FROM auto_launches WHERE task_id=?1", [&task_id.0], AutoLaunch::from_row).optional()?)
    }
    pub(crate) fn auto_launches(&self) -> Result<Vec<AutoLaunch>> {
        let mut query = self.conn.prepare("SELECT task_id,state,reason,detail,at FROM auto_launches ORDER BY task_id")?;
        Ok(query.query_map([], AutoLaunch::from_row)?.collect::<rusqlite::Result<_>>()?)
    }
    /// Takes the one automatic launch of a task: false when it was taken before. A blocked row is
    /// taken again only with `retry`.
    pub(crate) fn claim_auto_launch(&mut self, task_id: &TaskId, now: i64, retry: bool) -> Result<bool> {
        let inserted = self.conn.execute(
            "INSERT INTO auto_launches(task_id,state,at) VALUES(?1,'launching',?2) ON CONFLICT(task_id) DO NOTHING",
            params![task_id.0, now])?;
        let claimed = inserted == 1 || (retry && self.conn.execute(
            "UPDATE auto_launches SET state='launching',reason=NULL,detail=NULL,at=?2 WHERE task_id=?1 AND state='blocked'",
            params![task_id.0, now])? == 1);
        self.note_side_write();
        Ok(claimed)
    }
    pub(crate) fn set_auto_launch(&mut self, task_id: &TaskId, state: AutoLaunchState, reason: Option<AutoLaunchReason>, detail: Option<&str>, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO auto_launches(task_id,state,reason,detail,at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(task_id) DO UPDATE SET state=excluded.state,reason=excluded.reason,detail=excluded.detail,at=excluded.at",
            params![task_id.0, state.as_str(), reason.map(AutoLaunchReason::as_str), detail, now])?;
        self.note_side_write();
        Ok(())
    }
    pub(crate) fn launch_refusals(&self) -> Result<Vec<(TaskId, String, Option<String>)>> {
        #[cfg(test)]
        let mut trace = self.trace_live_read("launch_refusals");
        let mut query = self.conn.prepare("SELECT task_id,reason,detail FROM launch_refusals ORDER BY task_id")?;
        #[cfg(test)]
        if let Some(trace) = &mut trace { trace.statement(); }
        let rows = query.query_map([], |row| { #[cfg(test)] if let Some(trace) = &mut trace { trace.row(); } Ok((TaskId(row.get(0)?), row.get(1)?, row.get(2)?)) })?;
        let values = rows.collect::<rusqlite::Result<_>>()?;
        #[cfg(test)]
        if let Some(trace) = &mut trace { trace.complete(); }
        Ok(values)
    }
    pub(crate) fn set_launch_refusal(&mut self, task_id: &TaskId, refusal: Option<(&str, Option<&str>)>, now: i64) -> Result<()> {
        match refusal {
            Some((reason, detail)) => self.conn.execute(
                "INSERT INTO launch_refusals(task_id,reason,detail,at) VALUES(?1,?2,?3,?4) ON CONFLICT(task_id) DO UPDATE SET reason=excluded.reason,detail=excluded.detail,at=excluded.at",
                params![task_id.0, reason, detail, now])?,
            None => self.conn.execute("DELETE FROM launch_refusals WHERE task_id=?1", [&task_id.0])?,
        };
        self.note_side_write();
        Ok(())
    }

    pub(crate) fn launch_queue(&self) -> Result<Vec<QueuedLaunch>> {
        let mut query = self.conn.prepare("SELECT task_id,body,origin,enqueued_at FROM launch_queue ORDER BY enqueued_at,task_id")?;
        let rows = query.query_map([], |row| {
            let body: String = row.get(1)?;
            Ok((TaskId(row.get(0)?), body, row.get(2)?, row.get(3)?))
        })?;
        rows.map(|row| {
            let (task_id, body, origin, enqueued_at) = row?;
            Ok(QueuedLaunch { task_id, body: serde_json::from_str(&body)?, origin, enqueued_at })
        }).collect()
    }

    /// Queues a launch once: asking again keeps its place.
    pub(crate) fn enqueue_launch(&mut self, task_id: &TaskId, body: &serde_json::Value, origin: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO launch_queue(task_id,body,origin,enqueued_at) VALUES(?1,?2,?3,?4) ON CONFLICT(task_id) DO UPDATE SET body=excluded.body",
            rusqlite::params![task_id.0, serde_json::to_string(body)?, origin, now],
        )?;
        Ok(())
    }

    pub(crate) fn dequeue_launch(&mut self, task_id: &TaskId) -> Result<()> {
        self.conn.execute("DELETE FROM launch_queue WHERE task_id=?1", [&task_id.0])?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn forget_launch_plan(&mut self, task_id: &TaskId) -> Result<()> {
        self.conn.execute("DELETE FROM task_launch_plans WHERE task_id=?1", [&task_id.0])?;
        self.note_side_write();
        Ok(())
    }
    /// Every start Alondra tries again by itself: attempts so far and when next, by task.
    pub(crate) fn start_retries(&self) -> Result<std::collections::BTreeMap<TaskId, (u8, i64)>> {
        #[cfg(test)]
        let mut trace = self.trace_live_read("start_retries");
        let mut query = self.conn.prepare("SELECT task_id,attempts,retry_at FROM start_retries")?;
        #[cfg(test)]
        if let Some(trace) = &mut trace { trace.statement(); }
        let rows = query.query_map([], |row| { #[cfg(test)] if let Some(trace) = &mut trace { trace.row(); } Ok((TaskId(row.get(0)?), (row.get::<_, i64>(1)?.clamp(1, 255) as u8, row.get(2)?))) })?;
        let values = rows.collect::<rusqlite::Result<_>>()?;
        #[cfg(test)]
        if let Some(trace) = &mut trace { trace.complete(); }
        Ok(values)
    }
    pub(crate) fn set_start_retry(&mut self, task_id: &TaskId, retry: Option<(u8, i64)>) -> Result<()> {
        match retry {
            Some((attempts, at)) => self.conn.execute(
                "INSERT INTO start_retries(task_id,attempts,retry_at) VALUES(?1,?2,?3) ON CONFLICT(task_id) DO UPDATE SET attempts=excluded.attempts,retry_at=excluded.retry_at",
                params![task_id.0, i64::from(attempts), at])?,
            None => self.conn.execute("DELETE FROM start_retries WHERE task_id=?1", [&task_id.0])?,
        };
        self.note_side_write();
        Ok(())
    }
    /// The blocks an older daemon left on tasks it found without a launch plan: every task has one
    /// now, so they start again.
    pub(crate) fn forget_planless_blocks(&mut self) -> Result<usize> {
        let forgotten = self.conn.execute("DELETE FROM auto_launches WHERE reason='no_plan'", [])?;
        self.note_side_write();
        Ok(forgotten)
    }
    pub(crate) fn forget_auto_launch(&mut self, task_id: &TaskId) -> Result<()> {
        self.conn.execute("DELETE FROM auto_launches WHERE task_id=?1", [&task_id.0])?;
        self.note_side_write();
        Ok(())
    }
    /// Whether a launch of this task was ever recorded.
    pub(crate) fn task_was_launched(&self, task_id: &TaskId) -> Result<bool> {
        #[cfg(test)]
        let mut trace = self.trace_live_read("task_was_launched");
        #[cfg(test)]
        if let Some(trace) = &mut trace { trace.statement(); }
        let value = self.conn.query_row("SELECT EXISTS(SELECT 1 FROM model_decisions WHERE task_id=?1 AND kind IN('launch','relaunch'))", [&task_id.0], |row| { #[cfg(test)] if let Some(trace) = &mut trace { trace.row(); } row.get(0) })?;
        #[cfg(test)]
        if let Some(trace) = &mut trace { trace.complete(); }
        Ok(value)
    }
    /// Whether Alondra launched this session: a launch of it was recorded, or its agent was assigned
    /// a task by Alondra's own launch path (launch, relaunch, automatic launch), which is what
    /// launches did before decisions were kept. An `assign_task` the orchestrator sent to a session
    /// of the user's is not one.
    pub(crate) fn launched_by_alondra(&self, agent_id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM model_decisions WHERE agent_id=?1 AND kind IN('launch','relaunch'))
                 OR EXISTS(SELECT 1 FROM command_audit WHERE json_extract(command,'$.kind')='assign_task' AND json_extract(command,'$.agent_id')=?1
                   AND json_extract(actor,'$.kind') IN('orchestrator','system') AND command_id LIKE 'orchestrator-tool-%')",
            [agent_id], |row| row.get(0))?)
    }
    pub(crate) fn task_closing(&self, task_id: &TaskId) -> Result<Option<TaskClosing>> {
        Ok(self.conn.query_row(TASK_CLOSING_SELECT_ONE, [&task_id.0], TaskClosing::from_row).optional()?)
    }
    pub(crate) fn task_closings(&self) -> Result<Vec<TaskClosing>> {
        let mut query = self.conn.prepare(TASK_CLOSING_SELECT_ALL)?;
        Ok(query.query_map([], TaskClosing::from_row)?.collect::<rusqlite::Result<_>>()?)
    }
    /// The closer's row for a task, created the first time it sees the task due for closing.
    pub(crate) fn begin_task_closing(&mut self, task_id: &TaskId, now: i64) -> Result<TaskClosing> {
        self.conn.execute("INSERT OR IGNORE INTO task_closings(task_id,since) VALUES(?1,?2)", params![task_id.0, now])?;
        self.note_side_write();
        self.task_closing(task_id)?.context("task closing row missing")
    }
    pub(crate) fn save_task_closing(&mut self, row: &TaskClosing) -> Result<()> {
        self.conn.execute(
            "UPDATE task_closings SET since=?2,failures=?3,last_error=?4,next_attempt=?5,attempted_cleaned_at=?6,rm_started_at=?7,problem_id=?8,ready_since=?9 WHERE task_id=?1",
            params![row.task_id.0, row.since, row.failures, row.last_error, row.next_attempt, row.attempted_cleaned_at, row.rm_started_at, row.problem_id, row.ready_since])?;
        self.note_side_write();
        Ok(())
    }
    pub(crate) fn post_merge_check(&self, task_id: &TaskId) -> Result<Option<PostMergeCheck>> {
        Ok(self.conn.query_row(
            "SELECT task_id,declared_at,since,asked_at,problem_id,close_now FROM post_merge_checks WHERE task_id=?1",
            [&task_id.0],
            |row| Ok(PostMergeCheck {
                task_id: TaskId(row.get(0)?), declared_at: row.get(1)?, since: row.get(2)?, asked_at: row.get(3)?,
                problem_id: row.get(4)?, close_now: row.get::<_, i64>(5)? != 0,
            }),
        ).optional()?)
    }
    pub(crate) fn save_post_merge_check(&mut self, check: &PostMergeCheck) -> Result<()> {
        self.conn.execute(
            "INSERT INTO post_merge_checks(task_id,declared_at,since,asked_at,problem_id,close_now) VALUES(?1,?2,?3,?4,?5,?6) \
             ON CONFLICT(task_id) DO UPDATE SET declared_at=excluded.declared_at,since=excluded.since,asked_at=excluded.asked_at,\
             problem_id=excluded.problem_id,close_now=excluded.close_now",
            params![check.task_id.0, check.declared_at, check.since, check.asked_at, check.problem_id, check.close_now as i64])?;
        self.note_side_write();
        Ok(())
    }
    pub(crate) fn forget_post_merge_check(&mut self, task_id: &TaskId) -> Result<()> {
        self.conn.execute("DELETE FROM post_merge_checks WHERE task_id=?1", [&task_id.0])?;
        self.note_side_write();
        Ok(())
    }
    pub(crate) fn forget_task_closing(&mut self, task_id: &TaskId) -> Result<()> {
        self.conn.execute("DELETE FROM task_closings WHERE task_id=?1", [&task_id.0])?;
        self.note_side_write();
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn conn_for_test(&self) -> &Connection {
        &self.conn
    }
    /// A merge attempt a node claimed under the flow where nodes merged their own PRs.
    #[cfg(test)]
    pub(crate) fn claim_as_node(&mut self, authorization: &str, agent: &str) {
        self.conn.execute("UPDATE merge_authorizations SET attempt='claimed',claimed_by=?2 WHERE id=?1", params![authorization, agent]).unwrap();
    }
    pub(crate) fn set_cancel_reason(&self, task_id: &TaskId, reason: &str) -> Result<()> {
        self.conn.execute("INSERT INTO task_cancel_reason(task_id,reason) VALUES(?1,?2) ON CONFLICT(task_id) DO UPDATE SET reason=excluded.reason",
            params![task_id.0, reason])?;
        Ok(())
    }
    pub(crate) fn cancel_reasons(&self) -> Result<std::collections::BTreeMap<TaskId, String>> {
        #[cfg(test)]
        let mut trace = self.trace_live_read("cancel_reasons");
        let mut query = self.conn.prepare("SELECT task_id,reason FROM task_cancel_reason")?;
        #[cfg(test)]
        if let Some(trace) = &mut trace { trace.statement(); }
        let rows = query.query_map([], |row| { #[cfg(test)] if let Some(trace) = &mut trace { trace.row(); } Ok((TaskId(row.get(0)?), row.get(1)?)) })?;
        let values = rows.collect::<rusqlite::Result<_>>()?;
        #[cfg(test)]
        if let Some(trace) = &mut trace { trace.complete(); }
        Ok(values)
    }
    pub(crate) fn task_history(&self, task_id: &TaskId) -> Result<Vec<HistoryStep>> {
        let mut query = self.conn.prepare("SELECT a.at,h.state,h.summary FROM task_history h JOIN command_audit a ON a.revision=h.revision
            WHERE h.task_id=?1 ORDER BY h.revision")?;
        let rows = query.query_map([&task_id.0], |row| Ok(HistoryStep { at: row.get(0)?, state: row.get(1)?, summary: row.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    /// When an outbox message was written and the kind of actor whose command wrote it
    /// (`user`, `orchestrator`, `agent` or `system`).
    pub(crate) fn message_origin(&self, id: i64) -> Result<Option<(i64, String)>> {
        Ok(self.conn.query_row(
            "SELECT a.at,coalesce(json_extract(a.actor,'$.kind'),'') FROM domain_messages m JOIN command_audit a ON a.revision=m.revision WHERE m.id=?1",
            [id], |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?)
    }
    /// The notices for the orchestrator that left the outbox: handed to it, or given up.
    pub(crate) fn orchestrator_notices_out(&self) -> Result<Vec<String>> {
        let mut q = self.conn.prepare("SELECT DISTINCT m.interaction_id FROM domain_messages m JOIN domain_outbox o ON o.message_id=m.id \
            LEFT JOIN domain_message_user_targets u ON u.message_id=m.id \
            WHERE m.recipient_kind='orchestrator' AND u.message_id IS NULL AND m.interaction_id IS NOT NULL AND o.state IN('handed','discarded')")?;
        Ok(q.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
    }

    pub(crate) fn pending_messages(&self) -> Result<Vec<OutboxMessage>> {
        let mut q=self.conn.prepare("SELECT m.id,CASE WHEN u.message_id IS NOT NULL THEN 'user' ELSE m.recipient_kind END,m.agent_id,m.task_id,m.interaction_id,m.message,o.state FROM domain_messages m JOIN command_audit a ON a.revision=m.revision JOIN domain_outbox o ON o.message_id=m.id LEFT JOIN domain_message_user_targets u ON u.message_id=m.id WHERE o.state IN('pending','retryable') ORDER BY a.at,m.id")?;
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
    /// Whether the user was already pushed any of these cards: its push left (or may have) and
    /// the record outlives a daemon restart.
    pub(crate) fn user_push_handed(&self, interaction_ids: &[InteractionId]) -> Result<bool> {
        let mut q = self.conn.prepare("SELECT EXISTS(SELECT 1 FROM domain_messages m JOIN domain_message_user_targets u ON u.message_id=m.id
            JOIN domain_outbox o ON o.message_id=m.id WHERE m.interaction_id=?1 AND o.state IN('handed','uncertain'))")?;
        for id in interaction_ids {
            if q.query_row([&id.0], |row| row.get::<_, bool>(0))? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// The latest user notice the push gate let through for each card, by its message id: what a
    /// phone was told about that card, and what the Mac app tells the user while it is not in front.
    pub(crate) fn handed_user_notices(&self, interaction_ids: &[InteractionId]) -> Result<Vec<(i64, InteractionId)>> {
        let mut q = self.conn.prepare("SELECT MAX(m.id) FROM domain_messages m JOIN domain_message_user_targets u ON u.message_id=m.id
            JOIN domain_outbox o ON o.message_id=m.id WHERE m.interaction_id=?1 AND o.state IN('handed','uncertain')")?;
        let mut notices = Vec::new();
        for id in interaction_ids {
            if let Some(message) = q.query_row([&id.0], |row| row.get::<_, Option<i64>>(0))? {
                notices.push((message, id.clone()));
            }
        }
        notices.sort();
        Ok(notices)
    }
    /// Expire orchestrator inbox entries older than the supplied cutoff.
    /// User notifications share the `orchestrator` recipient kind but are
    /// distinguished by their user-target row and keep their own delivery policy.
    pub(crate) fn discard_expired_orchestrator_messages(&mut self, cutoff: i64) -> Result<usize> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_schema(&tx)?;
        let changed = tx.execute(
            "UPDATE domain_outbox SET state='discarded' WHERE state IN('pending','retryable') AND message_id IN (
                SELECT m.id FROM domain_messages m JOIN command_audit a ON a.revision=m.revision
                WHERE m.recipient_kind='orchestrator' AND a.at<?1
                AND NOT EXISTS (SELECT 1 FROM domain_message_user_targets u WHERE u.message_id=m.id)
            )",
            [cutoff],
        )?;
        tx.commit()?;
        if changed > 0 { self.note_side_write(); }
        Ok(changed)
    }
    #[cfg(test)]
    pub(crate) fn set_message_created_at_for_test(&mut self, id: i64, at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE command_audit SET at=?1 WHERE revision=(SELECT revision FROM domain_messages WHERE id=?2)",
            params![at, id],
        )?;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn message_outbox_state_for_test(&self, id: i64) -> Result<String> {
        Ok(self.conn.query_row(
            "SELECT state FROM domain_outbox WHERE message_id=?1", [id], |row| row.get(0),
        )?)
    }
    pub(crate) fn claim_message(&mut self, id: i64) -> Result<Option<DeliveryClaim>> {
        self.claim_message_inner(id, false, None)
    }
    pub(crate) fn claim_orchestrator_message(&mut self, id: i64) -> Result<Option<DeliveryClaim>> {
        self.claim_message_inner(id, true, None)
    }
    /// The orchestrator session a claimed message was handed to.
    pub(crate) fn record_orchestrator_delivery(&mut self, attempt_id: i64, session: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO orchestrator_deliveries(attempt_id,session_id) VALUES(?1,?2)",
            params![attempt_id, session],
        )?;
        self.note_side_write();
        Ok(())
    }
    pub(crate) fn orchestrator_delivery(&self, attempt_id: i64) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT session_id FROM orchestrator_deliveries WHERE attempt_id=?1", [attempt_id], |r| r.get(0)).optional()?)
    }
    #[cfg(test)]
    pub(crate) fn claim_orchestrator_message_at_for_test(
        &mut self,
        id: i64,
        now: i64,
    ) -> Result<Option<DeliveryClaim>> {
        self.claim_message_inner(id, true, Some(now))
    }
    fn claim_message_inner(
        &mut self,
        id: i64,
        expire_orchestrator: bool,
        test_now: Option<i64>,
    ) -> Result<Option<DeliveryClaim>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_schema(&tx)?;
        let row = tx.query_row(
            "SELECT m.id,CASE WHEN u.message_id IS NOT NULL THEN 'user' ELSE m.recipient_kind END,
                m.agent_id,m.task_id,m.interaction_id,m.message,o.state,a.at,m.recipient_kind,
                u.message_id IS NOT NULL
             FROM domain_messages m JOIN command_audit a ON a.revision=m.revision
             JOIN domain_outbox o ON o.message_id=m.id
             LEFT JOIN domain_message_user_targets u ON u.message_id=m.id
             WHERE m.id=?1 AND o.state IN('pending','retryable')",
            [id],
            |r| Ok((OutboxMessage {
                id: r.get(0)?, recipient_kind: r.get(1)?, agent_id: r.get(2)?,
                task_id: r.get(3)?, interaction_id: r.get(4)?, message: r.get(5)?, state: r.get(6)?,
            }, r.get::<_, i64>(7)?, r.get::<_, String>(8)?, r.get::<_, bool>(9)?)),
        ).optional()?;
        let Some((message, created_at, actual_recipient, user_target)) = row else {
            return Ok(None);
        };
        if expire_orchestrator && actual_recipient == "orchestrator" && !user_target {
            let now = test_now.unwrap_or_else(alondra_core::now_millis);
            if created_at < now.saturating_sub(ORCHESTRATOR_MESSAGE_TTL_MS) {
                tx.execute(
                    "UPDATE domain_outbox SET state='discarded' WHERE message_id=?1 AND state IN('pending','retryable')",
                    [id],
                )?;
                tx.commit()?;
                self.note_side_write();
                return Ok(None);
            }
        }
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
    /// Resolve one uncertain attempt from the daemon's own recovery, where no
    /// transport receipt exists; a device's decision goes through the
    /// authenticated variant.
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
#[path = "domain_store/load_bench.rs"]
mod load_bench;
#[cfg(test)]
#[path = "domain_store/acceptance_tests.rs"]
mod acceptance_tests;
#[cfg(test)]
#[path = "domain_store/schedule_acceptance_tests.rs"]
mod schedule_acceptance_tests;
