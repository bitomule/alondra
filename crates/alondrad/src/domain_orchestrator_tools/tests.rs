use super::*;
use crate::domain_store::HandoverProcess;
use crate::node_close::Answer;
use crate::node_push::testing::Node;
use crate::sdk_handover::BgSessions;
use crate::sdk_owner::LaunchOptions;
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

fn command(domain: &DomainRuntime, actor: Actor, command: Command) {
    let state = domain.state().unwrap();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
        command_id: format!("test-{}", crate::auth::random_hex(8)).into(), expected_revision: state.revision, command,
    };
    domain.execute(&actor, &envelope, None, || Ok(())).unwrap();
}

fn user() -> Actor {
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
    let sessions = Arc::new(FakeSessions::default());
    let _ = sessions.domain.set(Arc::downgrade(&domain));
    domain.handovers.use_sessions(sessions.clone() as Arc<dyn BgSessions>);
    *domain.handovers.turn_deadline.lock().unwrap() = Some(std::time::Duration::from_millis(20));
    command(&domain, Actor::System, Command::RegisterAgent { agent_id: "agent-b".into(), name: "Instance B".into(), at: 0 });
    command(&domain, user(), Command::CreateRequest { request_id: "r".into(), body: "Make the editor readable".into(), auto_merge: false, maintenance: false, message: false });
    command(&domain, user(), Command::TitleRequest { request_id: "r".into(), title: "Readable editor".into() });
    command(&domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, task_id: "t".into(), request_id: "r".into(), title: "Editor colours".into(),
        agent_id: None, review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
    Fixture { sdk: None, sessions, root, home, project, domain }
}

/// A backend whose host cannot start: every SDK launch fails.
fn broken_backend(f: &Fixture) -> crate::sdk_owner::SdkBackend {
    let missing = f.root.join("missing");
    crate::sdk_owner::SdkBackend {
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
    /// The connection a stop drops, which the revival then sees lost.
    drops_on_stop: Mutex<Option<u64>>,
}

const REVIVAL: crate::node_revival::Timing = crate::node_revival::Timing { grace: std::time::Duration::from_millis(10) };

impl BgSessions for FakeSessions {
    fn seen(&self, _: &DomainRuntime, _: &str) -> Option<HandoverProcess> {
        None
    }

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
            let dropped = self.drops_on_stop.lock().unwrap().take();
            if let (Some(connection), Some(domain)) = (dropped, &domain) {
                domain.live.mods.disconnect(session, connection);
                crate::node_revival::revive(domain, session, &REVIVAL).await;
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
    assert_eq!(f.sdk().options(&agent.0), LaunchOptions { model: Some("opus".into()), effort: Some("xhigh".into()), disallowed_tools: vec![] });
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
    command(&closed.domain, user(), Command::AssignTask { task_id: "t".into(), agent_id: "agent-b".into(), model: None });
    command(&closed.domain, Actor::Agent { agent_id: "agent-b".into() }, Command::StartTask { task_id: "t".into() });
    closed.domain.connect_node(serde_json::from_value(json!({"session_id": "agent-b", "version": "2.1.287", "cwd": closed.project.to_str().unwrap()})).unwrap(), 1);
    crate::node_revival::user_closed(&closed.domain, "agent-b").unwrap();
    relaunch(&closed.domain, &closed.home, &"t".into()).await.unwrap();
    assert_eq!(model(&closed), Some(TaskModel { model: "sonnet".into(), chosen_by: ModelChooser::Orchestrator }),
        "an auto-effort task that never recorded a model restarts on the starting model");

    let replaced = world();
    connected_node_on_the_task(&replaced);
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
    assert_eq!(f.sdk().options(&agent_of(&answer).0), LaunchOptions { model: Some("sonnet".into()), effort: Some("medium".into()), disallowed_tools: vec![] });
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
    assert_eq!(f.sdk().options(&agent_of(&answer).0), LaunchOptions { model: Some("claude-opus-5-5[1m]".into()), effort: None, disallowed_tools: vec![] });
}

#[tokio::test]
async fn a_launch_is_refused_for_a_closed_or_taken_task() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    command(&f.domain, actor.clone(), Command::AssignTask { task_id: "t".into(), agent_id: "agent-b".into(), model: None });
    crate::domain_agent_lifecycle::move_instance(&f.domain, &"agent-b".into(), alondra_core::domain::NodeEvent::TurnStarted,
        &crate::domain_agent_lifecycle::prepare_event(&f.domain, "start-b").unwrap()).unwrap();
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
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, task_id: "t2".into(), request_id: "r".into(), title: "Editor fonts".into(),
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
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, task_id: "t2".into(), request_id: "r".into(), title: "Editor fonts".into(),
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

#[tokio::test]
async fn a_launch_is_refused_while_the_disk_is_nearly_full_and_goes_ahead_once_it_has_space() {
    let f = world();
    f.domain.live.machine_watch.set_disk_low(true);
    let refused = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap_err();
    assert_eq!(refused.code, "disk_low");
    assert!(refused.message.starts_with("The disk is nearly full: Alondra starts no node until there is space again"), "{}", refused.message);
    assert!(refused.message.ends_with("t stays as it is and can be launched after that"), "{}", refused.message);
    assert!(f.sdk().starts().is_empty());
    assert!(f.task_node().is_none(), "the task is left as it was");

    f.domain.live.machine_watch.set_disk_low(false);
    launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    assert_eq!(f.launched(1).await.0.len(), 1);
}

#[tokio::test]
async fn a_task_whose_node_the_user_closed_is_relaunched_for_that_task_in_the_closed_nodes_folder() {
    let f = world();
    command(&f.domain, user(), Command::AssignTask { task_id: "t".into(), agent_id: "agent-b".into(), model: None });
    command(&f.domain, Actor::Agent { agent_id: "agent-b".into() }, Command::StartTask { task_id: "t".into() });
    f.domain.connect_node(serde_json::from_value(json!({"session_id": "agent-b", "version": "2.1.287", "cwd": f.project.to_str().unwrap()})).unwrap(), 1);
    let refused = relaunch(&f.domain, &f.home, &"t".into()).await.unwrap_err();
    assert_eq!(refused.code, "conflict", "a task whose node was not closed is not relaunched");
    assert!(f.sdk().starts().is_empty());

    crate::node_revival::user_closed(&f.domain, "agent-b").unwrap();
    let state = f.domain.state().unwrap();
    assert_eq!((state.tasks[&"t".into()].state, state.tasks[&"t".into()].agent_id.clone()), (TaskState::NotStarted, None));
    assert_eq!(f.domain.live.closed_task_ids(), ["t"]);

    let answer = relaunch(&f.domain, &f.home, &"t".into()).await.unwrap();
    let agent = agent_of(&answer);
    let (folders, prompts) = f.launched(1).await;
    assert!(same(&folders[0], &f.project), "the new node starts where the closed one worked: {folders:?}");
    assert!(prompts[0].contains("Editor colours") && prompts[0].contains("Make the editor readable"), "the mission comes with it: {}", prompts[0]);
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
    command(&f.domain, user(), Command::AssignTask { task_id: "t".into(), agent_id: "agent-b".into(), model: None });
    command(&f.domain, Actor::Agent { agent_id: "agent-b".into() }, Command::StartTask { task_id: "t".into() });
    f.domain.connect_node(serde_json::from_value(json!({"session_id": "agent-b", "version": "2.1.287", "cwd": f.project.to_str().unwrap()})).unwrap(), 1);
    crate::node_revival::user_closed(&f.domain, "agent-b").unwrap();
    relaunch(&f.domain, &f.home, &"t".into()).await.unwrap();
    assert_eq!(decisions(&f.domain), [row("relaunch", Some("sonnet"), None, Some("solo"), None)]);
}

#[tokio::test]
async fn closing_a_launched_node_keeps_its_model_and_task() {
    let f = bare_world();
    give(&f, "t", "agent-b");
    record(&f.domain, Decision { task_id: "t".into(), request_id: "r".into(), agent_id: Some("agent-b".into()), at: 1, kind: DecisionKind::Launch,
        model: Some("sonnet".into()), effort: None, approach: None, chosen_by: ModelChooser::Orchestrator, reason: None, title: "Editor colours".into() });
    f.domain.report(serde_json::from_value(json!({"session_id": "agent-b", "event": "session.start", "version": "2.1.287"})).unwrap(), 1);
    f.domain.report(serde_json::from_value(json!({"session_id": "agent-b", "event": "model", "model": "claude-sonnet-5-5"})).unwrap(), 3);
    assert!(f.domain.store.lock().unwrap().node_cost("agent-b").unwrap().is_none());

    crate::node_revival::user_closed(&f.domain, "agent-b").unwrap();
    let kept = f.domain.store.lock().unwrap().node_cost("agent-b").unwrap().expect("the closed node's cost is kept");
    assert_eq!((kept.task_id.as_deref(), kept.model.as_deref(), kept.cost_usd, kept.context_peak_percent),
        (Some("t"), Some("claude-sonnet-5-5"), None, None), "the task comes from its launch, though closing freed it");
    assert!(kept.started_at.is_some_and(|started| kept.ended_at >= started));
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
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, task_id: "t2".into(), request_id: "r".into(), title: "Editor fonts".into(),
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
    assert_eq!(clipped(&"x".repeat(5), 3, "[shortened]"), "xxx[shortened]");
    assert_eq!(clipped("short", 10, "[shortened]"), "short");
}

#[tokio::test]
async fn a_launch_of_a_task_with_a_plan_needs_no_name_or_cwd() {
    let f = world();
    let plan = crate::launch_plans::LaunchPlan {
        name: "app/editor".into(), cwd: f.project.to_str().unwrap().into(), mission: Some("Make the colours tokens".into()),
        model: Some("sonnet".into()), effort: None, approach: Some(alondra_core::options::Approach::Solo),
        reason: Some("Colour tokens in one file".into()), role: None,
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
    give(&f, "t", "agent-b");
    command(&f.domain, Actor::Agent { agent_id: "agent-b".into() }, Command::StartTask { task_id: "t".into() });
    command(&f.domain, Actor::System, Command::MoveNode { agent_id: "agent-b".into(), event: alondra_core::domain::NodeEvent::SessionLost, at: 0 });
    let answer = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    assert_eq!(f.task_node(), Some(agent_of(&answer)));
    assert!(f.domain.owns_sdk_host(&agent_of(&answer)), "the new node runs");
}

#[tokio::test]
async fn the_changes_the_user_asked_for_reach_the_node_that_takes_the_task_over() {
    let f = world();
    give(&f, "t", "agent-b");
    let targets = || vec![alondra_core::domain::DeliveryTarget::Text { text: "Result".into() }];
    command(&f.domain, Actor::Agent { agent_id: "agent-b".into() }, Command::PresentResult { checklist: Vec::new(), attachments: vec![], task_id: "t".into(),
        interaction_id: "final".into(), body: "Done".into(), targets: targets(), is_final: true, nothing_to_review: false,
        card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true) });
    command(&f.domain, user(), Command::SubmitFeedback { attachments: vec![], presentation_id: "final".into(), targets: targets(),
        body: "  Use the new palette  ".into(), instruction: alondra_core::domain::Instruction::Changes });
    command(&f.domain, Actor::System, Command::MoveNode { agent_id: "agent-b".into(), event: alondra_core::domain::NodeEvent::SessionLost, at: 0 });
    command(&f.domain, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: alondra_core::domain::NodeEnd::Closed, agent_id: Some("agent-b".into()) });
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].state, TaskState::NotStarted);
    launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    let prompt = f.prompt().await;
    assert!(prompt.contains("## Changes the user asked for on the result you take over\n\n- Use the new palette"), "{prompt}");
}

fn connected_node_on_the_task(f: &Fixture) -> u64 {
    connected_node(f, "agent-b")
}

fn connected_node(f: &Fixture, agent: &str) -> u64 {
    give(f, "t", agent);
    command(&f.domain, Actor::Agent { agent_id: agent.into() }, Command::StartTask { task_id: "t".into() });
    crate::domain_agent_lifecycle::move_instance(&f.domain, &agent.into(), alondra_core::domain::NodeEvent::TurnStarted,
        &crate::domain_agent_lifecycle::prepare_event(&f.domain, &format!("start-{agent}")).unwrap()).unwrap();
    f.domain.connect_node(serde_json::from_value(json!({"session_id": agent, "version": "2.1.287", "cwd": f.project.to_str().unwrap()})).unwrap(), 1)
}

#[tokio::test]
async fn the_replaced_node_is_never_revived_when_its_stop_drops_the_connection() {
    let f = world();
    let connection = connected_node_on_the_task(&f);
    *f.sessions.drops_on_stop.lock().unwrap() = Some(connection);
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    launch(&f.domain, &f.home, body, |_| None).await.unwrap();
    assert_eq!(*f.sessions.stopped.lock().unwrap(), ["agent-b"]);
    let agent_b = AgentId::from("agent-b");
    assert!(!crate::sdk_handover::moved(&f.domain, &agent_b), "the revival saw the stop and did not bring the old node back");

    crate::node_revival::revive(&f.domain, "agent-b", &REVIVAL).await;
    assert!(!crate::sdk_handover::moved(&f.domain, &agent_b), "nor later, when the daemon reports every known session as lost");
    assert_eq!(f.launched(1).await.0.len(), 1, "only the replacement started");
}

#[tokio::test]
async fn replacing_a_tasks_node_stops_it_keeps_its_folder_and_gives_the_task_to_the_new_model() {
    let f = world();
    connected_node_on_the_task(&f);
    let taken = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "opus"})), |_| None).await.unwrap_err();
    assert!(taken.message.contains("already has a connected agent"), "without replace a live node keeps its task: {}", taken.message);

    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus", "effort": "high", "prompt": "Finish the tokens"})).unwrap();
    let answer = launch(&f.domain, &f.home, body, |_| None).await.unwrap();
    let agent = agent_of(&answer);
    assert_eq!(*f.sessions.stopped.lock().unwrap(), ["agent-b"], "the old session is stopped");
    let (folders, prompts) = f.launched(1).await;
    assert!(same(&folders[0], &f.project), "the new node starts where the old one worked: {folders:?}");
    assert_eq!((f.sdk().flag("--model").as_deref(), f.sdk().flag("--effort").as_deref()), (Some("opus"), Some("high")), "{:?}", f.sdk().args());
    assert!(prompts[0].starts_with("Finish the tokens"), "{}", prompts[0]);
    let state = f.domain.state().unwrap();
    assert_eq!(state.agents[&agent].name, "Instance B", "it keeps the old node's name");
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id, Some(agent));
    assert!(!state.agents[&AgentId::from("agent-b")].phase.holds_slot(), "the old node is disconnected");
    assert!(f.domain.live.is_closed("agent-b"), "and is not brought back");
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
    assert!(state.agents[&AgentId::from("agent-b")].phase.holds_slot());

    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, task_id: "free".into(), request_id: "r".into(), title: "Free".into(),
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
    connected_node_on_the_task(&f);
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus", "cwd": linked.to_str().unwrap()})).unwrap();
    launch(&f.domain, &f.home, body, |_| None).await.unwrap();
    let (folders, _) = f.launched(1).await;
    assert!(same(&folders[0], &linked), "{folders:?}");
}

#[tokio::test]
async fn a_replacement_whose_stop_fails_launches_nothing_and_the_old_node_keeps_the_task() {
    let f = world();
    connected_node_on_the_task(&f);
    *f.sessions.stop_fails.lock().unwrap() = Some("claude agents still shows agent-b running after claude stop".into());
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    let refused = launch(&f.domain, &f.home, body, |_| None).await.unwrap_err();
    assert_eq!(refused.code, "replace_failed");
    assert!(refused.message.contains("still shows agent-b running"), "the orchestrator gets the reason: {}", refused.message);
    assert!(f.sdk().starts().is_empty(), "no second node is launched into the worktree");
    let state = f.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id, Some(AgentId::from("agent-b")), "the old node keeps its task");
    assert_eq!(state.tasks[&TaskId::from("t")].state, TaskState::Running);
    assert_eq!(state.agents.len(), 1, "no new agent was registered");
    assert!(state.agents[&AgentId::from("agent-b")].phase.holds_slot() && !f.domain.live.is_closed("agent-b"));
    assert!(!refused.message.contains("keeps its node"), "{}", refused.message);
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
    connected_node_on_the_task(&f);
    let body = || serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    let answer = launch(&f.domain, &f.home, body(), |_| None).await.unwrap();
    assert_eq!(f.domain.inherited_worktree(&agent_of(&answer).0).unwrap(), None, "the user's project folder is never Alondra's to delete");

    let g = world();
    connected_node_on_the_task(&g);
    let dir = g.project.to_str().unwrap();
    g.domain.set_inherited_worktree("agent-b", dir, "agent-a").unwrap();
    let answer = launch(&g.domain, &g.home, body(), |_| None).await.unwrap();
    assert_eq!(g.domain.inherited_worktree(&agent_of(&answer).0).unwrap().as_deref(), Some(dir), "a worktree passed down a chain of replacements stays Alondra's");
}

/// `claude stop` can report a failure for a session that ended anyway: the replacement goes on.
#[tokio::test]
async fn a_replacement_whose_stop_errs_but_whose_node_ended_finishes() {
    let f = world();
    connected_node_on_the_task(&f);
    f.domain.live.mods.set_job("agent-b", None, Some(f.root.join("jobs/gone")));
    *f.sessions.stop_fails.lock().unwrap() = Some("claude stop timed out".into());
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    let answer = launch(&f.domain, &f.home, body, |_| None).await.unwrap();
    assert_eq!(f.task_node(), Some(agent_of(&answer)));
    assert_eq!(release_reasons(&f), ["node replaced by the orchestrator"]);
}

/// The node a replacement could not stop ends later: the task is freed for that reason, never as
/// closed by the user, and the orchestrator hears it.
#[tokio::test]
async fn a_node_that_ends_after_its_replacement_stopped_short_frees_its_task_with_the_real_reason() {
    let f = world();
    connected_node_on_the_task(&f);
    *f.sessions.stop_fails.lock().unwrap() = Some("claude agents still shows agent-b running after claude stop".into());
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    launch(&f.domain, &f.home, body, |_| None).await.unwrap_err();

    crate::node_revival::user_closed(&f.domain, "agent-b").unwrap();
    assert_eq!(f.task_node(), None, "the task is free");
    assert_eq!(release_reasons(&f), [crate::node_revival::REPLACE_ABORTED]);
    let told = told_orchestrator(&f, "replace-aborted-");
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("“Editor colours” (t) has no node now") && told[0].contains("still shows agent-b running")
        && told[0].contains("launch (task t), without replace"), "{}", told[0]);
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
    *f.domain.handovers.turn_deadline.lock().unwrap() = Some(std::time::Duration::from_secs(5));
    let domain = f.domain.clone();
    let ends = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        domain.live.observe(&json!({"session_id": "agent-b", "hook_event_name": "Stop"}));
    });
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    launch(&f.domain, &f.home, body, |_| None).await.unwrap();
    ends.await.unwrap();
    assert_eq!(*f.sessions.busy_at_stop.lock().unwrap(), [false], "claude stop waited for the turn to end");
}

#[tokio::test]
async fn a_node_whose_turn_does_not_end_is_interrupted_before_it_is_stopped() {
    let f = world();
    connected_node_on_the_task(&f);
    let node = Node::connect(&f.domain, "agent-b");
    on_a_turn(&f);
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    launch(&f.domain, &f.home, body, |_| None).await.unwrap();
    assert_eq!(node.interrupts(), 1, "past the deadline the turn is interrupted first");
    assert_eq!(f.sessions.stopped.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn the_replacing_node_is_told_to_continue_in_the_worktree_without_making_another() {
    let f = world();
    connected_node_on_the_task(&f);
    let repo = f.root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-q", "-b", "main"]);
    git_in(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "start"]);
    git_in(&repo, &["worktree", "add", "-q", "-b", "worktree-t-cards-3", f.project.to_str().unwrap()]);
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    launch(&f.domain, &f.home, body, |_| None).await.unwrap();
    let prompt = f.prompt().await;
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

/// The bug: David stopped the node of a task, so it had no node to replace, and the orchestrator
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
    assert!(same(Path::new(&recorded), &made));
    launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet", "replace": true})), |_| None).await.unwrap();
    let (folders, _) = f.launched(2).await;
    assert!(same(&folders[1], &made), "the next launch works in the same worktree: {folders:?}");
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

/// What `alondra models --json` shows after a sonnet task launched by the orchestrator goes
/// through a review that asks for changes twice, every call made over the daemon's own routes.
#[tokio::test]
async fn a_review_that_asks_for_changes_twice_shows_its_rounds_and_serious_findings_in_the_model_report() {
    let root = Directory::new();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    drop(DomainRuntime::open(&home).unwrap());
    let daemon = crate::domain_bootstrap::DomainDaemon::open(&home).unwrap();
    let domain = daemon.domain();
    command(&domain, user(), Command::CreateRequest { request_id: "r".into(), body: "Make the editor readable".into(), auto_merge: false, maintenance: false, message: false });
    command(&domain, user(), Command::TitleRequest { request_id: "r".into(), title: "Readable editor".into() });
    command(&domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, task_id: "t".into(), request_id: "r".into(), title: "Editor colours".into(),
        agent_id: None, review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
    command(&domain, Actor::System, Command::RegisterAgent { agent_id: "reviewer".into(), name: "Reviewer".into(), at: 0 });
    command(&domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, task_id: "review".into(), request_id: "r".into(), title: "Review the editor colours".into(),
        agent_id: Some("reviewer".into()), review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
    command(&domain, user(), Command::SetRelations { task_id: "review".into(), delivered_dependencies: ["t".into()].into(), blocks: ["t".into()].into() });

    command(&domain, Actor::System, Command::RegisterAgent { agent_id: "worker".into(), name: "editor".into(), at: 0 });
    command(&domain, Actor::Orchestrator { generation: 0 }, Command::AssignTask { task_id: "t".into(), agent_id: "worker".into(), model: None });
    record(&domain, Decision { task_id: "t".into(), request_id: "r".into(), agent_id: Some("worker".into()), at: alondra_core::now_millis(),
        kind: DecisionKind::Launch, model: Some("sonnet".into()), effort: Some("medium".into()), approach: None,
        chosen_by: ModelChooser::Orchestrator, reason: Some("Colour tokens in one file: sonnet".into()), title: "Editor colours".into() });
    let worker = domain.bind_agent("worker").unwrap().expose_for_child_environment().to_owned();
    let reviewer = domain.bind_agent("reviewer").unwrap().expose_for_child_environment().to_owned();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = crate::domain_bootstrap::router(daemon.clone()).layer(axum::middleware::from_fn_with_state(daemon.auth(), crate::auth::guard));
    let server = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
    });
    let client = reqwest::Client::new();
    let epoch = domain.state().unwrap().data_epoch.0.clone();
    let mut sent = 0;
    let mut call = async |bearer: &str, path: String, mut body: Value| {
        sent += 1;
        body["data_epoch"] = json!(epoch);
        body["command_id"] = json!(format!("call-{sent}"));
        body["expected_revision"] = json!(0);
        let response = client.post(format!("{base}{path}")).bearer_auth(bearer).json(&body).send().await.unwrap();
        let status = response.status();
        assert!(status.is_success(), "{path}: {status} {}", response.text().await.unwrap());
    };
    let delivery = |task: &str, id: &str| json!({"task_id": task, "interaction_id": id, "body": "Done", "is_final": true, "attachments": [],
        "targets": [{"kind": "text", "text": "The result"}],
        "card": serde_json::to_value(alondra_core::domain::CardSpec::delivery("Review the result", &[], true)).unwrap()});

    call(&worker, "/tasks/t/start".into(), json!({"task_id": "t"})).await;
    let verdicts = [
        json!({"kind": "changes_requested", "text": "Empty list crashes\nContrast below 4.5:1", "serious_findings": 2}),
        json!({"kind": "changes_requested", "text": "Contrast still below 4.5:1", "serious_findings": 1}),
        json!({"kind": "approved", "summary": "Fixed"}),
    ];
    for (round, verdict) in verdicts.into_iter().enumerate() {
        call(&worker, "/tasks/t/results".into(), delivery("t", &format!("delivery-{round}"))).await;
        if round == 0 {
            call(&reviewer, "/tasks/review/start".into(), json!({"task_id": "review"})).await;
        }
        call(&reviewer, "/tasks/review/results".into(), delivery("review", &format!("review-{round}"))).await;
        call(&reviewer, "/tasks/review/verdict".into(), json!({"task_id": "review", "presentation_id": format!("review-{round}"), "verdict": verdict})).await;
    }

    let report: Value = client.get(format!("{base}/models")).send().await.unwrap().json().await.unwrap();
    println!("GET /models (what alondra models --json prints):\n{}", serde_json::to_string_pretty(&report).unwrap());
    let sonnet = &report["by_model"][0];
    assert_eq!((&sonnet["model"], &sonnet["effort"], &sonnet["tasks"]), (&json!("sonnet"), &json!("medium"), &json!(1)));
    assert_eq!((&sonnet["change_rounds"], &sonnet["reviewer_rounds"], &sonnet["serious_findings"]), (&json!(2), &json!(2), &json!(3)));
    let attempt = &report["attempts"][0];
    assert_eq!((&attempt["task_id"], &attempt["change_rounds"], &attempt["serious_findings"]), (&json!("t"), &json!(2), &json!(3)));
    assert_eq!(report["sonnet_trouble"][0]["task_id"], "t", "rounds on a delivery without a PR still make a sonnet task go badly");
    server.abort();
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
async fn until_replaced(f: &Fixture) {
    let mut changed = f.domain.subscribe();
    while f.task_node().is_none_or(|agent| agent.0 == "agent-b") {
        changed.changed().await.unwrap();
    }
}

/// Waits until the replacement under way gives the task's launch guard back.
async fn until_launchable(f: &Fixture) {
    for _ in 0..500 {
        if f.domain.live.begin_launch("t") {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the launch guard is released once it ends");
}

/// Waiting for the old node's turn can outlast the call: the orchestrator gets its answer at once,
/// and a replacement that finishes tells it nothing more.
#[tokio::test]
async fn a_replacement_answers_before_the_old_node_stops_and_finishes_without_a_notice() {
    let f = world();
    connected_node_on_the_task(&f);
    on_a_turn(&f);
    *f.domain.handovers.turn_deadline.lock().unwrap() = Some(std::time::Duration::from_secs(5));
    let answer = launch_replacing(f.domain.clone(), f.home.clone(), replace_body(), |_| None).await.unwrap();
    assert_eq!(answer["replacing"], true, "{answer}");
    assert!(answer["message"].as_str().unwrap().contains("you are told only if it did not start"), "{answer}");
    assert!(f.sessions.stopped.lock().unwrap().is_empty(), "answered while the old node is still on its turn");

    let again = launch_replacing(f.domain.clone(), f.home.clone(), replace_body(), |_| None).await.unwrap_err();
    assert_eq!(again.code, "conflict", "a second launch waits for the replacement under way: {}", again.message);

    f.domain.live.observe(&json!({"session_id": "agent-b", "hook_event_name": "Stop"}));
    until_replaced(&f).await;
    assert!(told_orchestrator(&f, "replaced-").is_empty(), "a finished replacement is no news");
    until_launchable(&f).await;
}

#[tokio::test]
async fn a_replacement_that_fails_after_answering_tells_the_orchestrator_why() {
    let f = world();
    connected_node_on_the_task(&f);
    *f.sessions.stop_fails.lock().unwrap() = Some("claude agents still shows agent-b running after claude stop".into());
    let answer = launch_replacing(f.domain.clone(), f.home.clone(), replace_body(), |_| None).await.unwrap();
    assert_eq!(answer["replacing"], true);
    let told = until_told(&f, "replaced-").await;
    assert!(told[0].contains("did not finish") && told[0].contains("still shows agent-b running"), "{}", told[0]);
    assert!(!told[0].contains("keeps its node"), "{}", told[0]);
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

/// Once the old node is stopped the task has no node: a launch that fails after that says so.
#[tokio::test]
async fn a_replacement_whose_new_node_does_not_start_says_the_task_has_no_node() {
    let f = bare_world();
    f.domain.enable_sdk(broken_backend(&f));
    connected_node_on_the_task(&f);
    launch_replacing(f.domain.clone(), f.home.clone(), replace_body(), |_| None).await.unwrap();
    let told = until_told(&f, "replaced-").await;
    assert!(told[0].contains("did not start through the Agent SDK")
        && told[0].contains("The node it replaced was stopped, so “Editor colours” (t) has no node now: start one with launch (task t), without replace."),
        "{}", told[0]);
    assert_eq!(f.task_node(), None);
}

/// The call can be dropped as soon as the replacement starts: the replacement still finishes.
#[tokio::test]
async fn a_dropped_replace_call_still_finishes_the_replacement() {
    let f = world();
    connected_node_on_the_task(&f);
    let call = launch_replacing(f.domain.clone(), f.home.clone(), replace_body(), |_| None);
    assert!(tokio::time::timeout(std::time::Duration::ZERO, call).await.is_err(), "the call is dropped before it answers");
    until_replaced(&f).await;
    until_launchable(&f).await;
}

/// A panic in the replacement gives the launch guard back and the orchestrator its reason.
#[tokio::test]
async fn a_replacement_that_panics_releases_the_task_and_tells_the_orchestrator() {
    let f = world();
    connected_node_on_the_task(&f);
    f.sessions.stop_panics.store(true, Ordering::SeqCst);
    let reason = match launch_replacing(f.domain.clone(), f.home.clone(), replace_body(), |_| None).await {
        Err(refused) => refused.message,
        Ok(_) => until_told(&f, "replaced-").await.remove(0),
    };
    assert!(reason.contains("stopped unexpectedly"), "{reason}");
    assert!(f.domain.live.begin_launch("t"), "the task is not left being launched");
}

/// The branch where Claude Code made the old node's worktree, read from a Claude home of the
/// test's own.
#[tokio::test]
async fn a_replacement_inherits_the_worktree_claude_code_made_for_the_old_node() {
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
    let answer = launch(&f.domain, &f.home, replace_body(), |_| None).await.unwrap();
    let (folders, _) = f.launched(1).await;
    assert!(same(&folders[0], &made), "the new node starts in that worktree: {folders:?}");
    assert_eq!(f.domain.inherited_worktree(&agent_of(&answer).0).unwrap().as_deref(), made.to_str(), "and Alondra may delete it with the new node");
}

/// What Alondra keeps about a session only while it lives goes when it ends.
#[tokio::test]
async fn an_ended_session_leaves_no_inherited_worktree_or_aborted_replace_behind() {
    let f = bare_world();
    connected_node_on_the_task(&f);
    f.domain.set_inherited_worktree("agent-b", f.project.to_str().unwrap(), "agent-a").unwrap();
    f.domain.live.note_aborted_replace("agent-b", "stop failed");
    crate::node_revival::take_out(&f.domain, "agent-b", "node closed by the user", None).unwrap();
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
    command(&f.domain, orchestrator, Command::CreateTask { plan_items: None, task_id: "build".into(), request_id: "implement-report".into(), title: "Index the library".into(),
        agent_id: None, review_prs: false, dependencies: ["t".into()].into(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });

    launch(&f.domain, &f.home, launch_body(&f, json!({"task": "build", "model": "sonnet"})), |_| None).await.unwrap();
    let prompt = f.prompt().await;
    assert!(prompt.contains("## The plan the user approved") && prompt.contains("do not plan again"), "{prompt}");
    assert!(prompt.contains("- [ ] Store feature prints in Core Data\n- [ ] Follow changes in the background"), "the whole plan: {prompt}");
    assert_eq!(prompt.matches("Store feature prints in Core Data").count(), 1, "the plan once, not again as the request: {prompt}");
}

/// The task the user decided on, «Editor fonts», and «Editor colours» (t), which waits for it.
fn decided_on(f: &Fixture) {
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { task_id: "fonts".into(), request_id: "r".into(),
        title: "Editor fonts".into(), agent_id: Some("agent-b".into()), review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("fonts", "/tmp")), plan_items: None });
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
        plan_items: Some(PlanItems { from_task: "t".into(), items: vec!["2".into(), "3".into()] }) });

    launch(&f.domain, &f.home, launch_body(&f, json!({"task": "index", "model": "sonnet"})), |_| None).await.unwrap();
    let prompt = f.prompt().await;
    assert!(prompt.contains("## The plan the user approved") && prompt.contains("do not plan again"), "{prompt}");
    assert!(prompt.contains("Your part of it is points 2, 3; the other points are other tasks"), "{prompt}");
    assert!(prompt.contains("- [ ] Guards") && prompt.contains("- [ ] Index matches the state"), "the whole plan: {prompt}");
}

// Replacing a claude --bg node of the previous release stops it, frees the task from it and
// admits a node of the SDK in its folder: the claude --bg node never goes down the SDK node's
// replacement, which only stops an SDK host.
#[tokio::test]
async fn replacing_a_claude_bg_node_stops_it_and_starts_an_sdk_node() {
    let f = world();
    connected_node_on_the_task(&f);
    let answer = launch(&f.domain, &f.home, replace_body(), |_| None).await.unwrap();
    assert_eq!(*f.sessions.stopped.lock().unwrap(), ["agent-b"], "the claude --bg node is stopped");
    let agent = agent_of(&answer);
    let state = f.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id.as_ref(), Some(&agent));
    assert!(crate::sdk_owner::runs_through_sdk(&state, &agent) && f.domain.owns_sdk_host(&agent), "a node of the SDK runs the task");
    assert!(f.domain.live.is_closed("agent-b"), "the old node is closed and not brought back");
}

struct NoBgSessions;

impl crate::node_close::Sessions for NoBgSessions {
    fn claude<'a>(&'a self, _: Vec<String>) -> Answer<'a, Result<String, String>> {
        Box::pin(async { Ok("[]".into()) })
    }
    fn leftovers<'a>(&'a self, _: &'a str) -> Answer<'a, Option<String>> {
        Box::pin(async { None })
    }
}

// `claude agents` lists only claude --bg sessions: a node of the SDK missing from it is not lost,
// and stays in Fleet while its task is open.
#[tokio::test]
async fn a_node_of_the_sdk_missing_from_claudes_listing_stays_in_fleet() {
    let f = world();
    let agent = agent_of(&launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap());
    assert!(crate::node_reconcile::once(&f.domain, &NoBgSessions).await);
    let first = alondra_core::now_millis();
    while alondra_core::now_millis() <= first {
        tokio::task::yield_now().await;
    }
    assert!(crate::node_reconcile::once(&f.domain, &NoBgSessions).await, "a starting node is judged on a second look");
    let phase = f.domain.state().unwrap().agents[&agent].phase.clone();
    assert!(phase.live() && phase != alondra_core::domain::NodePhase::Lost, "{phase:?}");
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
        email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0,
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
