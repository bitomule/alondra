use super::*;
use crate::domain_orchestrator_tools::{LaunchBody, launch};
use crate::launch_plans::LaunchPlan;
use alondra_core::domain::{
    Command, CommandEnvelope, DeliveryTarget, InteractionId, MergeMethod, NodeEnd, PROTOCOL_VERSION, PrState, TaskId,
};
use serde_json::{Value, json};
use std::sync::Mutex;

struct Fixture {
    root: PathBuf,
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
    let root = std::env::temp_dir().join(format!("alondra-auto-launch-{}", crate::auth::random_hex(8)));
    let home = root.join("home");
    let project = root.join("App");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let domain = Arc::new(DomainRuntime::open(&home).unwrap());
    command(&domain, user(), Command::CreateRequest { request_id: "r".into(), body: "Rebuild the editor".into(), auto_merge: false, maintenance: false });
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
        command: Command::CreateTask {
            task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
            dependencies: after.iter().map(|dep| TaskId::from(*dep)).collect(), stack: None,
        },
    };
    f.domain.execute_bearer_planned(&bearer, &envelope, plan).unwrap();
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
    command(&f.domain, author, Command::PresentResult { attachments: vec![], task_id: id.into(), interaction_id: format!("final-{id}").into(),
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
    command(&f.domain, Actor::System, Command::RegisterAgent { agent_id: format!("seed-{id}").into(), name: format!("Seed {id}").into() });
    command(&f.domain, user(), Command::CreateTask { task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"),
        agent_id: Some(format!("seed-{id}").into()), review_prs: false, dependencies: Default::default(), stack: None });
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
    command(&f.domain, author, Command::PresentResult { attachments: vec![], task_id: "a".into(), interaction_id: "final-a".into(),
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
async fn a_cancelled_task_a_delivery_dependency_waits_on_is_told_to_the_orchestrator() {
    let f = world();
    let claude = FakeClaude::new(&f);
    open_task(&f, "a");
    create(&f, "after", &[], Some(&plan(&f, "app/after")));
    command(&f.domain, user(), Command::SetRelations { task_id: "after".into(), delivered_dependencies: ["a".into()].into(), blocks: Default::default() });
    command(&f.domain, user(), Command::CancelTask { task_id: "a".into() });
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 0);
    assert!(told(&f).contains(&"dependency-cancelled-after-a".to_owned()), "{:?}", told(&f));
}

#[tokio::test]
async fn a_ready_task_without_a_plan_tells_the_orchestrator_once_and_starts_after_set_launch_plan() {
    let f = world();
    let claude = FakeClaude::new(&f);
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], None);
    pass(&f.domain, &f.home, &claude).await.unwrap();
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 0);
    assert_eq!(told(&f).iter().filter(|id| id.as_str() == "auto-launch-no-plan-f1").count(), 1);
    let notice = &f.domain.state().unwrap().interactions[&InteractionId::from("auto-launch-no-plan-f1")].body;
    assert!(notice.contains("“Task f1” (f1) can start now") && notice.contains("set_launch_plan"), "{notice}");
    assert_eq!(auto(&f, "f1").unwrap().reason, Some(AutoLaunchReason::NoPlan));

    let body = serde_json::from_value(json!({"task_id": "f1", "launch": plan(&f, "app/f1")})).unwrap();
    crate::domain_orchestrator_tools::set_launch_plan(&f.domain, &f.home, body).unwrap();
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
async fn an_untrusted_repository_blocks_the_launch_and_tells_the_orchestrator_the_steps() {
    let f = world();
    let claude = FakeClaude::new(&f);
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    let project = f.project.canonicalize().unwrap();
    let config = f.root.join(".claude.json");
    crate::claude_trust::use_config_for_test(&config);
    std::fs::write(&config, json!({"projects": {project.to_str().unwrap(): {"hasTrustDialogAccepted": false}}}).to_string()).unwrap();
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 0);
    assert_eq!(auto(&f, "f1").unwrap().reason, Some(AutoLaunchReason::WorkspaceUntrusted));
    let notice = &f.domain.state().unwrap().interactions[&InteractionId::from("auto-launch-untrusted-f1")].body;
    assert!(notice.contains("could not start “Task f1” (f1) by itself") && notice.contains(project.to_str().unwrap()) && notice.contains("start it with launch (task f1)"), "{notice}");
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 0, "the orchestrator launches it once the user trusts the repository");
}

#[tokio::test]
async fn a_failed_launch_tells_the_orchestrator_and_is_not_retried_by_itself() {
    let f = world();
    let claude = FakeClaude::new(&f);
    done_task(&f, "f0");
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    *claude.refuse.lock().unwrap() = Some("claude is not logged in".into());
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(auto(&f, "f1").unwrap().reason, Some(AutoLaunchReason::LaunchFailed));
    let notice = &f.domain.state().unwrap().interactions[&InteractionId::from("auto-launch-failed-f1")].body;
    assert!(notice.contains("claude is not logged in.") && notice.contains("raise_problem"), "{notice}");
    *claude.refuse.lock().unwrap() = None;
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(claude.launches(), 0, "it is not tried again by itself");
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
async fn a_cancelled_dependency_tells_the_orchestrator_once() {
    let f = world();
    let claude = FakeClaude::new(&f);
    create(&f, "f0", &[], None);
    create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
    command(&f.domain, user(), Command::CancelTask { task_id: "f0".into() });
    pass(&f.domain, &f.home, &claude).await.unwrap();
    pass(&f.domain, &f.home, &claude).await.unwrap();
    assert_eq!(told(&f).iter().filter(|id| id.as_str() == "dependency-cancelled-f1-f0").count(), 1);
    let notice = &f.domain.state().unwrap().interactions[&InteractionId::from("dependency-cancelled-f1-f0")].body;
    assert_eq!(notice, "“Task f0” was cancelled, so “Task f1” (f1), which depends on it, cannot start. Change its dependencies with set_dependencies, or cancel it with cancel_task.");
    assert_eq!(claude.launches(), 0);
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
        crate::domain_orchestrator_tools::orchestrator(&f.domain, Some(capability.expose_for_child_environment())).unwrap()
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
