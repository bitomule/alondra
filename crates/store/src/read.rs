//! What the daemon reads to rebuild its domain: every task's journal, replayed and checked as
//! `LifecycleTransaction::load` checks it. Reading writes nothing.
use crate::{AuthorityScope, Error, Result, check_epoch, codec, persistence};
use alondra_domain::{Event, InputId, TaskLifecycle};
use alondra_foundation::TaskId;
use rusqlite::{Connection, params};
use std::collections::BTreeMap;

/// One task's journal: the input it was planned with, its events in order and the text of each
/// input that has one. An input of the `claude --bg` backend has no text.
#[derive(Debug, PartialEq, Eq)]
pub struct JournalTask {
    pub initial: InputId,
    pub events: Vec<Event>,
    pub texts: BTreeMap<InputId, String>,
}

/// Every task's journal under `scope`. A journal that does not replay, or whose relations
/// disagree with its replay, fails the whole read: nothing repairs it.
pub fn read_journal(connection: &Connection, scope: &AuthorityScope) -> Result<BTreeMap<TaskId, JournalTask>> {
    crate::journal_format::verify(connection)?;
    check_epoch(connection, scope)?;
    let mut tasks = BTreeMap::new();
    let mut statement = connection.prepare("SELECT task_id,epoch,initial_input,revision FROM lifecycle_tasks ORDER BY task_id")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, crate::read_number(row, 3)?))
    })?;
    for row in rows {
        let (task, epoch, initial, revision) = row?;
        if epoch != scope.epoch().0 {
            return Err(Error::WrongScope);
        }
        let task = TaskId::from(task.as_str());
        let initial = InputId::new(initial)?;
        let mut aggregate = TaskLifecycle::planned(task.clone(), initial.clone())?;
        let mut known = persistence::KnownInputs::default();
        known.require(connection, &aggregate)?;
        let mut events = vec![];
        let mut journal = connection.prepare_cached("SELECT revision,event FROM lifecycle_events WHERE task_id=?1 ORDER BY revision")?;
        let entries = journal.query_map(params![task.0], |row| Ok((crate::read_number(row, 0)?, row.get::<_, String>(1)?)))?;
        for entry in entries {
            let (next, encoded) = entry?;
            if next != aggregate.revision() + 1 || encoded.len() > crate::MAX_PAYLOAD {
                return Err(Error::CorruptJournal);
            }
            let record: codec::EventRecord = serde_json::from_str(&encoded)?;
            crate::measure::event_decoded();
            let event = record.decode(&aggregate)?;
            aggregate.apply(aggregate.revision(), event.clone()).map_err(|_| Error::CorruptJournal)?;
            known.require(connection, &aggregate)?;
            events.push(event);
        }
        if aggregate.revision() != revision {
            return Err(Error::CorruptJournal);
        }
        persistence::verify_relations(connection, &aggregate)?;
        let mut texts = BTreeMap::new();
        let mut bodies = connection.prepare_cached("SELECT input_id,body FROM lifecycle_inputs WHERE task_id=?1 AND body IS NOT NULL")?;
        for body in bodies.query_map(params![task.0], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))? {
            let (input, text) = body?;
            texts.insert(InputId::new(input)?, text);
        }
        tasks.insert(task, JournalTask { initial, events, texts });
    }
    Ok(tasks)
}
