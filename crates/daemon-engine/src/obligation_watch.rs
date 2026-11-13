//! The one clock over what every live task is owed. It measures how long each step lasts, logs
//! the steps past their deadline, and acts on those whose deadline `obligation_policy` enforces:
//! an input handed to a process that then wrote nothing for too long is given to a host that
//! resumes the same session, flagged as possibly received already, a node
//! whose work ended with no turn to take up its notice is told, and what the orchestrator owes is
//! reminded to it and then settled by Alondra. Nothing it does opens a card. The daemon keeps one
//! clock, in `Live`: its own loop and every pass of the orchestrator step that one.
use crate::domain_runtime::DomainRuntime;
use crate::locks::Poisonable as _;
use crate::sdk_owner::untaken;
use alondra_store::AuthorityScope;
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
    /// The usage limit holds the orchestrator: what it owes does not age meanwhile.
    fn orchestrator_held(&self) -> bool;
    /// When the task's node process last wrote anything, or started if it wrote nothing yet.
    /// `None` when it has no live process.
    fn last_output(&self, task: &TaskId) -> Option<Millis>;
}

struct Seen {
    party: Party,
    owed: Owed,
    since: Revision,
    first_seen: Millis,
    /// Where the deadline counts from: the first sight, or the last time the step was acted on.
    clock: Millis,
    reported: bool,
}

/// What Alondra does with a step past its deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// The hung process is stopped: a host that resumes the same session takes its place, and the
    /// input goes again flagged as possibly received, once the gate gives the task a slot.
    Requeue,
    /// The node is told its work ended.
    Wake,
    /// The orchestrator is reminded of what it owes the task, and the next time Alondra settles it.
    Answer,
}

/// The action for a step past its deadline.
pub fn action(owed: &Owed) -> Option<Action> {
    match owed {
        Owed::HandInput => Some(Action::Requeue),
        Owed::Wake => Some(Action::Wake),
        Owed::Answer => Some(Action::Answer),
        _ => None,
    }
}

#[derive(Default)]
struct Finished {
    ages: Vec<Millis>,
    overdue: usize,
}

pub struct Watch {
    seen: BTreeMap<TaskId, Seen>,
    finished: BTreeMap<String, Finished>,
    summarized_at: Option<Millis>,
    last_pass: Option<Millis>,
    enforced: fn(&Owed) -> bool,
}

impl Default for Watch {
    fn default() -> Self {
        Self::enforcing(obligation_policy::enforced)
    }
}

fn effective(world: &dyn World, task: &TaskId, obligation: &Obligation) -> (Party, Owed) {
    match obligation.owed {
        Owed::HandInput | Owed::HandDecision if world.waits_for_slot(task) => (Party::Capacity(Capacity::Gate), Owed::Room(Capacity::Gate)),
        owed => (obligation.party, owed),
    }
}

impl Watch {
    pub fn enforcing(enforced: fn(&Owed) -> bool) -> Self {
        Self { seen: BTreeMap::new(), finished: BTreeMap::new(), summarized_at: None, last_pass: None, enforced }
    }

    pub fn step(&mut self, state: &DomainState, world: &dyn World, now: Millis) -> Vec<Overdue> {
        let obligations = state
            .tasks
            .iter()
            .filter(|(_, task)| !task.over())
            .filter_map(|(id, _)| Some((id.clone(), crate::orchestrator_inbox::obligation_of(state, id)?)))
            .collect();
        self.observe(obligations, world, now)
    }

    pub fn observe(&mut self, obligations: Vec<(TaskId, Obligation)>, world: &dyn World, now: Millis) -> Vec<Overdue> {
        let mut overdue = Vec::new();
        let held = if world.orchestrator_held() { now - self.last_pass.unwrap_or(now) } else { 0 };
        self.last_pass = Some(now);
        for seen in self.seen.values_mut().filter(|seen| seen.owed == Owed::Answer) {
            seen.clock += held;
        }
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
                self.seen.insert(task.clone(), Seen { party, owed, since: obligation.since, first_seen: now, clock: now, reported: false });
            }
            let Some(seen) = self.seen.get_mut(task) else { continue };
            let quiet_since = if owed == Owed::HandInput { world.last_output(task).map(|output| output.max(seen.clock)) } else { Some(seen.clock) };
            let age = quiet_since.map_or(0, |since| now - since);
            let late = party == Party::Alondra && quiet_since.is_some() && obligation_policy::deadline(&owed).is_some_and(|deadline| age >= deadline);
            if late && !seen.reported {
                seen.reported = true;
                self.finished.entry(format!("{owed:?}")).or_default().overdue += 1;
                overdue.push(Overdue { task: task.clone(), party, owed, age });
            }
        }
        overdue
    }

    /// What to do now with each step in `late` whose deadline is enforced. Each one acted on
    /// starts its deadline again.
    pub fn enforce(&mut self, late: &[Overdue], now: Millis) -> Vec<(TaskId, Action)> {
        let mut actions = Vec::new();
        for overdue in late.iter().filter(|overdue| (self.enforced)(&overdue.owed)) {
            let Some(seen) = self.seen.get_mut(&overdue.task) else { continue };
            seen.clock = now;
            seen.reported = false;
            if let Some(action) = action(&overdue.owed) {
                actions.push((overdue.task.clone(), action));
            }
        }
        actions
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

struct GateWorld<'a> {
    domain: &'a DomainRuntime,
    state: &'a DomainState,
}

impl World for GateWorld<'_> {
    fn waits_for_slot(&self, task: &TaskId) -> bool {
        self.domain.live.gate.waits_for_slot(self.domain, self.state, task)
    }

    fn orchestrator_held(&self) -> bool {
        self.domain.live.usage_pause.is_active()
    }

    fn last_output(&self, task: &TaskId) -> Option<Millis> {
        let agent = self.state.tasks.get(task)?.agent_id.clone()?;
        self.domain.owns_sdk_host(&agent).then(|| self.domain.live.outputs.lock_or_recover("the node outputs").get(task).copied().unwrap_or_default())
    }
}

/// One pass of the clock at `now`: what is past its deadline is logged and, where enforced, acted on.
pub fn pass(domain: &DomainRuntime, watch: &mut Watch, now: Millis) {
    let Ok(state) = domain.state() else { return };
    let late = watch.step(&state, &GateWorld { domain, state: &state }, now);
    for overdue in &late {
        log!("{} has waited {} s for {:?} owed by {:?}: past its deadline", overdue.task, overdue.age / 1000, overdue.owed, overdue.party);
    }
    let mut answers = std::collections::BTreeSet::new();
    for (task, action) in watch.enforce(&late, now) {
        if action == Action::Answer {
            answers.insert(task);
        } else if let Err(error) = apply(domain, &task, action) {
            log!("{task} is past its deadline and Alondra could not act on it ({action:?}): {error}");
        }
    }
    if !answers.is_empty() {
        crate::orchestrator_inbox::settle_answers(domain, &state, &answers);
    }
    for line in watch.summary(now) {
        log!("{line}");
    }
}

/// Does `action` for `task`. Requeueing is the owner's to do, on its next step.
pub fn apply(domain: &DomainRuntime, task: &TaskId, action: Action) -> Result<(), String> {
    let state = domain.state().map_err(|error| format!("{error:#}"))?;
    let scope = AuthorityScope::new(state.data_epoch.clone()).map_err(|error| format!("{error:?}"))?;
    let agent = state.tasks.get(task).and_then(|task| task.agent_id.clone());
    match action {
        Action::Wake => {
            let Some(agent) = agent else { return Ok(()) };
            let (told, _) = domain.own(scope, |owner| untaken::wake(owner, task, &agent)).map_err(|error| error.to_string())?;
            if told {
                log!("the work of {task}'s own ended and no turn took up its notice: Alondra tells its node");
            }
        }
        Action::Requeue => {
            if agent.is_some_and(|agent| domain.owns_sdk_host(&agent)) {
                domain.requeues.lock_or_recover("the requeued tasks").insert(task.clone());
                domain.seals.wake();
            }
        }
        Action::Answer => crate::orchestrator_inbox::settle_answers(domain, &state, &[task.clone()].into()),
    }
    Ok(())
}

pub async fn run(domain: Arc<DomainRuntime>) {
    loop {
        let domain = Arc::clone(&domain);
        let passed = tokio::task::spawn_blocking(move || pass(&domain, &mut domain.live.obligations.lock_or_recover("the obligation watch"), now_millis())).await;
        if let Err(error) = passed {
            log!("a pass of the obligation watch stopped: {error}");
        }
        tokio::time::sleep(PASS).await;
    }
}

#[cfg(test)]
mod tests;
