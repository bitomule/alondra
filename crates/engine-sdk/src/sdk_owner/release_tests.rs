//! A node that rests holds no process, its session goes on when its task has something to do, and a
//! node with anything of the task in its host keeps it.
use crate::domain_runtime::DomainRuntime;
use crate::gate::GateOf as _;
use crate::sdk_owner::SdkOwners as _;
use crate::live::LiveOf as _;
use crate::contexts::ContextsOf as _;
use alondra_engine_admission::AdmissionOf as _;
use super::codex_tests::{on_branch, request_move, resumed_threads, until_moved};
use crate::sdk_owner::fixtures::codex::{configured, launch, transcript};
use super::*;
use crate::idle_release::{Rests, Timing, pass};

const WAIT: Timing = Timing { wait: 1_000 };

fn enable(slice: &Slice) {
    slice.domain.enable_sdk(slice.backend.clone());
    slice.domain.admission().ignore_mac();
    slice.domain.admission().tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(6)) });
}

/// Two looks a wait apart: the second finds every resting host due.
fn look_after_the_wait(domain: &DomainRuntime) {
    let mut rests = Rests::default();
    let now = alondra_core::now_millis();
    pass(domain, &mut rests, now, now, &WAIT);
    pass(domain, &mut rests, now + WAIT.wait, now + WAIT.wait, &WAIT);
}

/// The daemon's looks until every task's host closed. An owner that finds its node still settling
/// when it is asked refuses and forgets the request, so a look that asked once is not enough: the
/// daemon asks again at its next look, and so does this.
async fn close_resting_hosts(domain: &DomainRuntime, tasks: &[&str]) {
    let released = || tasks.iter().all(|task| domain.read().state().lifecycle_of(&TaskId::from(*task)).is_ok_and(|lifecycle| lifecycle.host_released()));
    while !released() {
        look_after_the_wait(domain);
        tokio::task::yield_now().await;
    }
    until_released(domain, tasks).await;
}

async fn until_released(domain: &DomainRuntime, tasks: &[&str]) {
    let mut changed = domain.subscribe();
    let released = |domain: &DomainRuntime| {
        let read = domain.read();
        tasks.iter().all(|task| read.state().lifecycle_of(&TaskId::from(*task)).is_ok_and(|lifecycle| lifecycle.host_released()))
    };
    let arrived = changed.wait_for(|_| released(domain));
    arrived.await.unwrap();
    let ended = async { while !domain.node_groups().is_empty() { tokio::task::yield_now().await } };
    ended.await;
    // An owner reads as ended once its host closed, but its driver keeps the task's execution and
    // slot until its thread lets go of them: a look of the gate before that finds the task in hand
    // and takes nothing over, and the owner's end then withdraws the slot the look granted.
    let in_hand = || tasks.iter().any(|task| domain.sdk_owners().in_hand.lock_or_recover("the executions in hand").contains_key(&TaskId::from(*task)));
    let let_go = async { while in_hand() { tokio::task::yield_now().await } };
    let_go.await;
}

/// The daemon's own looks, one a wait after the other, from the takeover of a closed node until its
/// host closes again. An owner asked while it still settles what took its node over (a control it
/// applied, an input not yet acknowledged) declines, and the daemon asks again at its next look: a
/// single look finds that owner busy, closes nothing, and the wait for the release never ends.
async fn look_until_released(domain: &DomainRuntime, task: &str) {
    let released = |domain: &DomainRuntime| domain.read().state().lifecycle_of(&TaskId::from(task)).is_ok_and(|lifecycle| lifecycle.host_released());
    let mut changed = domain.subscribe();
    let taken_over = changed.wait_for(|_| !released(domain));
    taken_over.await.unwrap();
    let mut rests = Rests::default();
    let mut now = alondra_core::now_millis();
    loop {
        pass(domain, &mut rests, now, now, &WAIT);
        now += WAIT.wait;
        if released(domain) {
            break;
        }
        tokio::task::yield_now().await;
    }
    until_released(domain, &[task]).await;
}

#[cfg(not(feature = "real-hosts"))]
async fn until_working(slice: &Slice, count: usize) {
    let mut changed = slice.domain.subscribe();
    let reached = changed.wait_for(|_| crate::admission::working_tasks(&*slice.domain, &slice.domain.read().state()).len() == count);
    reached.await.unwrap();
}

/// The gate's look, which gives the turns that wait a slot and takes over the tasks with no host.
async fn gate_look(slice: &Slice) {
    crate::admission::pass_at(&*slice.domain, &slice._home, alondra_core::now_millis()).await.unwrap();
    slice.domain.join_retakes();
}

/// Tells `task` something, and takes the gate's look that lets its turn go on.
async fn tell(slice: &Slice, task: &str, text: &str) {
    crate::domain_messages::tell_sdk_node(&*slice.domain, &task.into(), crate::node_push::ALONDRA, text, &format!("tell-{task}-{text}"), false).unwrap();
    gate_look(slice).await;
}

#[cfg(not(feature = "real-hosts"))]
/// A node that ended its turn and rests, under an owner that runs by itself.
fn resting_node(slice: &Slice) -> (String, HostWatch) {
    enable(slice);
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    assert_eq!(lifecycle(&*slice.domain).activity(), ActivityView::Resting);
    let host = slice.hosts.watch(driver.worker.host_group().unwrap());
    let session = slice.starts().last().unwrap()["session"].as_str().unwrap().to_owned();
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    (session, host)
}

#[cfg(not(feature = "real-hosts"))]
fn launch_task(slice: &Slice, task: &str) -> Driver {
    slice.run(&Actor::Orchestrator { generation: 0 }, &format!("task-{task}"), Command::CreateTask { priority: Default::default(),
        plan_items: None, continues: None,
        task_id: task.into(),
        request_id: "sdk-slice-request".into(),
        title: format!("Work on {task}"),
        agent_id: None,
        review_prs: false,
        dependencies: BTreeSet::new(),
        stack: None,
        launch: Some(LaunchPlan::new("node", slice.worktree.to_str().unwrap())),
    });
    start_bound(slice.domain.clone(), &slice.backend, Launch {
        task: task.into(),
        name: task.into(),
        directory: slice.worktree.to_str().unwrap().into(),
        prompt: "Write the release notes".into(),
        plugin: slice.plugin.clone(),
        options: LaunchOptions::default(),
        model: None,
        account: AccountId::default_account(),
    }, capabilities(), None, None).unwrap()
}

// 7 November, 01:15: 39 `claude` and 35 `alondra-host` lived with 2 nodes at work, and the gate held
// every start for the swap they caused.
#[cfg(not(feature = "real-hosts"))]
#[tokio::test(flavor = "multi_thread")]
async fn nodes_that_wait_keep_no_process_and_a_restarted_daemon_starts_none() {
    let slice = Slice::new();
    enable(&slice);
    let tasks = ["t-rests-one", "t-rests-two", "t-rests-three"];
    let mut hosts = vec![];
    for task in tasks {
        let mut driver = launch_task(&slice, task);
        slice.settle(&mut driver);
        hosts.push(slice.hosts.watch(driver.worker.host_group().unwrap()));
        slice.domain.add_sdk_owner(Owner::spawn(driver));
    }
    let started = slice.starts().len();
    assert!(hosts.iter().all(HostWatch::runs), "the hosts and their Claudes run while the nodes rest");

    close_resting_hosts(&slice.domain, &tasks).await;

    assert!(!hosts.iter().any(HostWatch::runs), "no process is left of any node that waits");
    assert!(open_card(&slice.domain, SESSION_ENDED_PREFIX).is_none(), "no card asks the user");
    slice.domain.stop_sdk_owners();
    let restarted = slice.restart();
    restarted.enable_sdk(slice.backend.clone());
    adopt_all(&(restarted.clone() as std::sync::Arc<dyn alondra_engine_ports::Runtime>), &slice.backend);
    assert_eq!(slice.starts().len(), started, "a daemon that comes back starts no Claude for them");
    assert_eq!(restarted.node_groups(), Vec::<i32>::new(), "and no host");
}

// 7 November, 07:45: 39 nodes worked under a limit of 8, since a resume took no slot, and the memory
// ran out. A closed node with work waits its turn for a slot, as a state with no card.

#[cfg(not(feature = "real-hosts"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_node_rests_with_its_host_until_the_wait_passes() {
    let slice = Slice::new();
    let (_, host) = resting_node(&slice);
    let mut rests = Rests::default();
    let now = alondra_core::now_millis();
    pass(&*slice.domain, &mut rests, now, now, &WAIT);
    pass(&*slice.domain, &mut rests, now + WAIT.wait - 1, now + WAIT.wait - 1, &WAIT);
    assert!(!lifecycle(&*slice.domain).host_released());
    assert!(host.runs());
    slice.domain.stop_sdk_owners();
}

#[cfg(not(feature = "real-hosts"))]
#[tokio::test(flavor = "multi_thread")]
async fn work_for_a_closed_node_resumes_its_session_with_no_card_and_no_turn_of_its_own() {
    let slice = Slice::new();
    let (session, host) = resting_node(&slice);
    let told_before = slice.prompts().len();
    close_resting_hosts(&slice.domain, &[TASK]).await;
    assert!(!host.runs());

    tell(&slice, TASK, "Now the changelog.").await;
    until_last_prompt_ends_with(&slice.domain, &slice.hosts, "Now the changelog.").await;

    let (starts, prompts) = (slice.starts(), slice.prompts());
    slice.domain.stop_sdk_owners();
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert_eq!(starts.last().unwrap()["resume"].as_str(), Some(session.as_str()), "the same session goes on: {starts:?}");
    assert_eq!(prompts.len(), told_before + 1, "only what it was told reaches it: {prompts:?}");
    assert!(open_card(&slice.domain, SESSION_ENDED_PREFIX).is_none(), "no card asks the user");
    assert!(!lifecycle(&*slice.domain).host_released());
    assert_eq!(archived_hosts(&slice), Vec::<String>::new(), "a closed host keeps no folder: it told everything before closing");
}

#[cfg(not(feature = "real-hosts"))]
fn archived_hosts(slice: &Slice) -> Vec<String> {
    std::fs::read_dir(&slice.backend.state_root).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".ended-")).collect()
}

#[cfg(not(feature = "real-hosts"))]
// 15 November: three tasks had just finished (a design accepted, a pull request merged) while their
// hosts were closed. Telling the node to clean up took its execution over, and the closer of the
// task took it over too: the second takeover started a host while the first had reserved its owner,
// the first found that host live and refused, stopped it, could not start its own, and the user got
// «stopped: Alondra could not resume its session». One takeover of a task runs at a time: the other
// finds the task owned and the session goes on.
#[tokio::test(flavor = "multi_thread")]
async fn a_finished_task_whose_closed_host_is_taken_over_twice_at_once_resumes_once_and_asks_nobody() {
    use std::sync::mpsc::channel;
    let slice = Slice::new();
    let (session, _) = resting_node(&slice);
    close_resting_hosts(&slice.domain, &[TASK]).await;
    let agent = slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].agent_id.clone().unwrap();
    let (reserved, first_reserved) = channel();
    let (go, gone) = channel::<()>();
    slice.domain.after_owner_reserved(move || {
        reserved.send(()).unwrap();
        gone.recv().unwrap();
    });
    let (settled, closer_settled) = channel();
    let waits = settled.clone();
    slice.domain.on_takeover_wait(move || drop(waits.send("waits")));
    deliver_and_accept(&slice, &agent, vec![DeliveryTarget::Text { text: "The notes".into() }]);
    crate::domain_messages::tell_sdk_node(&*slice.domain, &TASK.into(), crate::node_push::ALONDRA, "Clean up and call cleanup_done.", "tell-cleanup", false).unwrap();
    tokio::task::block_in_place(|| first_reserved.recv()).expect("the first takeover never reserved its owner");
    let handle = tokio::runtime::Handle::current();
    std::thread::scope(|scope| {
        let closer = scope.spawn(|| {
            handle.block_on(close_pass(&slice));
            drop(settled.send("closed"));
        });
        tokio::task::block_in_place(|| closer_settled.recv()).expect("the closer neither took over nor waited");
        go.send(()).unwrap();
        closer.join().unwrap();
    });
    slice.domain.join_retakes();

    let lost = open_card(&slice.domain, alondra_domain_rules::HOST_LOST_PREFIX);
    assert!(lost.is_none(), "no card tells the user the session could not be resumed: {lost:?}");
    assert!(slice.domain.owns_sdk_host(&agent), "one host runs the finished task so it can clean up");
    until_last_prompt_ends_with(&slice.domain, &slice.hosts, "Clean up and call cleanup_done.").await;
    let starts = slice.starts();
    slice.domain.stop_sdk_owners();
    assert_eq!(starts.len(), 2, "one host resumed the session: {starts:?}");
    assert_eq!(starts[1]["resume"].as_str(), Some(session.as_str()), "the same session goes on: {starts:?}");
}

// No host to stop: the task ends as one whose host ended, and no Claude starts just to end it.
#[cfg(not(feature = "real-hosts"))]
#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_closed_node_ends_the_task_without_starting_anything() {
    let slice = Slice::new();
    resting_node(&slice);
    close_resting_hosts(&slice.domain, &[TASK]).await;
    slice.domain.join_retakes();
    slice.run(&user(), "cancel-closed-node", Command::CancelTask { task_id: TASK.into(), reason: None });
    crate::task_closer::pass(&*slice.domain, &NoClaudeSessions, &crate::task_closer::Timing::new(None), alondra_core::now_millis()).await.unwrap();
    let state = slice.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from(TASK)].state, TaskState::Cancelled);
    assert_eq!(slice.starts().len(), 1, "no Claude starts for a task that ends");
    assert!(open_card(&slice.domain, SESSION_ENDED_PREFIX).is_none(), "no card asks the user");
    assert_eq!(slice.domain.node_groups(), Vec::<i32>::new());
}

#[cfg(not(feature = "real-hosts"))]
#[tokio::test]
async fn a_closed_node_moves_to_another_provider_like_a_resting_one() {
    use crate::domain_store::provider_transition::Phase;
    let slice = Slice::new();
    let task: TaskId = TASK.into();
    configured(&slice, "complete");
    claude_ready(&slice);
    on_branch(&slice.worktree, "release-notes");
    enable(&slice);
    let mut driver = launch(&slice);
    slice.settle(&mut driver);
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    close_resting_hosts(&slice.domain, &[TASK]).await;
    request_move(&slice);
    let (pulse, mut changed) = tokio::sync::watch::channel(0_u64);
    slice.domain.before_owner_publish(move || pulse.send_modify(|count| *count += 1));
    let phase = crate::provider_transition::advance(&(slice.domain.clone() as std::sync::Arc<dyn alondra_engine_ports::Runtime>), &task).await.unwrap();
    assert!(matches!(phase, Phase::TargetAdmitted | Phase::Done), "{phase:?}");
    until_moved(&slice, &mut changed).await;
    let state = slice.domain.state().unwrap();
    assert!(state.tasks[&task].pending_selection.is_none(), "it moved at once, as a resting node does");
    assert_eq!(state.lifecycle_journal[&task].iter().filter(|record| matches!(record, LifecycleRecord::Admitted { .. })).count(), 1, "no node starts until something resumes it");
    slice.domain.stop_sdk_owners();
}

#[cfg(not(feature = "real-hosts"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_host_whose_owner_refused_the_first_request_to_close_closes_at_the_next_look() {
    let slice = Slice::new();
    enable(&slice);
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    driver.declines_next_release = true;
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    close_resting_hosts(&slice.domain, &[TASK]).await;
    assert!(lifecycle(&*slice.domain).host_released());
    slice.domain.stop_sdk_owners();
}

#[cfg(not(feature = "real-hosts"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_model_chosen_for_a_closed_node_reaches_it_and_stays_when_it_closes_again() {
    let slice = Slice::new();
    resting_node(&slice);
    close_resting_hosts(&slice.domain, &[TASK]).await;

    let state = slice.domain.state().unwrap();
    slice.domain.execute_as_system(envelope(&state, "model-of-a-closed-node", Command::SetModel { task_id: TASK.into(), model: "sonnet".into() })).unwrap();
    let mut changed = slice.domain.subscribe();
    let applied = changed.wait_for(|_| slice.control_rows().iter().any(|(kind, _, status)| kind == "set_model" && status != "requested"));
    applied.await.unwrap();
    assert!(slice.domain.read().state().last_model_asked(&TASK.into()).is_some_and(|model| model == "sonnet"));
    look_until_released(&slice.domain, TASK).await;
    tell(&slice, TASK, "Go on.").await;
    until_last_prompt_ends_with(&slice.domain, &slice.hosts, "Go on.").await;
    slice.domain.stop_sdk_owners();
    let capture: serde_json::Value = slice.hosts.native_capture();
    let args: Vec<&str> = capture["args"].as_array().unwrap().iter().filter_map(serde_json::Value::as_str).collect();
    assert!(args.contains(&"--model=sonnet") || args.windows(2).any(|pair| pair == ["--model", "sonnet"]), "the host that resumes opens with the model chosen: {capture}");
}

// The input committed after the owner decided, while its host ended: the release is refused, and
// the task is taken over as after any host that ended.
#[cfg(not(feature = "real-hosts"))]
#[test]
fn an_input_that_arrives_while_the_host_closes_reaches_the_host_that_replaces_it() {
    let slice = Slice::new();
    enable(&slice);
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    while !driver.settle_release().unwrap() {}
    assert!(driver.releases, "the node rests with nothing in its host");
    crate::domain_messages::tell_sdk_node(&*slice.domain, &TASK.into(), crate::node_push::ALONDRA, "Just in time.", "tell-while-closing", false).unwrap();
    assert!(driver.release_host().is_err(), "a task that moved on does not stay without a host");
    assert!(!lifecycle(&*slice.domain).host_released());
    driver.releases = false;
    driver.host_closed = true;
    let mut replacement = driver.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    slice.settle(&mut replacement);
    assert!(slice.prompts().last().unwrap().ends_with("Just in time."));
    replacement.stop().unwrap();
}

#[cfg(not(feature = "real-hosts"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_node_whose_own_work_runs_after_its_turn_keeps_its_host() {
    let slice = Slice::new();
    let (_, _, commands, host) = node_with_a_background_group(&slice);
    look_after_the_wait(&slice.domain);
    assert!(!slice.domain.read().state().host_releasable(&TASK.into()));
    slice.domain.stop_sdk_owners();
    assert_eq!(slice.starts().len(), 1);
    assert!(!lifecycle(&*slice.domain).host_released());
    drop((commands, host));
}

#[cfg(not(feature = "real-hosts"))]
#[test]
fn a_node_in_plan_mode_keeps_its_host_since_a_resumed_session_opens_out_of_it() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    assert!(slice.domain.read().state().host_releasable(&TASK.into()));
    slice.run(&system(), "plan-mode", Command::SetPermissionMode { task_id: TASK.into(), mode: PermissionModeRecord::Plan });
    assert!(!slice.domain.read().state().host_releasable(&TASK.into()));
    driver.stop().unwrap();
}

#[cfg(not(feature = "real-hosts"))]
#[test]
fn a_node_with_work_owed_after_its_merge_keeps_its_host() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let agent = driver.agent().clone();
    slice.run(&Actor::Agent { agent_id: agent.clone() }, "post-merge", Command::DeclarePostMergeWork { task_id: TASK.into(), reason: Some("Publishing".into()), waits_for: vec![] });
    assert!(!slice.domain.read().state().host_releasable(&TASK.into()));
    driver.stop().unwrap();
}

#[cfg(not(feature = "real-hosts"))]
// 24 November: two nodes had declared work after their merge and rested on a delivery still in
// review. They never closed their host, so an update ended it as a session cut mid-way and a card
// asked the user how each went on. Nothing runs after a merge that has not happened.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_resting_on_a_delivery_in_review_with_work_declared_after_the_merge_closes_its_host_without_asking() {
    let slice = Slice::new();
    enable(&slice);
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let agent = driver.agent().clone();
    slice.run(&Actor::Agent { agent_id: agent.clone() }, "post-merge", Command::DeclarePostMergeWork { task_id: TASK.into(), reason: Some("Publishing".into()), waits_for: vec![] });
    let targets = vec![DeliveryTarget::Text { text: "The notes".into() }];
    slice.run(&Actor::Agent { agent_id: agent }, "deliver-notes", Command::PresentResult {
        checklist: vec![], attachments: vec![], task_id: TASK.into(), interaction_id: "notes".into(), body: "The notes are written.".into(),
        targets: targets.clone(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the notes", &targets, true),
    });
    assert!(slice.domain.read().state().host_releasable(&TASK.into()), "{:?}", lifecycle(&*slice.domain));
    let host = slice.hosts.watch(driver.worker.host_group().unwrap());
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    close_resting_hosts(&slice.domain, &[TASK]).await;
    assert!(!host.runs());
    slice.domain.stop_sdk_owners();
    assert!(open_card(&slice.domain, SESSION_ENDED_PREFIX).is_none(), "no card asks the user");
}

#[cfg(not(feature = "real-hosts"))]
#[test]
fn a_node_resting_on_a_pause_closes_its_host_like_any_resting_node() {
    let slice = Slice::new();
    enable(&slice);
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let agent = driver.agent().clone();
    assert!(slice.domain.gate().hold_back(&agent.0, alondra_engine_ports::admission::Reason::Disk, alondra_core::now_millis()));
    assert!(slice.domain.read().state().host_releasable(&TASK.into()));
    driver.stop().unwrap();
}

#[cfg(not(feature = "real-hosts"))]
// 21 November: 75 nodes the user paused kept their hosts for days with nothing in them, 3.8 GB in
// all, since a paused node never closed its host. What it goes on with waits in its task for the
// pause's end, so its host closes as a resting node's does, and resuming it resumes the same
// session in a new host, told what it goes on with.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_the_user_paused_closes_its_host_and_its_resume_goes_on_with_the_same_session() {
    let slice = Slice::new();
    let (session, host) = resting_node(&slice);
    let agent = slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].agent_id.clone().unwrap();
    slice.domain.admission().pause_by_user(&*slice.domain, &agent.0, alondra_core::now_millis()).unwrap();
    slice.domain.join_retakes();
    assert!(slice.domain.read().state().host_releasable(&TASK.into()), "nothing of the task is in its host: {:?}", lifecycle(&*slice.domain));

    close_resting_hosts(&slice.domain, &[TASK]).await;
    assert!(!host.runs());

    assert!(slice.domain.admission().resume_now(&*slice.domain, &agent.0, alondra_core::now_millis()));
    gate_look(&slice).await;
    gate_look(&slice).await;
    until_last_prompt_ends_with(&slice.domain, &slice.hosts, "run again what was cut.").await;
    let starts = slice.starts();
    slice.domain.stop_sdk_owners();
    assert_eq!(starts.last().unwrap()["resume"].as_str(), Some(session.as_str()), "the same session goes on: {starts:?}");
    assert!(open_card(&slice.domain, SESSION_ENDED_PREFIX).is_none(), "no card asks the user");
    assert!(!lifecycle(&*slice.domain).host_released());
}

#[cfg(not(feature = "real-hosts"))]
// An owner that reads as ended has let go of its task already: a resume that finds it so takes the
// task over at once, however long the owner's thread takes to finish dropping its driver. Under load
// the resume of a paused node found the task still in the dead owner's hand, took nothing over, and
// its message never reached Claude.
#[tokio::test(flavor = "multi_thread")]
async fn an_owner_that_reads_as_ended_no_longer_holds_its_task() {
    let slice = Slice::new();
    enable(&slice);
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let (reached, at_the_end) = std::sync::mpsc::channel();
    let (finish, finishing) = std::sync::mpsc::channel::<()>();
    driver.before_release_ends = Some(Box::new(move || {
        reached.send(()).unwrap();
        let _ = finishing.recv();
    }));
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    look_after_the_wait(&slice.domain);
    at_the_end.recv().expect("the owner never closed its host");
    let state = slice.domain.state().unwrap();
    let task = TaskId::from(TASK);
    let held_by_the_ended_owner = !crate::sdk_owner::unowned(&*slice.domain, &state, &task);
    finish.send(()).unwrap();
    until_released(&slice.domain, &[TASK]).await;
    slice.domain.stop_sdk_owners();
    assert!(!held_by_the_ended_owner, "the owner reads as ended while its task is still in its hand");
}

#[cfg(not(feature = "real-hosts"))]
// 22 November: 65 hosts of releases before 0.3.65 stayed alive for days, each one of a node the user
// had paused. Every daemon start adopted it again, since it rests on a host of the release before, and
// the pause kept it from ever closing. A node paused on such a host must end up with no process, as
// one paused on a host of this release does.
#[tokio::test(flavor = "multi_thread")]
async fn a_paused_node_adopted_on_a_host_of_the_release_before_gives_up_that_host() {
    let slice = Slice::new();
    enable(&slice);
    let before = slice.release_before();
    let mut driver = slice.launch_with_backend(&before, "Write the release notes", capabilities());
    slice.settle(&mut driver);
    let agent = driver.agent().clone();
    let old_host = slice.hosts.watch(driver.worker.host_group().unwrap());
    slice.domain.admission().pause_by_user(&*slice.domain, &agent.0, alondra_core::now_millis()).unwrap();
    drop(driver);
    slice.domain.stop_sdk_owners();

    let restarted = slice.restart();
    restarted.enable_sdk(slice.backend.clone());
    // The adoption `adopt_all` makes, with the owner spawned after the check: spawned at once, it
    // gives the host up before a loaded Mac reads whether it was adopted.
    let runtime = restarted.clone() as std::sync::Arc<dyn alondra_engine_ports::Runtime>;
    let taken = take_all(&runtime, &slice.backend);
    assert_eq!(taken.len(), 1, "the paused node is the only one to take over");
    assert!(old_host.runs(), "the daemon adopted the host of the release before");
    for (_, driver) in taken {
        restarted.add_sdk_owner(Owner::spawn(driver.expect("the paused node is adopted on the host it had")));
    }

    look_after_the_wait(&restarted);
    let mut changed = restarted.subscribe();
    let gone = || !old_host.runs();
    changed.wait_for(|_| gone()).await.unwrap();
    restarted.stop_sdk_owners();
}

#[cfg(not(feature = "real-hosts"))]
// Codex: its thread resumes through `thread/resume`, as Claude's session through `--resume`.
#[tokio::test(flavor = "multi_thread")]
async fn a_closed_codex_node_resumes_the_same_thread() {
    let slice = Slice::new();
    let trace = configured(&slice, "complete");
    enable(&slice);
    let mut driver = launch(&slice);
    slice.settle(&mut driver);
    let host = slice.hosts.watch(driver.worker.host_group().unwrap());
    assert!(transcript(&trace).iter().any(|message| message["method"] == "thread/start"), "the node opened its thread");
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    close_resting_hosts(&slice.domain, &[TASK]).await;
    assert!(!host.runs(), "neither its host nor its app-server is left");

    tell(&slice, TASK, "Now the changelog.").await;
    let mut changed = slice.domain.subscribe();
    let resumed = changed.wait_for(|_| !resumed_threads(&trace).is_empty() && lifecycle(&*slice.domain).pending_inputs().is_empty());
    resumed.await.unwrap();
    slice.domain.stop_sdk_owners();
    assert_eq!(resumed_threads(&trace).len(), 1);
    assert!(open_card(&slice.domain, SESSION_ENDED_PREFIX).is_none(), "no card asks the user");
}

#[cfg(not(feature = "real-hosts"))]
// Codex reports its own work as a child agent or as a command with a process: neither closes.
#[test]
fn a_codex_node_with_a_child_agent_keeps_its_host() {
    let slice = Slice::new();
    configured(&slice, "child");
    let mut driver = launch(&slice);
    while lifecycle(&*slice.domain).activity() != ActivityView::OwnWork {
        advance(&mut driver);
    }
    assert!(!slice.domain.read().state().host_releasable(&TASK.into()));
    while !driver.settle_release().unwrap() {}
    assert!(!driver.releases);
    driver.stop().unwrap();
}

#[cfg(not(feature = "real-hosts"))]
#[test]
fn a_codex_node_with_a_command_running_after_its_turn_keeps_its_host() {
    keeps_its_host_while_its_command_runs("blocked_command");
}

#[cfg(not(feature = "real-hosts"))]
// A native exec_command returned a session id and the turn ended while the process lived, as
// write_stdin confirmed: that session is the node's own work, so its host stays.
#[test]
fn a_codex_node_whose_exec_session_runs_on_after_its_turn_keeps_its_host() {
    keeps_its_host_while_its_command_runs("session_command");
}

#[cfg(not(feature = "real-hosts"))]
fn keeps_its_host_while_its_command_runs(mode: &str) {
    let slice = Slice::new();
    configured(&slice, "complete");
    let release = slice._home.join("command-release");
    let path = std::ffi::CString::new(release.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: the path is a new name inside the test's own folder.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let fixture = serde_json::json!({"turnMode": mode, "ownedCommand": {"release": release, "pidFile": slice._home.join("command-pid.json")}});
    std::fs::write(slice._home.join("codex-native.account.json"), serde_json::to_vec(&fixture).unwrap()).unwrap();
    let mut driver = launch(&slice);
    while lifecycle(&*slice.domain).activity() != ActivityView::OwnWork {
        advance(&mut driver);
    }
    assert!(!slice.domain.read().state().host_releasable(&TASK.into()));
    while !driver.settle_release().unwrap() {}
    assert!(!driver.releases);
    slice.hosts.release_codex_commands(&release);
    driver.stop().unwrap();
}


#[cfg(not(feature = "real-hosts"))]
#[tokio::test(flavor = "multi_thread")]
async fn resumes_over_the_node_limit_wait_for_capacity_with_no_process_and_no_card() {
    let slice = Slice::new();
    enable(&slice);
    let tasks = ["t-over-one", "t-over-two", "t-over-three"];
    for task in tasks {
        let mut driver = launch_task(&slice, task);
        slice.settle(&mut driver);
        slice.domain.add_sdk_owner(Owner::spawn(driver));
    }
    close_resting_hosts(&slice.domain, &tasks).await;
    slice.hosts.hold();
    let limit = |slots| slice.domain.admission().tune(crate::admission::Tuning { spacing: 60_000, limit: Some(crate::admission::Limit::Fixed(slots)) });
    let working = || crate::admission::working_tasks(&*slice.domain, &slice.domain.read().state());
    limit(1);
    let started = slice.starts().len();
    for task in tasks {
        crate::domain_messages::tell_sdk_node(&*slice.domain, &task.into(), crate::node_push::ALONDRA, "Go on.", &format!("tell-{task}"), false).unwrap();
    }
    assert_eq!(slice.domain.node_groups(), Vec::<i32>::new(), "a message alone brings no host back past the gate");

    gate_look(&slice).await;
    until_working(&slice, 1).await;
    gate_look(&slice).await;

    assert_eq!(working().len(), 1, "the limit holds whatever the number of nodes with work");
    let waits = |task: &str| slice.domain.gate().waits_for_slot(&slice.domain.read().state(), &task.into());
    assert_eq!(tasks.iter().filter(|task| waits(task)).count(), 2, "the others wait for a slot");
    assert_eq!(slice.domain.node_groups().len(), 1, "with no host of their own");
    assert!(slice.starts().len() <= started + 1, "and no Claude: {:?}", slice.starts());
    assert!(open_card(&slice.domain, SESSION_ENDED_PREFIX).is_none(), "no card asks the user");

    limit(3);
    gate_look(&slice).await;
    until_working(&slice, 3).await;
    assert_eq!(tasks.iter().filter(|task| waits(task)).count(), 0, "a free slot resumes them, with the spacing of starts still at a minute");
    slice.domain.stop_sdk_owners();
}

#[cfg(not(feature = "real-hosts"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_node_whose_host_closed_while_it_rests_holds_no_capability_until_its_session_goes_on() {
    let slice = Slice::new();
    resting_node(&slice);
    let agent = slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].agent_id.clone().unwrap();
    let holds = |domain: &DomainRuntime| std::fs::read_to_string(domain.home().join("domain-capabilities.json")).unwrap().contains(&agent.0);
    assert!(holds(&slice.domain));
    close_resting_hosts(&slice.domain, &[TASK]).await;
    let retired = async { while holds(&slice.domain) { tokio::task::yield_now().await } };
    retired.await;
    tell(&slice, TASK, "Now the changelog.").await;
    until_last_prompt_ends_with(&slice.domain, &slice.hosts, "Now the changelog.").await;
    assert!(holds(&slice.domain), "the host that resumes the session is issued a new one");
    slice.domain.stop_sdk_owners();
}

/// A model selection of Claude's fixture catalog, as a launch records it.
fn claude_selection() -> alondra_core::model_selection::TaskSelection {
    use alondra_core::model_selection::*;
    let key = ModelKey { provider: Provider::Claude, model: "claude-fixture".into() };
    let mut selection = TaskSelection::automatic(ModelProfile::General);
    selection.request_model(Some(key.clone()));
    selection.effective = Some(ModelSelection { model: key, reasoning_effort: Some("high".into()), catalog_revision: "claude-catalog-1".into() });
    selection.account = Some(AccountKey::native(Provider::Claude));
    selection.native_tool_denials = Some(vec![]);
    selection
}

/// How a resting conversation of `tokens` goes on once told something, its last request made
/// `ago` milliseconds before: what each native Claude started with, and what the last was told.
struct Resumption { session: String, starts: Vec<serde_json::Value>, prompt: String, settings: serde_json::Value }

/// How the host of a resting conversation stands when it is told something.
#[derive(Clone, Copy, PartialEq)]
enum Standing {
    /// It closed, and a host that resumes the session takes its place.
    Released,
    /// It stayed alive with its session open, as a paused node's does.
    Live,
    /// It stayed alive while the daemon restarted, and the next daemon adopted it.
    Adopted,
}

async fn resumed_after(slice: &mut Slice, tokens: u64, ago: alondra_core::Millis, selected: bool) -> Resumption {
    went_on_after(slice, tokens, ago, selected, Standing::Released).await
}

/// Where the daemon finds how large the resting conversation is and when its cache expires.
#[derive(Clone, Copy, PartialEq)]
enum Measured {
    /// In the reading it kept of the task's last request.
    Stored,
    /// Only in Claude Code's transcript of the session: the task's last request came before
    /// readings were kept, or the reading was lost. The daemon restarts without its readings and
    /// adopts the host.
    InTranscript,
    /// In the reading it kept, after which the next request failed on the usage limit with an
    /// answer that used no tokens.
    StoredThenLimited,
    /// In Claude Code's transcript, the reading the daemon kept saying 0 tokens, as one replaced by
    /// a failed request's answer did before. The daemon restarts with that reading and adopts the
    /// host.
    InTranscriptOverEmpty,
}

async fn went_on_after(slice: &mut Slice, tokens: u64, ago: alondra_core::Millis, selected: bool, standing: Standing) -> Resumption {
    measured_went_on_after(slice, tokens, ago, selected, standing, Measured::Stored).await
}

/// `millis` as the RFC 3339 time Claude Code stamps each transcript record with.
fn rfc3339(millis: alondra_core::Millis) -> String {
    let (days, rest) = (millis / 86_400_000, millis % 86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (day, month) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z", rest / 3_600_000, rest / 60_000 % 60, rest / 1_000 % 60, rest % 1_000)
}

async fn measured_went_on_after(slice: &mut Slice, tokens: u64, ago: alondra_core::Millis, selected: bool, standing: Standing, measured: Measured) -> Resumption {
    enable(slice);
    claude_ready(slice);
    let selection = selected.then(claude_selection);
    let mut driver = start_bound(slice.domain.clone(), &slice.backend, Launch {
        task: TASK.into(), name: "Release notes".into(), directory: slice.worktree.to_str().unwrap().into(), prompt: "large-context".into(), plugin: slice.plugin.clone(),
        options: LaunchOptions { provider: Provider::Claude, model: selected.then(|| "claude-fixture".into()), effort: selected.then(|| "high".into()), selection, ..LaunchOptions::default() },
        model: None, account: AccountId::default_account(),
    }, capabilities(), None, None).unwrap();
    slice.settle(&mut driver);
    let session = slice.starts().last().unwrap()["session"].as_str().unwrap().to_owned();
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    match standing {
        Standing::Released => {
            close_resting_hosts(&slice.domain, &[TASK]).await;
        }
        Standing::Live => {}
        Standing::Adopted => {
            slice.domain.crash_sdk_owners();
            let readings = slice.domain.home().join("context-readings.json");
            match measured {
                Measured::InTranscript => std::fs::remove_file(readings).unwrap(),
                Measured::InTranscriptOverEmpty => {
                    let empty = serde_json::json!({TASK: {"tokens": 0, "requested_at": alondra_core::now_millis() - ago / 2, "cache_ms": 3_600_000, "compactable": false}});
                    std::fs::write(readings, empty.to_string()).unwrap();
                }
                Measured::Stored | Measured::StoredThenLimited => {}
            }
            slice.domain = slice.restart();
            enable(slice);
            let adopted = adopt(slice.domain.clone(), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
            slice.domain.add_sdk_owner(Owner::spawn(adopted));
        }
    }
    let slice: &Slice = slice;
    let (task, requested_at) = (TaskId::from(TASK), alondra_core::now_millis() - ago);
    match measured {
        Measured::Stored | Measured::StoredThenLimited => {
            slice.domain.contexts().handed(&task, requested_at);
            slice.domain.contexts().observed(&task, alondra_provider_claude::ContextEvent::Answered {
                tokens, cache: Some(std::time::Duration::from_secs(3600)), model: None, cached: Default::default(),
            }, requested_at);
            if measured == Measured::StoredThenLimited {
                let limited_at = requested_at + ago / 2;
                slice.domain.contexts().handed(&task, limited_at);
                slice.domain.contexts().observed(&task, alondra_provider_claude::ContextEvent::Answered {
                    tokens: 0, cache: None, model: Some("<synthetic>".into()), cached: Default::default(),
                }, limited_at);
            }
        }
        Measured::InTranscript | Measured::InTranscriptOverEmpty => {
            assert_eq!(slice.domain.contexts().last_request(&task), None, "the daemon holds no reading of the conversation");
            let transcript = claude_history::transcript(&slice.backend.native_environment, &session, &slice.worktree).expect("the session's transcript");
            let answer = serde_json::json!({"type": "assistant", "sessionId": session, "isSidechain": false, "timestamp": rfc3339(requested_at),
                "message": {"role": "assistant", "content": [], "usage": {"input_tokens": 3, "cache_read_input_tokens": tokens - 2_003, "cache_creation_input_tokens": 2_000,
                    "output_tokens": 400, "cache_creation": {"ephemeral_1h_input_tokens": 2_000, "ephemeral_5m_input_tokens": 0}}}});
            let mut file = std::fs::OpenOptions::new().append(true).open(transcript).unwrap();
            std::io::Write::write_all(&mut file, format!("{answer}\n").as_bytes()).unwrap();
        }
    }

    tell(slice, TASK, "Now the changelog.").await;
    let mut changed = slice.domain.subscribe();
    // Each change of the domain is looked at until the message reached Claude or the task ended up
    // somewhere it cannot go on from; how long the hosts take to start is the machine's, not the
    // test's to judge.
    loop {
        let open = slice.domain.store.lock_or_exit("the domain store").provider_transition(&task).unwrap().is_some();
        let pending = slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].pending_selection.is_some();
        if !open && !pending && slice.prompts().last().is_some_and(|prompt| prompt.ends_with("Now the changelog.")) { break }
        if let Some(lost) = open_card(&slice.domain, alondra_domain_rules::HOST_LOST_PREFIX) {
            panic!("the message never reached Claude, its host was lost: {:?}; prompts {:?}", lost.card.content.map(|content| content.body), slice.prompts());
        }
        if open {
            match crate::provider_transition::advance(&(slice.domain.clone() as std::sync::Arc<dyn alondra_engine_ports::Runtime>), &task).await {
                Ok(_) => {}
                Err(error) if error.code == "selection_transition" => {}
                Err(error) => panic!("advancing the transition: {error:?}"),
            }
        }
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), changed.changed()).await;
    }
    let capture: serde_json::Value = slice.hosts.native_capture();
    let args: Vec<&str> = capture["args"].as_array().unwrap().iter().filter_map(serde_json::Value::as_str).collect();
    let settings = args.iter().position(|arg| *arg == "--settings").map(|at| serde_json::from_str(args[at + 1]).unwrap()).unwrap_or_default();
    Resumption { session, starts: slice.starts(), prompt: slice.prompts().last().unwrap().clone(), settings }
}

const HOURS_AGO: alondra_core::Millis = 4 * 60 * 60_000;

#[cfg(not(feature = "real-hosts"))]
// 19 November: four nodes came back from a usage limit hours later, each resumed its conversation,
// and the first request of each wrote it whole to the prompt cache again: 2.1 million tokens at
// twice the price of their input. A large conversation whose cache expired goes on in a new session
// of the same model and account, which carries on from the task's records and finds the whole
// conversation before it in a file it reads only when it needs to.
#[tokio::test(flavor = "multi_thread")]
async fn a_large_conversation_whose_cache_expired_goes_on_in_a_new_session_that_can_read_the_old_one() {
    let mut slice = Slice::new();
    let resumed = resumed_after(&mut slice, 250_403, HOURS_AGO, true).await;
    slice.domain.stop_sdk_owners();
    let last = resumed.starts.last().unwrap();
    assert_eq!(resumed.starts.len(), 2, "{:?}", resumed.starts);
    assert!(last["resume"].is_null() && last["session"].as_str() != Some(resumed.session.as_str()), "a new session, not the old one resumed: {:?}", resumed.starts);
    assert_eq!(last["history"], 0, "nothing of the old conversation is sent again: {last}");
    let folder = slice._home.join("files/tasks").join(TASK);
    let file = folder.join(format!("conversation-{}.md", resumed.session));
    assert!(resumed.prompt.contains("Current input:\n") && resumed.prompt.ends_with("Now the changelog."), "{}", resumed.prompt);
    assert!(resumed.prompt.contains(&file.display().to_string()) && resumed.prompt.contains("grep -n '## Turn'"), "the new session is told where the old conversation is and how to search it: {}", resumed.prompt);
    let conversation = std::fs::read_to_string(&file).unwrap();
    assert!(conversation.contains("## Turn 1 · ") && conversation.contains("large-context"), "{conversation}");
    assert!(!resumed.prompt.contains("## Turn 1 · "), "the conversation itself is not in the prompt: {}", resumed.prompt);
    assert_eq!(resumed.settings["permissions"]["additionalDirectories"], serde_json::json!([folder]), "the new session reads its task's folder: {}", resumed.settings);
    assert!(slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].pending_selection.is_none(), "the task keeps its own selection, with nothing pending");
}

#[cfg(not(feature = "real-hosts"))]
// 21 November: the next input of the new session that took over a large conversation whose cache
// expired came before that session answered with the size of its own, and found the old
// conversation's reading: it started yet another new session, and each input after it one more.
// The new session holds a conversation of its own, and goes on with the next input itself.
#[tokio::test(flavor = "multi_thread")]
async fn the_new_session_that_took_over_a_large_conversation_goes_on_with_the_next_input_itself() {
    let mut slice = Slice::new();
    let resumed = resumed_after(&mut slice, 250_403, HOURS_AGO, true).await;
    let task = TaskId::from(TASK);
    let first = slice.domain.store.lock_or_exit("the domain store").latest_provider_transition(&task).unwrap().expect("the new session's transition").plan.id;
    tell(&slice, TASK, "And the release date.").await;
    let mut changed = slice.domain.subscribe();
    let latest = || slice.domain.store.lock_or_exit("the domain store").latest_provider_transition(&task).unwrap().unwrap().plan.id;
    let reached = async {
        while !slice.prompts().iter().any(|prompt| prompt.ends_with("And the release date.")) && latest() == first {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(100), changed.changed()).await;
        }
    };
    reached.await;
    slice.domain.stop_sdk_owners();
    assert_eq!(latest(), first, "no other new session was requested");
    let prompts = slice.prompts();
    let next = prompts.iter().find(|prompt| prompt.ends_with("And the release date.")).unwrap();
    assert!(!next.contains("Current input:"), "the input went to the session as it is, not in another handoff: {next}");
    assert_eq!(slice.starts().len(), resumed.starts.len(), "no other session started: {:?}", slice.starts());
    assert!(slice.domain.state().unwrap().tasks[&task].pending_selection.is_none());
}

#[cfg(not(feature = "real-hosts"))]
// 20 November: the task's records came to 64,085 tokens with the message that continued it, 85 over
// what the handoff could take, and the new session was refused: its conversation resumed and wrote
// 178,677 tokens to the cache again. The handoff of a new session is a few thousand tokens however
// large the task's records are.
#[tokio::test(flavor = "multi_thread")]
async fn a_task_with_records_of_64000_tokens_still_goes_on_in_a_new_session() {
    let records = "The request, repeated. ".repeat(3 * 64_000 / 23);
    let mut slice = Slice::with_request_body(crate::engine_tests::SliceHosts::default(), "domain.sqlite", &records);
    let resumed = resumed_after(&mut slice, 250_403, HOURS_AGO, true).await;
    slice.domain.stop_sdk_owners();
    let last = resumed.starts.last().unwrap();
    assert!(last["resume"].is_null() && last["session"].as_str() != Some(resumed.session.as_str()), "a new session, not the old one resumed: {:?}", resumed.starts);
    assert!(alondra_store::estimated_tokens(&resumed.prompt) < alondra_store::HANDOFF_TOKEN_CAP, "{}", resumed.prompt.len());
    let kept = std::fs::read_to_string(slice._home.join("files/tasks").join(TASK).join("task-records.md")).unwrap();
    assert!(kept.contains(&records), "the complete records are in a file to search");
}

#[cfg(not(feature = "real-hosts"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_large_conversation_whose_cache_still_holds_resumes() {
    let mut slice = Slice::new();
    let resumed = resumed_after(&mut slice, 250_403, 0, true).await;
    slice.domain.stop_sdk_owners();
    assert_eq!(resumed.starts.last().unwrap()["resume"].as_str(), Some(resumed.session.as_str()), "{:?}", resumed.starts);
}

#[cfg(not(feature = "real-hosts"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_small_conversation_whose_cache_expired_resumes() {
    let mut slice = Slice::new();
    let resumed = resumed_after(&mut slice, 90_000, HOURS_AGO, true).await;
    slice.domain.stop_sdk_owners();
    assert_eq!(resumed.starts.last().unwrap()["resume"].as_str(), Some(resumed.session.as_str()), "{:?}", resumed.starts);
}

#[cfg(not(feature = "real-hosts"))]
// A task with no model recorded cannot say which model a new session would start with: its
// conversation resumes, as it did before.
#[tokio::test(flavor = "multi_thread")]
async fn a_conversation_no_handoff_can_be_prepared_for_resumes() {
    let mut slice = Slice::new();
    let resumed = resumed_after(&mut slice, 250_403, HOURS_AGO, false).await;
    slice.domain.stop_sdk_owners();
    assert_eq!(resumed.starts.last().unwrap()["resume"].as_str(), Some(resumed.session.as_str()), "{:?}", resumed.starts);
    assert!(slice.domain.store.lock_or_exit("the domain store").latest_provider_transition(&TASK.into()).unwrap().is_none(), "nothing was requested");
}

#[cfg(not(feature = "real-hosts"))]
// 20 November: a task the user paused kept its host, which a daemon update adopted; resumed an hour
// and a half after its last request, its first input wrote 458,795 tokens to the prompt cache. The
// decision that a large conversation whose cache expired goes on in a new session is taken for any
// input that arrives after the expiry, whether the host was relaunched, adopted or never stopped.
async fn goes_on_in_a_new_session(standing: Standing) {
    let mut slice = Slice::new();
    let resumed = went_on_after(&mut slice, 458_795, HOURS_AGO, true, standing).await;
    slice.domain.stop_sdk_owners();
    let last = resumed.starts.last().unwrap();
    assert_eq!(resumed.starts.len(), 2, "{:?}", resumed.starts);
    assert!(last["resume"].is_null() && last["session"].as_str() != Some(resumed.session.as_str()), "a new session, not the old one going on: {:?}", resumed.starts);
    let file = slice._home.join("files/tasks").join(TASK).join(format!("conversation-{}.md", resumed.session));
    assert!(resumed.prompt.contains(&file.display().to_string()) && resumed.prompt.ends_with("Now the changelog."), "{}", resumed.prompt);
    assert!(std::fs::read_to_string(&file).unwrap().contains("large-context"));
}

#[cfg(not(feature = "real-hosts"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_large_conversation_whose_live_host_waited_past_its_cache_goes_on_in_a_new_session() {
    goes_on_in_a_new_session(Standing::Live).await;
}

#[cfg(not(feature = "real-hosts"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_large_conversation_whose_adopted_host_waited_past_its_cache_goes_on_in_a_new_session() {
    goes_on_in_a_new_session(Standing::Adopted).await;
}

#[cfg(not(feature = "real-hosts"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_large_conversation_whose_live_host_is_told_something_before_its_cache_expires_goes_on() {
    let mut slice = Slice::new();
    let resumed = went_on_after(&mut slice, 458_795, 0, true, Standing::Live).await;
    slice.domain.stop_sdk_owners();
    assert_eq!(resumed.starts.len(), 1, "the same host and session go on: {:?}", resumed.starts);
}

#[cfg(not(feature = "real-hosts"))]
// 20 November: a task paused before readings were kept was resumed hours later with no reading of
// its conversation, went on in the same session, and its first input wrote 155,628 tokens to the
// prompt cache. Without a stored reading, the size and the cache's expiry come from the last answer
// in Claude Code's transcript of the session.
#[tokio::test(flavor = "multi_thread")]
async fn a_large_conversation_with_no_stored_reading_whose_transcript_shows_its_cache_expired_goes_on_in_a_new_session() {
    let mut slice = Slice::new();
    let resumed = measured_went_on_after(&mut slice, 250_000, HOURS_AGO, true, Standing::Adopted, Measured::InTranscript).await;
    slice.domain.stop_sdk_owners();
    let last = resumed.starts.last().unwrap();
    assert_eq!(resumed.starts.len(), 2, "{:?}", resumed.starts);
    assert!(last["resume"].is_null() && last["session"].as_str() != Some(resumed.session.as_str()), "a new session, not the old one going on: {:?}", resumed.starts);
    let file = slice._home.join("files/tasks").join(TASK).join(format!("conversation-{}.md", resumed.session));
    assert!(resumed.prompt.contains(&file.display().to_string()) && resumed.prompt.ends_with("Now the changelog."), "{}", resumed.prompt);
}

#[cfg(not(feature = "real-hosts"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_large_conversation_with_no_stored_reading_whose_transcript_shows_its_cache_holds_resumes() {
    let mut slice = Slice::new();
    let resumed = measured_went_on_after(&mut slice, 250_000, 0, true, Standing::Adopted, Measured::InTranscript).await;
    slice.domain.stop_sdk_owners();
    assert_eq!(resumed.starts.len(), 1, "the same host and session go on: {:?}", resumed.starts);
}

#[cfg(not(feature = "real-hosts"))]
// 20 November: five nodes came back from a usage limit and resumed their conversations, the
// largest writing 206,811 tokens to the prompt cache: the last request of each had failed on the
// limit, and its answer, which used no tokens, had replaced the reading of the conversation with
// one of 0 tokens. An answer that used no tokens leaves the reading as it was, and a reading of 0
// tokens is no reading: the transcript says how large the conversation is.
#[tokio::test(flavor = "multi_thread")]
async fn a_large_conversation_whose_last_request_hit_the_usage_limit_goes_on_in_a_new_session() {
    let mut slice = Slice::new();
    let resumed = measured_went_on_after(&mut slice, 250_000, HOURS_AGO, true, Standing::Live, Measured::StoredThenLimited).await;
    slice.domain.stop_sdk_owners();
    let last = resumed.starts.last().unwrap();
    assert_eq!(resumed.starts.len(), 2, "{:?}", resumed.starts);
    assert!(last["resume"].is_null() && last["session"].as_str() != Some(resumed.session.as_str()), "a new session, not the old one going on: {:?}", resumed.starts);
}

#[cfg(not(feature = "real-hosts"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_large_conversation_whose_kept_reading_says_no_tokens_goes_on_in_a_new_session() {
    let mut slice = Slice::new();
    let resumed = measured_went_on_after(&mut slice, 250_000, HOURS_AGO, true, Standing::Adopted, Measured::InTranscriptOverEmpty).await;
    slice.domain.stop_sdk_owners();
    let last = resumed.starts.last().unwrap();
    assert_eq!(resumed.starts.len(), 2, "{:?}", resumed.starts);
    assert!(last["resume"].is_null() && last["session"].as_str() != Some(resumed.session.as_str()), "a new session, not the old one going on: {:?}", resumed.starts);
}

#[cfg(not(feature = "real-hosts"))]
#[test]
fn a_transcript_time_is_written_as_claude_code_writes_it() {
    for at in ["1970-01-01T00:00:00.000Z", "2026-02-28T23:59:59.999Z", "2028-02-29T12:30:05.042Z", "2026-11-20T07:38:00.000Z"] {
        assert_eq!(rfc3339(crate::watcher::parse_rfc3339_millis(at).unwrap()), at);
    }
}
