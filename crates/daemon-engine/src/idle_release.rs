//! Closes the host and native process of a node that rests with nothing in them, and resumes the
//! session when the task has something to do. A resting node spends memory for nothing: on an 18 GB
//! Mac, 33 of them held 12.8 GB, nearly all of it compressed or swapped.
//!
//! The prompt cache lives on the provider's servers, so closing the process costs no cache: a
//! session resumed within the hour read 98-99 % of its context from it, as one that stayed open
//! did. What the wait saves is the 1 to 4 seconds a resume takes on a node answered at once.
//!
//! The decision reads the task's lifecycle alone (`DomainState::host_releasable`). What is measured
//! lives in memory: a restarted daemon counts each rest from its start. A resume waits for a free
//! node slot like any turn.
use crate::sdk_owner::SdkOwners as _;
use alondra_core::clock;
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{DomainState, TaskId};
use alondra_core::{Millis, now_millis};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

pub struct Timing {
    /// How long a node rests before its host closes. Of 6 521 recorded rests, the 36 % answered
    /// within two minutes keep their host; the rest leave 94 % of all resting time without one.
    pub wait: Millis,
}

impl Timing {
    pub const LIVE: Timing = Timing { wait: 2 * 60_000 };
    pub const SANDBOX: Timing = Timing { wait: 3_000 };
}

/// One unchanged rest of a task: any change to its lifecycle starts it again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rest {
    revision: u64,
    since: Millis,
}

#[derive(Default)]
pub(crate) struct Rests {
    resting: BTreeMap<TaskId, Rest>,
}

impl Rests {
    /// Keeps only the tasks of `resting`, each as `(task, lifecycle revision)`, and returns the ones
    /// whose rest reached `wait` at `now` and when to look again: a due task still here then could
    /// not close, and is asked once more.
    fn due(&mut self, resting: Vec<(TaskId, u64)>, now: Millis, timing: &Timing) -> (Vec<TaskId>, Option<Millis>) {
        let mut kept = BTreeMap::new();
        let (mut due, mut next) = (vec![], None::<Millis>);
        for (task, revision) in resting {
            let rest = self.resting.get(&task).filter(|rest| rest.revision == revision).copied().unwrap_or(Rest { revision, since: now });
            let closes = rest.since + timing.wait;
            let look = if now >= closes {
                due.push(task.clone());
                now + timing.wait
            } else {
                closes
            };
            next = Some(next.map_or(look, |next| next.min(look)));
            kept.insert(task, rest);
        }
        self.resting = kept;
        (due, next)
    }
}

/// The tasks whose host this daemon owns and may close, with their lifecycle revision.
fn resting(domain: &DomainRuntime, state: &DomainState) -> Vec<(TaskId, u64)> {
    state.tasks.values().filter(|task| !task.over()).filter_map(|task| {
        let agent = task.agent_id.as_ref()?;
        (domain.owns_sdk_host(agent) && state.host_releasable(&task.id)).then(|| state.lifecycle_of(&task.id).ok().map(|lifecycle| (task.id.clone(), lifecycle.revision())))?
    }).collect()
}

/// Asks to close what rested long enough, and says when to look again. A closed node with work
/// comes back through the gate: its turn waits for a slot (`Admission::may_take_turn`), and the gate
/// takes the task over when it gives one.
pub(crate) fn pass(domain: &DomainRuntime, rests: &mut Rests, now: Millis, timing: &Timing) -> Option<Millis> {
    let read = domain.read();
    let state = read.state();
    let (due, next) = rests.due(resting(domain, state), now, timing);
    for agent in due.iter().filter_map(|task| state.tasks.get(task).and_then(|task| task.agent_id.as_ref())) {
        domain.ask_to_release_host(agent);
    }
    next
}

/// A task's lifecycle changes many times a second while a node works, and a look walks every open
/// task: what `TellNode` brings resumes without waiting for a look.
const LOOK_SPACING: Duration = Duration::from_secs(1);

pub async fn run(domain: Arc<DomainRuntime>, timing: &'static Timing) {
    if domain.sdk().is_none() {
        return;
    }
    let mut rests = Rests::default();
    let mut changes = domain.subscribe();
    loop {
        let now = now_millis();
        match pass(&domain, &mut rests, now, timing) {
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
        clock::sleep(LOOK_SPACING).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMING: Timing = Timing { wait: 100 };

    fn task(name: &str) -> TaskId {
        name.into()
    }

    #[test]
    fn a_rest_closes_once_it_lasted_the_wait_and_says_when_to_look() {
        let mut rests = Rests::default();
        assert_eq!(rests.due(vec![(task("t-1"), 7)], 1_000, &TIMING), (vec![], Some(1_100)));
        assert_eq!(rests.due(vec![(task("t-1"), 7)], 1_099, &TIMING), (vec![], Some(1_100)));
        assert_eq!(rests.due(vec![(task("t-1"), 7)], 1_100, &TIMING), (vec![task("t-1")], Some(1_200)));
    }

    #[test]
    fn any_change_to_the_lifecycle_starts_the_rest_again() {
        let mut rests = Rests::default();
        rests.due(vec![(task("t-1"), 7)], 1_000, &TIMING);
        assert_eq!(rests.due(vec![(task("t-1"), 8)], 1_100, &TIMING), (vec![], Some(1_200)));
    }

    #[test]
    fn a_task_that_stopped_resting_forgets_its_rest() {
        let mut rests = Rests::default();
        rests.due(vec![(task("t-1"), 7)], 1_000, &TIMING);
        rests.due(vec![], 1_050, &TIMING);
        assert_eq!(rests.due(vec![(task("t-1"), 7)], 1_100, &TIMING), (vec![], Some(1_200)));
    }

    #[test]
    fn each_rest_is_counted_on_its_own() {
        let mut rests = Rests::default();
        rests.due(vec![(task("t-1"), 1)], 1_000, &TIMING);
        assert_eq!(rests.due(vec![(task("t-1"), 1), (task("t-2"), 1)], 1_100, &TIMING), (vec![task("t-1")], Some(1_200)));
    }
}
