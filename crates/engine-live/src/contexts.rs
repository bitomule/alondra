//! What each SDK task's driver measured of its conversation: its size after each request, the
//! window its model has, and when the prompt cache of its last request expires. The live view shows
//! the size, and a session that would resume a large conversation whose cache expired starts anew
//! instead. The last request of each task is kept in Alondra's home, so a restarted daemon still
//! knows how large the conversation is and until when its cache holds; and the first answer of each
//! new host says what starting it wrote to the cache.
use crate::locks::AfterPanic as _;
use alondra_core::Millis;
use alondra_domain_model::TaskId;
use alondra_foundation::changes::{Changes, Cursor, Delta};
pub use alondra_provider_claude::ContextEvent;
use alondra_provider_claude::Cached;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

const FILE: &str = "context-readings.json";
/// How long the last request of a task is kept once a request of another task came after it.
const KEPT_MS: Millis = 7 * 24 * 60 * 60_000;

pub use alondra_engine_ports::sdk::{LastRequest, StartKind};

/// The first answer a new host got: what its request wrote to and read from the prompt cache, and
/// how long after the task's previous request it went out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionStart {
    pub kind: StartKind,
    pub requested_at: Millis,
    pub since_previous: Option<Millis>,
    pub tokens: u64,
    pub cached: Cached,
}

/// How much of its model's window the root conversation uses after its last request; the window
/// is unknown until Claude says it at the end of a turn.
pub use alondra_engine_ports::live_view::Used;

#[derive(Clone, Copy, Debug)]
struct Started {
    kind: StartKind,
    previous: Option<Millis>,
}

#[derive(Default)]
struct Measured {
    handed: bool,
    requested_at: Option<Millis>,
    cache: Option<Millis>,
    tokens: Option<u64>,
    model: Option<String>,
    windows: BTreeMap<String, u64>,
    started: Option<Started>,
}

#[derive(Default)]
struct Inner {
    measured: BTreeMap<TaskId, Measured>,
    last: BTreeMap<TaskId, LastRequest>,
}

/// What each SDK task's driver measured of its conversation.
#[derive(Default)]
pub struct Contexts {
    /// A holder that panics leaves it whole, so it is recovered: what is in memory is recomputed
    /// by the next answer or compaction, and the file is only ever replaced whole.
    measured: Mutex<Inner>,
    /// Counts the changes to what `used` reads, so a cached live view knows it is old.
    version: AtomicU64,
    changes: Changes<TaskId>,
    file: Option<PathBuf>,
}

impl Contexts {
    pub fn persisted_in(home: &Path) -> Self {
        let file = home.join(FILE);
        let last: BTreeMap<TaskId, LastRequest> = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        let last = last.into_iter().filter(|(_, last)| last.tokens > 0).collect();
        Self { measured: Mutex::new(Inner { last, ..Inner::default() }), file: Some(file), ..Self::default() }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.measured.lock().or_recover("the measured contexts", || {
            self.changes.reset();
            self.measured.clear_poison();
        })
    }

    fn save(&self, inner: &mut Inner) {
        let Some(file) = &self.file else { return };
        let newest = inner.last.values().map(|last| last.requested_at).max().unwrap_or(0);
        inner.last.retain(|_, last| last.requested_at.saturating_add(KEPT_MS) > newest);
        let written = serde_json::to_vec(&inner.last).map_err(std::io::Error::from).and_then(|bytes| alondra_foundation::save::atomic(file, &bytes));
        if let Err(error) = written {
            log!("could not keep the last request of each task in {}: {error}", file.display());
        }
    }

    /// A new host of `task` runs its conversation as `kind`: its first answer says what starting
    /// it cost. A host that does not resume the conversation before it holds a conversation of its
    /// own: until it answers, nothing is known of its size or its cache, and the reading of the one
    /// it replaced no longer says anything of the task's.
    pub fn launched(&self, task: &TaskId, kind: StartKind) {
        let mut guard = self.lock();
        let inner = &mut *guard;
        let previous = inner.last.get(task).map(|last| last.requested_at);
        let measured = inner.measured.entry(task.clone()).or_default();
        measured.started = Some(Started { kind, previous });
        if kind == StartKind::Resumed {
            return;
        }
        let measured_size = measured.tokens.take().is_some();
        measured.cache = None;
        let kept = inner.last.remove(task).is_some();
        if kept {
            self.save(inner);
        }
        if measured_size || kept {
            self.version.fetch_add(1, Ordering::Relaxed);
            self.changes.record(task.clone());
        }
    }

    /// The driver handed the host an input at `at`.
    pub fn handed(&self, task: &TaskId, at: Millis) {
        let mut inner = self.lock();
        let measured = inner.measured.entry(task.clone()).or_default();
        measured.handed = true;
        measured.requested_at = Some(at);
        self.changes.record(task.clone());
    }

    /// The host said `event` at `at`. Only a conversation this driver handed an input to times its
    /// requests; any says its size. The first answer to a new host's first request says what
    /// starting it cost. An answer that used no tokens, as the one Claude writes itself for an
    /// error, a usage limit or a refusal, made no request and measures nothing.
    pub fn observed(&self, task: &TaskId, event: ContextEvent, at: Millis) -> Option<SessionStart> {
        if matches!(event, ContextEvent::Answered { tokens: 0, .. }) {
            return None;
        }
        let mut guard = self.lock();
        let inner = &mut *guard;
        let measured = inner.measured.entry(task.clone()).or_default();
        let mut start = None;
        let mut save = false;
        match event {
            ContextEvent::Requested if measured.handed => measured.requested_at = Some(at),
            ContextEvent::Requested => {}
            // A request that only read the cache refreshed it for as long as it lasts already.
            ContextEvent::Answered { tokens, cache, model, cached } => {
                measured.tokens = Some(tokens);
                measured.model = model.or(measured.model.take());
                let known = inner.last.get(task).map(|last| last.cache_ms);
                measured.cache = cache.map(|cache| Millis::try_from(cache.as_millis()).unwrap_or(Millis::MAX)).or(measured.cache).or(known);
                if let (Some(requested_at), Some(cache_ms)) = (measured.requested_at, measured.cache) {
                    if let Some(started) = measured.started.take() {
                        let since_previous = started.previous.map(|previous| requested_at.saturating_sub(previous));
                        start = Some(SessionStart { kind: started.kind, requested_at, since_previous, tokens, cached });
                    }
                    inner.last.insert(task.clone(), LastRequest { tokens, requested_at, cache_ms });
                    save = true;
                }
            }
            ContextEvent::Windows(windows) => measured.windows.extend(windows),
            ContextEvent::Compacted => {
                measured.tokens = None;
                save = inner.last.remove(task).is_some();
            }
        }
        if save {
            self.save(inner);
        }
        self.version.fetch_add(1, Ordering::Relaxed);
        self.changes.record(task.clone());
        start
    }

    /// The task's host is gone: what was measured in memory goes. Its last request stays known.
    pub fn forget(&self, task: &TaskId) {
        let mut inner = self.lock();
        if inner.measured.remove(task).is_some() { self.changes.record(task.clone()); }
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    /// A position in all measured data.
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
        let inner = self.lock();
        let measured = inner.measured.get(task)?;
        let window = measured.model.as_ref().and_then(|model| measured.windows.get(model)).copied();
        Some(Used { tokens: measured.tokens?, window })
    }

    /// The last request this daemon, or one before it, timed for `task`.
    pub fn last_request(&self, task: &TaskId) -> Option<LastRequest> {
        self.lock().last.get(task).copied()
    }
}

crate::extension!(Contexts, |runtime: &dyn alondra_engine_kernel::Runtime| Contexts::persisted_in(runtime.home()));

pub trait ContextsOf {
    fn contexts(&self) -> &Contexts;
}

impl<R: alondra_engine_ports::Runtime + ?Sized> ContextsOf for R {
    fn contexts(&self) -> &Contexts {
        alondra_engine_ports::extension(self.as_runtime())
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
    fn context_changes_cover_the_last_request_and_deletions_without_changing_legacy_versions() {
        let contexts = Contexts::default();
        let task = TaskId::from("task");
        let mut cursor = contexts.change_cursor();
        contexts.handed(&task, 10);
        cursor = changed(&contexts, &cursor, &["task"]);
        assert_eq!(contexts.used(&task), None);
        assert_eq!(contexts.version(), 0);
        contexts.observed(&task, ContextEvent::Answered {
            tokens: 20, cache: Some(Duration::from_millis(100)), model: Some("model".into()), cached: Cached::default(),
        }, 11);
        cursor = changed(&contexts, &cursor, &["task"]);
        assert_eq!(contexts.version(), 1);
        let used = contexts.used(&task);
        contexts.observed(&task, ContextEvent::Requested, 50);
        cursor = changed(&contexts, &cursor, &["task"]);
        contexts.observed(&task, ContextEvent::Answered { tokens: 20, cache: None, model: None, cached: Cached::default() }, 51);
        cursor = changed(&contexts, &cursor, &["task"]);
        assert_eq!(contexts.used(&task), used, "a new cache deadline need not change the displayed size");
        assert_eq!(contexts.last_request(&task), Some(LastRequest { tokens: 20, requested_at: 50, cache_ms: 100 }));
        assert_eq!(contexts.version(), 3);
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
        contexts.observed(&task, ContextEvent::Answered { tokens: 10, cache: None, model: None, cached: Cached::default() }, 1);
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
                let mut inner = contexts.measured.lock().unwrap();
                inner.measured.entry(task.clone()).or_default().tokens = Some(42);
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
        let reading = |contexts: &Contexts, task: &TaskId| contexts.last_request(task).map(|last| (last.tokens, last.requested_at.saturating_add(last.cache_ms)));
        let contexts = Contexts::default();
        let task = TaskId::from("t-1");
        let answered = ContextEvent::Answered { tokens: 250_000, cache: Some(Duration::from_secs(3600)), model: None, cached: Cached::default() };
        contexts.observed(&task, ContextEvent::Requested, 10);
        contexts.observed(&task, answered.clone(), 20);
        assert_eq!(reading(&contexts, &task), None, "an adopted host's events carry no time of their own");
        contexts.handed(&task, 100);
        contexts.observed(&task, answered, 900);
        contexts.observed(&task, ContextEvent::Requested, 1_000);
        contexts.observed(&task, ContextEvent::Answered { tokens: 260_000, cache: None, model: None, cached: Cached::default() }, 5_000);
        assert_eq!(reading(&contexts, &task), Some((260_000, 1_000 + HOUR)), "a read of the cache refreshes it from its request");
        contexts.observed(&task, ContextEvent::Compacted, 6_000);
        assert_eq!(reading(&contexts, &task), None);
    }

    #[test]
    fn the_context_used_is_the_last_answer_against_the_window_claude_gave_its_model() {
        let contexts = Contexts::default();
        let task = TaskId::from("t-1");
        assert_eq!(contexts.used(&task), None);
        let opus = |tokens| ContextEvent::Answered { tokens, cache: None, model: Some("claude-opus-5-5".into()), cached: Cached::default() };
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

    fn hour(tokens: u64, cached: Cached) -> ContextEvent {
        ContextEvent::Answered { tokens, cache: Some(Duration::from_secs(3600)), model: None, cached }
    }

    #[test]
    fn the_last_request_outlives_the_daemon_and_its_host() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*home).unwrap();
        let task = TaskId::from("t-1");
        let contexts = Contexts::persisted_in(&home);
        contexts.handed(&task, 1_000);
        contexts.observed(&task, hour(250_000, Cached::default()), 2_000);
        contexts.forget(&task);
        drop(contexts);
        let restarted = Contexts::persisted_in(&home);
        assert_eq!(restarted.last_request(&task), Some(LastRequest { tokens: 250_000, requested_at: 1_000, cache_ms: HOUR }));
        restarted.observed(&task, ContextEvent::Compacted, 3_000);
        assert_eq!(Contexts::persisted_in(&home).last_request(&task), None, "a compacted conversation is small again");
    }

    #[test]
    fn the_first_answer_of_a_new_host_says_what_starting_it_wrote_to_the_cache() {
        let contexts = Contexts::default();
        let task = TaskId::from("t-1");
        contexts.handed(&task, 1_000);
        assert_eq!(contexts.observed(&task, hour(250_000, Cached::default()), 2_000), None, "no host started");
        contexts.launched(&task, StartKind::Resumed);
        contexts.handed(&task, 1_000 + 4 * HOUR);
        let rewrite = Cached { written_1h: 249_000, written_5m: 0, read: 13_000 };
        assert_eq!(contexts.observed(&task, hour(262_000, rewrite), 1_001 + 4 * HOUR), Some(SessionStart {
            kind: StartKind::Resumed, requested_at: 1_000 + 4 * HOUR, since_previous: Some(4 * HOUR), tokens: 262_000, cached: rewrite,
        }));
        contexts.observed(&task, ContextEvent::Requested, 2_000 + 4 * HOUR);
        assert_eq!(contexts.observed(&task, hour(263_000, Cached::default()), 3_000 + 4 * HOUR), None, "only the first answer counts");
        contexts.launched(&"t-2".into(), StartKind::New);
        contexts.handed(&"t-2".into(), 5);
        let started = contexts.observed(&"t-2".into(), hour(20_000, Cached::default()), 6).unwrap();
        assert_eq!((started.kind, started.since_previous), (StartKind::New, None));
    }

    // 20 November: the last request before a usage limit failed, and Claude's answer for it, which
    // used no tokens, replaced the reading of a 250,000-token conversation with one of 0 tokens.
    // When the limit renewed hours later, the conversation looked small and resumed whole.
    #[test]
    fn an_answer_that_used_no_tokens_leaves_the_last_request_as_it_was() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*home).unwrap();
        let task = TaskId::from("t-1");
        let contexts = Contexts::persisted_in(&home);
        contexts.handed(&task, 1_000);
        contexts.observed(&task, hour(250_000, Cached::default()), 2_000);
        contexts.launched(&task, StartKind::Resumed);
        contexts.handed(&task, 1_000 + HOUR);
        let limited = ContextEvent::Answered { tokens: 0, cache: None, model: Some("<synthetic>".into()), cached: Cached::default() };
        assert_eq!(contexts.observed(&task, limited, 1_001 + HOUR), None);
        let kept = Some(LastRequest { tokens: 250_000, requested_at: 1_000, cache_ms: HOUR });
        assert_eq!(contexts.last_request(&task), kept);
        assert_eq!(Contexts::persisted_in(&home).last_request(&task).map(|last| (last.tokens, last.requested_at)), Some((250_000, 1_000)));
        assert_eq!(contexts.used(&task).map(|used| used.tokens), Some(250_000));
        let started = contexts.observed(&task, hour(251_000, Cached::default()), 2_000 + 5 * HOUR).expect("the first request that answered says what starting cost");
        assert_eq!(started.tokens, 251_000);
    }

    #[test]
    fn a_kept_reading_of_no_tokens_is_no_reading_and_an_older_daemons_compaction_mark_is_read_past() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*home).unwrap();
        let kept = serde_json::json!({
            "t-limited": {"tokens": 0, "requested_at": 1_000, "cache_ms": HOUR, "compactable": true},
            "t-measured": {"tokens": 250_000, "requested_at": 1_000, "cache_ms": HOUR, "compactable": false},
        });
        std::fs::write(home.join(FILE), kept.to_string()).unwrap();
        let contexts = Contexts::persisted_in(&home);
        assert_eq!(contexts.last_request(&"t-limited".into()), None);
        assert_eq!(contexts.last_request(&"t-measured".into()), Some(LastRequest { tokens: 250_000, requested_at: 1_000, cache_ms: HOUR }));
    }

    // 21 November: a large conversation whose cache had expired went on in a new session, and the
    // next input of that session, which had not answered yet, found the old conversation's reading
    // and started yet another new session. A host that does not resume the conversation before it
    // knows nothing of the size and cache of its own until it answers.
    #[test]
    fn a_host_that_starts_a_conversation_of_its_own_leaves_the_reading_of_the_one_before() {
        for kind in [StartKind::Handoff, StartKind::New] {
            let home = crate::test_support::Directory::new();
            std::fs::create_dir_all(&*home).unwrap();
            let task = TaskId::from("t-1");
            let contexts = Contexts::persisted_in(&home);
            contexts.handed(&task, 1_000);
            contexts.observed(&task, hour(250_000, Cached::default()), 2_000);
            contexts.launched(&task, StartKind::Resumed);
            assert_eq!(contexts.last_request(&task).map(|last| last.tokens), Some(250_000), "a resumed conversation is the same one");
            let cursor = contexts.change_cursor();
            contexts.launched(&task, kind);
            assert_eq!(contexts.last_request(&task), None, "{kind:?}");
            changed(&contexts, &cursor, &["t-1"]);
            assert_eq!(contexts.used(&task), None, "{kind:?}");
            assert_eq!(Contexts::persisted_in(&home).last_request(&task), None, "a restarted daemon does not find it either: {kind:?}");
            contexts.handed(&task, 4 * HOUR);
            let started = contexts.observed(&task, hour(4_000, Cached::default()), 4 * HOUR + 1).expect("the new conversation's first answer");
            assert_eq!((started.kind, started.since_previous), (kind, Some(4 * HOUR - 1_000)), "what starting it cost is still measured from the request before");
            assert_eq!(contexts.last_request(&task).map(|last| (last.tokens, last.requested_at)), Some((4_000, 4 * HOUR)));
        }
    }
}
