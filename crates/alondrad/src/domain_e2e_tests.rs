//! One v2-only daemon on a temporary HOME. A fake Claude session speaks to it the
//! way a real one does: through the real `alondra mcp-node` binary with the
//! capability the daemon issued it, and through `alondra hook` on the hook socket.
use crate::domain_bootstrap::{DomainDaemon, router};
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::sync::Arc;

fn alondra_binary() -> PathBuf {
    let path = option_env!("CARGO_BIN_EXE_alondra")
        .expect("the alondra binary is a Bazel data dependency of this test");
    std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
}

struct Daemon {
    domain: Arc<DomainRuntime>,
    auth: Arc<crate::auth::Auth>,
    port: u16,
    server: tokio::task::JoinHandle<()>,
    hooks: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Daemon {
    async fn start(home: &Path) -> Self {
        drop(DomainRuntime::open(home).unwrap());
        let state = DomainDaemon::open(home).unwrap();
        let domain = state.domain();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let auth = state.auth.clone();
        let state_auth = auth.clone();
        let app = router(state).layer(axum::middleware::from_fn_with_state(auth, crate::auth::guard));
        let server = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
        });
        let socket = crate::hook_socket::bind(home).await.unwrap();
        let hooks = tokio::spawn(crate::domain_transport::serve(socket, domain.clone()));
        let auth = state_auth;
        Self { domain, auth, port, server, hooks }
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
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
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

/// The fake session's node mod: what `plugin/hooks/register.ts` runs for each report.
fn node_event(home: &Path, report: Value) {
    let mut child = Process::new(alondra_binary())
        .args(["node", "event"])
        .env("ALONDRA_HOME", home)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    child.stdin.as_mut().unwrap().write_all(report.to_string().as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

/// What the node mod runs once its session takes a message: `alondra node ack <id>`.
fn node_ack(home: &Path, ids: &[&str]) {
    let mut child = Process::new(alondra_binary())
        .args(["node", "ack"]).args(ids)
        .env("ALONDRA_HOME", home)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    child.stdin.as_mut().unwrap().write_all(json!({"session_id": SESSION}).to_string().as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

type Pushed = std::io::BufReader<std::process::ChildStdout>;

/// The next JSON line the helper copied from the daemon, or none within `wait`.
fn next_line(reader: &mut Option<Pushed>, wait: std::time::Duration) -> Option<Value> {
    let mut taken = reader.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        std::io::BufRead::read_line(&mut taken, &mut line).unwrap();
        let _ = sender.send((taken, line));
    });
    let (back, line) = receiver.recv_timeout(wait).ok()?;
    *reader = Some(back);
    (!line.is_empty()).then(|| serde_json::from_str(&line).unwrap())
}

const SESSION: &str = "ab1234cd-0000-4000-8000-000000000000";

/// A `claude --bg` node of the previous release working on a task of its own: the node, the task
/// and the bearer the daemon issued it, which the node's MCP server presents.
fn bg_node_with_a_task(daemon: &Daemon, home: &Path, title: &str) -> (String, TaskId) {
    daemon.execute(Actor::Orchestrator { generation: 0 }, "request", Command::CreateRequest {
        request_id: "r-1".into(), body: title.into(), auto_merge: false, maintenance: false, message: false,
    });
    daemon.execute(Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask {
        plan_items: None, continues: None, task_id: "t-1".into(), request_id: "r-1".into(), title: title.into(), agent_id: None,
        review_prs: false, dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("node", "/work")),
    });
    node_event(home, json!({"session_id": SESSION, "event": "session.start", "version": "2.1.287", "cwd": "/work"}));
    daemon.execute(Actor::System, "register", Command::RegisterAgent { agent_id: SESSION.into(), name: "scheduled/run-1".into(), at: 0 });
    daemon.execute(Actor::User { user_id: "u".into() }, "assign", Command::AssignTask { task_id: "t-1".into(), agent_id: SESSION.into(), model: None });
    let capability = daemon.domain.bind_agent(SESSION).unwrap();
    (capability.expose_for_child_environment().to_owned(), TaskId::from("t-1"))
}

fn tool_args(epoch: &str, id: &str, expected_revision: u64, command: Value) -> Value {
    let mut command = command.as_object().unwrap().clone();
    command.remove("kind");
    if let Some(need) = command.get("need").cloned() { command.entry("title").or_insert(need); }
    let mut args = json!({"data_epoch":epoch,"command_id":id,"expected_revision":expected_revision}).as_object().unwrap().clone();
    args.extend(command);
    Value::Object(args)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claude_bg_node_commands_and_receives_its_answer_once_across_a_daemon_restart() {
    let scratch_directory = crate::test_support::Directory::new();
    let root = scratch_directory.parent().unwrap().join(format!("alondra-e2e-{}", crate::auth::random_hex(8)));
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    std::fs::write(alondra_core::options::path_in(&home),
        format!("projects_root = {:?}\n", root.join("projects").to_str().unwrap())).unwrap();
    let daemon = Daemon::start(&home).await;
    let (capability, task) = bg_node_with_a_task(&daemon, &home, "Nightly review");
    let node_bearer = ("ALONDRA_DOMAIN_CAPABILITY", capability.as_str());
    let session = SESSION.to_owned();
    let state = daemon.domain.state().unwrap();
    let task = state.tasks[&task].clone();
    assert_eq!(task.agent_id.as_ref().unwrap().0, session);
    let epoch = state.data_epoch.0.clone();

    // The session reports through the MCP and asks the user a question.
    let scope = json!({"kind": "task", "task_id": task.id.0});
    let replies = mcp(&home, daemon.port, node_bearer, vec![
        ("present_result", tool_args(&epoch, "info-1", 0, json!({"kind": "present_result", "task_id": task.id.0,
            "interaction_id": "info-1", "need": "The review started", "body": "Started the review", "targets": [{"kind": "text", "text": "Started the review"}], "is_final": false}))),
        ("ask", tool_args(&epoch, "question-1", 0, json!({"kind": "ask_question", "interaction_id": "question-1",
            "scope": scope, "recipient": {"kind": "user"}, "need": "Decide on merging the fix", "why": "The review is done and waits for it", "blocking": true,
            "options": [{"id": "yes", "label": "Yes"}, {"id": "no", "label": "No"}]}))),
    ]);
    assert!(!tool_text(&replies[0]).0, "{}", tool_text(&replies[0]).1);
    assert!(!tool_text(&replies[1]).0, "{}", tool_text(&replies[1]).1);
    let state = daemon.domain.state().unwrap();
    assert!(matches!(state.interactions[&"question-1".into()].payload, InteractionPayload::Question { .. }));
    assert_ne!(state.tasks[&task.id].state, TaskState::Done);

    // The user answers; the answer is written down the session's open connection at once.
    let (mut helper, pushed) = connect(&home);
    let mut pushed = Some(pushed);
    let patience = std::time::Duration::from_secs(10);
    daemon.execute(Actor::User { user_id: "device:phone".into() }, "answer", Command::AnswerQuestion {
        attachments: vec![], interaction_id: "question-1".into(), option_id: "yes".into(), inputs: BTreeMap::new(),
    });
    let line = next_line(&mut pushed, patience).expect("the answer was written down the connection");
    let id = line["id"].as_str().unwrap().to_owned();
    assert!(line["text"].as_str().unwrap().ends_with("Answer to your card question-1 (Decide on merging the fix): Yes."), "{line}");
    assert_eq!(line["as_user"].as_bool(), Some(true), "{line}");
    assert!(daemon.domain.store.lock().unwrap().delivery_evidence_page(0).unwrap().is_empty(), "written is not handed");

    // The Stop hook holds back a turn end without a result, and never carries the outbox.
    assert_eq!(hook(&home, json!({"session_id": "stranger", "hook_event_name": "Stop"})), json!({}));
    let stop = hook(&home, json!({"session_id": session, "hook_event_name": "Stop"}));
    assert_eq!(stop["decision"], "block", "{stop}");
    assert!(stop["reason"].as_str().unwrap().starts_with("[Alondra] Do not end your turn yet") && !stop["reason"].as_str().unwrap().contains("Yes."), "{stop}");

    node_ack(&home, &[&id]);
    let evidence = daemon.domain.store.lock().unwrap().delivery_evidence_page(0).unwrap();
    assert_eq!(evidence.iter().filter(|item| item.recipient_kind == "agent").count(), 1);
    assert!(evidence.iter().all(|item| item.recipient_kind != "agent" || item.state == "handed"));

    // The daemon restarts: the session keeps acting with its bearer and is not written the answer again.
    helper.kill().unwrap();
    helper.wait().unwrap();
    daemon.stop().await;
    let daemon = Daemon::start(&home).await;
    let (mut helper, pushed) = connect(&home);
    let mut pushed = Some(pushed);
    assert!(next_line(&mut pushed, std::time::Duration::from_millis(500)).is_none(), "an acknowledged message is not written again");
    let replies = mcp(&home, daemon.port, node_bearer, vec![
        ("present_result", tool_args(&epoch, "info-2", 0, json!({"kind": "present_result", "task_id": task.id.0,
            "interaction_id": "info-2", "need": "Acting on the answer", "body": "Acting on the answer", "targets": [{"kind": "text", "text": "Acting on the answer"}], "is_final": false}))),
    ]);
    assert!(!tool_text(&replies[0]).0, "{}", tool_text(&replies[0]).1);
    assert!(daemon.domain.state().unwrap().interactions.contains_key(&"info-2".into()));
    let evidence = daemon.domain.store.lock().unwrap().delivery_evidence_page(0).unwrap();
    assert_eq!(evidence.iter().filter(|item| item.recipient_kind == "agent").count(), 1, "no second delivery");
    helper.kill().unwrap();
    helper.wait().unwrap();

    // The session ends: its bearer stops working.
    let mut recorded = daemon.domain.live.changed.subscribe();
    assert_eq!(hook(&home, json!({"session_id": session, "hook_event_name": "SessionEnd"})), json!({}));
    recorded.changed().await.unwrap();
    daemon.domain.hook_work.run(|| ()).await.unwrap();
    let replies = mcp(&home, daemon.port, ("ALONDRA_DOMAIN_CAPABILITY", &capability), vec![
        ("present_result", tool_args(&epoch, "info-3", 0, json!({"kind": "present_result", "task_id": task.id.0,
            "interaction_id": "info-3", "need": "Too late", "body": "Too late", "targets": [{"kind": "text", "text": "Too late"}], "is_final": false}))),
    ]);
    let (refused, text) = tool_text(&replies[0]);
    assert!(refused && text.contains("forbidden"), "{text}");
    assert!(!daemon.domain.state().unwrap().agents[&AgentId(session.clone())].phase.holds_slot());
    daemon.stop().await;
    std::fs::remove_dir_all(root).unwrap();
}

/// The node mod's connection: the real `alondra node connect`, holding the hook socket open.
fn connect(home: &Path) -> (std::process::Child, std::io::BufReader<std::process::ChildStdout>) {
    let mut child = Process::new(alondra_binary())
        .args(["node", "connect"])
        .env("ALONDRA_HOME", home)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().unwrap();
    let hello = json!({"session_id": SESSION, "version": "2.1.287", "cwd": "/work"});
    child.stdin.take().unwrap().write_all(hello.to_string().as_bytes()).unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    std::io::BufRead::read_line(&mut stdout, &mut line).unwrap();
    assert_eq!(line, "connected\n");
    (child, stdout)
}

async fn eventually(what: &str, condition: impl Fn() -> bool) {
    for _ in 0..500 {
        if condition() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("never happened: {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_mod_reports_through_the_cli_and_its_open_connection_is_its_liveness() {
    let scratch_directory = crate::test_support::Directory::new();
    let root = scratch_directory.parent().unwrap().join(format!("alondra-mods-{}", crate::auth::random_hex(8)));
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let daemon = Daemon::start(&home).await;
    let mods = &daemon.domain.live.mods;

    let (mut held, reader) = connect(&home);
    let mut reader = Some(reader);
    eventually("the connection is registered", || mods.session(SESSION).is_some_and(|s| s.connection.is_some())).await;
    let session = mods.session(SESSION).unwrap();
    assert_eq!(session.version.as_deref(), Some("2.1.287"), "a connection says the mod loaded");
    assert!(!session.connection_lost());

    let patience = std::time::Duration::from_secs(10);
    let acked = daemon.domain.live.pushes.message(SESSION, "Alice", "Use the new API", true).unwrap();
    let line = next_line(&mut reader, patience).expect("a direct message is written down the connection");
    assert!(line["text"].as_str().unwrap().ends_with("] Use the new API"), "{line}");
    assert_eq!(line["as_user"].as_bool(), Some(true));
    node_ack(&home, &[line["id"].as_str().unwrap()]);
    tokio::time::timeout(patience, acked).await.expect("the ack reached the sender").unwrap();
    daemon.domain.live.pushes.interrupt(SESSION).unwrap();
    assert_eq!(next_line(&mut reader, patience), Some(json!({"interrupt": true})));

    let interrupt = format!("http://127.0.0.1:{}/sandbox/interrupt/{SESSION}", daemon.port);
    let client = reqwest::Client::new();
    assert_eq!(client.post(&interrupt).send().await.unwrap().status(), 404, "only a sandbox daemon offers it");
    daemon.domain.live.sandbox.store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(client.post(&interrupt).send().await.unwrap().status(), 200);
    assert_eq!(next_line(&mut reader, patience), Some(json!({"interrupt": true})));

    for report in [
        json!({"event": "turn.start", "turn_id": "t1"}),
        json!({"event": "tool.start", "tool": "Bash", "tool_use_id": "u1", "input": {"command": "cargo test"}}),
        json!({"event": "model", "model": "claude-opus-5-5"}),
    ] {
        let mut report = report;
        report["session_id"] = json!(SESSION);
        node_event(&home, report);
    }
    let session = mods.session(SESSION).unwrap();
    assert_eq!(session.tool.as_ref().and_then(|tool| tool.summary.as_deref()), Some("cargo test"));
    assert_eq!(session.model.as_deref(), Some("claude-opus-5-5"));

    held.kill().unwrap();
    held.wait().unwrap();
    eventually("the closed connection makes the node unreachable", || mods.session(SESSION).unwrap().connection_lost()).await;

    let (mut again, _) = connect(&home);
    eventually("a new connection makes it reachable again", || !mods.session(SESSION).unwrap().connection_lost()).await;
    node_event(&home, json!({"session_id": SESSION, "event": "session.end"}));
    assert!(!mods.session(SESSION).unwrap().connection_lost(), "an ended session is not unreachable");
    again.kill().unwrap();
    again.wait().unwrap();

    let mut bad = Process::new(alondra_binary())
        .args(["node", "event"])
        .env("ALONDRA_HOME", &home)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    bad.stdin.take().unwrap().write_all(br#"{"session_id": "s", "event": "turn.started"}"#).unwrap();
    let refused = bad.wait_with_output().unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("invalid node report"), "{}", String::from_utf8_lossy(&refused.stderr));
    daemon.stop().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_after_a_daemon_restart_reaches_the_reconnected_node_under_an_id_the_mod_has_not_seen() {
    let scratch_directory = crate::test_support::Directory::new();
    let root = scratch_directory.parent().unwrap().join(format!("alondra-restart-{}", crate::auth::random_hex(8)));
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let patience = std::time::Duration::from_secs(10);
    let daemon = Daemon::start(&home).await;
    let (mut helper, reader) = connect(&home);
    let mut reader = Some(reader);
    let first = daemon.domain.live.pushes.message(SESSION, "Alice", "first", true).unwrap();
    let line = next_line(&mut reader, patience).expect("the first message is written");
    let first_id = line["id"].as_str().unwrap().to_owned();
    node_ack(&home, &[&first_id]);
    tokio::time::timeout(patience, first).await.unwrap().unwrap();

    helper.kill().unwrap();
    helper.wait().unwrap();
    daemon.stop().await;
    let daemon = Daemon::start(&home).await;
    let (mut helper, reader) = connect(&home);
    let mut reader = Some(reader);
    let mods = &daemon.domain.live.mods;
    eventually("the helper is connected to the new daemon", || mods.session(SESSION).is_some_and(|s| s.connection.is_some())).await;
    let _again = daemon.domain.live.pushes.message(SESSION, "Alice", "AGAIN", true).unwrap();
    let line = next_line(&mut reader, patience).expect("the message after the restart is written");
    assert!(line["text"].as_str().unwrap().ends_with("] AGAIN"), "{line}");
    assert_ne!(line["id"].as_str().unwrap(), first_id, "the mod would take the reused id for a resend and never submit it");
    helper.kill().unwrap();
    helper.wait().unwrap();
    daemon.stop().await;
    std::fs::remove_dir_all(root).unwrap();
}

/// What Claude runs for the user: the real CLI, inside a Claude Code session.
fn alondra_as_claude(home: &Path, port: u16, lang: &str, args: &[&str]) -> std::process::Output {
    Process::new(alondra_binary())
        .args(args)
        .env("ALONDRA_HOME", home)
        .env("ALONDRA_PORT", port.to_string())
        .env("ALONDRA_LANG", lang)
        .env("CLAUDECODE", "1")
        .env_remove("ALONDRA_ORCHESTRATOR_EXECUTION")
        .env_remove("ALONDRA_SESSION_ID")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .output().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_claude_sends_the_orchestrator_is_maintenance_and_its_answer_is_no_card() {
    let scratch_directory = crate::test_support::Directory::new();
    let root = scratch_directory.parent().unwrap().join(format!("alondra-claude-send-{}", crate::auth::random_hex(8)));
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let daemon = Daemon::start(&home).await;
    const SESSION: &str = "6f1c2b9e-1111-4a2b-9c3d-0123456789ab";
    daemon.domain.bind_orchestrator_session("an-earlier-session").unwrap();
    daemon.domain.bind_orchestrator_session(SESSION).unwrap();
    let cases = [
        ("orchestrator", "es", false, "Sube t-cards a prioridad Alta"),
        ("orchestrator", "en", false, "Lanza dos nodos más"),
        ("orchestrator", "es", true, "¿Cuántos nodos quedan?"),
    ];
    for (name, lang, reply, text) in cases {
        let mut args = vec!["send", name, text];
        if reply { args.push("--reply"); }
        let sent = alondra_as_claude(&home, daemon.port, lang, &args);
        assert!(sent.status.success(), "{name}: {}", String::from_utf8_lossy(&sent.stderr));
        let state = daemon.domain.state().unwrap();
        let request = state.requests.values().find(|r| r.body.ends_with(text))
            .unwrap_or_else(|| panic!("{name}: no request ({})", String::from_utf8_lossy(&sent.stdout)));
        assert!(request.body.contains("From Claude, for "), "{}", request.body);
        assert_eq!(request.maintenance, !reply, "{name} ({lang}, reply {reply}): only a message to the orchestrator that asks no reply is maintenance");
        assert!(request.message, "{name} ({lang}, reply {reply}): it is a message, so the Tasks list leaves it out until it makes tasks");
        if reply { continue; }
        let id = request.id.clone();
        daemon.execute(Actor::Orchestrator { generation: state.orchestrator_generation }, &format!("close-{name}-{lang}"), Command::CloseRequest {
            request_id: id.clone(), answer: Some("Hecho".into()), withdrawn: false, title: None, headline: None, folded_into: None });
        let state = daemon.domain.state().unwrap();
        assert!(state.requests[&id].closed);
        assert!(!state.interactions.values().any(|i| i.scope == Scope::Orchestrator { request_id: Some(id.clone()) }),
            "{name}: the answer to maintenance is no card for the user");
    }
    let requests = daemon.domain.state().unwrap().requests.len();
    for name in ["orquestador", "Orquestador", "Orchestrator", "orquestadr", "an-earlier-session", SESSION, "0123456789ab"] {
        let refused = alondra_as_claude(&home, daemon.port, "es", &["send", name, "Para el nodo de release"]);
        assert!(!refused.status.success(), "{name}: only a task id or orchestrator is sent to");
        assert!(String::from_utf8_lossy(&refused.stderr).contains(&format!("No hay ninguna tarea {name}; para el orquestador usa orchestrator")),
            "{}", String::from_utf8_lossy(&refused.stderr));
    }
    assert_eq!(daemon.domain.state().unwrap().requests.len(), requests, "nothing reached the orchestrator");
    assert!(!alondra_as_claude(&home, daemon.port, "es", &["close", "orchestrator"]).status.success(), "there is no close");
    daemon.stop().await;
    std::fs::remove_dir_all(root).unwrap();
}

/// The phone or Mac app: a paired device that signs each request, as the apps do.
struct App {
    base: String,
    key: p256::ecdsa::SigningKey,
    device: String,
    http: reqwest::Client,
}

impl App {
    fn pair(daemon: &Daemon) -> Self {
        use base64::Engine;
        let key = p256::ecdsa::SigningKey::from_slice(&[9u8; 32]).unwrap();
        let code = daemon.auth.new_pairing_code();
        let device = daemon.auth.pair(&code, "Test iPhone",
            &base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_sec1_bytes())).unwrap();
        Self { base: format!("http://127.0.0.1:{}", daemon.port), key, device: device.id, http: reqwest::Client::new() }
    }

    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        use base64::Engine;
        use p256::ecdsa::signature::Signer;
        static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        let bytes = body.as_ref().map(|body| serde_json::to_vec(body).unwrap()).unwrap_or_default();
        let timestamp = alondra_core::now_millis() + NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let signature: p256::ecdsa::Signature = self.key.sign(crate::auth::signed_message(method, path, &timestamp.to_string(), &bytes).as_bytes());
        let request = if method == "POST" { self.http.post(format!("{}{path}", self.base)) } else { self.http.get(format!("{}{path}", self.base)) }
            .header("x-alondra-device", &self.device)
            .header("x-alondra-timestamp", timestamp.to_string())
            .header("x-alondra-signature", base64::engine::general_purpose::STANDARD.encode(signature.to_der().as_bytes()));
        let request = if body.is_some() { request.header("content-type", "application/json").body(bytes) } else { request };
        request.send().await.unwrap().json().await.unwrap()
    }

    /// The card's "Request changes" action, as the app reads it from the card's view.
    async fn changes_action(&self, card: &str) -> (String, String) {
        let view = self.send("GET", &format!("/views/card%3A{card}?protocol={}&client=ios&lang=es",
            alondra_core::views::VIEWS_PROTOCOL), None).await;
        let actions = view.to_string();
        let id = view["sections"].as_array().into_iter().flatten()
            .flat_map(|section| section["items"].as_array().into_iter().flatten())
            .flat_map(|item| item["actions"].as_array().into_iter().flatten().chain(item.get("reply").filter(|reply| reply.is_object())))
            .filter_map(|action| action["id"].as_str())
            .find(|id| id.starts_with("changes:"))
            .unwrap_or_else(|| panic!("no request-changes action on card {card}: {actions}"))
            .to_owned();
        (id, view["data_epoch"].as_str().unwrap().to_owned())
    }

    /// Sends block comments with "Request changes", as the iOS and Mac apps do.
    async fn request_changes(&self, card: &str, comments: Value) -> Value {
        let action = self.changes_action(card).await;
        self.send_changes(action, comments).await
    }

    /// Sends the comments with an action read earlier: the card may have changed since.
    async fn send_changes(&self, (action_id, data_epoch): (String, String), comments: Value) -> Value {
        self.send("POST", "/actions", Some(json!({
            "protocol": alondra_core::views::VIEWS_PROTOCOL, "command_id": uuid_v4(), "data_epoch": data_epoch,
            "client": "ios", "lang": "es", "action_id": action_id, "input": {"comments": comments},
        }))).await
    }
}

fn uuid_v4() -> String {
    let hex = crate::auth::random_hex(16);
    format!("{}-{}-4{}-8{}-{}", &hex[0..8], &hex[8..12], &hex[13..16], &hex[17..20], &hex[20..32])
}

fn document(home: &Path, name: &str, text: &str) -> (String, String) {
    use sha2::Digest;
    std::fs::create_dir_all(home.join("files")).unwrap();
    std::fs::write(home.join("files").join(name), text).unwrap();
    let digest = sha2::Sha256::digest(text.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
    (digest, format!("files/{name}"))
}

/// A node with a task and its bearer, without a model.
async fn node_with_a_task(daemon: &Daemon, home: &Path) -> (String, TaskId, String) {
    let (capability, task) = bg_node_with_a_task(daemon, home, "Write the design");
    (capability, task, daemon.domain.state().unwrap().data_epoch.0.clone())
}

fn node_calls(home: &Path, daemon: &Daemon, capability: &str, calls: Vec<(&str, Value)>) {
    for (index, reply) in mcp(home, daemon.port, ("ALONDRA_DOMAIN_CAPABILITY", capability), calls).iter().enumerate() {
        let (failed, text) = tool_text(reply);
        assert!(!failed, "call {index}: {text}");
    }
}

fn pushed_text(home: &Path, pushed: &mut Option<Pushed>) -> String {
    let line = next_line(pushed, std::time::Duration::from_secs(10)).expect("the node was told");
    node_ack(home, &[line["id"].as_str().unwrap()]);
    line["text"].as_str().unwrap().to_owned()
}

fn applied_done(reply: &Value) {
    assert_eq!(reply["result"]["kind"], "done", "{reply}");
}

const COMMENTS: &str = r#"[
    {"anchor": "paragraph:1:0", "text": "Split this step in two", "quote": "Migrate the store in one go"},
    {"anchor": "paragraph:2:0", "text": "Keep the old endpoint a release longer", "quote": "Remove the old endpoint"}
]"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn comments_sent_on_a_document_review_reach_the_node_even_when_it_moves_on_to_a_newer_revision() {
    let scratch_directory = crate::test_support::Directory::new();
    let root = scratch_directory.parent().unwrap().join(format!("alondra-review-{}", crate::auth::random_hex(8)));
    let home = root.join("home");
    std::fs::create_dir_all(root.join("projects")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(alondra_core::options::path_in(&home),
        format!("projects_root = {:?}\n", root.join("projects").to_str().unwrap())).unwrap();
    let daemon = Daemon::start(&home).await;
    let (capability, task, epoch) = node_with_a_task(&daemon, &home).await;
    let app = App::pair(&daemon);
    let comments: Value = serde_json::from_str(COMMENTS).unwrap();
    let (mut helper, pushed) = connect(&home);
    let mut pushed = Some(pushed);
    let scope = json!({"kind": "task", "task_id": task.0});
    let ask = |id: &str, revision: &str, replaces: Option<&str>| {
        let mut question = json!({"kind": "ask_question", "interaction_id": id, "scope": scope, "recipient": {"kind": "user"},
            "need": "Review the migration design", "why": "The migration waits for it", "blocking": true,
            "options": [{"id": "approve", "label": "Approve"}, {"id": "drop", "label": "Drop it"}]});
        if let Some(replaces) = replaces { question["replaces"] = json!(replaces); }
        vec![
            ("ask", tool_args(&epoch, id, 0, question)),
            ("link_interaction_subject", tool_args(&epoch, &format!("link-{id}"), 0, json!({"kind": "link_interaction_subject",
                "interaction_id": id, "subject": {"kind": "document_review", "document_id": "design", "revision_id": revision}}))),
        ]
    };

    // The node writes a document and asks for its review; the app sends two block comments.
    let (digest, reference) = document(&home, "design-1.md", "# Design\n\nMigrate the store in one go\n\nRemove the old endpoint\n");
    let mut calls = vec![("create_document", tool_args(&epoch, "doc-1", 0, json!({"kind": "create_document", "task_id": task.0,
        "document_id": "design", "title": "Migration design", "revision_id": "design-r1", "content_digest": digest,
        "content_reference": reference, "role": "design"})))];
    calls.extend(ask("review-1", "design-r1", None));
    node_calls(&home, &daemon, &capability, calls);
    let reply = app.request_changes("review-1", comments.clone()).await;
    applied_done(&reply);
    let told = pushed_text(&home, &mut pushed);
    assert!(told.contains("Split this step in two") && told.contains("Migrate the store in one go")
        && told.contains("Keep the old endpoint a release longer"), "{told}");

    // The node publishes a second revision while the user still comments the first one.
    let (digest, reference) = document(&home, "design-2.md", "# Design\n\nMigrate the store in two steps\n");
    let mut calls = vec![("revise_document", tool_args(&epoch, "doc-2", 0, json!({"kind": "revise_document", "task_id": task.0,
        "document_id": "design", "revision_id": "design-r2", "content_digest": digest, "content_reference": reference})))];
    calls.extend(ask("review-2", "design-r1", None));
    node_calls(&home, &daemon, &capability, calls);
    let reply = app.request_changes("review-2", comments.clone()).await;
    applied_done(&reply);
    let told = pushed_text(&home, &mut pushed);
    assert!(told.contains("Split this step in two") && told.contains("Keep the old endpoint a release longer"), "{told}");

    // The node asks again about the newer revision, replacing the card the user had open.
    calls = ask("review-3", "design-r1", None);
    node_calls(&home, &daemon, &capability, calls);
    let open = app.changes_action("review-3").await;
    node_calls(&home, &daemon, &capability, ask("review-4", "design-r2", Some("review-3")));
    let reply = app.send_changes(open.clone(), comments.clone()).await;
    assert_eq!(reply["result"]["kind"], "changed", "{reply}");
    assert_eq!(reply["result"]["title"], "«Migration design» tiene una versión nueva", "{reply}");
    assert_eq!(reply["result"]["body"], "Mientras comentabas, el nodo pidió revisar la versión 2; tú leías la 1. Tus comentarios siguen sin enviar.");
    let resend = reply["result"]["next"]["id"].as_str().expect("it offers to send them to the new version").to_owned();
    assert!(resend.starts_with("changes:review-4@"), "{resend}");
    applied_done(&app.send_changes((resend, open.1), comments.clone()).await);
    let told = pushed_text(&home, &mut pushed);
    assert!(told.contains("Split this step in two") && told.contains("Keep the old endpoint a release longer"), "{told}");

    // A result that delivers the document: the node presents revision 2 again while the user comments.
    node_calls(&home, &daemon, &capability, vec![("present_result", tool_args(&epoch, "result-1", 0, json!({"kind": "present_result",
        "task_id": task.0, "interaction_id": "result-1", "need": "Review the migration design", "body": "The design",
        "targets": [{"kind": "document_revision", "document_id": "design", "revision_id": "design-r2"}], "is_final": true})))]);
    let open_result = app.changes_action("result-1").await;
    let (digest, reference) = document(&home, "design-3.md", "# Design\n\nMigrate the store in three steps\n");
    node_calls(&home, &daemon, &capability, vec![
        ("revise_document", tool_args(&epoch, "doc-3", 0, json!({"kind": "revise_document", "task_id": task.0,
            "document_id": "design", "revision_id": "design-r3", "content_digest": digest, "content_reference": reference}))),
        ("present_result", tool_args(&epoch, "result-2", 0, json!({"kind": "present_result",
        "task_id": task.0, "interaction_id": "result-2", "need": "Review the migration design", "body": "The design",
        "targets": [{"kind": "document_revision", "document_id": "design", "revision_id": "design-r3"}], "is_final": true})))]);
    applied_done(&app.send_changes(open_result, comments.clone()).await);
    let told = pushed_text(&home, &mut pushed);
    assert!(told.contains("These comments are on version 2 of “Migration design”, which the user was reading; you have since presented version 3.")
        && told.contains("Split this step in two") && told.contains("Keep the old endpoint a release longer"), "{told}");
    helper.kill().unwrap();
    helper.wait().unwrap();
    daemon.stop().await;
    std::fs::remove_dir_all(root).unwrap();
}

impl App {
    async fn view(&self, key: &str) -> Value {
        self.send("GET", &format!("/views/{}?protocol={}&client=ios&lang=es", key.replace(':', "%3A"),
            alondra_core::views::VIEWS_PROTOCOL), None).await
    }

    /// The cards waiting for the user, in the order the phone shows them.
    async fn queue(&self) -> Vec<String> {
        let home = self.view("home").await;
        home["sections"].as_array().into_iter().flatten().find(|section| section["id"] == "queue")
            .and_then(|section| section["card_ids"].as_array()).into_iter().flatten()
            .map(|id| id.as_str().unwrap().to_owned()).collect()
    }

    /// Every action a card's own screen offers: its buttons, the ones under "More" and its reply.
    async fn card_actions(&self, card: &str) -> Vec<Value> {
        let view = self.view(&format!("card:{card}")).await;
        assert_eq!(view["key"], format!("card:{card}"), "the card's screen loads under its own key: {view}");
        view["sections"].as_array().into_iter().flatten()
            .flat_map(|section| section["items"].as_array().into_iter().flatten())
            .flat_map(|item| item["actions"].as_array().into_iter().flatten().chain(item["more"].as_array().into_iter().flatten())
                .chain(item.get("reply").filter(|reply| reply.is_object())))
            .cloned().collect()
    }

    async fn action(&self, card: &str, prefix: &str) -> Value {
        let actions = self.card_actions(card).await;
        actions.iter().find(|action| action["id"].as_str().is_some_and(|id| id.starts_with(prefix))).cloned()
            .unwrap_or_else(|| panic!("card {card} offers no {prefix} action: {actions:?}"))
    }

    async fn perform(&self, action: &Value, option: Option<&str>, text: Option<&str>) -> Value {
        let epoch = self.view("home").await["data_epoch"].as_str().unwrap().to_owned();
        self.send("POST", "/actions", Some(json!({
            "protocol": alondra_core::views::VIEWS_PROTOCOL, "command_id": uuid_v4(), "data_epoch": epoch,
            "client": "ios", "lang": "es", "action_id": action["id"], "option_id": option, "input": {"text": text},
        }))).await
    }

    /// Acts on a card from its own screen and checks what the server promised with
    /// `resolves_card`: a card it said would leave is gone from the queue and from the next
    /// changes the phone asks for, and stays gone; one it said would stay is still there.
    async fn settle(&self, card: &str, prefix: &str, option: Option<&str>, text: Option<&str>, leaves: bool) {
        let action = self.action(card, prefix).await;
        assert_eq!(action["resolves_card"], json!(leaves), "{card}: {prefix} says whether it takes its card out: {action}");
        let before = self.view("home").await;
        assert!(self.queue().await.contains(&card.to_owned()), "{card} waits before the action");
        let reply = self.perform(&action, option, text).await;
        assert_eq!(reply["outcome"], "applied", "{card}: {reply}");
        let changes = self.send("GET", &format!("/changes?protocol={}&client=ios&lang=es&wait=0&cursor={}&views=home%40{}",
            alondra_core::views::VIEWS_PROTOCOL, before["cursor"].as_str().unwrap(), before["version"].as_str().unwrap()), None).await;
        if leaves {
            assert!(changes["stale"].as_array().unwrap().contains(&json!("home")), "the phone's next poll says home changed: {changes}");
        }
        for _ in 0..3 {
            assert_eq!(!self.queue().await.contains(&card.to_owned()), leaves,
                "{card} after {prefix}: the queue {} it", if leaves { "no longer lists" } else { "still lists" });
        }
    }
}

/// Every kind of card the phone answers leaves its queue when the server said it would, and only
/// then: an option, a text, a reply on a question, a question of a group, a permission and a
/// delivery. Each card of a group opens on its own screen.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_card_kind_leaves_the_queue_when_its_action_says_so_and_never_returns() {
    let scratch_directory = crate::test_support::Directory::new();
    let root = scratch_directory.parent().unwrap().join(format!("alondra-cards-{}", crate::auth::random_hex(8)));
    let home = root.join("home");
    std::fs::create_dir_all(root.join("projects")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(alondra_core::options::path_in(&home),
        format!("projects_root = {:?}\n", root.join("projects").to_str().unwrap())).unwrap();
    let daemon = Daemon::start(&home).await;
    let (capability, task, epoch) = node_with_a_task(&daemon, &home).await;
    let app = App::pair(&daemon);
    let scope = json!({"kind": "task", "task_id": task.0});
    let choice = |id: &str| json!({"id": id, "label": id, "description": format!("Go with {id}")});
    let ask = |id: &str, need: &str| ("ask", tool_args(&epoch, id, 0, json!({"interaction_id": id, "scope": scope,
        "recipient": {"kind": "user"}, "need": need, "why": "The release waits for it.", "blocking": false,
        "options": [choice("stable"), choice("beta")]})));
    node_calls(&home, &daemon, &capability, vec![
        ask("option", "Pick the release channel"),
        ask("reply", "Name the new settings screen"),
        ("request_input", json!({"data_epoch": epoch, "command_id": "text", "expected_revision": 0, "interaction_id": "text",
            "scope": scope, "recipient": {"kind": "user"}, "title": "Release note", "need": "Write the release note", "why": "The release notes need it.",
            "kind": "text", "blocking": false})),
        ("ask", tool_args(&epoch, "group", 0, json!({"interaction_id": "group", "scope": scope, "recipient": {"kind": "user"},
            "why": "", "blocking": false, "questions": (1..=4).map(|n| json!({"title": format!("Launch decision {n}"), "question": format!("Launch question {n}?"),
                "why": "The launch plan needs it.", "options": [choice("a"), choice("b")]})).collect::<Vec<_>>()}))),
    ]);
    let queue = app.queue().await;
    let group: Vec<String> = queue.iter().filter(|id| id.starts_with("group")).cloned().collect();
    assert_eq!(group.len(), 4, "four questions asked together are four cards: {queue:?}");
    for card in &group {
        assert!(!app.card_actions(card).await.is_empty(), "{card} opens on its own screen with its answers");
    }

    app.settle("option", "ans:", Some("stable"), None, true).await;
    app.settle("text", "ans:", None, Some("Faster sync and a new settings screen"), true).await;
    app.settle("reply", "reply:", None, Some("Call it Preferences"), true).await;
    app.settle(&group[2], "reply:", None, Some("Alice signs it"), true).await;
    for (card, option) in [(&group[0], "a"), (&group[1], "b"), (&group[3], "a")] {
        app.settle(card, "ans:", Some(option), None, true).await;
    }

    let waiting = app.queue().await;
    let hook_home = home.clone();
    let permission = std::thread::spawn(move || hook(&hook_home, json!({"session_id": SESSION, "hook_event_name": "PermissionRequest",
        "permission_mode": "default", "tool_name": "Bash", "tool_input": {"command": "git push"}, "cwd": "/work"})));
    let mut asked = None;
    for _ in 0..100 {
        asked = app.queue().await.into_iter().find(|id| !waiting.contains(id));
        if asked.is_some() { break; }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let asked = asked.expect("the permission the node waits on is a card");
    app.settle(&asked, "ans:", Some(Hold::ALLOW), None, true).await;
    let decision = permission.join().unwrap();
    assert_eq!(decision["hookSpecificOutput"]["decision"]["behavior"], "allow", "{decision}");

    node_calls(&home, &daemon, &capability, vec![("present_result", tool_args(&epoch, "delivery", 0, json!({
        "task_id": task.0, "interaction_id": "delivery", "need": "Review the migration report", "body": "The store moved in one go.",
        "targets": [{"kind": "text", "text": "The store moved in one go."}], "is_final": true})))]);
    app.settle("delivery", "accept:", None, None, true).await;
    assert!(app.queue().await.is_empty(), "nothing is left waiting");
    daemon.stop().await;
    std::fs::remove_dir_all(root).unwrap();
}
