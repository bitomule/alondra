//! Every task ends at work, or in the user's hands, whatever the provider, the user, the hosts,
//! the daemon's restarts, the Mac and the clock did before the machine rested.
use super::world::{self, History, Host, MINUTE, history};
use super::*;
use alondra_core::domain::{AlondraStep, Delivery, Situation, WaitsFor};
use proptest::prelude::*;

/// A rule the code does not apply yet. While its row is listed, the model applies it itself; the
/// PR that makes the code apply it removes the row and drives the real code from the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Gap {
    /// Phase 7 wrote the deadline of an input the session does not take, but enforces it only
    /// after a day of measurement (`obligation_policy::enforced`): until then the model does.
    HandInput,
}

pub(super) const KNOWN_GAPS: &[Gap] = &[Gap::HandInput];

fn listed(gap: Gap) -> bool {
    KNOWN_GAPS.contains(&gap)
}

/// Whether the model enforces a deadline in place of the clock.
pub(super) fn gap_open(owed: Owed) -> bool {
    match owed {
        Owed::HandInput => listed(Gap::HandInput),
        _ => false,
    }
}

/// The open tasks whose node holds a slot: what the header and the Live Activity count.
pub(super) fn slot_tasks(state: &DomainState) -> BTreeSet<TaskId> {
    state.slots().into_iter().filter_map(|agent| state.node_task(&agent.id).filter(|task| !task.over()).map(|task| task.id.clone())).collect()
}

fn situation(index: &ReadIndex, task: &TaskId) -> Situation {
    index.task(task).expect("every task has a reading").situation
}

fn open_tasks(state: &DomainState) -> Vec<TaskId> {
    state.tasks.values().filter(|task| !task.state.terminal()).map(|task| task.id.clone()).collect()
}

/// The world has no room for another task: every slot taken, or the Mac or the usage limit holds starts.
fn room_is_scarce(machine: &Machine, state: &DomainState) -> bool {
    let world = &machine.world;
    slot_tasks(state).len() >= world.slots || world.memory_tight || world.disk_full || world.usage_holds()
}

/// The host has given the daemon everything it observed of `task`.
fn delivered(machine: &Machine, task: &TaskId) -> bool {
    machine.world.hosts.get(task).is_none_or(|host| host.outbox.is_empty())
        && machine.world.retired.iter().all(|(of, host)| of != task || host.outbox.is_empty())
}

fn runs(machine: &Machine, task: &TaskId) -> bool {
    machine.world.hosts.get(task).is_some_and(Host::runs)
}

/// A step Alondra owes `task` that started so recently that its deadlines have not all passed yet:
/// a world that never goes quiet (a node that asks again as soon as it is answered) ends the two
/// hours with one under way.
fn within_its_deadlines(machine: &Machine, state: &DomainState, task: &TaskId) -> bool {
    let Some(owed) = state.lifecycle_of(task).ok().and_then(|lifecycle| lifecycle.obligation()).map(|obligation| obligation.owed) else { return false };
    let bound = alondra_core::domain::obligation_policy::deadline(&owed).unwrap_or(MINUTE) * 2 + world::PASS;
    machine.owed_for(task) < bound
}

/// Where `task` may end once the machine rested: at work with the provider running it, with a card
/// of the user's, paused by them, delivered to them, or waiting for room the world really lacks.
fn ends_well(machine: &Machine, state: &DomainState, index: &ReadIndex, task: &TaskId) -> bool {
    let world = &machine.world;
    match situation(index, task) {
        Situation::Working(_) => runs(machine, task),
        Situation::NeedsYou | Situation::Paused | Situation::Delivered(Delivery::ForReview) => true,
        Situation::Waiting(WaitsFor::Slot) => room_is_scarce(machine, state),
        Situation::Waiting(WaitsFor::Step(AlondraStep::Delivering)) if state.waits_for_turn(task) || state.stranded_tasks().contains(task) => {
            room_is_scarce(machine, state)
        }
        Situation::Waiting(WaitsFor::Machine) => world.memory_tight,
        Situation::Waiting(WaitsFor::Disk) => world.disk_full,
        Situation::Waiting(WaitsFor::Usage) => world.usage_holds(),
        Situation::Waiting(WaitsFor::Step(AlondraStep::Answering) | WaitsFor::PlanReview) if world.usage_holds() => true,
        Situation::Waiting(WaitsFor::Step(_) | WaitsFor::PlanReview) if within_its_deadlines(machine, state, task) => true,
        _ => false,
    }
}

/// What one run of a history showed, one verdict per property: `None` when it holds.
#[derive(Default)]
struct Verdicts {
    ends: Option<String>,
    working: Option<String>,
    counts: Option<String>,
    unchanged: Option<String>,
    close: Option<String>,
    wakes: Option<String>,
    moves: Option<String>,
    clock_cards: Option<String>,
    answers: Option<String>,
    unlisted: Option<String>,
}

fn with_trace(machine: &Machine, why: String) -> String {
    let start = machine.trace.len().saturating_sub(80);
    format!("{why}\nlast commands:\n{}", machine.trace[start..].join("\n"))
}

fn working_without_a_run(machine: &Machine, state: &DomainState, index: &ReadIndex) -> Option<String> {
    open_tasks(state).into_iter().filter(|task| delivered(machine, task)).find_map(|task| match situation(index, &task) {
        Situation::Working(doing) if !runs(machine, &task) => Some(format!("{task} reads working ({doing:?}) with nothing running for it")),
        _ => None,
    })
}

/// The header and the Live Activity count the tasks that read working; the gate counts the tasks
/// whose node holds a slot, which are the slots the domain gives.
fn counts_differ(state: &DomainState, index: &ReadIndex) -> Option<String> {
    let header = alondra_core::views::activity_counts(index).working as usize;
    let working: BTreeSet<TaskId> = state.tasks.keys().filter(|task| matches!(situation(index, task), Situation::Working(_))).cloned().collect();
    if header != working.len() {
        return Some(format!("the header counts {header} working, the tasks that read working are {working:?}"));
    }
    let gate = crate::admission::working_tasks(state);
    let slots = slot_tasks(state);
    (gate != slots).then(|| format!("the gate counts {gate:?} at work, the nodes holding a slot are {slots:?}"))
}

fn unlisted_wait(state: &DomainState, index: &ReadIndex) -> Option<String> {
    open_tasks(state).into_iter().find_map(|task| {
        let card = index.task(&task).expect("a reading").unlisted.clone()?;
        Some(format!("{task} waits on card {card}, which the inbox does not list"))
    })
}

/// Judges every property that holds at each step: the first failure of each is kept.
fn each_step(machine: &Machine, verdicts: &mut Verdicts, when: &str) {
    let state = machine.state();
    let index = ReadIndex::build(&state);
    let keep = |verdict: &mut Option<String>, why: Option<String>| {
        if verdict.is_none() && let Some(why) = why {
            *verdict = Some(with_trace(machine, format!("{why} {when}")));
        }
    };
    keep(&mut verdicts.working, working_without_a_run(machine, &state, &index));
    keep(&mut verdicts.counts, counts_differ(&state, &index));
    keep(&mut verdicts.unlisted, unlisted_wait(&state, &index));
}

/// Runs `history` once, through its rest, and judges it by every property.
fn judge(history: &History) -> Verdicts {
    let mut verdicts = Verdicts::default();
    let mut machine = Machine::new(history);
    for action in &history.actions {
        machine.act(action);
        each_step(&machine, &mut verdicts, &format!("after {action:?}"));
    }
    machine.rest(|machine| each_step(machine, &mut verdicts, "while resting"));
    let state = machine.state();
    let index = ReadIndex::build(&state);
    let failed = |why: String| Some(with_trace(&machine, why));
    let stuck: Vec<String> = open_tasks(&state).into_iter().filter(|task| !ends_well(&machine, &state, &index, task))
        .map(|task| format!("{task} ends {:?} ({:?})", situation(&index, &task), state.lifecycle_of(&task).ok().and_then(|lifecycle| lifecycle.obligation())))
        .collect();
    if !stuck.is_empty() {
        verdicts.ends = failed(stuck.join("; "));
    }
    let seen = &machine.seen;
    if !seen.unchanged.is_empty() {
        verdicts.unchanged = failed(format!("acknowledged with nothing changed: {:?}", seen.unchanged));
    }
    if !seen.close_left_running.is_empty() {
        verdicts.close = failed(seen.close_left_running.join("; "));
    }
    if seen.most_wakes > 3 {
        verdicts.wakes = failed(format!("a node was woken {} times with no progress between", seen.most_wakes));
    }
    let pending: Vec<&TaskId> = state.tasks.values().filter(|task| task.pending_selection.is_some()).map(|task| &task.id).collect();
    if !pending.is_empty() {
        verdicts.moves = failed(format!("a move is still pending for {pending:?}"));
    }
    if !seen.clock_cards.is_empty() {
        verdicts.clock_cards = failed(format!("a pass of the clock opened cards for the user: {:?}", seen.clock_cards));
    }
    let undelivered: Vec<TaskId> = open_tasks(&state).into_iter()
        .filter(|task| state.lifecycle_of(task).ok().and_then(|lifecycle| lifecycle.obligation()).is_some_and(|obligation| obligation.owed == Owed::HandDecision))
        .collect();
    let others_fill_the_slots = |task: &TaskId| slot_tasks(&state).into_iter().filter(|other| other != task).count() >= machine.world.slots;
    let undelivered: Vec<TaskId> = undelivered.into_iter().filter(|task| !others_fill_the_slots(task)).collect();
    if !undelivered.is_empty() {
        verdicts.answers = failed(format!("an answer never reached the turn of {undelivered:?}, with a slot free"));
    }
    verdicts
}

/// The ten properties generate the same histories from the same seed: each history runs once, and
/// every property reads its own verdict of that run.
fn verdicts(history: &History) -> std::sync::Arc<Verdicts> {
    type Cell = std::sync::Arc<std::sync::OnceLock<std::sync::Arc<Verdicts>>>;
    static RUNS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, Cell>>> = std::sync::LazyLock::new(Default::default);
    let cell = RUNS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(format!("{history:?}")).or_default().clone();
    cell.get_or_init(|| std::sync::Arc::new(judge(history))).clone()
}

fn holds(history: &History, verdict: impl Fn(&Verdicts) -> &Option<String>) -> Result<(), TestCaseError> {
    match verdict(&verdicts(history)) {
        None => Ok(()),
        Some(why) => Err(TestCaseError::fail(format!("{why}\nin {history:#?}"))),
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256, failure_persistence: None, rng_seed: proptest::test_runner::RngSeed::Fixed(14), ..ProptestConfig::default()
    })]

    #[test]
    fn every_task_ends_live_or_in_the_users_hands(history in history()) {
        holds(&history, |verdicts| &verdicts.ends)?;
    }

    #[test]
    fn nothing_reads_as_working_without_a_live_session_or_process(history in history()) {
        holds(&history, |verdicts| &verdicts.working)?;
    }

    #[test]
    fn the_counts_agree_at_every_step(history in history()) {
        holds(&history, |verdicts| &verdicts.counts)?;
    }

    #[test]
    fn an_acknowledged_command_changed_the_state_or_was_a_repeat(history in history()) {
        holds(&history, |verdicts| &verdicts.unchanged)?;
    }

    #[test]
    fn a_close_always_leaves_running(history in history()) {
        holds(&history, |verdicts| &verdicts.close)?;
    }

    #[test]
    fn no_node_is_woken_more_than_three_times_without_progress(history in history()) {
        holds(&history, |verdicts| &verdicts.wakes)?;
    }

    #[test]
    fn a_move_ends_moved_or_refused(history in history()) {
        holds(&history, |verdicts| &verdicts.moves)?;
    }

    #[test]
    fn no_deadline_ends_in_a_card_for_the_user(history in history()) {
        holds(&history, |verdicts| &verdicts.clock_cards)?;
    }

    #[test]
    fn an_answer_always_reaches_its_turn(history in history()) {
        holds(&history, |verdicts| &verdicts.answers)?;
    }

    #[test]
    fn a_task_waits_for_the_user_only_on_a_card_the_inbox_lists(history in history()) {
        holds(&history, |verdicts| &verdicts.unlisted)?;
    }
}


