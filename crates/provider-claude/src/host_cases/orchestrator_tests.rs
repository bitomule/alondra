//! An orchestrator batch: one prompt, one turn and every event the host sends until Claude exits,
//! from a new conversation, a resumed one, one the native Claude no longer has, and a turn stopped
//! while it is held. `sdk_worker_test` runs it on the real host with the fake orchestrator Claude,
//! `claude_host_model_test` on the orchestrator model that the orchestrator's own tests run on.
use super::*;
use crate::{BatchHost, HostEvent, HostOwner, OrchestratorExecution, OrchestratorLane, OrchestratorProfile};
use alondra_foundation::DataEpoch;

const SESSION: &str = "00000000-0000-4000-8000-00000000c0de";
const UNKNOWN_SESSION: &str = "00000000-0000-4000-8000-00000000dead";

/// What runs behind the port for an orchestrator batch, and how the test reads the native Claude.
struct Orchestrator {
    hosts: Arc<dyn AgentHost>,
    node: PathBuf,
    bundle: PathBuf,
    native: PathBuf,
    native_environment: BTreeMap<String, String>,
    arguments: Box<dyn Fn() -> Vec<String>>,
    held: Box<dyn Fn()>,
}

/// The real Node host with the fake orchestrator Claude, which holds a turn on a pipe only the
/// test could write to.
#[cfg(feature = "sdk_fixture")]
fn orchestrator(directory: &Path, home: &Path) -> Orchestrator {
    let under = under_test(directory, home);
    let runfile = PathBuf::from(std::env::var_os("TEST_SRCDIR").expect("Bazel runfiles root"))
        .join(std::env::var_os("TEST_WORKSPACE").expect("Bazel workspace"))
        .join(std::env::var_os("FIXTURE_ORCHESTRATOR_NATIVE").expect("declared rootpath data"));
    let native = directory.join("orchestrator-native");
    fs::copy(runfile, &native).unwrap();
    fs::set_permissions(&native, fs::Permissions::from_mode(0o700)).unwrap();
    let (capture, held, release) = (directory.join("orchestrator-capture.json"), directory.join("held"), directory.join("release"));
    assert!(std::process::Command::new("/usr/bin/python3").args(["-c", "import os, sys; os.mkfifo(sys.argv[1])"]).arg(&release).status().unwrap().success());
    let mut native_environment = test_support::fixture_environment(home);
    for (name, path) in [("FIXTURE_CAPTURE", &capture), ("FIXTURE_HELD", &held), ("FIXTURE_RELEASE", &release)] {
        native_environment.insert(name.into(), path.to_str().unwrap().into());
    }
    Orchestrator {
        hosts: under.hosts,
        node: under.node,
        bundle: under.bundle,
        native,
        native_environment,
        arguments: Box::new(move || {
            let capture: Value = serde_json::from_slice(&fs::read(&capture).unwrap()).unwrap();
            serde_json::from_value(capture["args"].clone()).unwrap()
        }),
        held: Box::new(move || {
            while !held.exists() {
                std::thread::sleep(Duration::from_millis(5));
            }
        }),
    }
}

/// The orchestrator model on the doubled hosts.
#[cfg(feature = "host_model")]
fn orchestrator(directory: &Path, home: &Path) -> Orchestrator {
    let under = under_test(directory, home);
    let model = alondra_claude_host_model::OrchestratorModel::default();
    let hosts = Arc::new(alondra_engine_doubles::agent_host::InProcessHosts::default());
    hosts.set_serve(model.clone());
    let (arguments, held) = (model.clone(), model.clone());
    Orchestrator {
        hosts,
        node: under.node,
        bundle: under.bundle,
        native: under.native,
        native_environment: under.native_environment,
        arguments: Box::new(move || arguments.arguments()),
        held: Box::new(move || {
            held.held();
        }),
    }
}

struct BatchOwner {
    epoch: DataEpoch,
    run: String,
}

impl HostOwner for BatchOwner {
    fn owner_generation(&self) -> u64 {
        1
    }
    fn epoch(&self) -> &DataEpoch {
        &self.epoch
    }
    fn host(&self) -> &str {
        "case-host"
    }
    fn wire_execution(&self) -> crate::Execution {
        crate::Execution::Orchestrator(OrchestratorExecution {
            lane: OrchestratorLane::Orchestrator,
            run_id: self.run.clone(),
            conversation_id: SESSION.into(),
            generation: 1,
        })
    }
}

/// The batches of one test, which share the native Claude's conversations as a daemon's do.
struct Batches {
    directory: test_support::Directory,
    under: Orchestrator,
    runs: usize,
}

impl Batches {
    fn new() -> Self {
        let directory = test_support::Directory::for_unix_socket();
        private_directory(&directory);
        let home = directory.join("home");
        private_directory(&home);
        let under = orchestrator(&directory, &home);
        Self { directory, under, runs: 0 }
    }

    fn start(&mut self, session: &str, resume: bool, prompt: &str) -> BatchHost<BatchOwner> {
        self.runs += 1;
        let (state, cwd) = (self.directory.join(format!("host-{}", self.runs)), self.directory.join(format!("batch-{}", self.runs)));
        private_directory(&state);
        private_directory(&cwd);
        let under = &self.under;
        let configuration = ClaudeConfiguration {
            artifact: VerifiedArtifact::verify(&under.node, &under.bundle, &hash(&under.node), &hash(&under.bundle)).unwrap(),
            state_directory: state,
            host_environment: test_support::fixture_environment(&self.directory.join("home")),
            native_executable: under.native.clone(),
            native_directory: cwd,
            native_environment: under.native_environment.clone(),
            profile_id: "alondra".into(),
            account_id: "case-account".into(),
            host_release: "case-host".into(),
            io_timeout: Duration::from_secs(300),
            confinement: None,
            hosts: under.hosts.clone(),
            worker: None,
        };
        let profile = OrchestratorProfile {
            model: "opus".into(),
            reasoning_effort: None,
            tools: vec!["Read".into(), "mcp__alondra-orchestrator".into()],
            disallowed_tools: Vec::new(),
            mcp_servers: BTreeMap::new(),
            plugins: Vec::new(),
            settings: serde_json::Map::new(),
        };
        let owner = BatchOwner { epoch: DataEpoch("case-epoch".into()), run: format!("case-run-{}", self.runs) };
        BatchHost::start(owner, &configuration, session, resume, &profile, prompt).unwrap()
    }

    /// The native Claude's own flags for the conversation it runs.
    fn conversation_flags(&self) -> Vec<String> {
        let arguments = (self.under.arguments)();
        ["--model", "--permission-mode", "--session-id", "--resume"].iter().filter_map(|name| {
            let at = arguments.iter().position(|argument| argument == name || argument.starts_with(&format!("{name}=")))?;
            let value = arguments[at].split_once('=').map_or_else(|| arguments[at + 1].clone(), |(_, value)| value.to_owned());
            Some(format!("{name} {value}"))
        }).collect()
    }
}

/// Every event of the batch until Claude exits, as the orchestrator reads it: each acknowledged
/// at a cursor above the last, a result named by whether it answers this batch's prompt.
fn read(host: &mut BatchHost<BatchOwner>, session: &str) -> Vec<String> {
    let (mut events, mut last) = (Vec::new(), None);
    loop {
        let pending = host.next_pending().unwrap();
        assert!(last.is_none_or(|last| pending.cursor > last), "cursor {} after {last:?}", pending.cursor);
        last = Some(pending.cursor);
        host.acknowledge(pending.cursor).unwrap();
        let event = match pending.event {
            HostEvent::Sdk(message) => {
                let field = |name: &str| message.get(name).and_then(Value::as_str).unwrap_or("-").to_owned();
                if message.get("session_id").is_some() {
                    assert_eq!(field("session_id"), session, "{message}");
                }
                match field("type").as_str() {
                    "result" => format!(
                        "result {} {} answers {}",
                        field("subtype"),
                        message["is_error"].as_bool().unwrap_or(false),
                        if field("user_message_uuid") == host.input() { "this prompt" } else { "another prompt" },
                    ),
                    "command_lifecycle" => format!("command_lifecycle {}", field("state")),
                    "system" => format!("system {}", field("subtype")),
                    other => other.to_owned(),
                }
            }
            HostEvent::Exited { code, signal } => {
                events.push(format!("exited {code:?} {signal:?}"));
                return events;
            }
            other => format!("{other:?}"),
        };
        events.push(event);
        assert!(events.len() < 64, "{events:#?}");
    }
}

#[test]
fn a_batch_answers_its_prompt_and_the_next_batch_resumes_its_conversation() {
    let mut batches = Batches::new();
    let mut first = batches.start(SESSION, false, "Plan the pending requests");
    let answered = [
        "command_lifecycle queued",
        "command_lifecycle started",
        "system init",
        "assistant",
        "result success false answers this prompt",
        "command_lifecycle completed",
        "exited Some(0) None",
    ];
    assert_eq!(read(&mut first, SESSION), answered);
    assert_eq!(batches.conversation_flags(), ["--model opus", "--permission-mode auto", &format!("--session-id {SESSION}")]);
    first.stop().unwrap();

    let mut resumed = batches.start(SESSION, true, "Plan the next requests");
    assert_eq!(read(&mut resumed, SESSION), answered);
    assert_eq!(batches.conversation_flags(), ["--model opus", "--permission-mode auto", &format!("--resume {SESSION}")]);
    resumed.stop().unwrap();
}

#[test]
fn a_batch_that_resumes_a_conversation_claude_never_had_ends_before_claude_initializes() {
    let mut batches = Batches::new();
    let mut lost = batches.start(UNKNOWN_SESSION, true, "Plan the pending requests");
    assert_eq!(read(&mut lost, UNKNOWN_SESSION), ["exited Some(3) None"]);
    lost.stop().unwrap();
}

#[test]
fn a_batch_stopped_while_claude_holds_its_turn_ends_without_a_result() {
    let mut batches = Batches::new();
    let mut held = batches.start(SESSION, false, "fixture:hold Plan the pending requests");
    let mut seen = Vec::new();
    while seen.last().is_none_or(|event: &HostEvent| !matches!(event, HostEvent::Sdk(message) if message["type"] == "assistant")) {
        let pending = held.next_pending().unwrap();
        held.acknowledge(pending.cursor).unwrap();
        seen.push(pending.event);
    }
    (batches.under.held)();
    held.stop().unwrap();
    assert!(!seen.iter().any(|event| matches!(event, HostEvent::Sdk(message) if message["type"] == "result")), "{seen:#?}");
}
