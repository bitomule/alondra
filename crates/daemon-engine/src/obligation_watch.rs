//! The one clock over what every live task is owed. It measures how long each step lasts and logs
//! the steps past their deadline; it changes nothing.
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::obligation_policy::{self, Millis};
use alondra_core::domain::{DomainState, TaskId};
use alondra_core::now_millis;
use alondra_domain::{Capacity, Obligation, Owed, Party};
use alondra_foundation::Revision;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

const PASS: Duration = Duration::from_secs(15);
const SUMMARY_EVERY: Millis = 60 * 60_000;

#[derive(Debug, PartialEq, Eq)]
pub struct Overdue {
    pub task: TaskId,
    pub party: Party,
    pub owed: Owed,
    pub age: Millis,
}

/// What the daemon knows that the task's lifecycle does not.
pub trait World {
    fn waits_for_slot(&self, task: &TaskId) -> bool;
}

struct Seen {
    party: Party,
    owed: Owed,
    since: Revision,
    first_seen: Millis,
    reported: bool,
}

#[derive(Default)]
struct Finished {
    ages: Vec<Millis>,
    overdue: usize,
}

#[derive(Default)]
pub struct Watch {
    seen: BTreeMap<TaskId, Seen>,
    finished: BTreeMap<String, Finished>,
    summarized_at: Option<Millis>,
}

fn effective(world: &dyn World, task: &TaskId, obligation: &Obligation) -> (Party, Owed) {
    match obligation.owed {
        Owed::HandInput | Owed::HandDecision if world.waits_for_slot(task) => (Party::Capacity(Capacity::Gate), Owed::Room(Capacity::Gate)),
        owed => (obligation.party, owed),
    }
}

impl Watch {
    pub fn step(&mut self, state: &DomainState, world: &dyn World, now: Millis) -> Vec<Overdue> {
        let obligations = state
            .tasks
            .iter()
            .filter(|(_, task)| !task.over())
            .filter_map(|(id, _)| Some((id.clone(), state.lifecycle_of(id).ok()?.obligation()?)))
            .collect();
        self.observe(obligations, world, now)
    }

    pub fn observe(&mut self, obligations: Vec<(TaskId, Obligation)>, world: &dyn World, now: Millis) -> Vec<Overdue> {
        let mut overdue = Vec::new();
        let live: std::collections::BTreeSet<&TaskId> = obligations.iter().map(|(task, _)| task).collect();
        let gone: Vec<TaskId> = self.seen.keys().filter(|task| !live.contains(task)).cloned().collect();
        for task in gone {
            if let Some(seen) = self.seen.remove(&task) {
                self.finish(&seen, now);
            }
        }
        for (task, obligation) in &obligations {
            let (party, owed) = effective(world, task, obligation);
            let current = self.seen.get(task).is_some_and(|seen| seen.since == obligation.since && seen.owed == owed && seen.party == party);
            if !current {
                if let Some(previous) = self.seen.remove(task) {
                    self.finish(&previous, now);
                }
                self.seen.insert(task.clone(), Seen { party, owed, since: obligation.since, first_seen: now, reported: false });
            }
            let Some(seen) = self.seen.get_mut(task) else { continue };
            let age = now - seen.first_seen;
            let late = party == Party::Alondra && obligation_policy::deadline(&owed).is_some_and(|deadline| age >= deadline);
            if late && !seen.reported {
                seen.reported = true;
                self.finished.entry(format!("{owed:?}")).or_default().overdue += 1;
                overdue.push(Overdue { task: task.clone(), party, owed, age });
            }
        }
        overdue
    }

    fn finish(&mut self, seen: &Seen, now: Millis) {
        if matches!(seen.party, Party::Alondra | Party::Provider) {
            self.finished.entry(format!("{:?}", seen.owed)).or_default().ages.push(now - seen.first_seen);
        }
    }

    /// One line per kind of step that ended since the last summary, once an hour.
    pub fn summary(&mut self, now: Millis) -> Vec<String> {
        let since = *self.summarized_at.get_or_insert(now);
        if now - since < SUMMARY_EVERY {
            return Vec::new();
        }
        self.summarized_at = Some(now);
        std::mem::take(&mut self.finished)
            .into_iter()
            .map(|(owed, mut finished)| {
                finished.ages.sort_unstable();
                let n = finished.ages.len();
                let seconds = |rank: f64| {
                    let index = ((rank * n as f64).ceil() as usize).saturating_sub(1).min(n.saturating_sub(1));
                    finished.ages.get(index).map_or(0, |age| age / 1000)
                };
                format!("obligations {owed}: n={n} p50={}s p99={}s max={}s overdue={}", seconds(0.5), seconds(0.99), seconds(1.0), finished.overdue)
            })
            .collect()
    }
}

struct StateWorld<'a>(&'a DomainState);

impl World for StateWorld<'_> {
    fn waits_for_slot(&self, task: &TaskId) -> bool {
        self.0.waits_for_turn(task)
    }
}

pub async fn run(domain: Arc<DomainRuntime>) {
    let mut watch = Watch::default();
    loop {
        if let Ok(state) = domain.state() {
            let now = now_millis();
            for late in watch.step(&state, &StateWorld(&state), now) {
                log!("{} has waited {} s for {:?} owed by {:?}: past its deadline", late.task, late.age / 1000, late.owed, late.party);
            }
            for line in watch.summary(now) {
                log!("{line}");
            }
        }
        tokio::time::sleep(PASS).await;
    }
}

#[cfg(test)]
mod tests;
