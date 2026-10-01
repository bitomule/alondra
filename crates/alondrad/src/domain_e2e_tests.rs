//! One v2-only daemon on a temporary HOME. A fake Claude session speaks to it the
//! way a real one does: through the real `alondra mcp-node` binary with the
//! capability the scheduler issued, and through `alondra hook` on the hook socket.
use crate::domain_bootstrap::{V2Only, router};
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain_v2::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::sync::{Arc, Mutex};

fn alondra_binary() -> PathBuf {
    let path = option_env!("CARGO_BIN_EXE_alondra")
        .expect("the alondra binary is a Bazel data dependency of this test");
    std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
}

struct Daemon {
    domain: Arc<DomainRuntime>,
    port: u16,
    server: tokio::task::JoinHandle<()>,
    hooks: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Daemon {
    async fn start(home: &Path) -> Self {
        drop(DomainRuntime::open(home).unwrap());
        let state = V2Only::open(home).unwrap();
        let domain = state.domain();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = router(state).layer(axum::middleware::from_fn_with_state(
            Arc::new(crate::auth::Auth::open(home).unwrap()),
            crate::auth::guard,
        ));
        let server = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
        });
        let socket = crate::hook_socket::bind(home).await.unwrap();
        let hooks = tokio::spawn(crate::domain_transport::serve(socket, domain.clone()));
        Self { domain, port, server, hooks }
    }

    async fn stop(self) {
        self.server.abort();
        self.hooks.abort();
        let _ = self.server.await;
        let _ = self.hooks.await;
        drop(self.domain);
    }

    fn execute(&self, actor: Actor, id: &str, command: Command) -> CommandOutcome {
        let state = self.domain.state().unwrap();
        self.domain.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch,
            command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap()
    }
}

/// The fake session's MCP side: the real entrypoint, started as Claude Code
/// would start it, with the environment the launcher handed the session.
fn mcp(home: &Path, port: u16, credential: (&str, &str), calls: Vec<(&str, Value)>) -> Vec<Value> {
    let mut child = Process::new(alondra_binary())
        .arg("mcp-node")
        .env("ALONDRA_HOME", home)
        .env("ALONDRA_PORT", port.to_string())
        .env("ALONDRA_DOMAIN_PROTOCOL", "2")
        .env_remove("ALONDRA_DOMAIN_CAPABILITY")
        .env(credential.0, credential.1)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    let stdin = child.stdin.as_mut().unwrap();
    writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18"}})).unwrap();
    for (index, (tool, arguments)) in calls.iter().enumerate() {
        writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":index + 1,"method":"tools/call",
            "params":{"name":tool,"arguments":arguments}})).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let replies: Vec<Value> = output.stdout.split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap()).collect();
    assert_eq!(replies.len(), calls.len() + 1, "{replies:?}");
    replies[1..].to_vec()
}

fn tool_text(reply: &Value) -> (bool, String) {
    let result = &reply["result"];
    (result["isError"] == true, result["content"][0]["text"].as_str().unwrap_or_default().to_owned())
}

/// The fake session's hook side: what Claude Code runs on each lifecycle event.
fn hook(home: &Path, payload: Value) -> Value {
    let mut child = Process::new(alondra_binary())
        .arg("hook")
        .env("ALONDRA_HOME", home)
        .env("ALONDRA_LAUNCHED", "1")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    child.stdin.as_mut().unwrap().write_all(payload.to_string().as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let text = String::from_utf8_lossy(&output.stdout);
    if text.trim().is_empty() { json!({}) } else { serde_json::from_str(text.trim()).unwrap() }
}

const SESSION: &str = "ab1234cd-0000-4000-8000-000000000000";

struct CapturingLauncher(Mutex<Vec<Vec<String>>>);
impl crate::orchestrate::Launcher for CapturingLauncher {
    async fn run(&self, _: &str, args: &[String]) -> Result<String, crate::orchestrate::Refusal> {
        self.0.lock().unwrap().push(args.to_vec());
        Ok("backgrounded · ab1234 · scheduled/run".into())
    }
    async fn session_of(&self, short: &str) -> Option<String> {
        (short == "ab1234").then(|| SESSION.to_owned())
    }
    fn usage(&self) -> Option<alondra_core::usage::Usage> { None }
    fn plugin(&self) -> Option<PathBuf> { Some("/synthetic/plugin".into()) }
}

fn tool_args(epoch: &str, id: &str, expected_revision: u64, command: Value) -> Value {
    let mut command = command.as_object().unwrap().clone();
    command.remove("kind");
    let mut args = json!({"data_epoch":epoch,"command_id":id,"expected_revision":expected_revision}).as_object().unwrap().clone();
    args.extend(command);
    Value::Object(args)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scheduled_session_commands_and_receives_its_answer_once_across_a_daemon_restart() {
    let root = std::env::temp_dir().join(format!("alondra-v2-e2e-{}", crate::auth::random_hex(8)));
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    std::fs::write(alondra_core::options::path_in(&home),
        format!("projects_root = {:?}\n", root.join("projects").to_str().unwrap())).unwrap();
    let daemon = Daemon::start(&home).await;
    daemon.execute(Actor::System, "schedule", Command::CreateSchedule { schedule: Schedule {
        id: "schedule-1".into(), title: "Nightly review".into(), text: "Review the project".into(),
        project: None, options: BTreeMap::new(), disallowed_tools: vec![],
        frequency: ScheduleFrequency::Daily, hour: 9, minute: 0, weekday: None, interval: None,
        start_at: None, enabled: false, next_run: None, revision: 1, deleted_at: None,
    }});
    daemon.execute(Actor::System, "claim", Command::ClaimScheduleRun {
        schedule_id: "schedule-1".into(), run_id: "run-1".into(), origin: ScheduleRunOrigin::Manual,
        now: alondra_core::now_millis(), next_after_now: None, expected_schedule_revision: None,
    });
    let launcher = CapturingLauncher(Mutex::new(vec![]));
    crate::domain_schedule_runner::run_cycle(&daemon.domain, &home, false, &launcher).await.unwrap();
    let args = launcher.0.lock().unwrap().pop().expect("one launch");
    let settings = args.windows(2).find(|pair| pair[0] == "--settings").unwrap()[1].clone();
    let written: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    let capability = written["env"]["ALONDRA_DOMAIN_CAPABILITY"].as_str().unwrap().to_owned();
    let from_settings = ("ALONDRA_DOMAIN_SETTINGS", settings.as_str());
    let session = SESSION.to_owned();
    let state = daemon.domain.state().unwrap();
    let task = state.tasks.values().next().unwrap().clone();
    assert_eq!(task.agent_id.as_ref().unwrap().0, session);
    let epoch = state.data_epoch.0.clone();

    // The session reports through the MCP and asks the user a question.
    let scope = json!({"kind": "task", "task_id": task.id.0});
    let replies = mcp(&home, daemon.port, from_settings, vec![
        ("publish_information", tool_args(&epoch, "info-1", 0, json!({"kind": "publish_information",
            "interaction_id": "info-1", "scope": scope, "recipient": {"kind": "user"}, "body": "Started the review"}))),
        ("ask", tool_args(&epoch, "question-1", 0, json!({"kind": "ask_question", "interaction_id": "question-1",
            "scope": scope, "recipient": {"kind": "user"}, "body": "Merge the fix?", "blocking": true,
            "options": [{"id": "yes", "label": "Yes", "inputs": []}, {"id": "no", "label": "No", "inputs": []}]}))),
        ("complete_task", tool_args(&epoch, "complete-early", 0, json!({"kind": "complete_task",
            "task_id": task.id.0, "presentation_id": "info-1"}))),
    ]);
    assert!(!tool_text(&replies[0]).0, "{}", tool_text(&replies[0]).1);
    assert!(!tool_text(&replies[1]).0, "{}", tool_text(&replies[1]).1);
    let (refused, text) = tool_text(&replies[2]);
    assert!(refused && text.contains("acceptance_missing") || text.contains("invalid_action_for_kind"), "{text}");
    let state = daemon.domain.state().unwrap();
    assert!(matches!(state.interactions[&"question-1".into()].payload, InteractionPayload::Question { .. }));
    assert_ne!(state.tasks[&task.id].state, TaskState::Done);

    // The user answers; the session ends its turn and receives the answer.
    daemon.execute(Actor::User { user_id: "device:phone".into() }, "answer", Command::AnswerQuestion {
        attachments: vec![], interaction_id: "question-1".into(), option_id: "yes".into(), inputs: BTreeMap::new(),
    });
    assert_eq!(hook(&home, json!({"session_id": "stranger", "hook_event_name": "Stop"})), json!({}));
    let stop = hook(&home, json!({"session_id": session, "hook_event_name": "Stop"}));
    let context = stop["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_owned();
    assert_eq!(context, "Yes.");
    assert!(!context.contains("data_epoch") && !context.contains("revision=")
        && !context.contains("interaction_id=") && !context.contains("option_id"), "{stop}");
    let evidence = daemon.domain.store.lock().unwrap().delivery_evidence_page(0).unwrap();
    assert_eq!(evidence.iter().filter(|item| item.recipient_kind == "agent").count(), 1);
    assert!(evidence.iter().all(|item| item.recipient_kind != "agent" || item.state == "handed"));

    // The daemon restarts: the session keeps acting with its bearer and is not handed the answer again.
    daemon.stop().await;
    let daemon = Daemon::start(&home).await;
    let replies = mcp(&home, daemon.port, from_settings, vec![
        ("publish_information", tool_args(&epoch, "info-2", 0, json!({"kind": "publish_information",
            "interaction_id": "info-2", "scope": scope, "recipient": {"kind": "user"}, "body": "Acting on the answer"}))),
    ]);
    assert!(!tool_text(&replies[0]).0, "{}", tool_text(&replies[0]).1);
    assert!(daemon.domain.state().unwrap().interactions.contains_key(&"info-2".into()));
    assert_eq!(hook(&home, json!({"session_id": session, "hook_event_name": "Stop"})), json!({}));
    let evidence = daemon.domain.store.lock().unwrap().delivery_evidence_page(0).unwrap();
    assert_eq!(evidence.iter().filter(|item| item.recipient_kind == "agent").count(), 1, "no second delivery");

    // The session ends: its bearer stops working.
    assert_eq!(hook(&home, json!({"session_id": session, "hook_event_name": "SessionEnd"})), json!({}));
    let replies = mcp(&home, daemon.port, ("ALONDRA_DOMAIN_CAPABILITY", &capability), vec![
        ("publish_information", tool_args(&epoch, "info-3", 0, json!({"kind": "publish_information",
            "interaction_id": "info-3", "scope": scope, "recipient": {"kind": "user"}, "body": "Too late"}))),
    ]);
    let (refused, text) = tool_text(&replies[0]);
    assert!(refused && text.contains("forbidden"), "{text}");
    assert!(!daemon.domain.state().unwrap().agents[&AgentId(session.clone())].connected);
    assert!(!Path::new(&settings).exists(), "the session's settings file goes with it");
    daemon.stop().await;
    std::fs::remove_dir_all(root).unwrap();
}
