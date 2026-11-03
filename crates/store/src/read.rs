//! What the daemon reads to rebuild its domain: every task's journal, replayed and checked as
//! `LifecycleTransaction::load` checks it. Reading writes nothing.
use crate::persistence::{self, KnownInputs, Replay};
use crate::{AuthorityScope, Error, Result, check_epoch};
use alondra_domain::{Event, InputId, TaskLifecycle};
use alondra_foundation::{Revision, TaskId};
use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap};

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
/// Reader failures report the event's location without their potentially private error text.
pub fn read_journal<T, E: From<Error>>(
    connection: &Connection,
    scope: &AuthorityScope,
    mut read: impl FnMut(&TaskLifecycle, &Event, &BTreeMap<InputId, String>) -> std::result::Result<T, E>,
) -> std::result::Result<BTreeMap<TaskId, JournalTask<T>>, E> {
    crate::journal_format::verify(connection)?;
    check_epoch(connection, scope)?;
    let mut heads = BTreeMap::new();
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
        let initial = InputId::new(initial).map_err(Error::from)?;
        heads.insert(task, Head { initial, revision, texts: BTreeMap::new(), known: KnownInputs::default() });
    }
    read_inputs(connection, &mut heads)?;
    let mut replays = in_stored_order(connection, &mut heads, &mut read)?;
    let mut tasks = BTreeMap::new();
    for (task, Head { initial, revision, texts, .. }) in heads {
        let task = TaskId::from(task.as_str());
        let (lifecycle, entries) = match replays.remove(&task.0).flatten() {
            Some(replay) => {
                let (lifecycle, entries, _) = replay.finish(connection, revision)?;
                (lifecycle, entries)
            }
            None => persistence::replay(connection, &task, initial.clone(), revision, &mut Default::default(), |aggregate, event| read(aggregate, event, &texts))?,
        };
        tasks.insert(task, JournalTask { initial, entries, texts, lifecycle });
    }
    Ok(tasks)
}

struct Head {
    initial: InputId,
    revision: Revision,
    texts: BTreeMap<InputId, String>,
    known: KnownInputs,
}

/// Every task's stored inputs in one pass over the table: the texts the reader is given, and the
/// inputs its replay would otherwise look up one at a time.
fn read_inputs(connection: &Connection, heads: &mut BTreeMap<String, Head>) -> Result<()> {
    let mut statement = connection.prepare("SELECT task_id,input_id,body FROM lifecycle_inputs NOT INDEXED")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let Ok(task) = row.get_ref(0)?.as_str() else { continue };
        let Some(head) = heads.get_mut(task) else { continue };
        match row.get::<_, Option<String>>(2)? {
            Some(text) => {
                let input = InputId::new(row.get::<_, String>(1)?)?;
                head.known.stored(&input);
                head.texts.insert(input, text);
            }
            None => {
                if let Some(input) = row.get_ref(1)?.as_str().ok().and_then(|input| InputId::new(input).ok()) {
                    head.known.stored(&input);
                }
            }
        }
    }
    Ok(())
}

/// Every task's events, replayed in the order the table stores them: one pass over its pages
/// instead of one index walk per task, whose rows lie scattered across the whole table. A task
/// whose rows are not stored in revision order, or that fails here, is left out (`None`) and
/// replayed through its index, which reports its failure as before.
fn in_stored_order<T, E: From<Error>>(
    connection: &Connection,
    heads: &mut BTreeMap<String, Head>,
    read: &mut impl FnMut(&TaskLifecycle, &Event, &BTreeMap<InputId, String>) -> std::result::Result<T, E>,
) -> Result<HashMap<String, Option<Replay<T>>>> {
    let mut replays: HashMap<&str, (Option<Replay<T>>, &BTreeMap<InputId, String>)> = heads
        .iter_mut()
        .map(|(task, head)| {
            let known = std::mem::take(&mut head.known);
            let replay = Replay::start(connection, TaskId::from(task.as_str()), head.initial.clone(), known).ok();
            (task.as_str(), (replay, &head.texts))
        })
        .collect();
    let mut statement = connection.prepare("SELECT task_id,revision,event FROM lifecycle_events NOT INDEXED")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let Ok(task) = row.get_ref(0)?.as_str() else { continue };
        let Some((slot, texts)) = replays.get_mut(task) else { continue };
        let Some(replay) = slot else { continue };
        let (Ok(next), Ok(encoded)) = (crate::read_number(row, 1), row.get_ref(2)?.as_str()) else {
            *slot = None;
            continue;
        };
        if next != replay.revision() + 1
            || replay.step(connection, next, encoded, &mut |aggregate, event| read(aggregate, event, texts)).is_err()
        {
            *slot = None;
        }
    }
    Ok(replays.into_iter().map(|(task, (replay, _))| (task.to_owned(), replay)).collect())
}
