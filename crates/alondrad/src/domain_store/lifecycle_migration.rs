//! Schema 4 to 5: every task's lifecycle moves into the store's journal, once, when this build
//! first opens the database. Tasks created before the domain kept a journal get the shortest one
//! that leads to the state their columns show; a journal the domain kept in its own table moves as
//! it is. Nothing is committed unless the whole domain then loads and every task agrees with its
//! journal, and a private copy of the database as it was is kept next to it first.
use super::*;
use alondra_store::{AuthorityScope, LifecycleTransaction};
use std::collections::BTreeMap;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

const COPY_SUFFIX: &str = "before-lifecycle-journal";

/// Where the copy of a database migrated by this build is kept.
pub(crate) fn copy_path(database: &Path) -> PathBuf {
    let mut name = database.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{COPY_SUFFIX}"));
    database.with_file_name(name)
}

/// A consistent private copy of the database, through SQLite's backup, made before anything of it
/// changes. An earlier copy is replaced only by a complete new one.
pub(super) fn keep_copy(conn: &Connection, database: &Path) -> Result<PathBuf> {
    keep_copy_as(conn, database, &copy_path(database))
}

/// `keep_copy` at `copy`.
pub(super) fn keep_copy_as(conn: &Connection, database: &Path, copy: &Path) -> Result<PathBuf> {
    let copy = copy.to_path_buf();
    let mut partial = copy.clone().into_os_string();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    match std::fs::remove_file(&partial) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&partial)?;
    {
        let mut target = Connection::open(&partial)?;
        let backup = rusqlite::backup::Backup::new(conn, &mut target)?;
        backup.run_to_completion(256, std::time::Duration::ZERO, None)?;
    }
    std::fs::File::open(&partial)?.sync_all()?;
    std::fs::rename(&partial, &copy)?;
    std::fs::File::open(copy.parent().context("the copy has a folder")?)?.sync_all()?;
    Ok(copy)
}

/// Installs the store's journal, writes every task's journal and commits only once the domain
/// loads from it. `tx` already holds every other change of this open.
pub(super) fn migrate(conn: &Connection, mut tx: rusqlite::Transaction<'_>) -> Result<()> {
    LifecycleTransaction::install(&mut tx)?;
    let epoch: String = tx.query_row("SELECT data_epoch FROM domain_meta WHERE singleton=1", [], |r| r.get(0))?;
    let mut journal = LifecycleTransaction::attach(tx, AuthorityScope::new(alondra_foundation::DataEpoch(epoch))?)?;
    journal.inspect_mut(|journal| -> Result<()> {
        let state = repository::load_rows(conn)?;
        let mut kept = kept_journals(conn)?;
        for (id, task) in &state.tasks {
            let records = match kept.remove(id) {
                Some(records) => with_execution_instances(id, records)?,
                None => journal_for(task).map_err(|error| anyhow::anyhow!("{}", error.message))?,
            };
            repository::save_journal(journal, id, &records, 0)
                .with_context(|| format!("task {id} ({}) cannot move into the lifecycle journal", task_shape(task)))?;
        }
        ensure!(kept.is_empty(), "the lifecycle journal names tasks that do not exist: {:?}", kept.keys().collect::<Vec<_>>());
        conn.execute_batch("DROP TABLE IF EXISTS task_lifecycle_journal")?;
        repository::load(conn).context("the migrated domain does not load")?;
        Ok(())
    })?;
    journal.commit()?;
    Ok(())
}

/// The journals the domain kept in its own table, before the store held them.
fn kept_journals(conn: &Connection) -> Result<BTreeMap<TaskId, Vec<LifecycleRecord>>> {
    let mut journals: BTreeMap<TaskId, Vec<LifecycleRecord>> = BTreeMap::new();
    let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='task_lifecycle_journal')", [], |r| r.get(0))?;
    if !exists {
        return Ok(journals);
    }
    let mut statement = conn.prepare("SELECT task_id, seq, record FROM task_lifecycle_journal ORDER BY task_id, seq")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let (task, seq, record): (String, i64, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
        let records = journals.entry(TaskId::from(task.as_str())).or_default();
        ensure!(records.len() as i64 == seq, "the lifecycle journal of task {task} has a gap before record {seq}");
        records.push(serde_json::from_str(&record)?);
    }
    Ok(journals)
}


#[cfg(test)]
#[path = "lifecycle_migration_tests.rs"]
mod tests;
