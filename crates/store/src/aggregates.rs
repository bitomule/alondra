//! Each task's aggregate, replayed from its journal at most once per transaction while only the
//! store's own operations write the journal. A journal write outside them, a schema change or a
//! failed operation forgets every entry, so the next read replays the journal again.
use crate::Result;
use alondra_domain::TaskLifecycle;
use alondra_foundation::TaskId;
use rusqlite::{Connection, Transaction};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    ops::{Deref, DerefMut},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// The tables a replay reads. Writes to the application's own tables (tasks, interactions, the
/// domain revision in domain_meta…) leave the entries alone: every read checks the epoch and the
/// relations a cached aggregate holds again.
const JOURNAL: [&str; 8] = [
    "lifecycle_tasks",
    "lifecycle_inputs",
    "lifecycle_events",
    "lifecycle_executions",
    "lifecycle_current_execution",
    "lifecycle_cards",
    "lifecycle_dependencies",
    "lifecycle_delivery",
];

#[derive(Default)]
pub(super) struct Aggregates {
    entries: RefCell<BTreeMap<TaskId, Arc<TaskLifecycle>>>,
    journal_writes: Arc<AtomicU64>,
    mark: Cell<Option<Mark>>,
}

/// What the connection looked like when the store's last operation ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Mark {
    journal_writes: u64,
    schema: i64,
}

impl Aggregates {
    fn mark(&self, connection: &Connection) -> Result<Mark> {
        Ok(Mark {
            journal_writes: self.journal_writes.load(Ordering::Relaxed),
            schema: connection.query_row("PRAGMA schema_version", [], |row| row.get(0))?,
        })
    }
    /// Called as an operation begins: whatever wrote the journal since the last one ended was not
    /// the store.
    pub(super) fn revalidate(&self, connection: &Connection) -> Result<()> {
        if self.mark.get() != Some(self.mark(connection)?) {
            self.forget();
        }
        Ok(())
    }
    pub(super) fn settle(&self, connection: &Connection, succeeded: bool) -> Result<()> {
        if succeeded {
            self.mark.set(Some(self.mark(connection)?));
        } else {
            self.forget();
        }
        Ok(())
    }
    fn forget(&self) {
        self.entries.borrow_mut().clear();
        self.mark.set(None);
    }
    pub(super) fn get(&self, task: &TaskId) -> Option<Arc<TaskLifecycle>> {
        self.entries.borrow().get(task).cloned()
    }
    pub(super) fn take(&self, task: &TaskId) -> Option<Arc<TaskLifecycle>> {
        self.entries.borrow_mut().remove(task)
    }
    pub(super) fn keep(&self, aggregate: TaskLifecycle) -> Arc<TaskLifecycle> {
        let aggregate = Arc::new(aggregate);
        self.entries
            .borrow_mut()
            .insert(aggregate.task().clone(), aggregate.clone());
        aggregate
    }
    /// Counts every row change to the journal on this connection until the transaction ends.
    /// SQLite reports no truncation nor rows a REPLACE deletes; a read checks the event count.
    pub(super) fn watch<'connection>(
        &self,
        transaction: Transaction<'connection>,
    ) -> Result<Watched<'connection>> {
        let writes = self.journal_writes.clone();
        transaction.update_hook(Some(move |_, _: &str, table: &str, _| {
            if JOURNAL.contains(&table) {
                writes.fetch_add(1, Ordering::Relaxed);
            }
        }))?;
        Ok(Watched(Some(transaction)))
    }
}

/// The SQL transaction with the journal watch installed; ending it either way removes the watch.
pub(super) struct Watched<'connection>(Option<Transaction<'connection>>);

impl Watched<'_> {
    fn unwatch(transaction: &Transaction<'_>) {
        let _ = transaction.update_hook(None::<fn(rusqlite::hooks::Action, &str, &str, i64)>);
    }
    pub(super) fn commit(mut self) -> Result<()> {
        let transaction = self.0.take().expect("a watched transaction ends once");
        Self::unwatch(&transaction);
        Ok(transaction.commit()?)
    }
}
impl<'connection> Deref for Watched<'connection> {
    type Target = Transaction<'connection>;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("a watched transaction ends once")
    }
}
impl DerefMut for Watched<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().expect("a watched transaction ends once")
    }
}
impl Drop for Watched<'_> {
    fn drop(&mut self) {
        if let Some(transaction) = &self.0 {
            Self::unwatch(transaction);
        }
    }
}
