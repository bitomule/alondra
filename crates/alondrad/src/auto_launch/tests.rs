use super::*;
use crate::domain_orchestrator_tools::{LaunchBody, launch};
use crate::launch_plans::LaunchPlan;
use crate::sdk_owner::tests::SdkLaunches;
use alondra_core::domain::{
    Actor, StartProblem,
    Command, CommandEnvelope, DeliveryTarget, InteractionId, MergeMethod, NodeEnd, PROTOCOL_VERSION, PrState, TaskId,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

struct Fixture {
    sdk: SdkLaunches,
    root: crate::test_support::Directory,
    home: PathBuf,
    project: PathBuf,
    domain: Arc<DomainRuntime>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.domain.stop_sdk_owners();
        self.sdk.refuse_starts(false);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn command(domain: &DomainRuntime, actor: Actor, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("c-{}", crate::auth::random_hex(8)).into(),
        expected_revision: state.revision, command,
    }, None, || Ok(())).unwrap();
}

fn world() -> Fixture {
    world_using_sdk(SdkLaunches::enable_holding)
}

fn world_using_sdk(enable: fn(&Arc<DomainRuntime>, &Path) -> SdkLaunches) -> Fixture {
    let root = crate::test_support::Directory::new();
    let home = root.join("home");
    let project = root.join("App");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let domain = Arc::new(DomainRuntime::open(&home).unwrap());
    let sdk = enable(&domain, &home);
    command(&domain, user(), Command::CreateRequest { request_id: "r".into(), body: "Rebuild the editor".into(), auto_merge: false, maintenance: false, message: false });
    command(&domain, user(), Command::TitleRequest { request_id: "r".into(), title: "Editor".into() });
    Fixture { sdk, root, home, project, domain }
}

fn plan(f: &Fixture, name: &str) -> LaunchPlan {
    LaunchPlan {
        name: name.into(), cwd: f.project.to_str().unwrap().into(), mission: None, model: Some("sonnet".into()), effort: None,
        approach: None, reason: Some("A clear, mechanical change".into()), role: None, tags: None, project: None,
    }
}

/// A task created the way the orchestrator creates one: with its plan, through its capability.
fn create(f: &Fixture, id: &str, after: &[&str], plan: Option<&LaunchPlan>) {
    let bearer = f.domain.bind_orchestrator(&format!("execution-{id}")).unwrap().expose_for_child_environment().to_owned();
    let state = f.domain.state().unwrap();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("create-{id}").into(), expected_revision: 0,
        command: Command::CreateTask { plan_items: None,
            task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
            dependencies: after.iter().map(|dep| TaskId::from(*dep)).collect(), stack: None,
            launch: plan.cloned(),
        },
    };
    f.domain.execute_bearer(&bearer, &envelope).unwrap();
}

fn text() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text { text: "The result".into() }]
}

/// The task is delivered, accepted and its node closed: done.
fn finish(f: &Fixture, id: &str, body: &str, pull_request: bool) {
    let agent = f.domain.state().unwrap().tasks[&TaskId::from(id)].agent_id.clone().unwrap();
    let author = Actor::Agent { agent_id: agent };
    let targets = if pull_request {
        vec![DeliveryTarget::PullRequest { pr_id: format!("pr-{id}").into(), head: "head1".into() }]
    } else {
        text()
    };
    if pull_request {
        command(&f.domain, author.clone(), Command::RegisterPr { task_id: id.into(), pr_id: format!("pr-{id}").into(), host: "github.com".into(),
            repository: "me/app".into(), number: 7, head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash] });
    }
    command(&f.domain, author, Command::PresentResult { checklist: Vec::new(), attachments: vec![], task_id: id.into(), interaction_id: format!("final-{id}").into(),
        body: body.into(), targets: targets.clone(), is_final: true, nothing_to_review: false, card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true) });
    command(&f.domain, user(), Command::AcceptDelivery { presentation_id: format!("final-{id}").into(), targets });
    if pull_request {
        command(&f.domain, Actor::System, Command::ObservePrState { pr_id: format!("pr-{id}").into(), head: "head1".into(), base: "main".into(),
            state: PrState::Merged, merged_head: Some("head1".into()) });
    }
    command(&f.domain, Actor::System, Command::RecordNodeEnd { task_id: id.into(), end: NodeEnd::Closed, agent_id: None });
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from(id)].state, TaskState::Done);
}

/// A task an earlier node works on, which was never launched by Alondra.
fn open_task(f: &Fixture, id: &str) {
    command(&f.domain, Actor::System, Command::RegisterAgent { agent_id: format!("seed-{id}").into(), name: format!("Seed {id}").into(), at: 0 });
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"),
        agent_id: Some(format!("seed-{id}").into()), review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
}

/// A finished earlier task for the others to wait on.
fn done_task(f: &Fixture, id: &str) {
    open_task(f, id);
    finish(f, id, &format!("What {id} delivered"), false);
}

/// How many nodes Alondra started through the SDK.
fn launches(f: &Fixture) -> usize {
    f.domain.state().unwrap().agents.keys().filter(|agent| agent.0.starts_with("sdk-")).count()
}

/// The prompt the task's last launch handed its Claude, as the store keeps it.
fn prompt(f: &Fixture, task: &str) -> String {
    let database = crate::domain_database_migration::active_path(&f.home);
    rusqlite::Connection::open(database).unwrap().query_row(
        "SELECT i.body FROM lifecycle_dispatches d JOIN lifecycle_inputs i ON i.task_id=d.task_id AND i.input_id=d.input_id \
         WHERE d.task_id=?1 AND d.kind='launch' ORDER BY d.id DESC LIMIT 1", [task], |row| row.get(0)).unwrap()
}

/// The name the task's node was started with.
fn node_name(f: &Fixture, task: &str) -> String {
    let agent = agent_of(f, task).expect("the task has a node");
    f.domain.state().unwrap().agents[&agent].name.clone()
}

fn told(f: &Fixture) -> Vec<String> {
    f.domain.state().unwrap().interactions.keys().map(|id| id.0.clone()).collect()
}

fn auto(f: &Fixture, task: &str) -> Option<crate::domain_store::AutoLaunch> {
    f.domain.store.lock().unwrap().auto_launch(&task.into()).unwrap()
}

fn agent_of(f: &Fixture, task: &str) -> Option<alondra_core::domain::AgentId> {
    f.domain.state().unwrap().tasks[&TaskId::from(task)].agent_id.clone()
}

#[tokio::test]
async fn a_chain_of_three_tasks_starts_each_one_when_the_one_before_is_done() {
    let f = world();
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    create(&f, "f2", &["f1"], Some(&plan(&f, "app/f2")));
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1, "f2 waits for f1");
    let first = agent_of(&f, "f1").expect("f1 started");
    assert!(f.sdk.options(&first.0).model.is_some(), "{:?}", f.sdk.options(&first.0));
    assert_eq!(node_name(&f, "f1"), "app/f1");
    assert!(prompt(&f, "f1").contains("### “Task f0” (done)\nWhat f0 delivered"), "{}", prompt(&f, "f1"));
    assert_eq!(auto(&f, "f1").unwrap().state, AutoLaunchState::Launched);

    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1, "a second pass starts nothing");
    finish(&f, "f1", "What f1 delivered", true);
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 2, "{:?} {:?}", auto(&f, "f2"), told(&f));
    assert!(prompt(&f, "f2").contains("### “Task f1” (done)\nWhat f1 delivered\nPull request: https://github.com/me/app/pull/7 (merged)"), "{}", prompt(&f, "f2"));
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 2);
    assert!(agent_of(&f, "f2").is_some());
    let decisions = f.domain.store.lock().unwrap().model_decision_rows().unwrap();
    let launches: Vec<_> = decisions.iter().filter(|row| row.1 == "launch").map(|row| (row.0.clone(), row.5.clone())).collect();
    assert_eq!(launches, [("f1".to_owned(), Some("A clear, mechanical change".to_owned())), ("f2".to_owned(), Some("A clear, mechanical change".to_owned()))]);
}

#[tokio::test]
async fn a_review_starts_by_itself_when_the_task_it_reviews_delivers_and_its_mission_has_that_delivery() {
    let f = world();
    open_task(&f, "a");
    create(&f, "rev", &[], Some(&plan(&f, "app/rev")));
    command(&f.domain, user(), Command::SetRelations {
        task_id: "rev".into(), delivered_dependencies: ["a".into()].into(), blocks: ["a".into()].into(),
    });
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 0, "nothing to review before the delivery");
    let author = Actor::Agent { agent_id: "seed-a".into() };
    command(&f.domain, author.clone(), Command::RegisterPr { task_id: "a".into(), pr_id: "pr-a".into(), host: "github.com".into(),
        repository: "me/app".into(), number: 9, head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash] });
    command(&f.domain, author, Command::PresentResult { checklist: Vec::new(), attachments: vec![], task_id: "a".into(), interaction_id: "final-a".into(),
        body: "What a delivered".into(), targets: vec![DeliveryTarget::PullRequest { pr_id: "pr-a".into(), head: "head1".into() }],
        is_final: true, nothing_to_review: false, card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true) });
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1, "the review starts at the delivery, not at the merge");
    let prompt = prompt(&f, "rev");
    assert!(prompt.contains("### “Task a” (delivered, not finished yet)\nWhat a delivered\nPull request: https://github.com/me/app/pull/9 (open)"), "{prompt}");
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1);
}

#[tokio::test]
async fn while_usage_is_exhausted_a_ready_task_waits_and_starts_when_usage_is_back() {
    let f = world();
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    let now = alondra_core::now_millis();
    f.domain.live.usage_pause.exhaust(Some(now + 3_600_000), now);
    pass(&f.domain, &f.home).await.unwrap();
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 0);
    assert_eq!(auto(&f, "f1").unwrap().reason, Some(AutoLaunchReason::UsageExhausted));
    assert!(told(&f).iter().all(|id| !id.starts_with("auto-launch")), "the orchestrator is not told: it can do nothing");

    f.domain.live.usage_pause.resume(alondra_core::now_millis());
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1);
    assert_eq!(auto(&f, "f1").unwrap().state, AutoLaunchState::Launched);
}

#[tokio::test]
async fn while_the_disk_is_nearly_full_a_ready_task_waits_and_starts_once_there_is_space() {
    let f = world();
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    f.domain.live.machine_watch.set_disk_low(true);
    pass(&f.domain, &f.home).await.unwrap();
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 0);
    assert_eq!(auto(&f, "f1"), None, "not claimed: it starts by itself once there is space");
    assert!(told(&f).iter().all(|id| !id.starts_with("auto-launch")), "the orchestrator is not told: the user has the disk card");
    let queued = f.domain.live.gate.live(&f.domain, alondra_core::now_millis()).unwrap().queued;
    assert!(queued.iter().any(|queued| queued.task_id == "f1" && matches!(queued.hold, alondra_core::views::LiveHold::Disk { .. })),
        "it waits at the gate for room on the disk: {queued:?}");

    f.domain.live.machine_watch.set_disk_low(false);
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1);
    assert_eq!(auto(&f, "f1").unwrap().state, AutoLaunchState::Launched);
}

#[tokio::test]
async fn two_dependencies_done_together_start_the_task_once() {
    let f = world();
    open_task(&f, "a");
    open_task(&f, "b");
    create(&f, "c", &["a", "b"], Some(&plan(&f, "app/c")));
    finish(&f, "a", "What a delivered", false);
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 0, "b is not done yet");
    finish(&f, "b", "What b delivered", false);
    pass(&f.domain, &f.home).await.unwrap();
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1);
    let prompt = prompt(&f, "c");
    assert!(prompt.contains("What a delivered") && prompt.contains("What b delivered"), "{prompt}");
}

#[tokio::test]
async fn a_task_whose_node_the_user_closed_is_not_started_again_by_itself() {
    let f = world();
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    f.domain.live.remember_closed_task("f1", crate::client_routes::ClosedNode { name: "app/f1".into(), cwd: None });
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 0);
    assert_eq!(auto(&f, "f1"), None);
}

#[tokio::test]
async fn a_launch_interrupted_by_a_restart_is_tried_again_once() {
    let f = world();
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    assert!(f.domain.store.lock().unwrap().claim_auto_launch(&"f1".into(), 1, false).unwrap());
    assert_eq!(auto(&f, "f1").unwrap().state, AutoLaunchState::Launching);
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 0, "a launching row is another pass's");
    recover(&f.domain).unwrap();
    assert_eq!(auto(&f, "f1"), None);
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1);
    recover(&f.domain).unwrap();
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1, "tried again once");
}

#[tokio::test]
async fn a_task_of_a_closed_request_or_archived_is_not_started() {
    let f = world();
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    let mut state = (*f.domain.state().unwrap()).clone();
    let task = state.tasks[&TaskId::from("f1")].clone();
    assert!(candidate(&state, &task));
    state.requests.get_mut(&"r".into()).unwrap().closed = true;
    assert!(!candidate(&state, &task), "its request is closed");
    state.requests.get_mut(&"r".into()).unwrap().closed = false;
    state.tasks.get_mut(&TaskId::from("f1")).unwrap().archived = true;
    assert!(!candidate(&state, &state.tasks[&TaskId::from("f1")].clone()), "it is archived");
}

#[tokio::test]
async fn a_manual_and_an_automatic_launch_never_both_start_a_task() {
    let f = world();
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    assert!(f.domain.live.begin_launch("f1"), "a manual launch is under way");
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 0, "the automatic one gives way");
    let body: LaunchBody = serde_json::from_value(json!({"task": "f1"})).unwrap();
    let refused = launch(&f.domain, &f.home, body, |_| None).await.unwrap_err();
    assert_eq!(refused.code, "conflict");
    assert!(refused.message.contains("is being launched already"), "{}", refused.message);
    f.domain.live.end_launch("f1");

    let body: LaunchBody = serde_json::from_value(json!({"task": "f1"})).unwrap();
    launch(&f.domain, &f.home, body, |_| None).await.unwrap();
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1, "a task with a node is not started again");
    let _unused: Option<Value> = None;
}

#[tokio::test]
async fn twelve_independent_tasks_with_a_plan_start_four_at_a_time_and_the_next_starts_when_one_closes() {
    let f = world();
    f.domain.live.gate.tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(4)) });
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
    let f = world();
    f.domain.live.gate.tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(1)) });
    create(&f, "a", &[], Some(&plan(&f, "app/a")));
    create(&f, "b", &[], Some(&plan(&f, "app/b")));
    let now = alondra_core::now_millis();
    let waiting = crate::admission::Waiting { key: crate::admission::Key::Task("a".into()), priority: alondra_core::domain::TaskPriority::Normal,
        resuming: false, since: 0 };
    assert_eq!(f.domain.live.gate.ask(&f.domain, waiting, now), Ok(()));
    assert!(f.domain.store.lock().unwrap().claim_auto_launch(&"a".into(), now, false).unwrap());
    assert!(f.domain.live.begin_launch("a"));
    let domain = f.domain.clone();
    let cancel: Box<dyn FnOnce() + Send> = Box::new(move || command(&domain, Actor::Orchestrator { generation: 0 }, Command::CancelTask { task_id: "a".into() }));
    let launched = crate::domain_orchestrator_tools::launch_guarded(&f.domain, &f.home, LaunchBody::from_plan("a".into(), &plan(&f, "app/a")), |_| None,
        Origin::Auto, Some(cancel)).await;
    f.domain.live.end_launch("a");
    assert!(launched.as_ref().is_err_and(|error| error.message.contains("Task is terminal")), "cancelled before its admission: {launched:?}");
    settle(&f.domain, &"a".into(), f.project.to_str().unwrap(), launched, now).unwrap();
    assert!(!f.domain.store.lock().unwrap().start_retries().unwrap().contains_key(&TaskId::from("a")), "no retry");
    assert!(problem(&f, "a").is_none(), "no card");
    assert!(auto(&f, "a").is_none());
    pass(&f.domain, &f.home).await.unwrap();
    pass(&f.domain, &f.home).await.unwrap();
    assert!(agent_of(&f, "b").is_some(), "the slot goes to the next task");
    assert!(agent_of(&f, "a").is_none());
}

struct NoSessions;
impl crate::node_close::Sessions for NoSessions {
    fn claude<'a>(&'a self, args: Vec<String>) -> crate::node_close::Answer<'a, Result<crate::node_close::Ran, String>> {
        panic!("the closer ran claude {args:?} for a task without a node")
    }
    fn leftovers<'a>(&'a self, session_id: &'a str) -> crate::node_close::Answer<'a, Option<String>> {
        panic!("the closer looked for leftovers of {session_id}, a task without a node")
    }
}

/// The worktree a start makes is the task's from the moment it exists: a task cancelled while it
/// starts, whose start fails, leaves no worktree behind once it closes.
#[tokio::test]
async fn the_worktree_made_for_a_task_cancelled_while_it_starts_goes_when_it_closes() {
    let f = world();
    for args in [&["init", "-q"][..], &["config", "user.email", "test@example.invalid"], &["config", "user.name", "Test"],
        &["commit", "-q", "--allow-empty", "-m", "base"]] {
        crate::worktree::testing::git(&f.project, args);
    }
    f.domain.live.gate.tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(1)) });
    create(&f, "a", &[], Some(&plan(&f, "app/a")));
    let now = alondra_core::now_millis();
    let waiting = crate::admission::Waiting { key: crate::admission::Key::Task("a".into()), priority: alondra_core::domain::TaskPriority::Normal,
        resuming: false, since: 0 };
    assert_eq!(f.domain.live.gate.ask(&f.domain, waiting, now), Ok(()));
    assert!(f.domain.store.lock().unwrap().claim_auto_launch(&"a".into(), now, false).unwrap());
    assert!(f.domain.live.begin_launch("a"));
    let domain = f.domain.clone();
    let cancel: Box<dyn FnOnce() + Send> = Box::new(move || command(&domain, Actor::Orchestrator { generation: 0 }, Command::CancelTask { task_id: "a".into() }));
    let launched = crate::domain_orchestrator_tools::launch_guarded(&f.domain, &f.home, LaunchBody::from_plan("a".into(), &plan(&f, "app/a")), |_| None,
        Origin::Auto, Some(cancel)).await;
    f.domain.live.end_launch("a");
    assert!(launched.is_err(), "cancelled before its admission: {launched:?}");
    settle(&f.domain, &"a".into(), f.project.to_str().unwrap(), launched, now).unwrap();
    let worktree = f.project.join(".claude/worktrees/a");
    assert!(worktree.join(".git").is_file(), "the start made the task's worktree");
    crate::task_closer::pass(&f.domain, &NoSessions, &crate::task_closer::Timing::new(None), alondra_core::now_millis()).await.unwrap();
    assert!(!worktree.exists(), "the cancelled task's worktree goes with its close");
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("a")].state, alondra_core::domain::TaskState::Cancelled);
}

fn problem(f: &Fixture, task: &str) -> Option<alondra_core::domain::Interaction> {
    f.domain.state().unwrap().open_start_problem(&task.into()).cloned()
}

fn answer(f: &Fixture, card: &InteractionId, option: &str) {
    command(&f.domain, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.clone(), option_id: option.into(), inputs: Default::default() });
}

fn retry_now(f: &Fixture, task: &str) {
    let mut store = f.domain.store.lock().unwrap();
    let (attempts, _) = store.start_retries().unwrap()[&TaskId::from(task)];
    store.set_start_retry(&task.into(), Some((attempts, 0))).unwrap();
}

/// What the BlurCamera weekly improvement did today: its request comes from a schedule, and the
/// orchestrator split it into tasks. One without a launch plan is refused and never exists; with a
/// plan that leaves model and effort to the schedule, it gets the schedule's and starts by itself.
#[tokio::test]
async fn a_task_of_a_scheduled_request_needs_its_plan_and_takes_the_schedules_model_and_effort() {
    let f = world();
    let options = BTreeMap::from([("effort".to_owned(), alondra_core::options::Choice::Value("hard".into()))]);
    command(&f.domain, Actor::System, Command::CreateSchedule { schedule: alondra_core::domain::Schedule {
        paid_account_grant: None,
        id: "weekly".into(), title: "BlurCamera · weekly improvement".into(), text: "Improve BlurCamera".into(), project: Some("App".into()),
        options: options.clone(), disallowed_tools: vec![], frequency: alondra_core::domain::ScheduleFrequency::Daily, hour: 9, minute: 0,
        weekday: None, interval: None, start_at: None, enabled: true, next_run: Some(100), revision: 1, deleted_at: None,
    } });
    command(&f.domain, Actor::System, Command::ClaimScheduleRun { schedule_id: "weekly".into(), run_id: "run".into(),
        origin: alondra_core::domain::ScheduleRunOrigin::Due, now: 100, next_after_now: Some(200), expected_schedule_revision: None });
    let resolved = alondra_core::options::OptionsFile::default().resolve(Some("App"), &options).unwrap();
    let mut request = alondra_core::domain::Request::new("weekly".into(), "Improve BlurCamera".into());
    request.title = Some("BlurCamera · weekly improvement".into());
    let task = alondra_core::domain::Task {
        id: "run".into(), request_id: "weekly".into(), agent_id: None, title: "BlurCamera · weekly improvement".into(), state: TaskState::NotStarted,
        revision: 0, started: false, review_prs: false, final_presentation: None, pr_id: None, dependencies: Default::default(), stack: None,
        documents: Default::default(), archived: false, model: None, delivered_dependencies: Default::default(), blocks: Default::default(),
        verdict: None, review_rounds: 0, wrap_up: Default::default(), priority: Default::default(), ending: None, stalls: 0, stall_closing: None, worktrees: Default::default(), account: None, provider: Default::default(), selection: None, pending_selection: None, allowed: Default::default(), project: None,
    };
    command(&f.domain, Actor::System, Command::RecordScheduleRunSent { run_id: "run".into(), request, task, finished_at: 101,
        launch: alondra_core::domain::ScheduleLaunchIntent { name: "scheduled/weekly-run".into(), cwd: f.project.to_str().unwrap().into(),
            prompt: "Improve BlurCamera".into(), options_json: serde_json::to_string(&resolved).unwrap() } });
    let choices = crate::domain_orchestrator_tools::request_choices(&f.domain, &"weekly".into()).unwrap().expect("the schedule's options are the request's");
    assert_eq!(choices.options.effort, "hard");

    let bearer = f.domain.bind_orchestrator("execution-weekly").unwrap().expose_for_child_environment().to_owned();
    let create = |id: &str, launch: Option<LaunchPlan>| {
        let state = f.domain.state().unwrap();
        f.domain.execute_bearer(&bearer, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("create-{id}").into(), expected_revision: 0,
            command: Command::CreateTask { plan_items: None, task_id: id.into(), request_id: "weekly".into(), title: "Register blurcamera.app".into(), agent_id: None,
                review_prs: true, dependencies: Default::default(), stack: None, launch },
        })
    };
    let refused = create("t-hf-web-registry-skills", None).unwrap_err();
    let crate::domain_store::StoreError::Domain(refused) = refused else { panic!("{refused:?}") };
    assert_eq!(refused.code, "launch_plan_required");
    assert!(!f.domain.state().unwrap().tasks.contains_key(&TaskId::from("t-hf-web-registry-skills")));

    let leave_it_to_the_schedule = LaunchPlan { model: None, reason: None, ..plan(&f, "agent-skills/hf-web-registry-skills") };
    crate::launch_plans::validate(&f.domain, &f.home, &"weekly".into(), &leave_it_to_the_schedule).unwrap();
    create("t-hf-web-registry-skills", Some(leave_it_to_the_schedule)).unwrap();
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1, "it starts by itself");
    let agent = agent_of(&f, "t-hf-web-registry-skills").expect("it started");
    let model = choices.options.flags.model.clone().expect("hard effort sets a model");
    assert_eq!(f.sdk.options(&agent.0).model, Some(model));
}

/// A task an older daemon let exist without a plan is cancelled with that reason, and the
/// orchestrator is told it can create it again.
#[tokio::test]
async fn a_task_left_without_a_plan_is_cancelled_saying_why() {
    let f = world();
    create(&f, "old", &[], Some(&plan(&f, "app/old")));
    f.domain.store.lock().unwrap().forget_launch_plan(&"old".into()).unwrap();
    recover(&f.domain).unwrap();
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("old")].state, TaskState::Cancelled);
    assert_eq!(f.domain.store.lock().unwrap().cancel_reasons().unwrap()[&TaskId::from("old")], PLANLESS);
    assert!(told(&f).contains(&"planless-cancelled-old".to_owned()), "{:?}", told(&f));
}

#[tokio::test]
async fn a_failed_start_is_tried_again_by_itself_then_put_to_the_user_who_can_try_again() {
    let f = world();
    create(&f, "f1", &[], Some(&plan(&f, "app/f1")));
    f.sdk.refuse_starts(true);
    for attempt in 1..=alondra_core::domain::START_RETRIES.len() as u8 {
        pass(&f.domain, &f.home).await.unwrap();
        assert_eq!(f.domain.store.lock().unwrap().start_retries().unwrap()[&TaskId::from("f1")].0, attempt);
        assert!(problem(&f, "f1").is_none(), "no card while Alondra tries again");
        pass(&f.domain, &f.home).await.unwrap();
        assert_eq!(f.domain.store.lock().unwrap().start_retries().unwrap()[&TaskId::from("f1")].0, attempt, "not before its time");
        retry_now(&f, "f1");
    }
    pass(&f.domain, &f.home).await.unwrap();
    let card = problem(&f, "f1").expect("the user is asked once the retries are spent");
    assert_eq!(card.card.need, "“Task f1” does not start");
    let body = &card.card.content.as_ref().unwrap().body;
    assert!(body.contains("did not start through the Agent SDK") && body.contains("Permission denied"), "the card says why: {body}");
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(f.domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with("start-problem-f1")).count(), 1, "one card");

    f.sdk.refuse_starts(false);
    answer(&f, &card.id, alondra_core::domain::START_RETRY);
    pass(&f.domain, &f.home).await.unwrap();
    pass(&f.domain, &f.home).await.unwrap();
    assert!(agent_of(&f, "f1").is_some(), "the user's try again starts it");
}

fn untrusted_config(f: &Fixture) -> std::path::PathBuf {
    let config = f.root.join(".claude.json");
    crate::claude_trust::use_config_for_test(&config);
    let project = f.project.canonicalize().unwrap();
    std::fs::write(&config, json!({"projects": {project.to_str().unwrap(): {"hasTrustDialogAccepted": false}}}).to_string()).unwrap();
    config
}

fn trusted(f: &Fixture, config: &Path) -> bool {
    let document: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config).unwrap()).unwrap();
    document["projects"][f.project.canonicalize().unwrap().to_str().unwrap()]["hasTrustDialogAccepted"] == true
}

/// Claude Code does not trust the folder: the user is asked at once, and choosing to trust it
/// records that trust as Claude Code's own prompt does and starts the task, so the card never
/// comes back.
#[tokio::test]
async fn trusting_an_untrusted_folder_from_its_card_starts_the_task() {
    let f = world();
    create(&f, "f1", &[], Some(&plan(&f, "app/f1")));
    let config = untrusted_config(&f);
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 0);
    assert_eq!(auto(&f, "f1").unwrap().reason, Some(AutoLaunchReason::WorkspaceUntrusted));
    let card = problem(&f, "f1").expect("only the user can trust it");
    assert_eq!(card.card.need, "Trust App so “Task f1” can start");

    answer(&f, &card.id, alondra_core::domain::START_RETRY);
    pass(&f.domain, &f.home).await.unwrap();
    pass(&f.domain, &f.home).await.unwrap();

    assert!(trusted(&f, &config), "the user's choice is saved where Claude Code reads it");
    assert_eq!(launches(&f), 1);
    assert!(agent_of(&f, "f1").is_some());
    pass(&f.domain, &f.home).await.unwrap();
    assert!(problem(&f, "f1").is_none(), "the card does not come back");
    assert_eq!(f.domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with("start-problem-f1")).count(), 1);
}

/// When Alondra cannot save the trust, the task does not start and a new card says why.
#[tokio::test]
async fn a_trust_alondra_cannot_save_comes_back_saying_why() {
    let f = world();
    create(&f, "f1", &[], Some(&plan(&f, "app/f1")));
    let config = untrusted_config(&f);
    pass(&f.domain, &f.home).await.unwrap();
    let card = problem(&f, "f1").expect("asked");
    let lock = f.root.join(".claude.json.lock");
    std::fs::create_dir(&lock).unwrap();

    answer(&f, &card.id, alondra_core::domain::START_RETRY);
    pass(&f.domain, &f.home).await.unwrap();
    pass(&f.domain, &f.home).await.unwrap();

    assert_eq!(launches(&f), 0);
    assert!(!trusted(&f, &config));
    let again = problem(&f, "f1").expect("a new card with the reason");
    assert_ne!(again.id, card.id);
    let Some(StartProblem::Untrusted { error: Some(error), .. }) = alondra_core::start_problem::from_body(&again.body) else { panic!("{}", again.body) };
    assert!(error.contains("locked"), "{error}");
    assert!(again.card.why.starts_with("You chose to trust it, but Alondra could not save"), "{}", again.card.why);

    std::fs::remove_dir(&lock).unwrap();
    answer(&f, &again.id, alondra_core::domain::START_RETRY);
    pass(&f.domain, &f.home).await.unwrap();
    pass(&f.domain, &f.home).await.unwrap();
    assert!(trusted(&f, &config));
    assert_eq!(launches(&f), 1);
}

/// A folder inside the projects folder the user chose at setup is trusted by that choice: the
/// task starts with no card.
#[tokio::test]
async fn a_folder_inside_the_projects_folder_starts_without_a_card() {
    let f = world();
    std::fs::create_dir_all(alondra_core::options::path_in(&f.home).parent().unwrap()).unwrap();
    std::fs::write(alondra_core::options::path_in(&f.home), format!("projects_root = {:?}\n", f.root.to_str().unwrap())).unwrap();
    create(&f, "f1", &[], Some(&plan(&f, "app/f1")));
    let config = untrusted_config(&f);

    pass(&f.domain, &f.home).await.unwrap();

    assert!(trusted(&f, &config));
    assert_eq!(launches(&f), 1);
    assert!(problem(&f, "f1").is_none());
}

/// 20 October: a cancelled dependency put a card in front of the user at once. The task never
/// starts while it waits for a cancelled task; the orchestrator is told and reminded, and only if
/// it leaves the task as it is does the user decide. Starting it without the cancelled task then
/// starts it.
#[tokio::test]
async fn a_cancelled_dependency_goes_to_the_orchestrator_first_and_then_to_the_user() {
    let f = world();
    open_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    command(&f.domain, user(), Command::CancelTask { task_id: "f0".into() });
    pass(&f.domain, &f.home).await.unwrap();
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 0);
    assert!(problem(&f, "f1").is_none(), "the orchestrator re-plans it first");

    crate::orchestrator_inbox::settle(&f.domain);
    let notices = || told(&f).into_iter().filter(|id| id.starts_with("dependency-cancelled-f1--f0-")).collect::<Vec<_>>();
    assert_eq!(notices(), ["dependency-cancelled-f1--f0-1"]);
    let state = f.domain.state().unwrap();
    let notice = &state.interactions[&"dependency-cancelled-f1--f0-1".into()].body;
    assert!(notice.contains("“Task f1” (f1) waits for “Task f0” (f0), which was cancelled") && notice.contains("set_dependencies")
        && notice.contains("cancel_task"), "{notice}");
    crate::orchestrator_inbox::settle(&f.domain);
    assert_eq!(notices().len(), 2, "one reminder");
    assert!(problem(&f, "f1").is_none());
    crate::orchestrator_inbox::settle(&f.domain);
    let card = problem(&f, "f1").expect("the user decides once the orchestrator left it");
    assert_eq!(card.card.need, "“Task f0” was cancelled: start “Task f1” without it?");
    crate::orchestrator_inbox::settle(&f.domain);
    assert_eq!(f.domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with("start-problem-f1")).count(), 1);
    assert_eq!(notices().len(), 2);

    answer(&f, &card.id, alondra_core::domain::START_WITHOUT);
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1);
}

/// The orchestrator replaces the cancelled task, before or after it was told: the task follows
/// its new dependencies, nobody else is asked, and a card already asking the user is withdrawn.
#[tokio::test]
async fn a_task_the_orchestrator_re_points_follows_its_replacement_and_its_card_is_withdrawn() {
    let f = world();
    open_task(&f, "f0");
    open_task(&f, "f2");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    command(&f.domain, user(), Command::CancelTask { task_id: "f0".into() });
    for _ in 0..3 {
        crate::orchestrator_inbox::settle(&f.domain);
    }
    let card = problem(&f, "f1").expect("the user was asked");
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::SetDependencies {
        task_id: "f1".into(), dependencies: ["f2".into()].into(), stack: None,
    });
    crate::orchestrator_inbox::settle(&f.domain);
    assert!(f.domain.state().unwrap().interactions[&card.id].withdrawn, "the card no longer asks anything");
    assert!(problem(&f, "f1").is_none());
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 0, "it waits for its replacement");
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("f1")].state, TaskState::WaitingDependency);
    let before = told(&f).len();
    crate::orchestrator_inbox::settle(&f.domain);
    assert_eq!(told(&f).len(), before, "nothing more for the orchestrator or the user");
}

/// The orchestrator cancels a task and re-points the one that waited for it in the same batch:
/// nobody is told anything.
#[tokio::test]
async fn cancelling_and_re_pointing_in_the_same_batch_tells_nobody() {
    let f = world();
    open_task(&f, "f0");
    open_task(&f, "f2");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CancelTask { task_id: "f0".into() });
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::SetDependencies {
        task_id: "f1".into(), dependencies: ["f2".into()].into(), stack: None,
    });
    crate::orchestrator_inbox::settle(&f.domain);
    assert!(!told(&f).iter().any(|id| id.starts_with("dependency-cancelled-")));
    assert!(problem(&f, "f1").is_none());
}

#[tokio::test]
async fn a_delivery_dependency_cancelled_goes_to_the_user_once_the_orchestrator_leaves_it_and_cancel_closes_the_task() {
    let f = world();
    open_task(&f, "a");
    create(&f, "after", &[], Some(&plan(&f, "app/after")));
    command(&f.domain, user(), Command::SetRelations { task_id: "after".into(), delivered_dependencies: ["a".into()].into(), blocks: Default::default() });
    command(&f.domain, user(), Command::CancelTask { task_id: "a".into() });
    pass(&f.domain, &f.home).await.unwrap();
    assert!(problem(&f, "after").is_none());
    for _ in 0..3 {
        crate::orchestrator_inbox::settle(&f.domain);
    }
    let card = problem(&f, "after").expect("the user decides");
    answer(&f, &card.id, alondra_core::domain::START_CANCEL);
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("after")].state, TaskState::Cancelled);
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 0);
}

/// The user closed the node of an open task: they are asked to relaunch it or cancel it, and a
/// relaunch starts a new node in the closed one's folder.
#[tokio::test]
async fn a_node_the_user_closes_asks_to_relaunch_and_the_relaunch_starts_a_new_node() {
    let f = world_using_sdk(SdkLaunches::enable);
    create(&f, "f1", &[], Some(&plan(&f, "app/f1")));
    pass(&f.domain, &f.home).await.unwrap();
    f.sdk.until_answered("f1").await;
    let node = agent_of(&f, "f1").expect("its original SDK node");
    let original = f.sdk.starts()[0]["session"].as_str().unwrap().to_owned();
    let original_account = f.domain.state().unwrap().tasks[&TaskId::from("f1")].account_key();
    f.domain.stop_sdk_host(&node).unwrap();
    let (backend, runtime) = f.domain.sdk().unwrap();
    crate::sdk_owner::end_unowned_host(runtime, backend, "f1".into()).unwrap();
    crate::node_revival::take_out(&f.domain, &node.0, "node closed by the user", Some(f.project.to_str().unwrap().into())).unwrap();
    assert!(agent_of(&f, "f1").is_none());
    let state = f.domain.state().unwrap();
    let cases: Vec<_> = state.interactions.values()
        .filter(|card| card.scope == (alondra_core::domain::Scope::Task { task_id: "f1".into() }) && alondra_core::domain::is_stall_case(card) && state.open(card))
        .map(|card| card.id.clone()).collect();
    for case in cases {
        command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::SettleStall {
            case_id: case, task_id: "f1".into(), outcome: alondra_core::domain::StallOutcome::Instruct { text: "Go on with your task".into() },
        });
    }
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("f1")].state, TaskState::NotStarted);
    pass(&f.domain, &f.home).await.unwrap();
    let card = problem(&f, "f1").expect("the user decides");
    assert_eq!(card.card.need, "Relaunch “Task f1” or cancel it");
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(f.domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with("start-problem-f1")).count(), 1, "one card");

    answer(&f, &card.id, alondra_core::domain::START_RETRY);
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 2, "one original node and its replacement");
    assert!(agent_of(&f, "f1").is_some_and(|agent| agent != node));
    let recovered = agent_of(&f, "f1").unwrap();
    let mut changed = f.domain.subscribe();
    loop {
        let ready = {
            let state = f.domain.state().unwrap();
            let lifecycle = state.lifecycle_of(&TaskId::from("f1")).unwrap();
            let execution = lifecycle.execution().expect("the recovered node has its own admission");
            assert_eq!(execution.node(), &recovered);
            assert_eq!(execution.generation(), 2);
            lifecycle.resource() == Some(alondra_domain::ResourceEvidence::Available)
        };
        if ready { break; }
        changed.changed().await.unwrap();
    }
    let starts = f.sdk.starts();
    let resumed = starts.iter().find(|start| start["resume"].as_str() == Some(original.as_str())).expect("relaunch resumes the original native session");
    assert!(resumed["history"].as_u64().unwrap() > 0, "relaunch retains the original native cache and history: {starts:?}");
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("f1")].account_key(), original_account);
    pass(&f.domain, &f.home).await.unwrap();
    assert!(problem(&f, "f1").is_none(), "nothing more to ask");
}

fn queued(f: &Fixture) -> Vec<(String, i64)> {
    f.domain.store.lock().unwrap().launch_queue().unwrap().into_iter().map(|row| (row.task_id.0, row.enqueued_at)).collect()
}

/// 15 October: t-fastconn-5-remove-mailbox was cancelled at 19:42 and reopened at 19:45. It stayed
/// in progress with no node, nothing started it, and once launched by hand the queue took it out of
/// its queue before starting it and was refused («Task already has a node»), so it was in no queue
/// at all. Reopened, it goes back to the queue with its plan and its seniority; a start the gate
/// cannot make leaves it there, and its retry starts it.
#[tokio::test]
async fn a_reopened_task_waits_in_the_queue_with_its_seniority_and_a_refused_start_keeps_it_there() {
    let f = world_using_sdk(SdkLaunches::enable);
    f.domain.live.gate.ignore_mac();
    create(&f, "f1", &[], Some(&plan(&f, "app/f1")));
    pass(&f.domain, &f.home).await.unwrap();
    f.sdk.until_answered("f1").await;
    let first = agent_of(&f, "f1").expect("its first node");
    let original = f.sdk.starts()[0]["session"].as_str().unwrap().to_owned();
    let original_account = f.domain.state().unwrap().tasks[&TaskId::from("f1")].account_key();
    let planned_at = f.domain.store.lock().unwrap().planned_tasks().unwrap()[&TaskId::from("f1")];

    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CancelTask { task_id: "f1".into() });
    f.domain.stop_sdk_host(&first).unwrap();
    command(&f.domain, Actor::System, Command::RecordNodeEnd { task_id: "f1".into(), end: NodeEnd::Closed, agent_id: Some(first) });
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::ReopenTask { task_id: "f1".into() });
    let task = f.domain.state().unwrap().tasks[&TaskId::from("f1")].clone();
    assert_eq!((task.state, task.started, task.agent_id), (TaskState::NotStarted, false, None), "never in progress with no node");
    assert_eq!(queued(&f), [("f1".to_owned(), planned_at)], "queued with the place its plan gave it");
    assert!(auto(&f, "f1").is_none(), "its first automatic launch belongs to its first life");

    create(&f, "f2", &[], Some(&plan(&f, "app/f2")));
    let body: LaunchBody = serde_json::from_value(json!({"task": "f2", "name": "app/f2", "cwd": f.project.to_str().unwrap()})).unwrap();
    crate::admission::enqueue(&f.domain, &body, "orchestrator", alondra_core::now_millis()).unwrap();
    let now = alondra_core::now_millis();
    let snapshot = f.domain.live.gate.snapshot(&f.domain, now).unwrap();
    let order: Vec<String> = snapshot.queue.iter().map(|waiting| format!("{:?}", waiting.key)).collect();
    assert_eq!(order[0], "Task(TaskId(\"f1\"))", "reopened, it keeps its place ahead of later work: {order:?}");
    assert_eq!(snapshot.working, 0, "back in the queue, its first start no longer counts as one under way");

    f.sdk.refuse_starts(true);
    crate::admission::pass_at(&f.domain, &f.home, now).await.unwrap();
    assert!(agent_of(&f, "f1").is_none());
    assert!(queued(&f).iter().any(|(task, _)| task == "f1"), "a refused start never takes it out of the queue: {:?}", queued(&f));
    let retry = f.domain.store.lock().unwrap().start_retries().unwrap()[&TaskId::from("f1")];
    assert!(retry.1 > now, "it waits for its retry");
    let order: Vec<String> = f.domain.live.gate.snapshot(&f.domain, now).unwrap().queue.iter()
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
async fn a_task_with_no_launch_plan_cannot_be_reopened_because_nothing_could_start_it() {
    let f = world();
    create(&f, "f1", &[], Some(&plan(&f, "app/f1")));
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CancelTask { task_id: "f1".into() });
    f.domain.store.lock().unwrap().forget_launch_plan(&"f1".into()).unwrap();
    let state = f.domain.state().unwrap();
    let refused = f.domain.execute(&Actor::Orchestrator { generation: 0 }, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "reopen-planless".into(),
        expected_revision: state.revision, command: Command::ReopenTask { task_id: "f1".into() },
    }, None, || Ok(())).unwrap_err();
    assert!(format!("{refused:?}").contains("no launch plan"), "{refused:?}");
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("f1")].state, TaskState::Cancelled);
    assert!(queued(&f).is_empty());
}

/// 15 October: the user chose 512 px prints on Redraft's phase B; phase C waited with a plan for
/// the 360 px copy. It starts only after the orchestrator reviewed it, with the plan it gave.
#[tokio::test]
async fn a_dependent_held_by_the_users_decision_starts_after_its_review_with_the_new_plan() {
    let f = world();
    open_task(&f, "b");
    command(&f.domain, Actor::Agent { agent_id: "seed-b".into() }, Command::StartTask { task_id: "b".into() });
    create(&f, "c", &["b"], Some(&LaunchPlan { mission: Some("Group with the 360 px copy".into()), ..plan(&f, "app/c") }));
    command(&f.domain, Actor::Agent { agent_id: "seed-b".into() }, Command::AskQuestion {
        attachments: vec![], interaction_id: "decision".into(), scope: alondra_core::domain::Scope::Task { task_id: "b".into() },
        recipient: alondra_core::domain::Recipient::User, body: "11 of 21 groups match".into(), blocking: true,
        options: vec![alondra_core::domain::QuestionOption { id: "threshold".into(), label: "Adjust the threshold".into(), description: None },
            alondra_core::domain::QuestionOption { id: "plan-512".into(), label: "512 px prints".into(), description: None }],
        card: alondra_core::domain::CardSpec::new(alondra_core::domain::CardKind::Decide, "Decide the index's next step").why("Phase C starts after it"),
    });
    command(&f.domain, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "decision".into(),
        option_id: "plan-512".into(), inputs: Default::default() });
    finish(&f, "b", "What b delivered", false);
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 0, "its dependency is done but its plan is not reviewed");
    assert!(auto(&f, "c").is_none(), "nothing claims it while it is held");
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::SettlePlanReview { review_id: "decision".into(), task_id: "c".into(),
        outcome: alondra_core::domain::PlanReviewOutcome::PlanChanged {
            launch: LaunchPlan { mission: Some("Group with 512 px prints".into()), ..plan(&f, "app/c-512") }, reason: "The user chose 512 px".into() } });
    pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(launches(&f), 1);
    assert_eq!(node_name(&f, "c"), "app/c-512");
    assert!(prompt(&f, "c").contains("Group with 512 px prints") && !prompt(&f, "c").contains("360 px"), "{}", prompt(&f, "c"));
}

fn queued_ids(f: &Fixture) -> Vec<(String, bool)> {
    let pacing = f.domain.live.gate.live(&f.domain, alondra_core::now_millis()).unwrap();
    pacing.queued.into_iter().map(|queued| (queued.task_id, queued.pausable)).collect()
}

fn paused_ids(f: &Fixture) -> Vec<String> {
    let pacing = f.domain.live.gate.live(&f.domain, alondra_core::now_millis()).unwrap();
    pacing.paused.into_iter().map(|paused| paused.task_id).collect()
}

#[tokio::test]
async fn pausing_a_task_waiting_for_its_automatic_start_takes_it_out_of_the_queue_until_the_user_resumes_it() {
    let f = world();
    f.domain.live.gate.tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(1)) });
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
    assert_eq!(f.domain.live.gate.snapshot(&f.domain, alondra_core::now_millis()).unwrap().working, 0, "q2 takes no slot while paused");
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
    let f = world();
    f.domain.live.gate.tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(1)) });
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
    let f = world();
    done_task(&f, "n0");
    open_task(&f, "n1");
    assert_eq!(crate::admission::hold_queued(&f.domain, &"n1".into(), alondra_core::now_millis()), Err("This task is not queued"));
    assert_eq!(crate::admission::hold_queued(&f.domain, &"n0".into(), alondra_core::now_millis()), Err("This task is not queued"));
}
