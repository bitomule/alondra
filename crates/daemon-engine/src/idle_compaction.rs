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
use crate::sdk_owner::SdkOwners as _;
use crate::live::LiveOf as _;
pub use alondra_engine_live::contexts::{Contexts, ContextsOf, Reading, Used};
use alondra_core::clock;
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{Command, CommandEnvelope, DomainState, PROTOCOL_VERSION, TaskId};
use alondra_core::{Millis, now_millis};
use alondra_domain::{ActivityView, ResourceEvidence};
use std::sync::Arc;
use std::time::Duration;

pub(crate) const MIN_TOKENS: u64 = 200_000;

pub struct Timing {
    /// How long before the cache expires the compaction starts: enough for the session to take
    /// it and for its request to reach the model while the cache holds.
    pub lead: Millis,
}

impl Timing {
    pub const LIVE: Timing = Timing { lead: 2 * 60_000 };
    /// A sandbox whose model double reports a five-minute cache compacts seconds after the rest.
    pub const SANDBOX: Timing = Timing { lead: 4 * 60_000 + 50_000 };
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
    let Some(lifecycle) = state.lifecycle_of(task).ok() else {
        return false;
    };
    let paused = state.tasks.get(task).and_then(|task| task.agent_id.as_ref()).and_then(|agent| state.node_pause(agent)).is_some_and(|pause| !pause.released);
    lifecycle.activity() == ActivityView::Resting && lifecycle.resource() == Some(ResourceEvidence::Available) && !paused
}

/// Asks for the compaction of every task due at `now`, and says when to look again.
pub(crate) fn compact_due(domain: &DomainRuntime, now: Millis, timing: &Timing) -> Option<Millis> {
    if domain.live().usage_pause.is_active() {
        return Some(now + 60_000);
    }
    let read = domain.read();
    let state = read.state();
    let readings = domain.contexts().readings();
    let candidates: Vec<Candidate> = readings.iter().map(|(task, reading)| Candidate { task: task.clone(), reading: *reading, resting: resting(state, task) }).collect();
    let (tasks, next) = due(&candidates, now, timing);
    for task in tasks {
        let reading = readings.iter().find(|(read, _)| *read == task).map(|(_, reading)| *reading).expect("a due task has its reading");
        domain.contexts().spend(&task, reading);
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

pub async fn run(domain: Arc<DomainRuntime>, timing: &'static Timing) {
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
                _ = clock::sleep(Duration::from_millis(u64::try_from(at - now).unwrap_or(0))) => {}
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
}
