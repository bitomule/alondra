//! Bounded, non-consuming change histories for independent readers.
//!
//! Writers record while holding the source's data lock, before making a mutation visible.
//! A cursor identifies a boundary, not an atomic transaction across multiple sources.

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

const CAPACITY: usize = 4096;

#[cfg(test)]
thread_local! { static INSPECTED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[derive(Clone, Debug)]
pub struct Cursor {
    source: Arc<()>,
    generation: u64,
}

impl PartialEq for Cursor {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.source, &other.source) && self.generation == other.generation
    }
}
impl Eq for Cursor {}

#[derive(Debug, PartialEq, Eq)]
pub enum Changed<K> {
    Keys(BTreeSet<K>),
    Reset,
}

#[derive(Debug)]
pub struct Delta<K> {
    pub through: Cursor,
    pub changed: Changed<K>,
}

struct State<K> {
    cursor: Cursor,
    floor: u64,
    entries: VecDeque<(u64, K)>,
}

impl<K> Default for State<K> {
    fn default() -> Self {
        Self { cursor: Cursor { source: Arc::new(()), generation: 0 }, floor: 0, entries: VecDeque::new() }
    }
}

impl<K> State<K> {
    fn advance(&mut self) {
        if let Some(next) = self.cursor.generation.checked_add(1) {
            self.cursor.generation = next;
        } else {
            *self = Self::default();
        }
    }
}

pub struct Changes<K>(Mutex<State<K>>);

impl<K> Default for Changes<K> {
    fn default() -> Self { Self(Mutex::new(State::default())) }
}

impl<K: Clone + Ord> Changes<K> {
    fn state(&self) -> MutexGuard<'_, State<K>> {
        self.0.lock().unwrap_or_else(|poisoned| {
            let mut state = poisoned.into_inner();
            *state = State::default();
            self.0.clear_poison();
            state
        })
    }

    pub fn cursor(&self) -> Cursor { self.state().cursor.clone() }

    pub fn record(&self, key: K) {
        let mut state = self.state();
        state.advance();
        let generation = state.cursor.generation;
        state.entries.push_back((generation, key));
        if state.entries.len() > CAPACITY {
            state.floor = state.entries.pop_front().expect("history exceeds capacity").0;
        }
    }

    /// Invalidate every older cursor when the affected keys cannot be established.
    pub fn reset(&self) {
        let mut state = self.state();
        state.advance();
        state.entries.clear();
        state.floor = state.cursor.generation;
    }

    pub fn since(&self, before: &Cursor) -> Delta<K> {
        let state = self.state();
        let changed = if !Arc::ptr_eq(&state.cursor.source, &before.source)
            || before.generation < state.floor || before.generation > state.cursor.generation {
            Changed::Reset
        } else if before.generation == state.cursor.generation {
            Changed::Keys(BTreeSet::new())
        } else {
            Changed::Keys(state.entries.iter().rev().take_while(|(generation, _)| {
                #[cfg(test)]
                INSPECTED.with(|count| count.set(count.get() + 1));
                *generation > before.generation
            })
                .map(|(_, key)| key.clone()).collect())
        };
        Delta { through: state.cursor.clone(), changed }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_coalesce_without_consuming_another_readers_history() {
        let changes = Changes::default();
        let first = changes.cursor();
        assert_eq!(changes.since(&first).changed, Changed::Keys(BTreeSet::new()));
        changes.record("a");
        let second = changes.cursor();
        changes.record("b");
        changes.record("a");
        for _ in 0..2 {
            assert_eq!(changes.since(&first).changed, Changed::Keys(BTreeSet::from(["a", "b"])));
            assert_eq!(changes.since(&second).changed, Changed::Keys(BTreeSet::from(["a", "b"])));
        }
        let delta = changes.since(&first);
        assert_eq!(changes.since(&delta.through).changed, Changed::Keys(BTreeSet::new()));
    }

    #[test]
    fn changes_reject_gaps_but_keep_the_oldest_complete_boundary() {
        let changes = Changes::default();
        let stale = changes.cursor();
        changes.record(0);
        let retained = changes.cursor();
        for key in 1..=CAPACITY { changes.record(key); }
        assert_eq!(changes.state().entries.len(), CAPACITY);
        assert_eq!(changes.since(&stale).changed, Changed::Reset);
        assert_eq!(changes.since(&retained).changed, Changed::Keys((1..=CAPACITY).collect()));
    }

    #[test]
    fn changes_reset_and_new_instances_reject_old_cursors() {
        let changes = Changes::default();
        let old = changes.cursor();
        changes.record(1);
        changes.reset();
        let fresh = changes.cursor();
        changes.record(2);
        assert_eq!(changes.since(&old).changed, Changed::Reset);
        assert_eq!(changes.since(&fresh).changed, Changed::Keys(BTreeSet::from([2])));
        assert_eq!(Changes::<i32>::default().since(&fresh).changed, Changed::Reset);
    }

    #[test]
    fn changes_never_wrap_a_generation_into_an_old_cursor() {
        let changes = Changes::default();
        changes.state().cursor.generation = u64::MAX;
        let old = changes.cursor();
        changes.record(1);
        assert_eq!(changes.since(&old).changed, Changed::Reset);
        assert_ne!(old, changes.cursor());
    }

    #[test]
    fn changes_discard_poisoned_history() {
        let changes = Changes::<i32>::default();
        let before = changes.cursor();
        let result = std::panic::catch_unwind(|| {
            let _guard = changes.0.lock().unwrap();
            panic!("interrupted mutation");
        });
        assert!(result.is_err());
        assert_eq!(changes.since(&before).changed, Changed::Reset);
        changes.record(1);
        assert_eq!(changes.since(&changes.cursor()).changed, Changed::Keys(BTreeSet::new()));
    }

    #[test]
    fn changes_copy_no_keys_for_unchanged_reads_and_only_the_changed_suffix() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        #[derive(Debug)]
        struct Key(usize, Arc<AtomicUsize>);
        impl Clone for Key {
            fn clone(&self) -> Self {
                self.1.fetch_add(1, Ordering::Relaxed);
                Self(self.0, self.1.clone())
            }
        }
        impl PartialEq for Key { fn eq(&self, other: &Self) -> bool { self.0 == other.0 } }
        impl Eq for Key {}
        impl PartialOrd for Key {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
        }
        impl Ord for Key { fn cmp(&self, other: &Self) -> std::cmp::Ordering { self.0.cmp(&other.0) } }
        let copies = Arc::new(AtomicUsize::new(0));
        let changes = Changes::default();
        for key in 0..CAPACITY { changes.record(Key(key, copies.clone())); }
        let cursor = changes.cursor();
        INSPECTED.with(|count| count.set(0));
        for _ in 0..32 {
            assert!(matches!(changes.since(&cursor).changed, Changed::Keys(keys) if keys.is_empty()));
        }
        assert_eq!(copies.load(Ordering::Relaxed), 0);
        assert_eq!(INSPECTED.with(|count| count.get()), 0);
        changes.record(Key(1, copies.clone()));
        assert!(matches!(changes.since(&cursor).changed, Changed::Keys(keys) if keys.len() == 1));
        assert_eq!(copies.load(Ordering::Relaxed), 1);
        assert_eq!(INSPECTED.with(|count| count.get()), 2);
    }
}
