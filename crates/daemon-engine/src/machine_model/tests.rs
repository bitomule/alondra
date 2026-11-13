//! Every task ends at work, or in the user's hands, whatever the provider, the user, the hosts,
//! the daemon's restarts, the Mac and the clock did before the machine rested.
use super::world::{self, History, Host, MINUTE, history};
use super::*;
use alondra_core::domain::{AlondraStep, Delivery, Situation, WaitsFor};
use alondra_domain::Party;
use proptest::prelude::*;

/// A rule the code does not apply yet. While its row is listed, the model applies it itself; the
/// PR that makes the code apply it removes the row and drives the real code from the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Gap {
    /// A task the user sent back still owes the user and reads as a step of Alondra's (delivering the hand-back, or answering the node)
    /// (`handed_back` holds until the node presents again), whether its node already took it and
    /// asked the orchestrator, or a deferred account move left nothing to start a node for it.
    HandedBackOwesUser,
}

pub(super) const KNOWN_GAPS: &[Gap] = &[Gap::HandedBackOwesUser];

fn listed(gap: Gap) -> bool {
    KNOWN_GAPS.contains(&gap)
}

/// The delivery deadline is no gap: the model runs the real watch, which enforces it.
#[test]
fn the_hand_input_deadline_is_enforced_by_the_real_policy() {
    assert!(alondra_core::domain::obligation_policy::enforced(&Owed::HandInput));
    assert!(!alondra_core::domain::obligation_policy::enforced(&Owed::ReadInput));
}

/// `task` was sent back by the user and the domain still says the user owes the next step.
fn handed_back_owing_the_user(state: &DomainState, task: &TaskId) -> bool {
    state.task_handed_back(task).is_some()
        && state.lifecycle_of(task).ok().and_then(|lifecycle| lifecycle.obligation()).is_some_and(|obligation| obligation.party == Party::User)
}

/// How many times a step may be owed again as the one before it, with nobody acting in between: a
/// node woken on each empty turn is owed its input three times, and a host that ends starts the count again.
const MOST_REARMS: u32 = 6;

/// The open tasks whose node holds a slot, as the gate reads them.
pub(super) fn slot_tasks(state: &DomainState) -> BTreeSet<TaskId> {
    crate::admission::slot_tasks(state, &ReadIndex::build(state))
}

/// The open tasks that must hold a slot by what every screen reads of them: at work or a step of
/// Alondra's from it, not ending, their node not paused, and their execution holding a slot.
fn tasks_shown_on_a_slot(state: &DomainState, index: &ReadIndex) -> BTreeSet<TaskId> {
    state.tasks.values()
        .filter(|task| !task.over() && task.state != alondra_core::domain::TaskState::Closing && task.wrap_up.asked_at.is_none())
        .filter(|task| task.agent_id.as_ref().is_some_and(|agent| state.through_sdk(agent) && state.node_pause(agent).is_none()))
        .filter(|task| matches!(situation(index, &task.id), Situation::Working(_) | Situation::Waiting(WaitsFor::Step(AlondraStep::Starting | AlondraStep::Delivering | AlondraStep::Waking))))
        .filter(|task| state.lifecycle_of(&task.id).is_ok_and(|lifecycle| lifecycle.holds_slot()))
        .map(|task| task.id.clone())
        .collect()
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
        Situation::Waiting(WaitsFor::Step(AlondraStep::Delivering)) if state.waits_for_turn(task) => {
            room_is_scarce(machine, state)
        }
        Situation::Waiting(WaitsFor::Step(AlondraStep::Delivering | AlondraStep::Answering)) if listed(Gap::HandedBackOwesUser) && handed_back_owing_the_user(state, task) => true,
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
    rearms: Option<String>,
    closing: Option<String>,
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
    let gate = crate::admission::slot_tasks(state, index);
    let shown = tasks_shown_on_a_slot(state, index);
    (gate != shown).then(|| format!("the gate counts {gate:?} on a slot, the tasks every screen reads on one are {shown:?}"))
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
        verdicts.wakes = failed(format!("a node was woken {} times with no progress between\n{}", seen.most_wakes, seen.excess.clone().unwrap_or_default()));
    }
    if seen.most_rearms > MOST_REARMS {
        verdicts.rearms = failed(format!("a step was owed again {} times with nobody acting: it re-arms in a loop\n{}", seen.most_rearms, seen.excess.clone().unwrap_or_default()));
    }
    let zombies: Vec<TaskId> = crate::sdk_owner::ended_node_tasks(&state).into_iter().filter(|task| machine.world.hosts.contains_key(task)).collect();
    let closing: Vec<String> = seen.closing_taken.iter().chain(&seen.closing_cards).cloned()
        .chain(zombies.iter().map(|task| format!("a host still runs for {task}, whose node ended"))).collect();
    if !closing.is_empty() {
        verdicts.closing = failed(closing.join("; "));
    }
    let pending: Vec<&TaskId> = state.tasks.values().filter(|task| task.pending_selection.is_some()).map(|task| &task.id).collect();
    if !pending.is_empty() {
        verdicts.moves = failed(format!("a move is still pending for {pending:?}"));
    }
    if !seen.clock_cards.is_empty() {
        verdicts.clock_cards = failed(format!("a pass of the clock opened cards for the user: {:?}", seen.clock_cards));
    }
    let undelivered: Vec<TaskId> = open_tasks(&state).into_iter()
        .filter(|task| state.lifecycle_of(task).ok().and_then(|lifecycle| lifecycle.obligation()).is_some_and(|obligation| obligation.owed == Owed::HandDecision)
            || state.waits_for_turn(task))
        .collect();
    let others_fill_the_slots = |task: &TaskId| slot_tasks(&state).into_iter().filter(|other| other != task).count() >= machine.world.slots;
    let undelivered: Vec<TaskId> = undelivered.into_iter().filter(|task| !others_fill_the_slots(task)).collect();
    if !undelivered.is_empty() {
        verdicts.answers = failed(format!("an answer or an input never reached the turn of {undelivered:?}, with a slot free"));
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

/// A host stopped on purpose while the node worked on what the user sent back leaves nothing that
/// starts a turn: the task still ends at work, never delivered with the user's request unheard.
#[test]
fn a_host_stopped_on_purpose_while_the_node_worked_on_what_the_user_sent_back_ends_at_work() {
    let history = History {
        script: vec![world::Turn::Deliver, world::Turn::Background],
        slots: 3,
        actions: vec![world::Action::TurnEnds(0), world::Action::User(world::UserAct::Return, 0), world::Action::Clock(world::PASS * 2), world::Action::StopsHost(0)],
    };
    if let Err(failure) = holds(&history, |verdicts| &verdicts.ends) {
        panic!("{failure}");
    }
}

/// A host that dies with its late facts still unread leaves the turn it cut: the host that takes
/// the task over tells how it ended, so the task never reads as running with nothing running it.
#[test]
fn a_host_that_dies_with_late_facts_leaves_a_task_taken_over_that_reports_the_turn_it_cut() {
    let history = History {
        script: vec![world::Turn::Deliver],
        slots: 2,
        actions: vec![world::Action::User(world::UserAct::Cancel, 0), world::Action::FactsLate(2), world::Action::FactsAgain(2), world::Action::HostDies(2)],
    };
    if let Err(failure) = holds(&history, |verdicts| &verdicts.ends) {
        panic!("{failure}");
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
    fn a_step_never_re_arms_in_a_loop(history in history()) {
        holds(&history, |verdicts| &verdicts.rearms)?;
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

    #[test]
    fn a_closing_node_is_never_taken_over_asks_nothing_and_keeps_no_host(history in world::history_with_cleanups()) {
        holds(&history, |verdicts| &verdicts.closing)?;
    }
}

/// The close of 12 November: a node confirmed its cleanup with its turn still open, the closer took
/// its host from the owner, and a look of the gate found the task working with no host. The task
/// must read as closing, nobody takes it over, nothing it asks reaches the user, and no host is
/// left once its node ended.
#[test]
fn a_node_that_confirms_its_cleanup_in_an_open_turn_closes_with_its_host() {
    use super::world::{Action, Turn, UserAct};
    let history = History {
        script: vec![Turn::Deliver, Turn::AskPermission], slots: 3,
        actions: vec![Action::TurnEnds(0), Action::User(UserAct::Accept, 0), Action::ConfirmsCleanup(0), Action::TurnEnds(0)],
    };
    let mut machine = Machine::new(&history);
    for action in &history.actions {
        machine.act(action);
    }
    machine.rest(|_| {});
    let trace = machine.trace.join("\n");
    assert!(trace.contains("ConfirmCleanup") && trace.contains("RecordNodeEnd"), "the node cleaned up and closed:\n{trace}");
    holds(&history, |verdicts| &verdicts.closing).unwrap();
    holds(&history, |verdicts| &verdicts.working).unwrap();
}

/// The histories the model found for `Gap::HandedBackOwesUser`: they keep ending well while the gap
/// is listed, and the row goes once the reading says what the task waits for.
#[test]
fn a_task_sent_back_ends_well_whether_its_node_asks_or_its_move_is_deferred() {
    use super::world::{Action, Turn, UserAct};
    let asking = History {
        script: vec![Turn::Deliver, Turn::AskOrchestrator], slots: 1,
        actions: vec![Action::Slots(3), Action::OrchestratorAnswers(false), Action::TurnEnds(2), Action::User(UserAct::Return, 2)],
    };
    let moved = History {
        script: vec![Turn::Deliver], slots: 3,
        actions: vec![Action::TurnEnds(2), Action::User(UserAct::Move, 2), Action::User(UserAct::Return, 2)],
    };
    for history in [asking, moved] {
        holds(&history, |verdicts| &verdicts.ends).unwrap();
    }
}

/// The shape behind the 11 November permission answered at 00:21 that reached its turn at 03:25: a
/// permission the user answers while the gate holds the slots of other tasks must reach its turn
/// once one frees.
#[test]
fn a_permission_answered_while_the_gate_is_full_reaches_its_turn() {
    use super::world::{Action, Turn, UserAct};
    let history = History {
        script: vec![Turn::AskPermission], slots: 1,
        actions: vec![Action::TurnEnds(0), Action::User(UserAct::Answer, 0), Action::Slots(2), Action::Slots(3)],
    };
    let mut machine = Machine::new(&history);
    for action in &history.actions {
        machine.act(action);
    }
    assert!(machine.trace.iter().any(|line| line.contains("AnswerQuestion") && line.ends_with("-> ok")), "the user's answer was never accepted:\n{}", machine.trace.join("\n"));
    machine.rest(|_| {});
    let state = machine.state();
    let task = TaskId::from("t-a");
    assert!(!state.lifecycle_of(&task).unwrap().answer_goes_on(), "the answer never reached its turn");
}

/// A node that opened a pull request and then stops with nothing to show is closed by the
/// orchestrator once it stopped too often: the close delivers the pull request to the user in the
/// same command, and never leaves the task running.
#[test]
fn closing_a_stalled_task_with_its_pull_request_open_delivers_it() {
    use super::world::{Action, Turn};
    let history = History { script: vec![Turn::OpenPr], slots: 3, actions: vec![Action::TurnEnds(0)] };
    holds(&history, |verdicts| &verdicts.close).unwrap();
}
