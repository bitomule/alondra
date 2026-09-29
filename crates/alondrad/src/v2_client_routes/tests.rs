//! Each route is exercised the way the TUI calls it: a signed request with the body of
//! `fixtures/client-v2`, over the real router, against a real domain database.
use super::*;
use crate::auth::Auth;
use crate::domain_bootstrap::V2Only;
use alondra_core::domain_v2::{
    Actor, AttachmentRef, Command, CommandEnvelope, CommandOutcome, PROTOCOL_VERSION, Recipient, Scope,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use std::collections::BTreeSet;
use std::net::SocketAddr;

pub(super) struct Fixture {
    pub home: PathBuf,
    pub base: String,
    http: reqwest::Client,
    key: SigningKey,
    device_id: String,
    pub domain: Arc<DomainRuntime>,
    pub client: Arc<Client>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

pub(super) fn fixture_json(name: &str) -> Value {
    let path = match std::env::var("RUNFILES_DIR").or_else(|_| std::env::var("TEST_SRCDIR")) {
        Ok(root) => PathBuf::from(root).join(std::env::var("TEST_WORKSPACE").unwrap_or_else(|_| "_main".into()))
            .join("fixtures/client-v2").join(name),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/client-v2").join(name),
    };
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap()
}

impl Fixture {
    pub(super) async fn start() -> Self {
        let home = std::env::temp_dir().join(format!("alondra-v2-client-{}", crate::auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        let epoch = DomainRuntime::open(&home).unwrap().state().unwrap().data_epoch.0;
        let auth = Auth::open(&home).unwrap();
        let key = SigningKey::from_slice(&[31u8; 32]).unwrap();
        let device = auth.pair(&auth.new_pairing_code(), "terminal", &STANDARD.encode(key.verifying_key().to_sec1_bytes())).unwrap();
        let state = V2Only::open(&home, &epoch).unwrap();
        let domain = state.domain();
        let client = state.client();
        let app = crate::domain_bootstrap::router(state.clone())
            .layer(axum::middleware::from_fn_with_state(state.auth(), crate::auth::guard));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
        });
        Self { home, base, http: reqwest::Client::new(), key, device_id: device.id, domain, client, server }
    }

    pub(super) fn command(&self, actor: Actor, id: &str, command: Command) -> CommandOutcome {
        let state = self.domain.state().unwrap();
        self.domain.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch,
            command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap()
    }

    pub(super) fn user(&self, id: &str, command: Command) -> CommandOutcome {
        self.command(Actor::User { user_id: "u".into() }, id, command)
    }

    pub(super) fn agent(&self, agent: &str, id: &str, command: Command) -> CommandOutcome {
        self.command(Actor::Agent { agent_id: agent.into() }, id, command)
    }

    fn signed(&self, method: &str, path: &str, body: Option<&Value>) -> reqwest::RequestBuilder {
        static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        let bytes = body.map(|body| serde_json::to_vec(body).unwrap()).unwrap_or_default();
        let timestamp = alondra_core::now_millis() + NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let message = crate::auth::signed_message(method, path, &timestamp.to_string(), &bytes);
        let signature: Signature = self.key.sign(message.as_bytes());
        let url = format!("{}{path}", self.base);
        let request = match method {
            "POST" => self.http.post(url),
            "PUT" => self.http.put(url),
            _ => self.http.get(url),
        }
        .header("x-alondra-device", &self.device_id)
        .header("x-alondra-timestamp", timestamp.to_string())
        .header("x-alondra-signature", STANDARD.encode(signature.to_der().as_bytes()));
        if body.is_some() { request.header("content-type", "application/json").body(bytes) } else { request }
    }

    pub(super) async fn get(&self, path: &str) -> (StatusCode, Vec<u8>) {
        let response = self.signed("GET", path, None).send().await.unwrap();
        (response.status(), response.bytes().await.unwrap().to_vec())
    }

    pub(super) async fn get_json(&self, path: &str) -> Value {
        let (status, body) = self.get(path).await;
        assert_eq!(status, StatusCode::OK, "{path}: {}", String::from_utf8_lossy(&body));
        serde_json::from_slice(&body).unwrap()
    }

    pub(super) async fn send(&self, method: &str, path: &str, body: &Value) -> (StatusCode, Value) {
        let response = self.signed(method, path, Some(body)).send().await.unwrap();
        let status = response.status();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    pub(super) async fn unsigned_get(&self, path: &str) -> StatusCode {
        self.http.get(format!("{}{path}", self.base)).send().await.unwrap().status()
    }
}

/// Every key the fixture names is in `actual` with the same kind of value; a null in the
/// fixture accepts anything. Arrays compare their first entry.
pub(super) fn assert_covers(expected: &Value, actual: &Value, at: &str) {
    match (expected, actual) {
        (Value::Null, _) => {}
        (Value::Object(want), Value::Object(got)) => {
            for (key, value) in want {
                let found = got.get(key).unwrap_or_else(|| panic!("{at}.{key} is missing from {actual}"));
                assert_covers(value, found, &format!("{at}.{key}"));
            }
        }
        (Value::Array(want), Value::Array(got)) => {
            if let (Some(want), Some(got)) = (want.first(), got.first()) {
                assert_covers(want, got, &format!("{at}[0]"));
            }
        }
        (Value::String(_), Value::String(_)) | (Value::Bool(_), Value::Bool(_)) => {}
        (Value::Number(_), Value::Number(_)) => {}
        _ => panic!("{at}: expected a value like {expected}, got {actual}"),
    }
}

struct PrGithub(Value);

impl PrView for PrGithub {
    fn view<'a>(&'a self, _: &'a str) -> pr_details::Viewed<'a> {
        Box::pin(async move { Some(self.0.clone()) })
    }
}

fn seed_work(fixture: &Fixture) {
    for (agent, name) in [("agent-a", "Instance A"), ("agent-b", "Instance B")] {
        fixture.command(Actor::System, &format!("register-{agent}"), Command::RegisterAgent { agent_id: agent.into(), name: name.into() });
    }
    for request in ["request-1", "request-2"] {
        fixture.user(request, Command::CreateRequest { request_id: request.into(), body: "Work".into() });
    }
    fixture.user("task-1", Command::CreateTask {
        task_id: "task-1".into(), request_id: "request-1".into(), title: "Readable editor colours".into(),
        agent_id: Some("agent-a".into()), review_prs: true, dependencies: BTreeSet::new(), stack: None,
    });
    fixture.command(Actor::Orchestrator { generation: 0 }, "answer-2", Command::PublishInformation {
        attachments: vec![], interaction_id: "info-2".into(),
        scope: Scope::Orchestrator { request_id: Some("request-2".into()) }, recipient: Recipient::User,
        body: "Nothing to change: it already works".into(),
    });
    fixture.user("close-2", Command::CloseRequest { request_id: "request-2".into(), answer: None, withdrawn: false, title: None });
    fixture.user("request-3", Command::CreateRequest { request_id: "request-3".into(), body: "Never mind".into() });
    fixture.user("close-3", Command::CloseRequest { request_id: "request-3".into(), answer: None, withdrawn: true, title: Some("Dropped idea".into()) });
    fixture.agent("agent-a", "pr-1", Command::RegisterPr {
        task_id: "task-1".into(), pr_id: "pr-1".into(), host: "github.com".into(), repository: "me/app".into(),
        number: 7, head: "0123456789abcdef0123456789abcdef01234567".into(), base: "main".into(),
        allowed_methods: vec![alondra_core::domain_v2::MergeMethod::Squash],
    });
}

fn hook(session: &str, event: &str, extra: Value) -> Value {
    let mut payload = json!({"session_id": session, "hook_event_name": event});
    payload.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    payload
}

fn observe_sessions(fixture: &Fixture) {
    let live = &fixture.domain.live;
    live.observe(&hook("agent-a", "SessionStart", json!({"cwd": "/Users/me/Projects/App"})));
    live.observe(&hook("agent-a", "UserPromptSubmit", json!({"prompt": "Fix the colours"})));
    live.observe(&hook("agent-a", "PreToolUse", json!({"tool_name": "Bash", "tool_use_id": "t1",
        "tool_input": {"command": "bazel test //crates/..."}})));
    live.observe(&hook("agent-b", "SessionStart", json!({"cwd": "/Users/me/Projects/Web"})));
    live.observe(&hook("agent-b", "Stop", json!({"last_assistant_message": "Done with the parser"})));
}

#[tokio::test]
async fn the_runtime_carries_live_sessions_times_and_pull_requests_in_the_fixture_shape() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    observe_sessions(&fixture);
    let github = PrGithub(json!({"title": "Editor covers readable text", "state": "OPEN", "mergeable": "CONFLICTING",
        "mergeStateStatus": "DIRTY", "statusCheckRollup": [
            {"name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"},
            {"name": "ui tests", "status": "COMPLETED", "conclusion": "FAILURE"}]}));
    fixture.client.details.refresh(&fixture.domain.state().unwrap(), &github).await;
    *fixture.client.gauges.lock().unwrap() = Some((serde_json::from_value(fixture_json("runtime.json")["machine"].clone()).unwrap(),
        serde_json::from_value(fixture_json("runtime.json")["usage"].clone()).ok()));

    assert_eq!(fixture.unsigned_get("/v2/runtime").await, StatusCode::FORBIDDEN, "runtime is for a paired device");
    let runtime = fixture.get_json("/v2/runtime").await;
    let expected = fixture_json("runtime.json");
    for key in ["machine", "usage"] {
        assert_covers(&expected[key], &runtime[key], key);
    }
    assert_covers(&json!({"current": "", "available": false}), &runtime["update"], "update");
    let nodes = runtime["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 2, "only the sessions the domain knows appear");
    let a = nodes.iter().find(|node| node["session_id"] == "agent-a").unwrap();
    let b = nodes.iter().find(|node| node["session_id"] == "agent-b").unwrap();
    assert_covers(&expected["nodes"][1], b, "nodes[agent-b]");
    let mut without_readings = expected["nodes"][0].clone();
    for key in ["last_message", "daemon_needs", "background"] {
        without_readings.as_object_mut().unwrap().remove(key);
    }
    assert_covers(&without_readings, a, "nodes[agent-a]");
    for node in nodes {
        let _: alondra_core::Node = serde_json::from_value(node.clone()).expect("a client reads the legacy Node");
        assert!(node.get("alive").is_none() && node.get("ledger").is_none(), "daemon-internal fields stay out");
    }
    assert_eq!((a["name"].as_str(), a["project"].as_str(), a["busy"].as_bool()), (Some("Instance A"), Some("App"), Some(true)));
    assert_eq!(b["project"], "Web");
    assert_eq!(b["last_message"], "Done with the parser");

    for key in expected["tasks"]["task-1"].as_object().unwrap().keys() {
        assert!(runtime["tasks"]["task-1"].get(key).is_some(), "tasks.task-1.{key} is always present, null when unknown");
    }
    assert_eq!(runtime["tasks"]["task-1"]["project"], "App");
    assert_eq!(runtime["orchestrator"]["phase"], "idle", "the orchestrator runner's status travels with the runtime");
    for key in expected["requests"]["request-2"].as_object().unwrap().keys() {
        assert!(runtime["requests"]["request-2"].get(key).is_some(), "requests.request-2.{key} is always present, null when unknown");
    }
    assert_eq!(runtime["requests"]["request-2"]["answer"], "Nothing to change: it already works");
    assert_eq!(runtime["requests"]["request-2"]["withdrawn"], false);
    assert_eq!((runtime["requests"]["request-3"]["withdrawn"].as_bool(), runtime["requests"]["request-3"]["title"].as_str()), (Some(true), Some("Dropped idea")));
    assert_eq!(runtime["requests"]["request-1"]["answer"], Value::Null);
    assert_covers(&expected["pull_requests"]["pr-1"], &runtime["pull_requests"]["pr-1"], "pull_requests.pr-1");
    assert_eq!(runtime["pull_requests"]["pr-1"]["title"], "Editor covers readable text");
    assert_eq!(runtime["pull_requests"]["pr-1"]["conflicts"], true);
    assert_eq!(runtime["pull_requests"]["pr-1"]["checks"].as_array().unwrap().len(), 2);
    let _: alondra_core::pr::Check = serde_json::from_value(runtime["pull_requests"]["pr-1"]["checks"][0].clone()).unwrap();
    assert!(runtime["tasks"]["task-1"]["state_since"].as_i64().unwrap() > 0 && runtime["requests"]["request-1"]["created_at"].as_i64().unwrap() > 0,
        "each time is the commit's own");
}

#[tokio::test]
async fn times_are_the_commits_own_and_survive_a_restart() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    let before = fixture.get_json("/v2/runtime").await;
    let created = before["requests"]["request-1"]["created_at"].as_i64().unwrap();
    let started = before["tasks"]["task-1"]["state_since"].as_i64().unwrap();
    assert!(created > 0 && started >= created, "the task began after its request");
    assert_eq!(before["requests"]["request-2"]["created_at"].as_i64().map(|at| at >= created), Some(true));

    fixture.agent("agent-a", "ask", Command::PublishInformation {
        attachments: vec![], interaction_id: "info-1".into(),
        scope: Scope::Task { task_id: "task-1".into() }, recipient: Recipient::User, body: "FYI".into(),
    });
    let informed = fixture.get_json("/v2/runtime").await;
    assert_eq!(informed["tasks"]["task-1"]["state_since"], before["tasks"]["task-1"]["state_since"], "information does not move the task's state");
    let appeared = informed["inbox_times"]["interaction:info-1"].as_i64().unwrap();
    assert!(appeared >= started, "the inbox row carries the time of the commit that added it");

    fixture.user("cancel", Command::CancelTask { task_id: "task-1".into() });
    let cancelled = fixture.get_json("/v2/runtime").await;
    assert!(cancelled["tasks"]["task-1"]["state_since"].as_i64().unwrap() >= appeared, "a change of state is timed by the commit that made it");
    let history = fixture.domain.store.lock().unwrap().task_history(&"task-1".into()).unwrap();
    assert_eq!(history.last().map(|step| step.state.as_str()), Some("cancelled"));
    assert_eq!(history.len(), 2, "one step per state the task entered: {history:?}");
    assert_eq!(history.last().map(|step| step.at), cancelled["tasks"]["task-1"]["state_since"].as_i64());

    let reopened = DomainRuntime::open(&fixture.home).unwrap();
    assert_eq!(reopened.store.lock().unwrap().task_state_times().unwrap(), fixture.domain.store.lock().unwrap().task_state_times().unwrap(), "times are in the database");
    assert_eq!(reopened.store.lock().unwrap().request_times().unwrap().len(), 3);
}

#[tokio::test]
async fn activity_and_search_answer_as_the_fixture_does() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    observe_sessions(&fixture);
    let live = &fixture.domain.live;
    live.observe(&hook("agent-a", "SubagentStart", json!({"agent_id": "sub-1", "agent_type": "Explore"})));
    let expected = fixture_json("requests.json");

    let activity = fixture.get_json("/v2/agents/agent-a/activity").await;
    assert_covers(&expected["activity"]["response"], &activity, "activity");
    let threads = activity["activity"].as_array().unwrap();
    let main = threads.iter().find(|thread| thread["agent_id"].is_null()).unwrap();
    assert_eq!((main["tool"].as_str(), main["summary"].as_str()), (Some("Bash"), Some("bazel test //crates/...")));
    assert_eq!(main["task"], "task-1", "the thread with no task of its own works on the agent's only open task");
    let sub = threads.iter().find(|thread| thread["agent_id"] == "sub-1").unwrap();
    assert_eq!(sub["agent_type"], "Explore");
    assert_covers(&expected["activity"]["response"]["activity"][1], sub, "activity.subagent");
    let idle = fixture.get_json("/v2/agents/agent-b/activity").await;
    assert!(idle["activity"].as_array().unwrap().iter().all(|thread| thread["tool"].is_null()), "{idle}");
    assert_eq!(fixture.get("/v2/agents/nobody/activity").await.0, StatusCode::NOT_FOUND);
    assert_eq!(fixture.unsigned_get("/v2/agents/agent-a/activity").await, StatusCode::FORBIDDEN);

    let found = fixture.get_json("/v2/agents/search?q=parser").await;
    assert_covers(&expected["search"]["response"], &found, "search");
    assert_eq!(found["via"], "local");
    assert_eq!(found["sessions"], json!(["agent-b"]), "the parser is in agent-b's last message");
    assert_eq!(fixture.get_json("/v2/agents/search?q=readable").await["sessions"], json!(["agent-a"]), "a task title is searched");
    assert_eq!(fixture.get_json("/v2/agents/search?q=nomatchwhatsoever").await["sessions"], json!([]));
}

#[tokio::test]
async fn unknown_v2_routes_are_a_bare_404_and_reads_are_named_exactly() {
    let fixture = Fixture::start().await;
    for path in ["/v2/nothing", "/v2/runtime/extra", "/v2/agents", "/v2/agents/a/nothing"] {
        let (status, body) = fixture.get(path).await;
        assert_eq!((status, body.len()), (StatusCode::NOT_FOUND, 0), "{path}");
    }
    assert_eq!(fixture.get("/v2/runtime?x=1").await, (StatusCode::NOT_FOUND, vec![]), "a runtime read takes no query");
    assert_eq!(fixture.get("/v2/agents/search").await, (StatusCode::NOT_FOUND, vec![]), "a search needs its query");
    for path in ["/v2/runtime", "/v2/agents/a-1/activity", "/v2/agents/search?q=x%20y"] {
        assert!(approved_read_path(path), "{path}");
    }
    for path in ["/v2/runtime?x=1", "/v2/agents/a%2Fb/activity", "/v2/agents//activity", "/v2/agents/search",
        "/v2/agents/search?q=", "/v2/agents/search?q=a&q=b", "/v2/agents/search?q=a&x=1", "/v2/runtime/", "/v2/agents/a/activity?x=1"] {
        assert!(!approved_read_path(path), "{path}");
    }
}

struct FakeSessions {
    listed: std::sync::Mutex<Vec<String>>,
    leftovers: Option<String>,
    calls: std::sync::Mutex<Vec<Vec<String>>>,
    stuck: bool,
}

impl FakeSessions {
    fn listing(ids: &[&str]) -> Arc<Self> {
        Arc::new(Self { listed: std::sync::Mutex::new(ids.iter().map(|id| id.to_string()).collect()), leftovers: None,
            calls: Default::default(), stuck: false })
    }
}

impl Sessions for FakeSessions {
    fn claude<'a>(&'a self, args: Vec<String>) -> close::Answer<'a, Result<String, String>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(args.clone());
            match args[0].as_str() {
                "agents" => Ok(json!(self.listed.lock().unwrap().iter().map(|id| json!({"id": id})).collect::<Vec<_>>()).to_string()),
                "rm" if self.stuck => Ok(format!("removed {}", args[1])),
                "rm" => {
                    self.listed.lock().unwrap().retain(|id| id != &args[1]);
                    Ok(format!("removed {}", args[1]))
                }
                _ => Err("unexpected".into()),
            }
        })
    }

    fn leftovers<'a>(&'a self, _: &'a str) -> close::Answer<'a, Option<String>> {
        Box::pin(async move { self.leftovers.clone() })
    }
}

#[tokio::test]
async fn closing_a_background_session_runs_claude_rm_and_takes_it_out_of_the_fleet() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    observe_sessions(&fixture);
    fixture.domain.live.observe(&hook("agent-a", "Stop", json!({})));
    fixture.command(Actor::System, "connect-a", Command::ObserveAgent { agent_id: "agent-a".into(), connected: true });
    let short = fixture.domain.live.node("agent-a").unwrap().short_id;
    let sessions = FakeSessions::listing(&[&short]);
    fixture.client.use_sessions(sessions.clone());
    let expected = fixture_json("requests.json");
    let request = &expected["close"];
    let bearer = fixture.domain.bind_agent("agent-a").unwrap().expose_for_child_environment().to_owned();

    let (status, answer) = fixture.send("POST", request["path"].as_str().unwrap(), &request["body"]).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_covers(&request["response"], &answer, "close");
    assert_eq!(answer["interactive"], false);
    let calls = sessions.calls.lock().unwrap().clone();
    assert!(calls.iter().any(|call| call == &["rm".to_string(), short.clone()]), "{calls:?}");
    assert!(!fixture.domain.state().unwrap().agents[&"agent-a".into()].connected, "the domain sees the session gone");
    assert!(fixture.domain.resolve_bearer(&bearer).is_err(), "its capability is revoked");
    let runtime = fixture.get_json("/v2/runtime").await;
    assert!(runtime["nodes"].as_array().unwrap().iter().all(|node| node["session_id"] != "agent-a"), "what was observed of it is forgotten");
}

#[tokio::test]
async fn closing_an_interactive_session_only_leaves_the_fleet() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    observe_sessions(&fixture);
    fixture.command(Actor::System, "connect-b", Command::ObserveAgent { agent_id: "agent-b".into(), connected: true });
    let sessions = FakeSessions::listing(&[]);
    fixture.client.use_sessions(sessions.clone());
    let (status, answer) = fixture.send("POST", "/v2/agents/agent-b/close", &json!({"force": false})).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["interactive"], true);
    assert!(answer["output"].as_str().unwrap().contains("interactive session"));
    assert!(sessions.calls.lock().unwrap().iter().all(|call| call[0] == "agents"), "claude rm is never run for a terminal session");
    assert!(!fixture.domain.state().unwrap().agents[&"agent-b".into()].connected);
}

#[tokio::test]
async fn a_busy_session_or_one_with_unsaved_work_is_not_closed() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    observe_sessions(&fixture);
    let short = fixture.domain.live.node("agent-a").unwrap().short_id;
    let sessions = Arc::new(FakeSessions {
        listed: std::sync::Mutex::new(vec![short.clone()]), leftovers: Some("2 uncommitted file(s) in /w".into()),
        calls: Default::default(), stuck: false,
    });
    fixture.client.use_sessions(sessions.clone());
    let (status, answer) = fixture.send("POST", "/v2/agents/agent-a/close", &json!({"force": true})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(answer["reasons"], json!([{"kind": "busy"}]), "{answer}");
    assert!(answer["error"].as_str().unwrap().contains("still has something pending"));

    fixture.domain.live.observe(&hook("agent-a", "Stop", json!({})));
    let (status, answer) = fixture.send("POST", "/v2/agents/agent-a/close", &json!({"force": true})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(answer["reasons"], json!([{"kind": "worktree", "detail": "2 uncommitted file(s) in /w"}]), "force never discards work: {answer}");
    assert!(sessions.calls.lock().unwrap().iter().all(|call| call[0] != "rm"), "nothing was removed");

    let stuck = Arc::new(FakeSessions { listed: std::sync::Mutex::new(vec![short.clone()]), leftovers: None, calls: Default::default(), stuck: true });
    fixture.client.use_sessions(stuck);
    let (status, answer) = fixture.send("POST", "/v2/agents/agent-a/close", &json!({"force": true})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert!(answer["error"].as_str().unwrap().contains("is still listed by claude agents"), "{answer}");
    let (status, error) = fixture.send("POST", "/v2/agents/nobody/close", &json!({"force": true})).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")));
}

#[test]
fn writes_are_named_exactly() {
    assert_eq!(parse_write("/v2/agents/agent-a/close"), Some(Write::Close("agent-a".into())));
    for path in ["/v2/agents//close", "/v2/agents/a%2Fb/close", "/v2/agents/a/close?x=1", "/v2/agents/a/other", "/v2/close",
        "/v2/agents/a/close/", "/v2/orchestrator/retry"] {
        assert_eq!(parse_write(path), None, "{path}");
    }
}

#[tokio::test]
async fn chunked_uploads_and_file_chunks_are_for_the_relay_and_not_on_this_mac() {
    let fixture = Fixture::start().await;
    let expected = fixture_json("requests.json");
    for step in ["start", "chunk", "finish", "cancel"] {
        let request = &expected[format!("upload_{step}")];
        let (status, _) = fixture.send("POST", request["path"].as_str().unwrap(), &json!({"transfer_id": "x"})).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{step}");
    }
    assert_eq!(fixture.get("/v2/files/chunk?path=files%2Fatt-1&offset=0").await, (StatusCode::NOT_FOUND, vec![]));
    for path in ["/v2/files/chunk?path=files%2Fatt-1&offset=0", "/v2/files/chunk?path=a&offset=5&version=1-2-3",
        "/v2/files/chunk?offset=0&path=a%2Fb"] {
        assert!(approved_read_path(path), "{path}");
    }
    for path in ["/v2/files/chunk", "/v2/files/chunk?path=a", "/v2/files/chunk?path=a&offset=5", "/v2/files/chunk?path=&offset=0",
        "/v2/files/chunk?path=a&offset=-1", "/v2/files/chunk?path=a&offset=0&offset=1", "/v2/files/chunk?path=a&offset=0&x=1",
        "/v2/files/chunk?path=a&offset=0&version=v1", "/v2/files/chunk?path=a%00b&offset=0"] {
        assert!(!approved_read_path(path), "{path}");
    }
    for step in ["start", "chunk", "finish", "cancel"] {
        assert_eq!(upload_step(&format!("/v2/domain/uploads/{step}")), Some(step));
    }
    for path in ["/v2/domain/uploads", "/v2/domain/uploads/other", "/v2/domain/uploads/start/", "/v2/uploads"] {
        assert_eq!(upload_step(path), None, "{path}");
    }
}

fn write_options(fixture: &Fixture, extra: &str) {
    let projects = fixture.home.join("Projects");
    for name in ["App", "Web"] {
        std::fs::create_dir_all(projects.join(name).join(".git")).unwrap();
    }
    std::fs::write(alondra_core::options::path_in(&fixture.home),
        format!("projects_root = {:?}\n{extra}", projects.to_str().unwrap())).unwrap();
}

struct FakeGithub {
    searches: std::sync::atomic::AtomicUsize,
    found: Value,
}

impl FakeGithub {
    fn with(found: Value) -> Arc<Self> {
        Arc::new(Self { searches: Default::default(), found })
    }

    fn searches(&self) -> usize {
        self.searches.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Github for FakeGithub {
    fn search_issues<'a>(&'a self, _: &'a [String]) -> close::Answer<'a, Result<Value, String>> {
        Box::pin(async move {
            self.searches.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self.found.clone())
        })
    }

    fn issue_view<'a>(&'a self, repo: &'a str, number: u64) -> close::Answer<'a, Result<Value, String>> {
        Box::pin(async move {
            Ok(json!({"url": format!("https://github.com/{repo}/issues/{number}"), "title": "Crash on launch",
                "body": "It crashes when the app opens.", "comments": []}))
        })
    }
}

const USER_OPTION: &str = "
[[option]]
id = \"tests\"
label = \"Tests\"
kind = \"toggle\"
default = true
on = \"Run the whole suite before the PR.\"

[projects.Web]
[[projects.Web.option]]
id = \"deploy\"
label = \"Deploy\"
kind = \"toggle\"
";

#[tokio::test]
async fn options_offer_pr_handling_effort_and_the_users_own() {
    let fixture = Fixture::start().await;
    let expected = fixture_json("requests.json");
    let request = &expected["options"];
    let bare = fixture.get_json("/v2/options").await;
    assert_eq!(bare["options"].as_array().unwrap().iter().map(|o| o["id"].as_str().unwrap()).collect::<Vec<_>>(), ["pr", "effort"], "without an options file the built-in PR handling and effort are offered");
    let shipped = fixture.get_json(request["path"].as_str().unwrap()).await;
    assert_covers(&request["response"], &shipped, "options");
    assert_eq!((shipped["options"][0]["default"].as_str(), shipped["options"][1]["default"].as_str()), (Some("review"), Some("auto")));

    write_options(&fixture, &format!("[defaults]\npr = \"auto_merge\"\n{USER_OPTION}"));
    let app = fixture.get_json("/v2/options?project=App").await;
    let ids = |value: &Value| value["options"].as_array().unwrap().iter().map(|o| o["id"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
    assert_eq!(ids(&app), ["pr", "effort", "tests"], "{app}");
    let pr = app["options"].as_array().unwrap().iter().find(|o| o["id"] == "pr").unwrap();
    assert_eq!(pr["default"], "auto_merge", "the file's default for PR handling is offered as 8ccd131 did");
    assert!(pr["values"].as_array().unwrap().iter().any(|v| v["id"] == "auto_merge"));
    assert_eq!(ids(&fixture.get_json("/v2/options?project=Web").await), ["pr", "effort", "tests", "deploy"], "a project's own options come with it");
    assert_eq!(fixture.get_json("/v2/options?project=Web").await["project"], "Web");

    std::fs::write(alondra_core::options::path_in(&fixture.home), "this is [not toml").unwrap();
    let broken = fixture.get_json("/v2/options?project=App").await;
    assert_eq!(ids(&broken), ["pr", "effort", "tests"], "a broken file keeps the last good options in use");
    assert!(broken["error"].as_str().is_some_and(|error| error.contains("options.toml")), "and says why: {broken}");
    assert_eq!(fixture.unsigned_get("/v2/options").await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn projects_are_the_checkouts_under_the_root_ranked_by_what_the_daemon_saw() {
    let fixture = Fixture::start().await;
    write_options(&fixture, "");
    let expected = fixture_json("requests.json");
    let listed = fixture.get_json(expected["projects"]["path"].as_str().unwrap()).await;
    assert_covers(&expected["projects"]["response"], &listed, "projects");
    let names = |value: &Value| value["projects"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
    assert_eq!(names(&listed), ["App", "Web"]);
    assert_eq!(listed["root"], fixture.home.join("Projects").to_str().unwrap());

    fixture.command(Actor::System, "register-web", Command::RegisterAgent { agent_id: "agent-web".into(), name: "Web agent".into() });
    let cwd = fixture.home.join("Projects").join("Web");
    fixture.domain.live.observe(&hook("agent-web", "SessionStart", json!({"cwd": cwd.to_str().unwrap()})));
    assert_eq!(names(&fixture.get_json("/v2/projects").await), ["Web", "App"], "the project a session is working in comes first");
}

#[tokio::test]
async fn issues_are_searched_once_and_kept_unless_refreshed() {
    let fixture = Fixture::start().await;
    write_options(&fixture, "[issues]\nowners = [\"me\"]\nexclude = [\"me/Secret\"]\n");
    let found = json!([
        {"repository": {"nameWithOwner": "me/App"}, "number": 3, "title": "Crash on launch", "url": "https://github.com/me/App/issues/3"},
        {"repository": {"nameWithOwner": "me/Secret"}, "number": 1, "title": "Hidden", "url": "https://github.com/me/Secret/issues/1"}]);
    let github = FakeGithub::with(found);
    fixture.client.use_github(github.clone());
    let expected = fixture_json("requests.json");
    let first = fixture.get_json(expected["issues"]["path"].as_str().unwrap()).await;
    assert_covers(&expected["issues"]["response"], &first, "issues");
    assert_eq!(first["issues"].as_array().unwrap().len(), 1, "an excluded repository is never offered");
    assert_eq!(first["issues"][0]["repo"], "me/App");
    fixture.get_json("/v2/issues").await;
    assert_eq!(github.searches(), 1, "the second read is served from memory");
    fixture.get_json("/v2/issues?refresh=1").await;
    assert_eq!(github.searches(), 2, "a refresh asks again");
    assert_eq!(fixture.get("/v2/issues?refresh=0").await, (StatusCode::NOT_FOUND, vec![]), "refresh takes only 1");
}

#[tokio::test]
async fn a_new_request_keeps_what_the_user_chose_in_the_same_commit_as_the_request() {
    let fixture = Fixture::start().await;
    write_options(&fixture, &format!("[issues]\nowners = [\"me\"]\n{USER_OPTION}"));
    fixture.client.use_github(FakeGithub::with(json!([])));
    let expected = fixture_json("requests.json");
    let request = &expected["new_request"];
    let path = request["path"].as_str().unwrap();

    let (status, outcome) = fixture.send("POST", path, &request["body"]).await;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    assert_covers(&request["response"], &outcome, "new_request");
    assert_eq!(outcome["created_ids"], json!(["request-9"]));
    assert_eq!(fixture.domain.state().unwrap().requests[&"request-9".into()].body, "Ship it");
    let launch = fixture.domain.request_launch(&"request-9".into()).unwrap().unwrap();
    assert_eq!((launch.project.as_deref(), launch.issue.as_deref()), (Some("App"), Some("me/App#3")));
    assert_eq!((launch.options.effort.as_str(), launch.options.flags.model.as_deref(), launch.options.auto_merge), ("hard", Some("opus"), false));
    assert!(launch.options.picked.iter().any(|p| p.id == "tests"), "the user's option is resolved with the rest");
    assert!(launch.options.picked.iter().any(|p| p.id == "pr" && p.value_label == "review each PR"), "PR handling is kept with the rest");
    assert!(launch.issue_text.as_deref().is_some_and(|text| text.contains("Crash on launch") && text.contains("It crashes when the app opens.")), "{:?}", launch.issue_text);
    assert_eq!(launch.attachments, serde_json::from_value::<Vec<AttachmentRef>>(request["body"]["attachments"].clone()).unwrap());

    let (status, again) = fixture.send("POST", path, &request["body"]).await;
    assert_eq!((status, &again), (StatusCode::OK, &outcome), "sending it again is the same request");
    assert_eq!(fixture.domain.state().unwrap().requests.len(), 1);
    let mut other = request["body"].clone();
    other["text"] = json!("Something else");
    let (status, error) = fixture.send("POST", path, &other).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::CONFLICT, Some("idempotency_conflict")), "one request id, one request");

    fixture.user("task-9", Command::CreateTask {
        task_id: "task-9".into(), request_id: "request-9".into(), title: "Ship".into(), agent_id: None,
        review_prs: true, dependencies: BTreeSet::new(), stack: None });
    let runtime = fixture.get_json("/v2/runtime").await;
    assert_eq!((runtime["tasks"]["task-9"]["project"].as_str(), runtime["tasks"]["task-9"]["effort"].as_str()), (Some("App"), Some("hard")), "its tasks carry what it asked for");
}

#[tokio::test]
async fn a_new_request_is_refused_whole_and_leaves_nothing_behind() {
    let fixture = Fixture::start().await;
    write_options(&fixture, &format!("[issues]\nowners = [\"me\"]\nexclude = [\"me/Secret\"]\n{USER_OPTION}"));
    fixture.client.use_github(FakeGithub::with(json!([])));
    let body = |patch: Value| {
        let mut body = json!({"request_id": "request-1", "text": "Do it", "options": {}, "attachments": []});
        body.as_object_mut().unwrap().extend(patch.as_object().unwrap().clone());
        body
    };
    let send = |patch: Value| {
        let body = body(patch);
        let fixture = &fixture;
        async move { fixture.send("POST", "/v2/requests", &body).await }
    };
    let (status, error) = send(json!({"options": {"effort": "extreme"}})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    let (status, error) = send(json!({"issue": "not an issue"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    let (status, error) = send(json!({"issue": "me/Secret#1"})).await;
    assert!(status == StatusCode::BAD_REQUEST && error["error"].as_str().unwrap().contains("excluded"), "{error}");
    let (status, error) = send(json!({"text": "   "})).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_input")));
    let (status, error) = send(json!({"unknown": 1})).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_input")));
    let bad_file = json!([{"id": "a", "name": "a.png", "content_digest": "short", "content_reference": "files/a", "bytes": 1}]);
    let (status, error) = send(json!({"attachments": bad_file})).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_input")));
    assert!(fixture.domain.state().unwrap().requests.is_empty(), "no refusal created a request");
    assert!(fixture.domain.request_launch(&"request-1".into()).unwrap().is_none());
    let unsigned = reqwest::Client::new().post(format!("{}/v2/requests", fixture.base)).json(&body(json!({}))).send().await.unwrap();
    assert_eq!(unsigned.status(), StatusCode::FORBIDDEN);

    let (status, _) = send(json!({"issue": "me/App#3"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fixture.domain.request_launch(&"request-1".into()).unwrap().unwrap().project.as_deref(), Some("App"), "an issue names its project when it is one of the checkouts");
}

#[test]
fn reads_of_new_work_are_named_exactly() {
    for path in ["/v2/options", "/v2/options?project=App", "/v2/options?project=My%20App", "/v2/projects", "/v2/issues", "/v2/issues?refresh=1"] {
        assert!(approved_read_path(path), "{path}");
    }
    for path in ["/v2/options?project=", "/v2/options?other=1", "/v2/options?project=a&project=b", "/v2/projects?x=1", "/v2/issues?refresh=0",
        "/v2/issues?refresh=1&x=1", "/v2/issues/", "/v2/option"] {
        assert!(!approved_read_path(path), "{path}");
    }
    assert_eq!(parse_write("/v2/requests"), Some(Write::NewRequest));
    assert!(atomic_write_path("/v2/requests"));
}
