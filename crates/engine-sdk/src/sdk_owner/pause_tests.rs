use super::*;
use super::tests::{Slice, envelope};

fn move_node(domain: &DomainRuntime, agent: &AgentId, command: &str, event: NodeEvent) {
    let state = domain.read();
    domain.execute_fixture(&Actor::System, envelope(state.state(), command, Command::MoveNode {
        agent_id: agent.clone(), event,
    })).unwrap();
}

fn pause(since: i64) -> NodeEvent {
    NodeEvent::Paused { pause: Pause {
        reason: PauseReason::Memory, since, cut: None, turn_since: None,
        cleanup: PauseCleanup::Done, released: false,
    } }
}

fn check_pause(driver: &Driver, expected: bool, resolutions: u64) {
    let queries = measure::pause_queries();
    let tasks = measure::tasks_read();
    assert_eq!(driver.paused(), expected);
    assert_eq!(measure::pause_queries() - queries, resolutions, "only a new publication resolves the pause");
    assert_eq!(measure::tasks_read() - tasks, resolutions, "the fixture has one task belonging to this driver");
    let queries = measure::pause_queries();
    let tasks = measure::tasks_read();
    for _ in 0..32 { assert_eq!(driver.paused(), expected); }
    assert_eq!(measure::pause_queries() - queries, 0, "unchanged checks never query the domain");
    assert_eq!(measure::tasks_read() - tasks, 0, "unchanged checks never inspect tasks");
}

#[test]
fn published_pause_and_resume_are_resolved_once_by_the_driver() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    assert!(!driver.paused());
    check_pause(&driver, false, 0);
    slice.domain.wake_subscribers();
    check_pause(&driver, false, 0);
    move_node(&slice.domain, driver.agent(), "pause", pause(1));
    check_pause(&driver, true, 1);
    move_node(&slice.domain, driver.agent(), "resume", NodeEvent::Resumed);
    check_pause(&driver, false, 1);
    move_node(&slice.domain, driver.agent(), "pause-again", pause(2));
    check_pause(&driver, true, 1);
    driver.stop().unwrap();
}

#[test]
fn rebinding_a_driver_replaces_its_pause_subscription() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    assert!(!driver.paused());
    let restarted = slice.restart();
    move_node(&restarted, driver.agent(), "pause-on-new-runtime", pause(1));
    check_pause(&driver, false, 0);
    let queries = measure::pause_queries();
    let tasks = measure::tasks_read();
    driver.rebind(Arc::clone(&restarted));
    assert_eq!(measure::pause_queries() - queries, 1, "rebind resolves the replacement subscription once");
    assert_eq!(measure::tasks_read() - tasks, 1);
    check_pause(&driver, true, 0);
    move_node(&restarted, driver.agent(), "resume-on-new-runtime", NodeEvent::Resumed);
    check_pause(&driver, false, 1);
    driver.stop().unwrap();
}

#[test]
fn rebinding_at_the_same_revision_observes_the_new_runtimes_publications() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    assert!(!driver.paused());
    let restarted = slice.restart();
    assert_eq!(slice.domain.read().state().revision, restarted.read().state().revision);
    let queries = measure::pause_queries();
    let tasks = measure::tasks_read();
    driver.rebind(Arc::clone(&restarted));
    assert_eq!(measure::pause_queries() - queries, 1, "equal revisions still require a new subscription");
    assert_eq!(measure::tasks_read() - tasks, 1);
    check_pause(&driver, false, 0);
    move_node(&restarted, driver.agent(), "pause-after-equal-revision-rebind", pause(1));
    assert!(slice.domain.read().node_pause(driver.agent()).is_none());
    check_pause(&driver, true, 1);
    move_node(&restarted, driver.agent(), "resume-after-equal-revision-rebind", NodeEvent::Resumed);
    check_pause(&driver, false, 1);
    driver.stop().unwrap();
}

#[test]
fn a_driver_created_for_an_already_paused_host_observes_its_first_resume() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    move_node(&slice.domain, driver.agent(), "pause-before-adoption", pause(1));
    drop(driver);
    let restarted = slice.restart();
    let mut adopted = slice.take_over(&restarted);
    assert!(adopted.paused(), "adopting an existing host does not release its pause");
    check_pause(&adopted, true, 0);
    move_node(&restarted, adopted.agent(), "resume-after-adoption", NodeEvent::Resumed);
    check_pause(&adopted, false, 1);
    adopted.stop().unwrap();
}

#[test]
fn a_driver_observes_pause_publications_from_another_thread() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    assert!(!driver.paused());
    let domain = Arc::clone(&slice.domain);
    let agent = driver.agent().clone();
    std::thread::scope(|scope| {
        let (published, committed) = std::sync::mpsc::channel();
        let (proceed, next) = std::sync::mpsc::channel();
        scope.spawn(move || {
            move_node(&domain, &agent, "pause-from-writer", pause(1));
            published.send(()).unwrap();
            next.recv().unwrap();
            move_node(&domain, &agent, "resume-from-writer", NodeEvent::Resumed);
            published.send(()).unwrap();
        });
        committed.recv().unwrap();
        check_pause(&driver, true, 1);
        proceed.send(()).unwrap();
        committed.recv().unwrap();
        check_pause(&driver, false, 1);
    });
    driver.stop().unwrap();
}

#[test]
fn idle_driver_steps_reach_the_event_wait_without_querying_pause_again() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    // The fake sends completion after the result that makes settle return.
    let completed = driver.worker.poll().unwrap().unwrap();
    let message = alondra_provider_claude::sdk_message(&completed).unwrap();
    assert_eq!(message["type"], "command_lifecycle");
    assert_eq!(message["state"], "completed");
    driver.observe().unwrap();
    assert!(!driver.paused());
    assert_eq!(driver.pending_permits(), 0);
    assert!(!driver.waits_for_host());
    let revision = slice.domain.read().state().revision;
    let waits = std::cell::Cell::new(0);
    let stopped = || { waits.set(waits.get() + 1); true };
    let queries = measure::pause_queries();
    let tasks = measure::tasks_read();
    for _ in 0..32 { driver.step(&stopped).unwrap(); }
    assert_eq!(measure::pause_queries() - queries, 0, "the real idle owner path does not resolve an unchanged pause");
    assert_eq!(measure::tasks_read() - tasks, 0, "idle owner steps do not inspect tasks for pause");
    assert_eq!(waits.get(), 32, "every step reached the seals wait, with no timeout needed");
    assert_eq!(slice.domain.read().state().revision, revision, "the fixture stayed idle throughout the measurement");
    driver.stop().unwrap();
}
