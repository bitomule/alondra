//! The journals of the domain's tasks, shared between the states of consecutive revisions: a
//! command copies none of them and only adds records to the ones it decides on.
use super::{LifecycleRecord, TaskId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::sync::Arc;

/// A task's lifecycle records, in order. It only grows: records are added at its end and never
/// changed or removed. Cloning it shares the records.
#[derive(Clone, Default)]
pub struct Journal(Arc<rpds::VectorSync<LifecycleRecord>>);

impl Journal {
    pub fn push(&mut self, record: LifecycleRecord) {
        Arc::make_mut(&mut self.0).push_back_mut(record);
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &LifecycleRecord> + ExactSizeIterator {
        self.0.iter()
    }
    pub fn first(&self) -> Option<&LifecycleRecord> {
        self.0.first()
    }
    pub fn last(&self) -> Option<&LifecycleRecord> {
        self.0.last()
    }
    pub fn get(&self, index: usize) -> Option<&LifecycleRecord> {
        self.0.get(index)
    }
    /// The records from `from` on.
    pub fn since(&self, from: usize) -> impl Iterator<Item = &LifecycleRecord> {
        self.0.iter().skip(from)
    }
    /// Whether both hold the very same records, not copies of them.
    pub fn shares_records_with(&self, other: &Journal) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn starts_with(&self, prefix: &Journal) -> bool {
        prefix.len() <= self.len() && (Arc::ptr_eq(&self.0, &prefix.0) || self.iter().zip(prefix.iter()).all(|(a, b)| a == b))
    }
    pub fn to_vec(&self) -> Vec<LifecycleRecord> {
        self.iter().cloned().collect()
    }
}

impl PartialEq for Journal {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
    }
}
impl Eq for Journal {}

impl PartialEq<Vec<LifecycleRecord>> for Journal {
    fn eq(&self, other: &Vec<LifecycleRecord>) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl PartialEq<Journal> for Vec<LifecycleRecord> {
    fn eq(&self, other: &Journal) -> bool {
        other == self
    }
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl std::ops::Index<usize> for Journal {
    type Output = LifecycleRecord;
    fn index(&self, index: usize) -> &LifecycleRecord {
        &self.0[index]
    }
}

impl FromIterator<LifecycleRecord> for Journal {
    fn from_iter<I: IntoIterator<Item = LifecycleRecord>>(records: I) -> Self {
        Journal(Arc::new(records.into_iter().collect()))
    }
}

impl From<Vec<LifecycleRecord>> for Journal {
    fn from(records: Vec<LifecycleRecord>) -> Self {
        records.into_iter().collect()
    }
}

impl Extend<LifecycleRecord> for Journal {
    fn extend<I: IntoIterator<Item = LifecycleRecord>>(&mut self, records: I) {
        let journal = Arc::make_mut(&mut self.0);
        for record in records {
            journal.push_back_mut(record);
        }
    }
}

impl<'a> IntoIterator for &'a Journal {
    type Item = &'a LifecycleRecord;
    type IntoIter = <&'a rpds::VectorSync<LifecycleRecord> as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl Serialize for Journal {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

impl<'de> Deserialize<'de> for Journal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<LifecycleRecord>::deserialize(deserializer).map(Journal::from)
    }
}

/// Every task's journal, by task.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Journals(BTreeMap<TaskId, Journal>);

impl Journals {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn get(&self, task: &TaskId) -> Option<&Journal> {
        self.0.get(task)
    }
    pub fn get_mut(&mut self, task: &TaskId) -> Option<&mut Journal> {
        self.0.get_mut(task)
    }
    pub fn insert(&mut self, task: TaskId, journal: impl Into<Journal>) -> Option<Journal> {
        self.0.insert(task, journal.into())
    }
    pub fn remove(&mut self, task: &TaskId) -> Option<Journal> {
        self.0.remove(task)
    }
    pub fn contains_key(&self, task: &TaskId) -> bool {
        self.0.contains_key(task)
    }
    pub fn keys(&self) -> std::collections::btree_map::Keys<'_, TaskId, Journal> {
        self.0.keys()
    }
    pub fn iter(&self) -> std::collections::btree_map::Iter<'_, TaskId, Journal> {
        self.0.iter()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// How many records `task`'s journal holds; none when it has no journal.
    pub fn records(&self, task: &TaskId) -> usize {
        self.0.get(task).map_or(0, Journal::len)
    }
    /// The tasks whose journal in `next` holds more records than here. Journals only grow, so
    /// that is every journal `next` changed.
    pub fn grown<'a>(&'a self, next: &'a Journals) -> impl Iterator<Item = (&'a TaskId, &'a Journal)> {
        next.iter().filter(|(task, journal)| journal.len() != self.records(task))
    }
}

impl std::fmt::Debug for Journals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::ops::Index<&TaskId> for Journals {
    type Output = Journal;
    fn index(&self, task: &TaskId) -> &Journal {
        &self.0[task]
    }
}

impl<'a> IntoIterator for &'a Journals {
    type Item = (&'a TaskId, &'a Journal);
    type IntoIter = std::collections::btree_map::Iter<'a, TaskId, Journal>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl IntoIterator for Journals {
    type Item = (TaskId, Journal);
    type IntoIter = std::collections::btree_map::IntoIter<TaskId, Journal>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl Extend<(TaskId, Journal)> for Journals {
    fn extend<I: IntoIterator<Item = (TaskId, Journal)>>(&mut self, journals: I) {
        self.0.extend(journals)
    }
}
