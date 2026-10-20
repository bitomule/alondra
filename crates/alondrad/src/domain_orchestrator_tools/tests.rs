use super::*;
use crate::node_push::testing::Node;
use alondra_core::domain::{AttachmentRef, ModelChooser, TaskModel, TaskState};
use alondra_core::options::OptionsFile;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const SESSION: &str = "ab1234cd-0000-4000-8000-000000000000";

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

fn command(domain: &DomainRuntime, actor: Actor, command: Command) {
    run(domain, &actor, command).unwrap();
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn world() -> Fixture {
    let root = crate::test_support::Directory::new();
    let home = root.join("home");
    let project = root.join("App");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let domain = Arc::new(DomainRuntime::open(&home).unwrap());
    command(&domain, Actor::System, Command::RegisterAgent { agent_id: "agent-b".into(), name: "Instance B".into(), at: 0 });
    command(&domain, user(), Command::CreateRequest { request_id: "r".into(), body: "Make the editor readable".into(), auto_merge: false, maintenance: false, message: false });
    command(&domain, user(), Command::TitleRequest { request_id: "r".into(), title: "Readable editor".into() });
    command(&domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, task_id: "t".into(), request_id: "r".into(), title: "Editor colours".into(),
        agent_id: None, review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
    Fixture { root, home, project, domain }
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

struct FakeClaude {
    calls: Mutex<Vec<(String, Vec<String>)>>,
    loads_mod_in: Option<Arc<DomainRuntime>>,
    stopped: Mutex<Vec<String>>,
    removed: Mutex<Vec<String>>,
    during_run: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// The session and connection a stop drops, which the revival then sees lost.
    drops_on_stop: Mutex<Option<(String, u64)>>,
    respawned: Mutex<Vec<String>>,
    /// Why `claude stop` fails, when it does.
    stop_fails: Mutex<Option<String>>,
    /// Whether agent-b was on a turn at each stop.
    busy_at_stop: Mutex<Vec<bool>>,
    turn: std::time::Duration,
    /// Why `claude --bg` fails, when it does.
    run_fails: Mutex<Option<String>>,
    run_panics: bool,
}
impl FakeClaude {
    fn loading(domain: &Arc<DomainRuntime>) -> Self {
        Self { loads_mod_in: Some(domain.clone()), ..Self::without_mod() }
    }

    fn without_mod() -> Self {
        Self { calls: Mutex::new(vec![]), loads_mod_in: None, stopped: Mutex::new(vec![]), removed: Mutex::new(vec![]), during_run: Mutex::new(None),
            drops_on_stop: Mutex::new(None), respawned: Mutex::new(vec![]), stop_fails: Mutex::new(None), busy_at_stop: Mutex::new(vec![]),
            turn: std::time::Duration::from_millis(20), run_fails: Mutex::new(None), run_panics: false }
    }
}

const REVIVAL: crate::node_revival::Timing = crate::node_revival::Timing {
    grace: std::time::Duration::from_millis(10), settle: std::time::Duration::from_millis(10),
    pauses: [std::time::Duration::from_millis(1), std::time::Duration::from_millis(1)],
};
impl crate::orchestrate::Launcher for FakeClaude {
    async fn run(&self, cwd: &str, args: &[String]) -> Result<String, crate::orchestrate::Refusal> {
        self.calls.lock().unwrap().push((cwd.into(), args.to_vec()));
        assert!(!self.run_panics, "claude --bg panicked");
        if let Some(reason) = self.run_fails.lock().unwrap().take() {
            return Err((axum::http::StatusCode::BAD_GATEWAY, reason));
        }
        if let Some(domain) = &self.loads_mod_in {
            domain.live.mods.loaded_by(SESSION, args);
        }
        if let Some(hook) = self.during_run.lock().unwrap().take() {
            hook();
        }
        Ok("backgrounded · ab1234 · editor".into())
    }
    async fn stop_checked(&self, short: &str) -> Result<(), String> {
        if let Some(reason) = self.stop_fails.lock().unwrap().take() {
            return Err(reason);
        }
        self.stop(short).await;
        Ok(())
    }
    fn turn_deadline(&self) -> std::time::Duration {
        self.turn
    }
    async fn stop(&self, short: &str) {
        self.stopped.lock().unwrap().push(short.into());
        if let Some(domain) = &self.loads_mod_in {
            self.busy_at_stop.lock().unwrap().push(domain.live.node("agent-b").is_some_and(|node| node.busy));
        }
        let dropped = self.drops_on_stop.lock().unwrap().take();
        if let (Some((session, connection)), Some(domain)) = (dropped, &self.loads_mod_in) {
            domain.live.mods.disconnect(&session, connection);
            crate::node_revival::revive(domain, &session, self, &REVIVAL).await;
        }
    }
    async fn respawn(&self, short: &str) -> Result<(), String> {
        self.respawned.lock().unwrap().push(short.into());
        if let Some(domain) = &self.loads_mod_in {
            domain.connect_node(serde_json::from_value(json!({"session_id": "agent-b", "version": "2.1.287"})).unwrap(), 2);
        }
        Ok(())
    }
    async fn remove(&self, short: &str) {
        self.removed.lock().unwrap().push(short.into());
    }
    fn mods_deadline(&self) -> std::time::Duration {
        std::time::Duration::from_millis(20)
    }
    fn plugin(&self) -> Option<PathBuf> {
        Some("/synthetic/plugin".into())
    }
}

fn launch_body(f: &Fixture, extra: Value) -> LaunchBody {
    let mut body = json!({"name": "editor", "cwd": f.project.to_str().unwrap(), "task": "t", "reason": "Colour tokens in one file: sonnet"});
    body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    serde_json::from_value(body).unwrap()
}

fn settings_files(home: &Path) -> Vec<Value> {
    std::fs::read_dir(home.join("runtime/sessions")).map(|entries| entries.map(|entry| {
        serde_json::from_slice::<Value>(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap()
    }).filter(|settings| settings["env"].as_object().is_some_and(|env| !env.is_empty())).collect()).unwrap_or_default()
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
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    let choices = hard_choices();
    let asked = Mutex::new(None);
    let answer = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"effort": "light", "title": "Editor colours"})),
        |request| { *asked.lock().unwrap() = Some(request.clone()); Some(choices.clone()) }, &claude).await.unwrap();
    assert_eq!(answer["agent_id"], SESSION);
    assert_eq!(asked.into_inner().unwrap(), Some("r".into()), "the Task's Request supplies the choices");
    let calls = claude.calls.lock().unwrap();
    let (cwd, args) = &calls[0];
    assert_eq!(cwd, f.project.to_str().unwrap());
    assert!(args.windows(2).any(|pair| pair == ["--model", "opus"]) && args.windows(2).any(|pair| pair == ["--effort", "xhigh"]),
        "an effort the user chose is binding: {args:?}");
    assert!(!args.iter().any(|arg| arg == "--session-id"));
    let recorded = f.domain.state().unwrap().tasks[&TaskId::from("t")].model.clone();
    assert_eq!(recorded, Some(TaskModel { model: "opus".into(), chosen_by: ModelChooser::User }), "the user's effort sets the model");
    let prompt = args.last().unwrap();
    for expected in ["Editor colours", "Make the editor readable", "Crash on launch", "shot.png"] {
        assert!(prompt.contains(expected), "{expected} missing from {prompt}");
    }
    assert!(!prompt.contains("Task t of Request r"), "internal domain IDs must stay out of the node prompt: {prompt}");
    assert!(prompt.contains(&f.home.join("files/att-1").display().to_string()), "{prompt}");
    let state = f.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id, Some(AgentId::from(SESSION)));
    assert_eq!(state.agents[&AgentId::from(SESSION)].name, "Editor colours");
    let settings = settings_files(&f.home);
    assert_eq!(settings.len(), 1);
    assert_eq!(settings[0]["env"]["ALONDRA_DOMAIN_AGENT"], SESSION);
    let bearer = settings[0]["env"]["ALONDRA_DOMAIN_CAPABILITY"].as_str().unwrap();
    assert!(matches!(f.domain.resolve_bearer(bearer).unwrap(), crate::domain_authority::Principal::Agent { session_id, .. } if session_id == SESSION));
}

#[tokio::test]
async fn a_launch_in_a_repository_claude_does_not_trust_is_refused_before_claude_runs() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    let project = f.project.canonicalize().unwrap();
    let config = f.root.join(".claude.json");
    crate::claude_trust::use_config_for_test(&config);
    let trust = |accepted: bool| std::fs::write(&config, json!({"projects": {project.to_str().unwrap(): {"hasTrustDialogAccepted": accepted}}}).to_string()).unwrap();

    trust(false);
    let refused = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap_err();
    assert_eq!(refused.code, "workspace_untrusted");
    assert!(refused.message.contains(project.to_str().unwrap()), "{}", refused.message);
    assert!(claude.calls.lock().unwrap().is_empty());
    assert!(f.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id.is_none());
    let refusals = || f.domain.store.lock().unwrap().launch_refusals().unwrap();
    assert_eq!(refusals(), [(TaskId::from("t"), "workspace_untrusted".to_owned(), Some(project.to_str().unwrap().to_owned()))],
        "the Tasks list says why it did not start");

    trust(true);
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap();
    assert_eq!(claude.calls.lock().unwrap().len(), 1);
    assert!(refusals().is_empty(), "a task that started has nothing left to explain");
}

#[tokio::test]
async fn without_choices_the_orchestrators_model_and_effort_apply_and_the_prompt_can_be_given() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet", "effort": "medium", "prompt": "Only the dark theme"})), |_| None, &claude).await.unwrap();
    let args = claude.calls.lock().unwrap()[0].1.clone();
    assert!(args.windows(2).any(|pair| pair == ["--model", "sonnet"]) && args.windows(2).any(|pair| pair == ["--effort", "medium"]), "{args:?}");
    let recorded = f.domain.state().unwrap().tasks[&TaskId::from("t")].model.clone();
    assert_eq!(recorded, Some(TaskModel { model: "sonnet".into(), chosen_by: ModelChooser::Orchestrator }));
    assert!(args.last().unwrap().starts_with("Only the dark theme\n"), "{}", args.last().unwrap());
}

#[tokio::test]
async fn with_auto_effort_the_launch_needs_a_known_model_and_a_users_effort_refuses_one() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    let refusal = |body: Value, choices: Option<Choices>| {
        let (f, actor, claude) = (&f, &actor, &claude);
        async move { launch(&f.domain, &f.home, actor, launch_body(f, body), |_| choices.clone(), claude).await.unwrap_err() }
    };
    let missing = refusal(json!({}), None).await;
    assert_eq!(missing.code, "invalid_input");
    assert!(missing.message.contains("pass model") && missing.message.contains("haiku, sonnet, opus, fable"), "{}", missing.message);
    assert!(refusal(json!({"model": "gpt"}), None).await.message.contains("gpt is not a model"));
    assert!(refusal(json!({"model": "sonnet", "effort": "light"}), None).await.message.contains("light is not an effort"));
    let overruled = refusal(json!({"model": "opus"}), Some(hard_choices())).await;
    assert_eq!(overruled.code, "invalid_input");
    assert!(overruled.message.contains("the user chose effort hard"), "{}", overruled.message);
    assert!(claude.calls.lock().unwrap().is_empty(), "nothing launched");
    assert!(f.domain.state().unwrap().tasks[&TaskId::from("t")].model.is_none());
}

#[tokio::test]
async fn a_model_alias_from_options_toml_is_launched_as_what_it_stands_for() {
    let f = world();
    std::fs::write(f.home.join("options.toml"), "[models]\nbig = \"claude-opus-5-5[1m]\"\n").unwrap();
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "big"})), |_| None, &claude).await.unwrap();
    let args = claude.calls.lock().unwrap()[0].1.clone();
    assert!(args.windows(2).any(|pair| pair == ["--model", "claude-opus-5-5[1m]"]), "{args:?}");
    assert!(!args.iter().any(|arg| arg == "--effort"), "no effort unless the orchestrator names one: {args:?}");
}

#[tokio::test]
async fn a_launch_is_refused_for_a_closed_or_taken_task_and_a_session_without_its_mod_leaves_nothing() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let unlisted = FakeClaude::without_mod();
    let error = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &unlisted).await.unwrap_err();
    assert_eq!(error.code, "launch_failed");
    assert!(settings_files(&f.home).is_empty());
    assert_eq!(f.domain.state().unwrap().agents.len(), 1);
    assert!(f.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id.is_none());

    command(&f.domain, actor.clone(), Command::AssignTask { task_id: "t".into(), agent_id: "agent-b".into(), model: None });
    crate::domain_agent_lifecycle::move_instance(&f.domain, &"agent-b".into(), alondra_core::domain::NodeEvent::TurnStarted,
        &crate::domain_agent_lifecycle::prepare_event(&f.domain, "start-b").unwrap()).unwrap();
    let claude = FakeClaude::loading(&f.domain);
    let taken = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap_err();
    assert_eq!(taken.code, "conflict");
    command(&f.domain, user(), Command::CancelTask { task_id: "t".into() });
    let closed = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap_err();
    assert_eq!(closed.code, "conflict");
    assert!(claude.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_session_whose_mod_never_loads_is_stopped_and_the_launch_fails_without_an_agent() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::without_mod();
    let error = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap_err();
    assert_eq!(error.code, "launch_failed");
    assert!(error.message.starts_with("The session's mod did not report within"), "{}", error.message);
    assert_eq!(*claude.stopped.lock().unwrap(), ["ab1234"], "the session is stopped, not left working unseen");
    assert!(settings_files(&f.home).is_empty());
    let state = f.domain.state().unwrap();
    assert!(!state.agents.contains_key(&AgentId::from(SESSION)));
    assert!(state.tasks[&TaskId::from("t")].agent_id.is_none(), "the task stays free for another launch");
}

fn give(f: &Fixture, task: &str, agent: &str) {
    command(&f.domain, user(), Command::AssignTask { task_id: task.into(), agent_id: agent.into(), model: None });
}

#[tokio::test]
async fn a_message_with_a_task_reaches_only_that_tasks_node() {
    let f = world();
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
    let f = world();
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
    let f = world();
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
    let f = world();
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
    let f = world();
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

/// Claude measures the week at each of `week_percents`, all with the same reset, as it reports a
/// window that has not ended.
fn measured(domain: &DomainRuntime, week_percents: &[f64]) {
    let reset = crate::log::local_iso(alondra_core::now_millis().div_euclid(1000) as libc::time_t + 5 * 86_400, 0);
    for week_percent in week_percents {
        let envelope = serde_json::from_value(json!({"session_id": "m", "event": "session.measure",
            "rate_limits": [{"kind": "seven_day", "percent_used": week_percent, "resets_at": reset}]})).unwrap();
        domain.report(envelope, alondra_core::now_millis());
    }
}

#[tokio::test]
async fn going_over_todays_slice_launches_without_a_warning_and_asks_the_user_once_a_day() {
    let f = world();
    measured(&f.domain, &[12.0, 53.0]);
    assert!(f.domain.live.mods.usage(alondra_core::now_millis()).unwrap().over_pace, "41% spent against a ~18% slice");
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    let answer = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap();
    assert_eq!(answer["agent_id"], SESSION, "over pace no longer blocks");
    assert!(answer.get("warning").is_none(), "the orchestrator is not asked to relay it: {answer}");
    assert_eq!(claude.calls.lock().unwrap().len(), 1);

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
    let answer = launch(&within.domain, &within.home, &the_orchestrator(&within.domain), launch_body(&within, json!({"over_pace_ok": true, "model": "sonnet"})),
        |_| None, &FakeClaude::loading(&within.domain)).await.unwrap();
    assert!(answer.get("warning").is_none(), "over_pace_ok is still accepted: {answer}");
    assert!(within.domain.state().unwrap().interactions.keys().all(|id| !id.0.starts_with("over-pace-")), "within its slice: no card");
}

#[tokio::test]
async fn a_launch_is_refused_while_usage_is_exhausted_and_goes_ahead_once_it_is_back() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    let now = alondra_core::now_millis();
    f.domain.live.usage_pause.exhaust(Some(now + 3_600_000), now);
    let refused = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap_err();
    assert_eq!(refused.code, "usage_exhausted");
    assert!(refused.message.contains("Claude usage ran out"), "{}", refused.message);
    assert!(claude.calls.lock().unwrap().is_empty());
    assert!(f.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id.is_none(), "the task is left as it was");

    f.domain.live.usage_pause.resume(alondra_core::now_millis());
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap();
    assert_eq!(claude.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_launch_is_refused_while_the_disk_is_nearly_full_and_goes_ahead_once_it_has_space() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    f.domain.live.machine_watch.set_disk_low(true);
    let refused = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap_err();
    assert_eq!(refused.code, "disk_low");
    assert!(refused.message.starts_with("The disk is nearly full: Alondra starts no node until there is space again"), "{}", refused.message);
    assert!(refused.message.ends_with("t stays as it is and can be launched after that"), "{}", refused.message);
    assert!(claude.calls.lock().unwrap().is_empty());
    assert!(f.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id.is_none(), "the task is left as it was");

    f.domain.live.machine_watch.set_disk_low(false);
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap();
    assert_eq!(claude.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_task_whose_node_the_user_closed_is_relaunched_for_that_task_in_the_closed_nodes_folder() {
    let f = world();
    command(&f.domain, user(), Command::AssignTask { task_id: "t".into(), agent_id: "agent-b".into(), model: None });
    command(&f.domain, Actor::Agent { agent_id: "agent-b".into() }, Command::StartTask { task_id: "t".into() });
    f.domain.connect_node(serde_json::from_value(json!({"session_id": "agent-b", "version": "2.1.287", "cwd": f.project.to_str().unwrap()})).unwrap(), 1);
    let claude = FakeClaude::loading(&f.domain);
    let refused = relaunch(&f.domain, &f.home, &user(), &"t".into(), &claude).await.unwrap_err();
    assert_eq!(refused.code, "conflict", "a task whose node was not closed is not relaunched");
    assert!(claude.calls.lock().unwrap().is_empty());

    crate::node_revival::user_closed(&f.domain, "agent-b").unwrap();
    let state = f.domain.state().unwrap();
    assert_eq!((state.tasks[&"t".into()].state, state.tasks[&"t".into()].agent_id.clone()), (TaskState::NotStarted, None));
    assert_eq!(f.domain.live.closed_task_ids(), ["t"]);

    let answer = relaunch(&f.domain, &f.home, &user(), &"t".into(), &claude).await.unwrap();
    assert_eq!(answer["agent_id"], SESSION);
    let calls = claude.calls.lock().unwrap();
    let (cwd, args) = &calls[0];
    assert_eq!(cwd, f.project.to_str().unwrap(), "the new node starts where the closed one worked");
    assert!(args.windows(2).any(|pair| pair == ["--name", "Instance B"]), "{args:?}");
    let prompt = args.last().unwrap();
    assert!(prompt.contains("Editor colours") && prompt.contains("Make the editor readable"), "the mission comes with it: {prompt}");
    let state = f.domain.state().unwrap();
    assert_eq!(state.tasks[&"t".into()].agent_id, Some(SESSION.into()));
    assert_eq!(state.tasks[&"t".into()].model.as_ref().map(|m| (m.model.as_str(), m.chosen_by)), Some(("sonnet", ModelChooser::Orchestrator)),
        "an auto-effort task that never recorded a model restarts on the starting model");
    assert!(f.domain.live.closed_task_ids().is_empty(), "the task no longer waits for a relaunch");
    drop(calls);
    let again = relaunch(&f.domain, &f.home, &user(), &"t".into(), &claude).await.unwrap_err();
    assert_eq!(again.code, "conflict");
}

#[tokio::test]
async fn a_launch_with_an_approach_writes_how_to_work_into_the_mission() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"effort": "light", "approach": "subagents"})), |_| Some(hard_choices()), &claude).await.unwrap();
    let calls = claude.calls.lock().unwrap();
    let prompt = calls[0].1.last().unwrap();
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
    let f = world();
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
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    for reason in [json!(null), json!("   ")] {
        let refused = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet", "reason": reason})), |_| None, &claude).await.unwrap_err();
        assert_eq!(refused.code, "invalid_input");
        assert!(refused.message.contains("pass reason"), "{}", refused.message);
    }
    assert!(claude.calls.lock().unwrap().is_empty(), "nothing launched");
    assert!(decisions(&f.domain).is_empty());

    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"reason": null})), |_| Some(hard_choices()), &claude).await.unwrap();
    assert_eq!(decisions(&f.domain), [row("launch", Some("opus"), Some("xhigh"), Some("solo"), None)],
        "the user's effort needs no reason");
    assert_eq!(f.domain.store.lock().unwrap().model_decision_meta().unwrap(), [(SESSION.to_owned(), "user".to_owned(), "Editor colours".to_owned())]);
}

#[tokio::test]
async fn every_launch_records_the_model_effort_approach_and_reason_the_orchestrator_chose() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet", "effort": "medium", "approach": "subagents"})), |_| None, &claude).await.unwrap();
    assert_eq!(decisions(&f.domain), [row("launch", Some("sonnet"), Some("medium"), Some("subagents"), Some("Colour tokens in one file: sonnet"))]);
    assert_eq!(f.domain.store.lock().unwrap().model_decision_meta().unwrap(), [(SESSION.to_owned(), "orchestrator".to_owned(), "Editor colours".to_owned())]);
}

#[tokio::test]
async fn a_message_with_an_approach_is_recorded_and_a_plain_one_is_not() {
    let f = world();
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
    relaunch(&f.domain, &f.home, &user(), &"t".into(), &FakeClaude::loading(&f.domain)).await.unwrap();
    assert_eq!(decisions(&f.domain), [row("relaunch", Some("sonnet"), None, Some("solo"), None)]);
}

#[tokio::test]
async fn closing_a_launched_node_keeps_its_cost_and_the_fullest_its_context_got() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap();
    for (context, cost) in [(json!({"window": 200000, "percent": 62.5}), 0.8), (json!({"window": 200000, "tokens": 50000}), 1.25)] {
        f.domain.report(serde_json::from_value(json!({"session_id": SESSION, "event": "session.measure", "context": context, "cost_usd": cost})).unwrap(), 2);
    }
    f.domain.report(serde_json::from_value(json!({"session_id": SESSION, "event": "model", "model": "claude-sonnet-5-5"})).unwrap(), 3);
    assert!(f.domain.store.lock().unwrap().node_cost(SESSION).unwrap().is_none());

    crate::node_revival::user_closed(&f.domain, SESSION).unwrap();
    let kept = f.domain.store.lock().unwrap().node_cost(SESSION).unwrap().expect("the closed node's cost is kept");
    assert_eq!((kept.task_id.as_deref(), kept.model.as_deref(), kept.cost_usd, kept.context_peak_percent),
        (Some("t"), Some("claude-sonnet-5-5"), Some(1.25), Some(62.5)), "the task comes from its launch, though closing freed it");
    assert!(kept.started_at.is_some_and(|started| kept.ended_at >= started));
}

#[tokio::test]
async fn a_launch_of_a_follow_up_task_carries_the_earlier_result_and_pull_request() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
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

    let claude = FakeClaude::loading(&f.domain);
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"task": "t2", "model": "sonnet"})), |_| None, &claude).await.unwrap();
    let calls = claude.calls.lock().unwrap();
    let prompt = calls[0].1.last().unwrap();
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
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    let plan = crate::launch_plans::LaunchPlan {
        name: "app/editor".into(), cwd: f.project.to_str().unwrap().into(), mission: Some("Make the colours tokens".into()),
        model: Some("sonnet".into()), effort: None, approach: Some(alondra_core::options::Approach::Solo),
        reason: Some("Colour tokens in one file".into()), role: None,
    };
    f.domain.store.lock().unwrap().set_launch_plan(&"t".into(), &plan, 1).unwrap();
    let body: LaunchBody = serde_json::from_value(json!({"task": "t"})).unwrap();
    launch(&f.domain, &f.home, &actor, body, |_| None, &claude).await.unwrap();
    let calls = claude.calls.lock().unwrap();
    let (cwd, args) = &calls[0];
    assert_eq!(cwd, f.project.to_str().unwrap());
    assert!(args.windows(2).any(|pair| pair == ["--name", "app/editor"]), "{args:?}");
    assert!(args.last().unwrap().contains("Make the colours tokens"), "the plan's mission is the node's mission");
    assert_eq!(decisions(&f.domain), [row("launch", Some("sonnet"), None, Some("solo"), Some("Colour tokens in one file"))]);
}

#[tokio::test]
async fn a_launch_of_a_task_left_without_a_plan_needs_a_name_and_a_folder() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    f.domain.store.lock().unwrap().forget_launch_plan(&"t".into()).unwrap();
    let body: LaunchBody = serde_json::from_value(json!({"task": "t", "model": "sonnet", "reason": "x"})).unwrap();
    let refused = launch(&f.domain, &f.home, &actor, body, |_| None, &claude).await.unwrap_err();
    assert_eq!(refused.code, "invalid_input");
    assert!(refused.message.contains("name and cwd are required"), "{}", refused.message);
    assert!(claude.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_refused_assignment_after_a_launch_stops_and_removes_the_new_session() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    let domain = f.domain.clone();
    *claude.during_run.lock().unwrap() = Some(Box::new(move || {
        command(&domain, user(), Command::CancelTask { task_id: "t".into() });
    }));
    let refused = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap_err();
    assert_eq!(refused.code, "conflict");
    assert_eq!(*claude.stopped.lock().unwrap(), ["ab1234"]);
    assert_eq!(*claude.removed.lock().unwrap(), ["ab1234"]);
    assert!(settings_files(&f.home).is_empty(), "its settings are released");
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id, None);
}

#[tokio::test]
async fn a_task_in_progress_whose_node_died_is_relaunched_and_assigned_to_the_new_node() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    give(&f, "t", "agent-b");
    command(&f.domain, Actor::Agent { agent_id: "agent-b".into() }, Command::StartTask { task_id: "t".into() });
    command(&f.domain, Actor::System, Command::MoveNode { agent_id: "agent-b".into(), event: alondra_core::domain::NodeEvent::SessionLost, at: 0 });
    let claude = FakeClaude::loading(&f.domain);
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap();
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id, Some(AgentId::from(SESSION)));
    assert!(claude.removed.lock().unwrap().is_empty(), "the new session is kept");
}

#[tokio::test]
async fn the_changes_the_user_asked_for_reach_the_node_that_takes_the_task_over() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
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
    let claude = FakeClaude::loading(&f.domain);
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap();
    let prompt = claude.calls.lock().unwrap()[0].1.last().unwrap().clone();
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
    let actor = the_orchestrator(&f.domain);
    let connection = connected_node_on_the_task(&f);
    let claude = FakeClaude::loading(&f.domain);
    *claude.drops_on_stop.lock().unwrap() = Some(("agent-b".into(), connection));
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    launch(&f.domain, &f.home, &actor, body, |_| None, &claude).await.unwrap();
    assert_eq!(*claude.stopped.lock().unwrap(), ["agent-b"]);
    assert!(claude.respawned.lock().unwrap().is_empty(), "the revival saw the stop and did not restart the old node");

    crate::node_revival::revive(&f.domain, "agent-b", &claude, &REVIVAL).await;
    assert!(claude.respawned.lock().unwrap().is_empty(), "nor later, when the daemon reports every known session as lost");
}

#[tokio::test]
async fn replacing_a_tasks_node_stops_it_keeps_its_folder_and_gives_the_task_to_the_new_model() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    connected_node_on_the_task(&f);
    let claude = FakeClaude::loading(&f.domain);
    let taken = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "opus"})), |_| None, &claude).await.unwrap_err();
    assert!(taken.message.contains("already has a connected agent"), "without replace a live node keeps its task: {}", taken.message);

    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus", "effort": "high", "prompt": "Finish the tokens"})).unwrap();
    let answer = launch(&f.domain, &f.home, &actor, body, |_| None, &claude).await.unwrap();
    assert_eq!(answer["agent_id"], SESSION);
    assert_eq!(*claude.stopped.lock().unwrap(), ["agent-b"], "the old session is stopped");
    assert!(claude.removed.lock().unwrap().is_empty(), "never claude rm: its worktree and uncommitted files stay");
    let calls = claude.calls.lock().unwrap();
    let (cwd, args) = &calls[0];
    assert_eq!(cwd, f.project.to_str().unwrap(), "the new node starts where the old one worked");
    assert!(args.windows(2).any(|pair| pair == ["--name", "Instance B"]), "{args:?}");
    assert!(args.windows(2).any(|pair| pair == ["--model", "opus"]) && args.windows(2).any(|pair| pair == ["--effort", "high"]), "{args:?}");
    assert!(args.last().unwrap().starts_with("Finish the tokens"), "{}", args.last().unwrap());
    let state = f.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id, Some(AgentId::from(SESSION)));
    assert_eq!(state.tasks[&TaskId::from("t")].model.as_ref().map(|m| m.model.as_str()), Some("opus"));
    assert!(!state.agents[&AgentId::from("agent-b")].phase.holds_slot(), "the old node is disconnected");
    assert!(f.domain.live.is_closed("agent-b"), "and is not brought back");
    assert!(f.domain.live.closed_task_ids().is_empty());
}

#[tokio::test]
async fn a_replacement_that_cannot_launch_leaves_the_old_node_working() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    connected_node_on_the_task(&f);
    let claude = FakeClaude::loading(&f.domain);
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus", "cwd": "/no/such/folder"})).unwrap();
    let refused = launch(&f.domain, &f.home, &actor, body, |_| None, &claude).await.unwrap_err();
    assert_eq!(refused.code, "invalid_input");
    assert!(claude.stopped.lock().unwrap().is_empty() && claude.calls.lock().unwrap().is_empty());
    let state = f.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id, Some(AgentId::from("agent-b")));
    assert!(state.agents[&AgentId::from("agent-b")].phase.holds_slot());

    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, task_id: "free".into(), request_id: "r".into(), title: "Free".into(),
        agent_id: None, review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
    let body = serde_json::from_value(json!({"task": "free", "replace": true, "model": "opus", "reason": "Needs opus", "name": "x", "cwd": f.project.to_str().unwrap()})).unwrap();
    let nothing = launch(&f.domain, &f.home, &actor, body, |_| None, &claude).await.unwrap_err();
    assert!(nothing.message.contains("no node to replace"), "{}", nothing.message);
}

#[tokio::test]
async fn a_replacement_works_in_a_worktree_of_a_trusted_repository() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
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
    let claude = FakeClaude::loading(&f.domain);
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus", "cwd": linked.to_str().unwrap()})).unwrap();
    launch(&f.domain, &f.home, &actor, body, |_| None, &claude).await.unwrap();
    assert_eq!(claude.calls.lock().unwrap()[0].0, linked.to_str().unwrap());
}

#[tokio::test]
async fn a_replacement_whose_stop_fails_launches_nothing_and_the_old_node_keeps_the_task() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    connected_node_on_the_task(&f);
    let claude = FakeClaude::loading(&f.domain);
    *claude.stop_fails.lock().unwrap() = Some("claude agents still shows agent-b running after claude stop".into());
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    let refused = launch(&f.domain, &f.home, &actor, body, |_| None, &claude).await.unwrap_err();
    assert_eq!(refused.code, "replace_failed");
    assert!(refused.message.contains("still shows agent-b running"), "the orchestrator gets the reason: {}", refused.message);
    assert!(claude.calls.lock().unwrap().is_empty(), "no second node is launched into the worktree");
    let state = f.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id, Some(AgentId::from("agent-b")), "the old node keeps its task");
    assert_eq!(state.tasks[&TaskId::from("t")].state, TaskState::Running);
    assert_eq!(state.agents.len(), 1, "no new agent was registered");
    assert!(state.agents[&AgentId::from("agent-b")].phase.holds_slot() && !f.domain.live.is_closed("agent-b"));
    assert!(settings_files(&f.home).is_empty(), "the new session's settings are discarded");
    assert!(!refused.message.contains("keeps its node"), "{}", refused.message);
}

fn release_reasons(f: &Fixture) -> Vec<String> {
    let store = f.domain.store.lock().unwrap();
    let mut query = store.conn_for_test().prepare(
        "SELECT json_extract(command,'$.reason') FROM command_audit WHERE json_extract(command,'$.kind')='release_task' ORDER BY revision").unwrap();
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
    let actor = the_orchestrator(&f.domain);
    connected_node_on_the_task(&f);
    let claude = FakeClaude::loading(&f.domain);
    let body = || serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    launch(&f.domain, &f.home, &actor, body(), |_| None, &claude).await.unwrap();
    assert_eq!(f.domain.inherited_worktree(SESSION).unwrap(), None, "the user's project folder is never Alondra's to delete");

    let g = world();
    let actor = the_orchestrator(&g.domain);
    connected_node_on_the_task(&g);
    let dir = g.project.to_str().unwrap();
    g.domain.set_inherited_worktree("agent-b", dir, "agent-a").unwrap();
    let claude = FakeClaude::loading(&g.domain);
    launch(&g.domain, &g.home, &actor, body(), |_| None, &claude).await.unwrap();
    assert_eq!(g.domain.inherited_worktree(SESSION).unwrap().as_deref(), Some(dir), "a worktree passed down a chain of replacements stays Alondra's");
}

/// `claude stop` can report a failure for a session that ended anyway: the replacement goes on.
#[tokio::test]
async fn a_replacement_whose_stop_errs_but_whose_node_ended_finishes() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    connected_node_on_the_task(&f);
    f.domain.live.mods.set_job("agent-b", None, Some(f.root.join("jobs/gone")));
    let claude = FakeClaude::loading(&f.domain);
    *claude.stop_fails.lock().unwrap() = Some("claude stop timed out".into());
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    let answer = launch(&f.domain, &f.home, &actor, body, |_| None, &claude).await.unwrap();
    assert_eq!(answer["agent_id"], SESSION);
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id, Some(AgentId::from(SESSION)));
    assert_eq!(release_reasons(&f), ["node replaced by the orchestrator"]);
}

/// The node a replacement could not stop ends later: the task is freed for that reason, never as
/// closed by the user, and the orchestrator hears it.
#[tokio::test]
async fn a_node_that_ends_after_its_replacement_stopped_short_frees_its_task_with_the_real_reason() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    connected_node_on_the_task(&f);
    let claude = FakeClaude::loading(&f.domain);
    *claude.stop_fails.lock().unwrap() = Some("claude agents still shows agent-b running after claude stop".into());
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    launch(&f.domain, &f.home, &actor, body, |_| None, &claude).await.unwrap_err();

    crate::node_revival::user_closed(&f.domain, "agent-b").unwrap();
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id, None, "the task is free");
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
    let actor = the_orchestrator(&f.domain);
    connected_node_on_the_task(&f);
    on_a_turn(&f);
    let claude = FakeClaude { turn: std::time::Duration::from_secs(5), ..FakeClaude::loading(&f.domain) };
    let domain = f.domain.clone();
    let ends = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        domain.live.observe(&json!({"session_id": "agent-b", "hook_event_name": "Stop"}));
    });
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    launch(&f.domain, &f.home, &actor, body, |_| None, &claude).await.unwrap();
    ends.await.unwrap();
    assert_eq!(*claude.busy_at_stop.lock().unwrap(), [false], "claude stop waited for the turn to end");
}

#[tokio::test]
async fn a_node_whose_turn_does_not_end_is_interrupted_before_it_is_stopped() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    connected_node_on_the_task(&f);
    let node = Node::connect(&f.domain, "agent-b");
    on_a_turn(&f);
    let claude = FakeClaude::loading(&f.domain);
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    launch(&f.domain, &f.home, &actor, body, |_| None, &claude).await.unwrap();
    assert_eq!(node.interrupts(), 1, "past the deadline the turn is interrupted first");
    assert_eq!(claude.stopped.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn the_replacing_node_is_told_to_continue_in_the_worktree_without_making_another() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    connected_node_on_the_task(&f);
    let repo = f.root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-q", "-b", "main"]);
    git_in(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "start"]);
    git_in(&repo, &["worktree", "add", "-q", "-b", "worktree-t-cards-3", f.project.to_str().unwrap()]);
    let claude = FakeClaude::loading(&f.domain);
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    launch(&f.domain, &f.home, &actor, body, |_| None, &claude).await.unwrap();
    let prompt = claude.calls.lock().unwrap()[0].1.last().unwrap().clone();
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

fn same(a: &Path, b: &Path) -> bool {
    a.canonicalize().unwrap() == b.canonicalize().unwrap()
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
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap();
    let (cwd, args) = claude.calls.lock().unwrap()[0].clone();
    assert!(same(Path::new(&cwd), &earlier), "the new node starts in the task's worktree, not {cwd}");
    assert!(earlier.join("half-done.rs").exists(), "with the earlier node's work in it");
    assert!(args.last().unwrap().contains("You continue in"), "and is told so: {}", args.last().unwrap());
    assert!(same(Path::new(&f.domain.inherited_worktree(SESSION).unwrap().unwrap()), &earlier), "Alondra deletes it with the task");
}

#[tokio::test]
async fn a_first_launch_in_a_main_checkout_gets_a_worktree_of_its_own_and_the_next_launch_reuses_it() {
    let f = world();
    main_checkout(&f);
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap();
    let made = f.project.join(".claude/worktrees/t");
    let (cwd, args) = claude.calls.lock().unwrap()[0].clone();
    assert!(same(Path::new(&cwd), &made), "{cwd}");
    assert!(args.last().unwrap().contains("the worktree Alondra made for this task"), "{}", args.last().unwrap());
    let recorded = f.domain.task_worktree(&TaskId::from("t")).unwrap().unwrap();
    assert!(same(Path::new(&recorded), &made));
    crate::node_revival::take_out(&f.domain, SESSION, "node closed by the user", None).unwrap();
    let again = FakeClaude::loading(&f.domain);
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &again).await.unwrap();
    assert!(same(Path::new(&again.calls.lock().unwrap()[0].0), &made), "the relaunch works in the same worktree");
}

#[tokio::test]
async fn no_node_is_launched_in_a_main_checkout_alondra_cannot_isolate() {
    let f = world();
    std::fs::create_dir_all(f.project.join(".git")).unwrap();
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    let refused = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap_err();
    assert_eq!(refused.code, "main_checkout", "{}", refused.message);
    assert!(claude.calls.lock().unwrap().is_empty(), "claude never ran");
}

#[test]
fn a_stopped_session_is_one_claude_agents_no_longer_shows_running() {
    let listing = json!([{"id": "agent-b", "state": "idle"}, {"id": "old00000", "state": "stopped"}, {"id": "done0000", "state": "done"}]).to_string();
    assert!(crate::orchestrate::still_running(&listing, "agent-b"));
    for gone in ["old00000", "done0000", "missing0"] {
        assert!(!crate::orchestrate::still_running(&listing, gone), "{gone}");
    }
}

/// What `alondra models --json` shows after a sonnet task launched by the orchestrator goes
/// through a review that asks for changes twice, every call made over the daemon's own routes.
#[tokio::test]
async fn a_review_that_asks_for_changes_twice_shows_its_rounds_and_serious_findings_in_the_model_report() {
    let scratch_directory = crate::test_support::Directory::new();
    let root = scratch_directory.parent().unwrap().join(format!("alondra-review-rounds-{}", crate::auth::random_hex(8)));
    let (home, project) = (root.join("home"), root.join("App"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
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

    let orchestrator = the_orchestrator(&domain);
    let claude = FakeClaude::loading(&domain);
    let body = serde_json::from_value(json!({"name": "editor", "cwd": project.to_str().unwrap(), "task": "t",
        "model": "sonnet", "effort": "medium", "reason": "Colour tokens in one file: sonnet"})).unwrap();
    launch(&domain, &home, &orchestrator, body, |_| None, &claude).await.unwrap();
    let worker = settings_files(&home)[0]["env"]["ALONDRA_DOMAIN_CAPABILITY"].as_str().unwrap().to_owned();
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
    let _ = std::fs::remove_dir_all(&root);
}

async fn until_told(f: &Fixture, prefix: &str) -> Vec<String> {
    for _ in 0..500 {
        let told = told_orchestrator(f, prefix);
        if !told.is_empty() {
            return told;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the orchestrator was never told how the replacement ended");
}

fn replace_body() -> LaunchBody {
    serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap()
}

async fn until_replaced(f: &Fixture) {
    for _ in 0..500 {
        if f.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id == Some(AgentId::from(SESSION)) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the replacement never finished");
}

/// Waiting for the old node's turn can outlast the call: the orchestrator gets its answer at once,
/// and a replacement that finishes tells it nothing more.
#[tokio::test]
async fn a_replacement_answers_before_the_old_node_stops_and_finishes_without_a_notice() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    connected_node_on_the_task(&f);
    on_a_turn(&f);
    let claude = Arc::new(FakeClaude { turn: std::time::Duration::from_secs(5), ..FakeClaude::loading(&f.domain) });
    let answer = launch_replacing(f.domain.clone(), f.home.clone(), actor.clone(), replace_body(), |_| None, claude.clone()).await.unwrap();
    assert_eq!(answer["replacing"], true, "{answer}");
    assert!(answer["message"].as_str().unwrap().contains("you are told only if it did not start"), "{answer}");
    assert!(claude.stopped.lock().unwrap().is_empty(), "answered while the old node is still on its turn");

    let again = launch_replacing(f.domain.clone(), f.home.clone(), actor, replace_body(), |_| None, claude.clone()).await.unwrap_err();
    assert_eq!(again.code, "conflict", "a second launch waits for the replacement under way: {}", again.message);

    f.domain.live.observe(&json!({"session_id": "agent-b", "hook_event_name": "Stop"}));
    until_replaced(&f).await;
    assert!(told_orchestrator(&f, "replaced-").is_empty(), "a finished replacement is no news");
    for _ in 0..500 {
        if f.domain.live.begin_launch("t") {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the launch guard is released once it ends");
}

#[tokio::test]
async fn a_replacement_that_fails_after_answering_tells_the_orchestrator_why() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    connected_node_on_the_task(&f);
    let claude = Arc::new(FakeClaude::loading(&f.domain));
    *claude.stop_fails.lock().unwrap() = Some("claude agents still shows agent-b running after claude stop".into());
    let answer = launch_replacing(f.domain.clone(), f.home.clone(), actor, replace_body(), |_| None, claude).await.unwrap();
    assert_eq!(answer["replacing"], true);
    let told = until_told(&f, "replaced-").await;
    assert!(told[0].contains("did not finish") && told[0].contains("still shows agent-b running"), "{}", told[0]);
    assert!(!told[0].contains("keeps its node"), "{}", told[0]);
}

#[tokio::test]
async fn a_replacement_refused_before_stopping_anything_answers_with_the_refusal() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    connected_node_on_the_task(&f);
    let claude = Arc::new(FakeClaude::loading(&f.domain));
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus", "cwd": "/no/such/folder"})).unwrap();
    let refused = launch_replacing(f.domain.clone(), f.home.clone(), actor, body, |_| None, claude.clone()).await.unwrap_err();
    assert_eq!(refused.code, "invalid_input");
    assert!(claude.stopped.lock().unwrap().is_empty());
    assert!(f.domain.live.begin_launch("t"), "the launch guard is released");
}

/// Once the old node is stopped the task has no node: a launch that fails after that says so.
#[tokio::test]
async fn a_replacement_whose_new_node_does_not_start_says_the_task_has_no_node() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    connected_node_on_the_task(&f);
    let claude = Arc::new(FakeClaude::loading(&f.domain));
    *claude.run_fails.lock().unwrap() = Some("claude --bg did not respond within 60 s".into());
    launch_replacing(f.domain.clone(), f.home.clone(), actor.clone(), replace_body(), |_| None, claude.clone()).await.unwrap();
    let told = until_told(&f, "replaced-").await;
    assert!(told[0].contains("did not respond within 60 s")
        && told[0].contains("The node it replaced was stopped, so “Editor colours” (t) has no node now: start one with launch (task t), without replace."),
        "{}", told[0]);
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id, None);

    let g = world();
    let actor = the_orchestrator(&g.domain);
    connected_node_on_the_task(&g);
    let refused = launch(&g.domain, &g.home, &actor, replace_body(), |_| None, &FakeClaude::without_mod()).await.unwrap_err();
    assert!(refused.message.contains("The session's mod did not report within") && refused.message.contains("has no node now"), "{}", refused.message);
}

/// The call can be dropped as soon as the replacement starts: the replacement still finishes.
#[tokio::test]
async fn a_dropped_replace_call_still_finishes_the_replacement() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    connected_node_on_the_task(&f);
    let claude = Arc::new(FakeClaude::loading(&f.domain));
    let call = launch_replacing(f.domain.clone(), f.home.clone(), actor, replace_body(), |_| None, claude);
    assert!(tokio::time::timeout(std::time::Duration::ZERO, call).await.is_err(), "the call is dropped before it answers");
    until_replaced(&f).await;
}

/// A panic in the replacement gives the launch guard back and tells the orchestrator.
#[tokio::test]
async fn a_replacement_that_panics_releases_the_task_and_tells_the_orchestrator() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    connected_node_on_the_task(&f);
    let claude = Arc::new(FakeClaude { run_panics: true, ..FakeClaude::loading(&f.domain) });
    launch_replacing(f.domain.clone(), f.home.clone(), actor, replace_body(), |_| None, claude).await.unwrap();
    let told = until_told(&f, "replaced-").await;
    assert!(told[0].contains("stopped unexpectedly"), "{}", told[0]);
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
    let actor = the_orchestrator(&f.domain);
    connected_node(&f, old);
    let claude = FakeClaude::loading(&f.domain);
    launch(&f.domain, &f.home, &actor, replace_body(), |_| None, &claude).await.unwrap();
    assert_eq!(claude.calls.lock().unwrap()[0].0, made.to_str().unwrap(), "the new node starts in that worktree");
    assert_eq!(f.domain.inherited_worktree(SESSION).unwrap().as_deref(), made.to_str(), "and Alondra may delete it with the new node");
}

/// What Alondra keeps about a session only while it lives goes when it ends.
#[tokio::test]
async fn an_ended_session_leaves_no_inherited_worktree_or_aborted_replace_behind() {
    let f = world();
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
    let actor = the_orchestrator(&f.domain);
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

    let claude = FakeClaude::loading(&f.domain);
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"task": "build", "model": "sonnet"})), |_| None, &claude).await.unwrap();
    let calls = claude.calls.lock().unwrap();
    let prompt = calls[0].1.last().unwrap();
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
    let claude = FakeClaude::loading(&f.domain);
    let held = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap_err();
    assert_eq!(held.code, "plan_review_pending");
    assert!(held.message.contains("Editor fonts"), "{}", held.message);
    assert!(claude.calls.lock().unwrap().is_empty(), "Claude never runs for a held task");
    command(&f.domain, actor.clone(), Command::SettlePlanReview { review_id: "font-choice".into(), task_id: "t".into(),
        outcome: alondra_core::domain::PlanReviewOutcome::Unchanged { reason: "Colours do not depend on the typeface".into() } });
    command(&f.domain, actor.clone(), Command::SetDependencies { task_id: "t".into(), dependencies: Default::default(), stack: None });
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap();
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id, Some(AgentId::from(SESSION)));
}

#[tokio::test]
async fn a_decision_that_arrives_while_its_dependent_starts_stops_that_node() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    decided_on(&f);
    command(&f.domain, actor.clone(), Command::SetDependencies { task_id: "t".into(), dependencies: Default::default(), stack: None });
    command(&f.domain, actor.clone(), Command::SetRelations { task_id: "t".into(), delivered_dependencies: [TaskId::from("fonts")].into(), blocks: Default::default() });
    let claude = FakeClaude::loading(&f.domain);
    let domain = f.domain.clone();
    *claude.during_run.lock().unwrap() = Some(Box::new(move || {
        let state = domain.state().unwrap();
        domain.execute(&Actor::User { user_id: "u".into() }, &CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: "decide".into(), expected_revision: state.revision, command: Command::AnswerQuestion { attachments: vec![],
                interaction_id: "font-choice".into(), option_id: "serif".into(), inputs: Default::default() } }, None, || Ok(())).unwrap();
    }));
    let refused = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"model": "sonnet"})), |_| None, &claude).await.unwrap_err();
    assert_eq!(refused.code, "plan_review_pending");
    assert!(!claude.stopped.lock().unwrap().is_empty(), "the node that started is stopped");
    let state = f.domain.state().unwrap();
    assert!(state.tasks[&TaskId::from("t")].agent_id.is_none() && state.plan_review_pending(&"t".into()));
}

#[tokio::test]
async fn a_launch_of_one_phase_of_an_approved_plan_carries_the_whole_plan_and_names_its_points() {
    use alondra_core::domain::{CardKind, CardSpec, ContentKind, Hold, PlanItems};
    let f = world();
    let actor = the_orchestrator(&f.domain);
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

    let claude = FakeClaude::loading(&f.domain);
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"task": "index", "model": "sonnet"})), |_| None, &claude).await.unwrap();
    let calls = claude.calls.lock().unwrap();
    let prompt = calls[0].1.last().unwrap();
    assert!(prompt.contains("## The plan the user approved") && prompt.contains("do not plan again"), "{prompt}");
    assert!(prompt.contains("Your part of it is points 2, 3; the other points are other tasks"), "{prompt}");
    assert!(prompt.contains("- [ ] Guards") && prompt.contains("- [ ] Index matches the state"), "the whole plan: {prompt}");
}

// With an SDK backend, replacing a claude --bg node stops it, frees the task from it and admits a
// node of the SDK in its folder: the claude --bg node never goes down the SDK node's replacement,
// which only stops an SDK host.
#[tokio::test]
async fn replacing_a_claude_bg_node_with_the_sdk_on_stops_it_and_starts_an_sdk_node() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    connected_node_on_the_task(&f);
    let hosts = crate::test_support::Directory::for_unix_socket();
    let (backend, _) = crate::sdk_owner::tests::fixture_backend(&f.home, &hosts);
    f.domain.enable_sdk(backend);
    let claude = FakeClaude::loading(&f.domain);
    let body = serde_json::from_value(json!({"task": "t", "replace": true, "model": "opus", "reason": "Needs opus"})).unwrap();
    let answer = launch(&f.domain, &f.home, &actor, body, |_| None, &claude).await.unwrap();
    assert_eq!(*claude.stopped.lock().unwrap(), ["agent-b"], "the claude --bg node is stopped");
    assert!(claude.calls.lock().unwrap().is_empty(), "no claude --bg takes its place");
    let agent = AgentId::from(answer["agent_id"].as_str().unwrap());
    let state = f.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id.as_ref(), Some(&agent));
    assert!(crate::sdk_owner::runs_through_sdk(&state, &agent) && f.domain.owns_sdk_host(&agent), "a node of the SDK runs the task");
    assert!(f.domain.live.is_closed("agent-b"), "the old node is closed and not brought back");
    f.domain.stop_sdk_owners();
}

// With an SDK backend, a launch runs through the SDK or does not start, with its reason: it never
// falls back to `claude --bg`, and no node takes the task.
#[tokio::test]
async fn an_sdk_launch_that_fails_does_not_start_claude_in_the_background_instead() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let missing = f.root.join("missing");
    f.domain.enable_sdk(crate::sdk_owner::SdkBackend {
        node: missing.join("node"), node_sha256: "0".repeat(64),
        host: missing.join("host.cjs"), host_sha256: "0".repeat(64),
        native: missing.join("claude"), native_sha256: "0".repeat(64),
        host_environment: Default::default(), native_environment: Default::default(),
        confinement: None, state_root: f.root.join("sdk"), io_timeout_seconds: 5,
    });
    let claude = FakeClaude::loading(&f.domain);
    let refused = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"effort": "light", "title": "Editor colours"})), |_| Some(hard_choices()), &claude).await.unwrap_err();
    assert_eq!(refused.code, "launch_failed", "{refused:?}");
    assert!(refused.message.contains("did not start through the Agent SDK"), "{}", refused.message);
    assert!(claude.calls.lock().unwrap().is_empty(), "claude --bg never runs in its place");
    let state = f.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id, None, "no node took the task");
    assert_eq!(state.agents.len(), 1, "no node was registered for it");
}
