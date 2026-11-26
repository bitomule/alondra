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
use alondra_engine_kernel::sdk_backend::SdkOf as _;
use crate::sdk_owner::SdkOwners as _;
use alondra_core::clock;
use alondra_engine_ports::Runtime;
use alondra_domain_model::{DomainState, TaskId};
use alondra_core::{Millis, now_millis};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

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
pub struct Rests {
    resting: BTreeMap<TaskId, Rest>,
}

impl Rests {
    /// Keeps only the tasks of `resting`, each as `(task, lifecycle revision)`, and returns the ones
    /// whose rest reached `wait` at `now` and when to look again: a due task still here then could
    /// not close, and is asked once more. A rest seen for the first time began at `since`, the
    /// change that announced it, however late the look comes.
    fn due(&mut self, resting: Vec<(TaskId, u64)>, since: Millis, now: Millis, timing: &Timing) -> (Vec<TaskId>, Option<Millis>) {
        let mut kept = BTreeMap::new();
        let (mut due, mut next) = (vec![], None::<Millis>);
        for (task, revision) in resting {
            let rest = self.resting.get(&task).filter(|rest| rest.revision == revision).copied().unwrap_or(Rest { revision, since: since.min(now) });
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
fn resting(domain: &dyn Runtime, state: &DomainState) -> Vec<(TaskId, u64)> {
    state.tasks.values().filter(|task| !task.over()).filter_map(|task| {
        let agent = task.agent_id.as_ref()?;
        (domain.owns_sdk_host(agent) && state.host_releasable(&task.id)).then(|| state.lifecycle_of(&task.id).ok().map(|lifecycle| (task.id.clone(), lifecycle.revision())))?
    }).collect()
}

/// Asks to close what rested long enough, and says when to look again. A closed node with work
/// comes back through the gate: its turn waits for a slot (`Admission::may_take_turn`), and the gate
/// takes the task over when it gives one. `since` is when the changes this look reads began.
pub fn pass(domain: &dyn Runtime, rests: &mut Rests, since: Millis, now: Millis, timing: &Timing) -> Option<Millis> {
    let read = domain.read();
    let state = read.state();
    let (due, next) = rests.due(resting(domain, state), since, now, timing);
    for agent in due.iter().filter_map(|task| state.tasks.get(task).and_then(|task| task.agent_id.as_ref())) {
        domain.ask_to_release_host(agent);
    }
    next
}

/// A task's lifecycle changes many times a second while a node works, and a look walks every open
/// task: what `TellNode` brings resumes without waiting for a look.
const LOOK_SPACING: Duration = Duration::from_secs(1);

pub async fn run(domain: Arc<dyn Runtime>, timing: &'static Timing) {
    if domain.sdk().is_none() {
        return;
    }
    let mut rests = Rests::default();
    looks(domain.subscribe(), |since, now| pass(&*domain, &mut rests, since, now, timing)).await;
}

/// Calls `look` with when the first change it reads came and the time now, and again when it asks
/// or a change comes. A host closes when its rest is due, at the instant `look` names: a look a
/// change brings is spaced from the last one, a due rest is looked at on time, so when a host
/// closes depends on when its task changed, never on how its changes were notified.
async fn looks<T>(mut changes: watch::Receiver<T>, mut look: impl FnMut(Millis, Millis) -> Option<Millis>) {
    // When the first change the next look reads came: a rest it finds began then, whether the
    // change was committed just before a look, just after it, or while the look was spaced.
    let mut changed = None;
    loop {
        let now = now_millis();
        let next = look(changed.take().unwrap_or(now), now);
        let woken_by_change = match next {
            Some(at) => tokio::select! {
                result = changes.changed() => result.is_ok(),
                _ = clock::sleep(Duration::from_millis(u64::try_from(at - now).unwrap_or(0))) => false,
            },
            None => changes.changed().await.is_ok(),
        };
        if !woken_by_change {
            if changes.has_changed().is_err() {
                return;
            }
            continue;
        }
        changed = Some(now_millis());
        let spaced = clock::sleep(LOOK_SPACING);
        tokio::pin!(spaced);
        loop {
            tokio::select! {
                () = &mut spaced => break,
                result = changes.changed() => if result.is_err() { return },
            }
        }
    }
}

#[cfg(test)]
#[cfg(not(feature = "whole-engine"))]
mod tests {
    use super::*;

    const TIMING: Timing = Timing { wait: 100 };

    fn task(name: &str) -> TaskId {
        name.into()
    }

    #[test]
    fn a_rest_closes_once_it_lasted_the_wait_and_says_when_to_look() {
        let mut rests = Rests::default();
        assert_eq!(rests.due(vec![(task("t-1"), 7)], 1_000, 1_000, &TIMING), (vec![], Some(1_100)));
        assert_eq!(rests.due(vec![(task("t-1"), 7)], 1_099, 1_099, &TIMING), (vec![], Some(1_100)));
        assert_eq!(rests.due(vec![(task("t-1"), 7)], 1_100, 1_100, &TIMING), (vec![task("t-1")], Some(1_200)));
    }

    #[test]
    fn any_change_to_the_lifecycle_starts_the_rest_again() {
        let mut rests = Rests::default();
        rests.due(vec![(task("t-1"), 7)], 1_000, 1_000, &TIMING);
        assert_eq!(rests.due(vec![(task("t-1"), 8)], 1_100, 1_100, &TIMING), (vec![], Some(1_200)));
    }

    #[test]
    fn a_task_that_stopped_resting_forgets_its_rest() {
        let mut rests = Rests::default();
        rests.due(vec![(task("t-1"), 7)], 1_000, 1_000, &TIMING);
        rests.due(vec![], 1_050, 1_050, &TIMING);
        assert_eq!(rests.due(vec![(task("t-1"), 7)], 1_100, 1_100, &TIMING), (vec![], Some(1_200)));
    }

    /// The look that finds a rest may come after the change that began it: the rest counts from
    /// that change, so when the host closes does not depend on which of the two came first.
    #[test]
    fn a_rest_a_later_look_finds_counts_from_the_change_that_began_it() {
        let mut rests = Rests::default();
        assert_eq!(rests.due(vec![(task("t-1"), 7)], 1_000, 1_040, &TIMING), (vec![], Some(1_100)));
        assert_eq!(rests.due(vec![(task("t-1"), 7)], 1_060, 1_100, &TIMING), (vec![task("t-1")], Some(1_200)));
    }

    #[test]
    fn each_rest_is_counted_on_its_own() {
        let mut rests = Rests::default();
        rests.due(vec![(task("t-1"), 1)], 1_000, 1_000, &TIMING);
        assert_eq!(rests.due(vec![(task("t-1"), 1), (task("t-2"), 1)], 1_100, 1_100, &TIMING), (vec![task("t-1")], Some(1_200)));
    }

    /// The instants a rest that begins at second 2 closes at, with the daemon also notifying a
    /// change of another task at each second of `other_changes`.
    fn closes(other_changes: &[u64]) -> Vec<Millis> {
        let runtime = clock::VirtualRuntime::new(0).unwrap();
        runtime.block_on(async {
            let (notify, changes) = watch::channel(());
            let resting = Arc::new(std::sync::Mutex::new(vec![]));
            let closed = Arc::new(std::sync::Mutex::new(vec![]));
            let (seen, released) = (resting.clone(), closed.clone());
            let mut rests = Rests::default();
            let looking = tokio::spawn(looks(changes, move |since, now| {
                let (due, next) = rests.due(seen.lock().unwrap().clone(), since, now, &Timing::SANDBOX);
                if !due.is_empty() {
                    released.lock().unwrap().push(now);
                    seen.lock().unwrap().clear();
                }
                next
            }));
            for second in 0..10 {
                if second == 2 {
                    *resting.lock().unwrap() = vec![(task("t-1"), 1)];
                    notify.send_replace(());
                }
                if other_changes.contains(&second) {
                    notify.send_replace(());
                }
                for _ in 0..16 {
                    tokio::task::yield_now().await;
                }
                clock::advance(Duration::from_secs(1)).await;
            }
            drop(notify);
            looking.await.unwrap();
            closed.lock().unwrap().clone()
        })
    }

    /// 23 November: on a loaded Mac a scenario's host closed a second earlier than on a calm one.
    /// The daemon's changes reached the look in fewer notices, and a notice the look took while a
    /// rest ran made it look at the second the rest was due, where a look woken by its timer waited
    /// one more spacing. A rest closes the host when it is due, however the changes came.
    #[test]
    fn a_rest_closes_when_it_is_due_however_the_changes_were_notified() {
        assert_eq!(closes(&[]), vec![5_000]);
        assert_eq!(closes(&[3]), vec![5_000]);
        assert_eq!(closes(&[4]), vec![5_000]);
    }
}
