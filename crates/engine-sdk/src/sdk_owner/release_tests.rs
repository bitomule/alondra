//! A node that rests holds no process, its session goes on when its task has something to do, and a
//! node with anything of the task in its host keeps it.
use crate::gate::GateOf as _;
use crate::sdk_owner::SdkOwners as _;
use crate::live::LiveOf as _;
use crate::idle_compaction::ContextsOf as _;
use alondra_engine_admission::AdmissionOf as _;
use super::codex_tests::{claude_ready, configured, git, launch, request_move, resumed_threads, transcript, until_moved};
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
    pass(domain, &mut rests, now, &WAIT);
    pass(domain, &mut rests, now + WAIT.wait, &WAIT);
}

async fn until_released(domain: &DomainRuntime, tasks: &[&str]) {
    let mut changed = domain.subscribe();
    let released = |domain: &DomainRuntime| {
        let read = domain.read();
        tasks.iter().all(|task| read.state().lifecycle_of(&TaskId::from(*task)).is_ok_and(|lifecycle| lifecycle.host_released()))
    };
    let arrived = changed.wait_for(|_| released(domain));
    tokio::time::timeout(std::time::Duration::from_secs(60), arrived).await.expect("the hosts never closed").unwrap();
    let ended = async { while !domain.node_groups().is_empty() { tokio::task::yield_now().await } };
    tokio::time::timeout(std::time::Duration::from_secs(60), ended).await.expect("the owners never ended");
}

async fn until_working(slice: &Slice, count: usize) {
    let mut changed = slice.domain.subscribe();
    let reached = changed.wait_for(|_| crate::admission::working_tasks(&slice.domain, &slice.domain.read().state()).len() == count);
    tokio::time::timeout(std::time::Duration::from_secs(60), reached).await.expect("the nodes never resumed").unwrap();
}

/// The gate's look, which gives the turns that wait a slot and takes over the tasks with no host.
async fn gate_look(slice: &Slice) {
    crate::admission::pass_at(&slice.domain, &slice._home, alondra_core::now_millis()).await.unwrap();
    slice.domain.join_retakes();
}

/// Tells `task` something, and takes the gate's look that lets its turn go on.
async fn tell(slice: &Slice, task: &str, text: &str) {
    crate::domain_messages::tell_sdk_node(&slice.domain, &task.into(), crate::node_push::ALONDRA, text, &format!("tell-{task}-{text}"), false).unwrap();
    gate_look(slice).await;
}

/// A node that ended its turn and rests, under an owner that runs by itself.
fn resting_node(slice: &Slice) -> (String, Vec<alondra_process_group::Member>) {
    enable(slice);
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    assert_eq!(lifecycle(&slice.domain).activity(), ActivityView::Resting);
    let host = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
    let session = slice.starts().last().unwrap()["session"].as_str().unwrap().to_owned();
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    (session, host)
}

fn launch_task(slice: &Slice, task: &str) -> Driver {
    slice.run(&Actor::Orchestrator { generation: 0 }, &format!("task-{task}"), Command::CreateTask {
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
    start_bound(Arc::clone(&slice.domain), &slice.backend, Launch {
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
#[tokio::test(flavor = "multi_thread")]
async fn nodes_that_wait_keep_no_process_and_a_restarted_daemon_starts_none() {
    let slice = Slice::new();
    enable(&slice);
    let tasks = ["t-rests-one", "t-rests-two", "t-rests-three"];
    let mut hosts = vec![];
    for task in tasks {
        let mut driver = launch_task(&slice, task);
        slice.settle(&mut driver);
        hosts.extend(alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap());
        slice.domain.add_sdk_owner(Owner::spawn(driver));
    }
    let started = slice.starts().len();
    assert_eq!(survivors(&hosts).len(), hosts.len(), "the hosts and their Claudes run while the nodes rest");

    look_after_the_wait(&slice.domain);
    until_released(&slice.domain, &tasks).await;

    assert_eq!(survivors(&hosts), Vec::<String>::new(), "no process is left of any node that waits");
    assert!(open_card(&slice.domain, SESSION_ENDED_PREFIX).is_none(), "no card asks the user");
    slice.domain.stop_sdk_owners();
    let restarted = slice.restart();
    restarted.enable_sdk(slice.backend.clone());
    adopt_all(&restarted, &slice.backend);
    assert_eq!(slice.starts().len(), started, "a daemon that comes back starts no Claude for them");
    assert_eq!(restarted.node_groups(), Vec::<i32>::new(), "and no host");
}

// 7 November, 07:45: 39 nodes worked under a limit of 8, since a resume took no slot, and the memory
// ran out. A closed node with work waits its turn for a slot, as a state with no card.

#[tokio::test(flavor = "multi_thread")]
async fn a_node_rests_with_its_host_until_the_wait_passes() {
    let slice = Slice::new();
    let (_, host) = resting_node(&slice);
    let mut rests = Rests::default();
    let now = alondra_core::now_millis();
    pass(&slice.domain, &mut rests, now, &WAIT);
    pass(&slice.domain, &mut rests, now + WAIT.wait - 1, &WAIT);
    assert!(!lifecycle(&slice.domain).host_released());
    assert_eq!(survivors(&host).len(), host.len());
    slice.domain.stop_sdk_owners();
}

#[tokio::test(flavor = "multi_thread")]
async fn work_for_a_closed_node_resumes_its_session_with_no_card_and_no_turn_of_its_own() {
    let slice = Slice::new();
    let (session, host) = resting_node(&slice);
    let told_before = slice.prompts().len();
    look_after_the_wait(&slice.domain);
    until_released(&slice.domain, &[TASK]).await;
    assert_eq!(survivors(&host), Vec::<String>::new());

    tell(&slice, TASK, "Now the changelog.").await;
    until_last_prompt_ends_with(&slice.domain, &slice.capture, "Now the changelog.").await;

    let (starts, prompts) = (slice.starts(), slice.prompts());
    slice.domain.stop_sdk_owners();
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert_eq!(starts.last().unwrap()["resume"].as_str(), Some(session.as_str()), "the same session goes on: {starts:?}");
    assert_eq!(prompts.len(), told_before + 1, "only what it was told reaches it: {prompts:?}");
    assert!(open_card(&slice.domain, SESSION_ENDED_PREFIX).is_none(), "no card asks the user");
    assert!(!lifecycle(&slice.domain).host_released());
    assert_eq!(archived_hosts(&slice), Vec::<String>::new(), "a closed host keeps no folder: it told everything before closing");
}

fn archived_hosts(slice: &Slice) -> Vec<String> {
    std::fs::read_dir(&slice.backend.state_root).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".ended-")).collect()
}

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
    look_after_the_wait(&slice.domain);
    until_released(&slice.domain, &[TASK]).await;
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
    crate::domain_messages::tell_sdk_node(&slice.domain, &TASK.into(), crate::node_push::ALONDRA, "Clean up and call cleanup_done.", "tell-cleanup", false).unwrap();
    tokio::task::block_in_place(|| first_reserved.recv_timeout(std::time::Duration::from_secs(60))).expect("the first takeover never reserved its owner");
    let handle = tokio::runtime::Handle::current();
    std::thread::scope(|scope| {
        let closer = scope.spawn(|| {
            handle.block_on(close_pass(&slice));
            drop(settled.send("closed"));
        });
        tokio::task::block_in_place(|| closer_settled.recv_timeout(std::time::Duration::from_secs(60))).expect("the closer neither took over nor waited");
        go.send(()).unwrap();
        closer.join().unwrap();
    });
    slice.domain.join_retakes();

    let lost = open_card(&slice.domain, alondra_domain_rules::HOST_LOST_PREFIX);
    assert!(lost.is_none(), "no card tells the user the session could not be resumed: {lost:?}");
    assert!(slice.domain.owns_sdk_host(&agent), "one host runs the finished task so it can clean up");
    until_last_prompt_ends_with(&slice.domain, &slice.capture, "Clean up and call cleanup_done.").await;
    let starts = slice.starts();
    slice.domain.stop_sdk_owners();
    assert_eq!(starts.len(), 2, "one host resumed the session: {starts:?}");
    assert_eq!(starts[1]["resume"].as_str(), Some(session.as_str()), "the same session goes on: {starts:?}");
}

// Compaction is an input no `TellNode` carries: the look at the state finds it. The closed host
// keeps what was measured of its conversation, so the cache can still be saved before it expires.
#[tokio::test(flavor = "multi_thread")]
async fn a_large_closed_conversation_is_resumed_to_compact_before_its_cache_expires() {
    use crate::idle_compaction::{Timing as Compaction, compact_due};
    let slice = Slice::new();
    enable(&slice);
    let mut driver = slice.launch_with("large-context");
    slice.settle(&mut driver);
    let session = slice.starts().last().unwrap()["session"].as_str().unwrap().to_owned();
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    look_after_the_wait(&slice.domain);
    until_released(&slice.domain, &[TASK]).await;

    let readings = slice.domain.contexts().readings();
    assert_eq!(readings.len(), 1, "closing the host forgets nothing of the conversation");
    compact_due(&slice.domain, readings[0].1.expires_at - Compaction::LIVE.lead, &Compaction::LIVE);
    assert_eq!(lifecycle(&slice.domain).pending_inputs().len(), 1, "the compaction waits for a host");
    assert_eq!(slice.starts().len(), 1, "nothing but the gate starts one");
    gate_look(&slice).await;
    let mut changed = slice.domain.subscribe();
    let compacted = changed.wait_for(|_| lifecycle(&slice.domain).pending_inputs().is_empty());
    tokio::time::timeout(std::time::Duration::from_secs(60), compacted).await.expect("the compaction never reached the node").unwrap();
    slice.domain.stop_sdk_owners();
    assert_eq!(slice.starts().last().unwrap()["resume"].as_str(), Some(session.as_str()));
}

// No host to stop: the task ends as one whose host ended, and no Claude starts just to end it.
#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_closed_node_ends_the_task_without_starting_anything() {
    let slice = Slice::new();
    resting_node(&slice);
    look_after_the_wait(&slice.domain);
    until_released(&slice.domain, &[TASK]).await;
    slice.domain.join_retakes();
    slice.run(&user(), "cancel-closed-node", Command::CancelTask { task_id: TASK.into(), reason: None });
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &crate::task_closer::Timing::new(None), alondra_core::now_millis()).await.unwrap();
    let state = slice.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from(TASK)].state, TaskState::Cancelled);
    assert_eq!(slice.starts().len(), 1, "no Claude starts for a task that ends");
    assert!(open_card(&slice.domain, SESSION_ENDED_PREFIX).is_none(), "no card asks the user");
    assert_eq!(slice.domain.node_groups(), Vec::<i32>::new());
}

#[tokio::test]
async fn a_closed_node_moves_to_another_provider_like_a_resting_one() {
    use crate::domain_store::provider_transition::Phase;
    let slice = Slice::new();
    let task: TaskId = TASK.into();
    configured(&slice, "complete");
    claude_ready(&slice);
    git(&slice.worktree, &["init", "-q", "-b", "release-notes"]);
    enable(&slice);
    let mut driver = launch(&slice);
    slice.settle(&mut driver);
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    look_after_the_wait(&slice.domain);
    until_released(&slice.domain, &[TASK]).await;
    request_move(&slice);
    let (pulse, mut changed) = tokio::sync::watch::channel(0_u64);
    slice.domain.before_owner_publish(move || pulse.send_modify(|count| *count += 1));
    let phase = crate::provider_transition::advance(&slice.domain, &task).await.unwrap();
    assert!(matches!(phase, Phase::TargetAdmitted | Phase::Done), "{phase:?}");
    until_moved(&slice, &mut changed).await;
    let state = slice.domain.state().unwrap();
    assert!(state.tasks[&task].pending_selection.is_none(), "it moved at once, as a resting node does");
    assert_eq!(state.lifecycle_journal[&task].iter().filter(|record| matches!(record, LifecycleRecord::Admitted { .. })).count(), 1, "no node starts until something resumes it");
    slice.domain.stop_sdk_owners();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_model_chosen_for_a_closed_node_reaches_it_and_stays_when_it_closes_again() {
    let slice = Slice::new();
    resting_node(&slice);
    look_after_the_wait(&slice.domain);
    until_released(&slice.domain, &[TASK]).await;

    let state = slice.domain.state().unwrap();
    slice.domain.execute_as_system(envelope(&state, "model-of-a-closed-node", Command::SetModel { task_id: TASK.into(), model: "sonnet".into() })).unwrap();
    let mut changed = slice.domain.subscribe();
    let applied = changed.wait_for(|_| slice.control_rows().iter().any(|(kind, _, status)| kind == "set_model" && status != "requested"));
    tokio::time::timeout(std::time::Duration::from_secs(60), applied).await.expect("the control never reached a host").unwrap();
    assert!(slice.domain.read().state().last_model_asked(&TASK.into()).is_some_and(|model| model == "sonnet"));

    look_after_the_wait(&slice.domain);
    until_released(&slice.domain, &[TASK]).await;
    tell(&slice, TASK, "Go on.").await;
    until_last_prompt_ends_with(&slice.domain, &slice.capture, "Go on.").await;
    slice.domain.stop_sdk_owners();
    let capture: serde_json::Value = serde_json::from_slice(&std::fs::read(&slice.capture).unwrap()).unwrap();
    assert!(capture["args"].as_array().unwrap().iter().any(|arg| arg == "--model=sonnet"), "the host that resumes opens with the model chosen: {capture}");
}

// The input committed after the owner decided, while its host ended: the release is refused, and
// the task is taken over as after any host that ended.
#[test]
fn an_input_that_arrives_while_the_host_closes_reaches_the_host_that_replaces_it() {
    let slice = Slice::new();
    enable(&slice);
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    while !driver.settle_release().unwrap() {}
    assert!(driver.releases, "the node rests with nothing in its host");
    crate::domain_messages::tell_sdk_node(&slice.domain, &TASK.into(), crate::node_push::ALONDRA, "Just in time.", "tell-while-closing", false).unwrap();
    assert!(driver.release_host().is_err(), "a task that moved on does not stay without a host");
    assert!(!lifecycle(&slice.domain).host_released());
    driver.releases = false;
    driver.host_closed = true;
    let mut replacement = driver.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    slice.settle(&mut replacement);
    assert!(slice.prompts().last().unwrap().ends_with("Just in time."));
    replacement.stop().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_whose_own_work_runs_after_its_turn_keeps_its_host() {
    let slice = Slice::new();
    let (_, _, commands, host) = node_with_a_background_group(&slice);
    look_after_the_wait(&slice.domain);
    assert!(!slice.domain.read().state().host_releasable(&TASK.into()));
    slice.domain.stop_sdk_owners();
    assert_eq!(slice.starts().len(), 1);
    assert!(!lifecycle(&slice.domain).host_released());
    drop((commands, host));
}

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

#[test]
fn a_paused_node_keeps_what_its_pause_decided() {
    let slice = Slice::new();
    enable(&slice);
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let agent = driver.agent().clone();
    assert!(slice.domain.gate().hold_back(&agent.0, alondra_engine_ports::admission::Reason::Disk, alondra_core::now_millis()));
    assert!(!slice.domain.read().state().host_releasable(&TASK.into()));
    driver.stop().unwrap();
}

// Codex: its thread resumes through `thread/resume`, as Claude's session through `--resume`.
#[tokio::test(flavor = "multi_thread")]
async fn a_closed_codex_node_resumes_the_same_thread() {
    let slice = Slice::new();
    let trace = configured(&slice, "complete");
    enable(&slice);
    let mut driver = launch(&slice);
    slice.settle(&mut driver);
    let native = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
    assert!(transcript(&trace).iter().any(|message| message["method"] == "thread/start"), "the node opened its thread");
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    look_after_the_wait(&slice.domain);
    until_released(&slice.domain, &[TASK]).await;
    assert_eq!(survivors(&native), Vec::<String>::new(), "neither its host nor its app-server is left");

    tell(&slice, TASK, "Now the changelog.").await;
    let mut changed = slice.domain.subscribe();
    let resumed = changed.wait_for(|_| !resumed_threads(&trace).is_empty() && lifecycle(&slice.domain).pending_inputs().is_empty());
    tokio::time::timeout(std::time::Duration::from_secs(60), resumed).await.expect("the input never reached the thread").unwrap();
    slice.domain.stop_sdk_owners();
    assert_eq!(resumed_threads(&trace).len(), 1);
    assert!(open_card(&slice.domain, SESSION_ENDED_PREFIX).is_none(), "no card asks the user");
}

// Codex reports its own work as a child agent or as a command with a process: neither closes.
#[test]
fn a_codex_node_with_a_child_agent_keeps_its_host() {
    let slice = Slice::new();
    configured(&slice, "child");
    let mut driver = launch(&slice);
    while lifecycle(&slice.domain).activity() != ActivityView::OwnWork {
        advance(&mut driver);
    }
    assert!(!slice.domain.read().state().host_releasable(&TASK.into()));
    while !driver.settle_release().unwrap() {}
    assert!(!driver.releases);
    driver.stop().unwrap();
}

#[test]
fn a_codex_node_with_a_command_running_after_its_turn_keeps_its_host() {
    keeps_its_host_while_its_command_runs("blocked_command");
}

// A native exec_command returned a session id and the turn ended while the process lived, as
// write_stdin confirmed: that session is the node's own work, so its host stays.
#[test]
fn a_codex_node_whose_exec_session_runs_on_after_its_turn_keeps_its_host() {
    keeps_its_host_while_its_command_runs("session_command");
}

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
    while lifecycle(&slice.domain).activity() != ActivityView::OwnWork {
        advance(&mut driver);
    }
    assert!(!slice.domain.read().state().host_releasable(&TASK.into()));
    while !driver.settle_release().unwrap() {}
    assert!(!driver.releases);
    std::fs::write(&release, "").unwrap();
    driver.stop().unwrap();
}


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
    look_after_the_wait(&slice.domain);
    until_released(&slice.domain, &tasks).await;
    std::fs::write(format!("{}.hold", slice.capture.display()), "").unwrap();
    let limit = |slots| slice.domain.admission().tune(crate::admission::Tuning { spacing: 60_000, limit: Some(crate::admission::Limit::Fixed(slots)) });
    let working = || crate::admission::working_tasks(&slice.domain, &slice.domain.read().state());
    limit(1);
    let started = slice.starts().len();
    for task in tasks {
        crate::domain_messages::tell_sdk_node(&slice.domain, &task.into(), crate::node_push::ALONDRA, "Go on.", &format!("tell-{task}"), false).unwrap();
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

#[tokio::test(flavor = "multi_thread")]
async fn a_node_whose_host_closed_while_it_rests_holds_no_capability_until_its_session_goes_on() {
    let slice = Slice::new();
    resting_node(&slice);
    let agent = slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].agent_id.clone().unwrap();
    let holds = |domain: &DomainRuntime| std::fs::read_to_string(domain.home().join("domain-capabilities.json")).unwrap().contains(&agent.0);
    assert!(holds(&slice.domain));
    look_after_the_wait(&slice.domain);
    until_released(&slice.domain, &[TASK]).await;
    let retired = async { while holds(&slice.domain) { tokio::task::yield_now().await } };
    tokio::time::timeout(std::time::Duration::from_secs(60), retired).await.expect("the closed host's capability is retired");
    tell(&slice, TASK, "Now the changelog.").await;
    until_last_prompt_ends_with(&slice.domain, &slice.capture, "Now the changelog.").await;
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

async fn went_on_after(slice: &mut Slice, tokens: u64, ago: alondra_core::Millis, selected: bool, standing: Standing) -> Resumption {
    enable(slice);
    claude_ready(slice);
    let selection = selected.then(claude_selection);
    let mut driver = start_bound(Arc::clone(&slice.domain), &slice.backend, Launch {
        task: TASK.into(), name: "Release notes".into(), directory: slice.worktree.to_str().unwrap().into(), prompt: "large-context".into(), plugin: slice.plugin.clone(),
        options: LaunchOptions { provider: Provider::Claude, model: selected.then(|| "claude-fixture".into()), effort: selected.then(|| "high".into()), selection, ..LaunchOptions::default() },
        model: None, account: AccountId::default_account(),
    }, capabilities(), None, None).unwrap();
    slice.settle(&mut driver);
    let session = slice.starts().last().unwrap()["session"].as_str().unwrap().to_owned();
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    match standing {
        Standing::Released => {
            look_after_the_wait(&slice.domain);
            until_released(&slice.domain, &[TASK]).await;
        }
        Standing::Live => {}
        Standing::Adopted => {
            slice.domain.crash_sdk_owners();
            slice.domain = slice.restart();
            enable(slice);
            let adopted = adopt(Arc::clone(&slice.domain), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
            slice.domain.add_sdk_owner(Owner::spawn(adopted));
        }
    }
    let slice: &Slice = slice;
    let (task, requested_at) = (TaskId::from(TASK), alondra_core::now_millis() - ago);
    slice.domain.contexts().handed(&task, requested_at);
    slice.domain.contexts().observed(&task, alondra_provider_claude::ContextEvent::Answered {
        tokens, cache: Some(std::time::Duration::from_secs(3600)), model: None, cached: Default::default(),
    }, requested_at);

    tell(slice, TASK, "Now the changelog.").await;
    let mut changed = slice.domain.subscribe();
    let goes_on = async {
        loop {
            let open = slice.domain.store.lock_or_exit("the domain store").provider_transition(&task).unwrap().is_some();
            if !open && prompts_at(&slice.capture).last().is_some_and(|prompt| prompt.ends_with("Now the changelog.")) { break }
            if open {
                crate::provider_transition::advance(&slice.domain, &task).await.unwrap();
            }
            let _ = tokio::time::timeout(std::time::Duration::from_millis(100), changed.changed()).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(60), goes_on).await.unwrap_or_else(|_| panic!("the message never reached Claude: {:?}", prompts_at(&slice.capture)));
    let capture: serde_json::Value = serde_json::from_slice(&std::fs::read(&slice.capture).unwrap()).unwrap();
    let args: Vec<&str> = capture["args"].as_array().unwrap().iter().filter_map(serde_json::Value::as_str).collect();
    let settings = args.iter().position(|arg| *arg == "--settings").map(|at| serde_json::from_str(args[at + 1]).unwrap()).unwrap_or_default();
    Resumption { session, starts: slice.starts(), prompt: prompts_at(&slice.capture).last().unwrap().clone(), settings }
}

const HOURS_AGO: alondra_core::Millis = 4 * 60 * 60_000;

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

#[tokio::test(flavor = "multi_thread")]
async fn a_large_conversation_whose_cache_still_holds_resumes() {
    let mut slice = Slice::new();
    let resumed = resumed_after(&mut slice, 250_403, 0, true).await;
    slice.domain.stop_sdk_owners();
    assert_eq!(resumed.starts.last().unwrap()["resume"].as_str(), Some(resumed.session.as_str()), "{:?}", resumed.starts);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_small_conversation_whose_cache_expired_resumes() {
    let mut slice = Slice::new();
    let resumed = resumed_after(&mut slice, 90_000, HOURS_AGO, true).await;
    slice.domain.stop_sdk_owners();
    assert_eq!(resumed.starts.last().unwrap()["resume"].as_str(), Some(resumed.session.as_str()), "{:?}", resumed.starts);
}

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

#[tokio::test(flavor = "multi_thread")]
async fn a_large_conversation_whose_live_host_waited_past_its_cache_goes_on_in_a_new_session() {
    goes_on_in_a_new_session(Standing::Live).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_large_conversation_whose_adopted_host_waited_past_its_cache_goes_on_in_a_new_session() {
    goes_on_in_a_new_session(Standing::Adopted).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_large_conversation_whose_live_host_is_told_something_before_its_cache_expires_goes_on() {
    let mut slice = Slice::new();
    let resumed = went_on_after(&mut slice, 458_795, 0, true, Standing::Live).await;
    slice.domain.stop_sdk_owners();
    assert_eq!(resumed.starts.len(), 1, "the same host and session go on: {:?}", resumed.starts);
}
