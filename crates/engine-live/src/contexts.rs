//! What each SDK task's driver measured of its conversation: its size after each request, the
//! window its model has, and when the prompt cache of its last request expires. The idle compaction
//! reads it to compact a resting conversation in time; the live view shows the size.
use crate::domain_runtime::DomainRuntime;
use crate::locks::AfterPanic as _;
use alondra_core::Millis;
use alondra_domain_rules::TaskId;
use alondra_foundation::changes::{Changes, Cursor, Delta};
use alondra_provider_claude::ContextEvent;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// The conversation's size after its last request, and when that request's cache expires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    pub tokens: u64,
    pub expires_at: Millis,
}

/// How much of its model's window the root conversation uses after its last request; the window
/// is unknown until Claude says it at the end of a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Used {
    pub tokens: u64,
    pub window: Option<u64>,
}

impl Used {
    pub fn percent(&self) -> Option<f64> {
        self.window.map(|window| self.tokens as f64 * 100.0 / window as f64)
    }
}

#[derive(Default)]
struct Measured {
    handed: bool,
    requested_at: Option<Millis>,
    cache: Option<Millis>,
    reading: Option<Reading>,
    tokens: Option<u64>,
    model: Option<String>,
    windows: BTreeMap<String, u64>,
}

/// What each SDK task's driver measured of its conversation.
#[derive(Default)]
pub struct Contexts {
    /// A holder that panics leaves it whole, so it is recovered: readings in memory only,
    /// recomputed by the next answer or compaction.
    measured: Mutex<BTreeMap<TaskId, Measured>>,
    /// Counts the changes to what `used` reads, so a cached live view knows it is old.
    version: AtomicU64,
    changes: Changes<TaskId>,
}

impl Contexts {
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<TaskId, Measured>> {
        self.measured.lock().or_recover("the measured contexts", || {
            self.changes.reset();
            self.measured.clear_poison();
        })
    }

    /// The driver handed the host an input at `at`.
    pub fn handed(&self, task: &TaskId, at: Millis) {
        let mut contexts = self.lock();
        let measured = contexts.entry(task.clone()).or_default();
        measured.handed = true;
        measured.requested_at = Some(at);
        self.changes.record(task.clone());
    }

    /// The host said `event` at `at`. Only a conversation this driver handed an input to times its
    /// requests; any says its size.
    pub fn observed(&self, task: &TaskId, event: ContextEvent, at: Millis) {
        let mut contexts = self.lock();
        let measured = contexts.entry(task.clone()).or_default();
        match event {
            ContextEvent::Requested if measured.handed => measured.requested_at = Some(at),
            ContextEvent::Requested => {}
            // A request that only read the cache refreshed it for as long as it lasts already.
            ContextEvent::Answered { tokens, cache, model } => {
                measured.tokens = Some(tokens);
                measured.model = model.or(measured.model.take());
                measured.cache = cache.map(|cache| Millis::try_from(cache.as_millis()).unwrap_or(Millis::MAX)).or(measured.cache);
                measured.reading = measured.requested_at.zip(measured.cache).map(|(start, cache)| Reading { tokens, expires_at: start.saturating_add(cache) });
            }
            ContextEvent::Windows(windows) => measured.windows.extend(windows),
            ContextEvent::Compacted => {
                measured.reading = None;
                measured.tokens = None;
            }
        }
        self.version.fetch_add(1, Ordering::Relaxed);
        self.changes.record(task.clone());
    }

    /// A conversation of `tokens` on `model`, whose window is `window`: what the driver's answer
    /// and end of turn say, for tests that cannot link the adapter that names those events.
    #[cfg(feature = "test-hooks")]
    pub fn measured_for_test(&self, task: &TaskId, model: &str, tokens: u64, window: u64, at: Millis) {
        self.observed(task, ContextEvent::Answered { tokens, cache: None, model: Some(model.into()) }, at);
        self.observed(task, ContextEvent::Windows(vec![(model.into(), window)]), at + 1);
    }

    pub fn forget(&self, task: &TaskId) {
        let mut contexts = self.lock();
        if contexts.remove(task).is_some() { self.changes.record(task.clone()); }
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    /// A position in all measured data, including the compactor's readings.
    pub fn change_cursor(&self) -> Cursor {
        let _contexts = self.lock();
        self.changes.cursor()
    }

    pub fn changes_since(&self, cursor: &Cursor) -> Delta<TaskId> {
        let _contexts = self.lock();
        self.changes.since(cursor)
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    pub fn used(&self, task: &TaskId) -> Option<Used> {
        let contexts = self.lock();
        let measured = contexts.get(task)?;
        let window = measured.model.as_ref().and_then(|model| measured.windows.get(model)).copied();
        Some(Used { tokens: measured.tokens?, window })
    }

    pub fn readings(&self) -> Vec<(TaskId, Reading)> {
        self.lock().iter().filter_map(|(task, measured)| Some((task.clone(), measured.reading?))).collect()
    }

    /// The compaction of `reading` was asked for: that rest compacts no more.
    pub fn spend(&self, task: &TaskId, reading: Reading) {
        if let Some(measured) = self.lock().get_mut(task)
            && measured.reading == Some(reading)
        {
            measured.reading = None;
            self.changes.record(task.clone());
        }
    }
}

crate::extension!(Contexts);

pub trait ContextsOf {
    fn contexts(&self) -> &Contexts;
}

impl ContextsOf for DomainRuntime {
    fn contexts(&self) -> &Contexts {
        self.extension()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_foundation::changes::Changed;
    use std::time::Duration;

    const HOUR: Millis = 60 * 60_000;

    fn changed(contexts: &Contexts, from: &Cursor, expected: &[&str]) -> Cursor {
        let delta = contexts.changes_since(from);
        let Changed::Keys(keys) = delta.changed else { panic!("retained context changes must be incremental"); };
        assert_eq!(keys, expected.iter().map(|id| TaskId::from(*id)).collect());
        delta.through
    }

    #[test]
    fn context_changes_cover_compactor_data_and_deletions_without_changing_legacy_versions() {
        let contexts = Contexts::default();
        let task = TaskId::from("task");
        let mut cursor = contexts.change_cursor();
        contexts.handed(&task, 10);
        cursor = changed(&contexts, &cursor, &["task"]);
        assert_eq!(contexts.used(&task), None);
        assert_eq!(contexts.version(), 0);
        contexts.observed(&task, ContextEvent::Answered {
            tokens: 20, cache: Some(Duration::from_millis(100)), model: Some("model".into()),
        }, 11);
        cursor = changed(&contexts, &cursor, &["task"]);
        assert_eq!(contexts.version(), 1);
        let used = contexts.used(&task);
        contexts.observed(&task, ContextEvent::Requested, 50);
        cursor = changed(&contexts, &cursor, &["task"]);
        contexts.observed(&task, ContextEvent::Answered { tokens: 20, cache: None, model: None }, 51);
        cursor = changed(&contexts, &cursor, &["task"]);
        assert_eq!(contexts.used(&task), used, "a new cache deadline need not change the displayed size");
        let reading = contexts.readings()[0].1;
        assert_eq!(reading.expires_at, 150);
        contexts.spend(&task, Reading { tokens: 1, ..reading });
        cursor = changed(&contexts, &cursor, &[]);
        contexts.spend(&task, reading);
        cursor = changed(&contexts, &cursor, &["task"]);
        assert!(contexts.readings().is_empty());
        assert_eq!(contexts.used(&task), used);
        assert_eq!(contexts.version(), 3, "spending a compactor reading retains the legacy version contract");
        contexts.observed(&task, ContextEvent::Windows(vec![("model".into(), 200)]), 52);
        cursor = changed(&contexts, &cursor, &["task"]);
        assert_eq!(contexts.used(&task).unwrap().window, Some(200));
        contexts.observed(&task, ContextEvent::Compacted, 53);
        cursor = changed(&contexts, &cursor, &["task"]);
        assert_eq!(contexts.used(&task), None);
        contexts.forget(&task);
        cursor = changed(&contexts, &cursor, &["task"]);
        contexts.forget(&task);
        changed(&contexts, &cursor, &[]);
        assert_eq!(contexts.version(), 7, "even forgetting an absent task retains its legacy version increment");
    }

    #[test]
    fn context_readers_keep_independent_cursors_across_coalesced_changes() {
        let contexts = Contexts::default();
        let slow = contexts.change_cursor();
        contexts.handed(&"a".into(), 1);
        let fast = changed(&contexts, &slow, &["a"]);
        contexts.handed(&"b".into(), 2);
        contexts.handed(&"a".into(), 3);
        contexts.forget(&"b".into());
        let fast = changed(&contexts, &fast, &["a", "b"]);
        changed(&contexts, &slow, &["a", "b"]);
        changed(&contexts, &fast, &[]);
    }

    #[test]
    fn a_context_write_between_reading_and_validation_is_not_missed() {
        let contexts = Contexts::default();
        let task = TaskId::from("task");
        contexts.observed(&task, ContextEvent::Answered { tokens: 10, cache: None, model: None }, 1);
        let cursor = contexts.change_cursor();
        let selected = contexts.used(&task);
        std::thread::scope(|scope| {
            let (write, begin) = std::sync::mpsc::channel();
            let (done, written) = std::sync::mpsc::channel();
            let contexts = &contexts;
            let task = &task;
            let writer = scope.spawn(move || {
                begin.recv().unwrap();
                contexts.forget(task);
                done.send(()).unwrap();
            });
            write.send(()).unwrap();
            written.recv().unwrap();
            let through = changed(contexts, &cursor, &["task"]);
            assert!(selected.is_some());
            assert_eq!(contexts.used(task), None);
            changed(contexts, &through, &[]);
            writer.join().unwrap();
        });
    }

    #[test]
    fn recovering_context_data_after_a_panic_resets_every_older_cursor() {
        for recover_from_getter in [false, true] {
            let contexts = Contexts::default();
            let task = TaskId::from("task");
            let before = contexts.change_cursor();
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut measured = contexts.measured.lock().unwrap();
                measured.entry(task.clone()).or_default().tokens = Some(42);
                panic!("between changing context data and recording its key");
            }));
            assert!(panic.is_err());
            assert!(contexts.measured.is_poisoned());
            if recover_from_getter { assert_eq!(contexts.used(&task).unwrap().tokens, 42); }
            let after = contexts.change_cursor();
            assert!(!contexts.measured.is_poisoned());
            assert!(matches!(contexts.changes_since(&before).changed, Changed::Reset));
            changed(&contexts, &after, &[]);
        }
    }

    #[test]
    fn only_what_the_driver_handed_measures_and_a_compaction_ends_the_reading() {
        let contexts = Contexts::default();
        let task = TaskId::from("t-1");
        let answered = ContextEvent::Answered { tokens: 250_000, cache: Some(Duration::from_secs(3600)), model: None };
        contexts.observed(&task, ContextEvent::Requested, 10);
        contexts.observed(&task, answered.clone(), 20);
        assert!(contexts.readings().is_empty(), "an adopted host's events carry no time of their own");
        contexts.handed(&task, 100);
        contexts.observed(&task, answered, 900);
        contexts.observed(&task, ContextEvent::Requested, 1_000);
        contexts.observed(&task, ContextEvent::Answered { tokens: 260_000, cache: None, model: None }, 5_000);
        assert_eq!(contexts.readings(), vec![(task.clone(), Reading { tokens: 260_000, expires_at: 1_000 + HOUR })], "a read of the cache refreshes it from its request");
        contexts.observed(&task, ContextEvent::Compacted, 6_000);
        assert!(contexts.readings().is_empty());
    }

    #[test]
    fn the_context_used_is_the_last_answer_against_the_window_claude_gave_its_model() {
        let contexts = Contexts::default();
        let task = TaskId::from("t-1");
        assert_eq!(contexts.used(&task), None);
        let opus = |tokens| ContextEvent::Answered { tokens, cache: None, model: Some("claude-opus-5-5".into()) };
        contexts.observed(&task, opus(150_000), 1);
        assert_eq!(contexts.used(&task), Some(Used { tokens: 150_000, window: None }), "an adopted host still says its size");
        assert_eq!(contexts.used(&task).unwrap().percent(), None, "no window yet, no percent");
        contexts.observed(&task, ContextEvent::Windows(vec![("claude-haiku-4-5-20251001".into(), 200_000)]), 2);
        assert_eq!(contexts.used(&task).unwrap().window, None, "a subagent's model is not the conversation's");
        contexts.observed(&task, ContextEvent::Windows(vec![("claude-opus-5-5".into(), 1_000_000)]), 3);
        contexts.observed(&task, opus(250_000), 4);
        assert_eq!(contexts.used(&task), Some(Used { tokens: 250_000, window: Some(1_000_000) }));
        assert_eq!(contexts.used(&task).unwrap().percent(), Some(25.0));
        contexts.observed(&task, ContextEvent::Compacted, 5);
        assert_eq!(contexts.used(&task), None, "a compaction leaves the size unknown until the next answer");
    }

    #[test]
    fn a_reading_compacts_once() {
        let contexts = Contexts::default();
        let task = TaskId::from("t-1");
        contexts.handed(&task, 0);
        contexts.observed(&task, ContextEvent::Answered { tokens: 250_000, cache: Some(Duration::from_secs(3600)), model: None }, 1);
        let reading = contexts.readings()[0].1;
        contexts.spend(&task, Reading { tokens: 1, ..reading });
        assert_eq!(contexts.readings().len(), 1, "another reading is not spent");
        contexts.spend(&task, reading);
        assert!(contexts.readings().is_empty());
        contexts.forget(&task);
        contexts.observed(&task, ContextEvent::Answered { tokens: 250_000, cache: Some(Duration::from_secs(3600)), model: None }, 2);
        assert!(contexts.readings().is_empty(), "a forgotten task measures again only after its next handoff");
    }
}
