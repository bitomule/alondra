//! What Alondra does with an input its SDK session did not take, and with work of the node's own
//! that ended with no turn to take up its notice. The clock is a parameter: every pass of the one
//! clock gets the time it runs at.
use super::*;
use crate::obligation_watch::{Watch, pass};
use alondra_core::domain::obligation_policy::Millis;
use alondra_domain::Owed;

const S: Millis = 1000;

pub fn enforcing_every_deadline() -> Watch {
    Watch::enforcing(|owed| matches!(owed, Owed::HandInput | Owed::Wake))
}

pub fn tell(slice: &Slice, text: &str, id: &str) {
    crate::domain_messages::tell_sdk_node(&slice.domain, &TASK.into(), "Alice", text, id, true).unwrap();
}

pub fn told(slice: &Slice, text: &str) -> usize {
    slice.prompts().iter().filter(|prompt| prompt.contains(text)).count()
}

/// Sends what the store issues and reads the host until Claude got `text` `times` times.
pub fn until_prompted(slice: &Slice, driver: &mut Driver, text: &str, times: usize) {
    while told(slice, text) < times {
        if driver.pending_permits() > 0 {
            driver.step(&|| false).unwrap();
        } else if let Err(error) = driver.observe() {
            panic!("{error} before Claude got {text:?} {times} times: {:?}", slice.prompts());
        }
    }
}

/// A resting session is told `text`, its host is handed it, and Claude got it.
pub fn hand(slice: &Slice, text: &str) -> Driver {
    let mut driver = slice.launch_with("Write the release notes");
    slice.settle(&mut driver);
    tell(slice, text, "tell-held");
    until_prompted(slice, &mut driver, text, 1);
    driver
}

fn dispatch_status(slice: &Slice, input: &str) -> Option<String> {
    slice.observer().query_row("SELECT status FROM lifecycle_dispatches WHERE task_id=?1 AND input_id=?2", [TASK, input], |row| row.get(0)).ok()
}

fn journal(slice: &Slice) -> Vec<LifecycleRecord> {
    slice.domain.state().unwrap().lifecycle_journal[&TaskId::from(TASK)].iter().cloned().collect()
}

fn work_ended_told(slice: &Slice) -> usize {
    journal(slice).iter().filter(|record| matches!(record, LifecycleRecord::Told { text: Some(text), .. } if text == crate::sdk_owner::untaken::WORK_ENDED)).count()
}

/// The test steps the owner of `driver` itself, as the daemon would.
pub fn drive_by_hand(slice: &Slice, driver: &Driver) {
    slice.domain.driven_by_hand.lock().unwrap().insert(driver.agent().clone());
}

/// The owner gives its host up on its next step and, with the gate full, a host that resumes the
/// same session takes its place.
pub fn replaced_with_the_gate_full(slice: &Slice, mut driver: Driver) -> Driver {
    assert!(driver.step(&|| false).is_err(), "the owner gives its host up");
    slice.domain.live.gate.tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(0)) });
    assert!(driver.host_to_replace());
    let mut next = driver.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    drive_by_hand(slice, &next);
    next.step(&|| false).unwrap();
    next
}

/// The task holds no slot and its input waits in the gate's queue like any other, with no card.
pub fn assert_waits_for_a_slot(slice: &Slice, driver: &mut Driver) {
    assert_eq!(driver.pending_permits(), 1, "its input waits for the gate");
    let gate = slice.domain.live.gate.snapshot(&slice.domain, alondra_core::now_millis()).unwrap();
    assert!(gate.queue.iter().any(|waiting| waiting.key == crate::admission::Key::Turn(TASK.into())), "its turn is queued: {:?}", gate.queue);
    assert!(!gate.working_tasks.contains(&TaskId::from(TASK)), "it holds no slot: {:?}", gate.working_tasks);
    assert!(lifecycle(&slice.domain).cards().is_empty(), "no card asks the user");
}

/// The gate has room again: the host that took the place of the old one gets the input, Claude
/// starts on it, and every host resumed the session the first one opened.
pub fn goes_on_when_the_gate_has_room(slice: &Slice, driver: &mut Driver, text: &str, times: usize) {
    slice.let_claude_start();
    slice.domain.live.gate.tune(crate::admission::Tuning::default());
    tokio::runtime::Runtime::new().unwrap().block_on(crate::admission::pass(&slice.domain, &slice._home)).unwrap();
    until_prompted(slice, driver, text, times);
    slice.settle(driver);
    let starts = slice.starts();
    assert!(starts[1..].iter().all(|start| start["resume"] == starts[0]["session"]), "every host resumed the same session: {starts:?}");
    assert_ne!(lifecycle(&slice.domain).activity(), ActivityView::Starting, "Claude started on it");
}

// E1, 10 November: the host refused an input before offering it, the dispatch stayed `rejected`, and
// no later input of the task went either; the task read «Waiting for Claude to start» until its host
// died. The refusal is the event: the same text goes again in the same step, and what follows goes.
#[test]
fn a_refused_input_is_prepared_again_at_once_and_later_inputs_still_go() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("Write the release notes");
    slice.settle(&mut driver);
    driver.refuse_offers = 1;
    tell(&slice, "Add the tests first", "tell-first");
    tell(&slice, "And the changelog", "tell-second");
    until_prompted(&slice, &mut driver, "And the changelog", 1);
    assert_eq!(told(&slice, "Add the tests first"), 1, "{:?}", slice.prompts());
    assert!(journal(&slice).iter().any(|record| matches!(record, LifecycleRecord::HandoffResent { .. })), "the refused input went again");
    assert!(!driver.host_to_replace(), "the task keeps its host");
    slice.settle(&mut driver);
    driver.stop().unwrap();
}

#[test]
fn an_input_refused_twice_goes_back_to_the_queue() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("Write the release notes");
    slice.settle(&mut driver);
    drive_by_hand(&slice, &driver);
    driver.refuse_offers = 2;
    tell(&slice, "Add the tests first", "tell-first");
    while driver.refuse_offers == 2 {
        driver.step(&|| false).unwrap();
    }
    let mut queued = replaced_with_the_gate_full(&slice, driver);
    assert_eq!(told(&slice, "Add the tests first"), 0, "nothing reached Claude");
    assert_waits_for_a_slot(&slice, &mut queued);
    goes_on_when_the_gate_has_room(&slice, &mut queued, "Add the tests first", 1);
    queued.stop().unwrap();
}

// 10 November: Claude held what it was handed and started no turn on it, and the task read «Waiting
// for Claude to start» with no end, holding its slot. Past the deadline the input is offered again;
// past it once more the task gives up its slot, waits in the queue with its input, and the host the
// gate admits next resumes the same session and takes it up. No card asks the user.
#[test]
fn an_offered_input_nobody_takes_is_offered_again_then_requeued() {
    let slice = Slice::new();
    let text = "Add the tests first stall-while-held";
    let mut driver = hand(&slice, text);
    drive_by_hand(&slice, &driver);
    let cards = slice.domain.state().unwrap().interactions.len();
    let mut watch = enforcing_every_deadline();
    pass(&slice.domain, &mut watch, 0);
    pass(&slice.domain, &mut watch, 119 * S);
    assert!(!journal(&slice).iter().any(|record| matches!(record, LifecycleRecord::DoubtResent { .. })), "nothing before the deadline");

    pass(&slice.domain, &mut watch, 121 * S);
    let resent = journal(&slice).iter().find_map(|record| match record {
        LifecycleRecord::DoubtResent { resent, text: Some(again), .. } if again.contains(text) => Some(resent.clone()),
        _ => None,
    }).expect("the input is offered again");
    while driver.pending_permits() > 0 {
        driver.step(&|| false).unwrap();
    }
    assert_eq!(dispatch_status(&slice, &resent).as_deref(), Some("handed"), "the host has it again");

    pass(&slice.domain, &mut watch, 200 * S);
    driver.step(&|| false).unwrap();
    assert!(!driver.host_to_replace(), "the offer again has a deadline of its own");
    pass(&slice.domain, &mut watch, 242 * S);
    let mut queued = replaced_with_the_gate_full(&slice, driver);
    assert_waits_for_a_slot(&slice, &mut queued);
    assert_eq!(slice.domain.state().unwrap().interactions.len(), cards, "no card asks the user");
    goes_on_when_the_gate_has_room(&slice, &mut queued, text, 2);
    queued.stop().unwrap();
}

// The deadline is on the session's own word that it took the input, not on its first message:
// a session that says it runs at once and writes nothing for minutes is never offered it again.
#[test]
fn a_session_that_is_running_is_not_reoffered() {
    let slice = Slice::new();
    let mut driver = hand(&slice, "Add the tests first running-then-silent");
    while lifecycle(&slice.domain).activity() != ActivityView::Working && driver.worker.readable(Duration::from_secs(5)).unwrap() {
        driver.observe().unwrap();
    }
    let mut watch = enforcing_every_deadline();
    for at in [0, 2, 121, 242, 400] {
        pass(&slice.domain, &mut watch, at * S);
    }
    assert!(!journal(&slice).iter().any(|record| matches!(record, LifecycleRecord::DoubtResent { .. })), "nothing is offered again");
    assert!(lifecycle(&slice.domain).execution().is_some(), "the task keeps its execution");
    assert_eq!(lifecycle(&slice.domain).activity(), ActivityView::Working);
    driver.stop().unwrap();
}

// Claude's background command ended and Claude opened no turn to take up its notice: nothing woke
// the node. Once the deadline passes with no such turn, Alondra tells the node, as it tells Codex
// at once, and the turn that takes it up ends the wait. This deadline is enforced by default.
#[test]
fn finished_work_wakes_a_claude_node_after_two_minutes() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("background-command-silent");
    until_own_work(&slice, &mut driver);
    slice.release_background_command();
    while lifecycle(&slice.domain).activity() != ActivityView::Continuation {
        driver.observe().unwrap();
    }
    let mut watch = Watch::default();
    pass(&slice.domain, &mut watch, 0);
    pass(&slice.domain, &mut watch, 119 * S);
    assert_eq!(work_ended_told(&slice), 0, "Claude may still open its own turn");
    pass(&slice.domain, &mut watch, 121 * S);
    assert_eq!(work_ended_told(&slice), 1, "Alondra tells the node");
    assert!(!lifecycle(&slice.domain).pending_inputs().is_empty());
    slice.settle(&mut driver);
    assert_ne!(lifecycle(&slice.domain).activity(), ActivityView::Continuation, "a turn took up the notice");
    driver.stop().unwrap();
}

#[test]
fn a_notification_turn_that_opens_in_time_gets_no_second_wake() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("background-command");
    until_own_work(&slice, &mut driver);
    slice.release_background_command();
    let mut watch = Watch::default();
    let mut seen = false;
    while !seen || matches!(lifecycle(&slice.domain).activity(), ActivityView::Continuation | ActivityView::OwnWork) {
        driver.observe().unwrap();
        if !seen && lifecycle(&slice.domain).activity() == ActivityView::Continuation {
            pass(&slice.domain, &mut watch, 0);
            seen = true;
        }
    }
    slice.settle(&mut driver);
    for at in [5, 121, 600] {
        pass(&slice.domain, &mut watch, at * S);
    }
    assert_eq!(work_ended_told(&slice), 0, "Claude's own turn took up the notice");
    driver.stop().unwrap();
}
