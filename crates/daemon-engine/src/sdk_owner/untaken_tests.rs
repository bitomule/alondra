//! What Alondra does with an input its SDK session did not take, and with work of the node's own
//! that ended with no turn to take up its notice. The clock is a parameter: every pass of the one
//! clock gets the time it runs at.
use super::*;
use crate::obligation_watch::{Watch, pass};
use alondra_core::domain::obligation_policy::Millis;
use alondra_domain::{Owed, Party};

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
    assert!(driver.host_to_replace() || driver.step(&|| false).is_err(), "the owner gives its host up");
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

// Claude Code refuses an input before queueing it: nobody is asked, the same text goes again at
// once as a new input, and when the same session refuses that one too a host that resumes the
// same session takes the message when the gate gives the task a slot.
#[test]
fn an_input_the_provider_refuses_goes_again_and_a_second_refusal_goes_to_a_resuming_host() {
    let slice = Slice::new();
    let text = "Add the tests first refused-by-claude";
    let mut driver = slice.launch_with("Write the release notes");
    slice.settle(&mut driver);
    drive_by_hand(&slice, &driver);
    let cards = slice.domain.state().unwrap().interactions.len();
    tell(&slice, text, "tell-refused");
    for _ in 0..5000 {
        if driver.step(&|| false).is_err() {
            break;
        }
    }
    assert!(driver.host_to_replace(), "the second refusal made the owner give its host up");
    assert_eq!(told(&slice, text), 2, "the refused text went again once: {:?}", slice.prompts());
    assert_eq!(journal(&slice).iter().filter(|record| matches!(record, LifecycleRecord::HandoffResent { .. })).count(), 2, "each refusal staged its resend");
    let mut queued = replaced_with_the_gate_full(&slice, driver);
    let gate = slice.domain.live.gate.snapshot(&slice.domain, alondra_core::now_millis()).unwrap();
    assert!(gate.queue.iter().any(|waiting| waiting.key == crate::admission::Key::Turn(TASK.into())), "its turn is queued: {:?}", gate.queue);
    assert_eq!(lifecycle(&slice.domain).pending_inputs().len(), 1, "one input waits for the resuming host");
    assert_eq!(slice.domain.state().unwrap().interactions.len(), cards, "no card asks the user");
    goes_on_when_the_gate_has_room(&slice, &mut queued, text, 3);
    assert_eq!(told(&slice, text), 3, "the resuming host got the message once: {:?}", slice.prompts());
    queued.stop().unwrap();
}

/// The process of the node wrote at `at`, the last time the clock sees it write.
fn wrote_at(slice: &Slice, at: Millis) {
    slice.domain.live.outputs.lock().unwrap().insert(TASK.into(), at);
}

// A Claude process that hangs on what it was handed writes nothing at all and no event names the
// input. Past the deadline since its last output the task gives up its slot, waits in the queue
// with its input, and the host the gate admits next resumes the same session and takes it up,
// flagged as possibly received. No card asks the user.
#[test]
fn a_hung_process_that_wrote_nothing_is_given_up_for_a_host_that_resumes_the_session() {
    let slice = Slice::new();
    let text = "Add the tests first hung-silent";
    let mut driver = hand(&slice, text);
    drive_by_hand(&slice, &driver);
    wrote_at(&slice, 0);
    let cards = slice.domain.state().unwrap().interactions.len();
    let mut watch = enforcing_every_deadline();
    pass(&slice.domain, &mut watch, 0);
    pass(&slice.domain, &mut watch, 299 * S);
    driver.step(&|| false).unwrap();
    assert!(!driver.host_to_replace(), "nothing before the deadline");

    wrote_at(&slice, 0);
    pass(&slice.domain, &mut watch, 301 * S);
    assert!(slice.domain.requeues.lock().unwrap().contains(&TaskId::from(TASK)), "the watch gave the process up; the task owes {:?}", lifecycle(&slice.domain).obligation());
    let mut queued = replaced_with_the_gate_full(&slice, driver);
    assert_waits_for_a_slot(&slice, &mut queued);
    assert_eq!(slice.domain.state().unwrap().interactions.len(), cards, "no card asks the user");
    assert!(journal(&slice).iter().any(|record| matches!(record, LifecycleRecord::DoubtResent { text: Some(again), .. } if again.contains(text))), "the input goes again flagged as possibly received");
    goes_on_when_the_gate_has_room(&slice, &mut queued, text, 2);
    queued.stop().unwrap();
}

// A process that wrote recently is not hung, whatever the input did not get.
#[test]
fn a_process_that_wrote_within_the_deadline_is_not_given_up() {
    let slice = Slice::new();
    let mut driver = hand(&slice, "Add the tests first hung-silent");
    drive_by_hand(&slice, &driver);
    wrote_at(&slice, 0);
    let mut watch = enforcing_every_deadline();
    pass(&slice.domain, &mut watch, 0);
    wrote_at(&slice, 250 * S);
    pass(&slice.domain, &mut watch, 301 * S);
    driver.step(&|| false).unwrap();
    assert!(!driver.host_to_replace(), "it wrote 51 s ago");
    driver.stop().unwrap();
}

// 10 November: Claude held what it was handed and started no turn on it. Claude Code said it had
// the input (queued), so the provider owes the next step and no deadline of Alondra's ends it: the
// input waits for the provider's own event however long it takes.
#[test]
fn an_input_the_provider_holds_is_never_given_up() {
    let slice = Slice::new();
    let mut driver = hand(&slice, "Add the tests first stall-while-held");
    drive_by_hand(&slice, &driver);
    while lifecycle(&slice.domain).obligation().is_some_and(|obligation| obligation.owed != Owed::ReadInput) {
        driver.observe().unwrap();
    }
    wrote_at(&slice, 0);
    assert_eq!(lifecycle(&slice.domain).obligation().map(|obligation| (obligation.party, obligation.owed)), Some((Party::Provider, Owed::ReadInput)));
    assert_eq!(lifecycle(&slice.domain).activity(), ActivityView::Starting, "the user still reads that Alondra is delivering");
    let mut watch = enforcing_every_deadline();
    for at in [0, 301, 3600, 24 * 3600] {
        pass(&slice.domain, &mut watch, at * S);
    }
    driver.step(&|| false).unwrap();
    assert!(!driver.host_to_replace(), "the task keeps its host");
    assert!(!journal(&slice).iter().any(|record| matches!(record, LifecycleRecord::DoubtResent { .. })), "nothing is offered again");
    driver.stop().unwrap();
}

// The deadline is on the session's own word that it took the input, not on its first message:
// a session that says it runs at once and writes nothing for minutes is never given up.
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
