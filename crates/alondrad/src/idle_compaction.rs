//! Compacts the conversation of an SDK task that rests on a card shortly before the prompt cache of
//! its last request expires. Answered after that, Claude would write its whole context into the
//! cache again; compacted while the cache still holds, the next turn starts from a summary. Only a
//! conversation of at least 200 000 tokens is compacted (Claude Code's own `idleCompaction`
//! default), and at most once per rest.
//!
//! The cache lasts from the start of the request that wrote or read it
//! (platform.claude.com/docs/en/build-with-claude/prompt-caching). That start is when this daemon
//! handed the input to the host or saw a tool's result go back to Claude. A host this daemon did
//! not hand anything to says nothing: its events may have waited in its journal, so their time is
//! not the request's. What is measured lives in memory; a restarted daemon compacts again after
//! the task's next turn.
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{Command, CommandEnvelope, DomainState, NodePhase, PROTOCOL_VERSION, TaskId, replay};
use alondra_core::{Millis, now_millis};
use alondra_domain::{ActivityView, ResourceEvidence};
use alondra_provider_claude::ContextEvent;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) const MIN_TOKENS: u64 = 200_000;

pub(crate) struct Timing {
    /// How long before the cache expires the compaction starts: enough for the session to take
    /// it and for its request to reach the model while the cache holds.
    pub lead: Millis,
}

impl Timing {
    pub(crate) const LIVE: Timing = Timing { lead: 2 * 60_000 };
    /// A sandbox whose model double reports a five-minute cache compacts seconds after the rest.
    pub(crate) const SANDBOX: Timing = Timing { lead: 4 * 60_000 + 50_000 };
}

/// The conversation's size after its last request, and when that request's cache expires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Reading {
    pub tokens: u64,
    pub expires_at: Millis,
}

/// How much of its model's window the root conversation uses after its last request; the window
/// is unknown until Claude says it at the end of a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Used {
    pub tokens: u64,
    pub window: Option<u64>,
}

impl Used {
    pub(crate) fn percent(&self) -> Option<f64> {
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
pub(crate) struct Contexts {
    measured: Mutex<BTreeMap<TaskId, Measured>>,
    /// Counts the changes to what `used` reads, so a cached live view knows it is old.
    version: AtomicU64,
}

impl Contexts {
    /// The driver handed the host an input at `at`.
    pub(crate) fn handed(&self, task: &TaskId, at: Millis) {
        let mut contexts = self.measured.lock().expect("contexts poisoned");
        let measured = contexts.entry(task.clone()).or_default();
        measured.handed = true;
        measured.requested_at = Some(at);
    }

    /// The host said `event` at `at`. Only a conversation this driver handed an input to times its
    /// requests; any says its size.
    pub(crate) fn observed(&self, task: &TaskId, event: ContextEvent, at: Millis) {
        let mut contexts = self.measured.lock().expect("contexts poisoned");
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
    }

    pub(crate) fn forget(&self, task: &TaskId) {
        self.measured.lock().expect("contexts poisoned").remove(task);
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    pub(crate) fn used(&self, task: &TaskId) -> Option<Used> {
        let contexts = self.measured.lock().expect("contexts poisoned");
        let measured = contexts.get(task)?;
        let window = measured.model.as_ref().and_then(|model| measured.windows.get(model)).copied();
        Some(Used { tokens: measured.tokens?, window })
    }

    pub(crate) fn readings(&self) -> Vec<(TaskId, Reading)> {
        self.measured.lock().expect("contexts poisoned").iter().filter_map(|(task, measured)| Some((task.clone(), measured.reading?))).collect()
    }

    /// The compaction of `reading` was asked for: that rest compacts no more.
    fn spend(&self, task: &TaskId, reading: Reading) {
        if let Some(measured) = self.measured.lock().expect("contexts poisoned").get_mut(task)
            && measured.reading == Some(reading)
        {
            measured.reading = None;
        }
    }
}

/// A task with a reading, and whether it rests now, with its host there and its node not paused.
#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    pub task: TaskId,
    pub reading: Reading,
    pub resting: bool,
}

/// The tasks to compact at `now`, and when to look again. A reading whose window has not opened
/// yet sets the next look; one whose cache already expired is past saving.
pub(crate) fn due(candidates: &[Candidate], now: Millis, timing: &Timing) -> (Vec<TaskId>, Option<Millis>) {
    let mut due = vec![];
    let mut next: Option<Millis> = None;
    for candidate in candidates.iter().filter(|candidate| candidate.reading.tokens >= MIN_TOKENS && now < candidate.reading.expires_at) {
        let opens = candidate.reading.expires_at - timing.lead;
        if now < opens {
            next = Some(next.map_or(opens, |next| next.min(opens)));
        } else if candidate.resting {
            due.push(candidate.task.clone());
        }
    }
    (due, next)
}

/// Whether `task` rests on a card with its host there and its node free to take an input.
pub(crate) fn resting(state: &DomainState, task: &TaskId) -> bool {
    let Some(lifecycle) = state.lifecycle_journal.get(task).and_then(|journal| replay(task, journal).ok()) else {
        return false;
    };
    let paused = state.tasks.get(task).and_then(|task| task.agent_id.as_ref()).and_then(|agent| state.node_phase(agent)).is_some_and(|phase| matches!(phase, NodePhase::Paused(pause) if !pause.released));
    lifecycle.activity() == ActivityView::Resting && lifecycle.resource() == Some(ResourceEvidence::Available) && !paused
}

/// Asks for the compaction of every task due at `now`, and says when to look again.
pub(crate) fn compact_due(domain: &DomainRuntime, now: Millis, timing: &Timing) -> Option<Millis> {
    if domain.live.usage_pause.is_active() {
        return Some(now + 60_000);
    }
    let read = domain.read();
    let state = read.state();
    let readings = domain.contexts.readings();
    let candidates: Vec<Candidate> = readings.iter().map(|(task, reading)| Candidate { task: task.clone(), reading: *reading, resting: resting(state, task) }).collect();
    let (tasks, next) = due(&candidates, now, timing);
    for task in tasks {
        let reading = readings.iter().find(|(read, _)| *read == task).map(|(_, reading)| *reading).expect("a due task has its reading");
        domain.contexts.spend(&task, reading);
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("idle-compaction-{task}-{}", reading.expires_at).as_str().into(),
            expected_revision: state.revision,
            command: Command::CompactConversation { task_id: task.clone() },
        };
        match domain.execute_as_system(envelope) {
            Ok(_) => log!("compacting {task} while it rests: {} tokens, its prompt cache expires in {} s", reading.tokens, (reading.expires_at - now) / 1000),
            Err(error) => log!("could not compact {task} while it rests: {error}"),
        }
    }
    next
}

pub(crate) async fn run(domain: Arc<DomainRuntime>, timing: &'static Timing) {
    if domain.sdk().is_none() {
        return;
    }
    let mut changes = domain.subscribe();
    loop {
        let now = now_millis();
        let next = compact_due(&domain, now, timing);
        match next {
            Some(at) => tokio::select! {
                _ = changes.changed() => {}
                _ = tokio::time::sleep(Duration::from_millis(u64::try_from(at - now).unwrap_or(0))) => {}
            },
            None => {
                if changes.changed().await.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Millis = 60 * 60_000;

    fn candidate(tokens: u64, expires_at: Millis, resting: bool) -> Candidate {
        Candidate { task: "t-1".into(), reading: Reading { tokens, expires_at }, resting }
    }

    #[test]
    fn a_large_resting_conversation_compacts_in_the_last_minutes_of_its_cache() {
        let large = [candidate(250_000, HOUR, true)];
        let opens = HOUR - Timing::LIVE.lead;
        assert_eq!(due(&large, 1_000, &Timing::LIVE), (vec![], Some(opens)), "too early: it looks again when the window opens");
        assert_eq!(due(&large, opens, &Timing::LIVE), (vec![TaskId::from("t-1")], None));
        assert_eq!(due(&large, HOUR - 1, &Timing::LIVE).0, vec![TaskId::from("t-1")]);
        assert_eq!(due(&large, HOUR, &Timing::LIVE), (vec![], None), "an expired cache is past saving");
    }

    #[test]
    fn a_small_or_busy_conversation_is_left_alone() {
        let opens = HOUR - Timing::LIVE.lead;
        assert_eq!(due(&[candidate(MIN_TOKENS - 1, HOUR, true)], opens, &Timing::LIVE), (vec![], None));
        assert_eq!(due(&[candidate(MIN_TOKENS, HOUR, true)], opens, &Timing::LIVE).0, vec![TaskId::from("t-1")]);
        assert_eq!(due(&[candidate(900_000, HOUR, false)], opens, &Timing::LIVE), (vec![], None), "working, paused or without its host");
    }

    #[test]
    fn the_earliest_window_sets_the_next_look() {
        let mut later = candidate(300_000, 2 * HOUR, true);
        later.task = "t-2".into();
        let (due, next) = due(&[later, candidate(300_000, HOUR, true)], 0, &Timing::SANDBOX);
        assert_eq!((due, next), (vec![], Some(HOUR - Timing::SANDBOX.lead)));
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
