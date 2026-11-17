//! An input the provider refuses, queued again at the gate.
use super::*;
use alondra_daemon_engine::obligation_watch::{Watch, pass};
use alondra_daemon_engine::sdk_owner::tests::untaken::drive_by_hand;
use alondra_domain::{Owed, Party};
use alondra_daemon_engine::sdk_owner::*;
use alondra_daemon_engine::sdk_owner::tests::*;
use alondra_daemon_engine::sdk_owner::tests::untaken::*;

#[test]
fn an_input_the_provider_refuses_goes_again_and_a_second_refusal_goes_to_a_resuming_host() {
    wired();
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
    let gate = slice.domain.admission().snapshot(&slice.domain, alondra_core::now_millis()).unwrap();
    assert!(gate.queue.iter().any(|waiting| waiting.key == crate::ports::admission::Key::Turn(TASK.into())), "its turn is queued: {:?}", gate.queue);
    assert_eq!(lifecycle(&slice.domain).pending_inputs().len(), 1, "one input waits for the resuming host");
    assert_eq!(slice.domain.state().unwrap().interactions.len(), cards, "no card asks the user");
    goes_on_when_the_gate_has_room(&slice, &mut queued, text, 3);
    assert_eq!(told(&slice, text), 3, "the resuming host got the message once: {:?}", slice.prompts());
    queued.stop().unwrap();
}

#[test]
fn a_hung_process_that_wrote_nothing_is_given_up_for_a_host_that_resumes_the_session() {
    wired();
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
    assert!(slice.domain.sdk_owners().requeues.lock().unwrap().contains(&TaskId::from(TASK)), "the watch gave the process up; the task owes {:?}", lifecycle(&slice.domain).obligation());
    let mut queued = replaced_with_the_gate_full(&slice, driver);
    assert_waits_for_a_slot(&slice, &mut queued);
    assert_eq!(slice.domain.state().unwrap().interactions.len(), cards, "no card asks the user");
    assert!(journal(&slice).iter().any(|record| matches!(record, LifecycleRecord::DoubtResent { text: Some(again), .. } if again.contains(text))), "the input goes again flagged as possibly received");
    goes_on_when_the_gate_has_room(&slice, &mut queued, text, 2);
    queued.stop().unwrap();
}

#[test]
fn a_replacement_the_watch_gives_up_is_replaced_with_no_card() {
    wired();
    let slice = Slice::new();
    let text = "Add the tests first hung-silent";
    let mut driver = hand(&slice, text);
    drive_by_hand(&slice, &driver);
    wrote_at(&slice, 0);
    let mut watch = enforcing_every_deadline();
    pass(&slice.domain, &mut watch, 0);
    pass(&slice.domain, &mut watch, 301 * S);
    let mut replacement = replaced_with_the_gate_full(&slice, driver);
    let cards = slice.domain.state().unwrap().interactions.len();
    slice.domain.sdk_owners().requeues.lock().unwrap().insert(TASK.into());
    assert!(replacement.step(&|| false).is_err(), "the watch gave the replacement's process up");
    assert!(replacement.host_to_replace(), "a host that resumes the same session takes its place");
    let mut next = replacement.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    assert_eq!(slice.domain.state().unwrap().interactions.len(), cards, "no card asks the user");
    next.stop().unwrap();
}

#[test]
fn an_input_refused_twice_goes_back_to_the_queue() {
    wired();
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

/// The owner gives its host up on its next step and, with the gate full, a host that resumes the
/// same session takes its place.
pub fn replaced_with_the_gate_full(slice: &Slice, mut driver: Driver) -> Driver {
    assert!(driver.host_to_replace() || driver.step(&|| false).is_err(), "the owner gives its host up");
    slice.domain.admission().tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(0)) });
    assert!(driver.host_to_replace());
    let mut next = driver.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    drive_by_hand(slice, &next);
    next.step(&|| false).unwrap();
    next
}

/// The task holds no slot and its input waits in the gate's queue like any other, with no card.
pub fn assert_waits_for_a_slot(slice: &Slice, driver: &mut Driver) {
    assert_eq!(driver.pending_permits(), 1, "its input waits for the gate");
    let gate = slice.domain.admission().snapshot(&slice.domain, alondra_core::now_millis()).unwrap();
    assert!(gate.queue.iter().any(|waiting| waiting.key == crate::ports::admission::Key::Turn(TASK.into())), "its turn is queued: {:?}", gate.queue);
    assert!(!gate.working_tasks.contains(&TaskId::from(TASK)), "it holds no slot: {:?}", gate.working_tasks);
    assert!(lifecycle(&slice.domain).cards().is_empty(), "no card asks the user");
}

/// The gate has room again: the host that took the place of the old one gets the input, Claude
/// starts on it, and every host resumed the session the first one opened.
pub fn goes_on_when_the_gate_has_room(slice: &Slice, driver: &mut Driver, text: &str, times: usize) {
    slice.let_claude_start();
    slice.domain.admission().tune(crate::admission::Tuning::default());
    tokio::runtime::Runtime::new().unwrap().block_on(crate::admission::pass(&slice.domain, &slice._home)).unwrap();
    until_prompted(slice, driver, text, times);
    slice.settle(driver);
    let starts = slice.starts();
    assert!(starts[1..].iter().all(|start| start["resume"] == starts[0]["session"]), "every host resumed the same session: {starts:?}");
    assert_ne!(lifecycle(&slice.domain).activity(), ActivityView::Starting, "Claude started on it");
}
