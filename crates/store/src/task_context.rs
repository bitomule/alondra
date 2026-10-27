//! Public conversation content retained independently of the raw transport journal.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub fn native_context_id(identity: &str) -> String {
    if identity.len() <= 4096 && !identity.contains('\0') { identity.to_owned() }
    else { format!("sha256:{:x}", Sha256::digest(identity.as_bytes())) }
}

pub const CONTEXT_ITEM_BYTES: usize = 256 * 1024;
pub const TOOL_EXCERPT_BYTES: usize = 16 * 1024;
pub const TASK_CONTEXT_BYTES: usize = 32 * 1024 * 1024;
pub const TASK_CONTEXT_ITEMS: u64 = 8192;
const PAGE_BYTES: usize = 2 * 1024 * 1024;

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS task_context_items(\n\
 id INTEGER PRIMARY KEY, task_id TEXT NOT NULL REFERENCES lifecycle_tasks(task_id),\n\
 generation INTEGER NOT NULL CHECK(generation>0), cursor INTEGER NOT NULL CHECK(cursor>0),\n\
 native_id TEXT NOT NULL, source TEXT NOT NULL, content TEXT NOT NULL,\n\
 omitted_bytes INTEGER NOT NULL CHECK(omitted_bytes>=0), exit_code INTEGER, artifact TEXT,\n\
 digest BLOB NOT NULL CHECK(length(digest)=32),\n\
 UNIQUE(task_id,generation,native_id,source));\n\
CREATE UNIQUE INDEX IF NOT EXISTS task_context_task ON task_context_items(task_id,id);\n\
CREATE TABLE IF NOT EXISTS task_context_usage(task_id TEXT PRIMARY KEY REFERENCES lifecycle_tasks(task_id),items INTEGER NOT NULL CHECK(items>=0),bytes INTEGER NOT NULL CHECK(bytes>=0));
\
CREATE TABLE IF NOT EXISTS task_context_gaps(\n\
 task_id TEXT PRIMARY KEY REFERENCES lifecycle_tasks(task_id), missing INTEGER NOT NULL, reason TEXT NOT NULL);\n\
CREATE TABLE IF NOT EXISTS task_context_pins(\n\
 transfer TEXT NOT NULL, task_id TEXT NOT NULL REFERENCES lifecycle_tasks(task_id),\n\
 through_id INTEGER NOT NULL CHECK(through_id>=0), PRIMARY KEY(transfer,task_id));";

pub(crate) const LEGACY_COVERAGE: &str = "INSERT OR IGNORE INTO task_context_gaps(task_id,missing,reason)
\
 SELECT task_id,1,'This task predates retained public conversation capture; its earlier provider history is not verified'
\
 FROM lifecycle_tasks WHERE task_id IN(SELECT task_id FROM provider_normalization_checkpoints
\
 UNION SELECT task_id FROM provider_observations UNION SELECT task_id FROM lifecycle_executions);";

pub(crate) const USAGE_BACKFILL: &str = "INSERT OR IGNORE INTO task_context_usage(task_id,items,bytes) SELECT i.task_id,count(*),sum(length(CAST(i.content AS BLOB))+2*length(CAST(i.native_id AS BLOB))+2*coalesce(length(CAST(i.artifact AS BLOB)),0)+3*length(CAST(i.task_id AS BLOB))+1024) FROM task_context_items i WHERE NOT EXISTS(SELECT 1 FROM task_context_usage u WHERE u.task_id=i.task_id) GROUP BY i.task_id;";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource { Assistant, ToolResult }

impl ContextSource {
    fn as_str(self) -> &'static str {
        match self { Self::Assistant => "assistant", Self::ToolResult => "tool_result" }
    }
    fn decode(value: &str) -> Result<Self> {
        match value { "assistant" => Ok(Self::Assistant), "tool_result" => Ok(Self::ToolResult), _ => Err(Error::CorruptJournal) }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskContextItem {
    pub native_id: String,
    pub source: ContextSource,
    pub content: String,
    pub omitted_bytes: u64,
    pub exit_code: Option<i64>,
    pub artifact: Option<String>,
}

fn prefix(text: &str, capacity: usize) -> &str {
    let mut end = text.len().min(capacity);
    while !text.is_char_boundary(end) { end -= 1; }
    &text[..end]
}

impl TaskContextItem {
    pub fn text(native_id: impl Into<String>, source: ContextSource, content: impl Into<String>) -> Result<Self> {
        let native_id = native_id.into();
        if native_id.is_empty() || native_id.len() > 4096 || native_id.contains('\0') { return Err(Error::InvalidInput); }
        let content = content.into();
        let capacity = if source == ContextSource::ToolResult { TOOL_EXCERPT_BYTES } else { CONTEXT_ITEM_BYTES };
        let excerpt = prefix(&content, capacity);
        Ok(Self { native_id, source, content: excerpt.into(), omitted_bytes: (content.len() - excerpt.len()) as u64, exit_code: None, artifact: None })
    }

    pub fn tool_result(native_id: impl Into<String>, content: impl Into<String>, exit_code: Option<i64>, artifact: Option<String>) -> Result<Self> {
        let mut item = Self::text(native_id, ContextSource::ToolResult, content)?;
        if artifact.as_ref().is_some_and(|reference| reference.is_empty() || reference.len() > 4096 || reference.contains('\0')) { return Err(Error::InvalidInput); }
        item.exit_code = exit_code;
        item.artifact = artifact;
        Ok(item)
    }

    fn validate(&self) -> Result<()> {
        let capacity = if self.source == ContextSource::ToolResult { TOOL_EXCERPT_BYTES } else { CONTEXT_ITEM_BYTES };
        if self.native_id.is_empty() || self.native_id.len() > 4096 || self.native_id.contains('\0') || self.content.len() > capacity
            || self.omitted_bytes > i64::MAX as u64
            || self.artifact.as_ref().is_some_and(|reference| reference.is_empty() || reference.len() > 4096 || reference.contains('\0'))
        { return Err(Error::InvalidInput); }
        Ok(())
    }
    fn digest(&self) -> Result<Vec<u8>> { Ok(Sha256::digest(serde_json::to_vec(self)?).to_vec()) }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRef {
    pub task: TaskId,
    pub sequence: u64,
    pub generation: u64,
    pub cursor: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryCoverage {
    pub through: Option<ContextRef>,
    pub omitted_items: u64,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextEntry {
    pub reference: ContextRef,
    pub item: TaskContextItem,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPage {
    pub entries: Vec<ContextEntry>,
    pub next: Option<u64>,
    pub coverage: HistoryCoverage,
}

impl LifecycleTransaction<'_> {
    /// Called inside the observation/checkpoint transaction. A repeated native item is checked
    /// by digest; capacity exhaustion records a gap without stopping the node.
    pub fn capture_context(&mut self, execution: &ExecutionStamp, cursor: u64, items: &[TaskContextItem]) -> Result<()> {
        staging!(self, {
            if cursor == 0 || items.len() > 256 { return Err(Error::InvalidInput); }
            let lifecycle = self.load(execution.task())?;
            if lifecycle.execution() != Some(execution) { return Err(Error::BindingRevoked); }
            let checkpoint = self.normalization_checkpoint(execution)?.ok_or(Error::CheckpointConflict)?;
            if checkpoint.cursor() != cursor { return Err(Error::CheckpointConflict); }
            for item in items {
                item.validate()?;
                let digest = item.digest()?;
                let previous: Option<Vec<u8>> = self.transaction.query_row(
                    "SELECT digest FROM task_context_items WHERE task_id=?1 AND generation=?2 AND native_id=?3 AND source=?4",
                    params![execution.task().0, sql_number(execution.generation())?, item.native_id, item.source.as_str()], |row| row.get(0),
                ).optional()?;
                if let Some(previous) = previous {
                    if previous != digest { self.context_gap(execution.task(), "A native item changed after its public content was retained")?; }
                    continue;
                }
                let (count, used): (u64,u64) = self.transaction.query_row("SELECT items,bytes FROM task_context_usage WHERE task_id=?1", [&execution.task().0], |row| Ok((read_number(row,0)?,read_number(row,1)?))).optional()?.unwrap_or((0,0));
                let charge = item.content.len() as u64 + 2 * item.native_id.len() as u64 + 2 * item.artifact.as_ref().map_or(0, String::len) as u64 + 3 * execution.task().0.len() as u64 + 1024;
                if count >= TASK_CONTEXT_ITEMS || used.saturating_add(charge) > TASK_CONTEXT_BYTES as u64 {
                    self.context_gap(execution.task(), "The retained provider conversation reached its capacity")?;
                    continue;
                }
                self.transaction.execute(
                    "INSERT INTO task_context_items(task_id,generation,cursor,native_id,source,content,omitted_bytes,exit_code,artifact,digest) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                    params![execution.task().0, sql_number(execution.generation())?, sql_number(cursor)?, item.native_id, item.source.as_str(), item.content, sql_number(item.omitted_bytes)?, item.exit_code, item.artifact, digest],
                )?;
                self.transaction.execute("INSERT INTO task_context_usage(task_id,items,bytes) VALUES(?1,1,?2) ON CONFLICT(task_id) DO UPDATE SET items=items+1,bytes=bytes+excluded.bytes", params![execution.task().0,sql_number(charge)?])?;
            }
            Ok(())
        })
    }

    fn context_gap(&self, task: &TaskId, reason: &str) -> Result<()> {
        self.transaction.execute("INSERT INTO task_context_gaps(task_id,missing,reason) VALUES(?1,1,?2) ON CONFLICT(task_id) DO UPDATE SET missing=missing+1,reason=excluded.reason", params![task.0, reason])?;
        Ok(())
    }

    /// The application authorizes this task before exposing a page to its bound agent.
    pub fn context_page(&self, task: &TaskId, after: Option<&ContextRef>, limit: usize) -> Result<ContextPage> {
        self.inspect(|| {
            self.load(task)?;
            if !(1..=128).contains(&limit) || after.is_some_and(|reference| &reference.task != task) { return Err(Error::InvalidInput); }
            if let Some(reference) = after {
                let valid: bool = self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM task_context_items WHERE task_id=?1 AND id=?2 AND generation=?3 AND cursor=?4)", params![task.0, sql_number(reference.sequence)?, sql_number(reference.generation)?, sql_number(reference.cursor)?], |row| row.get(0))?;
                if !valid { return Err(Error::InvalidInput); }
            }
            let after = after.map_or(0, |reference| reference.sequence);
            let mut statement = self.transaction.prepare(
                "SELECT id,generation,cursor,native_id,source,content,omitted_bytes,exit_code,artifact FROM task_context_items WHERE task_id=?1 AND id>?2 ORDER BY id LIMIT ?3",
            )?;
            let rows = statement.query_map(params![task.0, sql_number(after)?, i64::try_from(limit + 1).map_err(|_| Error::InvalidInput)?], |row| {
                Ok((read_number(row, 0)?, read_number(row, 1)?, read_number(row, 2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?, read_number(row, 6)?, row.get::<_, Option<i64>>(7)?, row.get::<_, Option<String>>(8)?))
            })?;
            let mut entries = Vec::with_capacity(limit);
            let mut encoded_bytes = 0usize;
            let mut more = false;
            for row in rows {
                let (sequence, generation, cursor, native_id, source, content, omitted_bytes, exit_code, artifact) = row?;
                let entry = ContextEntry { reference: ContextRef { task: task.clone(), sequence, generation, cursor }, item: TaskContextItem { native_id, source: ContextSource::decode(&source)?, content, omitted_bytes, exit_code, artifact } };
                let size = serde_json::to_vec(&entry)?.len();
                if entries.len() == limit || encoded_bytes.saturating_add(size) > PAGE_BYTES { more = true; break; }
                encoded_bytes += size;
                entries.push(entry);
            }
            let next = more.then(|| entries.last().expect("a nonempty context page").reference.sequence);
            let gap: Option<(u64, String)> = self.transaction.query_row("SELECT missing,reason FROM task_context_gaps WHERE task_id=?1", [&task.0], |row| Ok((read_number(row, 0)?, row.get(1)?))).optional()?;
            let coverage = HistoryCoverage { through: entries.last().map(|entry| entry.reference.clone()), omitted_items: gap.as_ref().map_or(0, |(missing, _)| *missing), reason: gap.map(|(_, reason)| reason) };
            Ok(ContextPage { entries, next, coverage })
        })
    }

    pub fn pin_context(&mut self, transfer: &str, through: &ContextRef) -> Result<()> {
        staging!(self, {
            identity(transfer)?;
            self.load(&through.task)?;
            let valid: bool = self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM task_context_items WHERE task_id=?1 AND id=?2 AND generation=?3 AND cursor=?4)", params![through.task.0, sql_number(through.sequence)?, sql_number(through.generation)?, sql_number(through.cursor)?], |row| row.get(0))?;
            if !valid { return Err(Error::InvalidInput); }
            let previous: Option<u64> = self.transaction.query_row("SELECT through_id FROM task_context_pins WHERE transfer=?1 AND task_id=?2", params![transfer, through.task.0], |row| read_number(row, 0)).optional()?;
            if previous.is_some_and(|sequence| sequence != through.sequence) { return Err(Error::InputConflict); }
            self.transaction.execute("INSERT OR IGNORE INTO task_context_pins(transfer,task_id,through_id) VALUES(?1,?2,?3)", params![transfer, through.task.0, sql_number(through.sequence)?])?;
            Ok(())
        })
    }

    pub fn release_context(&mut self, transfer: &str, task: &TaskId) -> Result<()> {
        staging!(self, {
            identity(transfer)?;
            self.load(task)?;
            self.transaction.execute("DELETE FROM task_context_pins WHERE transfer=?1 AND task_id=?2", params![transfer, task.0])?;
            Ok(())
        })
    }

    /// Explicit compaction records a gap and cannot free a pending handoff's references.
    pub fn retain_context(&mut self, task: &TaskId, before: u64, limit: usize) -> Result<usize> {
        staging!(self, {
            self.load(task)?;
            if limit > 1024 { return Err(Error::InvalidInput); }
            let removed = self.transaction.execute(
                "DELETE FROM task_context_items WHERE id IN(SELECT i.id FROM task_context_items i WHERE i.task_id=?1 AND i.id<?2 AND NOT EXISTS(SELECT 1 FROM task_context_pins p WHERE p.task_id=i.task_id AND p.through_id>=i.id) ORDER BY i.id LIMIT ?3)",
                params![task.0, sql_number(before)?, i64::try_from(limit).map_err(|_| Error::InvalidInput)?],
            )?;
            if removed > 0 {
                self.transaction.execute("UPDATE task_context_usage SET items=(SELECT count(*) FROM task_context_items i WHERE i.task_id=?1),bytes=(SELECT coalesce(sum(length(CAST(i.content AS BLOB))+2*length(CAST(i.native_id AS BLOB))+2*coalesce(length(CAST(i.artifact AS BLOB)),0)+3*length(CAST(i.task_id AS BLOB))+1024),0) FROM task_context_items i WHERE i.task_id=?1) WHERE task_id=?1", [&task.0])?;
                self.context_gap(task, "Earlier provider conversation content was explicitly released")?;
            }
            Ok(removed)
        })
    }
}
