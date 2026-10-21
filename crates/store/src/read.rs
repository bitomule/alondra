//! What the daemon reads to rebuild its domain: every task's journal, replayed and checked as
//! `LifecycleTransaction::load` checks it. Reading writes nothing.
use crate::{AuthorityScope, Error, Result, check_epoch, persistence};
use alondra_domain::{Event, InputId, TaskLifecycle};
use alondra_foundation::TaskId;
use rusqlite::{Connection, params};
use std::collections::BTreeMap;

/// One task's journal: the input it was planned with, what the reader made of each event, the
/// text of each input that has one, and the lifecycle its events rebuild. An input of the
/// `claude --bg` backend has no text.
#[derive(Debug, PartialEq, Eq)]
pub struct JournalTask<T> {
    pub initial: InputId,
    pub entries: Vec<T>,
    pub texts: BTreeMap<InputId, String>,
    pub lifecycle: TaskLifecycle,
}

/// Every task's journal under `scope`, replayed once: `read` sees each event with the aggregate it
/// applies to and the task's input texts. A journal that does not replay, or whose relations
/// disagree with its replay, fails the whole read: nothing repairs it.
pub fn read_journal<T, E: From<Error>>(
    connection: &Connection,
    scope: &AuthorityScope,
    mut read: impl FnMut(&TaskLifecycle, &Event, &BTreeMap<InputId, String>) -> std::result::Result<T, E>,
) -> std::result::Result<BTreeMap<TaskId, JournalTask<T>>, E> {
    crate::journal_format::verify(connection)?;
    check_epoch(connection, scope)?;
    let mut tasks = BTreeMap::new();
    let mut statement = connection
        .prepare("SELECT task_id,epoch,initial_input,revision FROM lifecycle_tasks ORDER BY task_id")
        .map_err(Error::from)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, crate::read_number(row, 3)?))
        })
        .map_err(Error::from)?;
    for row in rows {
        let (task, epoch, initial, revision) = row.map_err(Error::from)?;
        if epoch != scope.epoch().0 {
            return Err(Error::WrongScope.into());
        }
        let task = TaskId::from(task.as_str());
        let initial = InputId::new(initial).map_err(Error::from)?;
        let texts = texts(connection, &task)?;
        let (lifecycle, entries) =
            persistence::replay(connection, &task, initial.clone(), revision, |aggregate, event| read(aggregate, event, &texts))?;
        tasks.insert(task, JournalTask { initial, entries, texts, lifecycle });
    }
    Ok(tasks)
}

fn texts(connection: &Connection, task: &TaskId) -> Result<BTreeMap<InputId, String>> {
    let mut texts = BTreeMap::new();
    let mut bodies = connection.prepare_cached("SELECT input_id,body FROM lifecycle_inputs WHERE task_id=?1 AND body IS NOT NULL")?;
    for body in bodies.query_map(params![task.0], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))? {
        let (input, text) = body?;
        texts.insert(InputId::new(input)?, text);
    }
    Ok(texts)
}
