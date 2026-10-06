use super::*;
use crate::node_push::testing::Node;
use alondra_core::domain::{AttachmentRef, TaskState};
use alondra_core::options::OptionsFile;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const SESSION: &str = "ab1234cd-0000-4000-8000-000000000000";

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

fn command(domain: &DomainRuntime, actor: Actor, command: Command) {
    run(domain, &actor, command).unwrap();
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn world() -> Fixture {
    let root = std::env::temp_dir().join(format!("alondra-orchestrator-tools-{}", crate::auth::random_hex(8)));
    let home = root.join("home");
    let project = root.join("App");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let domain = Arc::new(DomainRuntime::open(&home).unwrap());
    command(&domain, Actor::System, Command::RegisterAgent { agent_id: "agent-b".into(), name: "Instance B".into() });
    command(&domain, user(), Command::CreateRequest { request_id: "r".into(), body: "Make the editor readable".into(), auto_merge: false });
    command(&domain, user(), Command::TitleRequest { request_id: "r".into(), title: "Readable editor".into() });
    command(&domain, user(), Command::CreateTask { task_id: "t".into(), request_id: "r".into(), title: "Editor colours".into(),
        agent_id: None, review_prs: false, dependencies: Default::default(), stack: None });
    Fixture { root, home, project, domain }
}

fn the_orchestrator(domain: &DomainRuntime) -> Actor {
    let capability = domain.bind_orchestrator("execution-1").unwrap();
    orchestrator(domain, Some(capability.expose_for_child_environment())).unwrap()
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
}
impl FakeClaude {
    fn loading(domain: &Arc<DomainRuntime>) -> Self {
        Self { calls: Mutex::new(vec![]), loads_mod_in: Some(domain.clone()), stopped: Mutex::new(vec![]) }
    }

    fn without_mod() -> Self {
        Self { calls: Mutex::new(vec![]), loads_mod_in: None, stopped: Mutex::new(vec![]) }
    }
}
impl crate::orchestrate::Launcher for FakeClaude {
    async fn run(&self, cwd: &str, args: &[String]) -> Result<String, crate::orchestrate::Refusal> {
        self.calls.lock().unwrap().push((cwd.into(), args.to_vec()));
        if let Some(domain) = &self.loads_mod_in {
            domain.live.mods.loaded_by(SESSION, args);
        }
        Ok("backgrounded · ab1234 · editor".into())
    }
    async fn stop(&self, short: &str) {
        self.stopped.lock().unwrap().push(short.into());
    }
    fn mods_deadline(&self) -> std::time::Duration {
        std::time::Duration::from_millis(20)
    }
    fn plugin(&self) -> Option<PathBuf> {
        Some("/synthetic/plugin".into())
    }
}

fn launch_body(f: &Fixture, extra: Value) -> LaunchBody {
    let mut body = json!({"name": "editor", "cwd": f.project.to_str().unwrap(), "task": "t"});
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
    assert_eq!(orchestrator(&f.domain, None).unwrap_err().code, "forbidden");
    assert_eq!(orchestrator(&f.domain, Some(&"a".repeat(64))).unwrap_err().code, "forbidden");
    let agent = f.domain.bind_agent("agent-b").unwrap();
    assert_eq!(orchestrator(&f.domain, Some(agent.expose_for_child_environment())).unwrap_err().code, "forbidden");
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
    let refused = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({})), |_| None, &claude).await.unwrap_err();
    assert_eq!(refused.code, "workspace_untrusted");
    assert!(refused.message.contains(project.to_str().unwrap()), "{}", refused.message);
    assert!(claude.calls.lock().unwrap().is_empty());
    assert!(f.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id.is_none());

    trust(true);
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({})), |_| None, &claude).await.unwrap();
    assert_eq!(claude.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn without_choices_the_orchestrators_effort_applies_and_the_prompt_can_be_given() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"effort": "light", "prompt": "Only the dark theme"})), |_| None, &claude).await.unwrap();
    let args = claude.calls.lock().unwrap()[0].1.clone();
    assert!(args.windows(2).any(|pair| pair == ["--model", "sonnet"]), "{args:?}");
    assert!(args.last().unwrap().starts_with("Only the dark theme\n"), "{}", args.last().unwrap());
}

#[tokio::test]
async fn a_launch_is_refused_for_a_closed_or_taken_task_and_a_session_without_its_mod_leaves_nothing() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let unlisted = FakeClaude::without_mod();
    let error = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({})), |_| None, &unlisted).await.unwrap_err();
    assert_eq!(error.code, "launch_failed");
    assert!(settings_files(&f.home).is_empty());
    assert_eq!(f.domain.state().unwrap().agents.len(), 1);
    assert!(f.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id.is_none());

    command(&f.domain, actor.clone(), Command::AssignTask { task_id: "t".into(), agent_id: "agent-b".into() });
    crate::domain_agent_lifecycle::observe_instance(&f.domain, &"agent-b".into(), true,
        &crate::domain_agent_lifecycle::prepare_event(&f.domain, "start-b").unwrap()).unwrap();
    let claude = FakeClaude::loading(&f.domain);
    let taken = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({})), |_| None, &claude).await.unwrap_err();
    assert_eq!(taken.code, "conflict");
    command(&f.domain, user(), Command::CancelTask { task_id: "t".into() });
    let closed = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({})), |_| None, &claude).await.unwrap_err();
    assert_eq!(closed.code, "conflict");
    assert!(claude.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_session_whose_mod_never_loads_is_stopped_and_the_launch_fails_without_an_agent() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::without_mod();
    let error = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({})), |_| None, &claude).await.unwrap_err();
    assert_eq!(error.code, "launch_failed");
    assert!(error.message.contains("mods did not load") && error.message.contains("not trusted"), "{}", error.message);
    assert_eq!(*claude.stopped.lock().unwrap(), ["ab1234"], "the session is stopped, not left working unseen");
    assert!(settings_files(&f.home).is_empty());
    let state = f.domain.state().unwrap();
    assert!(!state.agents.contains_key(&AgentId::from(SESSION)));
    assert!(state.tasks[&TaskId::from("t")].agent_id.is_none(), "the task stays free for another launch");
}

#[tokio::test]
async fn a_message_about_a_task_nobody_works_on_gives_it_to_that_agent_and_a_cancelled_one_reopens() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let node = Node::connect(&f.domain, "agent-b");
    let body = |text: &str| -> SendBody { serde_json::from_value(json!({"node": "Instance B", "text": text, "task": "t"})).unwrap() };
    let answer = send(&f.domain, &f.home, &actor, body("Take the editor colours")).await.unwrap();
    assert_eq!(answer["delivery_state"], "handed");
    assert_eq!(node.texts()[0], "[Task t “Editor colours”] Take the editor colours");
    assert_eq!(node.lines.lock().unwrap()[0]["as_user"], false, "the orchestrator's words reach the model as its own");
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id, Some(AgentId::from("agent-b")));

    command(&f.domain, user(), Command::CancelTask { task_id: "t".into() });
    send(&f.domain, &f.home, &actor, body("Carry on after all")).await.unwrap();
    assert_ne!(f.domain.state().unwrap().tasks[&TaskId::from("t")].state, TaskState::Cancelled);

    command(&f.domain, Actor::System, Command::RegisterAgent { agent_id: "agent-c".into(), name: "Instance C".into() });
    let foreign: SendBody = serde_json::from_value(json!({"node": "agent-c", "text": "Mine now", "task": "t"})).unwrap();
    assert_eq!(send(&f.domain, &f.home, &actor, foreign).await.unwrap_err().code, "conflict");
    assert_eq!(node.texts().len(), 2);
}

#[tokio::test]
async fn a_node_on_a_task_takes_no_other_until_it_ends_but_still_gets_messages() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let node = Node::connect(&f.domain, "agent-b");
    command(&f.domain, user(), Command::CreateTask { task_id: "t2".into(), request_id: "r".into(), title: "Editor fonts".into(),
        agent_id: None, review_prs: false, dependencies: Default::default(), stack: None });
    let body = |task: Option<&str>| -> SendBody { serde_json::from_value(json!({"node": "Instance B", "text": "Go", "task": task})).unwrap() };
    send(&f.domain, &f.home, &actor, body(Some("t"))).await.unwrap();

    let refused = send(&f.domain, &f.home, &actor, body(Some("t2"))).await.unwrap_err();
    assert_eq!(refused.code, "agent_busy");
    assert!(refused.message.contains("launch a new node for t2"), "{}", refused.message);
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t2")].agent_id, None);
    send(&f.domain, &f.home, &actor, body(None)).await.unwrap();
    send(&f.domain, &f.home, &actor, body(Some("t"))).await.unwrap();
    assert_eq!(node.texts().len(), 3, "the refused task sent nothing");

    command(&f.domain, user(), Command::CancelTask { task_id: "t".into() });
    send(&f.domain, &f.home, &actor, body(Some("t2"))).await.unwrap();
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t2")].agent_id, Some(AgentId::from("agent-b")), "a node whose task ended takes the next one");
    assert_eq!(send(&f.domain, &f.home, &actor, body(Some("t"))).await.unwrap_err().code, "agent_busy",
        "reopening its old task would give it two");
}

#[tokio::test]
async fn an_interrupt_is_a_line_for_the_connected_agent_and_is_refused_for_one_that_is_not() {
    let f = world();
    let node = Node::connect(&f.domain, "agent-b");
    let named = || -> NodeBody { serde_json::from_value(json!({"node": "agent-b"})).unwrap() };
    assert_eq!(interrupt(&f.domain, named()).unwrap(), json!({"interrupted": "agent-b"}));
    tokio::task::yield_now().await;
    assert_eq!(node.interrupts(), 1);
    command(&f.domain, Actor::System, Command::RegisterAgent { agent_id: "agent-a".into(), name: "Instance A".into() });
    let gone: NodeBody = serde_json::from_value(json!({"node": "agent-a"})).unwrap();
    assert_eq!(interrupt(&f.domain, gone).unwrap_err().code, "session_unreachable");
    assert_eq!(node.interrupts(), 1);
}

#[test]
fn the_choices_are_the_ones_recorded_with_the_request() {
    let f = world();
    let state = f.domain.state().unwrap();
    let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch, command_id: "asked".into(),
        expected_revision: state.revision, command: Command::CreateRequest { request_id: "r2".into(), body: "Fix the crash".into(), auto_merge: false } };
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

fn measured(domain: &DomainRuntime, week_percent: f64) {
    let reset = crate::log::local_iso(alondra_core::now_millis().div_euclid(1000) as libc::time_t + 5 * 86_400, 0);
    let envelope = serde_json::from_value(json!({"session_id": "m", "event": "session.measure",
        "rate_limits": [{"kind": "seven_day", "percent_used": week_percent, "resets_at": reset}]})).unwrap();
    domain.live.mods.apply(envelope, alondra_core::now_millis());
}

#[tokio::test]
async fn going_over_todays_slice_launches_without_a_warning_and_asks_the_user_once_a_day() {
    let f = world();
    measured(&f.domain, 12.0);
    measured(&f.domain, 53.0);
    assert!(f.domain.live.mods.usage(alondra_core::now_millis()).unwrap().over_pace, "41% spent against a ~18% slice");
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude::loading(&f.domain);
    let answer = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({})), |_| None, &claude).await.unwrap();
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
    measured(&within.domain, 12.0);
    measured(&within.domain, 15.0);
    let answer = launch(&within.domain, &within.home, &the_orchestrator(&within.domain), launch_body(&within, json!({"over_pace_ok": true})),
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
    let refused = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({})), |_| None, &claude).await.unwrap_err();
    assert_eq!(refused.code, "usage_exhausted");
    assert!(refused.message.contains("Claude usage ran out"), "{}", refused.message);
    assert!(claude.calls.lock().unwrap().is_empty());
    assert!(f.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id.is_none(), "the task is left as it was");

    f.domain.live.usage_pause.resume(alondra_core::now_millis());
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({})), |_| None, &claude).await.unwrap();
    assert_eq!(claude.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_task_whose_node_the_user_closed_is_relaunched_for_that_task_in_the_closed_nodes_folder() {
    let f = world();
    command(&f.domain, user(), Command::AssignTask { task_id: "t".into(), agent_id: "agent-b".into() });
    command(&f.domain, Actor::Agent { agent_id: "agent-b".into() }, Command::StartTask { task_id: "t".into() });
    f.domain.live.mods.connect(serde_json::from_value(json!({"session_id": "agent-b", "version": "2.1.287", "cwd": f.project.to_str().unwrap()})).unwrap(), 1);
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
    assert!(f.domain.live.closed_task_ids().is_empty(), "the task no longer waits for a relaunch");
    drop(calls);
    let again = relaunch(&f.domain, &f.home, &user(), &"t".into(), &claude).await.unwrap_err();
    assert_eq!(again.code, "conflict");
}
