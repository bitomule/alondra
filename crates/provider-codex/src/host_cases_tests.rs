//! An orchestrator batch of Codex, run by `codex_host_test` on the real Codex host
//! (`agent-host/codex-session.ts`) with the fake app-server (`agent-host/fake-codex.py`), and by
//! `provider_codex_test` on the Codex host model: the same steps and the same expectations, so the
//! model cannot drift from the host it stands for.
use crate::test_support;
use crate::{BatchHost, CodexConfiguration, Execution, HostEvent, HostOwner, McpServer, OrchestratorExecution, OrchestratorLane, OrchestratorProfile};
use alondra_engine_ports::agent_host::{AgentHost, VerifiedArtifact};
use alondra_foundation::DataEpoch;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, DirBuilder};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const SESSION: &str = "00000000-0000-4000-8000-00000000c0de";

/// What runs behind the port: the hosts, and the executables a launch names.
struct UnderTest {
    hosts: Arc<dyn AgentHost>,
    host: PathBuf,
    native: PathBuf,
    /// What went wrong behind the port, when a model plays it.
    trouble: Box<dyn Fn() -> Option<String>>,
}

/// The real Codex host, Node and host in one executable, with the fake app-server.
#[cfg(feature = "host_fixture")]
fn under_test(directory: &Path) -> UnderTest {
    let runfile = |variable: &str| {
        PathBuf::from(std::env::var_os("TEST_SRCDIR").expect("Bazel runfiles root"))
            .join(std::env::var_os("TEST_WORKSPACE").expect("Bazel workspace"))
            .join(std::env::var_os(variable).expect("declared rootpath data"))
            .canonicalize()
            .expect("resolve actual declared runfile")
    };
    let native = directory.join("codex-native");
    fs::copy(runfile("FIXTURE_CODEX_NATIVE"), &native).unwrap();
    fs::set_permissions(&native, fs::Permissions::from_mode(0o700)).unwrap();
    UnderTest {
        hosts: Arc::new(alondra_agent_runtime::owned_host::ProcessHosts::new(Default::default())),
        host: runfile("FIXTURE_CODEX_HOST"),
        native,
        trouble: Box::new(|| None),
    }
}

/// The Codex host model on the doubled hosts: no process, and files no one runs in place of the
/// executables.
#[cfg(feature = "host_model")]
fn under_test(directory: &Path) -> UnderTest {
    let (hosts, model) = (Arc::new(alondra_engine_doubles::agent_host::InProcessHosts::default()), alondra_codex_host_model::CodexModel::default());
    hosts.set_serve(model.clone());
    let file = |name: &str| {
        let path = directory.join(name);
        fs::write(&path, format!("the model plays {name}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path.canonicalize().unwrap()
    };
    UnderTest { hosts, host: file("codex-host.cjs"), native: file("codex-native"), trouble: Box::new(move || model.trouble()) }
}

struct BatchOwner {
    epoch: DataEpoch,
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
    fn wire_execution(&self) -> Execution {
        Execution::Orchestrator(OrchestratorExecution {
            lane: OrchestratorLane::Orchestrator,
            run_id: "case-run".into(),
            conversation_id: SESSION.into(),
            generation: 1,
        })
    }
}

fn hash(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

fn private_directory(path: &Path) {
    DirBuilder::new().mode(0o700).create(path).unwrap();
}

/// One batch of the orchestrator on Codex, from its start to its host's end, and the native
/// requests the app-server got.
struct Batch {
    events: Vec<String>,
    transcript: Vec<Value>,
}

/// Runs one batch whose fake app-server plays `mode`, reading every event as the orchestrator
/// does: each acknowledged at a cursor above the last, until Codex exits.
fn batch(mode: &str) -> Batch {
    let directory = test_support::Directory::for_unix_socket();
    private_directory(&directory);
    let (home, state, cwd) = (directory.join("home"), directory.join("host"), directory.join("batch"));
    for folder in [&home, &state, &cwd] {
        private_directory(folder);
    }
    let under = under_test(&directory);
    let transcript = directory.join("codex-transcript");
    let mut native_environment = test_support::fixture_environment(&home);
    native_environment.extend(BTreeMap::from([
        ("ALONDRA_FAKE_MODE".into(), mode.into()),
        ("ALONDRA_FAKE_TRANSCRIPT".into(), transcript.display().to_string()),
        ("ALONDRA_FAKE_FINAL_TEXT".into(), "Planned the pending requests".into()),
        ("ALONDRA_HOME".into(), home.display().to_string()),
        ("ALONDRA_ORCHESTRATOR".into(), "1".into()),
        ("ALONDRA_ORCHESTRATOR_EXECUTION".into(), "case-run".into()),
        ("ALONDRA_PROVIDER".into(), "codex".into()),
        ("ALONDRA_SESSION_ID".into(), "orchestrator".into()),
        ("ALONDRA_DOMAIN_CAPABILITY".into(), "case-capability".into()),
        ("ALONDRA_DOMAIN_PROTOCOL".into(), "2".into()),
    ]));
    let configuration = CodexConfiguration {
        artifact: VerifiedArtifact::verify(&under.host, &under.host, &hash(&under.host), &hash(&under.host)).unwrap(),
        state_directory: state,
        host_environment: test_support::fixture_environment(&home),
        native_executable: under.native.clone(),
        native_directory: cwd.clone(),
        native_environment,
        profile_id: "alondra".into(),
        account_id: "case-account".into(),
        host_release: env!("CARGO_PKG_VERSION").into(),
        io_timeout: Duration::from_secs(60),
        confinement: None,
        hosts: under.hosts.clone(),
        worker: None,
    };
    let profile = OrchestratorProfile {
        model: "fixture-model".into(),
        reasoning_effort: Some("high".into()),
        tools: Vec::new(),
        disallowed_tools: Vec::new(),
        mcp_servers: BTreeMap::from([("alondra-orchestrator".into(), McpServer { command: directory.join("alondra"), args: vec!["mcp-orchestrator".into()] })]),
        plugins: Vec::new(),
        settings: json!({"codex": {
            "developerInstructions": "You are the Alondra orchestrator. Use only Alondra role tools.",
            "nativeAccountPolicy": {"expectedEmail": "first@example.test", "expectedWorkspaceId": "fixture-workspace"},
        }})
        .as_object()
        .unwrap()
        .clone(),
    };
    let owner = BatchOwner { epoch: DataEpoch("case-epoch".into()) };
    let mut host = BatchHost::start(owner, &configuration, SESSION, false, &profile, "Plan the pending requests")
        .unwrap_or_else(|error| panic!("the batch did not start: {error:?} ({:?}) {:?}", (under.trouble)(), left_behind(&configuration.state_directory)));
    let (mut events, mut last) = (Vec::new(), None);
    loop {
        let pending = host.next_pending().unwrap();
        assert!(last.is_none_or(|last| pending.cursor > last), "cursor {} after {last:?}", pending.cursor);
        last = Some(pending.cursor);
        host.acknowledge(pending.cursor).unwrap();
        let ended = matches!(pending.event, HostEvent::Exited { .. });
        events.push(match pending.event {
            HostEvent::Sdk(message) => message["method"].as_str().unwrap_or("-").to_owned(),
            other => format!("{other:?}"),
        });
        assert!(events.len() < 128, "{events:#?}");
        if ended {
            break;
        }
    }
    let places = [(cwd.display().to_string(), "<cwd>"), (directory.display().to_string(), "<fixture>"), (host.input().to_owned(), "<input>")];
    let transcript = fs::read_to_string(&transcript)
        .unwrap()
        .lines()
        .map(|line| {
            let mut message: Value = serde_json::from_str(line).unwrap();
            if let Some(fields) = message.as_object_mut()
                && fields.contains_key("method")
            {
                fields.remove("id");
            }
            let mut text = message.to_string();
            for (place, name) in &places {
                text = text.replace(place.as_str(), name);
            }
            serde_json::from_str(&text).unwrap()
        })
        .collect();
    host.stop().unwrap();
    Batch { events, transcript }
}

/// What a host that did not start left in its folder, to say why.
fn left_behind(state: &Path) -> Vec<(String, String)> {
    fs::read_dir(state).map_or_else(|_| Vec::new(), |entries| {
        entries.filter_map(Result::ok).map(|entry| {
            let text = fs::read(entry.path()).map(|bytes| String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]).into_owned()).unwrap_or_default();
            (entry.file_name().to_string_lossy().into_owned(), text)
        }).collect()
    })
}

fn methods(transcript: &[Value]) -> Vec<String> {
    transcript.iter().map(|message| message["method"].as_str().map_or_else(|| format!("answer {}", message["id"]), str::to_owned)).collect()
}

fn request<'a>(transcript: &'a [Value], method: &str) -> &'a Value {
    let mut found = transcript.iter().filter(|message| message["method"] == method);
    let request = found.next().unwrap_or_else(|| panic!("no {method} in {transcript:#?}"));
    assert!(found.next().is_none(), "{method} twice in {transcript:#?}");
    &request["params"]
}

#[test]
fn a_codex_batch_opens_one_read_only_thread_runs_its_one_turn_and_its_host_ends_when_the_turn_completes() {
    let batch = batch("complete");
    assert_eq!(batch.events, EVENTS_OF_A_COMPLETED_TURN, "{:#?}", batch.events);
    assert_eq!(methods(&batch.transcript), NATIVE_REQUESTS, "{:#?}", batch.transcript);
    assert_eq!(request(&batch.transcript, "thread/start"), &thread_start(), "{:#?}", batch.transcript);
    assert_eq!(request(&batch.transcript, "turn/start"), &turn_start(), "{:#?}", batch.transcript);
}

#[test]
fn a_codex_batch_refuses_the_native_request_of_its_turn_and_still_ends_when_the_turn_completes() {
    let batch = batch("permission");
    assert_eq!(batch.events, EVENTS_OF_A_COMPLETED_TURN, "{:#?}", batch.events);
    assert_eq!(methods(&batch.transcript), [NATIVE_REQUESTS, &["answer 88"]].concat(), "{:#?}", batch.transcript);
    assert_eq!(
        batch.transcript.last().unwrap(),
        &json!({"id": 88, "error": {"code": -32601, "message": "This Alondra host does not expose that request"}}),
        "the app-server's request for approval is refused, never granted",
    );
}

/// What the app-server is asked, in order: who the account is before the thread opens and again
/// before its one turn starts.
const NATIVE_REQUESTS: &[&str] = &[
    "initialize",
    "initialized",
    "config/read",
    "getAuthStatus",
    "account/read",
    "account/rateLimits/read",
    "config/read",
    "thread/start",
    "config/read",
    "getAuthStatus",
    "account/read",
    "account/rateLimits/read",
    "turn/start",
];

/// What the orchestrator reads of a batch whose turn completed: the session's own events, the
/// app-server's notifications raw, and the app-server ended by the host once the turn completed.
const EVENTS_OF_A_COMPLETED_TURN: &[&str] = &[
    "alondra/orchestratorPolicy",
    "alondra/threadReady",
    "alondra/inputStarting",
    "alondra/inputAccepted",
    "turn/started",
    "item/started",
    "item/agentMessage/delta",
    "item/completed",
    "turn/completed",
    "Exited { code: None, signal: Some(\"SIGTERM\") }",
];

/// A read-only thread that asks nothing, with the app-server's inherited MCP server and every native
/// tool family off and the orchestrator's MCP server required.
fn thread_start() -> Value {
    json!({
        "approvalPolicy": "never",
        "approvalsReviewer": "user",
        "config": {
            "agents.enabled": false,
            "features.apps": false,
            "features.code_mode": false,
            "features.code_mode_host": false,
            "features.code_mode_host_grpc": false,
            "features.code_mode_only": false,
            "features.code_mode_only_strict_3p_tools": false,
            "features.code_mode_prewarm": false,
            "features.code_mode_tool_search": false,
            "features.goals": false,
            "features.hooks": false,
            "features.image_generation": false,
            "features.imagegenext": false,
            "features.multi_agent": false,
            "features.multi_agent_v2": {"disable_native_tools": true, "enabled": false},
            "features.plugins": false,
            "features.shell_tool": false,
            "features.skill_mcp_dependency_install": false,
            "features.sleep_tool": false,
            "features.stable_environment_tools": false,
            "mcp_servers.alondra-orchestrator": {
                "args": ["mcp-orchestrator"],
                "command": "<fixture>/alondra",
                "default_tools_approval_mode": "approve",
                "enabled": true,
                "env": {
                    "ALONDRA_CODEX_THREAD_MAPPING": format!("<fixture>/native-threads/{SESSION}"),
                    "ALONDRA_DOMAIN_CAPABILITY": "case-capability",
                    "ALONDRA_DOMAIN_PROTOCOL": "2",
                    "ALONDRA_HOME": "<fixture>/home",
                    "ALONDRA_ORCHESTRATOR": "1",
                    "ALONDRA_ORCHESTRATOR_EXECUTION": "case-run",
                    "ALONDRA_PROVIDER": "codex",
                    "ALONDRA_SESSION_ID": "orchestrator",
                },
                "env_vars": [],
                "required": true,
                "tool_timeout_sec": 3900,
            },
            "mcp_servers.inherited-danger.enabled": false,
            "tools.experimental_request_user_input.enabled": false,
            "tools.update_plan.enabled": false,
            "web_search": "disabled",
        },
        "cwd": "<cwd>",
        "developerInstructions": "You are the Alondra orchestrator. Use only Alondra role tools.",
        "environments": [],
        "ephemeral": false,
        "model": "fixture-model",
        "sandbox": "read-only",
    })
}

/// The batch's one prompt as its one turn, read-only, at the profile's model and effort.
fn turn_start() -> Value {
    json!({
        "approvalPolicy": "never",
        "approvalsReviewer": "user",
        "clientUserMessageId": "<input>",
        "collaborationMode": {"mode": "default", "settings": {"developer_instructions": null, "model": "fixture-model", "reasoning_effort": "high"}},
        "cwd": "<cwd>",
        "effort": "high",
        "environments": [],
        "input": [{"text": "Plan the pending requests", "text_elements": [], "type": "text"}],
        "model": "fixture-model",
        "sandboxPolicy": {"type": "readOnly"},
        "threadId": "00000000-0000-4000-8000-000000000001",
    })
}
