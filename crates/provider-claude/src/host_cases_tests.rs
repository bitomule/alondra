//! The behaviour cases of a Claude host, run by `sdk_worker_test` against the real Node host with
//! the fake native Claude, and by `claude_host_model_test` against the Claude host model on the
//! doubled hosts: the same steps and the same expectations, so the model cannot drift from the
//! host it stands for. One file per family of cases; this one is what they share.
#[path = "../../agent-application/tests/support/controller.rs"]
mod fixture_controller;
#[path = "host_cases/turns_tests.rs"]
mod turns;
#[path = "host_cases/arguments_tests.rs"]
mod arguments;
#[path = "host_cases/decisions_tests.rs"]
mod decisions;

use fixture_controller::WorkerController;
use super::*;
use alondra_agent_application::Error as ControllerError;
use alondra_agent_protocol::{FramedReader, Observation};
use alondra_engine_ports::agent_host::AgentHost;
use alondra_foundation::{ConversationId, HostId, InputId, InstanceId};
use alondra_store::{AuthorityScope, InputPayload};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, DirBuilder};
use std::io::Cursor;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex};
#[path = "../../test_support.rs"]
mod test_support;

/// What runs behind the port: the hosts, the executables a launch names, and what each native
/// Claude was started with.
struct UnderTest {
    hosts: Arc<dyn AgentHost>,
    node: PathBuf,
    bundle: PathBuf,
    native: PathBuf,
    native_environment: BTreeMap<String, String>,
    arguments: Box<dyn Fn() -> Vec<String>>,
}

fn hash(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

/// The real Node host with the fake native Claude, which records its command line in a capture.
#[cfg(feature = "sdk_fixture")]
fn under_test(directory: &Path, home: &Path) -> UnderTest {
    let runfile = |variable: &str| {
        PathBuf::from(std::env::var_os("TEST_SRCDIR").expect("Bazel runfiles root"))
            .join(std::env::var_os("TEST_WORKSPACE").expect("Bazel workspace"))
            .join(std::env::var_os(variable).expect("declared rootpath data"))
            .canonicalize()
            .expect("resolve actual declared runfile")
    };
    let native = directory.join("native");
    fs::copy(runfile("FIXTURE_NATIVE"), &native).unwrap();
    fs::set_permissions(&native, fs::Permissions::from_mode(0o700)).unwrap();
    let capture = directory.join("native-capture.json");
    let mut native_environment = test_support::fixture_environment(home);
    native_environment.insert("FIXTURE_CAPTURE".into(), capture.to_str().unwrap().into());
    UnderTest {
        hosts: Arc::new(alondra_agent_runtime::owned_host::ProcessHosts::new(Default::default())),
        node: runfile("FIXTURE_NODE"),
        bundle: runfile("FIXTURE_HOST"),
        native,
        native_environment,
        arguments: Box::new(move || {
            let capture: Value = serde_json::from_slice(&fs::read(&capture).unwrap()).unwrap();
            serde_json::from_value(capture["args"].clone()).unwrap()
        }),
    }
}

/// The model on the doubled hosts: no process, and files no one runs in place of the executables.
#[cfg(feature = "host_model")]
fn under_test(directory: &Path, home: &Path) -> UnderTest {
    let model = alondra_claude_host_model::ClaudeModel::new(home, Arc::default());
    model.set_turns(alondra_claude_host_model::Turns::Answered);
    let hosts = Arc::new(alondra_engine_doubles::agent_host::InProcessHosts::default());
    hosts.set_serve(model.clone());
    let file = |name: &str| {
        let path = directory.join(name);
        fs::write(&path, format!("the model plays {name}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path.canonicalize().unwrap()
    };
    UnderTest {
        hosts,
        node: file("node"),
        bundle: file("host.cjs"),
        native: file("native"),
        native_environment: test_support::fixture_environment(home),
        arguments: Box::new(move || model.starts().pop().expect("a native Claude started").arguments),
    }
}

/// What a case reads of one launch: the observations the host sent, as the engine reads them, and
/// the command line of the native Claude.
struct Launched {
    observations: Vec<String>,
    arguments: Vec<String>,
    /// Each decision the host asked the daemon for: its kind, tool use and payload.
    decisions: Vec<Value>,
    /// The folder the native Claude worked in.
    cwd: PathBuf,
}

fn private_directory(path: &Path) {
    DirBuilder::new().mode(0o700).create(path).unwrap();
}

/// The kind of each observation with what decides how the engine reads it: the type of the SDK
/// message, its subtype or state, and how a result ended.
fn summary(observation: &Observation) -> Option<String> {
    let Observation::Sdk { kind, payload, .. } = observation else { return None };
    let field = |name: &str| payload.get(name).and_then(Value::as_str).unwrap_or("-");
    let detail = match field("type") {
        "command_lifecycle" => field("state").to_owned(),
        "result" => format!("{} {} {}", field("subtype"), payload.get("is_error").and_then(Value::as_bool).unwrap_or(false), field("terminal_reason")),
        "system" => field("subtype").to_owned(),
        other => other.to_owned(),
    };
    let kind = serde_json::to_value(kind).ok()?.as_str()?.to_owned();
    Some(format!("{kind} {} {detail}", field("type")))
}

/// Launches a task's host with `worker` and offers it `prompt`; reads it until its turn's result,
/// which waits for the domain's transaction.
fn launch(prompt: &str, worker: Option<WorkerProfile>) -> Launched {
    let directory = test_support::Directory::for_unix_socket();
    private_directory(&directory);
    let (state, home, cwd) = (directory.join("host"), directory.join("home"), directory.join("workspace"));
    for path in [&state, &home, &cwd] {
        private_directory(path);
    }
    let under = under_test(&directory, &home);
    let database = directory.join("domain.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    connection.execute_batch(include_str!("../../domain-store-schema/schema.sql")).unwrap();
    connection.execute_batch("CREATE TABLE task_worktrees(task_id TEXT PRIMARY KEY,dir TEXT NOT NULL,at INTEGER NOT NULL); INSERT INTO domain_meta VALUES(1,'case-epoch',0,0); INSERT INTO requests(id,body,closed) VALUES('case-request','Work',0); INSERT INTO agents(id,name) VALUES('case-node','Worker'); INSERT INTO tasks(id,request_id,title,state,revision,started,review_prs,archived) VALUES('case-task','case-request','Case task','not_started',0,0,0,0)").unwrap();
    connection.execute("INSERT INTO task_worktrees VALUES('case-task',?1,0)", [cwd.to_str().unwrap()]).unwrap();
    let configuration = ClaudeConfiguration {
        artifact: VerifiedArtifact::verify(&under.node, &under.bundle, &hash(&under.node), &hash(&under.bundle)).unwrap(),
        state_directory: state,
        host_environment: test_support::fixture_environment(&home),
        native_executable: under.native.clone(),
        native_directory: cwd.clone(),
        native_environment: under.native_environment.clone(),
        profile_id: "case-profile".into(),
        account_id: "case-account".into(),
        host_release: "case-host".into(),
        io_timeout: Duration::from_secs(30),
        confinement: None,
        hosts: under.hosts.clone(),
        worker,
    };
    let binding = alondra_provider_contract::ProviderBinding::new(
        "case-binding".into(), "claude-agent-sdk".into(), "case-instance".into(), "case-fixture".into(), 1, 1,
        BTreeSet::from([alondra_provider_contract::Capability::OfferInput]), "fixture-no-login".into(), 1,
    ).unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let adapter = Recording { inner: ClaudeAdapter::new(binding, configuration).unwrap(), received: received.clone() };
    let mut controller = WorkerController::open(connection, AuthorityScope::new("case-epoch".into()).unwrap(), adapter).unwrap();
    controller.launch(
        &"case-task".into(),
        InputPayload::new(InputId::new("case-initial").unwrap(), prompt).unwrap(),
        alondra_domain::Event::Admit {
            node: "case-node".into(),
            conversation: ConversationId::new("case-conversation").unwrap(),
            instance: InstanceId::new("case-execution").unwrap(),
            host: HostId::new("case-host").unwrap(),
        },
        Placement::LocalDirectory(cwd.to_str().unwrap().into()),
    ).unwrap();
    let permit = controller.next_permit().expect("the launch's permit");
    let attempt = controller.claim(permit).unwrap();
    assert_eq!(controller.dispatch(attempt).unwrap(), Handoff::Offered);
    let mut reached = false;
    for _ in 0..32 {
        match controller.observe() {
            Err(ControllerError::RequiresDomainCommand) => {
                reached = true;
                break;
            }
            Ok(true) => {}
            other => panic!("unexpected observation: {other:?}"),
        }
    }
    assert!(reached, "the turn reached no result");
    drop(controller);
    let events: Vec<Observation> = received.lock().unwrap().iter().filter_map(|batch: &OrderedObservation| {
        let mut framed = u32::try_from(batch.evidence.len()).unwrap().to_be_bytes().to_vec();
        framed.extend_from_slice(&batch.evidence);
        let frame = FramedReader::new(Cursor::new(framed)).read_received().unwrap().unwrap();
        match &frame.frame().body {
            Body::Event(event) => Some(event.observation.clone()),
            _ => None,
        }
    }).collect();
    let decisions = events.iter().filter_map(|observation| match observation {
        Observation::DecisionRequested { kind, tool_use_id, payload, .. } => Some(serde_json::json!({"kind": kind, "tool_use_id": tool_use_id, "payload": payload})),
        _ => None,
    }).collect();
    let observations = events.iter().filter_map(summary).collect();
    Launched { observations, arguments: (under.arguments)(), decisions, cwd: cwd.canonicalize().unwrap() }
}

/// The real adapter, recording each batch it hands the controller and stopping its host when the
/// controller lets it go.
struct Recording {
    inner: ClaudeAdapter,
    received: Arc<Mutex<Vec<OrderedObservation>>>,
}
impl Drop for Recording {
    fn drop(&mut self) {
        if let Some(host) = self.inner.host.as_mut() {
            let _ = host.request_stop();
            let _ = host.wait();
        }
    }
}
impl DurableAdapter for Recording {
    type Normalizer = Normalizer;
    fn normalizer(&self) -> &Normalizer {
        self.inner.normalizer()
    }
    fn owner_generation(&self) -> Result<u64, AdapterError> {
        self.inner.owner_generation()
    }
    fn stage_input(&mut self, metadata: InputMetadata<'_, '_>) -> Result<(), AdapterError> {
        self.inner.stage_input(metadata)
    }
    fn load_input(&mut self, metadata: InputMetadata<'_, '_>) -> Result<InputRecord, AdapterError> {
        self.inner.load_input(metadata)
    }
    fn install_owner(&mut self, owner: OwnerGrant, state: AdapterState) -> Result<(), AdapterError> {
        self.inner.install_owner(owner, state)
    }
}
impl Adapter for Recording {
    type NativeInput = InputRecord;
    fn binding(&self) -> &ProviderBinding {
        self.inner.binding()
    }
    fn prepare(&mut self, invocation: Invocation<'_>, native: InputRecord) -> Result<PreparedCommand, AdapterError> {
        self.inner.prepare(invocation, native)
    }
    fn submit(&mut self, invocation: Invocation<'_>, command: &PreparedCommand) -> Result<Handoff, AdapterError> {
        self.inner.submit(invocation, command)
    }
    fn next(&mut self) -> Result<Option<OrderedObservation>, AdapterError> {
        let next = self.inner.next()?;
        if let Some(batch) = &next {
            self.received.lock().unwrap().push(batch.clone());
        }
        Ok(next)
    }
    fn acknowledge(&mut self, stamp: &ExecutionStamp, cursor: u64) -> Result<(), AdapterError> {
        self.inner.acknowledge(stamp, cursor)
    }
}
