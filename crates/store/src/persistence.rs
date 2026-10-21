use crate::{AuthorityScope, Error, Result, check_epoch, codec};
use alondra_domain::{Event, InputId, TaskLifecycle};
use alondra_foundation::{Revision, TaskId};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeSet;

pub(super) fn load(
    connection: &Connection,
    scope: &AuthorityScope,
    task: &TaskId,
) -> Result<TaskLifecycle> {
    let (initial, revision) = head(connection, scope, task)?;
    crate::measure::aggregate_loaded();
    let (aggregate, _) = replay(connection, task, InputId::new(initial)?, revision, |_, _| Ok::<_, Error>(()))?;
    Ok(aggregate)
}

/// The aggregate a task's stored events rebuild, checked as `load` checks it, and what `read`
/// made of each event from the aggregate it applies to.
pub(super) fn replay<T, E: From<Error>>(
    connection: &Connection,
    task: &TaskId,
    initial: InputId,
    revision: Revision,
    mut read: impl FnMut(&TaskLifecycle, &Event) -> std::result::Result<T, E>,
) -> std::result::Result<(TaskLifecycle, Vec<T>), E> {
    let mut aggregate = TaskLifecycle::planned(task.clone(), initial).map_err(Error::from)?;
    let mut known = KnownInputs::default();
    known.require(connection, &aggregate)?;
    let mut statement = connection
        .prepare_cached("SELECT revision,event FROM lifecycle_events WHERE task_id=?1 ORDER BY revision")
        .map_err(Error::from)?;
    let rows = statement
        .query_map(params![task.0], |row| Ok((crate::read_number(row, 0)?, row.get::<_, String>(1)?)))
        .map_err(Error::from)?;
    let mut entries = vec![];
    for row in rows {
        let (next, encoded) = row.map_err(Error::from)?;
        if next != aggregate.revision() + 1 || encoded.len() > crate::MAX_PAYLOAD {
            return Err(Error::CorruptJournal.into());
        }
        let event: codec::EventRecord = serde_json::from_str(&encoded).map_err(Error::from)?;
        crate::measure::event_decoded();
        let event = event.decode(&aggregate)?;
        entries.push(read(&aggregate, &event)?);
        let named: Vec<InputId> = event.inputs().into_iter().cloned().collect();
        aggregate
            .apply(aggregate.revision(), event)
            .map_err(|_| Error::CorruptJournal)?;
        known.require_named(connection, &aggregate, &named)?;
    }
    if aggregate.revision() != revision {
        return Err(Error::CorruptJournal.into());
    }
    verify_relations(connection, &aggregate)?;
    Ok((aggregate, entries))
}

/// The checks a replay ends with, for an aggregate this transaction already replayed.
pub(super) fn revalidate(
    connection: &Connection,
    scope: &AuthorityScope,
    aggregate: &TaskLifecycle,
) -> Result<()> {
    let (_, revision) = head(connection, scope, aggregate.task())?;
    let (events, last): (Revision, Revision) = connection.query_row(
        "SELECT count(*),coalesce(max(revision),0) FROM lifecycle_events WHERE task_id=?1",
        params![aggregate.task().0],
        |row| Ok((crate::read_number(row, 0)?, crate::read_number(row, 1)?)),
    )?;
    if aggregate.revision() != revision || (events, last) != (revision, revision) {
        return Err(Error::CorruptJournal);
    }
    require_inputs(connection, aggregate)?;
    verify_relations(connection, aggregate)
}

fn head(connection: &Connection, scope: &AuthorityScope, task: &TaskId) -> Result<(String, Revision)> {
    check_epoch(connection, scope)?;
    let row: Option<(String, String, Revision)> = connection
        .query_row(
            "SELECT epoch,initial_input,revision FROM lifecycle_tasks WHERE task_id=?1",
            params![task.0],
            |row| Ok((row.get(0)?, row.get(1)?, crate::read_number(row, 2)?)),
        )
        .optional()?;
    let (epoch, initial, revision) = row.ok_or(Error::MissingTask)?;
    if epoch != scope.epoch.0 {
        return Err(Error::WrongScope);
    }
    Ok((initial, revision))
}

pub(super) fn require_inputs(connection: &Connection, aggregate: &TaskLifecycle) -> Result<()> {
    KnownInputs::default().require(connection, aggregate)
}

/// The inputs of one task a replay already found stored. Its inputs never change once written, so
/// a replay looks each one up once however many events it stays pending for, and after each event
/// only looks at the inputs that event names: no other can have become pending.
#[derive(Default)]
pub(super) struct KnownInputs(BTreeSet<InputId>);

impl KnownInputs {
    pub(super) fn require(&mut self, connection: &Connection, aggregate: &TaskLifecycle) -> Result<()> {
        for input in aggregate.pending_inputs() {
            if !self.0.contains(input) {
                self.look_up(connection, aggregate, input)?;
            }
        }
        Ok(())
    }
    fn require_named(&mut self, connection: &Connection, aggregate: &TaskLifecycle, named: &[InputId]) -> Result<()> {
        for input in named {
            if !aggregate.is_pending(input) || self.0.contains(input) {
                continue;
            }
            self.look_up(connection, aggregate, input)?;
        }
        Ok(())
    }
    fn look_up(&mut self, connection: &Connection, aggregate: &TaskLifecycle, input: &InputId) -> Result<()> {
        crate::measure::input_checked();
        let exists: bool = connection
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM lifecycle_inputs WHERE task_id=?1 AND input_id=?2)")?
            .query_row(params![aggregate.task().0, input.as_str()], |row| row.get(0))?;
        if !exists {
            return Err(Error::MissingInput);
        }
        self.0.insert(input.clone());
        Ok(())
    }
}

pub(super) fn save_relations(connection: &Connection, aggregate: &TaskLifecycle) -> Result<()> {
    let task = &aggregate.task().0;
    connection.execute(
        "DELETE FROM lifecycle_cards WHERE task_id=?1",
        params![task],
    )?;
    for (card, _) in aggregate.cards() {
        require_card(connection, task, &card.0)?;
        connection.execute(
            "INSERT INTO lifecycle_cards(task_id,card_id) VALUES(?1,?2)",
            params![task, card.0],
        )?;
    }
    connection.execute(
        "DELETE FROM lifecycle_dependencies WHERE task_id=?1",
        params![task],
    )?;
    for dependency in aggregate.dependencies() {
        connection.execute(
            "INSERT INTO lifecycle_dependencies(task_id,dependency_id) VALUES(?1,?2)",
            params![task, dependency.0],
        )?;
    }
    connection.execute(
        "DELETE FROM lifecycle_delivery WHERE task_id=?1",
        params![task],
    )?;
    if let Some(delivery) = aggregate.delivery() {
        require_delivery(connection, task, &delivery.0)?;
        connection.execute(
            "INSERT INTO lifecycle_delivery(task_id,card_id) VALUES(?1,?2)",
            params![task, delivery.0],
        )?;
    }
    connection.execute(
        "DELETE FROM lifecycle_current_execution WHERE task_id=?1",
        params![task],
    )?;
    if let Some(stamp) = aggregate.execution() {
        for value in [
            stamp.task().0.as_str(),
            stamp.node().0.as_str(),
            stamp.instance().as_str(),
            stamp.conversation().as_str(),
            stamp.host().as_str(),
        ] {
            crate::identity(value)?;
        }
        let backend = if crate::through_sdk(stamp).is_ok() { "sdk" } else { "claude_bg" };
        connection.execute("INSERT INTO lifecycle_executions(task_id,generation,node_id,instance_id,conversation_id,host_id,backend) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(task_id,generation) DO NOTHING",
            params![task, crate::sql_number(stamp.generation())?, stamp.node().0, stamp.instance().as_str(), stamp.conversation().as_str(), stamp.host().as_str(), backend])?;
        let exact: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM lifecycle_executions WHERE task_id=?1 AND generation=?2 AND node_id=?3 AND instance_id=?4 AND conversation_id=?5 AND host_id=?6)",
            params![task, crate::sql_number(stamp.generation())?, stamp.node().0, stamp.instance().as_str(), stamp.conversation().as_str(), stamp.host().as_str()], |row| row.get(0))?;
        if !exact {
            return Err(Error::CorruptJournal);
        }
        connection.execute(
            "INSERT INTO lifecycle_current_execution(task_id,generation,node_id) VALUES(?1,?2,?3)",
            params![task, crate::sql_number(stamp.generation())?, stamp.node().0],
        )?;
    }
    Ok(())
}

fn require_card(connection: &Connection, task: &str, card: &str) -> Result<()> {
    let need: Option<String> = connection.query_row("SELECT json_extract(card,'$.need') FROM interactions WHERE task_id=?1 AND id=?2 AND scope_kind='task' AND recipient_kind='user' AND kind IN('question','action_request') AND answer IS NULL AND withdrawn=0 AND json_type(options)='array' AND json_extract(card,'$.kind') IN('decide','review','authorize','provide','test','do','problem') AND json_type(card,'$.need')='text'",
        params![task, card], |row| row.get(0)).optional()?;
    let need = need.ok_or(Error::InvalidCard)?;
    let trimmed = need.trim();
    if need.contains('\0')
        || trimmed.is_empty()
        || trimmed.contains('\n')
        || trimmed.chars().count() > 90
    {
        return Err(Error::InvalidCard);
    }
    Ok(())
}

fn values(connection: &Connection, sql: &str, task: &str) -> Result<BTreeSet<String>> {
    let mut statement = connection.prepare(sql)?;
    Ok(statement
        .query_map(params![task], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
pub(super) fn verify_relations(connection: &Connection, aggregate: &TaskLifecycle) -> Result<()> {
    let task = &aggregate.task().0;
    let cards = values(
        connection,
        "SELECT card_id FROM lifecycle_cards WHERE task_id=?1",
        task,
    )?;
    if cards
        != aggregate
            .cards()
            .iter()
            .map(|(card, _)| card.0.clone())
            .collect()
    {
        return Err(Error::CorruptJournal);
    }
    for card in cards {
        require_card(connection, task, &card)?;
    }
    if values(
        connection,
        "SELECT dependency_id FROM lifecycle_dependencies WHERE task_id=?1",
        task,
    )? != aggregate
        .dependencies()
        .iter()
        .map(|task| task.0.clone())
        .collect()
    {
        return Err(Error::CorruptJournal);
    }
    let delivery: Option<String> = connection
        .query_row(
            "SELECT card_id FROM lifecycle_delivery WHERE task_id=?1",
            params![task],
            |row| row.get(0),
        )
        .optional()?;
    if delivery.as_deref() != aggregate.delivery().map(|card| card.0.as_str()) {
        return Err(Error::CorruptJournal);
    }
    if let Some(delivery) = delivery {
        require_delivery(connection, task, &delivery)?;
    }
    let current: Option<(u64, String, String, String, String)> = connection.query_row(
        "SELECT c.generation,e.node_id,e.instance_id,e.conversation_id,e.host_id FROM lifecycle_current_execution c JOIN lifecycle_executions e USING(task_id,generation) WHERE c.task_id=?1", params![task],
        |row| Ok((crate::read_number(row, 0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))).optional()?;
    match (aggregate.execution(), current) {
        (None, None) => {}
        (Some(stamp), Some((generation, node, instance, conversation, host)))
            if generation == stamp.generation()
                && node == stamp.node().0
                && instance == stamp.instance().as_str()
                && conversation == stamp.conversation().as_str()
                && host == stamp.host().as_str() => {}
        _ => return Err(Error::CorruptJournal),
    }
    Ok(())
}

pub(super) fn require_intents(connection: &Connection, aggregate: &TaskLifecycle) -> Result<()> {
    let Some(stamp) = aggregate.execution() else {
        return Ok(());
    };
    let has_launch: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM lifecycle_dispatches WHERE task_id=?1 AND generation=?2 AND kind='launch')", params![aggregate.task().0, crate::sql_number(stamp.generation())?], |row| row.get(0))?;
    if !has_launch {
        return Err(Error::MissingIntent);
    }
    for input in aggregate.pending_inputs() {
        let exists: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM lifecycle_dispatches WHERE task_id=?1 AND generation=?2 AND input_id=?3)", params![aggregate.task().0, crate::sql_number(stamp.generation())?, input.as_str()], |row| row.get(0))?;
        if !exists {
            return Err(Error::MissingIntent);
        }
    }
    Ok(())
}

fn require_delivery(connection: &Connection, task: &str, delivery: &str) -> Result<()> {
    let valid: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM interactions WHERE task_id=?1 AND id=?2 AND scope_kind='task' AND kind='delivery')", params![task, delivery], |row| row.get(0))?;
    if !valid {
        return Err(Error::InvalidCard);
    }
    Ok(())
}
