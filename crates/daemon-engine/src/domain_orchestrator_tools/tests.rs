use super::*;
use crate::domain_store::HandoverProcess;
use crate::node_close::Answer;
use crate::node_push::testing::Node;
use crate::sdk_handover::BgSessions;
use alondra_daemon_engine_model::LaunchOptions;
use crate::sdk_owner::tests::SdkLaunches;
use crate::test_support::Directory;
use alondra_core::domain::{AttachmentRef, Command, CommandEnvelope, ModelChooser, PROTOCOL_VERSION, TaskModel, TaskState};
use alondra_core::options::OptionsFile;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// A task «Editor colours» of a request, a `claude --bg` node agent-b of the previous release, and
/// the project folder App. Its launches start through the SDK fixture when it has one.
struct Fixture {
    sdk: Option<SdkLaunches>,
    sessions: Arc<FakeSessions>,
    root: Directory,
    home: PathBuf,
    project: PathBuf,
    domain: Arc<DomainRuntime>,
}

pub fn command(domain: &DomainRuntime, actor: Actor, command: Command) {
    let state = domain.state().unwrap();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
        command_id: format!("test-{}", crate::auth::random_hex(8)).into(), expected_revision: state.revision, command,
    };
    domain.execute(&actor, &envelope, None, || Ok(())).unwrap();
}

pub fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

/// A world whose launches start through the SDK fixture.
fn world() -> Fixture {
    let mut f = bare_world();
    f.sdk = Some(SdkLaunches::enable(&f.domain, &f.home));
    f
}

/// A world without an SDK backend, or with the one a test gives it.
fn bare_world() -> Fixture {
    let root = Directory::new();
    let home = root.join("home");
    let project = root.join("App");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let domain = Arc::new(DomainRuntime::open(&home).unwrap());
    crate::machine_watch::pace(&domain, &alondra_core::Machine { at: 1, memory_total: 16 << 30,
        memory_available: 12 << 30, memory_free_percent: Some(75), cpus: 11,
        disk_total: 500_000_000_000, disk_available: 400_000_000_000, ..Default::default() });
    domain.live.machine_watch.measure_disk(crate::disk::Space { total: 500_000_000_000, free: 400_000_000_000, purgeable: 0 });
    let sessions = Arc::new(FakeSessions::default());
    let _ = sessions.domain.set(Arc::downgrade(&domain));
    domain.handovers.use_sessions(sessions.clone() as Arc<dyn BgSessions>);
    *domain.handovers.turn_deadline.lock().unwrap() = Some(std::time::Duration::from_millis(20));
    command(&domain, Actor::System, Command::RegisterAgent { agent_id: "agent-b".into(), name: "Instance B".into(), at: 0 });
    command(&domain, user(), Command::CreateRequest { request_id: "r".into(), body: "Make the editor readable".into(), auto_merge: false, maintenance: false, message: false });
    command(&domain, user(), Command::TitleRequest { request_id: "r".into(), title: "Readable editor".into() });
    command(&domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None, task_id: "t".into(), request_id: "r".into(), title: "Editor colours".into(),
        agent_id: None, review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
    Fixture { sdk: None, sessions, root, home, project, domain }
}

/// A backend whose host cannot start: every SDK launch fails.
fn broken_backend(f: &Fixture) -> crate::sdk_owner::SdkBackend {
    let missing = f.root.join("missing");
    crate::sdk_owner::SdkBackend {
        provider: Default::default(), origin: None,
        node: missing.join("node"), node_sha256: "0".repeat(64),
        host: missing.join("host.cjs"), host_sha256: "0".repeat(64),
        native: missing.join("claude"), native_sha256: "0".repeat(64),
        host_environment: Default::default(), native_environment: Default::default(),
        confinement: None, state_root: f.root.join("sdk"), io_timeout_seconds: 5, installed_native: false,
    }
}

impl Fixture {
    fn sdk(&self) -> &SdkLaunches {
        self.sdk.as_ref().expect("this world launches through the SDK")
    }

    /// Waits on the domain until the native Claudes got `prompts` prompts: the folder each started
    /// in, and every prompt.
    async fn launched(&self, prompts: usize) -> (Vec<PathBuf>, Vec<String>) {
        let mut changed = self.domain.subscribe();
        while self.sdk().prompts().len() < prompts {
            changed.changed().await.unwrap();
        }
        let folders = self.sdk().starts().iter().map(|start| PathBuf::from(start["cwd"].as_str().unwrap())).collect();
        (folders, self.sdk().prompts())
    }

    /// The prompt of the only launch.
    async fn prompt(&self) -> String {
        self.launched(1).await.1.remove(0)
    }

    fn task_node(&self) -> Option<AgentId> {
        self.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id.clone()
    }
}

fn agent_of(answer: &Value) -> AgentId {
    AgentId::from(answer["agent_id"].as_str().expect("the launch names its node"))
}

fn the_orchestrator(domain: &DomainRuntime) -> Actor {
    let capability = domain.bind_orchestrator("execution-1").unwrap();
    orchestrator(domain, Some(capability.expose_for_child_environment()), None).unwrap()
}

#[test]
fn an_execution_ends_with_every_change_it_was_let_make_and_none_after() {
    let f = world();
    let capability = f.domain.bind_orchestrator("execution-1").unwrap();
    let bearer = capability.expose_for_child_environment();
    orchestrator(&f.domain, Some(bearer), None).unwrap();
    orchestrator(&f.domain, Some(bearer), Some("launch")).unwrap();
    let state = f.domain.state().unwrap();
    f.domain.execute_bearer(bearer, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "request".into(),
        expected_revision: state.revision,
        command: Command::CreateRequest { request_id: "r-2".into(), body: "More".into(), auto_merge: false, maintenance: false, message: false },
    }).unwrap();
    let state = f.domain.state().unwrap();
    assert!(f.domain.execute_bearer(bearer, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "refused".into(),
        expected_revision: state.revision, command: Command::RetitleTask { task_id: "missing".into(), title: "X".into() },
    }).is_err());
    assert_eq!(f.domain.end_execution("execution-1"), ["launch", "create_request"], "reads and refused commands change nothing");
    assert!(orchestrator(&f.domain, Some(bearer), Some("send")).is_err(), "an ended execution can change nothing more");
    assert!(f.domain.end_execution("execution-1").is_empty());
}

fn hard_choices() -> Choices {
    Choices {
        project: Some("App".into()),
        options: OptionsFile::default().orchestrator_defaults(Some("App"), Some("hard")),
        issue: Some("me/App#3".into()),
        issue_text: Some("Crash on launch".into()),
        attachments: vec![AttachmentRef { id: "att-1".into(), name: "shot.png".into(), content_digest: "d".repeat(64),
            content_reference: "files/att-1".into(), bytes: 3 }],
    }
}

/// Claude Code's `claude stop` and `claude agents` for the `claude --bg` nodes being replaced.
#[derive(Default)]
struct FakeSessions {
    domain: OnceLock<Weak<DomainRuntime>>,
    stopped: Mutex<Vec<String>>,
    /// Why `claude stop` fails, when it does.
    stop_fails: Mutex<Option<String>>,
    stop_panics: AtomicBool,
    /// Whether the node was on a turn at each stop.
    busy_at_stop: Mutex<Vec<bool>>,
}

impl BgSessions for FakeSessions {
    fn listed<'a>(&'a self, _: &'a str) -> Answer<'a, Result<Option<HandoverProcess>, String>> {
        Box::pin(async { Ok(None) })
    }

    fn stop<'a>(&'a self, session: &'a str) -> Answer<'a, Result<(), String>> {
        Box::pin(async move {
            assert!(!self.stop_panics.load(Ordering::SeqCst), "claude stop panicked");
            if let Some(reason) = self.stop_fails.lock().unwrap().take() {
                return Err(reason);
            }
            self.stopped.lock().unwrap().push(session.into());
            let domain = self.domain.get().and_then(Weak::upgrade);
            if let Some(domain) = &domain {
                self.busy_at_stop.lock().unwrap().push(domain.live.node(session).is_some_and(|node| node.busy));
            }
            Ok(())
        })
    }

    fn alive(&self, _: &HandoverProcess) -> bool {
        false
    }
}

fn launch_body(f: &Fixture, extra: Value) -> LaunchBody {
    let mut body = json!({"name": "editor", "cwd": f.project.to_str().unwrap(), "task": "t", "reason": "Colour tokens in one file: sonnet"});
    body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    serde_json::from_value(body).unwrap()
}

fn same(a: &Path, b: &Path) -> bool {
    a.canonicalize().unwrap() == b.canonicalize().unwrap()
}

#[test]
fn only_a_current_orchestrator_bearer_calls_the_tools() {
    let f = world();
    assert_eq!(orchestrator(&f.domain, None, None).unwrap_err().code, "forbidden");
    assert_eq!(orchestrator(&f.domain, Some(&"a".repeat(64)), None).unwrap_err().code, "forbidden");
    let agent = f.domain.bind_agent("agent-b").unwrap();
    assert_eq!(orchestrator(&f.domain, Some(agent.expose_for_child_environment()), None).unwrap_err().code, "forbidden");
    assert!(matches!(the_orchestrator(&f.domain), Actor::Orchestrator { .. }));
}

#[tokio::test]
async fn a_launch_carries_the_requests_choices_a_bound_capability_and_takes_the_task() {
    let f = world();
    let choices = hard_choices();
    let asked = Mutex::new(None);
    let answer = launch(&f.domain, &f.home, launch_body(&f, json!({"effort": "light"})),
        |request| { *asked.lock().unwrap() = Some(request.clone()); Some(choices.clone()) }).await.unwrap();
    let agent = agent_of(&answer);
    assert_eq!(asked.into_inner().unwrap(), Some("r".into()), "the Task's Request supplies the choices");
    let (folders, prompts) = f.launched(1).await;
    assert!(same(&folders[0], &f.project), "{folders:?}");
    assert_eq!((f.sdk().flag("--model").as_deref(), f.sdk().flag("--effort").as_deref()), (Some("opus"), Some("xhigh")),
        "an effort the user chose is binding: {:?}", f.sdk().args());
    assert_eq!(f.sdk().options(&agent.0), LaunchOptions { model: Some("opus".into()), effort: Some("xhigh".into()), disallowed_tools: vec![] , ..Default::default() });
    let prompt = &prompts[0];
    for expected in ["Editor colours", "Make the editor readable", "Crash on launch", "shot.png"] {
        assert!(prompt.contains(expected), "{expected} missing from {prompt}");
    }
    assert!(!prompt.contains("Task t of Request r"), "internal domain IDs must stay out of the node prompt: {prompt}");
    assert!(prompt.contains(&f.home.join("files/att-1").display().to_string()), "{prompt}");
    let state = f.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id, Some(agent.clone()));
    assert_eq!(state.agents[&agent].name, "editor");
    let capture: Value = serde_json::from_slice(&std::fs::read(&f.sdk().capture).unwrap()).unwrap();
    assert!(capture["env_keys"].as_array().unwrap().iter().any(|key| key == "ALONDRA_DOMAIN_CAPABILITY"), "the node gets its capability");
    use sha2::Digest;
    let bound: String = sha2::Sha256::digest(agent.0.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(capture["env_value_sha256"]["ALONDRA_SESSION_ID"], bound, "bound to the node that takes the task");
}

#[tokio::test]
async fn a_launch_with_a_title_names_its_node_by_it() {
    let f = world();
    let answer = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet", "title": "Editor colours"})), |_| None).await.unwrap();
    assert_eq!(f.domain.state().unwrap().agents[&agent_of(&answer)].name, "Editor colours");
}

/// A launch admits the execution with the model it resolved, and the task records it.
#[tokio::test]
async fn a_launch_records_on_the_task_the_model_it_runs() {
    let model = |f: &Fixture| f.domain.state().unwrap().tasks[&TaskId::from("t")].model.clone();
    let chosen = world();
    launch(&chosen.domain, &chosen.home, launch_body(&chosen, json!({"effort": "light"})), |_| Some(hard_choices())).await.unwrap();
    assert_eq!(model(&chosen), Some(TaskModel { model: "opus".into(), chosen_by: ModelChooser::User }), "the user's effort sets the model");

    let orchestrated = world();
    launch(&orchestrated.domain, &orchestrated.home, launch_body(&orchestrated, json!({"model": "sonnet", "effort": "medium"})), |_| None).await.unwrap();
    assert_eq!(model(&orchestrated), Some(TaskModel { model: "sonnet".into(), chosen_by: ModelChooser::Orchestrator }));

    let closed = world();
    let old = sdk_node_on_the_task(&closed, &closed.project).await;
    close_sdk_node(&closed, &old);
    relaunch(&closed.domain, &closed.home, &"t".into()).await.unwrap();
    assert_eq!(model(&closed), Some(TaskModel { model: "sonnet".into(), chosen_by: ModelChooser::Orchestrator }),
        "recovery retains the native model recorded by its original launch");

    let replaced = world();
    sdk_node_on_the_task(&replaced, &replaced.project).await;
    launch(&replaced.domain, &replaced.home, replace_body(), |_| None).await.unwrap();
    assert_eq!(model(&replaced).map(|m| m.model), Some("opus".into()), "the replacement's model is the task's");
}

#[tokio::test]
async fn a_launch_in_a_repository_claude_does_not_trust_is_refused_before_claude_runs() {
    let f = world();
    let project = f.project.canonicalize().unwrap();
    let config = f.root.join(".claude.json");
    crate::claude_trust::use_config_for_test(&config);
    let trust = |accepted: bool| std::fs::write(&config, json!({"projects": {project.to_str().unwrap(): {"hasTrustDialogAccepted": accepted}}}).to_string()).unwrap();

    trust(false);
    let refused = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap_err();
    assert_eq!(refused.code, "workspace_untrusted");
    assert!(refused.message.contains(project.to_str().unwrap()), "{}", refused.message);
    assert!(f.sdk().starts().is_empty());
    assert_eq!(f.domain.state().unwrap().agents.len(), 1, "no node was registered");
    assert!(f.task_node().is_none());
    let refusals = || f.domain.store.lock().unwrap().launch_refusals().unwrap();
    assert_eq!(refusals(), [(TaskId::from("t"), "workspace_untrusted".to_owned(), Some(project.to_str().unwrap().to_owned()))],
        "the Tasks list says why it did not start");

    trust(true);
    launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    assert_eq!(f.launched(1).await.0.len(), 1);
    assert!(refusals().is_empty(), "a task that started has nothing left to explain");
}

#[tokio::test]
async fn without_choices_the_orchestrators_model_and_effort_apply_and_the_prompt_can_be_given() {
    let f = world();
    let answer = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet", "effort": "medium", "prompt": "Only the dark theme"})), |_| None).await.unwrap();
    let prompt = f.prompt().await;
    assert_eq!((f.sdk().flag("--model").as_deref(), f.sdk().flag("--effort").as_deref()), (Some("sonnet"), Some("medium")), "{:?}", f.sdk().args());
    assert_eq!(f.sdk().options(&agent_of(&answer).0), LaunchOptions { model: Some("sonnet".into()), effort: Some("medium".into()), disallowed_tools: vec![] , ..Default::default() });
    assert!(prompt.starts_with("Only the dark theme\n"), "{prompt}");
}

#[tokio::test]
async fn with_auto_effort_the_launch_needs_a_known_model_and_a_users_effort_refuses_one() {
    let f = world();
    let refusal = |body: Value, choices: Option<Choices>| {
        let f = &f;
        async move { launch(&f.domain, &f.home, launch_body(f, body), |_| choices.clone()).await.unwrap_err() }
    };
    let missing = refusal(json!({}), None).await;
    assert_eq!(missing.code, "invalid_input");
    assert!(missing.message.contains("pass model") && missing.message.contains("haiku, sonnet, opus, fable"), "{}", missing.message);
    assert!(refusal(json!({"model": "gpt"}), None).await.message.contains("gpt is not a model"));
    assert!(refusal(json!({"model": "sonnet", "effort": "light"}), None).await.message.contains("light is not an effort"));
    let overruled = refusal(json!({"model": "opus"}), Some(hard_choices())).await;
    assert_eq!(overruled.code, "invalid_input");
    assert!(overruled.message.contains("the user chose effort hard"), "{}", overruled.message);
    assert!(f.sdk().starts().is_empty(), "nothing launched");
    assert!(f.domain.state().unwrap().tasks[&TaskId::from("t")].model.is_none());
}

#[tokio::test]
async fn a_model_alias_from_options_toml_is_launched_as_what_it_stands_for() {
    let f = world();
    std::fs::write(f.home.join("options.toml"), "[models]\nbig = \"claude-opus-5-5[1m]\"\n").unwrap();
    let answer = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "big"})), |_| None).await.unwrap();
    f.launched(1).await;
    assert_eq!(f.sdk().flag("--model").as_deref(), Some("claude-opus-5-5[1m]"), "{:?}", f.sdk().args());
    assert_eq!(f.sdk().flag("--effort"), None, "no effort unless the orchestrator names one: {:?}", f.sdk().args());
    assert_eq!(f.sdk().options(&agent_of(&answer).0), LaunchOptions { model: Some("claude-opus-5-5[1m]".into()), effort: None, disallowed_tools: vec![] , ..Default::default() });
}

#[tokio::test]
async fn a_launch_is_refused_for_a_closed_or_taken_task() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    command(&f.domain, actor.clone(), Command::AssignTask { task_id: "t".into(), agent_id: "agent-b".into(), model: None });
    let taken = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap_err();
    assert_eq!(taken.code, "conflict");
    command(&f.domain, user(), Command::CancelTask { task_id: "t".into() });
    let closed = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap_err();
    assert_eq!(closed.code, "conflict");
    assert!(f.sdk().starts().is_empty());
    assert_eq!(f.domain.state().unwrap().agents.len(), 1, "no node was registered");
}

#[tokio::test]
async fn without_an_sdk_backend_a_launch_is_refused_and_no_node_takes_the_task() {
    let f = bare_world();
    let refused = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap_err();
    assert_eq!((refused.code.as_str(), refused.message.as_str()), ("unavailable", crate::sdk_owner::NO_HOST));
    let state = f.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id, None);
    assert_eq!(state.agents.len(), 1, "no node was registered");
}

fn give(f: &Fixture, task: &str, agent: &str) {
    command(&f.domain, user(), Command::AssignTask { task_id: task.into(), agent_id: agent.into(), model: None });
}

#[tokio::test]
async fn a_message_with_a_task_reaches_only_that_tasks_node() {
    let f = bare_world();
    let actor = the_orchestrator(&f.domain);
    let node = Node::connect(&f.domain, "agent-b");
    give(&f, "t", "agent-b");
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None, task_id: "t2".into(), request_id: "r".into(), title: "Editor fonts".into(),
        agent_id: None, review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
    let body = |task: &str, text: &str| -> SendBody { serde_json::from_value(json!({"node": "Instance B", "text": text, "task": task})).unwrap() };
    let answer = send(&f.domain, &f.home, &actor, body("t", "Take the editor colours")).await.unwrap();
    assert_eq!(answer["delivery_state"], "handed");
    assert_eq!(node.texts()[0], "[Task t “Editor colours”] Take the editor colours");
    assert_eq!(node.lines.lock().unwrap()[0]["as_user"], false, "the orchestrator's words reach the model as its own");

    let unassigned = send(&f.domain, &f.home, &actor, body("t2", "Take these too")).await.unwrap_err();
    assert_eq!(unassigned.code, "not_assigned");
    assert!(unassigned.message.contains("Launch a new node for t2 with launch"), "{}", unassigned.message);
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t2")].agent_id, None, "a message never assigns");

    command(&f.domain, Actor::System, Command::RegisterAgent { agent_id: "agent-c".into(), name: "Instance C".into(), at: 0 });
    let foreign: SendBody = serde_json::from_value(json!({"node": "agent-c", "text": "Mine now", "task": "t"})).unwrap();
    assert_eq!(send(&f.domain, &f.home, &actor, foreign).await.unwrap_err().code, "not_assigned");
    assert_eq!(node.texts().len(), 1, "the refused messages sent nothing");
}

#[tokio::test]
async fn a_message_about_a_closed_task_is_refused_as_closed() {
    let f = bare_world();
    let actor = the_orchestrator(&f.domain);
    let node = Node::connect(&f.domain, "agent-b");
    give(&f, "t", "agent-b");
    command(&f.domain, user(), Command::CancelTask { task_id: "t".into() });
    let body: SendBody = serde_json::from_value(json!({"node": "Instance B", "text": "Carry on after all", "task": "t"})).unwrap();
    let refused = send(&f.domain, &f.home, &actor, body).await.unwrap_err();
    assert_eq!(refused.code, "task_closed");
    assert!(refused.message.contains("create_task") && refused.message.contains("dependencies [t]"), "{}", refused.message);
    assert!(f.domain.state().unwrap().tasks[&TaskId::from("t")].cancelled(), "a message never reopens");
    assert!(node.texts().is_empty());
}

#[tokio::test]
async fn a_node_on_a_task_takes_no_other_but_still_gets_plain_messages() {
    let f = bare_world();
    let actor = the_orchestrator(&f.domain);
    let node = Node::connect(&f.domain, "agent-b");
    give(&f, "t", "agent-b");
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None, task_id: "t2".into(), request_id: "r".into(), title: "Editor fonts".into(),
        agent_id: None, review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
    let body = |task: Option<&str>| -> SendBody { serde_json::from_value(json!({"node": "Instance B", "text": "Go", "task": task})).unwrap() };
    assert_eq!(send(&f.domain, &f.home, &actor, body(Some("t2"))).await.unwrap_err().code, "not_assigned");
    send(&f.domain, &f.home, &actor, body(None)).await.unwrap();
    send(&f.domain, &f.home, &actor, body(Some("t"))).await.unwrap();
    assert_eq!(node.texts().len(), 2, "the refused task sent nothing");

    command(&f.domain, user(), Command::CancelTask { task_id: "t".into() });
    assert_eq!(send(&f.domain, &f.home, &actor, body(Some("t2"))).await.unwrap_err().code, "not_assigned",
        "not even when its own task ended");
}

#[tokio::test]
async fn send_takes_no_model() {
    let parsed: Result<SendBody, _> = serde_json::from_value(json!({"node": "Instance B", "text": "Go", "task": "t", "model": "sonnet"}));
    assert!(parsed.is_err(), "deny_unknown_fields refuses it as invalid_input");
}

#[tokio::test]
async fn an_interrupt_is_a_line_for_the_connected_agent_and_is_refused_for_one_that_is_not() {
    let f = bare_world();
    let node = Node::connect(&f.domain, "agent-b");
    let named = || -> NodeBody { serde_json::from_value(json!({"node": "agent-b"})).unwrap() };
    assert_eq!(interrupt(&f.domain, named()).unwrap(), json!({"interrupted": "agent-b"}));
    tokio::task::yield_now().await;
    assert_eq!(node.interrupts(), 1);
    command(&f.domain, Actor::System, Command::RegisterAgent { agent_id: "agent-a".into(), name: "Instance A".into(), at: 0 });
    let gone: NodeBody = serde_json::from_value(json!({"node": "agent-a"})).unwrap();
    assert_eq!(interrupt(&f.domain, gone).unwrap_err().code, "session_unreachable");
    assert_eq!(node.interrupts(), 1);
}

#[test]
fn the_choices_are_the_ones_recorded_with_the_request() {
    let f = bare_world();
    let state = f.domain.state().unwrap();
    let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "asked".into(),
        expected_revision: state.revision, command: Command::CreateRequest { request_id: "r2".into(), body: "Fix the crash".into(), auto_merge: false, maintenance: false, message: false } };
    f.domain.execute_with(&user(), &envelope, None, Some(&hard_choices()), || Ok(())).unwrap();
    assert_eq!(request_choices(&f.domain, &"r2".into()).unwrap(), Some(hard_choices()));
    assert_eq!(request_choices(&f.domain, &"r".into()).unwrap(), None);
}

#[test]
fn the_requests_choices_read_as_8ccd131_told_the_orchestrator() {
    let home = Path::new("/h");
    let text = choices_text("Ana", home, &hard_choices());
    assert!(text.contains("options: project App") && text.contains("Effort hard was chosen by Ana and is binding"), "{text}");
    assert!(text.contains("issue me/App#3: Crash on launch"), "{text}");
    assert!(text.contains("attached shot.png: /h/files/att-1") && text.contains("paths in the mission"), "{text}");
}

/// Claude measures the default account's week at each of `week_percents`, all with the same reset,
/// as it reports a window that has not ended.
fn measured(domain: &DomainRuntime, week_percents: &[f64]) {
    use alondra_core::usage::{Share, UsageReading, WindowKind, WindowReading};
    let reset = alondra_core::now_millis() + 5 * 86_400_000;
    for week_percent in week_percents {
        let reading = UsageReading {
            windows: vec![WindowReading { kind: WindowKind::Weekly, used: Share::of(week_percent / 100.0), resets_at: Some(reset), exhausted: false }],
            plan: None, complete: false,
        };
        crate::usage_probe::record(domain, &alondra_core::accounts::AccountId::default_account(), reading, alondra_core::now_millis());
    }
}

#[tokio::test]
async fn going_over_todays_slice_launches_without_a_warning_and_asks_the_user_once_a_day() {
    let f = world();
    measured(&f.domain, &[12.0, 53.0]);
    assert!(crate::usage_probe::usage(&f.domain, alondra_core::now_millis()).unwrap().over_pace, "41% spent against a ~18% slice");
    let answer = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    assert_eq!(f.task_node(), Some(agent_of(&answer)), "over pace no longer blocks");
    assert!(answer.get("warning").is_none(), "the orchestrator is not asked to relay it: {answer}");
    assert_eq!(f.launched(1).await.0.len(), 1);

    let cards = || f.domain.state().unwrap().interactions.values()
        .filter(|card| card.id.0.starts_with("over-pace-")).cloned().collect::<Vec<_>>();
    let [card] = cards().try_into().unwrap();
    assert_eq!(card.card.kind, alondra_core::domain::CardKind::Decide);
    assert_eq!(card.author, alondra_core::domain::Author::System);
    assert!(!card.blocking);
    assert_eq!(card.body, "41/18/53");
    crate::usage_pause::raise_over_pace_card(&f.domain, alondra_core::now_millis());
    assert_eq!(cards().len(), 1, "one card per local day");

    let within = world();
    measured(&within.domain, &[12.0, 15.0]);
    let answer = launch(&within.domain, &within.home, launch_body(&within, json!({"over_pace_ok": true, "model": "sonnet"})), |_| None).await.unwrap();
    assert!(answer.get("warning").is_none(), "over_pace_ok is still accepted: {answer}");
    assert!(within.domain.state().unwrap().interactions.keys().all(|id| !id.0.starts_with("over-pace-")), "within its slice: no card");
}

#[tokio::test]
async fn a_disk_preflight_refusal_invalidates_admission_before_a_remounted_volume_is_budgeted() {
    use crate::admission::{Hold, Key, Limit, Tuning, Waiting};
    use alondra_core::domain::TaskPriority;
    use std::collections::BTreeMap;
    const GB: u64 = 1_000_000_000;
    for origin in [Origin::Auto, Origin::Admitted] {
        let f = bare_world();
        let gate = &f.domain.live.gate;
        let watch = &f.domain.live.machine_watch;
        gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(4)) });
        let mount = PathBuf::from("/Volumes/BuildCache");
        let space = |free| crate::disk::Space { total: 500 * GB, free, purgeable: 0 };
        let working = BTreeMap::from([("builder".into(), Some("app".to_owned()))]);
        watch.give_volumes_holding(vec![(mount.clone(), space(30 * GB), 20 * GB)]);
        watch.measure_volumes(&BTreeMap::new(), 0);
        watch.measure_volumes(&working, 1);
        watch.give_volumes_holding(vec![(mount.clone(), space(27 * GB), 23 * GB)]);
        watch.measure_volumes(&working, 2);
        watch.measure_volumes(&BTreeMap::new(), 3);
        watch.give_volumes(vec![]);
        watch.measure_volumes(&BTreeMap::new(), 4);
        let now = alondra_core::now_millis();
        let waiting = Waiting { key: Key::Task("t".into()), priority: TaskPriority::Normal, resuming: false, since: now };
        assert_eq!(gate.ask(&f.domain, waiting.clone(), now), Ok(()), "the learned volume is confirmed absent");

        watch.set_disk_low(true);
        let refused = launch_from(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None, origin).await.unwrap_err();
        assert_eq!(refused.code, "disk_low");
        assert!(f.task_node().is_none());
        watch.set_disk_low(false);
        watch.give_volumes(vec![(mount.clone(), space(4 * GB))]);
        watch.measure_volumes(&BTreeMap::new(), 5);
        assert!(matches!(gate.ask(&f.domain, waiting.clone(), now), Err(Hold::Disk { .. })),
            "an unused admission from before preflight cannot bypass the first insufficient volume budget");

        watch.give_volumes(vec![(mount, space(10 * GB))]);
        watch.measure_volumes(&BTreeMap::new(), 6);
        assert_eq!(gate.ask(&f.domain, waiting.clone(), now), Ok(()));
        let reserved = watch.disk_room(&BTreeMap::new(), None, now).expect("the new grant reserves its build on the remounted volume");
        assert_eq!(reserved.needed, 11 * GB);
        assert_eq!(gate.ask(&f.domain, waiting, now), Ok(()));
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, now), Some(reserved), "re-reading the same grant charges it once");
    }
}

#[tokio::test]
async fn a_launch_is_refused_while_usage_is_exhausted_and_goes_ahead_once_it_is_back() {
    let f = world();
    let now = alondra_core::now_millis();
    f.domain.live.usage_pause.exhaust(Some(now + 3_600_000), now);
    let refused = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap_err();
    assert_eq!(refused.code, "usage_exhausted");
    assert!(refused.message.contains("No Claude account on this Mac has usage left"), "{}", refused.message);
    assert!(f.sdk().starts().is_empty());
    assert!(f.task_node().is_none(), "the task is left as it was");

    f.domain.live.usage_pause.resume(alondra_core::now_millis());
    launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    assert_eq!(f.launched(1).await.0.len(), 1);
}

/// A task with a model selection finds no account while every one is out: it waits for usage too,
/// and says so, instead of failing as a start that went wrong.
#[tokio::test]
async fn a_launch_that_finds_no_account_while_usage_is_exhausted_waits_for_usage() {
    use alondra_core::model_selection::{ModelProfile, TaskSelection};
    let f = world();
    std::fs::write(alondra_core::options::path_in(&f.home), "[accounts]\nconnected = []\n").unwrap();
    command(&f.domain, Actor::System, Command::RecordTaskSelection { task_id: "t".into(), selection: TaskSelection::automatic(ModelProfile::Implementation), pending: None });
    let now = alondra_core::now_millis();
    f.domain.live.usage_pause.exhaust(Some(now + 3_600_000), now);
    let refused = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "claude/sonnet"})), |_| None).await.unwrap_err();
    assert_eq!(refused.code, "usage_exhausted", "{refused:?}");
    assert!(f.sdk().starts().is_empty());
}

/// 25 October: the disk filled. A launch while the disk is nearly full waits in the queue for room
/// instead of being refused for the orchestrator to try again, and starts by itself once there is.
#[tokio::test]
async fn a_launch_while_the_disk_is_nearly_full_waits_in_the_queue_and_starts_once_it_has_space() {
    let f = world();
    f.domain.live.machine_watch.set_disk_low(true);
    let queued = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    assert_eq!(queued["queued"], true, "{queued}");
    assert!(queued["message"].as_str().unwrap().contains("the disk has no room for one more build (1.0 GB free of 5.0 GB needed)"), "{queued}");
    assert!(f.sdk().starts().is_empty());
    assert!(f.task_node().is_none(), "the task waits as it was");
    assert_eq!(f.domain.store.lock().unwrap().launch_queue().unwrap().len(), 1);

    f.domain.live.machine_watch.set_disk_low(false);
    crate::admission::pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(f.launched(1).await.0.len(), 1);
}

#[tokio::test]
async fn a_task_whose_node_the_user_closed_is_relaunched_for_that_task_in_the_closed_nodes_folder() {
    let f = world();
    let old = sdk_node_on_the_task(&f, &f.project).await;
    let original = f.sdk().starts()[0]["session"].as_str().unwrap().to_owned();
    let refused = relaunch(&f.domain, &f.home, &"t".into()).await.unwrap_err();
    assert_eq!(refused.code, "conflict", "a task whose node was not closed is not relaunched");
    assert_eq!(f.sdk().starts().len(), 1);

    close_sdk_node(&f, &old);
    let state = f.domain.state().unwrap();
    assert_eq!((state.tasks[&"t".into()].state, state.tasks[&"t".into()].agent_id.clone()), (TaskState::NotStarted, None));
    assert_eq!(f.domain.live.closed_task_ids(), ["t"]);

    let answer = relaunch(&f.domain, &f.home, &"t".into()).await.unwrap();
    let agent = agent_of(&answer);
    let (folders, prompts) = f.launched(2).await;
    assert_resumed(&f, &original);
    assert!(same(&folders[1], &f.project), "the new node starts where the closed one worked: {folders:?}");
    assert!(prompts[1].contains("Editor colours") && prompts[1].contains("Make the editor readable"), "the mission comes with it: {}", prompts[1]);
    let state = f.domain.state().unwrap();
    assert_eq!(state.agents[&agent].name, "Instance B", "it keeps the closed node's name");
    assert_eq!(state.tasks[&"t".into()].agent_id, Some(agent));
    assert!(f.domain.live.closed_task_ids().is_empty(), "the task no longer waits for a relaunch");
    let again = relaunch(&f.domain, &f.home, &"t".into()).await.unwrap_err();
    assert_eq!(again.code, "conflict");
}

#[tokio::test]
async fn a_launch_with_an_approach_writes_how_to_work_into_the_mission() {
    let f = world();
    launch(&f.domain, &f.home, launch_body(&f, json!({"effort": "light", "approach": "subagents"})), |_| Some(hard_choices())).await.unwrap();
    let prompt = f.prompt().await;
    assert!(prompt.contains("## How to work") && prompt.contains("TaskCreate") && prompt.contains("the Agent tool"), "{prompt}");
}

#[test]
fn a_launch_or_send_with_an_unknown_approach_is_refused_naming_the_three() {
    let launch = serde_json::from_value::<LaunchBody>(json!({"name": "n", "cwd": "/", "task": "t", "approach": "swarm"})).err().unwrap().to_string();
    let send = serde_json::from_value::<SendBody>(json!({"node": "n", "text": "Go", "approach": "swarm"})).err().unwrap().to_string();
    for error in [launch, send] {
        assert!(["solo", "subagents", "workflow"].iter().all(|name| error.contains(name)), "{error}");
    }
}

#[tokio::test]
async fn a_message_with_an_approach_ends_with_how_to_work() {
    let f = bare_world();
    let actor = the_orchestrator(&f.domain);
    let node = Node::connect(&f.domain, "agent-b");
    give(&f, "t", "agent-b");
    let body: SendBody = serde_json::from_value(json!({"node": "Instance B", "text": "Take it", "task": "t", "approach": "workflow"})).unwrap();
    send(&f.domain, &f.home, &actor, body).await.unwrap();
    let text = &node.texts()[0];
    assert!(text.contains("Take it") && text.contains("## How to work") && text.contains("the Workflow tool"), "{text}");
}

type DecisionRow = (String, String, Option<String>, Option<String>, Option<String>, Option<String>);

fn decisions(domain: &DomainRuntime) -> Vec<DecisionRow> {
    domain.store.lock().unwrap().model_decision_rows().unwrap()
}

fn row(kind: &str, model: Option<&str>, effort: Option<&str>, approach: Option<&str>, reason: Option<&str>) -> DecisionRow {
    let text = |value: Option<&str>| value.map(str::to_owned);
    ("t".into(), kind.into(), text(model), text(effort), text(approach), text(reason))
}

#[tokio::test]
async fn with_auto_effort_a_launch_without_a_reason_is_refused_before_claude_runs() {
    let f = world();
    for reason in [json!(null), json!("   ")] {
        let refused = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet", "reason": reason})), |_| None).await.unwrap_err();
        assert_eq!(refused.code, "invalid_input");
        assert!(refused.message.contains("pass reason"), "{}", refused.message);
    }
    assert!(f.sdk().starts().is_empty(), "nothing launched");
    assert!(decisions(&f.domain).is_empty());

    let answer = launch(&f.domain, &f.home, launch_body(&f, json!({"reason": null})), |_| Some(hard_choices())).await.unwrap();
    assert_eq!(decisions(&f.domain), [row("launch", Some("opus"), Some("xhigh"), Some("solo"), None)],
        "the user's effort needs no reason");
    assert_eq!(f.domain.store.lock().unwrap().model_decision_meta().unwrap(), [(agent_of(&answer).0, "user".to_owned(), "Editor colours".to_owned())]);
}

#[tokio::test]
async fn every_launch_records_the_model_effort_approach_and_reason_the_orchestrator_chose() {
    let f = world();
    let answer = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet", "effort": "medium", "approach": "subagents"})), |_| None).await.unwrap();
    assert_eq!(decisions(&f.domain), [row("launch", Some("sonnet"), Some("medium"), Some("subagents"), Some("Colour tokens in one file: sonnet"))]);
    assert_eq!(f.domain.store.lock().unwrap().model_decision_meta().unwrap(), [(agent_of(&answer).0, "orchestrator".to_owned(), "Editor colours".to_owned())]);
}

#[tokio::test]
async fn a_message_with_an_approach_is_recorded_and_a_plain_one_is_not() {
    let f = bare_world();
    let actor = the_orchestrator(&f.domain);
    let _node = Node::connect(&f.domain, "agent-b");
    give(&f, "t", "agent-b");
    f.domain.report(serde_json::from_value(json!({"session_id": "agent-b", "event": "model", "model": "claude-sonnet-5-5"})).unwrap(), 1);
    let body = |extra: Value| -> SendBody {
        let mut body = json!({"node": "Instance B", "text": "Go", "task": "t"});
        body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(body).unwrap()
    };
    send(&f.domain, &f.home, &actor, body(json!({}))).await.unwrap();
    send(&f.domain, &f.home, &actor, body(json!({"task": null, "approach": "solo"}))).await.unwrap();
    assert!(decisions(&f.domain).is_empty(), "a message without a task, or without an approach, is no decision");
    send(&f.domain, &f.home, &actor, body(json!({"approach": "workflow", "reason": "Same files as its last task"}))).await.unwrap();
    send(&f.domain, &f.home, &actor, body(json!({"approach": "solo"}))).await.unwrap();
    assert_eq!(decisions(&f.domain), [
        row("send", Some("sonnet"), None, Some("workflow"), Some("Same files as its last task")),
        row("send", Some("sonnet"), None, Some("solo"), None),
    ]);
}

#[tokio::test]
async fn a_relaunch_is_recorded_as_one_and_needs_no_reason() {
    let f = world();
    let old = sdk_node_on_the_task(&f, &f.project).await;
    close_sdk_node(&f, &old);
    let before = decisions(&f.domain).len();
    relaunch(&f.domain, &f.home, &"t".into()).await.unwrap();
    assert_eq!(&decisions(&f.domain)[before..], [row("relaunch", Some("sonnet"), None, Some("solo"), None)]);
}

#[tokio::test]
async fn a_launch_of_a_follow_up_task_carries_the_earlier_result_and_pull_request() {
    let f = world();
    let author = Actor::Agent { agent_id: "agent-b".into() };
    give(&f, "t", "agent-b");
    command(&f.domain, author.clone(), Command::RegisterPr { task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(),
        repository: "me/app".into(), number: 12, head: "head1".into(), base: "main".into(), allowed_methods: vec![alondra_core::domain::MergeMethod::Squash] });
    let targets = || vec![alondra_core::domain::DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }];
    command(&f.domain, author, Command::PresentResult { checklist: Vec::new(), attachments: vec![], task_id: "t".into(), interaction_id: "final".into(),
        body: "  The colours live in one file now.  ".into(), targets: targets(), is_final: true, nothing_to_review: false,
        card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true) });
    command(&f.domain, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets: targets() });
    command(&f.domain, Actor::System, Command::ObservePrState { pr_id: "pr".into(), head: "head1".into(), base: "main".into(),
        state: alondra_core::domain::PrState::Merged, merged_head: Some("head1".into()) });
    command(&f.domain, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: alondra_core::domain::NodeEnd::Closed, agent_id: None });
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].state, TaskState::Done);
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None, task_id: "t2".into(), request_id: "r".into(), title: "Editor fonts".into(),
        agent_id: None, review_prs: false, dependencies: ["t".into()].into(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });

    launch(&f.domain, &f.home, launch_body(&f, json!({"task": "t2", "model": "sonnet"})), |_| None).await.unwrap();
    let prompt = f.prompt().await;
    assert!(prompt.contains("\n\n## Earlier work this task builds on\n\n### “Editor colours” (done)\nThe colours live in one file now.\nPull request: https://github.com/me/app/pull/12 (merged)"), "{prompt}");
}

#[test]
fn the_follow_up_context_is_empty_without_earlier_work_and_shortens_a_long_result() {
    let f = world();
    let state = f.domain.state().unwrap();
    assert_eq!(follow_up_context(&state, &state.tasks[&TaskId::from("t")]), "");
    assert_eq!(clipped(&"x".repeat(5), 3, "[shortened]"), alondra_core::domain::Clipped { text: "xxx[shortened]".into(), shortened: true });
    assert_eq!(clipped("short", 10, "[shortened]"), alondra_core::domain::Clipped { text: "short".into(), shortened: false });
}

#[test]
fn canonical_task_context_is_scoped_bounded_and_preserves_source_ids() {
    let f = world();
    let mut state = f.domain.state().unwrap().as_ref().clone();
    let task = state.tasks[&TaskId::from("t")].clone();
    state.requests.get_mut(&task.request_id).unwrap().body = "The requested feature".into();
    state.requests.insert("unrelated".into(), alondra_core::domain::Request {
        id: "unrelated".into(), body: "Private unrelated request".into(), ..state.requests[&task.request_id].clone()
    });
    let context = canonical_task_context(&state, &task);
    assert!(context.complete);
    assert!(context.text.contains("The requested feature"));
    assert!(context.text.contains(&task.request_id.to_string()));
    assert!(!context.text.contains("Private unrelated request"));
    state.requests.get_mut(&task.request_id).unwrap().body = "界".repeat(CANONICAL_CONTEXT_LIMIT * 2);
    let context = canonical_task_context(&state, &task);
    assert!(context.text.chars().count() <= CANONICAL_CONTEXT_LIMIT);
    assert!(!context.complete);
    assert!(context.omitted_sources.contains(&format!("request:{}", task.request_id)));
    assert!(!context.text.contains("界"));
    assert!(context.text.contains(&task.id.to_string()));
}

#[test]
fn canonical_task_context_keeps_ordered_public_user_inputs_and_refuses_missing_or_oversized_text() {
    use alondra_core::domain::LifecycleRecord;
    let f = world();
    let mut state = f.domain.state().unwrap().as_ref().clone();
    let task = state.tasks[&TaskId::from("t")].clone();
    for record in [
        LifecycleRecord::Told { input: "refinement".into(), text: Some("Only change the file-backed profile".into()), answers: None },
        LifecycleRecord::Answered { card: "question".into(), input: "freeform".into(), text: Some("Other: preserve the native default credentials".into()) },
        LifecycleRecord::HandedBack { delivery: "delivery".into(), input: "review".into(), text: Some("The native credentials still changed; repair that".into()) },
    ] { state.lifecycle_journal.get_mut(&task.id).unwrap().push(record); }
    state.lifecycle_journal.insert("unrelated-task".into(), vec![LifecycleRecord::Told { input: "private".into(), text: Some("Unrelated private input".into()), answers: None }]);
    let context = canonical_task_context(&state, &task);
    assert!(context.complete);
    let first = context.text.find("Only change the file-backed profile").unwrap();
    let second = context.text.find("Other: preserve the native default credentials").unwrap();
    let third = context.text.find("The native credentials still changed; repair that").unwrap();
    assert!(first < second && second < third);
    assert!(!context.text.contains("Unrelated private input"));
    state.lifecycle_journal.get_mut(&task.id).unwrap().push(LifecycleRecord::Told { input: "lost".into(), text: None, answers: None });
    let context = canonical_task_context(&state, &task);
    assert!(!context.complete);
    assert!(context.omitted_sources.contains(&format!("task:{}:input:lost", task.id)));
    state.lifecycle_journal.get_mut(&task.id).unwrap().push(LifecycleRecord::Told { input: "oversized".into(), text: Some("界".repeat(CANONICAL_CONTEXT_LIMIT)), answers: None });
    let context = canonical_task_context(&state, &task);
    assert!(!context.complete);
    assert!(context.omitted_sources.contains(&format!("task:{}:input:oversized", task.id)));
    assert!(!context.text.contains("界"));
}

#[test]
fn a_task_moves_to_another_provider_by_what_compaction_cut_never_by_a_marker_in_its_text() {
    use alondra_core::domain::{Author, CardKind, CardSpec, Interaction, InteractionPayload, Recipient, Scope};
    let f = world();
    let mut state = f.domain.state().unwrap().as_ref().clone();
    let mut task = state.tasks[&TaskId::from("t")].clone();
    state.requests.get_mut(&task.request_id).unwrap().body = "Explain why a result that ends in [shortened] still moves".into();
    assert!(crate::provider_transition::critical_context(&state, &task).is_ok(), "a literal marker written by a person cuts nothing");

    let mut earlier = task.clone();
    earlier.id = "earlier".into();
    earlier.final_presentation = Some("earlier-result".into());
    let result = |shortened: bool| Interaction { system_card: None,
        attachments: vec![], id: "earlier-result".into(), scope: Scope::Task { task_id: "earlier".into() }, author: Author::System,
        recipient: Recipient::User, body: "The colours live in one file now.".into(), blocking: false, subject: None,
        payload: InteractionPayload::Information { read_by: Default::default() }, revision: 1, order: 1, withdrawn: false,
        card: CardSpec::derived(CardKind::Know, "Read the result"), clarification: None, snoozed_until: None, resurfaced_at_revision: None, shortened,
    };
    state.tasks.insert(earlier.id.clone(), earlier);
    task.dependencies.insert("earlier".into());
    state.interactions.insert("earlier-result".into(), result(false));
    assert!(crate::provider_transition::critical_context(&state, &task).is_ok());
    state.interactions.insert("earlier-result".into(), result(true));
    let context = canonical_task_context(&state, &task);
    assert!(!context.complete);
    assert!(context.omitted_sources.contains(&"interaction:earlier-result".to_owned()), "{:?}", context.omitted_sources);
    assert!(crate::provider_transition::critical_context(&state, &task).is_err());
}

#[tokio::test]
async fn a_launch_of_a_task_with_a_plan_needs_no_name_or_cwd() {
    let f = world();
    let plan = alondra_daemon_engine_model::LaunchPlan {
        name: "app/editor".into(), cwd: f.project.to_str().unwrap().into(), mission: Some("Make the colours tokens".into()),
        model: Some("sonnet".into()), effort: None, approach: Some(alondra_core::options::Approach::Solo),
        reason: Some("Colour tokens in one file".into()), role: None, tags: None, project: None,
    };
    f.domain.store.lock().unwrap().set_launch_plan(&"t".into(), &plan, 1).unwrap();
    let body: LaunchBody = serde_json::from_value(json!({"task": "t"})).unwrap();
    let answer = launch(&f.domain, &f.home, body, |_| None).await.unwrap();
    let (folders, prompts) = f.launched(1).await;
    assert!(same(&folders[0], &f.project), "{folders:?}");
    assert_eq!(f.domain.state().unwrap().agents[&agent_of(&answer)].name, "app/editor");
    assert!(prompts[0].contains("Make the colours tokens"), "the plan's mission is the node's mission");
    assert_eq!(decisions(&f.domain), [row("launch", Some("sonnet"), None, Some("solo"), Some("Colour tokens in one file"))]);
}

#[tokio::test]
async fn a_decision_keeps_the_provider_and_the_tags_of_the_plan_or_of_the_launch() {
    let f = world();
    let tags = serde_json::from_value(json!({"kind": "docs", "areas": ["ui"], "risks": ["user_visible"]})).unwrap();
    let plan = alondra_daemon_engine_model::LaunchPlan { model: Some("sonnet".into()), reason: Some("Docs only".into()), tags: Some(tags),
        ..alondra_daemon_engine_model::LaunchPlan::new("app/docs", f.project.to_str().unwrap()) };
    f.domain.store.lock().unwrap().set_launch_plan(&"t".into(), &plan, 1).unwrap();
    launch(&f.domain, &f.home, serde_json::from_value(json!({"task": "t"})).unwrap(), |_| None).await.unwrap();
    let routing = f.domain.store.lock().unwrap().model_decision_routing().unwrap();
    assert_eq!(routing, [(Some("claude".into()), Some("docs".into()), Some(r#"["ui"]"#.into()), Some(r#"["user_visible"]"#.into()))]);

    let repeated: LaunchBody = serde_json::from_value(json!({"task": "t", "tags": {"kind": "fix", "risks": ["security", "security"]}})).unwrap();
    let refused = launch(&f.domain, &f.home, repeated, |_| None).await.unwrap_err();
    assert_eq!((refused.code.as_str(), refused.message.as_str()), ("invalid_input", "tags.risks repeats a risk"));
    assert!(serde_json::from_value::<LaunchBody>(json!({"task": "t", "tags": {"kind": "chore"}})).is_err(), "a kind outside the list is refused");
}

#[tokio::test]
async fn a_launch_of_a_task_left_without_a_plan_needs_a_name_and_a_folder() {
    let f = world();
    f.domain.store.lock().unwrap().forget_launch_plan(&"t".into()).unwrap();
    let body: LaunchBody = serde_json::from_value(json!({"task": "t", "model": "sonnet", "reason": "x"})).unwrap();
    let refused = launch(&f.domain, &f.home, body, |_| None).await.unwrap_err();
    assert_eq!(refused.code, "invalid_input");
    assert!(refused.message.contains("name and cwd are required"), "{}", refused.message);
    assert!(f.sdk().starts().is_empty());
}

#[tokio::test]
async fn a_task_in_progress_whose_node_died_is_relaunched_and_assigned_to_the_new_node() {
    let f = world();
    let old = sdk_node_on_the_task(&f, &f.project).await;
    let original = f.sdk().starts()[0]["session"].as_str().unwrap().to_owned();
    f.domain.stop_sdk_host(&old).unwrap();
    let (backend, runtime) = f.domain.sdk().unwrap();
    crate::sdk_owner::end_unowned_host(runtime, backend, "t".into()).unwrap();
    crate::node_exit::take_out(&f.domain, &old.0, "the native process ended", Some(f.project.to_str().unwrap().into()), crate::live::Relaunch::AskUser).unwrap();
    assert!(f.task_node().is_none(), "the confirmed exit releases the old assignment before recovery");
    answer_ended_cards(&f);
    let answer = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    assert_eq!(f.task_node(), Some(agent_of(&answer)));
    assert!(f.domain.owns_sdk_host(&agent_of(&answer)), "the recovered node runs");
    f.launched(2).await;
    assert_resumed(&f, &original);
}

#[tokio::test]
async fn the_changes_the_user_asked_for_reach_the_node_that_takes_the_task_over() {
    let f = world();
    let answer = crate::sdk_owner::launch(&f.domain, crate::sdk_owner::Launch {
        task: "t".into(), name: "Instance B".into(), directory: f.project.to_str().unwrap().into(),
        prompt: "consume-and-hold".into(), plugin: crate::plugins::node_in(&f.home).unwrap(),
        options: LaunchOptions::default(), model: None,
        account: alondra_core::accounts::AccountId::default_account(),
    }).await.unwrap();
    let old = agent_of(&answer);
    let mut changed = f.domain.subscribe();
    while !f.domain.state().unwrap().lifecycle_journal[&TaskId::from("t")].iter()
        .any(|record| matches!(record, alondra_core::domain::LifecycleRecord::TurnStarted { .. })) {
        changed.changed().await.unwrap();
    }
    let original = f.sdk().starts()[0]["session"].as_str().unwrap().to_owned();
    let targets = || vec![alondra_core::domain::DeliveryTarget::Text { text: "Result".into() }];
    command(&f.domain, Actor::Agent { agent_id: old.clone() }, Command::PresentResult { checklist: Vec::new(), attachments: vec![], task_id: "t".into(),
        interaction_id: "final".into(), body: "Done".into(), targets: targets(), is_final: true, nothing_to_review: false,
        card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true) });
    command(&f.domain, user(), Command::SubmitFeedback { merge_failure: None, attachments: vec![], presentation_id: "final".into(), targets: targets(),
        body: "  Use the new palette  ".into(), instruction: alondra_core::domain::Instruction::Changes });
    f.domain.stop_sdk_host(&old).unwrap();
    let (backend, runtime) = f.domain.sdk().unwrap();
    crate::sdk_owner::end_unowned_host(runtime, backend, "t".into()).unwrap();
    command(&f.domain, Actor::System, Command::RecordNodeEnd {
        task_id: "t".into(), end: alondra_core::domain::NodeEnd::Closed, agent_id: Some(old.clone()),
    });
    assert!(f.task_node().is_none(), "confirmed source exit releases the reviewed node while retaining its delivery and feedback");
    answer_ended_cards(&f);
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].state, TaskState::NotStarted);
    launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    let prompt = f.launched(2).await.1[1].clone();
    assert_resumed(&f, &original);
    assert!(prompt.contains("## Changes the user asked for on the result you take over\n\n- Use the new palette"), "{prompt}");
}

async fn sdk_node_on_the_task(f: &Fixture, directory: &Path) -> AgentId {
    let answer = launch(&f.domain, &f.home, launch_body(f, json!({"model": "sonnet", "name": "Instance B", "cwd": directory.to_str().unwrap()})), |_| None).await.unwrap();
    f.sdk().until_answered("t").await;
    agent_of(&answer)
}

fn close_sdk_node(f: &Fixture, agent: &AgentId) {
    f.domain.stop_sdk_host(agent).unwrap();
    let (backend, runtime) = f.domain.sdk().unwrap();
    crate::sdk_owner::end_unowned_host(runtime, backend, "t".into()).unwrap();
    crate::node_exit::user_closed(&f.domain, &agent.0).unwrap();
    answer_ended_cards(f);
}

/// Says how the task goes on where its ended turns left a card open: the user on theirs, the
/// orchestrator on the case of a stopped node.
fn answer_ended_cards(f: &Fixture) {
    let state = f.domain.state().unwrap();
    let open: Vec<_> = state.interactions.values()
        .filter(|card| card.scope == (alondra_core::domain::Scope::Task { task_id: "t".into() }) && state.open(card))
        .collect();
    let cases: Vec<_> = open.iter().filter(|card| alondra_core::domain::is_stall_case(card)).map(|card| card.id.clone()).collect();
    let cards: Vec<_> = open.iter().filter(|card| matches!(&card.payload, alondra_core::domain::InteractionPayload::Question { options, .. }
        if options.iter().any(|option| option.id == alondra_core::domain::CARRY_ON))).map(|card| card.id.clone()).collect();
    for case in cases {
        command(&f.domain, Actor::Orchestrator { generation: state.orchestrator_generation }, Command::SettleStall {
            case_id: case.clone(), task_id: "t".into(), outcome: alondra_core::domain::StallOutcome::Instruct { text: "Go on with your task".into() },
        });
        assert!(!f.domain.state().unwrap().open(&f.domain.state().unwrap().interactions[&case]), "the orchestrator settled the case");
    }
    for card in cards {
        command(&f.domain, user(), Command::AnswerQuestion {
            interaction_id: card.clone(), option_id: alondra_core::domain::CARRY_ON.into(),
            inputs: Default::default(), attachments: vec![],
        });
        assert!(!f.domain.state().unwrap().open(&f.domain.state().unwrap().interactions[&card]), "the user answered the existing card");
    }
}

fn assert_resumed(f: &Fixture, original: &str) {
    let starts = f.sdk().starts();
    let resumed = starts.iter().find(|start| start["resume"].as_str() == Some(original)).expect("the replacement resumes the original native session");
    assert!(resumed["history"].as_u64().unwrap() > 0, "the native history is preserved: {starts:?}");
}

fn connected_node_on_the_task(f: &Fixture) -> u64 {
    connected_node(f, "agent-b")
}

fn connected_node(f: &Fixture, agent: &str) -> u64 {
    give(f, "t", agent);
    command(&f.domain, Actor::Agent { agent_id: agent.into() }, Command::StartTask { task_id: "t".into() });
    f.domain.connect_node(serde_json::from_value(json!({"session_id": agent, "version": "2.1.287", "cwd": f.project.to_str().unwrap()})).unwrap(), 1)
}

#[tokio::test]
async fn replacing_a_tasks_node_stops_it_keeps_its_folder_and_gives_the_task_to_the_new_model() {
    let f = world();
    let old = sdk_node_on_the_task(&f, &f.project).await;
    let original = f.sdk().starts()[0]["session"].as_str().unwrap().to_owned();
    let taken = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "opus"})), |_| None).await.unwrap_err();
    assert!(taken.message.contains("already has a connected agent"), "without replace a live node keeps its task: {}", taken.message);

    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus", "effort": "high", "prompt": "Finish the tokens"})).unwrap();
    let answer = launch(&f.domain, &f.home, body, |_| None).await.unwrap();
    let agent = agent_of(&answer);
    assert!(!f.domain.owns_sdk_host(&old), "the old SDK host was stopped");
    assert!(f.sessions.stopped.lock().unwrap().is_empty(), "native SDK recovery does not stop a legacy process");
    let (folders, prompts) = f.launched(2).await;
    assert_resumed(&f, &original);
    assert!(same(&folders[1], &f.project), "the new node starts where the old one worked: {folders:?}");
    assert_eq!((f.sdk().flag("--model").as_deref(), f.sdk().flag("--effort").as_deref()), (Some("opus"), Some("high")), "{:?}", f.sdk().args());
    assert!(prompts[1].starts_with("Finish the tokens"), "{}", prompts[1]);
    let state = f.domain.state().unwrap();
    assert_eq!(state.agents[&agent].name, "Instance B", "it keeps the old node's name");
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id, Some(agent));
    assert!(!state.slots().iter().any(|agent| agent.id == old), "the old node is disconnected");
    assert!(f.domain.live.closed_task_ids().is_empty());
}

#[tokio::test]
async fn a_replacement_that_cannot_launch_leaves_the_old_node_working() {
    let f = world();
    connected_node_on_the_task(&f);
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus", "cwd": "/no/such/folder"})).unwrap();
    let refused = launch(&f.domain, &f.home, body, |_| None).await.unwrap_err();
    assert_eq!(refused.code, "invalid_input");
    assert!(f.sessions.stopped.lock().unwrap().is_empty() && f.sdk().starts().is_empty());
    let state = f.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id, Some(AgentId::from("agent-b")));
    assert!(state.agents[&AgentId::from("agent-b")].phase.live());

    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None, task_id: "free".into(), request_id: "r".into(), title: "Free".into(),
        agent_id: None, review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
    let body = serde_json::from_value(json!({"task": "free", "replace": true, "model": "opus", "reason": "Needs opus", "name": "x", "cwd": f.project.to_str().unwrap()})).unwrap();
    let nothing = launch(&f.domain, &f.home, body, |_| None).await.unwrap_err();
    assert!(nothing.message.contains("no node to replace"), "{}", nothing.message);
}

#[tokio::test]
async fn a_replacement_works_in_a_worktree_of_a_trusted_repository() {
    let f = world();
    let main = f.project.canonicalize().unwrap();
    let admin = main.join(".git/worktrees/cards");
    let linked = main.join(".claude/worktrees/cards");
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::create_dir_all(&linked).unwrap();
    std::fs::write(linked.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
    std::fs::write(admin.join("commondir"), "../..\n").unwrap();
    std::fs::write(admin.join("gitdir"), format!("{}\n", linked.join(".git").display())).unwrap();
    let config = f.root.join(".claude.json");
    crate::claude_trust::use_config_for_test(&config);
    std::fs::write(&config, json!({"projects": {main.to_str().unwrap(): {"hasTrustDialogAccepted": true}}}).to_string()).unwrap();
    sdk_node_on_the_task(&f, &linked).await;
    let original = f.sdk().starts()[0]["session"].as_str().unwrap().to_owned();
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus", "cwd": linked.to_str().unwrap()})).unwrap();
    launch(&f.domain, &f.home, body, |_| None).await.unwrap();
    let (folders, _) = f.launched(2).await;
    assert_resumed(&f, &original);
    assert!(same(&folders[1], &linked), "{folders:?}");
}

#[tokio::test]
async fn a_replacement_whose_stop_fails_launches_nothing_and_the_old_node_keeps_the_task() {
    let f = world();
    connected_node_on_the_task(&f);
    *f.sessions.stop_fails.lock().unwrap() = Some("claude agents still shows agent-b running after claude stop".into());
    let refused = stop_replaced(&f.domain, &"agent-b".into()).await.unwrap_err();
    assert!(refused.contains("still shows agent-b running"), "the stop returns its actual reason: {refused}");
    assert!(f.sdk().starts().is_empty(), "the stop component never starts a fresh session");
    let state = f.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id, Some(AgentId::from("agent-b")), "the old node keeps its task");
    assert_eq!(state.tasks[&TaskId::from("t")].state, TaskState::Running);
    assert_eq!(state.agents.len(), 1, "no new agent was registered");
    assert!(state.agents[&AgentId::from("agent-b")].phase.live() && !f.domain.live.is_closed("agent-b"));
}

fn release_reasons(f: &Fixture) -> Vec<String> {
    let store = f.domain.store.lock().unwrap();
    let mut query = store.conn_for_test().prepare(
        "SELECT json_extract(command,'$.reason') FROM command_bodies WHERE json_extract(command,'$.kind')='release_task' ORDER BY revision").unwrap();
    query.query_map([], |row| row.get(0)).unwrap().map(Result::unwrap).collect()
}

fn told_orchestrator(f: &Fixture, prefix: &str) -> Vec<String> {
    f.domain.state().unwrap().interactions.values().filter(|i| i.id.0.starts_with(prefix)).map(|i| i.body.clone()).collect()
}

/// Only a worktree Alondra could already delete passes to the replacing node: never the folder
/// the old node was merely launched in.
#[tokio::test]
async fn a_replacement_inherits_only_a_worktree_alondra_owned() {
    let f = world();
    sdk_node_on_the_task(&f, &f.project).await;
    let body = || serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    let answer = launch(&f.domain, &f.home, body(), |_| None).await.unwrap();
    assert_eq!(f.domain.inherited_worktree(&agent_of(&answer).0).unwrap(), None, "the user's project folder is never Alondra's to delete");

    let g = world();
    let old = sdk_node_on_the_task(&g, &g.project).await;
    let dir = g.project.to_str().unwrap();
    g.domain.set_inherited_worktree(&old.0, dir, "agent-a").unwrap();
    let answer = launch(&g.domain, &g.home, body(), |_| None).await.unwrap();
    assert_eq!(g.domain.inherited_worktree(&agent_of(&answer).0).unwrap().as_deref(), Some(dir), "a worktree passed down a chain of replacements stays Alondra's");
}

/// The node a replacement could not stop ends later: the task is freed for that reason, never as
/// closed by the user, and Alondra starts its next node by itself instead of asking the
/// orchestrator to.
#[tokio::test]
async fn a_node_that_ends_after_its_replacement_stopped_short_frees_its_task_to_be_relaunched_by_itself() {
    let f = world();
    connected_node_on_the_task(&f);
    *f.sessions.stop_fails.lock().unwrap() = Some("claude agents still shows agent-b running after claude stop".into());
    stop_replaced(&f.domain, &"agent-b".into()).await.unwrap_err();
    crate::node_exit::user_closed(&f.domain, "agent-b").unwrap();
    assert_eq!(f.task_node(), None, "the task is free");
    assert_eq!(release_reasons(&f), [crate::node_exit::REPLACE_ABORTED]);
    assert_eq!(f.domain.live.closed_task("t").map(|node| node.relaunch), Some(crate::live::Relaunch::ByItself));
    assert!(f.domain.live.user_closed_task_ids().is_empty(), "nobody is asked to relaunch it");
    assert!(told_orchestrator(&f, "replace-aborted-").is_empty(), "the orchestrator is not asked to launch it");
    assert!(f.sdk().starts().is_empty());
}

fn on_a_turn(f: &Fixture) {
    f.domain.live.apply(&alondra_core::Event::DaemonSeen(alondra_core::DaemonObservation {
        session_id: "agent-b".into(), short_id: "agent-b".into(), name: Some("Instance B".into()), ..Default::default() }));
    f.domain.live.observe(&json!({"session_id": "agent-b", "hook_event_name": "UserPromptSubmit", "prompt": "go"}));
    assert!(f.domain.live.node("agent-b").is_some_and(|node| node.busy), "agent-b is on a turn");
}

#[tokio::test]
async fn a_node_on_a_turn_is_stopped_once_the_turn_ends() {
    let f = world();
    connected_node_on_the_task(&f);
    on_a_turn(&f);
    let old = AgentId::from("agent-b");
    let mut stopping = Box::pin(stop_replaced(&f.domain, &old));
    std::future::poll_fn(|context| {
        assert!(std::future::Future::poll(stopping.as_mut(), context).is_pending(), "a busy legacy turn must hold its stop");
        std::task::Poll::Ready(())
    }).await;
    assert!(f.sessions.stopped.lock().unwrap().is_empty());
    f.domain.live.observe(&json!({"session_id": "agent-b", "hook_event_name": "Stop"}));
    stopping.await.unwrap();
    assert_eq!(*f.sessions.busy_at_stop.lock().unwrap(), [false], "claude stop waited for the turn to end");
    assert!(f.sdk().starts().is_empty());
}

#[tokio::test]
async fn a_node_whose_turn_does_not_end_is_interrupted_before_it_is_stopped() {
    let f = world();
    connected_node_on_the_task(&f);
    let node = Node::connect(&f.domain, "agent-b");
    on_a_turn(&f);
    stop_replaced(&f.domain, &"agent-b".into()).await.unwrap();
    assert_eq!(node.interrupts(), 1, "past the deadline the turn is interrupted first");
    assert_eq!(f.sessions.stopped.lock().unwrap().len(), 1);
    assert!(f.sdk().starts().is_empty());
}

#[tokio::test]
async fn the_replacing_node_is_told_to_continue_in_the_worktree_without_making_another() {
    let f = world();
    let repo = f.root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-q", "-b", "main"]);
    git_in(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "start"]);
    git_in(&repo, &["worktree", "add", "-q", "-b", "worktree-t-cards-3", f.project.to_str().unwrap()]);
    sdk_node_on_the_task(&f, &f.project).await;
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    launch(&f.domain, &f.home, body, |_| None).await.unwrap();
    let prompt = f.launched(2).await.1[1].clone();
    let expected = format!("You continue in {}, branch worktree-t-cards-3", f.project.display());
    assert!(prompt.contains(&expected) && prompt.contains("Do not create another worktree and do not use EnterWorktree"), "{prompt}");
}

fn git_in(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// The project folder as a main checkout with one commit.
fn main_checkout(f: &Fixture) {
    git_in(&f.project, &["init", "-q", "-b", "main"]);
    git_in(&f.project, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "start"]);
}

/// The bug: The user stopped the node of a task, so it had no node to replace, and the orchestrator
/// launched it again with the repository's root as the folder while the task's worktree, with the
/// earlier node's work, was still there. The new node started in the main checkout.
#[tokio::test]
async fn a_relaunch_of_a_task_whose_earlier_worktree_exists_starts_there_not_in_the_main_checkout() {
    let f = world();
    main_checkout(&f);
    let earlier = f.project.join(".claude/worktrees/safe-permissions");
    git_in(&f.project, &["worktree", "add", "-q", "-b", "worktree-safe-permissions", earlier.to_str().unwrap()]);
    std::fs::write(earlier.join("half-done.rs"), "work").unwrap();
    f.domain.set_task_worktree(&TaskId::from("t"), earlier.to_str().unwrap()).unwrap();
    let answer = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    let (folders, prompts) = f.launched(1).await;
    assert!(same(&folders[0], &earlier), "the new node starts in the task's worktree, not {folders:?}");
    assert!(earlier.join("half-done.rs").exists(), "with the earlier node's work in it");
    assert!(prompts[0].contains("You continue in"), "and is told so: {}", prompts[0]);
    assert!(same(Path::new(&f.domain.inherited_worktree(&agent_of(&answer).0).unwrap().unwrap()), &earlier), "Alondra deletes it with the task");
}

#[tokio::test]
async fn a_task_launched_through_the_sdk_in_a_worktree_alondra_made_has_it_as_the_tasks_worktree() {
    let f = world();
    main_checkout(&f);
    let answer = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    let made = f.project.join(".claude/worktrees/t");
    let recorded = f.domain.task_worktree(&TaskId::from("t")).unwrap().expect("the task's worktree is recorded");
    assert!(same(Path::new(&recorded), &made), "{recorded}");
    let owned = f.domain.inherited_worktree(&agent_of(&answer).0).unwrap().expect("the SDK node owns the worktree Alondra made");
    assert!(same(Path::new(&owned), &made), "{owned}");
    let (folders, _) = f.launched(1).await;
    assert!(same(&folders[0], &made), "the SDK node works in it: {folders:?}");
}

#[tokio::test]
async fn a_first_launch_in_a_main_checkout_gets_a_worktree_of_its_own_and_the_next_launch_reuses_it() {
    let f = world();
    main_checkout(&f);
    launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    let made = f.project.join(".claude/worktrees/t");
    let (folders, prompts) = f.launched(1).await;
    assert!(same(&folders[0], &made), "{folders:?}");
    assert!(prompts[0].contains("the worktree Alondra made for this task"), "{}", prompts[0]);
    let recorded = f.domain.task_worktree(&TaskId::from("t")).unwrap().unwrap();
    f.sdk().until_answered("t").await;
    let original = f.sdk().starts()[0]["session"].as_str().unwrap().to_owned();
    let original_account = f.domain.state().unwrap().tasks[&TaskId::from("t")].account_key();
    assert!(same(Path::new(&recorded), &made));
    launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet", "replace": true})), |_| None).await.unwrap();
    let (folders, _) = f.launched(2).await;
    assert!(same(&folders[1], &made), "the next launch works in the same worktree: {folders:?}");
    let starts = f.sdk().starts();
    assert_eq!(starts[1]["resume"].as_str(), Some(original.as_str()), "replacement retains the original native session: {starts:?}");
    assert!(starts[1]["history"].as_u64().unwrap() > 0, "replacement retains native history: {starts:?}");
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].account_key(), original_account);
}

#[tokio::test]
async fn no_node_is_launched_in_a_main_checkout_alondra_cannot_isolate() {
    let f = world();
    std::fs::create_dir_all(f.project.join(".git")).unwrap();
    let refused = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap_err();
    assert_eq!(refused.code, "main_checkout", "{}", refused.message);
    assert!(f.sdk().starts().is_empty(), "claude never ran");
}

#[test]
fn a_stopped_session_is_one_claude_agents_no_longer_shows_running() {
    let listing = json!([{"id": "agent-b", "state": "idle"}, {"id": "old00000", "state": "stopped"}, {"id": "done0000", "state": "done"}]).to_string();
    assert!(crate::sdk_handover::still_running(&listing, "agent-b"));
    for gone in ["old00000", "done0000", "missing0"] {
        assert!(!crate::sdk_handover::still_running(&listing, gone), "{gone}");
    }
}


/// Waits on the domain until the orchestrator is told something with `prefix`.
async fn until_told(f: &Fixture, prefix: &str) -> Vec<String> {
    let mut changed = f.domain.subscribe();
    loop {
        let told = told_orchestrator(f, prefix);
        if !told.is_empty() {
            return told;
        }
        changed.changed().await.unwrap();
    }
}

fn replace_body() -> LaunchBody {
    serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap()
}

/// Waits on the domain until a node other than agent-b holds the task.
async fn until_replaced(f: &Fixture, old: &AgentId) {
    let mut changed = f.domain.subscribe();
    while f.task_node().is_none_or(|agent| agent == *old) {
        changed.changed().await.unwrap();
    }
}

/// Waits until the replacement under way gives the task's launch guard back.
async fn until_launchable(f: &Fixture) {
    f.domain.live.until_launch_ends("t").await;
    assert!(f.domain.live.begin_launch("t"), "the launch guard is released once it ends");
}

/// Waiting for the old node's turn can outlast the call: the orchestrator gets its answer at once,
/// and a replacement that finishes tells it nothing more.
#[tokio::test]
async fn a_replacement_answers_before_the_old_node_stops_and_finishes_without_a_notice() {
    let f = world();
    connected_node_on_the_task(&f);
    on_a_turn(&f);
    let (release, released) = tokio::sync::oneshot::channel();
    let domain = Arc::clone(&f.domain);
    let answer = launch_replacement_work(Arc::clone(&f.domain), "t".into(), move |checked| async move {
        checked();
        released.await.map_err(|_| DomainError::new("replace_failed", "the explicit stop barrier was dropped"))?;
        stop_replaced(&domain, &"agent-b".into()).await.map_err(|reason| DomainError::new("replace_failed", reason))?;
        Ok(json!({"stopped": "agent-b"}))
    }).await.unwrap();
    assert_eq!(answer["replacing"], true, "{answer}");
    assert!(answer["message"].as_str().unwrap().contains("Alondra starts it again by itself; you are told only if the old node keeps the task"), "{answer}");
    assert!(f.sessions.stopped.lock().unwrap().is_empty(), "the shared supervisor answers before its checked stop work is released");
    let again = launch_replacing(f.domain.clone(), f.home.clone(), replace_body(), |_| None).await.unwrap_err();
    assert_eq!(again.code, "conflict", "a second launch waits for the replacement under way: {}", again.message);
    f.domain.live.observe(&json!({"session_id": "agent-b", "hook_event_name": "Stop"}));
    release.send(()).unwrap();
    until_launchable(&f).await;
    assert_eq!(*f.sessions.stopped.lock().unwrap(), ["agent-b"]);
    assert!(told_orchestrator(&f, "replaced-").is_empty(), "the successful supervised stop has no failure notice");
    assert!(f.sdk().starts().is_empty(), "the component does not fabricate a replacement session");
}

#[tokio::test]
async fn a_replacement_that_fails_after_answering_tells_the_orchestrator_why() {
    let f = world();
    connected_node_on_the_task(&f);
    *f.sessions.stop_fails.lock().unwrap() = Some("claude agents still shows agent-b running after claude stop".into());
    let domain = Arc::clone(&f.domain);
    let answer = launch_replacement_work(Arc::clone(&f.domain), "t".into(), move |checked| async move {
        checked();
        stop_replaced(&domain, &"agent-b".into()).await.map_err(|reason| DomainError::new("replace_failed", reason))?;
        Ok(json!({"stopped": "agent-b"}))
    }).await.unwrap();
    assert_eq!(answer["replacing"], true);
    let told = until_told(&f, "replaced-").await;
    assert!(told[0].contains("did not finish") && told[0].contains("still shows agent-b running"), "{}", told[0]);
    assert!(!told[0].contains("keeps its node"), "{}", told[0]);
    until_launchable(&f).await;
    assert_eq!(f.task_node(), Some("agent-b".into()));
    assert!(f.sdk().starts().is_empty());
}

#[tokio::test]
async fn a_replacement_refused_before_stopping_anything_answers_with_the_refusal() {
    let f = world();
    connected_node_on_the_task(&f);
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus", "cwd": "/no/such/folder"})).unwrap();
    let refused = launch_replacing(f.domain.clone(), f.home.clone(), body, |_| None).await.unwrap_err();
    assert_eq!(refused.code, "invalid_input");
    assert!(f.sessions.stopped.lock().unwrap().is_empty());
    assert!(f.domain.live.begin_launch("t"), "the launch guard is released");
}

/// Once the old node is stopped the task has no node: when the new one does not start, the
/// automatic launch starts it again by itself, resuming the same session, and the orchestrator is
/// not asked to launch it.
#[tokio::test]
async fn a_replacement_whose_new_node_does_not_start_is_relaunched_by_itself() {
    let f = world();
    let old = sdk_node_on_the_task(&f, &f.project).await;
    let original = f.sdk().starts()[0]["session"].as_str().unwrap().to_owned();
    struct RestoreStarts<'a>(&'a SdkLaunches);
    impl Drop for RestoreStarts<'_> { fn drop(&mut self) { self.0.refuse_starts(false); } }
    let _restore = RestoreStarts(f.sdk());
    f.sdk().refuse_starts(true);
    launch_replacing(f.domain.clone(), f.home.clone(), replace_body(), |_| None).await.unwrap();
    until_launchable(&f).await;
    f.domain.live.end_launch("t");
    assert_eq!(f.task_node(), None);
    assert!(!f.domain.owns_sdk_host(&old));
    assert_eq!(f.sdk().starts().len(), 1, "only the original verified native session was started");

    f.sdk().refuse_starts(false);
    crate::auto_launch::pass(&f.domain, &f.home).await.unwrap();
    assert!(f.task_node().is_some_and(|agent| agent != old), "Alondra started a new node by itself");
    f.launched(2).await;
    assert_resumed(&f, &original);
    assert!(told_orchestrator(&f, "replaced-").is_empty(), "the orchestrator is not asked to launch it");
}

/// The call can be dropped as soon as the replacement starts: the replacement still finishes.
#[tokio::test]
async fn a_dropped_replace_call_still_finishes_the_replacement() {
    let f = world();
    let old = sdk_node_on_the_task(&f, &f.project).await;
    let native = f.sdk().starts()[0]["session"].as_str().unwrap().to_owned();
    let call = launch_replacing(f.domain.clone(), f.home.clone(), replace_body(), |_| None);
    assert!(tokio::time::timeout(std::time::Duration::ZERO, call).await.is_err(), "the call is dropped before it answers");
    until_replaced(&f, &old).await;
    until_launchable(&f).await;
    f.launched(2).await;
    assert_resumed(&f, &native);
}

/// A panic in the replacement gives the launch guard back and the orchestrator its reason.
#[tokio::test]
async fn a_replacement_that_panics_releases_the_task_and_tells_the_orchestrator() {
    let f = world();
    connected_node_on_the_task(&f);
    f.sessions.stop_panics.store(true, Ordering::SeqCst);
    let domain = Arc::clone(&f.domain);
    let reason = match launch_replacement_work(Arc::clone(&f.domain), "t".into(), move |checked| async move {
        checked();
        stop_replaced(&domain, &"agent-b".into()).await.map_err(|reason| DomainError::new("replace_failed", reason))?;
        Ok(json!({"stopped": "agent-b"}))
    }).await {
        Err(refused) => refused.message,
        Ok(_) => until_told(&f, "replaced-").await.remove(0),
    };
    assert!(reason.contains("stopped unexpectedly"), "{reason}");
    until_launchable(&f).await;
    assert_eq!(f.task_node(), Some("agent-b".into()));
    assert!(f.sdk().starts().is_empty(), "panic recovery cannot create a new native identity");
}

/// The branch where Claude Code made the old node's worktree, read from a Claude home of the
/// test's own.
#[tokio::test]
async fn a_legacy_worktree_is_found_without_replacing_an_unverified_native_session() {
    let f = world();
    let claude_home = f.root.join("claude");
    crate::watcher::use_claude_home_for_test(&claude_home);
    let old = "0ldn0de0-0000-4000-8000-000000000000";
    command(&f.domain, Actor::System, Command::RegisterAgent { agent_id: old.into(), name: "Old".into(), at: 0 });
    let made = f.project.join(".claude/worktrees/t");
    std::fs::create_dir_all(&made).unwrap();
    std::fs::create_dir_all(claude_home.join("jobs/0ldn0de0")).unwrap();
    std::fs::write(claude_home.join("jobs/0ldn0de0/state.json"),
        json!({"cwd": f.project.to_str().unwrap(), "worktreePath": made.to_str().unwrap()}).to_string()).unwrap();
    connected_node(&f, old);
    assert_eq!(owned_worktree(&f.domain, old).as_deref(), made.to_str());
    let refused = launch(&f.domain, &f.home, replace_body(), |_| None).await.unwrap_err();
    assert_eq!(refused.code, "native_session_required");
    assert_eq!(f.task_node(), Some(old.into()));
    assert!(f.sessions.stopped.lock().unwrap().is_empty());
    assert!(f.sdk().starts().is_empty());
}

/// What Alondra keeps about a session only while it lives goes when it ends.
#[tokio::test]
async fn an_ended_session_leaves_no_inherited_worktree_or_aborted_replace_behind() {
    let f = bare_world();
    connected_node_on_the_task(&f);
    f.domain.set_inherited_worktree("agent-b", f.project.to_str().unwrap(), "agent-a").unwrap();
    f.domain.live.note_aborted_replace("agent-b", "stop failed");
    crate::node_exit::take_out(&f.domain, "agent-b", "node closed by the user", None, crate::live::Relaunch::AskUser).unwrap();
    assert_eq!(f.domain.inherited_worktree("agent-b").unwrap(), None);
    assert_eq!(f.domain.live.take_aborted_replace("agent-b"), None);
}

#[tokio::test]
async fn a_launch_of_the_task_that_implements_an_approved_plan_carries_the_whole_plan_and_says_not_to_plan_again() {
    let f = world();
    let author = Actor::Agent { agent_id: "agent-b".into() };
    give(&f, "t", "agent-b");
    let plan = "# Index the library\n\n- [ ] Store feature prints in Core Data\n- [ ] Follow changes in the background\n";
    let targets = || vec![alondra_core::domain::DeliveryTarget::Text { text: "Report".into() }];
    command(&f.domain, author, Command::PresentResult { checklist: Vec::new(), attachments: vec![], task_id: "t".into(), interaction_id: "report".into(),
        body: "Viable: index once".into(), targets: targets(), is_final: true, nothing_to_review: false,
        card: alondra_core::domain::CardSpec::delivery("Read the report", &targets(), true).plan(plan) });
    command(&f.domain, user(), Command::DecideProposedPlan { presentation_id: "report".into(), targets: targets(),
        choice: alondra_core::domain::PlanChoice::Implement });
    let orchestrator = Actor::Orchestrator { generation: 0 };
    command(&f.domain, orchestrator.clone(), Command::TitleRequest { request_id: "implement-report".into(), title: "Index the library".into() });
    command(&f.domain, orchestrator, Command::CreateTask { plan_items: None, continues: None, task_id: "build".into(), request_id: "implement-report".into(), title: "Index the library".into(),
        agent_id: None, review_prs: false, dependencies: ["t".into()].into(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });

    launch(&f.domain, &f.home, launch_body(&f, json!({"task": "build", "model": "sonnet"})), |_| None).await.unwrap();
    let prompt = f.prompt().await;
    assert!(prompt.contains("## The plan the user approved") && prompt.contains("do not plan again"), "{prompt}");
    assert!(prompt.contains("- [ ] Store feature prints in Core Data\n- [ ] Follow changes in the background"), "the whole plan: {prompt}");
    assert_eq!(prompt.matches("Store feature prints in Core Data").count(), 1, "the plan once, not again as the request: {prompt}");
}

#[test]
fn a_reopened_task_carries_the_plan_approved_in_its_own_previous_native_session() {
    use alondra_core::domain::{CardKind, CardSpec, ContentKind, Hold, NodeEnd};
    let f = bare_world();
    give(&f, "t", "agent-b");
    let plan = "# Make the editor readable\n\n- [ ] Preserve the chosen typeface\n- [ ] Verify the editor contrast\n";
    command(&f.domain, Actor::System, Command::HoldHook {
        interaction_id: "own-plan".into(), task_id: "t".into(), agent_id: "agent-b".into(),
        hold: Hold::Plan, body: plan.into(), options: Hold::Plan.options(),
        card: CardSpec::new(CardKind::Review, "Review the editor plan").content(ContentKind::Plan, plan),
    });
    command(&f.domain, user(), Command::AnswerQuestion {
        attachments: vec![], interaction_id: "own-plan".into(), option_id: Hold::APPROVE.into(), inputs: Default::default(),
    });
    let before = f.domain.state().unwrap();
    let checklist = before.checklists[&TaskId::from("t")].clone();
    assert!(approved_plan(&before, &before.tasks[&TaskId::from("t")]).is_empty(), "the current session already holds its own approved plan");

    command(&f.domain, user(), Command::CancelTask { task_id: "t".into() });
    command(&f.domain, Actor::System, Command::RecordNodeEnd {
        task_id: "t".into(), end: NodeEnd::Closed, agent_id: Some("agent-b".into()),
    });
    command(&f.domain, the_orchestrator(&f.domain), Command::ReopenTask { task_id: "t".into() });

    let state = f.domain.state().unwrap();
    assert_eq!(state.checklists[&TaskId::from("t")], checklist);
    let context = approved_plan(&state, &state.tasks[&TaskId::from("t")]);
    assert!(context.contains(plan.trim()), "a clean native session must receive the whole approved plan: {context}");
    assert!(context.contains("do not plan again"), "the previous approval still applies: {context}");
    assert_eq!(context.matches("Preserve the chosen typeface").count(), 1);
}

/// The task the user decided on, «Editor fonts», and «Editor colours» (t), which waits for it.
fn decided_on(f: &Fixture) {
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { task_id: "fonts".into(), request_id: "r".into(),
        title: "Editor fonts".into(), agent_id: Some("agent-b".into()), review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("fonts", "/tmp")), plan_items: None, continues: None });
    command(&f.domain, Actor::Agent { agent_id: "agent-b".into() }, Command::StartTask { task_id: "fonts".into() });
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::SetDependencies { task_id: "t".into(),
        dependencies: [TaskId::from("fonts")].into(), stack: None });
    command(&f.domain, Actor::Agent { agent_id: "agent-b".into() }, Command::AskQuestion {
        attachments: vec![], interaction_id: "font-choice".into(), scope: alondra_core::domain::Scope::Task { task_id: "fonts".into() },
        recipient: alondra_core::domain::Recipient::User, body: "Which typeface?".into(), blocking: true,
        options: vec![alondra_core::domain::QuestionOption { id: "mono".into(), label: "Monospaced".into(), description: None },
            alondra_core::domain::QuestionOption { id: "serif".into(), label: "Serif".into(), description: None }],
        card: alondra_core::domain::CardSpec::new(alondra_core::domain::CardKind::Decide, "Choose the editor's typeface").why("Its colours come next"),
    });
}

fn decide(f: &Fixture) {
    command(&f.domain, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "font-choice".into(),
        option_id: "serif".into(), inputs: Default::default() });
}

#[tokio::test]
async fn a_task_held_for_a_plan_review_is_not_launched_until_the_orchestrator_settles_it() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    decided_on(&f);
    decide(&f);
    let held = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap_err();
    assert_eq!(held.code, "plan_review_pending");
    assert!(held.message.contains("Editor fonts"), "{}", held.message);
    assert!(f.sdk().starts().is_empty(), "Claude never runs for a held task");
    command(&f.domain, actor.clone(), Command::SettlePlanReview { review_id: "font-choice".into(), task_id: "t".into(),
        outcome: alondra_core::domain::PlanReviewOutcome::Unchanged { reason: "Colours do not depend on the typeface".into() } });
    command(&f.domain, actor.clone(), Command::SetDependencies { task_id: "t".into(), dependencies: Default::default(), stack: None });
    let answer = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    assert_eq!(f.task_node(), Some(agent_of(&answer)));
}

#[tokio::test]
async fn a_launch_of_one_phase_of_an_approved_plan_carries_the_whole_plan_and_names_its_points() {
    use alondra_core::domain::{CardKind, CardSpec, ContentKind, Hold, PlanItems};
    let f = world();
    give(&f, "t", "agent-b");
    let plan = "# Fast views\n\n## Phase 1\n- [ ] Guards\n\n## Phase 2\nDepends on: phase 1\n- [ ] Read index\n- [ ] Index matches the state\n";
    command(&f.domain, Actor::System, Command::HoldHook { interaction_id: "p".into(), task_id: "t".into(), agent_id: "agent-b".into(),
        hold: Hold::Plan, body: plan.into(), options: Hold::Plan.options(),
        card: CardSpec::new(CardKind::Review, "Fast views").content(ContentKind::Plan, plan) });
    command(&f.domain, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "p".into(), option_id: Hold::APPROVE.into(), inputs: Default::default() });
    let request = f.domain.state().unwrap().tasks[&TaskId::from("t")].request_id.clone();
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { task_id: "index".into(), request_id: request, title: "Read index".into(),
        agent_id: None, review_prs: false, dependencies: ["t".into()].into(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        plan_items: Some(PlanItems { from_task: "t".into(), items: vec!["2".into(), "3".into()] }), continues: None });

    connect_claude_and_codex(&f);
    let original = alondra_daemon_engine_model::LaunchPlan { mission: Some("Implement the read index; preserve the approved scope.".into()),
        model: Some("claude/opus".into()), reason: Some("Implementation".into()),
        ..alondra_daemon_engine_model::LaunchPlan::new("app/index", f.project.to_str().unwrap()) };
    f.domain.store.lock().unwrap().set_launch_plan(&"index".into(), &original, 1).unwrap();
    let checklist = f.domain.state().unwrap().checklists[&TaskId::from("index")].clone();
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("index")].state, TaskState::WaitingDependency);
    partial_plan(&f, "index", json!({"model": "claude/sonnet"})).unwrap();
    assert_eq!(f.domain.state().unwrap().checklists[&TaskId::from("index")], checklist);
    assert_eq!(get_launch_plan(&f.domain, &"index".into()).unwrap()["launch"]["mission"], original.mission.unwrap());
    launch(&f.domain, &f.home, launch_body(&f, json!({"task": "index"})), |_| None).await.unwrap();
    let prompt = f.prompt().await;
    assert!(prompt.contains("## The plan the user approved") && prompt.contains("do not plan again"), "{prompt}");
    assert!(prompt.contains("Your part of it is points 2, 3; the other points are other tasks"), "{prompt}");
    assert!(prompt.contains("- [ ] Guards") && prompt.contains("- [ ] Index matches the state"), "the whole plan: {prompt}");
}

// The verified legacy handover has its own integration suite; an unknown session stays intact.
#[tokio::test]
async fn an_unverified_legacy_native_session_is_kept_until_it_can_be_recovered() {
    let f = world();
    connected_node_on_the_task(&f);
    let original = f.domain.state().unwrap().tasks[&TaskId::from("t")].clone();
    let refused = launch(&f.domain, &f.home, replace_body(), |_| None).await.unwrap_err();
    assert_eq!(refused.code, "native_session_required");
    assert!(f.sessions.stopped.lock().unwrap().is_empty());
    assert!(f.sdk().starts().is_empty());
    let state = f.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id, original.agent_id);
    assert_eq!(state.tasks[&TaskId::from("t")].account_key(), original.account_key());
    assert!(state.agents[&AgentId::from("agent-b")].phase.live());
    assert!(!f.domain.live.is_closed("agent-b"));
}

#[tokio::test]
async fn a_bare_model_launches_on_the_provider_whose_sheets_know_it_when_two_are_connected() {
    use alondra_core::model_selection::{ModelProfile, TaskSelection};
    let f = world();
    std::fs::write(alondra_core::options::path_in(&f.home), "[accounts]\nconnected = [\"claude\", \"codex\"]\n").unwrap();
    command(&f.domain, Actor::System, Command::RecordTaskSelection { task_id: "t".into(), selection: TaskSelection::automatic(ModelProfile::Implementation), pending: None });
    let launched = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "opus"})), |_| None).await;
    assert!(launched.as_ref().err().is_none_or(|refusal| !refusal.message.contains("Specify the provider")), "{launched:?}");
}

// A launch runs through the SDK or does not start, with its reason, and no node takes the task.
#[tokio::test]
async fn an_sdk_launch_that_fails_leaves_the_task_without_a_node() {
    let f = bare_world();
    f.domain.enable_sdk(broken_backend(&f));
    let refused = launch(&f.domain, &f.home, launch_body(&f, json!({"effort": "light", "title": "Editor colours"})), |_| Some(hard_choices())).await.unwrap_err();
    assert_eq!(refused.code, "launch_failed", "{refused:?}");
    assert!(refused.message.contains("did not start through the Agent SDK"), "{}", refused.message);
    let state = f.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id, None, "no node took the task");
    assert_eq!(state.agents.len(), 1, "no node was registered for it");
}

/// The user added a second Claude account, Work, signed in like the default one.
fn with_work_account(f: &Fixture) -> PathBuf {
    use alondra_core::accounts::{AccountBilling, AccountId, AccountState, AccountStatus, Provider};
    std::fs::write(alondra_core::options::path_in(&f.home), "[[accounts.claude]]\nid = \"work\"\nname = \"Work\"\n").unwrap();
    let folder = f.home.join("accounts/work");
    std::fs::create_dir_all(&folder).unwrap();
    let accounts = ["default", "work"].map(|id| AccountStatus {
        provider: Provider::Claude, id: AccountId::parse(id).unwrap(), name: id.into(), state: AccountState::SignedIn,
        native_identity: None, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0, profile: None,
    }).to_vec();
    command(&f.domain, Actor::System, Command::RecordAccounts { accounts, reserve: alondra_core::usage::default_reserve() });
    folder
}

fn week_used(f: &Fixture, account: &str, used: f64) {
    use alondra_core::usage::{Share, UsageReading, WindowKind, WindowReading};
    let reading = UsageReading {
        windows: vec![WindowReading { kind: WindowKind::Weekly, used: Share::of(used), resets_at: Some(alondra_core::now_millis() + 3 * 86_400_000), exhausted: false }],
        plan: None, complete: true,
    };
    crate::usage_probe::record(&f.domain, &alondra_core::accounts::AccountId::parse(account).unwrap(), reading, alondra_core::now_millis());
}

fn config_dir_sha256(f: &Fixture) -> Option<String> {
    let capture: Value = serde_json::from_slice(&std::fs::read(&f.sdk().capture).unwrap()).unwrap();
    capture["env_value_sha256"]["CLAUDE_CONFIG_DIR"].as_str().map(str::to_owned)
}

fn sha256(text: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(text.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

#[tokio::test]
async fn a_launch_starts_on_the_account_with_the_most_margin() {
    let f = world();
    let folder = with_work_account(&f);
    week_used(&f, "default", 0.8);
    week_used(&f, "work", 0.1);
    launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    f.launched(1).await;
    assert_eq!(config_dir_sha256(&f), Some(sha256(folder.to_str().unwrap())), "Claude runs with the work account's folder");
    let task = f.domain.state().unwrap().tasks[&TaskId::from("t")].clone();
    assert_eq!(task.account.as_ref().map(|account| account.as_str()), Some("work"), "the task keeps the account its execution runs on");
}

// With Codex connected too, margin still spreads new Claude work: the account with more room takes
// it, its folder is the host's configuration and the task keeps the account.
#[tokio::test]
async fn with_codex_connected_a_launch_still_starts_on_the_claude_account_with_the_most_margin() {
    use alondra_core::accounts::{AccountBilling, AccountId, AccountState, AccountStatus, Provider};
    let f = world();
    let folder = with_work_account(&f);
    std::fs::write(alondra_core::options::path_in(&f.home),
        "[accounts]\nconnected = [\"claude\", \"codex\"]\n\n[[accounts.claude]]\nid = \"work\"\nname = \"Work\"\n\n[[accounts.codex]]\nid = \"default\"\nname = \"Default\"\n").unwrap();
    let status = |provider, id: &str| AccountStatus { provider, id: AccountId::parse(id).unwrap(), name: id.into(), state: AccountState::SignedIn,
        native_identity: None, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0, profile: None };
    let accounts = vec![status(Provider::Claude, "default"), status(Provider::Claude, "work"), status(Provider::Codex, "default")];
    command(&f.domain, Actor::System, Command::RecordAccounts { accounts, reserve: alondra_core::usage::default_reserve() });
    let model = |name: &str| alondra_core::model_selection::CatalogModel { model: name.into(), display_name: name.into(), reasoning_efforts: vec!["medium".into()],
        default_reasoning_effort: Some("medium".into()), context_window: None, input_modalities: vec!["text".into()], is_default: false, hidden: false };
    for (provider, id, models) in [(Provider::Claude, "default", vec![model("sonnet")]), (Provider::Claude, "work", vec![model("sonnet")]), (Provider::Codex, "default", vec![model("gpt-6")])] {
        let account = alondra_core::accounts::AccountKey::new(provider, AccountId::parse(id).unwrap());
        command(&f.domain, Actor::System, Command::RecordModelCatalog { catalog: alondra_core::model_selection::AccountModelCatalog { account, revision: "fixture".into(), native_default: None, measured_at: 100, models } });
    }
    week_used(&f, "default", 0.21);
    week_used(&f, "work", 0.0);
    launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    f.launched(1).await;
    assert_eq!(config_dir_sha256(&f), Some(sha256(folder.to_str().unwrap())), "Claude runs with the work account's folder");
    let task = f.domain.state().unwrap().tasks[&TaskId::from("t")].clone();
    assert_eq!(task.account.as_ref().map(|account| account.as_str()), Some("work"), "the task keeps the account its execution runs on");
    let stored: String = rusqlite::Connection::open(crate::domain_database_migration::active_path(&f.home)).unwrap()
        .query_row("SELECT account FROM task_accounts WHERE task_id='t'", [], |row| row.get(0)).unwrap();
    assert_eq!(stored, "work");
}

// No account can work: the launch waits for capacity, a state of its task the gate holds, never a
// task in nobody's hands; once an account resets it goes ahead there.
#[tokio::test]
async fn while_no_account_can_work_a_launch_waits_for_capacity_and_goes_ahead_once_one_can() {
    use alondra_core::domain::{GateCause, Holder};
    let f = world();
    let folder = with_work_account(&f);
    let now = alondra_core::now_millis();
    crate::usage_probe::record_limit(&f.domain, &alondra_core::accounts::AccountId::default_account(), Some(now + 3_600_000), now);
    crate::usage_probe::record_limit(&f.domain, &alondra_core::accounts::AccountId::parse("work").unwrap(), Some(now + 7_200_000), now);
    let refused = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap_err();
    assert_eq!(refused.code, "usage_exhausted", "{}", refused.message);
    assert!(f.sdk().starts().is_empty() && f.task_node().is_none(), "nothing started");
    let state = f.domain.state().unwrap();
    let facts = crate::start_failures::facts(&f.domain, &state).unwrap();
    let task = &state.tasks[&TaskId::from("t")];
    assert_eq!(state.holder(task, &facts[&task.id]), Ok(Holder::Gate(GateCause::Room)), "the task waits for capacity");

    week_used(&f, "work", 0.3);
    crate::usage_probe::follow(&f.domain, alondra_core::now_millis());
    f.domain.live.usage_pause.resume(alondra_core::now_millis());
    launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    f.launched(1).await;
    assert_eq!(config_dir_sha256(&f), Some(sha256(folder.to_str().unwrap())), "on the account that came back");
}

/// The user also has API, paid per use, and chose it for the request: its task runs there even
/// while every subscription is at its limit. A request left on automatic waits as before.
#[tokio::test]
async fn a_request_that_chose_an_account_paid_per_use_runs_there_and_only_then() {
    let f = world();
    with_work_account(&f);
    std::fs::write(alondra_core::options::path_in(&f.home),
        "[[accounts.claude]]\nid = \"work\"\nname = \"Work\"\n\n[[accounts.claude]]\nid = \"api\"\nname = \"API\"\nbilling = { kind = \"pay_per_use\" }\n").unwrap();
    let api = f.home.join("accounts/api");
    std::fs::create_dir_all(&api).unwrap();
    let now = alondra_core::now_millis();
    crate::usage_probe::record_limit(&f.domain, &alondra_core::accounts::AccountId::default_account(), Some(now + 3_600_000), now);
    crate::usage_probe::record_limit(&f.domain, &alondra_core::accounts::AccountId::parse("work").unwrap(), Some(now + 7_200_000), now);
    let file = OptionsFile::parse(&std::fs::read_to_string(alondra_core::options::path_in(&f.home)).unwrap()).unwrap();
    let choices = |account: &str| Choices {
        project: None,
        options: file.resolve(None, &std::collections::BTreeMap::from([(alondra_core::options::ACCOUNT.to_owned(),
            alondra_core::options::Choice::Value(account.into()))])).unwrap(),
        issue: None, issue_text: None, attachments: vec![],
    };

    let automatic = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| Some(choices("auto"))).await.unwrap_err();
    assert_eq!(automatic.code, "usage_exhausted", "Alondra never picks the account paid per use: {}", automatic.message);
    assert!(f.sdk().starts().is_empty());

    launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| Some(choices("api"))).await.unwrap();
    f.launched(1).await;
    assert_eq!(config_dir_sha256(&f), Some(sha256(api.to_str().unwrap())), "Claude runs with the API account's folder");
    let task = f.domain.state().unwrap().tasks[&TaskId::from("t")].clone();
    assert_eq!(task.account.as_ref().map(|account| account.as_str()), Some("api"));
}


#[tokio::test]
async fn a_started_legacy_background_node_cannot_be_stopped_to_select_another_account(){
    use alondra_core::accounts::{AccountId,AccountState};
    let f=world();with_work_account(&f);connected_node_on_the_task(&f);
    let mut accounts=f.domain.state().unwrap().accounts.clone();
    accounts.iter_mut().find(|account|account.id.is_default()).unwrap().state=AccountState::Disabled;
    command(&f.domain,Actor::System,Command::RecordAccounts{accounts,reserve:alondra_core::usage::default_reserve()});
    let choices=Choices{project:None,options:OptionsFile::parse("[[accounts.claude]]\nid=\"default\"\nname=\"Native default\"\n[[accounts.claude]]\nid=\"work\"\nname=\"Work\"\n[defaults]\naccount=\"claude/work\"\n").unwrap().resolve(None,&std::collections::BTreeMap::new()).unwrap(),issue:None,issue_text:None,attachments:vec![]};
    let original=f.task_node();let before=f.sdk().starts();
    assert!(f.domain.live.mods.session("agent-b").is_some(), "the fixture has an actual connected mod session");
    let error=launch(&f.domain,&f.home,launch_body(&f,json!({"replace":true,"model":"sonnet"})),|_|Some(choices)).await.unwrap_err();
    assert_eq!(error.code,"active_account_immutable");assert_eq!(f.task_node(),original);
    assert!(f.sessions.stopped.lock().unwrap().is_empty(),"wrong-account replacement is refused before stopping the legacy native session");
    assert_eq!(f.sdk().starts(),before);assert!(f.domain.live.mods.session("agent-b").is_some());
    assert!(!f.domain.live.is_closed("agent-b"));
    assert!(f.domain.state().unwrap().agents[&AgentId::from("agent-b")].phase.live());
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].account_key().account,AccountId::default_account());
}

#[tokio::test]
async fn changing_the_recovery_directory_refuses_before_workspace_effects_or_stopping_the_original_sdk_node() {
    let f = world();
    let old = sdk_node_on_the_task(&f, &f.project).await;
    let original = f.domain.task_worktree(&"t".into()).unwrap();
    for (name, repository) in [("Other", false), ("OtherRepository", true)] {
        let directory = f.root.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        if repository {
            git_in(&directory, &["init", "-q", "-b", "main"]);
            git_in(&directory, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "start"]);
        }
        let trust = f.root.join("trust.json");
        let bytes = json!({"projects": {directory.to_str().unwrap(): {"hasTrustDialogAccepted": true}}}).to_string();
        std::fs::write(&trust, &bytes).unwrap();
        crate::claude_trust::use_config_for_test(&trust);
        let refused = launch(&f.domain, &f.home, launch_body(&f, json!({"replace": true, "model": "sonnet", "cwd": directory.to_str().unwrap()})), |_| None).await.unwrap_err();
        assert_eq!(refused.code, "native_session_required");
        assert_eq!(f.task_node(), Some(old.clone()));
        assert!(f.domain.owns_sdk_host(&old));
        assert_eq!(f.sdk().starts().len(), 1);
        assert_eq!(f.domain.task_worktree(&"t".into()).unwrap(), original);
        assert!(!directory.join(".claude/worktrees/t").exists());
        assert_eq!(std::fs::read_to_string(&trust).unwrap(), bytes);
    }
}

/// Claude and Codex connected, both signed in, each advertising its models; `twin` is in both catalogs.
fn connect_claude_and_codex(f: &Fixture) {
    use alondra_core::accounts::{AccountBilling, AccountKey, AccountState, AccountStatus, AccountsConfig, Provider};
    use alondra_core::model_selection::{AccountModelCatalog, CatalogModel};
    let mut config = AccountsConfig::fresh();
    config.connect(Provider::Claude);
    config.connect(Provider::Codex);
    let keys = vec![AccountKey::native(Provider::Claude), AccountKey::native(Provider::Codex)];
    config.set_order(keys.clone()).unwrap();
    std::fs::write(alondra_core::options::path_in(&f.home), config.to_toml()).unwrap();
    let statuses = keys.iter().map(|key| AccountStatus { provider: key.provider, id: key.account.clone(), name: "Personal".into(), state: AccountState::SignedIn,
        native_identity: None, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0, profile: None }).collect();
    command(&f.domain, Actor::System, Command::RecordAccounts { accounts: statuses, reserve: alondra_core::usage::Share::of(config.distribution.reserve) });
    command(&f.domain, Actor::System, Command::RecordAccountConfiguration { order: keys.clone(), version: alondra_core::accounts::config_version(&config) });
    let model = |name: &str| CatalogModel { model: name.into(), display_name: name.into(), reasoning_efforts: vec!["low".into(), "medium".into(), "high".into()],
        default_reasoning_effort: Some("medium".into()), context_window: None, input_modalities: vec!["text".into()], is_default: false, hidden: false };
    for key in keys {
        let models = if key.provider == Provider::Claude { vec![model("opus"), model("sonnet"), model("twin")] } else { vec![model("gpt-6"), model("twin")] };
        command(&f.domain, Actor::System, Command::RecordModelCatalog { catalog: AccountModelCatalog { account: key, revision: "fixture".into(), native_default: None, measured_at: 100, models } });
    }
}

#[test]
fn an_unstarted_selection_replaces_a_disabled_provider() {
    use alondra_core::accounts::{AccountKey, Provider};
    use alondra_core::model_selection::{ModelProfile, TaskSelection};
    let f = bare_world();
    connect_claude_and_codex(&f);
    for mut catalog in f.domain.state().unwrap().model_catalogs.clone() {
        catalog.native_default = Some(catalog.models[0].model.clone());
        command(&f.domain, Actor::System, Command::RecordModelCatalog { catalog });
    }
    let file = crate::domain_schedule_runner::options_at(&f.home).unwrap();
    let requested = file.orchestrator_defaults(None, None);
    let mut task = f.domain.state().unwrap().tasks[&TaskId::from("t")].clone();
    let mut intent = TaskSelection::automatic(ModelProfile::Implementation);
    let chosen = crate::accounts::select_for_node(&f.domain, &f.home, &intent, None, Some(Provider::Codex), 100).unwrap();
    task.provider = Provider::Codex;
    intent.effective = Some(chosen.selection);
    task.selection = Some(intent);
    std::fs::write(alondra_core::options::path_in(&f.home), "[accounts]\nconnected = [\"claude\"]\n").unwrap();
    let file = crate::domain_schedule_runner::options_at(&f.home).unwrap();
    let body = launch_body(&f, json!({"model": "codex/gpt-6", "provider": "codex"}));
    let (_, account, _) = native_launch(&f.domain, &f.home, &task, &requested, &file, &body, false, false)
        .expect("a task that never started must reselect an active provider").unwrap();
    assert_eq!(account, AccountKey::native(Provider::Claude));
}

#[test]
fn an_unstarted_selection_replaces_a_disabled_legacy_provider() {
    use alondra_core::accounts::{AccountKey, Provider};
    let f = bare_world();
    connect_claude_and_codex(&f);
    let mut catalog = f.domain.state().unwrap().model_catalogs.iter().find(|catalog| catalog.account.provider == Provider::Codex).unwrap().clone();
    catalog.native_default = Some("gpt-6".into());
    let mut collision = catalog.models[0].clone();
    collision.model = "sonnet".into();
    catalog.models.push(collision);
    command(&f.domain, Actor::System, Command::RecordModelCatalog { catalog });
    let mut requested = OptionsFile::default().resolve(None, &std::collections::BTreeMap::new()).unwrap();
    requested.picked.push(alondra_core::options::Picked {
        id: alondra_core::options::ACCOUNT.into(), label: "Account".into(),
        choice: alondra_core::options::Choice::Value("default".into()), value_label: "Default".into(), mission: None,
    });
    assert!(requested.selection.is_none(), "stored legacy requests have no modern selection");
    assert_eq!(requested.account_key(), Some(AccountKey::native(Provider::Claude)));
    std::fs::write(alondra_core::options::path_in(&f.home), "[accounts]\nconnected = [\"codex\"]\n").unwrap();
    let file = crate::domain_schedule_runner::options_at(&f.home).unwrap();
    let mut task = f.domain.state().unwrap().tasks[&TaskId::from("t")].clone();
    let body = launch_body(&f, json!({"model": "sonnet"}));
    let (resolved, account, _) = native_launch(&f.domain, &f.home, &task, &requested, &file, &body, false, false).unwrap()
        .expect("an unstarted legacy request must resolve against active providers");
    assert_eq!(account, AccountKey::native(Provider::Codex));
    assert!(native_launch(&f.domain, &f.home, &task, &requested, &file, &body, true, false).unwrap().is_none(), "a started legacy session stays on its original launch path");
    task.provider = Provider::Codex;
    task.account = Some(account.account.clone());
    task.selection = resolved.selection;
    task.started = true;
    let recovery = launch_body(&f, json!({}));
    let (recovered, account, _) = native_launch(&f.domain, &f.home, &task, &requested, &file, &recovery, true, false).unwrap().unwrap();
    assert_eq!(account, AccountKey::native(Provider::Codex));
    assert_eq!(recovered.selection.unwrap().effective.as_ref().unwrap().model.model, "gpt-6");
}

#[tokio::test]
async fn an_unstarted_selection_replaces_a_disabled_account_but_a_started_one_cannot_move() {
    use alondra_core::accounts::{AccountId, AccountKey, Provider};
    use alondra_core::model_selection::{ModelKey, ModelProfile, ModelSelection, TaskSelection};
    let f = world();
    connect_claude_and_codex(&f);
    let mut catalog = f.domain.state().unwrap().model_catalogs.iter().find(|catalog| catalog.account.provider == Provider::Claude).unwrap().clone();
    with_work_account(&f);
    let work = AccountKey::new(Provider::Claude, AccountId::parse("work").unwrap());
    catalog.account = work.clone();
    command(&f.domain, Actor::System, Command::RecordModelCatalog { catalog });
    std::fs::write(alondra_core::options::path_in(&f.home), "[accounts]\nconnected = [\"claude\"]\n[[accounts.claude]]\nid = \"default\"\nname = \"Default\"\nenabled = false\n[[accounts.claude]]\nid = \"work\"\nname = \"Work\"\n").unwrap();
    let file = crate::domain_schedule_runner::options_at(&f.home).unwrap();
    command(&f.domain, Actor::System, Command::RecordAccountConfiguration { order: vec![AccountKey::native(Provider::Claude), work.clone()], version: alondra_core::accounts::config_version(&file.accounts) });
    let requested = file.orchestrator_defaults(None, None);
    let mut task = f.domain.state().unwrap().tasks[&TaskId::from("t")].clone();
    let mut intent = TaskSelection::automatic(ModelProfile::Implementation);
    intent.account = Some(AccountKey::native(Provider::Claude));
    intent.requested = Some(ModelKey::parse_wire("claude/sonnet").unwrap());
    intent.allow_model_substitution = false;
    intent.effective = Some(ModelSelection { model: intent.requested.clone().unwrap(), reasoning_effort: Some("medium".into()), catalog_revision: "fixture".into() });
    task.selection = Some(intent.clone());
    command(&f.domain, Actor::System, Command::RecordTaskSelection { task_id: "t".into(), selection: intent, pending: None });
    let body = launch_body(&f, json!({"model": "claude/sonnet"}));
    let (resolved, account, _) = native_launch(&f.domain, &f.home, &task, &requested, &file, &body, false, false)
        .expect("a task that never started must reselect an active account").unwrap();
    assert_eq!(account, work);
    assert_eq!(resolved.selection.unwrap().requested, task.selection.as_ref().unwrap().requested, "a compatible model stays binding");
    task.started = true;
    assert!(native_launch(&f.domain, &f.home, &task, &requested, &file, &body, true, false).is_err(), "an existing session never migrates");
    f.domain.live.machine_watch.set_disk_low(true);
    let queued = launch(&f.domain, &f.home, body, |_| None).await.unwrap();
    assert_eq!(queued["queued"], true);
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].selection.as_ref().unwrap().account, Some(work.clone()), "the move survives a refusal before admission");
    assert!(f.sdk().starts().is_empty());
    f.domain.live.machine_watch.set_disk_low(false);
    crate::admission::pass(&f.domain, &f.home).await.unwrap();
    f.launched(1).await;
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].account_key(), work, "the admitted execution keeps the reselected account");
}

#[tokio::test]
async fn an_unstarted_selection_waits_without_a_card_then_starts_on_an_active_provider() {
    use alondra_core::accounts::{AccountKey, Provider};
    use alondra_core::model_selection::{ModelKey, ModelProfile, TaskSelection};
    let f = world();
    connect_claude_and_codex(&f);
    for mut catalog in f.domain.state().unwrap().model_catalogs.clone() {
        catalog.native_default = Some(catalog.models[0].model.clone());
        command(&f.domain, Actor::System, Command::RecordModelCatalog { catalog });
    }
    let mut intent = TaskSelection::automatic(ModelProfile::Implementation);
    intent.account = Some(AccountKey::native(Provider::Codex));
    intent.requested = Some(ModelKey::parse_wire("codex/gpt-6").unwrap());
    intent.requested_reasoning_effort = Some("medium".into());
    intent.allow_model_substitution = false;
    command(&f.domain, Actor::System, Command::RecordTaskSelection { task_id: "t".into(), selection: intent.clone(), pending: None });
    let plan = alondra_daemon_engine_model::LaunchPlan { model: Some("codex/gpt-6".into()), effort: Some("medium".into()),
        ..alondra_daemon_engine_model::LaunchPlan::new("app/editor", f.project.to_str().unwrap()) };
    f.domain.store.lock().unwrap().set_launch_plan(&"t".into(), &plan, 1).unwrap();
    std::fs::write(alondra_core::options::path_in(&f.home), "[accounts]\nconnected = []\n").unwrap();
    let cards = f.domain.state().unwrap().interactions.len();
    for _ in 0..=alondra_core::domain::START_RETRIES.len() {
        crate::auto_launch::pass(&f.domain, &f.home).await.unwrap();
        let store = f.domain.store.lock().unwrap();
        assert!(store.start_retries().unwrap().is_empty(), "account availability is not a failed start");
        assert!(store.auto_launch(&"t".into()).unwrap().is_none(), "the next pass can try again");
        drop(store);
        assert_eq!(f.domain.live.gate.snapshot(&f.domain, alondra_core::now_millis()).unwrap().working, 0, "an unavailable account reserves no working slot");
    }
    assert_eq!(f.domain.state().unwrap().interactions.len(), cards);
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].selection.as_ref(), Some(&intent), "waiting preserves the selection until a destination is known");
    assert!(f.sdk().starts().is_empty());
    std::fs::write(alondra_core::options::path_in(&f.home), "[accounts]\nconnected = [\"claude\"]\n").unwrap();
    let mut catalog = f.domain.state().unwrap().model_catalogs.iter().find(|catalog| catalog.account.provider == Provider::Claude).unwrap().clone();
    let default = catalog.native_default.take();
    command(&f.domain, Actor::System, Command::RecordModelCatalog { catalog: catalog.clone() });
    crate::auto_launch::pass(&f.domain, &f.home).await.unwrap();
    assert!(f.domain.store.lock().unwrap().start_retries().unwrap().is_empty(), "a destination without a compatible model waits too");
    assert!(f.sdk().starts().is_empty());
    catalog.native_default = default;
    command(&f.domain, Actor::System, Command::RecordModelCatalog { catalog });
    crate::auto_launch::pass(&f.domain, &f.home).await.unwrap();
    f.launched(1).await;
    let state = f.domain.state().unwrap();
    let task = &state.tasks[&TaskId::from("t")];
    assert_eq!(task.account_key(), AccountKey::native(Provider::Claude));
    let selection = task.selection.as_ref().unwrap();
    assert_eq!(selection.account, Some(task.account_key()));
    assert_eq!(selection.effective.as_ref().unwrap().model.provider, Provider::Claude);
    assert!(selection.requested.is_none());
    assert_eq!(state.interactions.len(), cards, "no intervention is needed to reselect");
}

#[test]
fn an_unstarted_selection_does_not_transfer_payment_authorization_to_another_account() {
    use alondra_core::accounts::{AccountBilling, AccountKey, Provider};
    use alondra_core::model_selection::{ModelProfile, TaskSelection};
    let f = bare_world();
    connect_claude_and_codex(&f);
    let mut accounts = f.domain.state().unwrap().accounts.clone();
    for account in &mut accounts { account.billing = AccountBilling::PayPerUse; }
    command(&f.domain, Actor::System, Command::RecordAccounts { accounts, reserve: alondra_core::usage::default_reserve() });
    std::fs::write(alondra_core::options::path_in(&f.home), "[accounts]\nconnected = [\"codex\"]\n[[accounts.codex]]\nid = \"default\"\nname = \"Default\"\nbilling = { kind = \"pay_per_use\" }\n").unwrap();
    let file = crate::domain_schedule_runner::options_at(&f.home).unwrap();
    let requested = file.orchestrator_defaults(None, None);
    let mut task = f.domain.state().unwrap().tasks[&TaskId::from("t")].clone();
    let mut intent = TaskSelection::automatic(ModelProfile::Implementation);
    intent.account = Some(AccountKey::native(Provider::Claude));
    intent.paid_account_grant = intent.account.clone();
    task.selection = Some(intent);
    let body = launch_body(&f, json!({"model": "claude/opus"}));
    let error = native_launch(&f.domain, &f.home, &task, &requested, &file, &body, false, false).unwrap_err();
    assert_eq!(error.code, "account_unavailable", "a grant for the disabled account cannot authorize Codex");
}

#[tokio::test]
async fn an_unstarted_selection_keeps_the_fallback_account_when_the_original_request_is_reused() {
    use alondra_core::accounts::{AccountId, AccountKey, Provider};
    use alondra_core::model_selection::{ModelProfile, TaskSelection};
    for preselected in [false, true] {
        let f = world();
        connect_claude_and_codex(&f);
        let mut catalog = f.domain.state().unwrap().model_catalogs.iter().find(|catalog| catalog.account.provider == Provider::Claude).unwrap().clone();
        let opus = catalog.models.iter().find(|model| model.model == "opus").unwrap().clone();
        with_work_account(&f);
        let work = AccountKey::new(Provider::Claude, AccountId::parse("work").unwrap());
        catalog.account = work.clone();
        catalog.native_default = Some("sonnet".into());
        catalog.models.retain(|model| model.model == "sonnet");
        command(&f.domain, Actor::System, Command::RecordModelCatalog { catalog: catalog.clone() });
        let mut intent = TaskSelection::automatic(ModelProfile::Implementation);
        intent.account = Some(AccountKey::native(Provider::Claude));
        if preselected {
            command(&f.domain, Actor::System, Command::RecordTaskSelection { task_id: "t".into(), selection: intent.clone(), pending: None });
        }
        let mut requested = crate::domain_schedule_runner::options_at(&f.home).unwrap().orchestrator_defaults(None, None);
        requested.selection = Some(intent);
        let choices = Choices { project: None, options: requested, issue: None, issue_text: None, attachments: vec![] };
        std::fs::write(alondra_core::options::path_in(&f.home), "[accounts]\nconnected = [\"claude\"]\n[[accounts.claude]]\nid = \"default\"\nname = \"Default\"\nenabled = false\n[[accounts.claude]]\nid = \"work\"\nname = \"Work\"\n").unwrap();
        let file = crate::domain_schedule_runner::options_at(&f.home).unwrap();
        command(&f.domain, Actor::System, Command::RecordAccountConfiguration { order: vec![AccountKey::native(Provider::Claude), work.clone()], version: alondra_core::accounts::config_version(&file.accounts) });
        let plan = alondra_daemon_engine_model::LaunchPlan { model: Some("opus".into()), ..alondra_daemon_engine_model::LaunchPlan::new("app/editor", f.project.to_str().unwrap()) };
        f.domain.store.lock().unwrap().set_launch_plan(&"t".into(), &plan, 1).unwrap();
        launch(&f.domain, &f.home, launch_body(&f, json!({})), |_| Some(choices.clone())).await.unwrap();
        f.launched(1).await;
        f.sdk().until_answered("t").await;
        let original = f.sdk().starts()[0]["session"].as_str().unwrap().to_owned();
        assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].selection.as_ref().unwrap().account, Some(work.clone()), "the fallback account is durable even without a previous task selection");
        assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].selection.as_ref().unwrap().effective.as_ref().unwrap().model.model, "sonnet");
        catalog.models.push(opus);
        catalog.revision = "more-models".into();
        command(&f.domain, Actor::System, Command::RecordModelCatalog { catalog });
        launch(&f.domain, &f.home, launch_body(&f, json!({"replace": true})), |_| Some(choices.clone())).await.unwrap();
        f.launched(2).await;
        assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].account_key(), work);
        assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].selection.as_ref().unwrap().effective.as_ref().unwrap().model.model, "sonnet", "a stale launch plan cannot replace the retained model during recovery");
        assert_eq!(f.sdk().starts()[1]["resume"].as_str(), Some(original.as_str()), "the request's old pin cannot change the recovered native session");
    }
}

#[tokio::test]
async fn a_plan_naming_opus_without_provider_starts_on_claude_when_claude_and_codex_are_connected() {
    let f = world();
    connect_claude_and_codex(&f);
    let plan = alondra_daemon_engine_model::LaunchPlan { model: Some("opus".into()), effort: Some("medium".into()), reason: Some("Design work".into()),
        ..alondra_daemon_engine_model::LaunchPlan::new("app/editor", f.project.to_str().unwrap()) };
    f.domain.store.lock().unwrap().set_launch_plan(&"t".into(), &plan, 1).unwrap();
    let body: LaunchBody = serde_json::from_value(json!({"task": "t"})).unwrap();
    launch(&f.domain, &f.home, body, |_| None).await.unwrap();
    f.launched(1).await;
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].provider, alondra_core::accounts::Provider::Claude);
}

#[tokio::test]
async fn a_model_in_two_catalogs_is_refused_when_the_plan_is_written_and_a_unique_one_is_not() {
    let f = world();
    connect_claude_and_codex(&f);
    let plan = |model: &str| alondra_daemon_engine_model::LaunchPlan { model: Some(model.into()), reason: Some("Design work".into()),
        ..alondra_daemon_engine_model::LaunchPlan::new("app/editor", f.project.to_str().unwrap()) };
    let request = RequestId::from("r");
    for model in ["opus", "gpt-6", "claude/twin", "codex/twin"] {
        crate::launch_plans::validate(&f.domain, &f.home, &request, &plan(model)).unwrap_or_else(|error| panic!("{model}: {}", error.message));
    }
    let refused = crate::launch_plans::validate(&f.domain, &f.home, &request, &plan("twin")).unwrap_err();
    assert_eq!((refused.code.as_str(), refused.message.as_str()), ("invalid_input", "“twin” is offered by claude and codex: name it claude/twin or codex/twin"));
    let body: LaunchPlanBody = serde_json::from_value(json!({"task_id": "t", "launch": plan("twin")})).unwrap();
    assert_eq!(set_launch_plan(&f.domain, &f.home, body).unwrap_err().message, refused.message);
}

/// What kept “Triage legible en AX3” from ever starting on 2026-11-10: its plan held a native effort
/// with the model on Automatic, every write took it and every start refused it.
#[tokio::test]
async fn a_plan_with_a_native_effort_and_no_model_is_refused_when_it_is_written() {
    let f = world();
    connect_claude_and_codex(&f);
    let plan = alondra_daemon_engine_model::LaunchPlan { effort: Some("medium".into()), reason: Some("Layout fix".into()),
        ..alondra_daemon_engine_model::LaunchPlan::new("app/editor", f.project.to_str().unwrap()) };
    let refused = crate::launch_plans::validate(&f.domain, &f.home, &"r".into(), &plan).unwrap_err();
    assert_eq!((refused.code.as_str(), refused.message.as_str()), ("invalid_input", crate::launch_plans::EFFORT_WITHOUT_MODEL));
    let body: LaunchPlanBody = serde_json::from_value(json!({"task_id": "t", "launch": plan})).unwrap();
    assert_eq!(set_launch_plan(&f.domain, &f.home, body).unwrap_err().message, refused.message);
    assert_eq!(partial_plan(&f, "t", json!({"effort": "medium"})).unwrap_err().message, refused.message);
    let named = alondra_daemon_engine_model::LaunchPlan { model: Some("claude/sonnet".into()), ..plan };
    crate::launch_plans::validate(&f.domain, &f.home, &"r".into(), &named).unwrap();
}

/// A plan an older daemon stored as the one that stopped “Triage legible en AX3”. Its start is
/// refused the same way every time, so it is never retried. The orchestrator gets it at once, the
/// user's inbox gets nothing, and the plan the orchestrator fixes starts the task.
fn refused_plan(f: &Fixture) {
    connect_claude_and_codex(f);
    let stored = alondra_daemon_engine_model::LaunchPlan { effort: Some("medium".into()), reason: Some("Layout fix".into()),
        ..alondra_daemon_engine_model::LaunchPlan::new("app/editor", f.project.to_str().unwrap()) };
    f.domain.store.lock().unwrap().set_launch_plan(&"t".into(), &stored, 1).unwrap();
}

async fn refuse_its_start(f: &Fixture) -> alondra_core::domain::Interaction {
    crate::auto_launch::pass(&f.domain, &f.home).await.unwrap();
    assert!(f.sdk().starts().is_empty());
    assert!(f.domain.store.lock().unwrap().start_retries().unwrap().is_empty(), "no retry is promised for a start that cannot work");
    assert_eq!(f.domain.live.gate.snapshot(&f.domain, alondra_core::now_millis()).unwrap().in_flight, 0,
        "a start that did not start holds no slot, so the next try asks the gate again");
    let state = f.domain.state().unwrap();
    let card = state.open_plan_refusal(&"t".into()).cloned().expect("the orchestrator is handed the refused plan at once");
    assert_eq!(card.recipient, alondra_core::domain::Recipient::Orchestrator);
    assert!(card.body.contains("Choose a model before its native reasoning effort"), "{}", card.body);
    assert!(state.interactions.values().all(|i| i.recipient != alondra_core::domain::Recipient::User
        || !matches!(&i.scope, alondra_core::domain::Scope::Task { task_id } if task_id.0 == "t")), "nothing reaches the user's inbox");
    let messages = f.domain.store.lock().unwrap().pending_messages().unwrap();
    assert!(messages.iter().any(|m| m.recipient_kind == "orchestrator" && m.interaction_id.as_deref() == Some(card.id.0.as_str())),
        "the orchestrator hears of it in its next batch: {messages:?}");
    assert!(messages.iter().filter(|m| m.interaction_id.as_deref() == Some(card.id.0.as_str())).all(|m| m.recipient_kind == "orchestrator"), "{messages:?}");
    card
}

#[tokio::test]
async fn a_start_refused_for_its_plan_goes_to_the_orchestrator_and_fixing_the_plan_starts_it() {
    let f = world();
    refused_plan(&f);
    refuse_its_start(&f).await;
    partial_plan(&f, "t", json!({"model": "claude/sonnet"})).unwrap();
    assert!(f.domain.state().unwrap().open_plan_refusal(&"t".into()).is_none(), "the fixed plan settles the card");
    crate::auto_launch::pass(&f.domain, &f.home).await.unwrap();
    f.launched(1).await;
}

#[tokio::test]
async fn the_orchestrator_cancels_a_refused_plan_with_its_reason() {
    let f = world();
    refused_plan(&f);
    let card = refuse_its_start(&f).await;
    command(&f.domain, the_orchestrator(&f.domain), Command::AnswerQuestion { attachments: vec![], interaction_id: card.id,
        option_id: alondra_core::domain::START_CANCEL.into(),
        inputs: [(alondra_core::domain::ANSWER_TEXT.to_owned(), alondra_core::domain::InputValue::Text("The editor no longer needs it".into()))].into() });
    crate::auto_launch::pass(&f.domain, &f.home).await.unwrap();
    assert!(f.domain.state().unwrap().tasks[&TaskId::from("t")].cancelled());
    assert_eq!(f.domain.store.lock().unwrap().cancel_reasons().unwrap()[&TaskId::from("t")], "The editor no longer needs it");
}

#[tokio::test]
async fn a_refused_plan_the_orchestrator_leaves_is_reminded_once_then_cancelled_saying_why() {
    let f = world();
    refused_plan(&f);
    refuse_its_start(&f).await;
    crate::orchestrator_inbox::settle(&f.domain);
    let state = f.domain.state().unwrap();
    let reminder = state.interactions.values().find(|i| i.id.0.starts_with(alondra_core::orchestrator_cards::PLAN_REFUSED_NOTICE_PREFIX))
        .expect("one reminder to the orchestrator");
    assert_eq!(reminder.recipient, alondra_core::domain::Recipient::Orchestrator);
    assert!(!state.tasks[&TaskId::from("t")].cancelled());
    crate::orchestrator_inbox::settle(&f.domain);
    let state = f.domain.state().unwrap();
    assert!(state.tasks[&TaskId::from("t")].cancelled(), "left past its reminder, Alondra cancels it");
    assert!(state.open_plan_refusal(&"t".into()).is_none());
    let reason = f.domain.store.lock().unwrap().cancel_reasons().unwrap()[&TaskId::from("t")].clone();
    assert!(reason.contains("Choose a model before its native reasoning effort"), "{reason}");
    assert!(state.interactions.values().all(|i| i.recipient != alondra_core::domain::Recipient::User
        || !matches!(&i.scope, alondra_core::domain::Scope::Task { task_id } if task_id.0 == "t")), "the user is never asked");
}

fn partial_plan(f: &Fixture, task: &str, changes: Value) -> Result<Value, DomainError> {
    update_launch_plan(&f.domain, &f.home, serde_json::from_value(json!({"task_id": task, "changes": changes})).unwrap())
}


fn cancelled_launch_plan_with_history(f: &Fixture) -> alondra_daemon_engine_model::LaunchPlan {
    connect_claude_and_codex(f);
    let before = stored_selection(f);
    give(f, "t", "agent-b");
    command(&f.domain, Actor::Agent { agent_id: "agent-b".into() }, Command::StartTask { task_id: "t".into() });
    record(&f.domain, Decision {
        task_id: "t".into(), request_id: "r".into(), agent_id: Some("agent-b".into()), at: alondra_core::now_millis(),
        kind: DecisionKind::Launch, model: Some("sonnet".into()), effort: Some("medium".into()), approach: None,
        chosen_by: ModelChooser::Orchestrator, reason: Some("Original launch".into()), title: "Editor colours".into(),
        provider: Some(alondra_core::accounts::Provider::Claude), tags: None,
    });
    command(&f.domain, user(), Command::CancelTask { task_id: "t".into() });
    command(&f.domain, Actor::System, Command::RecordNodeEnd {
        task_id: "t".into(), end: alondra_core::domain::NodeEnd::Closed, agent_id: Some("agent-b".into()),
    });
    assert!(f.domain.store.lock().unwrap().task_was_launched(&"t".into()).unwrap());
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].state, TaskState::Cancelled);
    before
}

#[tokio::test]
async fn reopening_a_legacy_task_admits_its_retained_project_policy_after_the_projects_root_changes() {
    let f = world();
    main_checkout(&f);
    let original = f.project.join(".claude/worktrees/t");
    git_in(&f.project, &["worktree", "add", "-q", "-b", "worktree-t", original.to_str().unwrap()]);
    let options = alondra_core::options::path_in(&f.home);
    std::fs::write(&options, format!("projects_root = {:?}\n[projects.App]\ndefaults = {{ effort = \"hard\" }}\n", f.root.to_str().unwrap())).unwrap();
    let saved = alondra_daemon_engine_model::LaunchPlan {
        project: Some("App".into()), mission: Some("Preserve the approved editor colours and contrast.".into()),
        ..alondra_daemon_engine_model::LaunchPlan::new("app/editor", original.to_str().unwrap())
    };
    crate::launch_plans::validate(&f.domain, &f.home, &"r".into(), &saved).unwrap();
    f.domain.store.lock().unwrap().set_launch_plan(&"t".into(), &saved, 1).unwrap();
    f.domain.set_task_worktree(&"t".into(), original.to_str().unwrap()).unwrap();
    give(&f, "t", "agent-b");
    command(&f.domain, Actor::Agent { agent_id: "agent-b".into() }, Command::StartTask { task_id: "t".into() });
    command(&f.domain, user(), Command::CancelTask { task_id: "t".into() });
    git_in(&f.project, &["worktree", "remove", original.to_str().unwrap()]);
    command(&f.domain, Actor::System, Command::ReleaseTaskWorktree {
        task_id: "t".into(), path: original.display().to_string(), end: alondra_core::domain::WorktreeEnd::Removed,
    });
    command(&f.domain, Actor::System, Command::RecordNodeEnd {
        task_id: "t".into(), end: alondra_core::domain::NodeEnd::Closed, agent_id: Some("agent-b".into()),
    });
    assert!(!original.exists());
    assert!(f.domain.state().unwrap().tasks[&TaskId::from("t")].selection.is_none(), "the previous release used legacy effort options");
    assert!(request_choices(&f.domain, &"r".into()).unwrap().is_none());

    std::fs::write(&options, format!("projects_root = {:?}\n[projects.App]\ndefaults = {{ effort = \"hard\" }}\n", f.root.join("new-projects").to_str().unwrap())).unwrap();
    let file = crate::domain_schedule_runner::options_at(&f.home).unwrap();
    assert!(crate::options::project_of(&file, &original).is_none(), "the old folder is outside the newly configured projects root");
    assert_eq!(file.orchestrator_defaults(None, None).effort, alondra_core::options::AUTO);
    partial_plan(&f, "t", json!({"reason": "Continue the approved work with its original project policy"})).unwrap();
    command(&f.domain, the_orchestrator(&f.domain), Command::ReopenTask { task_id: "t".into() });
    assert!(f.domain.state().unwrap().pending_reopening(&"t".into()).is_some());
    crate::admission::pass_at(&f.domain, &f.home, alondra_core::now_millis()).await.unwrap();
    assert!(f.task_node().is_some(), "admission must use App's hard policy instead of rejecting a missing model under global auto");
    let (folders, prompts) = f.launched(1).await;
    assert_eq!(f.sdk().starts().len(), 1);
    assert!(same(&folders[0], &original), "the managed worktree is reconstructed before the native launch: {folders:?}");
    assert_eq!(f.sdk().flag("--model").as_deref(), Some("opus"));
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].model,
        Some(TaskModel { model: "opus".into(), chosen_by: ModelChooser::User }));
    assert!(prompts[0].contains(saved.mission.as_deref().unwrap()));
    let store = f.domain.store.lock().unwrap();
    assert!(store.launch_queue().unwrap().is_empty());
    assert!(store.start_retries().unwrap().is_empty());
}

#[tokio::test]
async fn terminal_launch_choices_can_be_prepared_without_enqueuing_or_starting_the_task() {
    let f = world();
    let before = cancelled_launch_plan_with_history(&f);
    let state = f.domain.state().unwrap();
    assert!(state.tasks[&TaskId::from("t")].started, "a closed task keeps its start as history");
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id, Some("agent-b".into()));
    partial_plan(&f, "t", json!({"model": "codex/gpt-6", "reasoning_effort": "high", "reason": "Use Codex for the approved implementation"})).unwrap();
    assert_eq!(*f.domain.state().unwrap(), *state, "preparing choices does not reopen or rewrite the task and its approvals");
    let prepared = f.domain.launch_plan(&"t".into()).unwrap().unwrap();
    assert_eq!(prepared, alondra_daemon_engine_model::LaunchPlan { model: Some("codex/gpt-6".into()), effort: Some("high".into()),
        reason: Some("Use Codex for the approved implementation".into()), ..before });
    crate::admission::pass_at(&f.domain, &f.home, alondra_core::now_millis()).await.unwrap();
    crate::auto_launch::pass(&f.domain, &f.home).await.unwrap();
    assert!(f.sdk().starts().is_empty(), "admission between preparation and reopening cannot start a terminal task");
    {
        let store = f.domain.store.lock().unwrap();
        assert!(store.launch_queue().unwrap().is_empty());
        assert!(store.start_retries().unwrap().is_empty());
        assert!(store.task_was_launched(&"t".into()).unwrap(), "launch history is never erased to edit choices");
    }
    command(&f.domain, the_orchestrator(&f.domain), Command::ReopenTask { task_id: "t".into() });
    assert_eq!(f.domain.store.lock().unwrap().launch_queue().unwrap()[0].body["body"]["model"], "codex/gpt-6");
    partial_plan(&f, "t", json!({"reason": "The reopened task keeps the prepared provider"})).unwrap();
    assert_eq!(f.domain.launch_plan(&"t".into()).unwrap().unwrap().model.as_deref(), Some("codex/gpt-6"),
        "historical launch decisions do not block an edit before this reopening's first admission");
    let pending = f.domain.launch_plan(&"t".into()).unwrap().unwrap();
    for changes in [json!({"tags": {"kind": "feature"}}), json!({"approach": "subagents"})] {
        assert_eq!(partial_plan(&f, "t", changes).unwrap_err().code, "invalid_input");
    }
    let replacement = serde_json::from_value(json!({"task_id": "t", "launch": pending})).unwrap();
    assert_eq!(set_launch_plan(&f.domain, &f.home, replacement).unwrap_err().code, "invalid_input");
    assert_eq!(f.domain.launch_plan(&"t".into()).unwrap().unwrap(), pending,
        "a pending reopening preserves its mission, directory, name and approved scope too");
}

#[test]
fn preparing_terminal_choices_preserves_scope_and_requires_an_open_request_and_an_ended_node() {
    let f = world();
    let before = cancelled_launch_plan_with_history(&f);
    for changes in [json!({"tags": {"kind": "feature"}}), json!({"approach": "subagents"})] {
        assert_eq!(partial_plan(&f, "t", changes).unwrap_err().code, "invalid_input");
    }
    let replacement = serde_json::from_value(json!({"task_id": "t", "launch": before})).unwrap();
    assert_eq!(set_launch_plan(&f.domain, &f.home, replacement).unwrap_err().code, "invalid_input");
    assert_eq!(f.domain.launch_plan(&"t".into()).unwrap().unwrap(), before);
    let mut state = (*f.domain.state().unwrap()).clone();
    for end in [alondra_core::domain::NodeEnd::KeptByUser, alondra_core::domain::NodeEnd::UsersOwn] {
        for agent in [Some(AgentId::from("agent-b")), None] {
            let task = state.tasks.get_mut(&TaskId::from("t")).unwrap();
            task.wrap_up.node = Some(end);
            task.agent_id = agent;
            assert!(editable_plan(&state, &"t".into(), true).is_err(), "a retained live node prevents preparation, including legacy data without its agent id");
        }
    }
    state.tasks.get_mut(&TaskId::from("t")).unwrap().wrap_up.node = Some(alondra_core::domain::NodeEnd::Gone);
    assert!(editable_plan(&state, &"t".into(), true).is_ok());
    state.tasks.get_mut(&TaskId::from("t")).unwrap().archived = true;
    assert!(editable_plan(&state, &"t".into(), true).is_err(), "preparation preserves the archived-task guard");
    state.tasks.get_mut(&TaskId::from("t")).unwrap().archived = false;
    state.requests.get_mut(&RequestId::from("r")).unwrap().closed = true;
    assert!(editable_plan(&state, &"t".into(), true).is_err(), "a closed request cannot reopen its task");
}

#[tokio::test]
async fn preparing_terminal_choices_preserves_a_deleted_launch_directory_without_recreating_it() {
    let f = world();
    let before = cancelled_launch_plan_with_history(&f);
    std::fs::remove_dir(&f.project).unwrap();
    partial_plan(&f, "t", json!({"model": "codex/gpt-6", "reasoning_effort": "high"})).unwrap();
    let after = f.domain.launch_plan(&"t".into()).unwrap().unwrap();
    assert_eq!(after, alondra_daemon_engine_model::LaunchPlan { model: Some("codex/gpt-6".into()), effort: Some("high".into()), ..before });
    crate::admission::pass_at(&f.domain, &f.home, alondra_core::now_millis()).await.unwrap();
    assert!(!f.project.exists(), "only the reopened admission reconstructs its workspace");
    assert!(f.sdk().starts().is_empty());
    let store = f.domain.store.lock().unwrap();
    assert!(store.launch_queue().unwrap().is_empty());
    assert!(store.start_retries().unwrap().is_empty());
}

fn stored_selection(f: &Fixture) -> alondra_daemon_engine_model::LaunchPlan {
    let plan = alondra_daemon_engine_model::LaunchPlan {
        mission: Some("Keep the full mission\n\nAdded context: preserve colours.".into()),
        model: Some("claude/sonnet".into()), effort: Some("medium".into()), reason: Some("Implementation".into()),
        role: Some(alondra_core::domain::TaskRole::Implementation), project: Some("App".into()),
        ..alondra_daemon_engine_model::LaunchPlan::new("app/editor", f.project.to_str().unwrap())
    };
    f.domain.store.lock().unwrap().set_launch_plan(&"t".into(), &plan, 1).unwrap();
    plan
}

#[test]
fn partial_launch_plan_changes_only_the_model_and_preserves_queued_context() {
    let f = world();
    connect_claude_and_codex(&f);
    let before = stored_selection(&f);
    let mut queued = LaunchBody::from_plan("t".into(), &before).to_queue();
    queued["body"]["prompt"] = json!("Extra queued context that must survive.");
    queued["body"]["provider"] = json!("claude");
    f.domain.store.lock().unwrap().enqueue_launch(&"t".into(), &queued, "orchestrator", 2).unwrap();
    let before_state = f.domain.state().unwrap();
    assert_eq!(partial_plan(&f, "t", json!({"model": "codex/gpt-6"})).unwrap(), json!({"task": "t", "stored": true}));
    let after = f.domain.launch_plan(&"t".into()).unwrap().unwrap();
    assert_eq!(after, alondra_daemon_engine_model::LaunchPlan { model: Some("codex/gpt-6".into()), ..before });
    assert_eq!(*f.domain.state().unwrap(), *before_state, "the task, checklist and lifecycle context are untouched");
    let store = f.domain.store.lock().unwrap();
    let queue = store.launch_queue().unwrap();
    assert_eq!(queue[0].body["body"]["model"], "codex/gpt-6");
    assert!(queue[0].body["body"].get("provider").is_none(), "the old queue provider must not override the new model");
    assert_eq!(queue[0].body["body"]["prompt"], "Extra queued context that must survive.");
    assert_eq!(queue[0].enqueued_at, 2);
    assert_eq!(store.planned_tasks().unwrap()[&TaskId::from("t")], 1, "keep its place");
}

#[test]
fn partial_launch_plan_reads_the_complete_stored_mission_without_writing() {
    let f = world();
    let mut before = stored_selection(&f);
    before.mission = Some("Saved context.\n".repeat(2_000));
    f.domain.store.lock().unwrap().set_launch_plan(&"t".into(), &before, 1).unwrap();
    let mark = f.domain.store.lock().unwrap().write_mark().unwrap();
    assert_eq!(get_launch_plan(&f.domain, &"t".into()).unwrap(), json!({"task": "t", "launch": before}));
    assert_eq!(f.domain.store.lock().unwrap().write_mark().unwrap(), mark);
    assert_eq!(get_launch_plan(&f.domain, &"missing".into()).unwrap_err().code, "not_found");
    f.domain.store.lock().unwrap().forget_launch_plan(&"t".into()).unwrap();
    assert_eq!(get_launch_plan(&f.domain, &"t".into()).unwrap_err().code, "not_found");
}

#[test]
fn partial_launch_plan_validates_the_merged_selection_and_changes_nothing_on_refusal() {
    let f = world();
    connect_claude_and_codex(&f);
    let before = stored_selection(&f);
    for changes in [json!({"model": "codex/unknown"}), json!({"model": "codex/gpt-6", "effort": "impossible"}), json!({})] {
        assert!(partial_plan(&f, "t", changes).is_err());
        assert_eq!(f.domain.launch_plan(&"t".into()).unwrap().unwrap(), before);
    }
    partial_plan(&f, "t", json!({"model": "codex/gpt-6", "reasoning_effort": "high", "approach": "solo", "reason": "Measured results", "tags": {"kind": "feature"}})).unwrap();
    let read = get_launch_plan(&f.domain, &"t".into()).unwrap();
    assert_eq!(read["launch"]["effort"], "high");
    assert_eq!(read["launch"]["approach"], "solo");
    assert_eq!(read["launch"]["reason"], "Measured results");
    assert_eq!(read["launch"]["tags"]["kind"], "feature");
    assert_eq!(partial_plan(&f, "missing", json!({"model": "codex/gpt-6"})).unwrap_err().code, "not_found");
}

#[test]
fn partial_launch_plan_refuses_null_unknown_and_duplicate_fields() {
    for changes in [json!({"mission": "lost"}), json!({"name": "lost"}), json!({"cwd": "/"}),
        json!({"role": "review"}), json!({"project": "other"}), json!({"model": null}), json!({"effort": null}),
        json!({"approach": null}), json!({"tags": null}), json!({"reason": null}), json!({"effort": "high", "reasoning_effort": "low"})] {
        assert!(serde_json::from_value::<UpdateLaunchPlanBody>(json!({"task_id": "t", "changes": changes})).is_err());
    }
}

#[test]
fn partial_launch_plan_and_replacement_refuse_a_started_task_and_an_active_launch() {
    let f = world();
    let before = stored_selection(&f);
    let guard = Launching::begin(&f.domain, &"t".into()).unwrap();
    assert_eq!(partial_plan(&f, "t", json!({"model": "sonnet"})).unwrap_err().code, "conflict");
    drop(guard);
    give(&f, "t", "agent-b");
    assert_eq!(partial_plan(&f, "t", json!({"model": "sonnet"})).unwrap_err().code, "conflict");
    let body = serde_json::from_value(json!({"task_id": "t", "launch": before})).unwrap();
    assert_eq!(set_launch_plan(&f.domain, &f.home, body).unwrap_err().code, "conflict");
    assert_eq!(f.domain.launch_plan(&"t".into()).unwrap().unwrap(), before);
    let _released = Launching::begin(&f.domain, &"t".into()).unwrap();
}

#[test]
fn partial_launch_plan_refuses_the_users_fixed_work_effort_even_for_a_native_model() {
    let f = world();
    connect_claude_and_codex(&f);
    let mut choices = crate::domain_store::RequestLaunch {
        project: None, options: alondra_core::options::OptionsFile::default().orchestrator_defaults(None, None),
        issue: None, issue_text: None, attachments: vec![],
    };
    choices.options.effort = "high".into();
    let state = f.domain.state().unwrap();
    f.domain.execute_with(&user(), &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "scheduled".into(), expected_revision: state.revision,
        command: Command::CreateRequest { request_id: "scheduled".into(), body: "Scheduled work with a fixed effort".into(), auto_merge: false, maintenance: false, message: false },
    }, None, Some(&choices), || Ok(())).unwrap();
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
        task_id: "scheduled".into(), request_id: "scheduled".into(), title: "Scheduled".into(), agent_id: None, review_prs: false,
        dependencies: Default::default(), stack: None, launch: Some(alondra_daemon_engine_model::LaunchPlan::new("scheduled", f.project.to_str().unwrap())) });
    let before = get_launch_plan(&f.domain, &"scheduled".into()).unwrap();
    let error = partial_plan(&f, "scheduled", json!({"model": "codex/gpt-6"})).unwrap_err();
    assert_eq!(error.code, "invalid_input");
    assert!(error.message.contains("user chose effort high"), "{}", error.message);
    assert_eq!(get_launch_plan(&f.domain, &"scheduled".into()).unwrap(), before);
}

#[test]
fn partial_launch_plan_refuses_a_task_with_historical_start_evidence() {
    let f = world();
    let mut state = (*f.domain.state().unwrap()).clone();
    state.lifecycle_journal.insert("t".into(), vec![alondra_core::domain::LifecycleRecord::ResourceAvailable]);
    assert_eq!(editable_plan(&state, &"t".into(), false).unwrap_err().code, "conflict");
    state.lifecycle_journal.remove(&TaskId::from("t"));
    assert_eq!(editable_plan(&state, &"t".into(), true).unwrap_err().code, "conflict");
}

#[tokio::test]
async fn partial_launch_plan_is_reread_after_an_admitted_launch_captured_the_old_queue() {
    let f = world();
    connect_claude_and_codex(&f);
    let before = stored_selection(&f);
    let stale = LaunchBody::from_plan("t".into(), &before);
    f.domain.store.lock().unwrap().enqueue_launch(&"t".into(), &stale.to_queue(), "orchestrator", 2).unwrap();
    partial_plan(&f, "t", json!({"model": "claude/opus"})).unwrap();
    launch_from(&f.domain, &f.home, stale, |_| None, Origin::Admitted).await.unwrap();
    f.launched(1).await;
    assert_eq!(f.sdk().flag("--model").as_deref(), Some("opus"));
    assert!(f.prompt().await.contains("Added context: preserve colours."));
}

#[test]
fn partial_launch_plan_storage_rolls_back_when_the_queued_body_is_invalid() {
    let f = world();
    connect_claude_and_codex(&f);
    let before = stored_selection(&f);
    f.domain.store.lock().unwrap().enqueue_launch(&"t".into(), &json!({"body": null}), "orchestrator", 2).unwrap();
    assert_eq!(partial_plan(&f, "t", json!({"model": "codex/gpt-6"})).unwrap_err().code, "storage_unavailable");
    assert_eq!(f.domain.launch_plan(&"t".into()).unwrap().unwrap(), before);
}
