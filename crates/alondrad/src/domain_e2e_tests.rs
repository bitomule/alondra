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

struct CapturingLauncher(Mutex<Vec<Vec<String>>>);
impl crate::orchestrate::Launcher for CapturingLauncher {
    async fn run(&self, _: &str, args: &[String]) -> Result<String, crate::orchestrate::Refusal> {
        self.0.lock().unwrap().push(args.to_vec());
        Ok("backgrounded · ab1234 · scheduled/run".into())
    }
    async fn stop(&self, _: &str) {}
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
    node_event(&home, json!({"session_id": SESSION, "event": "session.start", "version": "2.1.287", "cwd": "/work", "launch_key": "run-1"}));
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
            "interaction_id": "info-1", "scope": scope, "recipient": {"kind": "user"}, "need": "The review started", "body": "Started the review"}))),
        ("ask", tool_args(&epoch, "question-1", 0, json!({"kind": "ask_question", "interaction_id": "question-1",
            "scope": scope, "recipient": {"kind": "user"}, "need": "Decide on merging the fix", "why": "The review is done and waits for it", "blocking": true,
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

    // The user answers; the answer is written down the session's open connection at once.
    let (mut helper, pushed) = connect(&home);
    let mut pushed = Some(pushed);
    let patience = std::time::Duration::from_secs(10);
    daemon.execute(Actor::User { user_id: "device:phone".into() }, "answer", Command::AnswerQuestion {
        attachments: vec![], interaction_id: "question-1".into(), option_id: "yes".into(), inputs: BTreeMap::new(),
    });
    let line = next_line(&mut pushed, patience).expect("the answer was written down the connection");
    let id = line["id"].as_str().unwrap().to_owned();
    assert_eq!((line["text"].as_str(), line["as_user"].as_bool()), (Some("Yes."), Some(false)), "{line}");
    assert!(daemon.domain.store.lock().unwrap().delivery_evidence_page(0).unwrap().is_empty(), "written is not handed");

    // The Stop hook keeps its nudge but no longer carries the outbox.
    assert_eq!(hook(&home, json!({"session_id": "stranger", "hook_event_name": "Stop"})), json!({}));
    let stop = hook(&home, json!({"session_id": session, "hook_event_name": "Stop"}));
    assert!(stop["hookSpecificOutput"]["additionalContext"].as_str().unwrap().starts_with("[Alondra] Your turn ended"), "{stop}");

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
    let replies = mcp(&home, daemon.port, from_settings, vec![
        ("publish_information", tool_args(&epoch, "info-2", 0, json!({"kind": "publish_information",
            "interaction_id": "info-2", "scope": scope, "recipient": {"kind": "user"}, "need": "Acting on the answer", "body": "Acting on the answer"}))),
    ]);
    assert!(!tool_text(&replies[0]).0, "{}", tool_text(&replies[0]).1);
    assert!(daemon.domain.state().unwrap().interactions.contains_key(&"info-2".into()));
    let evidence = daemon.domain.store.lock().unwrap().delivery_evidence_page(0).unwrap();
    assert_eq!(evidence.iter().filter(|item| item.recipient_kind == "agent").count(), 1, "no second delivery");
    helper.kill().unwrap();
    helper.wait().unwrap();

    // The session ends: its bearer stops working.
    assert_eq!(hook(&home, json!({"session_id": session, "hook_event_name": "SessionEnd"})), json!({}));
    let replies = mcp(&home, daemon.port, ("ALONDRA_DOMAIN_CAPABILITY", &capability), vec![
        ("publish_information", tool_args(&epoch, "info-3", 0, json!({"kind": "publish_information",
            "interaction_id": "info-3", "scope": scope, "recipient": {"kind": "user"}, "need": "Too late", "body": "Too late"}))),
    ]);
    let (refused, text) = tool_text(&replies[0]);
    assert!(refused && text.contains("forbidden"), "{text}");
    assert!(!daemon.domain.state().unwrap().agents[&AgentId(session.clone())].connected);
    assert!(!Path::new(&settings).exists(), "the session's settings file goes with it");
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
    let root = std::env::temp_dir().join(format!("alondra-v2-mods-{}", crate::auth::random_hex(8)));
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
    let acked = daemon.domain.live.pushes.message(SESSION, "Use the new API", true).unwrap();
    let line = next_line(&mut reader, patience).expect("a direct message is written down the connection");
    assert_eq!((line["text"].as_str(), line["as_user"].as_bool()), (Some("Use the new API"), Some(true)));
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
        json!({"event": "session.measure", "context": {"tokens": 54944, "window": 1000000, "percent": 5},
            "rate_limits": [{"kind": "seven_day", "percent_used": 78, "resets_at": "2026-10-05T00:00:00Z"}], "cost_usd": 1.25}),
    ] {
        let mut report = report;
        report["session_id"] = json!(SESSION);
        node_event(&home, report);
    }
    let session = mods.session(SESSION).unwrap();
    assert_eq!(session.tool.as_ref().and_then(|tool| tool.summary.as_deref()), Some("cargo test"));
    assert_eq!(session.context.as_ref().and_then(|context| context.percent), Some(5.0));
    assert_eq!(session.cost_usd, Some(1.25));
    assert_eq!(mods.usage(alondra_core::now_millis()).map(|usage| usage.week_percent), Some(78.0));

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
    let root = std::env::temp_dir().join(format!("alondra-v2-restart-{}", crate::auth::random_hex(8)));
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let patience = std::time::Duration::from_secs(10);
    let daemon = Daemon::start(&home).await;
    let (mut helper, reader) = connect(&home);
    let mut reader = Some(reader);
    let first = daemon.domain.live.pushes.message(SESSION, "first", true).unwrap();
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
    let _again = daemon.domain.live.pushes.message(SESSION, "AGAIN", true).unwrap();
    let line = next_line(&mut reader, patience).expect("the message after the restart is written");
    assert_eq!(line["text"], "AGAIN");
    assert_ne!(line["id"].as_str().unwrap(), first_id, "the mod would take the reused id for a resend and never submit it");
    helper.kill().unwrap();
    helper.wait().unwrap();
    daemon.stop().await;
    std::fs::remove_dir_all(root).unwrap();
}
