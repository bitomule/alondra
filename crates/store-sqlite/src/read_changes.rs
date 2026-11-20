//! Conservative boundaries for live reads on one stable, read-only SQLite connection.
use super::{ReadTransaction, Revision};
pub use alondra_engine_ports::journal::ReadCursor;
use crate::locks::Poisonable as _;
use anyhow::{Result, ensure};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadChange {
    Unchanged,
    /// Every changed commit is currently unclassified, including auxiliary writes. No setter
    /// annotation or domain cache hint proves which view components a commit affects.
    Reset,
}

#[derive(Clone, Debug)]
pub struct ReadDelta {
    /// None on a failed read: there is no boundary at which a cache may be retained.
    pub through: Option<ReadCursor>,
    pub changed: ReadChange,
}

/// A holder that panics leaves one complete identity, so it is recovered: one Arc assignment.
pub(super) struct Identity(Mutex<Arc<()>>);

impl Default for Identity {
    fn default() -> Self { Self(Mutex::new(Arc::new(()))) }
}

impl Identity {
    fn current(&self) -> Arc<()> {
        self.0.lock_or_recover("the store reader identity").clone()
    }

    pub(super) fn invalidate(&self) {
        // Retained cursors retain the old allocation; there is no generation counter to wrap.
        *self.0.lock_or_recover("the store reader identity") = Arc::new(());
    }
}

impl super::DomainStore {
    /// Pins and identifies the committed snapshot this read transaction sees. Only read-only
    /// connections qualify: a writer's uncommitted changes cannot become a read boundary.
    pub fn read_cursor(&self) -> Result<ReadCursor> {
        let cursor = (|| {
            ensure!(!self.conn.is_autocommit(), "a read boundary requires an active read transaction");
            ensure!(self.conn.is_readonly("main")?, "a read boundary requires a read-only connection");
            let mark = self.read_mark()?;
            Ok(ReadCursor::new(self.read_identity.current(), mark, self.conn.total_changes()))
        })();
        if cursor.is_err() { self.read_identity.invalidate(); }
        cursor
    }

    /// Compares boundaries from this reader; external, unknown and auxiliary commits all reset.
    /// The result describes this transaction's snapshot, not commits made while it stays pinned.
    /// Validate a cached view again from a fresh read transaction before retaining it.
    pub fn changes_since(&self, before: &ReadCursor) -> ReadDelta {
        match self.read_cursor() {
            Ok(through) => ReadDelta {
                changed: if &through == before { ReadChange::Unchanged } else { ReadChange::Reset },
                through: Some(through),
            },
            Err(_) => ReadDelta { through: None, changed: ReadChange::Reset },
        }
    }
}
