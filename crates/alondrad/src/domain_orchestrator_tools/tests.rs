use super::*;
use crate::wake::WriteFailure;
use alondra_core::domain_v2::{AttachmentRef, TaskState};
use alondra_core::options::OptionsFile;
use std::path::PathBuf;
use std::sync::Mutex;

const SESSION: &str = "ab1234cd-0000-4000-8000-000000000000";

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    project: PathBuf,
    domain: DomainRuntime,
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
    let domain = DomainRuntime::open(&home).unwrap();
    command(&domain, Actor::System, Command::RegisterAgent { agent_id: "agent-b".into(), name: "Instance B".into() });
    command(&domain, user(), Command::CreateRequest { request_id: "r".into(), body: "Make the editor readable".into(), auto_merge: false });
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
    listed: bool,
}
impl crate::orchestrate::Launcher for FakeClaude {
    async fn run(&self, cwd: &str, args: &[String]) -> Result<String, crate::orchestrate::Refusal> {
        self.calls.lock().unwrap().push((cwd.into(), args.to_vec()));
        Ok("backgrounded · ab1234 · editor".into())
    }
    async fn session_of(&self, short: &str) -> Option<String> {
        (self.listed && short == "ab1234").then(|| SESSION.to_owned())
    }
    fn usage(&self) -> Option<alondra_core::usage::Usage> {
        None
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
        serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap()
    }).collect()).unwrap_or_default()
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
    let claude = FakeClaude { calls: Mutex::new(vec![]), listed: true };
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
async fn without_choices_the_orchestrators_effort_applies_and_the_prompt_can_be_given() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let claude = FakeClaude { calls: Mutex::new(vec![]), listed: true };
    launch(&f.domain, &f.home, &actor, launch_body(&f, json!({"effort": "light", "prompt": "Only the dark theme"})), |_| None, &claude).await.unwrap();
    let args = claude.calls.lock().unwrap()[0].1.clone();
    assert!(args.windows(2).any(|pair| pair == ["--model", "sonnet"]), "{args:?}");
    assert!(args.last().unwrap().starts_with("Only the dark theme\n"), "{}", args.last().unwrap());
}

#[tokio::test]
async fn a_launch_is_refused_for_a_closed_or_taken_task_and_an_unlisted_session_leaves_nothing() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let unlisted = FakeClaude { calls: Mutex::new(vec![]), listed: false };
    let error = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({})), |_| None, &unlisted).await.unwrap_err();
    assert_eq!(error.code, "launch_failed");
    assert!(settings_files(&f.home).is_empty());
    assert_eq!(f.domain.state().unwrap().agents.len(), 1);
    assert!(f.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id.is_none());

    command(&f.domain, actor.clone(), Command::AssignTask { task_id: "t".into(), agent_id: "agent-b".into() });
    crate::domain_agent_lifecycle::observe_instance(&f.domain, &"agent-b".into(), true,
        &crate::domain_agent_lifecycle::prepare_event(&f.domain, "start-b").unwrap()).unwrap();
    let claude = FakeClaude { calls: Mutex::new(vec![]), listed: true };
    let taken = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({})), |_| None, &claude).await.unwrap_err();
    assert_eq!(taken.code, "conflict");
    command(&f.domain, user(), Command::CancelTask { task_id: "t".into() });
    let closed = launch(&f.domain, &f.home, &actor, launch_body(&f, json!({})), |_| None, &claude).await.unwrap_err();
    assert_eq!(closed.code, "conflict");
    assert!(claude.calls.lock().unwrap().is_empty());
}

struct Listed(Option<Session>);
impl Sessions for Listed {
    async fn find(&self, _: &str) -> Option<Session> {
        self.0.clone()
    }
}

#[derive(Default)]
struct Typed(Mutex<Vec<(String, String)>>);
impl Terminal for Typed {
    fn write(&self, short: &str, text: &str) -> Result<(), WriteFailure> {
        self.0.lock().unwrap().push((short.into(), text.into()));
        Ok(())
    }
}

fn background(short: &str) -> Listed {
    Listed(Some(Session { short: short.into(), interactive: false }))
}

#[tokio::test]
async fn a_message_about_a_task_nobody_works_on_gives_it_to_that_agent_and_a_cancelled_one_reopens() {
    let f = world();
    let actor = the_orchestrator(&f.domain);
    let terminal = Arc::new(Typed::default());
    let body = |text: &str| -> SendBody { serde_json::from_value(json!({"node": "Instance B", "text": text, "task": "t"})).unwrap() };
    let answer = send(&f.domain, &f.home, &actor, body("Take the editor colours"), &background("b1"), &terminal).await.unwrap();
    assert_eq!(answer["delivery_state"], "handed");
    assert_eq!(terminal.0.lock().unwrap()[0], ("b1".to_string(), "[Task t “Editor colours”] Take the editor colours".to_string()));
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id, Some(AgentId::from("agent-b")));

    command(&f.domain, user(), Command::CancelTask { task_id: "t".into() });
    send(&f.domain, &f.home, &actor, body("Carry on after all"), &background("b1"), &terminal).await.unwrap();
    assert_ne!(f.domain.state().unwrap().tasks[&TaskId::from("t")].state, TaskState::Cancelled);

    command(&f.domain, Actor::System, Command::RegisterAgent { agent_id: "agent-c".into(), name: "Instance C".into() });
    let foreign: SendBody = serde_json::from_value(json!({"node": "agent-c", "text": "Mine now", "task": "t"})).unwrap();
    assert_eq!(send(&f.domain, &f.home, &actor, foreign, &background("c1"), &terminal).await.unwrap_err().code, "conflict");
    assert_eq!(terminal.0.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn an_interrupt_presses_esc_in_a_background_session_only() {
    let f = world();
    let pressed = Arc::new(Mutex::new(vec![]));
    let record = |pressed: Arc<Mutex<Vec<String>>>| move |short: &str| { pressed.lock().unwrap().push(short.to_owned()); Ok(()) };
    let node = || -> NodeBody { serde_json::from_value(json!({"node": "agent-b"})).unwrap() };
    let answer = interrupt(&f.domain, node(), &background("b1"), record(pressed.clone())).await.unwrap();
    assert_eq!(answer, json!({"interrupted": "b1"}));
    let terminal = interrupt(&f.domain, node(), &Listed(Some(Session { short: "b1".into(), interactive: true })), record(pressed.clone())).await;
    assert_eq!(terminal.unwrap_err().code, "interactive_session");
    let gone = interrupt(&f.domain, node(), &Listed(None), record(pressed.clone())).await;
    assert_eq!(gone.unwrap_err().code, "session_unreachable");
    assert_eq!(*pressed.lock().unwrap(), ["b1"]);
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
