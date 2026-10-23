//! Each task's aggregate, replayed from its journal at most once per transaction while only the
//! store's own operations write the journal. A journal write outside them, a schema change or a
//! failed operation forgets every entry, so the next read replays the journal again. The inputs a
//! task's reads found stored are kept and forgotten the same way.
//!
//! An `AggregateCache` keeps the aggregates from one transaction to the next on the same
//! connection, so a command replays only what it adds. Events are never deleted nor changed
//! (`lifecycle_event_retained`, `lifecycle_event_immutable`), so a task whose committed revision
//! is the one an aggregate was built at holds the very events it was built from. The cache serves
//! an aggregate only under these rules:
//! - only a transaction that committed gives its aggregates to the cache. One that rolls back or
//!   fails gives back only the aggregates it inherited and never changed: what it applied or
//!   replayed may hold writes that no longer exist;
//! - a transaction inherits nothing when another connection committed (`data_version`) or the
//!   schema changed (`schema_version`) since the cache was last given its aggregates;
//! - an inherited aggregate is checked against the committed rows the first time a transaction
//!   reads it: epoch, initial input, revision, events, relations and pending inputs. Any mismatch,
//!   such as a write on this connection between two transactions, replays the task instead;
//! - the cache belongs to one connection: it starts empty with it and is never used with another.
//!
//! The inputs a transaction found stored are never inherited.
use crate::{Result, persistence::KnownInputs};
use alondra_domain::{InputId, TaskLifecycle};
use alondra_foundation::TaskId;
use rusqlite::{Connection, Transaction};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    ops::{Deref, DerefMut},
    sync::{
        Arc, Mutex, PoisonError, Weak,
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

/// The aggregates the last committed transaction on one connection left, kept for the next one.
#[derive(Default)]
pub struct AggregateCache(Mutex<Given>);

#[derive(Default)]
struct Given {
    entries: BTreeMap<TaskId, Entry>,
    version: Option<Version>,
}

impl AggregateCache {
    fn give(&self, entries: BTreeMap<TaskId, Entry>, version: Version) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Given { entries, version: Some(version) };
    }
    fn take(&self, version: Version) -> BTreeMap<TaskId, Entry> {
        let given = std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner));
        if given.version == Some(version) { given.entries } else { BTreeMap::new() }
    }
}

/// What other connections committed and the schema, as a transaction sees them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Version {
    data: i64,
    schema: i64,
}

impl Version {
    pub(super) fn read(connection: &Connection) -> Result<Self> {
        let schema = connection.query_row("PRAGMA schema_version", [], |row| row.get(0))?;
        let data = connection.query_row("PRAGMA data_version", [], |row| row.get(0))?;
        Ok(Self { data, schema })
    }
}

#[derive(Clone)]
pub(super) struct Entry {
    pub(super) aggregate: Arc<TaskLifecycle>,
    pub(super) initial: InputId,
}

struct Inherited<'cache> {
    cache: &'cache AggregateCache,
    version: Version,
    aggregates: BTreeMap<TaskId, (Weak<TaskLifecycle>, InputId)>,
    given: Cell<bool>,
}

#[derive(Default)]
pub(super) struct Aggregates<'cache> {
    entries: RefCell<BTreeMap<TaskId, Entry>>,
    unchecked: RefCell<BTreeSet<TaskId>>,
    inputs: RefCell<BTreeMap<TaskId, KnownInputs>>,
    journal_writes: Arc<AtomicU64>,
    mark: Cell<Option<Mark>>,
    inherited: Option<Inherited<'cache>>,
}

/// What the connection looked like when the store's last operation ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Mark {
    journal_writes: u64,
    schema: i64,
}

impl<'cache> Aggregates<'cache> {
    /// The aggregates the cache holds, when nothing committed elsewhere since it was given them.
    pub(super) fn inherit(cache: &'cache AggregateCache, connection: &Connection) -> Result<Self> {
        let version = Version::read(connection)?;
        let entries = cache.take(version);
        let aggregates = Self {
            unchecked: RefCell::new(entries.keys().cloned().collect()),
            inherited: Some(Inherited {
                cache,
                version,
                aggregates: entries
                    .iter()
                    .map(|(task, entry)| (task.clone(), (Arc::downgrade(&entry.aggregate), entry.initial.clone())))
                    .collect(),
                given: Cell::new(false),
            }),
            entries: RefCell::new(entries),
            inputs: RefCell::default(),
            journal_writes: Arc::default(),
            mark: Cell::new(None),
        };
        aggregates.mark.set(Some(aggregates.mark(connection)?));
        Ok(aggregates)
    }
    pub(super) fn version(&self, connection: &Connection) -> Result<Option<Version>> {
        self.inherited.as_ref().map(|_| Version::read(connection)).transpose()
    }
    /// Called once the transaction committed, with the version it read before committing.
    pub(super) fn give(&self, version: Option<Version>) {
        if let (Some(inherited), Some(version)) = (&self.inherited, version) {
            inherited.cache.give(self.entries.borrow().clone(), version);
            inherited.given.set(true);
        }
    }
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
        self.unchecked.borrow_mut().clear();
        self.inputs.borrow_mut().clear();
        self.mark.set(None);
    }
    pub(super) fn inputs<T>(&self, task: &TaskId, use_them: impl FnOnce(&mut KnownInputs) -> T) -> T {
        use_them(self.inputs.borrow_mut().entry(task.clone()).or_default())
    }
    pub(super) fn get(&self, task: &TaskId) -> Option<Entry> {
        self.entries.borrow().get(task).cloned()
    }
    /// Whether the aggregate came from an earlier transaction and this one has not checked it yet;
    /// asking marks it checked.
    pub(super) fn first_read_of_inherited(&self, task: &TaskId) -> bool {
        self.unchecked.borrow_mut().remove(task)
    }
    pub(super) fn take(&self, task: &TaskId) -> Option<Entry> {
        self.entries.borrow_mut().remove(task)
    }
    pub(super) fn keep(&self, aggregate: TaskLifecycle, initial: InputId) -> Arc<TaskLifecycle> {
        let aggregate = Arc::new(aggregate);
        self.entries
            .borrow_mut()
            .insert(aggregate.task().clone(), Entry { aggregate: aggregate.clone(), initial });
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

impl Drop for Aggregates<'_> {
    fn drop(&mut self) {
        let Some(inherited) = self.inherited.take() else { return };
        if inherited.given.get() {
            return;
        }
        let untouched = inherited
            .aggregates
            .into_iter()
            .filter_map(|(task, (aggregate, initial))| Some((task, Entry { aggregate: aggregate.upgrade()?, initial })))
            .collect();
        inherited.cache.give(untouched, inherited.version);
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
