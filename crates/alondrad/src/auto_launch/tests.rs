use super::*;
use crate::domain_orchestrator_tools::{LaunchBody, launch};
use crate::launch_plans::LaunchPlan;
use alondra_core::domain::{
    StartProblem,
    Command, CommandEnvelope, DeliveryTarget, InteractionId, MergeMethod, NodeEnd, PROTOCOL_VERSION, PrState, TaskId,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

struct Fixture {
    root: crate::test_support::Directory,
    home: PathBuf,
    project: PathBuf,
    domain: Arc<DomainRuntime>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
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
    let root = crate::test_support::Directory::new();
    let home = root.join("home");
    let project = root.join("App");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let domain = Arc::new(DomainRuntime::open(&home).unwrap());
    command(&domain, user(), Command::CreateRequest { request_id: "r".into(), body: "Rebuild the editor".into(), auto_merge: false, maintenance: false, message: false });
    command(&domain, user(), Command::TitleRequest { request_id: "r".into(), title: "Editor".into() });
    Fixture { root, home, project, domain }
}

fn plan(f: &Fixture, name: &str) -> LaunchPlan {
    LaunchPlan {
        name: name.into(), cwd: f.project.to_str().unwrap().into(), mission: None, model: Some("sonnet".into()), effort: None,
        approach: None, reason: Some("A clear, mechanical change".into()), role: None,
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

struct FakeClaude {
    domain: Arc<DomainRuntime>,
    calls: Mutex<Vec<(String, Vec<String>)>>,
    stopped: Mutex<Vec<String>>,
    removed: Mutex<Vec<String>>,
    refuse: Mutex<Option<String>>,
    during_run: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
impl FakeClaude {
    fn new(f: &Fixture) -> Self {
        Self { domain: f.domain.clone(), calls: Default::default(), stopped: Default::default(), removed: Default::default(),
            refuse: Default::default(), during_run: Default::default() }
    }
    fn launches(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
    fn prompt(&self, index: usize) -> String {
        self.calls.lock().unwrap()[index].1.last().unwrap().clone()
    }
}
impl crate::orchestrate::Launcher for FakeClaude {
    async fn run(&self, cwd: &str, args: &[String]) -> Result<String, crate::orchestrate::Refusal> {
        if let Some(reason) = self.refuse.lock().unwrap().clone() {
            return Err((axum::http::StatusCode::BAD_GATEWAY, reason));
        }
        let number = {
            let mut calls = self.calls.lock().unwrap();
            calls.push((cwd.into(), args.to_vec()));
            calls.len()
        };
        let session = format!("ab12{number:04x}-0000-4000-8000-000000000000");
        self.domain.live.mods.loaded_by(&session, args);
        if let Some(hook) = self.during_run.lock().unwrap().take() {
            hook();
        }
        Ok(format!("backgrounded · ab12{number:04x} · editor"))
    }
    async fn stop(&self, short: &str) {
        self.stopped.lock().unwrap().push(short.into());
    }
    async fn remove(&self, short: &str) {
        self.removed.lock().unwrap().push(short.into());
    }
    fn mods_deadline(&self) -> Duration {
        Duration::from_millis(20)
    }
    fn plugin(&self) -> Option<PathBuf> {
        Some("/synthetic/plugin".into())
    }
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
    let claude = FakeClaude::new(&f);
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    create(&f, "f2", &["f1"], Some(&plan(&f, "app/f2")));
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 1, "f2 waits for f1");
    let first = claude.calls.lock().unwrap()[0].1.clone();
    assert!(first.windows(2).any(|pair| pair[0] == "--model"), "{first:?}");
    assert!(first.windows(2).any(|pair| pair == ["--name", "app/f1"]), "{first:?}");
    assert!(claude.prompt(0).contains("### “Task f0” (done)\nWhat f0 delivered"), "{}", claude.prompt(0));
    assert_eq!(auto(&f, "f1").unwrap().state, AutoLaunchState::Launched);

    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 1, "a second pass starts nothing");
    finish(&f, "f1", "What f1 delivered", true);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 2, "{:?} {:?}", auto(&f, "f2"), told(&f));
    assert!(claude.prompt(1).contains("### “Task f1” (done)\nWhat f1 delivered\nPull request: https://github.com/me/app/pull/7 (merged)"), "{}", claude.prompt(1));
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 2);
    assert!(agent_of(&f, "f2").is_some());
    let decisions = f.domain.store.lock().unwrap().model_decision_rows().unwrap();
    let launches: Vec<_> = decisions.iter().filter(|row| row.1 == "launch").map(|row| (row.0.clone(), row.5.clone())).collect();
    assert_eq!(launches, [("f1".to_owned(), Some("A clear, mechanical change".to_owned())), ("f2".to_owned(), Some("A clear, mechanical change".to_owned()))]);
}

#[tokio::test]
async fn a_review_starts_by_itself_when_the_task_it_reviews_delivers_and_its_mission_has_that_delivery() {
    let f = world();
    let claude = FakeClaude::new(&f);
    open_task(&f, "a");
    create(&f, "rev", &[], Some(&plan(&f, "app/rev")));
    command(&f.domain, user(), Command::SetRelations {
        task_id: "rev".into(), delivered_dependencies: ["a".into()].into(), blocks: ["a".into()].into(),
    });
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 0, "nothing to review before the delivery");
    let author = Actor::Agent { agent_id: "seed-a".into() };
    command(&f.domain, author.clone(), Command::RegisterPr { task_id: "a".into(), pr_id: "pr-a".into(), host: "github.com".into(),
        repository: "me/app".into(), number: 9, head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash] });
    command(&f.domain, author, Command::PresentResult { checklist: Vec::new(), attachments: vec![], task_id: "a".into(), interaction_id: "final-a".into(),
        body: "What a delivered".into(), targets: vec![DeliveryTarget::PullRequest { pr_id: "pr-a".into(), head: "head1".into() }],
        is_final: true, nothing_to_review: false, card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true) });
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 1, "the review starts at the delivery, not at the merge");
    let prompt = claude.prompt(0);
    assert!(prompt.contains("### “Task a” (delivered, not finished yet)\nWhat a delivered\nPull request: https://github.com/me/app/pull/9 (open)"), "{prompt}");
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 1);
}

#[tokio::test]
async fn while_usage_is_exhausted_a_ready_task_waits_and_starts_when_usage_is_back() {
    let f = world();
    let claude = FakeClaude::new(&f);
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    let now = alondra_core::now_millis();
    f.domain.live.usage_pause.exhaust(Some(now + 3_600_000), now);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 0);
    assert_eq!(auto(&f, "f1").unwrap().reason, Some(AutoLaunchReason::UsageExhausted));
    assert!(told(&f).iter().all(|id| !id.starts_with("auto-launch")), "the orchestrator is not told: it can do nothing");

    f.domain.live.usage_pause.resume(alondra_core::now_millis());
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 1);
    assert_eq!(auto(&f, "f1").unwrap().state, AutoLaunchState::Launched);
}

#[tokio::test]
async fn while_the_disk_is_nearly_full_a_ready_task_waits_and_starts_once_there_is_space() {
    let f = world();
    let claude = FakeClaude::new(&f);
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    f.domain.live.machine_watch.set_disk_low(true);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 0);
    assert_eq!(auto(&f, "f1"), None, "not claimed: it starts by itself once there is space");
    assert!(told(&f).iter().all(|id| !id.starts_with("auto-launch")), "the orchestrator is not told: the user has the disk card");

    f.domain.live.machine_watch.set_disk_low(false);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 1);
    assert_eq!(auto(&f, "f1").unwrap().state, AutoLaunchState::Launched);
}

#[tokio::test]
async fn a_task_cancelled_while_its_launch_runs_stops_and_removes_the_new_session() {
    let f = world();
    let claude = FakeClaude::new(&f);
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    let domain = f.domain.clone();
    *claude.during_run.lock().unwrap() = Some(Box::new(move || {
        command(&domain, Actor::User { user_id: "u".into() }, Command::CancelTask { task_id: "f1".into() });
    }));
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(*claude.stopped.lock().unwrap(), ["ab120001"]);
    assert_eq!(*claude.removed.lock().unwrap(), ["ab120001"]);
    let state = f.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from("f1")].state, TaskState::Cancelled);
    assert_eq!(state.tasks[&TaskId::from("f1")].agent_id, None);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 1, "a cancelled task is not started again");
}

#[tokio::test]
async fn two_dependencies_done_together_start_the_task_once() {
    let f = world();
    let claude = FakeClaude::new(&f);
    open_task(&f, "a");
    open_task(&f, "b");
    create(&f, "c", &["a", "b"], Some(&plan(&f, "app/c")));
    finish(&f, "a", "What a delivered", false);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 0, "b is not done yet");
    finish(&f, "b", "What b delivered", false);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 1);
    let prompt = claude.prompt(0);
    assert!(prompt.contains("What a delivered") && prompt.contains("What b delivered"), "{prompt}");
}

#[tokio::test]
async fn a_task_whose_node_the_user_closed_is_not_started_again_by_itself() {
    let f = world();
    let claude = FakeClaude::new(&f);
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    f.domain.live.remember_closed_task("f1", crate::client_routes::ClosedNode { name: "app/f1".into(), cwd: None });
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 0);
    assert_eq!(auto(&f, "f1"), None);
}

#[tokio::test]
async fn a_launch_interrupted_by_a_restart_is_tried_again_once() {
    let f = world();
    let claude = FakeClaude::new(&f);
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    assert!(f.domain.store.lock().unwrap().claim_auto_launch(&"f1".into(), 1, false).unwrap());
    assert_eq!(auto(&f, "f1").unwrap().state, AutoLaunchState::Launching);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 0, "a launching row is another pass's");
    recover(&f.domain).unwrap();
    assert_eq!(auto(&f, "f1"), None);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 1);
    recover(&f.domain).unwrap();
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 1, "tried again once");
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
    let claude = FakeClaude::new(&f);
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    assert!(f.domain.live.begin_launch("f1"), "a manual launch is under way");
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 0, "the automatic one gives way");
    let orchestrator = {
        let capability = f.domain.bind_orchestrator("execution-1").unwrap();
        crate::domain_orchestrator_tools::orchestrator(&f.domain, Some(capability.expose_for_child_environment()), None).unwrap()
    };
    let body: LaunchBody = serde_json::from_value(json!({"task": "f1"})).unwrap();
    let refused = launch(&f.domain, &f.home, &orchestrator, body, |_| None, &claude).await.unwrap_err();
    assert_eq!(refused.code, "conflict");
    assert!(refused.message.contains("is being launched already"), "{}", refused.message);
    f.domain.live.end_launch("f1");

    let body: LaunchBody = serde_json::from_value(json!({"task": "f1"})).unwrap();
    launch(&f.domain, &f.home, &orchestrator, body, |_| None, &claude).await.unwrap();
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 1, "a task with a node is not started again");
    let _unused: Option<Value> = None;
}

#[tokio::test]
async fn twelve_independent_tasks_with_a_plan_start_four_at_a_time_and_the_next_starts_when_one_closes() {
    let f = world();
    let claude = FakeClaude::new(&f);
    f.domain.live.gate.tune(crate::fleet_gate::Tuning { spacing: 0, limit: Some(crate::fleet_gate::Limit::Fixed(4)) });
    let ids: Vec<String> = (1..=12).map(|n| format!("i{n:02}")).collect();
    for id in &ids {
        create(&f, id, &[], Some(&plan(&f, &format!("app/{id}"))));
    }
    pass(&f.domain, &f.home, &claude).await.unwrap();
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 4, "the gate lets four start");
    let started: Vec<&String> = ids.iter().filter(|id| agent_of(&f, id).is_some()).collect();
    assert_eq!(started, ["i01", "i02", "i03", "i04"], "the oldest plans go first");
    assert!(told(&f).iter().all(|id| !id.starts_with("auto-launch")), "{:?}", told(&f));

    finish(&f, "i02", "What i02 delivered", false);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 5, "a closed task frees its slot for the next");
    assert!(agent_of(&f, "i05").is_some());
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 5);
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
    let claude = FakeClaude::new(&f);
    let options = BTreeMap::from([("effort".to_owned(), alondra_core::options::Choice::Value("hard".into()))]);
    command(&f.domain, Actor::System, Command::CreateSchedule { schedule: alondra_core::domain::Schedule {
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
        verdict: None, review_rounds: 0, wrap_up: Default::default(), priority: Default::default(), ending: None, worktrees: Default::default(),
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
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 1, "it starts by itself");
    let args = claude.calls.lock().unwrap()[0].1.clone();
    let model = choices.options.flags.model.clone().expect("hard effort sets a model");
    assert!(args.windows(2).any(|pair| pair[0] == "--model" && pair[1] == model), "{args:?}");
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
    let claude = FakeClaude::new(&f);
    create(&f, "f1", &[], Some(&plan(&f, "app/f1")));
    *claude.refuse.lock().unwrap() = Some("claude is not logged in".into());
    for attempt in 1..=alondra_core::domain::START_RETRIES.len() as u8 {
        pass(&f.domain, &f.home, &claude).await.unwrap();
        assert_eq!(f.domain.store.lock().unwrap().start_retries().unwrap()[&TaskId::from("f1")].0, attempt);
        assert!(problem(&f, "f1").is_none(), "no card while Alondra tries again");
        pass(&f.domain, &f.home, &claude).await.unwrap();
        assert_eq!(f.domain.store.lock().unwrap().start_retries().unwrap()[&TaskId::from("f1")].0, attempt, "not before its time");
        retry_now(&f, "f1");
    }
    pass(&f.domain, &f.home, &claude).await.unwrap();
    let card = problem(&f, "f1").expect("the user is asked once the retries are spent");
    assert_eq!(card.card.need, "“Task f1” does not start");
    assert_eq!(card.card.content.as_ref().unwrap().body, "claude is not logged in");
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(f.domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with("start-problem-f1")).count(), 1, "one card");

    *claude.refuse.lock().unwrap() = None;
    answer(&f, &card.id, alondra_core::domain::START_RETRY);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    pass(&f.domain, &f.home, &claude).await.unwrap();
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
    let claude = FakeClaude::new(&f);
    create(&f, "f1", &[], Some(&plan(&f, "app/f1")));
    let config = untrusted_config(&f);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 0);
    assert_eq!(auto(&f, "f1").unwrap().reason, Some(AutoLaunchReason::WorkspaceUntrusted));
    let card = problem(&f, "f1").expect("only the user can trust it");
    assert_eq!(card.card.need, "Trust App so “Task f1” can start");

    answer(&f, &card.id, alondra_core::domain::START_RETRY);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    pass(&f.domain, &f.home, &claude).await.unwrap();

    assert!(trusted(&f, &config), "the user's choice is saved where Claude Code reads it");
    assert_eq!(claude.launches(), 1);
    assert!(agent_of(&f, "f1").is_some());
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert!(problem(&f, "f1").is_none(), "the card does not come back");
    assert_eq!(f.domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with("start-problem-f1")).count(), 1);
}

/// When Alondra cannot save the trust, the task does not start and a new card says why.
#[tokio::test]
async fn a_trust_alondra_cannot_save_comes_back_saying_why() {
    let f = world();
    let claude = FakeClaude::new(&f);
    create(&f, "f1", &[], Some(&plan(&f, "app/f1")));
    let config = untrusted_config(&f);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    let card = problem(&f, "f1").expect("asked");
    let lock = f.root.join(".claude.json.lock");
    std::fs::create_dir(&lock).unwrap();

    answer(&f, &card.id, alondra_core::domain::START_RETRY);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    pass(&f.domain, &f.home, &claude).await.unwrap();

    assert_eq!(claude.launches(), 0);
    assert!(!trusted(&f, &config));
    let again = problem(&f, "f1").expect("a new card with the reason");
    assert_ne!(again.id, card.id);
    let Some(StartProblem::Untrusted { error: Some(error), .. }) = alondra_core::start_problem::from_body(&again.body) else { panic!("{}", again.body) };
    assert!(error.contains("locked"), "{error}");
    assert!(again.card.why.starts_with("You chose to trust it, but Alondra could not save"), "{}", again.card.why);

    std::fs::remove_dir(&lock).unwrap();
    answer(&f, &again.id, alondra_core::domain::START_RETRY);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert!(trusted(&f, &config));
    assert_eq!(claude.launches(), 1);
}

/// A folder inside the projects folder the user chose at setup is trusted by that choice: the
/// task starts with no card.
#[tokio::test]
async fn a_folder_inside_the_projects_folder_starts_without_a_card() {
    let f = world();
    let claude = FakeClaude::new(&f);
    std::fs::create_dir_all(alondra_core::options::path_in(&f.home).parent().unwrap()).unwrap();
    std::fs::write(alondra_core::options::path_in(&f.home), format!("projects_root = {:?}\n", f.root.to_str().unwrap())).unwrap();
    create(&f, "f1", &[], Some(&plan(&f, "app/f1")));
    let config = untrusted_config(&f);

    pass(&f.domain, &f.home, &claude).await.unwrap();

    assert!(trusted(&f, &config));
    assert_eq!(claude.launches(), 1);
    assert!(problem(&f, "f1").is_none());
}

#[tokio::test]
async fn a_cancelled_dependency_asks_the_user_once_and_starting_without_it_starts_the_task() {
    let f = world();
    let claude = FakeClaude::new(&f);
    open_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    command(&f.domain, user(), Command::CancelTask { task_id: "f0".into() });
    pass(&f.domain, &f.home, &claude).await.unwrap();
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 0);
    let card = problem(&f, "f1").expect("the user decides");
    assert_eq!(card.card.need, "“Task f0” was cancelled: start “Task f1” without it?");
    assert_eq!(f.domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with("start-problem-f1")).count(), 1);
    assert!(!told(&f).iter().any(|id| id.starts_with("dependency-cancelled-")), "the user has the card; the orchestrator has nothing to do");

    answer(&f, &card.id, alondra_core::domain::START_WITHOUT);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 1);
}

#[tokio::test]
async fn a_delivery_dependency_cancelled_asks_the_user_and_cancel_closes_the_task() {
    let f = world();
    let claude = FakeClaude::new(&f);
    open_task(&f, "a");
    create(&f, "after", &[], Some(&plan(&f, "app/after")));
    command(&f.domain, user(), Command::SetRelations { task_id: "after".into(), delivered_dependencies: ["a".into()].into(), blocks: Default::default() });
    command(&f.domain, user(), Command::CancelTask { task_id: "a".into() });
    pass(&f.domain, &f.home, &claude).await.unwrap();
    let card = problem(&f, "after").expect("the user decides");
    answer(&f, &card.id, alondra_core::domain::START_CANCEL);
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("after")].state, TaskState::Cancelled);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 0);
}

/// The user closed the node of an open task: they are asked to relaunch it or cancel it, and a
/// relaunch starts a new node in the closed one's folder.
#[tokio::test]
async fn a_node_the_user_closes_asks_to_relaunch_and_the_relaunch_starts_a_new_node() {
    let f = world();
    let claude = FakeClaude::new(&f);
    create(&f, "f1", &[], Some(&plan(&f, "app/f1")));
    pass(&f.domain, &f.home, &claude).await.unwrap();
    let node = agent_of(&f, "f1").expect("started");
    crate::node_revival::take_out(&f.domain, &node.0, "node closed by the user", Some(f.project.to_str().unwrap().into())).unwrap();
    assert!(agent_of(&f, "f1").is_none());
    pass(&f.domain, &f.home, &claude).await.unwrap();
    let card = problem(&f, "f1").expect("the user decides");
    assert_eq!(card.card.need, "Relaunch “Task f1” or cancel it");
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(f.domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with("start-problem-f1")).count(), 1, "one card");

    answer(&f, &card.id, alondra_core::domain::START_RETRY);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 2, "a new node");
    assert!(agent_of(&f, "f1").is_some_and(|agent| agent != node));
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert!(problem(&f, "f1").is_none(), "nothing more to ask");
}

fn queued(f: &Fixture) -> Vec<(String, i64)> {
    f.domain.store.lock().unwrap().launch_queue().unwrap().into_iter().map(|row| (row.task_id.0, row.enqueued_at)).collect()
}

/// 15 October: t-fastconn-5-remove-mailbox was cancelled at 19:42 and reopened at 19:45. It stayed
/// in progress with no node, nothing started it, and once launched by hand the fleet took it out of
/// its queue before starting it and was refused («Task already has a node»), so it was in no queue
/// at all. Reopened, it goes back to the queue with its plan and its seniority; a start the fleet
/// cannot make leaves it there, and its retry starts it.
#[tokio::test]
async fn a_reopened_task_waits_in_the_fleets_queue_with_its_seniority_and_a_refused_start_keeps_it_there() {
    let f = world();
    let claude = FakeClaude::new(&f);
    f.domain.live.gate.ignore_mac();
    create(&f, "f1", &[], Some(&plan(&f, "app/f1")));
    pass(&f.domain, &f.home, &claude).await.unwrap();
    let first = agent_of(&f, "f1").expect("its first node");
    let planned_at = f.domain.store.lock().unwrap().planned_tasks().unwrap()[&TaskId::from("f1")];

    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CancelTask { task_id: "f1".into() });
    command(&f.domain, Actor::System, Command::RecordNodeEnd { task_id: "f1".into(), end: NodeEnd::Closed, agent_id: Some(first) });
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::ReopenTask { task_id: "f1".into() });
    let task = f.domain.state().unwrap().tasks[&TaskId::from("f1")].clone();
    assert_eq!((task.state, task.started, task.agent_id), (TaskState::NotStarted, false, None), "never in progress with no node");
    assert_eq!(queued(&f), [("f1".to_owned(), planned_at)], "queued with the place its plan gave it");
    assert!(auto(&f, "f1").is_none(), "its first automatic launch belongs to its first life");

    create(&f, "f2", &[], Some(&plan(&f, "app/f2")));
    let body: LaunchBody = serde_json::from_value(json!({"task": "f2", "name": "app/f2", "cwd": f.project.to_str().unwrap()})).unwrap();
    crate::fleet_gate::enqueue(&f.domain, &body, "orchestrator", alondra_core::now_millis()).unwrap();
    let now = alondra_core::now_millis();
    let snapshot = f.domain.live.gate.snapshot(&f.domain, now).unwrap();
    let order: Vec<String> = snapshot.queue.iter().map(|waiting| format!("{:?}", waiting.key)).collect();
    assert_eq!(order[0], "Task(TaskId(\"f1\"))", "reopened, it keeps its place ahead of later work: {order:?}");
    assert_eq!(snapshot.working, 0, "back in the queue, its first start no longer counts as one under way");

    *claude.refuse.lock().unwrap() = Some("claude is not logged in".into());
    crate::fleet_gate::pass_at(&f.domain, &f.home, &claude, now).await.unwrap();
    assert!(agent_of(&f, "f1").is_none());
    assert!(queued(&f).iter().any(|(task, _)| task == "f1"), "a refused start never takes it out of the queue: {:?}", queued(&f));
    let retry = f.domain.store.lock().unwrap().start_retries().unwrap()[&TaskId::from("f1")];
    assert!(retry.1 > now, "it waits for its retry");
    let order: Vec<String> = f.domain.live.gate.snapshot(&f.domain, now).unwrap().queue.iter()
        .map(|waiting| format!("{:?}", waiting.key)).collect();
    assert_eq!(order, ["Task(TaskId(\"f2\"))"], "while it waits for its retry it holds nobody back");

    *claude.refuse.lock().unwrap() = None;
    crate::fleet_gate::pass_at(&f.domain, &f.home, &claude, retry.1).await.unwrap();
    assert!(agent_of(&f, "f1").is_some(), "its retry starts it");
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
    let claude = FakeClaude::new(&f);
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
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 0, "its dependency is done but its plan is not reviewed");
    assert!(auto(&f, "c").is_none(), "nothing claims it while it is held");
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::SettlePlanReview { review_id: "decision".into(), task_id: "c".into(),
        outcome: alondra_core::domain::PlanReviewOutcome::PlanChanged {
            launch: LaunchPlan { mission: Some("Group with 512 px prints".into()), ..plan(&f, "app/c-512") }, reason: "The user chose 512 px".into() } });
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 1);
    let args = claude.calls.lock().unwrap()[0].1.clone();
    assert!(args.windows(2).any(|pair| pair == ["--name", "app/c-512"]), "{args:?}");
    assert!(claude.prompt(0).contains("Group with 512 px prints") && !claude.prompt(0).contains("360 px"), "{}", claude.prompt(0));
}

fn orchestrator(f: &Fixture) -> Actor {
    let capability = f.domain.bind_orchestrator("execution-1").unwrap();
    crate::domain_orchestrator_tools::orchestrator(&f.domain, Some(capability.expose_for_child_environment()), None).unwrap()
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
    let claude = FakeClaude::new(&f);
    f.domain.live.gate.tune(crate::fleet_gate::Tuning { spacing: 0, limit: Some(crate::fleet_gate::Limit::Fixed(1)) });
    create(&f, "q1", &[], Some(&plan(&f, "app/q1")));
    create(&f, "q2", &[], Some(&plan(&f, "app/q2")));
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 1);
    assert_eq!(queued_ids(&f), [("q2".to_owned(), true)], "q2 waits for its automatic start, and the user may pause it");

    crate::fleet_gate::hold_queued(&f.domain, &"q2".into(), alondra_core::now_millis()).unwrap();
    assert!(queued_ids(&f).is_empty());
    assert_eq!(paused_ids(&f), ["q2"]);

    finish(&f, "q1", "What q1 delivered", false);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 1, "a slot is free, but only the user brings q2 back");
    assert_eq!(f.domain.live.gate.snapshot(&f.domain, alondra_core::now_millis()).unwrap().working, 0, "q2 takes no slot while paused");
    let body: LaunchBody = serde_json::from_value(json!({"task": "q2"})).unwrap();
    let refused = launch(&f.domain, &f.home, &orchestrator(&f), body, |_| None, &claude).await.unwrap_err();
    assert_eq!(refused.code, "conflict");
    assert!(refused.message.contains("The user paused"), "{}", refused.message);

    crate::fleet_gate::release_queued(&f.domain, &f.home, &"q2".into(), &claude).await.unwrap();
    assert_eq!(claude.launches(), 2);
    assert!(agent_of(&f, "q2").is_some());
    assert!(paused_ids(&f).is_empty());
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 2, "the resumed task is not started twice");
}

#[tokio::test]
async fn pausing_a_launch_the_orchestrator_queued_takes_it_out_of_the_queue_until_the_user_resumes_it() {
    let f = world();
    let claude = FakeClaude::new(&f);
    f.domain.live.gate.tune(crate::fleet_gate::Tuning { spacing: 0, limit: Some(crate::fleet_gate::Limit::Fixed(1)) });
    create(&f, "o1", &[], Some(&plan(&f, "app/o1")));
    create(&f, "o2", &[], Some(&plan(&f, "app/o2")));
    let body: LaunchBody = serde_json::from_value(json!({"task": "o1"})).unwrap();
    launch(&f.domain, &f.home, &orchestrator(&f), body, |_| None, &claude).await.unwrap();
    let body: LaunchBody = serde_json::from_value(json!({"task": "o2"})).unwrap();
    let queued = launch(&f.domain, &f.home, &orchestrator(&f), body, |_| None, &claude).await.unwrap();
    assert_eq!(queued["queued"], true);
    assert_eq!(f.domain.store.lock().unwrap().launch_queue().unwrap().len(), 1);

    crate::fleet_gate::hold_queued(&f.domain, &"o2".into(), alondra_core::now_millis()).unwrap();
    assert!(f.domain.store.lock().unwrap().launch_queue().unwrap().is_empty());
    assert_eq!(paused_ids(&f), ["o2"]);

    finish(&f, "o1", "What o1 delivered", false);
    crate::fleet_gate::pass(&f.domain, &f.home, &claude).await.unwrap();
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 1, "neither the queue nor the automatic start brings o2 back");

    crate::fleet_gate::release_queued(&f.domain, &f.home, &"o2".into(), &claude).await.unwrap();
    assert_eq!(claude.launches(), 2);
    assert!(agent_of(&f, "o2").is_some());
}

#[tokio::test]
async fn pausing_a_task_that_waits_for_nothing_says_it_is_not_queued() {
    let f = world();
    done_task(&f, "n0");
    open_task(&f, "n1");
    assert_eq!(crate::fleet_gate::hold_queued(&f.domain, &"n1".into(), alondra_core::now_millis()), Err("This task is not queued"));
    assert_eq!(crate::fleet_gate::hold_queued(&f.domain, &"n0".into(), alondra_core::now_millis()), Err("This task is not queued"));
}
