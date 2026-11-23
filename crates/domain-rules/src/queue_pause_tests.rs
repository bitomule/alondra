//! A task paused while it waits in the queue to start: one fact of the domain, so it is never lost
//! between two stores, and it never outlives the task.
use super::lifecycle_tests::{exec, must, task, user, world};
use super::*;

fn paused(s: &DomainState, id: &str) -> Option<alondra_core::model::Millis> {
    s.tasks[&TaskId::from(id)].paused_in_queue
}

#[test]
fn cancelling_a_task_paused_in_the_queue_ends_its_pause() {
    let mut s = world();
    task(&mut s, "t", None);
    must(&mut s, user(), Command::PauseQueuedTask { task_id: "t".into(), at: 20 });
    assert_eq!(paused(&s, "t"), Some(20));
    must(&mut s, user(), Command::CancelTask { task_id: "t".into(), reason: None });
    assert_eq!(paused(&s, "t"), None, "a cancelled task stays paused nowhere");
}

#[test]
fn a_task_is_paused_in_the_queue_once_and_only_before_it_has_a_node() {
    let mut s = world();
    task(&mut s, "t", None);
    task(&mut s, "started", Some("a"));
    must(&mut s, user(), Command::PauseQueuedTask { task_id: "t".into(), at: 20 });
    assert_eq!(exec(&mut s, user(), Command::PauseQueuedTask { task_id: "t".into(), at: 30 }).unwrap_err().code, "unchanged");
    assert_eq!(paused(&s, "t"), Some(20), "it keeps the time it was first paused");
    assert_eq!(exec(&mut s, user(), Command::PauseQueuedTask { task_id: "started".into(), at: 30 }).unwrap_err().code, "conflict");
    must(&mut s, user(), Command::ResumeQueuedTask { task_id: "t".into() });
    assert_eq!(paused(&s, "t"), None);
    assert_eq!(exec(&mut s, user(), Command::ResumeQueuedTask { task_id: "t".into() }).unwrap_err().code, "conflict");
}
