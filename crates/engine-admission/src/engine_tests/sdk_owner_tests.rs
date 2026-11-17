//! The SDK owner's tests that need the gate itself: its slots, its pauses and the disk's floor as the
//! owner meets them.
use super::*;
use alondra_daemon_engine::sdk_owner::*;
use alondra_daemon_engine::sdk_owner::tests::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_pause_by_the_user_stops_the_host_its_background_commands_and_every_turn_until_it_resumes() {
    wired();
    let slice = Slice::new();
    let (agent, session, commands, host) = node_with_a_background_group(&slice);
    let gate = &slice.domain.admission();
    gate.pause_by_user(&slice.domain, &agent.0, alondra_core::now_millis()).unwrap();
    slice.domain.join_retakes();
    assert_stopped_whole(&slice, &agent, &commands, &host);
    assert!(slice.domain.state().unwrap().host_ended_since_pause(&TASK.into()));
    crate::admission::pass_at(&slice.domain, &slice._home, alondra_core::now_millis()).await.unwrap();
    slice.domain.join_retakes();
    assert!(gate.paused(&slice.domain, &agent.0).is_some(), "only the user ends it");
    assert_eq!(slice.starts().len(), 1, "a later look at the pauses starts nothing");

    assert!(gate.resume_now(&slice.domain, &agent.0, alondra_core::now_millis()));
    crate::admission::pass_at(&slice.domain, &slice._home, alondra_core::now_millis()).await.unwrap();
    until_last_prompt_ends_with(&slice.domain, &slice.capture, "run again what was cut.").await;
    slice.domain.stop_sdk_owners();
    assert_eq!(slice.starts().last().unwrap()["resume"].as_str(), Some(session.as_str()), "the same session goes on: {:?}", slice.starts());
    assert!(open_card(&slice.domain, SESSION_ENDED_PREFIX).is_none(), "no card asks the user");
}

#[tokio::test(flavor = "multi_thread")]
async fn no_pause_but_the_users_kills_what_the_node_runs() {
    wired();
    use crate::ports::admission::Reason;
    for reason in [Reason::Disk, Reason::Declared { reason: "Waiting for a full machine".into() }, Reason::Usage] {
        let slice = Slice::new();
        let (agent, _, commands, host) = node_with_a_background_group(&slice);
        slice.domain.admission().tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(0)) });
        assert!(slice.domain.gate().hold_back(&agent.0, reason.clone(), alondra_core::now_millis()));
        crate::admission::pass_at(&slice.domain, &slice._home, alondra_core::now_millis()).await.unwrap();
        slice.domain.join_retakes();
        assert_eq!(commands.running().len(), 2, "{reason:?}: the background command and its child go on");
        assert_eq!(survivors(&host).len(), host.len(), "{reason:?}: the host and its Claude go on");
        assert_eq!(slice.starts().len(), 1, "{reason:?}: the same Claude goes on");
        slice.domain.stop_sdk_owners();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_a_restarted_daemon_adopts_for_a_paused_node_stops_at_the_gates_next_look() {
    wired();
    let slice = Slice::new();
    let mut driver = slice.launch_with("background-group");
    until_own_work(&slice, &mut driver);
    let commands = BackgroundCommands::of(&slice);
    let host = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
    let agent = driver.agent().clone();
    assert!(slice.domain.gate().hold_back(&agent.0, crate::ports::admission::Reason::ByUser, alondra_core::now_millis()));
    drop(driver);
    let restarted = slice.restart();
    restarted.enable_sdk(slice.backend.clone());
    restarted.admission().ignore_mac();
    adopt_all(&restarted, &slice.backend);
    assert_eq!(commands.running().len(), 2, "the adopted host still runs them");
    crate::admission::pass_at(&restarted, &slice._home, alondra_core::now_millis()).await.unwrap();
    restarted.join_retakes();
    assert_eq!(commands.running(), Vec::<String>::new(), "the background command and its child ended");
    assert_eq!(survivors(&host), Vec::<String>::new());
    restarted.stop_sdk_owners();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_takeover_the_full_disk_refused_waits_for_space_and_then_resumes_the_same_session() {
    wired();
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    kill_host_and_daemon(driver);
    let restarted = slice.restart();
    restarted.enable_sdk(slice.backend.clone());
    restarted.full_disk.store(true, std::sync::atomic::Ordering::SeqCst);
    adopt_all(&restarted, &slice.backend);
    let task = TaskId::from(TASK);
    let agent = restarted.state().unwrap().tasks[&task].agent_id.clone().unwrap();
    assert!(open_card(&restarted, HOST_LOST_PREFIX).is_none(), "a full disk is not the task's failure: no card");
    assert_ne!(restarted.state().unwrap().tasks[&task].state, TaskState::NeedsYou);
    assert!(!restarted.owns_sdk_host(&agent));
    assert!(restarted.sdk_owners().waiting_for_space.lock().unwrap().contains(&task), "the takeover waits for space");
    restarted.full_disk.store(false, std::sync::atomic::Ordering::SeqCst);
    let disk = |free: u64| alondra_core::Machine { disk_total: 500_000_000_000, disk_available: free, ..alondra_core::Machine::default() };
    crate::machine_watch::look(&restarted, disk(900_000_000));
    restarted.join_retakes();
    assert!(!restarted.owns_sdk_host(&agent), "a sample that measures the disk full takes nothing over");
    assert!(restarted.sdk_owners().waiting_for_space.lock().unwrap().contains(&task));
    loop {
        crate::machine_watch::look(&restarted, disk(400_000_000_000));
        if !restarted.gate().disk_low() {
            break;
        }
    }
    restarted.join_retakes();
    assert!(restarted.owns_sdk_host(&agent), "the sample that measured space took the execution over");
    assert!(restarted.sdk_owners().waiting_for_space.lock().unwrap().is_empty());
    assert!(open_card(&restarted, HOST_LOST_PREFIX).is_none());
    let mut changed = restarted.subscribe();
    let capture = slice.capture.clone();
    changed.wait_for(|_| prompts_at(&capture).last().is_some_and(|prompt| prompt.starts_with("Your session was cut off when its host stopped"))).await.unwrap();
    restarted.stop_sdk_owners();
    let starts = slice.starts();
    assert_eq!(starts.last().unwrap()["resume"].as_str(), Some(session.as_str()), "the same session is resumed: {starts:?}");
}

/// The nodes of 2026-10-22: five nodes of the Agent SDK wait inside their turn for the user to
/// approve a plan or answer a question, one works, and a High task waits in the queue with a limit
/// of 6. They hold no slot, so it starts, and the gate, the log's "waiting on the user" and the
/// apps' count agree on who works. A plan approved once every slot is taken again waits for a
/// slot like any turn, and goes on when one frees.
#[test]
fn nodes_waiting_on_the_user_inside_their_turn_leave_their_slots_to_the_queue() {
    wired();
    use crate::admission::{Hold as Held, Key, Limit, Tuning, Waiting};
    let slice = Slice::new();
    let gate = &slice.domain.admission();
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
    let task_of = |state: &DomainState, agent: &AgentId| state.tasks.values().find(|task| task.agent_id.as_ref() == Some(agent)).map(|task| task.id.clone());
    let activity = move |state: &DomainState, agent: &AgentId| task_of(state, agent)
        .and_then(|task| state.lifecycle_of(&task).ok()).map(|lifecycle| (lifecycle.activity(), !lifecycle.cards().is_empty()));
    let waits_on_user = move |state: &DomainState, agent: &AgentId| activity(state, agent) == Some((ActivityView::Working, true));
    let works = move |state: &DomainState, agent: &AgentId| activity(state, agent) == Some((ActivityView::Working, false));
    let mut drivers = vec![];
    for (task, prompt) in [("p1", "decide:plan"), ("p2", "decide:plan"), ("p3", "decide:plan"), ("q1", "decide:question"), ("q2", "decide:question")] {
        slice.task(task);
        drivers.push(slice.launch_until(task, prompt, waits_on_user));
    }
    slice.task("busy");
    drivers.push(slice.launch_until("busy", "consume-and-hold", works));
    let state = slice.domain.state().unwrap();
    for driver in &drivers[..5] {
        assert!(state.node_waits_on_user(driver.agent()), "the log lists it as waiting on the user");
    }
    assert_eq!(crate::admission::working_tasks(&slice.domain, &state), BTreeSet::from([TaskId::from("busy")]), "only busy works");
    let now = alondra_core::now_millis();
    assert_eq!(gate.snapshot(&slice.domain, now).unwrap().working, 1);
    assert_eq!(gate.live(&slice.domain, now).unwrap().working, 1, "the apps show the gate's count");

    slice.task("high");
    slice.run(&user(), "priority-high", Command::SetPriority { task_id: "high".into(), priority: TaskPriority::High });
    let high = Waiting { key: Key::Task("high".into()), priority: TaskPriority::High, resuming: false, since: now };
    assert_eq!(gate.ask(&slice.domain, high, now), Ok(()), "the queued High task starts");
    drivers.push(slice.launch_until("high", "consume-and-hold", works));

    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(2)) });
    let plan = slice.domain.state().unwrap().lifecycle_of(&"p1".into()).unwrap().cards()[0].0.clone();
    slice.answer(&plan.0, Hold::APPROVE, None).unwrap();
    let p1 = &mut drivers[0];
    for _ in 0..3 {
        p1.step(&|| false).unwrap();
    }
    assert!(!slice.decisions().iter().any(|decision| decision["response"]["behavior"] == "allow"), "the approval waits for a slot");
    slice.task("next");
    let next = Waiting { key: Key::Task("next".into()), priority: TaskPriority::High, resuming: false, since: now };
    assert_eq!(gate.ask(&slice.domain, next, now), Err(Held::Slots { working: 2, limit: 2 }), "the waiting approval takes no slot");
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(3)) });
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
        .block_on(crate::admission::pass_at(&slice.domain, slice.worktree.as_path(), now)).unwrap();
    let p1 = &mut drivers[0];
    while !slice.decisions().iter().any(|decision| decision["response"]["behavior"] == "allow") {
        p1.step(&|| false).unwrap();
    }
    for mut driver in drivers {
        driver.stop().unwrap();
    }
}

/// The answer to a decision lets a turn go on that took no slot while it waited on the user: the
/// answer waits for one with the turn held, an interrupt never does.
#[test]
fn an_approved_plan_reaches_claude_only_when_the_gate_has_a_slot() {
    wired();
    let slice = Slice::new();
    let (mut driver, card) = slice.asked("decide:plan");
    slice.domain.sdk_owners().driven_by_hand.lock().unwrap().insert(driver.agent().clone());
    slice.domain.admission().tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(0)) });
    slice.answer(&card, Hold::APPROVE, None).unwrap();
    for _ in 0..3 {
        driver.step(&|| true).unwrap();
    }
    assert!(slice.control_rows().iter().all(|(kind, _, status)| kind != "respond" || status == "prepared"), "{:?}", slice.control_rows());
    slice.domain.admission().tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(1)) });
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
        .block_on(crate::admission::pass_at(&slice.domain, slice.worktree.as_path(), alondra_core::now_millis())).unwrap();
    slice.rest(&mut driver);
    assert_eq!(only_response(&slice)["behavior"], "allow");
    driver.stop().unwrap();
}

/// 11 November, 00:21: a permission answered inside an open turn got no slot until 03:25, because
/// only an owner that asked waited in the queue, and the turn kept counting as working meanwhile.
/// The answer waits in the queue because the domain says so, takes no slot while it waits, and goes
/// on when one frees though nobody asked.
#[test]
fn an_answered_permission_goes_on_when_a_slot_frees_with_nobody_asking() {
    wired();
    let slice = Slice::new();
    let (mut driver, card) = slice.asked("decide:bash make test");
    slice.domain.sdk_owners().driven_by_hand.lock().unwrap().insert(driver.agent().clone());
    let gate = &slice.domain.admission();
    slots(&slice.domain, 0);
    slice.answer(&card, Hold::ALLOW, None).unwrap();
    gate_pass(&slice.domain, slice.worktree.as_path());
    let snapshot = gate.snapshot(&slice.domain, alondra_core::now_millis()).unwrap();
    assert!(!snapshot.working_tasks.contains(&TaskId::from(TASK)), "a turn whose answer has not reached it takes no slot: {:?}", snapshot.working_tasks);
    assert!(snapshot.queue.iter().any(|waiting| waiting.key == crate::ports::admission::Key::Turn(TASK.into())), "its answer waits for a slot, though its owner never asked");
    assert!(slice.decisions().is_empty());

    slots(&slice.domain, 1);
    gate_pass(&slice.domain, slice.worktree.as_path());
    assert!(gate.gave_slot(&TASK.into()), "the slot that freed went to the answer in that pass");
    assert_eq!(gate.snapshot(&slice.domain, alondra_core::now_millis()).unwrap().working, 1, "the slot given counts until the answer reaches its turn");
    slice.rest(&mut driver);
    assert_eq!(only_response(&slice)["behavior"], "allow");
    driver.stop().unwrap();
}

/// With a free slot, the pass that sees the answer gives it the slot: its owner need not ask first.
#[test]
fn an_answered_permission_with_room_goes_on_in_the_same_pass() {
    wired();
    let slice = Slice::new();
    let (mut driver, card) = slice.asked("decide:bash make test");
    slice.domain.sdk_owners().driven_by_hand.lock().unwrap().insert(driver.agent().clone());
    slots(&slice.domain, 1);
    slice.answer(&card, Hold::ALLOW, None).unwrap();
    gate_pass(&slice.domain, slice.worktree.as_path());
    assert!(slice.domain.admission().gave_slot(&TASK.into()), "the pass that saw the answer gave it a slot");
    slice.rest(&mut driver);
    assert_eq!(only_response(&slice)["behavior"], "allow");
    driver.stop().unwrap();
}

/// The owner of a turn the gate gave a slot ends before it starts it: the slot goes back.
#[test]
fn a_host_that_ends_with_a_slot_it_never_used_gives_it_back() {
    wired();
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let gate = &slice.domain.admission();
    gate.tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(0)) });
    slice.run(&Actor::System, "tell", Command::TellNode { task_id: TASK.into(), text: "Go on".into(), from_user: false, answers: None });
    assert!(!gate.may_take_turn(&slice.domain, &TASK.into(), alondra_core::now_millis()));
    gate.tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(1)) });
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
        .block_on(crate::admission::pass_at(&slice.domain, slice.worktree.as_path(), alondra_core::now_millis())).unwrap();
    assert_eq!(gate.snapshot(&slice.domain, alondra_core::now_millis()).unwrap().working, 1, "the slot is given and not taken yet");
    kill_host(&driver);
    drop(driver);
    assert_eq!(gate.snapshot(&slice.domain, alondra_core::now_millis()).unwrap().working, 0);
}

/// The orchestrator's answer to a node that stopped is one more input: it waits for a slot like any
/// turn, and reaches Claude as written once one frees.
#[test]
fn the_orchestrators_instruction_waits_for_a_slot_like_any_turn() {
    wired();
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let (case, _) = Slice::read_cards(&slice.domain).pop().unwrap();
    let gate = &slice.domain.admission();
    gate.tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(0)) });
    let state = slice.domain.state().unwrap();
    let settle = Command::SettleStall { case_id: case.as_str().into(), task_id: TASK.into(), outcome: StallOutcome::Instruct { text: "Add the tests first".into() } };
    let orchestrator = Actor::Orchestrator { generation: state.orchestrator_generation };
    slice.domain.execute_with(&orchestrator, &envelope(&state, "orchestrator-instructs", settle), None, None, || Ok(())).unwrap();
    for _ in 0..3 {
        driver.step(&|| false).unwrap();
    }
    assert_eq!(slice.prompts(), ["Write the release notes"], "nothing reaches Claude without a slot");
    gate.tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(1)) });
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
        .block_on(crate::admission::pass_at(&slice.domain, slice.worktree.as_path(), alondra_core::now_millis())).unwrap();
    slice.settle(&mut driver);
    assert_eq!(slice.prompts(), ["Write the release notes", "Add the tests first"]);
    driver.stop().unwrap();
}

#[test]
fn an_adopted_host_without_a_process_is_ended_at_adoption() {
    wired();
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let processes = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
    drop(driver);
    let restarted = slice.restart();
    let live = slice.take_over(&restarted);
    assert!(live.worker.host_group().is_some(), "an adopted host knows its process");
    drop(live);
    let restarted = slice.restart();
    ADOPTED_PROCESS_GONE.with(|gone| gone.set(true));
    let replaced = slice.take_over(&restarted);
    let journal = restarted.state().unwrap().lifecycle_journal[&TaskId::from(TASK)].clone();
    assert!(journal.iter().any(|record| matches!(record, alondra_domain_rules::LifecycleRecord::HostEnded { .. })), "{journal:?}");
    assert!(!crate::admission::working_tasks(&restarted, &restarted.state().unwrap()).contains(&TaskId::from(TASK)));
    assert!(processes.iter().all(|process| !alondra_process_group::alive(process).unwrap()), "nothing of the host is left running");
    kill_host_and_daemon(replaced);
}

/// An answer whose task has no owner in this daemon is not left waiting for one: the pass that
/// gives it a slot has a host take the task over, without waiting for the minute between takeovers
/// of tasks that read as working with no owner.
#[test]
fn an_answered_permission_of_a_task_without_an_owner_is_retaken() {
    wired();
    let slice = Slice::new();
    let (driver, card) = slice.asked("decide:bash make test");
    let agent = driver.agent().clone();
    drop(driver);
    slice.domain.enable_sdk(slice.backend.clone());
    slots(&slice.domain, 1);
    slice.answer(&card, Hold::ALLOW, None).unwrap();
    let revision = Slice::lifecycle(&slice.domain).revision();
    slice.domain.sdk_owners().unowned_takes.lock().unwrap().insert(TASK.into(), (alondra_core::now_millis(), revision));
    assert!(!slice.domain.owns_sdk_host(&agent));
    gate_pass(&slice.domain, slice.worktree.as_path());
    assert!(slice.domain.sdk_owners().retakes.lock().unwrap().iter().any(|(task, _)| *task == TaskId::from(TASK)), "the pass that gave the answer its slot had a host take its task over");
    slice.domain.stop_sdk_owners();
}

fn gate_pass(domain: &DomainRuntime, home: &std::path::Path) {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
        .block_on(crate::admission::pass_at(domain, home, alondra_core::now_millis())).unwrap();
}

fn slots(domain: &DomainRuntime, limit: u32) {
    domain.admission().tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(limit)) });
}

/// A node with a background command in a group of its own, a shell with a child of its own, under
/// an owner that runs by itself, as a daemon's are.
pub(super) fn node_with_a_background_group(slice: &Slice) -> (AgentId, String, BackgroundCommands, Vec<alondra_process_group::Member>) {
    slice.domain.enable_sdk(slice.backend.clone());
    slice.domain.admission().ignore_mac();
    slice.domain.admission().tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(6)) });
    let mut driver = slice.launch_with("background-group");
    until_own_work(slice, &mut driver);
    let commands = BackgroundCommands::of(slice);
    assert_eq!(commands.running().len(), 2, "the shell and its child run");
    let host = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
    let agent = driver.agent().clone();
    let session = slice.starts().last().unwrap()["session"].as_str().unwrap().to_owned();
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    (agent, session, commands, host)
}
