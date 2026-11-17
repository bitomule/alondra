//! The automatic launch's tests that need the gate itself: the order of its queue, its limit, the
//! user's pauses of a queued task and the disk's floor.
use super::*;
use alondra_daemon_engine::domain_orchestrator_tools::launch;
use alondra_daemon_engine::domain_store::AutoLaunchState;
use alondra_daemon_engine::auto_launch::*;
use alondra_daemon_engine::auto_launch::tests::*;

#[tokio::test]
async fn while_the_disk_is_nearly_full_a_ready_task_waits_and_starts_once_there_is_space() {
    wired();
    let f = world();
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    f.domain.machine_watch().set_disk_low(true);
    pass(&f.domain, &f.home).await.unwrap();
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 0);
    assert_eq!(auto(&f, "f1"), None, "not claimed: it starts by itself once there is space");
    assert!(told(&f).iter().all(|id| !id.starts_with("auto-launch")), "the orchestrator is not told: the user has the disk card");
    let queued = f.domain.admission().live(&f.domain, alondra_core::now_millis()).unwrap().queued;
    assert!(queued.iter().any(|queued| queued.task_id == "f1" && matches!(queued.hold, alondra_views::LiveHold::Disk { .. })),
        "it waits at the gate for room on the disk: {queued:?}");

    f.domain.machine_watch().set_disk_low(false);
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1);
    assert_eq!(auto(&f, "f1").unwrap().state, AutoLaunchState::Launched);
}

#[tokio::test]
async fn twelve_independent_tasks_with_a_plan_start_four_at_a_time_and_the_next_starts_when_one_closes() {
    wired();
    let f = world();
    f.domain.admission().tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(4)) });
    let ids: Vec<String> = (1..=12).map(|n| format!("i{n:02}")).collect();
    for id in &ids {
        create(&f, id, &[], Some(&plan(&f, &format!("app/{id}"))));
    }
    pass(&f.domain, &f.home).await.unwrap();
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 4, "the gate lets four start");
    let started: Vec<&String> = ids.iter().filter(|id| agent_of(&f, id).is_some()).collect();
    assert_eq!(started, ["i01", "i02", "i03", "i04"], "the oldest plans go first");
    assert!(told(&f).iter().all(|id| !id.starts_with("auto-launch")), "{:?}", told(&f));

    finish(&f, "i02", "What i02 delivered", false);
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 5, "a closed task frees its slot for the next");
    assert!(agent_of(&f, "i05").is_some());
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 5);
}

/// The orchestrator cancels a task after the gate let it through, while its start is under way: the
/// start did not fail, so nothing is tried again or put to the user, and the next task starts.
#[tokio::test]
async fn a_task_cancelled_while_it_starts_leaves_without_a_retry_or_a_card_and_the_next_one_starts() {
    wired();
    let f = world();
    f.domain.admission().tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(1)) });
    create(&f, "a", &[], Some(&plan(&f, "app/a")));
    create(&f, "b", &[], Some(&plan(&f, "app/b")));
    let now = alondra_core::now_millis();
    let waiting = crate::ports::admission::Waiting { key: crate::ports::admission::Key::Task("a".into()), priority: alondra_domain_rules::TaskPriority::Normal,
        resuming: false, since: 0 };
    assert_eq!(f.domain.gate().ask(waiting, now), Ok(()));
    assert!(f.domain.store.lock().unwrap().claim_auto_launch(&"a".into(), now, false).unwrap());
    assert!(f.domain.live().begin_launch("a"));
    let domain = f.domain.clone();
    let cancel: Box<dyn FnOnce() + Send> = Box::new(move || command(&domain, Actor::Orchestrator { generation: 0 }, Command::CancelTask { task_id: "a".into(), reason: None }));
    let launched = crate::domain_orchestrator_tools::launch_guarded(&f.domain, &f.home, LaunchBody::from_plan("a".into(), &plan(&f, "app/a")), |_| None,
        Origin::Auto, Some(cancel)).await;
    f.domain.live().end_launch("a");
    assert!(launched.as_ref().is_err_and(|error| error.message.contains("Task is terminal")), "cancelled before its admission: {launched:?}");
    settle(&f.domain, &"a".into(), f.project.to_str(), launched, now).unwrap();
    assert!(!f.domain.store.lock().unwrap().start_retries().unwrap().contains_key(&TaskId::from("a")), "no retry");
    assert!(problem(&f, "a").is_none(), "no card");
    assert!(auto(&f, "a").is_none());
    pass(&f.domain, &f.home).await.unwrap();
    pass(&f.domain, &f.home).await.unwrap();
    assert!(agent_of(&f, "b").is_some(), "the slot goes to the next task");
    assert!(agent_of(&f, "a").is_none());
}

/// The worktree a start makes is the task's from the moment it exists: a task cancelled while it
/// starts, whose start fails, leaves no worktree behind once it closes.
#[tokio::test]
async fn the_worktree_made_for_a_task_cancelled_while_it_starts_goes_when_it_closes() {
    wired();
    let f = world();
    for args in [&["init", "-q"][..], &["config", "user.email", "test@example.invalid"], &["config", "user.name", "Test"],
        &["commit", "-q", "--allow-empty", "-m", "base"]] {
        crate::worktree::testing::git(&f.project, args);
    }
    f.domain.admission().tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(1)) });
    create(&f, "a", &[], Some(&plan(&f, "app/a")));
    let now = alondra_core::now_millis();
    let waiting = crate::ports::admission::Waiting { key: crate::ports::admission::Key::Task("a".into()), priority: alondra_domain_rules::TaskPriority::Normal,
        resuming: false, since: 0 };
    assert_eq!(f.domain.gate().ask(waiting, now), Ok(()));
    assert!(f.domain.store.lock().unwrap().claim_auto_launch(&"a".into(), now, false).unwrap());
    assert!(f.domain.live().begin_launch("a"));
    let domain = f.domain.clone();
    let cancel: Box<dyn FnOnce() + Send> = Box::new(move || command(&domain, Actor::Orchestrator { generation: 0 }, Command::CancelTask { task_id: "a".into(), reason: None }));
    let launched = crate::domain_orchestrator_tools::launch_guarded(&f.domain, &f.home, LaunchBody::from_plan("a".into(), &plan(&f, "app/a")), |_| None,
        Origin::Auto, Some(cancel)).await;
    f.domain.live().end_launch("a");
    assert!(launched.is_err(), "cancelled before its admission: {launched:?}");
    settle(&f.domain, &"a".into(), f.project.to_str(), launched, now).unwrap();
    let worktree = f.project.join(".claude/worktrees/a");
    assert!(worktree.join(".git").is_file(), "the start made the task's worktree");
    crate::task_closer::pass(&f.domain, &NoSessions, &crate::task_closer::Timing::new(None), alondra_core::now_millis()).await.unwrap();
    assert!(!worktree.exists(), "the cancelled task's worktree goes with its close");
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("a")].state, alondra_domain_rules::TaskState::Cancelled);
}

/// 15 October: t-fastconn-5-remove-mailbox was cancelled at 19:42 and reopened at 19:45. It stayed
/// in progress with no node, nothing started it, and once launched by hand the queue took it out of
/// its queue before starting it and was refused («Task already has a node»), so it was in no queue
/// at all. Reopened, it goes back to the queue with its plan and its seniority; a start the gate
/// cannot make leaves it there, and its retry starts it.
#[tokio::test]
async fn a_reopened_task_waits_in_the_queue_with_its_seniority_and_a_refused_start_keeps_it_there() {
    wired();
    let f = world_using_sdk(SdkLaunches::enable);
    f.domain.admission().ignore_mac();
    create(&f, "f1", &[], Some(&plan(&f, "app/f1")));
    pass(&f.domain, &f.home).await.unwrap();
    f.sdk.until_answered("f1").await;
    let first = agent_of(&f, "f1").expect("its first node");
    let original = f.sdk.starts()[0]["session"].as_str().unwrap().to_owned();
    let original_account = f.domain.state().unwrap().tasks[&TaskId::from("f1")].account_key();
    let planned_at = f.domain.store.lock().unwrap().planned_tasks().unwrap()[&TaskId::from("f1")];

    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CancelTask { task_id: "f1".into(), reason: None });
    f.domain.stop_sdk_host(&first).unwrap();
    command(&f.domain, Actor::System, Command::RecordNodeEnd { task_id: "f1".into(), end: NodeEnd::Closed, agent_id: Some(first) });
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::ReopenTask { task_id: "f1".into() });
    let task = f.domain.state().unwrap().tasks[&TaskId::from("f1")].clone();
    assert_eq!((task.state, task.started, task.agent_id), (TaskState::NotStarted, false, None), "never in progress with no node");
    assert_eq!(queued(&f), [("f1".to_owned(), planned_at)], "queued with the place its plan gave it");
    assert!(auto(&f, "f1").is_none(), "its first automatic launch belongs to its first life");

    create(&f, "f2", &[], Some(&plan(&f, "app/f2")));
    let body: LaunchBody = serde_json::from_value(json!({"task": "f2", "name": "app/f2", "cwd": f.project.to_str().unwrap()})).unwrap();
    crate::domain_orchestrator_tools::enqueue(&f.domain, &body, "orchestrator", alondra_core::now_millis()).unwrap();
    let now = alondra_core::now_millis();
    let snapshot = f.domain.admission().snapshot(&f.domain, now).unwrap();
    let order: Vec<String> = snapshot.queue.iter().map(|waiting| format!("{:?}", waiting.key)).collect();
    assert_eq!(order[0], "Task(TaskId(\"f1\"))", "reopened, it keeps its place ahead of later work: {order:?}");
    assert_eq!(snapshot.working, 0, "back in the queue, its first start no longer counts as one under way");

    f.sdk.refuse_starts(true);
    crate::admission::pass_at(&f.domain, &f.home, now).await.unwrap();
    assert!(agent_of(&f, "f1").is_none());
    assert!(queued(&f).iter().any(|(task, _)| task == "f1"), "a refused start never takes it out of the queue: {:?}", queued(&f));
    let retry = f.domain.store.lock().unwrap().start_retries().unwrap()[&TaskId::from("f1")];
    assert!(retry.1 > now, "it waits for its retry");
    let order: Vec<String> = f.domain.admission().snapshot(&f.domain, now).unwrap().queue.iter()
        .map(|waiting| format!("{:?}", waiting.key)).collect();
    assert_eq!(order, ["Task(TaskId(\"f2\"))"], "while it waits for its retry it holds nobody back");

    f.sdk.refuse_starts(false);
    crate::admission::pass_at(&f.domain, &f.home, retry.1).await.unwrap();
    assert!(agent_of(&f, "f1").is_some(), "its retry starts it");
    f.sdk.until_turns_ended("f1", 2).await;
    let starts = f.sdk.starts();
    let resumed = starts.iter().find(|start| start["resume"].as_str() == Some(original.as_str())).expect("the reopened task resumes its original native session");
    assert!(resumed["history"].as_u64().unwrap() > 0, "the reopened task retains its native history: {starts:?}");
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("f1")].account_key(), original_account);
    assert!(!queued(&f).iter().any(|(task, _)| task == "f1"));
    assert!(f.domain.store.lock().unwrap().start_retries().unwrap().is_empty());
}

#[tokio::test]
async fn pausing_a_task_waiting_for_its_automatic_start_takes_it_out_of_the_queue_until_the_user_resumes_it() {
    wired();
    let f = world();
    f.domain.admission().tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(1)) });
    create(&f, "q1", &[], Some(&plan(&f, "app/q1")));
    create(&f, "q2", &[], Some(&plan(&f, "app/q2")));
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1);
    assert_eq!(queued_ids(&f), [("q2".to_owned(), true)], "q2 waits for its automatic start, and the user may pause it");

    crate::admission::hold_queued(&f.domain, &"q2".into(), alondra_core::now_millis()).unwrap();
    assert!(queued_ids(&f).is_empty());
    assert_eq!(paused_ids(&f), ["q2"]);

    finish(&f, "q1", "What q1 delivered", false);
    pass(&f.domain, &f.home).await.unwrap();
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1, "a slot is free, but only the user brings q2 back");
    assert_eq!(f.domain.admission().snapshot(&f.domain, alondra_core::now_millis()).unwrap().working, 0, "q2 takes no slot while paused");
    let body: LaunchBody = serde_json::from_value(json!({"task": "q2"})).unwrap();
    let refused = launch(&f.domain, &f.home, body, |_| None).await.unwrap_err();
    assert_eq!(refused.code, "conflict");
    assert!(refused.message.contains("The user paused"), "{}", refused.message);

    crate::admission::release_queued(&f.domain, &"q2".into()).unwrap();
    assert!(paused_ids(&f).is_empty());
    crate::admission::pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 2, "the slot q1 left is free: the gate starts it");
    assert!(agent_of(&f, "q2").is_some());
    assert!(paused_ids(&f).is_empty());
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 2, "the resumed task is not started twice");
}

#[tokio::test]
async fn pausing_a_launch_the_orchestrator_queued_takes_it_out_of_the_queue_until_the_user_resumes_it_and_then_it_waits_for_a_slot() {
    wired();
    let f = world();
    f.domain.admission().tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(1)) });
    create(&f, "o1", &[], Some(&plan(&f, "app/o1")));
    create(&f, "o2", &[], Some(&plan(&f, "app/o2")));
    let body: LaunchBody = serde_json::from_value(json!({"task": "o1"})).unwrap();
    launch(&f.domain, &f.home, body, |_| None).await.unwrap();
    let body: LaunchBody = serde_json::from_value(json!({"task": "o2"})).unwrap();
    let queued = launch(&f.domain, &f.home, body, |_| None).await.unwrap();
    assert_eq!(queued["queued"], true);
    assert_eq!(f.domain.store.lock().unwrap().launch_queue().unwrap().len(), 1);

    crate::admission::hold_queued(&f.domain, &"o2".into(), alondra_core::now_millis()).unwrap();
    assert!(f.domain.store.lock().unwrap().launch_queue().unwrap().is_empty());
    assert_eq!(paused_ids(&f), ["o2"]);

    crate::admission::pass(&f.domain, &f.home).await.unwrap();
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1, "neither the queue nor the automatic start brings o2 back");

    crate::admission::release_queued(&f.domain, &"o2".into()).unwrap();
    assert!(paused_ids(&f).is_empty());
    crate::admission::pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1, "o1 holds the one slot: o2 waits in the queue");
    assert_eq!(f.domain.store.lock().unwrap().launch_queue().unwrap().len(), 1);

    finish(&f, "o1", "What o1 delivered", false);
    crate::admission::pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 2, "the slot o1 left goes to o2");
    assert!(agent_of(&f, "o2").is_some());
}

#[tokio::test]
async fn pausing_a_task_that_waits_for_nothing_says_it_is_not_queued() {
    wired();
    let f = world();
    done_task(&f, "n0");
    open_task(&f, "n1");
    assert_eq!(crate::admission::hold_queued(&f.domain, &"n1".into(), alondra_core::now_millis()), Err("This task is not queued"));
    assert_eq!(crate::admission::hold_queued(&f.domain, &"n0".into(), alondra_core::now_millis()), Err("This task is not queued"));
}

fn paused_ids(f: &Fixture) -> Vec<String> {
    let pacing = f.domain.admission().live(&f.domain, alondra_core::now_millis()).unwrap();
    pacing.paused.into_iter().map(|paused| paused.task_id).collect()
}

fn queued_ids(f: &Fixture) -> Vec<(String, bool)> {
    let pacing = f.domain.admission().live(&f.domain, alondra_core::now_millis()).unwrap();
    pacing.queued.into_iter().map(|queued| (queued.task_id, queued.pausable)).collect()
}
