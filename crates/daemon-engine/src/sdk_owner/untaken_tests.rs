//! What Alondra does with an input its SDK session did not take, and with work of the node's own
//! that ended with no turn to take up its notice. The clock is a parameter: every pass of the one
//! clock gets the time it runs at.
use crate::sdk_owner::SdkOwners as _;
use crate::live::LiveOf as _;
use super::*;
use crate::obligation_watch::{Watch, pass};
use alondra_domain_rules::obligation_policy::Millis;
use alondra_domain::{Owed, Party};

pub const S: Millis = 1000;

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

pub fn journal(slice: &Slice) -> Vec<LifecycleRecord> {
    slice.domain.state().unwrap().lifecycle_journal[&TaskId::from(TASK)].iter().cloned().collect()
}

fn work_ended_told(slice: &Slice) -> usize {
    journal(slice).iter().filter(|record| matches!(record, LifecycleRecord::Told { text: Some(text), .. } if text == crate::sdk_owner::untaken::WORK_ENDED)).count()
}

/// The test steps the owner of `driver` itself, as the daemon would.
pub fn drive_by_hand(slice: &Slice, driver: &Driver) {
    slice.domain.sdk_owners().driven_by_hand.lock().unwrap().insert(driver.agent().clone());
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


// Claude Code refuses an input before queueing it: nobody is asked, the same text goes again at
// once as a new input, and when the same session refuses that one too a host that resumes the
// same session takes the message when the gate gives the task a slot.

/// The process of the node wrote at `at`, the last time the clock sees it write.
pub fn wrote_at(slice: &Slice, at: Millis) {
    slice.domain.live().outputs.lock().unwrap().insert(TASK.into(), at);
}

// A Claude process that hangs on what it was handed writes nothing at all and no event names the
// input. Past the deadline since its last output the task gives up its slot, waits in the queue
// with its input, and the host the gate admits next resumes the same session and takes it up,
// flagged as possibly received. No card asks the user.

// Production, 2026-11-13 (t-message-delivery-fresh-codex): every commit that touched the task while
// its input waited issued a permit, and the owner took them oldest first. After a takeover that
// committed thousands of events it claimed 4,876 stale permits, one owner transaction each, before
// the input went 46 s later. Only the permit of the last commit can be claimed.
#[test]
fn an_owner_claims_only_the_permit_the_last_commit_issued() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("Write the release notes");
    slice.settle(&mut driver);
    for (text, id) in [("First waiting message", "tell-first"), ("Second waiting message", "tell-second"), ("Third waiting message", "tell-third")] {
        tell(&slice, text, id);
    }
    assert_eq!(driver.pending_permits(), 1, "the permits of earlier commits are stale");
    let first = lifecycle(&slice.domain).pending_inputs()[0].as_str().to_owned();
    driver.step(&|| false).unwrap();
    assert_eq!(dispatch_status(&slice, &first).as_deref(), Some("handed"), "the first step hands the input that waits first");
    driver.stop().unwrap();
}

// Production, 2026-11-13 20:50 (t-message-not-lost-on-close): after a restart, the host that
// resumed the session was given up by the watch like a first one, but its owner, a replacement,
// gave the task up as lost and put a card, though two seconds later Alondra took it over again by
// itself. A replacement the watch gives up is replaced, as a first host is.

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
