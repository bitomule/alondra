//! Orchestrator batches through the real SDK host: the pinned Node host with the official SDK and
//! a fake native Claude that answers each prompt as its marker says. A held turn waits on a FIFO
//! the test writes, never on a clock.
use super::*;
use crate::domain_orchestrator::{Executor, Failure, load, stub_claude};
use crate::domain_runtime::DomainRuntime;
use crate::orchestrator_process::Ownership;
use crate::test_support::Directory;
use alondra_core::domain::*;
use alondra_core::orchestrator_cards::{UNSETTLED_NOTICE_PREFIX, UNSPLIT_NOTICE_PREFIX, UNSPLIT_PROBLEM_PREFIX};
use alondra_provider_contract::Handoff;
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

fn runfile(name: &str) -> PathBuf {
    std::fs::canonicalize(std::env::var(name).unwrap_or_else(|_| panic!("{name} names a runfile"))).unwrap()
}

struct Fixture {
    root: Directory,
    _state: Directory,
    executor: Executor,
    ownership: Ownership,
    capture: PathBuf,
    held: PathBuf,
    release: PathBuf,
    claude_home: PathBuf,
}

fn fifo(path: &Path) {
    assert!(std::process::Command::new("/usr/bin/mkfifo").arg(path).status().unwrap().success());
}

fn fixture() -> Fixture {
    let root = Directory::new();
    std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let (claude_home, executable) = stub_claude(&root, "exit 97");
    let native = root.join("native");
    std::fs::copy(runfile("FIXTURE_ORCHESTRATOR_NATIVE"), &native).unwrap();
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (capture, held, release) = (root.join("capture.json"), root.join("held"), root.join("release"));
    fifo(&held);
    fifo(&release);
    let environment = BTreeMap::from([("HOME".to_owned(), root.to_str().unwrap().to_owned()), ("PATH".to_owned(), "/usr/bin:/bin".to_owned())]);
    let mut native_environment = environment.clone();
    for (name, path) in [("FIXTURE_CAPTURE", &capture), ("FIXTURE_HELD", &held), ("FIXTURE_RELEASE", &release)] {
        native_environment.insert(name.into(), path.to_str().unwrap().into());
    }
    let (node, host) = (runfile("FIXTURE_NODE"), runfile("FIXTURE_HOST"));
    let state = Directory::for_unix_socket();
    let backend = SdkBackend {
        node_sha256: crate::sdk_owner::backend::digest(&node).unwrap(),
        host_sha256: crate::sdk_owner::backend::digest(&host).unwrap(),
        native_sha256: crate::sdk_owner::backend::digest(&native).unwrap(),
        node,
        host,
        native,
        host_environment: environment,
        native_environment,
        confinement: None,
        state_root: state.join("h"),
        io_timeout_seconds: 30,
    };
    let domain = Arc::new(DomainRuntime::open(&home).unwrap());
    domain.enable_sdk(backend);
    let ownership = Ownership::acquire(&home.join("runtime/orchestrator")).unwrap().unwrap();
    Fixture { root, _state: state, executor: Executor { domain, home, claude_home: claude_home.clone(), executable }, ownership, capture, held, release, claude_home }
}

impl Fixture {
    fn domain(&self) -> &DomainRuntime {
        &self.executor.domain
    }
    fn run(&self, actor: Actor, id: &str, command: Command) {
        let state = self.domain().state().unwrap();
        self.domain().execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }
    /// A node working on task `t` asks the orchestrator `body`: one orchestrator inbox message.
    fn ask(&self, n: u32, body: &str) {
        if self.domain().state().unwrap().tasks.is_empty() {
            self.run(Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
            self.run(Actor::Orchestrator { generation: 0 }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
            self.run(Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None,
                task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()),
                review_prs: false, dependencies: BTreeSet::new(), stack: None,
                launch: Some(LaunchPlan::new("node", "/tmp")),
            });
        }
        self.run(Actor::Agent { agent_id: "a".into() }, &format!("question-{n}"), Command::AskQuestion {
            attachments: vec![], interaction_id: format!("q{n}").into(), scope: Scope::Task { task_id: "t".into() },
            recipient: Recipient::Orchestrator, body: body.into(), blocking: false,
            options: vec![QuestionOption { id: "here".into(), label: "Here".into(), description: None }],
            card: CardSpec::derived(CardKind::Provide, "Answer the question"),
        });
    }
    /// A message from the user: once the orchestrator handled it, `settle` closes it.
    fn tell(&self, id: &str, body: &str) {
        self.run(Actor::User { user_id: "u".into() }, id, Command::CreateRequest { request_id: id.into(), body: body.into(), auto_merge: false, maintenance: false, message: true });
    }
    fn prompts(&self) -> Vec<String> {
        match std::fs::read_to_string(format!("{}.prompts", self.capture.display())) {
            Ok(text) => text.lines().map(|line| serde_json::from_str(line).unwrap()).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(error) => panic!("{error}"),
        }
    }
    fn arguments(&self) -> Vec<String> {
        let capture: serde_json::Value = serde_json::from_slice(&std::fs::read(&self.capture).unwrap()).unwrap();
        serde_json::from_value(capture["args"].clone()).unwrap()
    }
    fn pending(&self) -> usize {
        self.domain().store.lock().unwrap().pending_messages().unwrap().into_iter().filter(|m| m.recipient_kind == "orchestrator").count()
    }
    fn attempts(&self) -> Vec<String> {
        self.domain().store.lock().unwrap().delivery_evidence_page(0).unwrap().into_iter()
            .filter(|i| i.recipient_kind == "orchestrator").map(|i| i.state).collect()
    }
    fn cards(&self, prefix: &str) -> Vec<String> {
        self.domain().state().unwrap().interactions.keys().filter(|id| id.0.starts_with(prefix)).map(|id| id.0.clone()).collect()
    }
    fn unresolved(&self) -> Vec<String> {
        let state = self.domain().state().unwrap();
        let requests: Vec<RequestId> = state.requests.keys().cloned().collect();
        let suggestions: Vec<InteractionId> = state.interactions.keys().cloned().collect();
        crate::orchestrator_inbox::unresolved(&state, &requests, &suggestions)
    }
    async fn tick(&self) {
        self.executor.tick(&self.ownership).await.unwrap();
    }
    /// Blocks until the fake Claude holds its turn.
    fn wait_held(&self) {
        let mut text = String::new();
        std::fs::File::open(&self.held).unwrap().read_to_string(&mut text).unwrap();
        assert_eq!(text, "held\n");
    }
    fn release(&self) {
        std::fs::OpenOptions::new().write(true).open(&self.release).unwrap().write_all(b"go").unwrap();
    }
}

#[tokio::test]
async fn a_batch_through_the_sdk_is_one_turn_and_is_handed_once() {
    let f = fixture();
    f.tell("m-1", "Where do I put change 1?");
    f.tell("m-2", "Where do I put change 2?");
    f.tick().await;
    let prompts = f.prompts();
    assert_eq!(prompts.len(), 1, "one batch, one prompt: {prompts:?}");
    assert!(prompts[0].contains("Where do I put change 1?") && prompts[0].contains("Where do I put change 2?") && prompts[0].contains("new conversation"), "{}", prompts[0]);
    assert_eq!(f.attempts(), ["handed", "handed"]);
    assert_eq!(f.pending(), 0);
    let runtime = load(&f.executor.home).unwrap();
    assert!(runtime.active_run.is_none() && runtime.last_error.is_none(), "{runtime:?}");
    let session = runtime.session_id.clone().unwrap();
    assert!(f.arguments().contains(&format!("--session-id={session}")), "{:?}", f.arguments());
    assert!(f.arguments().contains(&"--permission-mode=auto".to_owned()), "{:?}", f.arguments());
    {
        let store = f.domain().store.lock().unwrap();
        for item in store.delivery_evidence_page(0).unwrap().into_iter().filter(|i| i.recipient_kind == "orchestrator") {
            assert_eq!(store.orchestrator_delivery(item.attempt_id).unwrap().as_deref(), Some(session.as_str()), "each message records the session it was handed to");
        }
    }
    assert!(!f.executor.home.join("runtime/orchestrator/owner.json").exists(), "the host's record goes with it");
    f.tell("m-3", "Where do I put change 3?");
    f.tick().await;
    let prompts = f.prompts();
    assert_eq!(prompts.len(), 2);
    assert!(prompts[1].contains("Where do I put change 3?") && !prompts[1].contains("Where do I put change 1?"), "{}", prompts[1]);
}

/// The context of a batch is what Alondra writes in its prompt: each starts its own conversation,
/// with the note the previous one left and the questions the orchestrator still waits on, and
/// nothing of the conversations before it.
#[tokio::test]
async fn every_batch_is_a_new_conversation_and_carries_what_the_next_needs() {
    let f = fixture();
    let mut sessions = vec![];
    for n in 1..=5 {
        f.tell(&format!("m-{n}"), &format!("Where do I put change {n}?"));
        f.tick().await;
        let arguments = f.arguments();
        assert!(!arguments.iter().any(|argument| argument.starts_with("--resume")), "batch {n} resumes a conversation: {arguments:?}");
        let session = load(&f.executor.home).unwrap().session_id.unwrap();
        assert!(arguments.contains(&format!("--session-id={session}")), "{arguments:?}");
        sessions.push(session);
        if n == 1 {
            f.run(Actor::Orchestrator { generation: 0 }, "question", Command::AskQuestion {
                attachments: vec![], interaction_id: "q-waits".into(), scope: Scope::Orchestrator { request_id: None },
                recipient: Recipient::User, body: "Which repository is change 1 for?".into(), blocking: false,
                options: vec![QuestionOption { id: "web".into(), label: "Web".into(), description: None }, QuestionOption { id: "ios".into(), label: "iOS".into(), description: None }],
                card: CardSpec::derived(CardKind::Decide, "Choose the repository"),
            });
        }
    }
    assert_eq!(sessions.iter().collect::<BTreeSet<_>>().len(), 5, "five batches, five conversations: {sessions:?}");
    let prompts = f.prompts();
    assert_eq!(prompts.len(), 5, "{prompts:?}");
    for prompt in &prompts[1..] {
        assert!(prompt.contains("YOUR NOTE FROM THE PREVIOUS BATCH:\nBatch handled\n"), "the note of the batch before: {prompt}");
        assert!(prompt.contains("your question q-waits, still unanswered: Which repository is change 1 for?"), "{prompt}");
    }
    let lengths: Vec<usize> = prompts[1..].iter().map(String::len).collect();
    assert!(lengths.windows(2).all(|pair| pair[0] == pair[1]), "a batch does not grow with the batches before it: {lengths:?}");
    let context = load(&f.executor.home).unwrap().last_context;
    assert_eq!(context, Some(crate::domain_orchestrator::BatchContext { first: 25, last: 25, window: None }), "each batch records the context it read");
}

#[tokio::test]
async fn a_message_that_arrives_during_a_turn_waits_for_the_next_batch() {
    let f = fixture();
    f.ask(1, "fixture:hold Where do I put change 1?");
    let during = async {
        let held = f.held.clone();
        tokio::task::spawn_blocking(move || {
            let mut text = String::new();
            std::fs::File::open(held).unwrap().read_to_string(&mut text).unwrap();
        }).await.unwrap();
        f.tell("m-2", "Where do I put change 2?");
        let release = f.release.clone();
        tokio::task::spawn_blocking(move || std::fs::OpenOptions::new().write(true).open(release).unwrap().write_all(b"go").unwrap()).await.unwrap();
    };
    let ((), ()) = tokio::join!(f.tick(), during);
    let prompts = f.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(prompts[0].contains("change 1") && !prompts[0].contains("change 2"), "the message that came during the turn is not in it: {}", prompts[0]);
    assert_eq!(f.attempts(), ["handed"]);
    assert_eq!(f.pending(), 1, "it waits for the next batch");
    assert!(!f.domain().state().unwrap().requests[&"m-2".into()].closed, "nothing settles a message the orchestrator has not seen");
    f.tick().await;
    let prompts = f.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts[1].contains("change 2") && !prompts[1].contains("change 1"), "{}", prompts[1]);
    assert_eq!(f.attempts()[..2], ["handed", "handed"]);
}

#[test]
fn the_host_of_a_batch_takes_one_prompt_and_refuses_another() {
    let f = fixture();
    let session = "00000000-0000-4000-8000-00000000c0de";
    let epoch = f.domain().state().unwrap().data_epoch.clone();
    let (claims, owner) = f.domain().store.lock().unwrap().claim_orchestrator_batch(&[], "run-refuses", session, epoch).unwrap();
    assert!(claims.is_empty());
    let (backend, _) = f.domain().sdk().unwrap();
    let directory = f.executor.home.join("runtime/orchestrator");
    let plugin = |name| crate::plugins::dir(&f.claude_home, name).unwrap();
    let batch = Batch {
        owner, backend: backend.clone(), home: f.executor.home.clone(), directory,
        node_plugin: plugin(crate::plugins::NODE), orchestrator_plugin: plugin(crate::plugins::ORCHESTRATOR),
        projects_root: None, capability: "c".repeat(64), session: session.into(), prompt: String::new(),
    };
    let (configuration, profile) = (configuration(&batch).unwrap(), profile(&batch));
    let mut host = BatchHost::start(batch.owner, &configuration, session, &profile, "fixture:hold the first batch").unwrap();
    f.wait_held();
    assert_eq!(host.offer("the second batch").unwrap(), Handoff::RefusedBeforeOffer, "one batch, one turn");
    f.release();
    let mut stream = Stream::for_input(host.input());
    loop {
        match host.next().unwrap() {
            HostEvent::Sdk(message) => assert!(!stream.accept(&serde_json::to_vec(&message).unwrap(), session).is_err()),
            HostEvent::Diagnostic(text) => panic!("{text}"),
            HostEvent::Exited { code, .. } => {
                assert_eq!(code, Some(0));
                break;
            }
        }
    }
    host.stop().unwrap();
    assert_eq!(stream.end(true, "", session, false, false), process::BatchEnd::Handled);
    assert_eq!(f.prompts(), ["fixture:hold the first batch"], "the refused prompt never reached Claude");
}

#[tokio::test]
async fn every_request_of_a_handled_batch_ends_with_a_task_an_answer_or_a_question() {
    let f = fixture();
    let user = || Actor::User { user_id: "u".into() };
    for id in ["r-1", "r-2", "r-3"] {
        f.run(user(), id, Command::CreateRequest { request_id: id.into(), body: format!("Do {id}"), auto_merge: false, maintenance: false, message: false });
    }
    f.run(Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
    f.run(Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None,
        task_id: "t-2".into(), request_id: "r-2".into(), title: "Task".into(), agent_id: Some("a".into()), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    f.run(Actor::Orchestrator { generation: 0 }, "answer", Command::CloseRequest { request_id: "r-3".into(), withdrawn: false, answer: Some("Already done".into()), title: None });
    f.run(Actor::Agent { agent_id: "a".into() }, "start", Command::StartTask { task_id: "t-2".into() });
    f.run(Actor::Agent { agent_id: "a".into() }, "suggest", Command::AskQuestion {
        attachments: vec![], interaction_id: "s-1".into(), scope: Scope::Task { task_id: "t-2".into() },
        recipient: Recipient::Orchestrator, body: "The badge is stale too".into(), blocking: false,
        options: suggestion_options(),
        card: CardSpec::new(CardKind::Propose, "Fix the stale badge").why("Not this task"),
    });
    assert!(f.pending() > 0, "the batch path runs, not the idle one");
    f.tick().await;
    assert_eq!(f.unresolved(), Vec::<String>::new(), "petición sin resultado");
    assert_eq!(f.cards(UNSPLIT_NOTICE_PREFIX), [format!("{UNSPLIT_NOTICE_PREFIX}r-1-1")], "r-1 goes back to the orchestrator once");
    assert_eq!(f.cards(UNSETTLED_NOTICE_PREFIX), [format!("{UNSETTLED_NOTICE_PREFIX}s-1")], "the suggestion goes back once");
    f.tick().await;
    assert_eq!(f.prompts().len(), 2, "the reminders are the next batch, a turn of their own");
    assert_eq!(f.unresolved(), Vec::<String>::new(), "petición sin resultado");
    assert_eq!(f.cards(UNSPLIT_PROBLEM_PREFIX), [format!("{UNSPLIT_PROBLEM_PREFIX}r-1-1")], "then it is a question to the user");
}

async fn ended(marker: &str) -> Fixture {
    let f = fixture();
    f.ask(1, &format!("{marker} Where do I put change 1?"));
    f.tick().await;
    f
}

#[tokio::test]
async fn a_batch_claude_never_answered_is_offered_again_without_a_card() {
    let f = ended("fixture:exit-before-init").await;
    let runtime = load(&f.executor.home).unwrap();
    assert_eq!(runtime.failure, Some(Failure::Launch), "{runtime:?}");
    assert!(runtime.active_run.is_none() && runtime.retry_at.is_some());
    assert_eq!(f.attempts(), ["retryable"]);
    assert!(f.cards("problem-orchestrator-").is_empty());
}

#[tokio::test]
async fn a_batch_that_called_an_alondra_tool_and_died_is_the_users_call() {
    let f = ended("fixture:tool-then-exit").await;
    let runtime = load(&f.executor.home).unwrap();
    assert_eq!(runtime.failure, Some(Failure::Interrupted), "{runtime:?}");
    assert_eq!(f.attempts(), ["uncertain"]);
    assert_eq!(f.cards("problem-orchestrator-").len(), 1);
}

#[tokio::test]
async fn a_batch_with_an_alondra_tool_refused_is_the_users_call() {
    let f = ended("fixture:denied").await;
    assert_eq!(load(&f.executor.home).unwrap().failure, Some(Failure::Denied));
    assert_eq!(f.attempts(), ["uncertain"]);
    assert_eq!(f.cards("problem-orchestrator-").len(), 1);
}

#[tokio::test]
async fn a_result_of_another_input_is_not_the_batch_handled() {
    let f = ended("fixture:another-input").await;
    let runtime = load(&f.executor.home).unwrap();
    assert_eq!(runtime.failure, Some(Failure::Interrupted), "{runtime:?}");
    assert!(runtime.last_error.as_deref().is_some_and(|error| error.contains("another input")), "{runtime:?}");
    assert_eq!(f.attempts(), ["uncertain"]);
}

#[tokio::test]
async fn a_batch_that_ran_out_of_usage_before_answering_pauses_and_waits_without_a_card() {
    let f = ended("fixture:limited").await;
    assert!(f.domain().live.usage_pause.is_active(), "usage pauses everything");
    let runtime = load(&f.executor.home).unwrap();
    assert!(runtime.active_run.is_none() && runtime.failure.is_none(), "{runtime:?}");
    assert_eq!(f.attempts(), ["retryable"]);
    assert!(f.cards("problem-orchestrator-").is_empty());
}

#[tokio::test]
async fn a_cancelled_turn_that_may_have_acted_is_the_users_call() {
    let f = fixture();
    f.ask(1, "fixture:hold Where do I put change 1?");
    let during = async {
        let held = f.held.clone();
        tokio::task::spawn_blocking(move || {
            let mut text = String::new();
            std::fs::File::open(held).unwrap().read_to_string(&mut text).unwrap();
        }).await.unwrap();
        let run = load(&f.executor.home).unwrap().active_run.unwrap().id;
        f.executor.cancel(&run).unwrap();
    };
    let ((), ()) = tokio::join!(f.tick(), during);
    let runtime = load(&f.executor.home).unwrap();
    assert_eq!(runtime.failure, Some(Failure::Cancelled), "{runtime:?}");
    assert_eq!(f.attempts(), ["uncertain"]);
    assert_eq!(f.cards("problem-orchestrator-").len(), 1);
}
