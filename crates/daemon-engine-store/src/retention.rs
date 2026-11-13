//! What the store keeps only for a while after its commit. A command's body and its receipt are
//! read while the same command can still arrive again, which is minutes; they are kept a week. The
//! audit row stays for as long as the domain, with only the command's kind: the time of every
//! revision is read for that long. The body lives in its own table, so letting it go removes whole
//! rows and leaves no half-empty pages behind.
use super::DomainStore;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

pub(super) const WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1_000;

/// The part of the command in `json` the audit row keeps: its kind, or all of an `assign_task`,
/// which is how `launched_by_alondra` knows the nodes launched before launches were recorded.
pub(super) fn summary(json: &str) -> String {
    format!("CASE WHEN json_extract({json},'$.kind')='assign_task' THEN {json} ELSE json_object('kind',json_extract({json},'$.kind')) END")
}

/// One table's retention: a pass changes at most `limit` rows and says how many it changed; the
/// owner repeats it until it changes fewer.
pub struct Policy {
    pub name: &'static str,
    pass: fn(&Transaction<'_>, i64, i64) -> rusqlite::Result<usize>,
}

impl Policy {
    pub(super) fn pass(&self, tx: &Transaction<'_>, now: i64, limit: i64) -> rusqlite::Result<usize> {
        (self.pass)(tx, now, limit)
    }
}

pub const POLICIES: [Policy; 8] = [
    Policy { name: "audit rows written with their body", pass: bodies_written_inline },
    Policy { name: "commands older than a week", pass: week_old_commands },
    Policy { name: "undated receipts", pass: undated_receipts },
    Policy { name: "receipts", pass: receipts },
    Policy { name: "messages of ended tasks", pass: messages_of_ended_tasks },
    Policy { name: "usage samples of tasks folded a week ago", pass: super::metrics_fold::samples_of_folded_tasks },
    Policy { name: "idle sessions without a task", pass: super::metrics_fold::idle_sessions_without_task },
    Policy { name: "task metrics older than 90 days, into weeks", pass: super::metrics_fold::tasks_into_weeks },
];

fn mark(tx: &Transaction<'_>, policy: &str) -> rusqlite::Result<Option<i64>> {
    tx.query_row("SELECT revision FROM retention_marks WHERE policy=?1", [policy], |row| row.get(0)).optional()
}
fn set_mark(tx: &Transaction<'_>, policy: &str, revision: i64) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO retention_marks(policy,revision) VALUES(?1,?2) ON CONFLICT(policy) DO UPDATE SET revision=excluded.revision",
        params![policy, revision],
    )?;
    Ok(())
}

/// Audit rows written before bodies had their own table, up to the `inline_bodies_end` mark: the
/// body of one inside the window moves to `command_bodies`, and every one keeps only its summary.
/// Once all are done the store is rebuilt once (`rebuild` mark), since shrinking rows in place
/// leaves their pages mostly empty.
fn bodies_written_inline(tx: &Transaction<'_>, now: i64, limit: i64) -> rusqlite::Result<usize> {
    let Some(end) = mark(tx, "inline_bodies_end")? else { return Ok(0) };
    let from = mark(tx, "inline_bodies")?.unwrap_or(0);
    let last: Option<i64> = tx.query_row(
        "SELECT max(revision) FROM (SELECT revision FROM command_audit WHERE revision>?1 AND revision<=?2 ORDER BY revision LIMIT ?3)",
        params![from, end, limit],
        |row| row.get(0),
    )?;
    let finish = || -> rusqlite::Result<()> {
        tx.execute("DELETE FROM retention_marks WHERE policy IN('inline_bodies','inline_bodies_end')", [])?;
        set_mark(tx, "rebuild", end)
    };
    let Some(last) = last else {
        finish()?;
        return Ok(0);
    };
    tx.execute(
        "INSERT INTO command_bodies(revision,command) SELECT revision,command FROM command_audit WHERE revision>?1 AND revision<=?2 AND at>=?3",
        params![from, last, now - WINDOW_MS],
    )?;
    let moved = tx.execute(
        &format!("UPDATE command_audit SET command={} WHERE revision>?1 AND revision<=?2", summary("command")),
        params![from, last],
    )?;
    if last == end {
        finish()?;
    } else {
        set_mark(tx, "inline_bodies", last)?;
    }
    Ok(moved)
}

/// Commands older than the window lose their body and their receipt. The walk follows revisions
/// from where it stopped and stops at the first one still inside the window.
fn week_old_commands(tx: &Transaction<'_>, now: i64, limit: i64) -> rusqlite::Result<usize> {
    let from = mark(tx, "week_old_commands")?.unwrap_or(0);
    let mut rows = tx.prepare_cached(
        "SELECT revision,actor,command_id,data_epoch,at FROM command_audit WHERE revision>?1 ORDER BY revision LIMIT ?2",
    )?;
    let old = rows
        .query_map(params![from, limit], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, i64>(4)?))
        })?
        .take_while(|row| row.as_ref().map_or(true, |row| row.4 < now - WINDOW_MS))
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let Some(last) = old.last().map(|row| row.0) else { return Ok(0) };
    let mut forget = tx.prepare_cached("DELETE FROM semantic_receipts WHERE actor=?1 AND command_id=?2 AND data_epoch=?3 AND NOT EXISTS(SELECT 1 FROM orchestrator_batch_effects e WHERE e.actor=?1 AND e.command_id=?2 AND e.data_epoch=?3 AND e.pinned=1)")?;
    for (_, actor, command_id, data_epoch, _) in &old {
        forget.execute(params![actor, command_id, data_epoch])?;
    }
    tx.execute("DELETE FROM command_bodies WHERE revision>?1 AND revision<=?2", params![from, last])?;
    set_mark(tx, "week_old_commands", last)?;
    Ok(old.len())
}

/// Receipts written before they carried a time: the first pass dates them, so they last a week
/// from then.
fn undated_receipts(tx: &Transaction<'_>, now: i64, limit: i64) -> rusqlite::Result<usize> {
    tx.execute(
        "UPDATE semantic_receipts SET at=?1 WHERE rowid IN(SELECT rowid FROM semantic_receipts WHERE at IS NULL LIMIT ?2)",
        params![now, limit],
    )
}

fn receipts(tx: &Transaction<'_>, now: i64, limit: i64) -> rusqlite::Result<usize> {
    tx.execute(
        "DELETE FROM semantic_receipts WHERE rowid IN(SELECT r.rowid FROM semantic_receipts r WHERE at<?1 AND NOT EXISTS(SELECT 1 FROM orchestrator_batch_effects e WHERE e.actor=r.actor AND e.command_id=r.command_id AND e.data_epoch=r.data_epoch AND e.pinned=1) LIMIT ?2)",
        params![now - WINDOW_MS, limit],
    )
}

/// A message's text is read only while it waits in the outbox or is retried. Once it was handed or
/// given up and its task is done or cancelled, the row stays for the delivery evidence and the
/// push records, without its text.
fn messages_of_ended_tasks(tx: &Transaction<'_>, _: i64, limit: i64) -> rusqlite::Result<usize> {
    tx.execute(
        "UPDATE domain_messages SET message='' WHERE id IN(
           SELECT m.id FROM domain_messages m JOIN domain_outbox o ON o.message_id=m.id JOIN tasks t ON t.id=m.task_id
           WHERE m.message<>'' AND o.state IN('handed','discarded') AND t.state IN('done','cancelled') LIMIT ?1)",
        [limit],
    )
}

impl DomainStore {
    /// One pass of `policy` in its own transaction. What it changes is no domain state, so the
    /// cached state stays.
    pub fn retain(&mut self, policy: &Policy, now: i64, limit: usize) -> anyhow::Result<usize> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let changed = policy.pass(&tx, now, i64::try_from(limit)?)?;
        tx.commit()?;
        self.note_side_write();
        Ok(changed)
    }
    /// Whether the rows a policy shrank in place are all done and the store is owed its one rebuild.
    pub(super) fn owes_rebuild(&self) -> rusqlite::Result<bool> {
        self.conn.query_row("SELECT EXISTS(SELECT 1 FROM retention_marks WHERE policy='rebuild')", [], |row| row.get(0))
    }
    /// The first time no ended task is left to compact, the cards it shrank in place are done and
    /// the store is owed its one rebuild.
    pub fn ended_tasks_compacted(&self) -> rusqlite::Result<()> {
        let first = self.conn.execute("INSERT OR IGNORE INTO retention_marks(policy,revision) VALUES('ended_tasks_compacted',0)", [])? == 1;
        if first {
            self.conn.execute("INSERT OR IGNORE INTO retention_marks(policy,revision) VALUES('rebuild',0)", [])?;
        }
        Ok(())
    }
    pub(super) fn rebuilt(&self) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM retention_marks WHERE policy='rebuild'", []).map(drop)
    }
}

const AUDIT: &str = "CREATE TABLE command_audit(revision INTEGER PRIMARY KEY,actor TEXT NOT NULL,command_id TEXT NOT NULL,data_epoch TEXT NOT NULL,command TEXT NOT NULL CHECK(json_valid(command)),at INTEGER NOT NULL)";
pub(super) const AUDIT_BOUND_TO_RECEIPTS: &str = "CREATE TABLE command_audit(revision INTEGER PRIMARY KEY,actor TEXT NOT NULL,command_id TEXT NOT NULL,data_epoch TEXT NOT NULL,command TEXT NOT NULL CHECK(json_valid(command)),at INTEGER NOT NULL,FOREIGN KEY(actor,command_id,data_epoch) REFERENCES semantic_receipts(actor,command_id,data_epoch) DEFERRABLE INITIALLY DEFERRED)";

/// A store written before retention: its audit rows lose the foreign key to their receipts, which
/// SQLite allows by editing the table's definition without rewriting it
/// (https://www.sqlite.org/lang_altertable.html), receipts gain their time, and bodies get their
/// own table; the audit rows written with their body are left to `bodies_written_inline`.
pub(super) fn upgrade(tx: &Transaction<'_>) -> anyhow::Result<()> {
    let audit: String = tx.query_row("SELECT sql FROM sqlite_schema WHERE type='table' AND name='command_audit'", [], |row| row.get(0))?;
    if audit == AUDIT_BOUND_TO_RECEIPTS {
        let version: i64 = tx.pragma_query_value(None, "schema_version", |row| row.get(0))?;
        tx.pragma_update(None, "writable_schema", true)?;
        tx.execute("UPDATE sqlite_schema SET sql=?1 WHERE type='table' AND name='command_audit'", [AUDIT])?;
        tx.pragma_update(None, "schema_version", version + 1)?;
        tx.pragma_update(None, "writable_schema", false)?;
    } else {
        anyhow::ensure!(audit == AUDIT, "unexpected command_audit definition");
    }
    let dated: bool = tx.query_row("SELECT count(*) FROM pragma_table_info('semantic_receipts') WHERE name='at'", [], |row| row.get::<_, i64>(0))? == 1;
    if !dated {
        tx.execute_batch("ALTER TABLE semantic_receipts ADD COLUMN at INTEGER")?;
    }
    tx.execute_batch("CREATE TABLE IF NOT EXISTS retention_marks(policy TEXT PRIMARY KEY,revision INTEGER NOT NULL)")?;
    let separate: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='command_bodies')", [], |row| row.get(0))?;
    if !separate {
        tx.execute_batch(
            "CREATE TABLE command_bodies(revision INTEGER PRIMARY KEY REFERENCES command_audit(revision),command TEXT NOT NULL CHECK(json_valid(command)));
             INSERT INTO retention_marks(policy,revision) SELECT 'inline_bodies_end',max(revision) FROM command_audit HAVING count(*)>0;",
        )?;
    }
    Ok(())
}
