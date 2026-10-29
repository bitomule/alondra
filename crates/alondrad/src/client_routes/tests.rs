//! Public routes are exercised with signed requests. Views use the same client
//! in-process, so their reads and actions exercise that client directly.
use super::*;
use crate::auth::Auth;
use crate::domain_bootstrap::DomainDaemon;
use alondra_core::domain::{
    Actor, AttachmentRef, Command, CommandEnvelope, CommandOutcome, PROTOCOL_VERSION, Recipient, Scope,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use std::collections::BTreeSet;
use std::net::SocketAddr;

pub(crate) struct Fixture {
    pub(crate) home: crate::test_support::Directory,
    pub(crate) base: String,
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
    pub(crate) async fn start() -> Self { Self::start_with_accounts(None).await }
    pub(crate) async fn start_with_accounts(accounts: Option<alondra_core::accounts::AccountsConfig>) -> Self {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        if let Some(accounts)=accounts { std::fs::write(alondra_core::options::path_in(&home),accounts.to_toml()).unwrap(); }
        drop(DomainRuntime::open(&home).unwrap());
        let auth = Auth::open(&home).unwrap();
        let key = SigningKey::from_slice(&[31u8; 32]).unwrap();
        let device = auth.pair(&auth.new_pairing_code(), "terminal", &STANDARD.encode(key.verifying_key().to_sec1_bytes())).unwrap();
        let state = DomainDaemon::open(&home).unwrap();
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
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap()
    }

    pub(super) fn user(&self, id: &str, command: Command) -> CommandOutcome {
        self.command(Actor::User { user_id: "u".into() }, id, command)
    }

    pub(super) fn orchestrator(&self, id: &str, command: Command) -> CommandOutcome {
        self.command(Actor::Orchestrator { generation: 0 }, id, command)
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

    pub(crate) fn device_id(&self) -> &str {
        &self.device_id
    }

    pub(crate) async fn get(&self, path: &str) -> (StatusCode, Vec<u8>) {
        let response = self.signed("GET", path, None).send().await.unwrap();
        (response.status(), response.bytes().await.unwrap().to_vec())
    }

    pub(super) async fn get_json(&self, path: &str) -> Value {
        let (status, body) = self.local_get(path).await;
        assert_eq!(status, StatusCode::OK, "{path}: {}", String::from_utf8_lossy(&body));
        serde_json::from_slice(&body).unwrap()
    }

    async fn local_get(&self, path: &str) -> (StatusCode, Vec<u8>) {
        let Some(response) = self.client.read(path).await else { return (StatusCode::NOT_FOUND, vec![]) };
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024).await.unwrap();
        (status, body.to_vec())
    }

    async fn local_send(&self, path: &str, body: &Value) -> (StatusCode, Value) {
        let route = parse_write(path).unwrap();
        let reply = self.client.write("device:test", route, body, None, &|| Ok(())).await.unwrap();
        (reply.status, reply.body)
    }

    async fn runtime_json(&self) -> Value {
        let response = self.client.runtime();
        let body = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    pub(crate) async fn send(&self, method: &str, path: &str, body: &Value) -> (StatusCode, Value) {
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
        fixture.command(Actor::System, &format!("register-{agent}"), Command::RegisterAgent { agent_id: agent.into(), name: name.into(), at: 0 });
    }
    for request in ["request-1", "request-2"] {
        fixture.user(request, Command::CreateRequest { request_id: request.into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    }
    fixture.orchestrator("task-1", Command::CreateTask { plan_items: None,
        task_id: "task-1".into(), request_id: "request-1".into(), title: "Readable editor colours".into(),
        agent_id: Some("agent-a".into()), review_prs: true, dependencies: BTreeSet::new(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan { project: Some("Editor".into()), ..alondra_core::domain::LaunchPlan::new("node", "/tmp") }),
    });
    fixture.command(Actor::System, "answer-2", Command::PublishInformation {
        attachments: vec![], interaction_id: "info-2".into(),
        scope: Scope::Orchestrator { request_id: Some("request-2".into()) }, recipient: Recipient::User,
        body: "Nothing to change: it already works".into(), card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Know, "Read the update"),
    });
    fixture.user("close-2", Command::CloseRequest { request_id: "request-2".into(), answer: None, withdrawn: false, title: None, headline: None });
    fixture.user("request-3", Command::CreateRequest { request_id: "request-3".into(), body: "Never mind".into(), auto_merge: false, maintenance: false, message: false });
    fixture.user("close-3", Command::CloseRequest { request_id: "request-3".into(), answer: None, withdrawn: true, title: Some("Dropped idea".into()), headline: None });
    fixture.agent("agent-a", "pr-1", Command::RegisterPr {
        task_id: "task-1".into(), pr_id: "pr-1".into(), host: "github.com".into(), repository: "me/app".into(),
        number: 7, head: "0123456789abcdef0123456789abcdef01234567".into(), base: "main".into(),
        allowed_methods: vec![alondra_core::domain::MergeMethod::Squash],
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

    assert_eq!(fixture.unsigned_get("/runtime").await, StatusCode::NOT_FOUND);
    let runtime = fixture.runtime_json().await;
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
    assert_eq!(runtime["tasks"]["task-1"]["project"], "Editor", "the task's own project, never its node's");
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
    let before = fixture.runtime_json().await;
    let created = before["requests"]["request-1"]["created_at"].as_i64().unwrap();
    let started = before["tasks"]["task-1"]["state_since"].as_i64().unwrap();
    assert!(created > 0 && started >= created, "the task began after its request");
    assert_eq!(before["requests"]["request-2"]["created_at"].as_i64().map(|at| at >= created), Some(true));

    fixture.command(Actor::System, "ask", Command::PublishInformation {
        attachments: vec![], interaction_id: "info-1".into(),
        scope: Scope::Task { task_id: "task-1".into() }, recipient: Recipient::User, body: "FYI".into(), card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Know, "Read the update"),
    });
    let informed = fixture.runtime_json().await;
    assert_eq!(informed["tasks"]["task-1"]["state_since"], before["tasks"]["task-1"]["state_since"], "information does not move the task's state");
    let appeared = informed["inbox_times"]["interaction:info-1"].as_i64().unwrap();
    assert!(appeared >= started, "the inbox row carries the time of the commit that added it");

    fixture.user("cancel", Command::CancelTask { task_id: "task-1".into() });
    fixture.command(Actor::System, "end", Command::RecordNodeEnd { task_id: "task-1".into(), end: alondra_core::domain::NodeEnd::Closed, agent_id: Some("agent-a".into()) });
    let cancelled = fixture.runtime_json().await;
    assert!(cancelled["tasks"]["task-1"]["state_since"].as_i64().unwrap() >= appeared, "a change of state is timed by the commit that made it");
    let history = fixture.domain.store.lock().unwrap().task_history(&"task-1".into()).unwrap();
    assert_eq!(history.last().map(|step| step.state.as_str()), Some("cancelled"));
    assert_eq!(history.len(), 3, "one step per state the task entered, closing while its node is closed: {history:?}");
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

    let activity = fixture.get_json("/agents/agent-a/activity").await;
    assert_covers(&expected["activity"]["response"], &activity, "activity");
    let threads = activity["activity"].as_array().unwrap();
    let main = threads.iter().find(|thread| thread["agent_id"].is_null()).unwrap();
    assert_eq!((main["tool"].as_str(), main["summary"].as_str()), (Some("Bash"), Some("bazel test //crates/...")));
    assert_eq!(main["task"], "task-1", "the thread with no task of its own works on the agent's only open task");
    let sub = threads.iter().find(|thread| thread["agent_id"] == "sub-1").unwrap();
    assert_eq!(sub["agent_type"], "Explore");
    assert_covers(&expected["activity"]["response"]["activity"][1], sub, "activity.subagent");
    let idle = fixture.get_json("/agents/agent-b/activity").await;
    assert!(idle["activity"].as_array().unwrap().iter().all(|thread| thread["tool"].is_null()), "{idle}");
    assert_eq!(fixture.local_get("/agents/nobody/activity").await.0, StatusCode::NOT_FOUND);
    assert_eq!(fixture.get("/agents/agent-a/activity").await.0, StatusCode::NOT_FOUND);

    let found = fixture.get_json("/agents/search?q=parser").await;
    assert_covers(&expected["search"]["response"], &found, "search");
    assert_eq!(found["via"], "local");
    assert_eq!(found["sessions"], json!(["agent-b"]), "the parser is in agent-b's last message");
    assert_eq!(fixture.get_json("/agents/search?q=readable").await["sessions"], json!(["agent-a"]), "a task title is searched");
    assert_eq!(fixture.get_json("/agents/search?q=nomatchwhatsoever").await["sessions"], json!([]));
}

#[tokio::test]
async fn unknown_domain_routes_are_a_bare_404_and_reads_are_named_exactly() {
    let fixture = Fixture::start().await;
    for path in ["/nothing", "/runtime/extra", "/agents/a/nothing", "/tasks/a/history",
        "/options", "/projects", "/issues", "/agents/search?q=parser", "/agents/a/activity"] {
        let (status, body) = fixture.get(path).await;
        assert_eq!((status, body.len()), (StatusCode::NOT_FOUND, 0), "{path}");
    }
    for path in ["/agents/a/close", "/pull-requests/a/resolve-conflicts"] {
        assert_eq!(fixture.send("POST", path, &json!({})).await.0, StatusCode::NOT_FOUND, "{path}");
    }
    assert_eq!(fixture.get("/agents").await.0, StatusCode::NOT_FOUND);
    assert_eq!(fixture.get("/runtime?x=1").await, (StatusCode::NOT_FOUND, vec![]), "a runtime read takes no query");
    assert_eq!(fixture.get("/agents/search").await, (StatusCode::NOT_FOUND, vec![]), "a search needs its query");
    for path in ["/agents/a-1/activity", "/agents/search?q=x%20y"] {
        assert!(approved_read_path(path), "{path}");
    }
    for path in ["/runtime", "/runtime?x=1", "/agents/a%2Fb/activity", "/agents//activity", "/agents/search",
        "/agents/search?q=", "/agents/search?q=a&q=b", "/agents/search?q=a&x=1", "/runtime/", "/agents/a/activity?x=1"] {
        assert!(!approved_read_path(path), "{path}");
    }
}

struct FakeSessions {
    listed: std::sync::Mutex<Vec<String>>,
    leftovers: Option<String>,
    calls: std::sync::Mutex<Vec<Vec<String>>>,
    stuck: bool,
}

/// `claude rm` that drops the session's connection while it runs, as the real one does, and lets
/// the revival loop look at the session before it answers.
struct DroppingSessions {
    inner: Arc<FakeSessions>,
    domain: Arc<crate::domain_runtime::DomainRuntime>,
    session: String,
    connection: u64,
    /// Whether the revival could take the session each time it looked at it while it was removed.
    free_to_revive: std::sync::Mutex<Vec<bool>>,
}

const REVIVAL: crate::node_revival::Timing = crate::node_revival::Timing { grace: std::time::Duration::from_millis(10) };

impl Sessions for DroppingSessions {
    fn claude<'a>(&'a self, args: Vec<String>) -> close::Answer<'a, Result<String, String>> {
        Box::pin(async move {
            if args[0] == "rm" {
                self.domain.live.mods.disconnect(&self.session, self.connection);
                let free = self.domain.live.begin_watch(&self.session);
                if free {
                    self.domain.live.end_watch(&self.session);
                }
                self.free_to_revive.lock().unwrap().push(free);
                crate::node_revival::revive(&self.domain, &self.session, &REVIVAL).await;
            }
            self.inner.claude(args).await
        })
    }

    fn leftovers<'a>(&'a self, session_id: &'a str) -> close::Answer<'a, Option<String>> {
        self.inner.leftovers(session_id)
    }
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
async fn closing_a_background_session_runs_claude_rm_and_closes_it() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    observe_sessions(&fixture);
    fixture.domain.live.observe(&hook("agent-a", "Stop", json!({})));
    fixture.command(Actor::System, "connect-a", Command::MoveNode { agent_id: "agent-a".into(), event: alondra_core::domain::NodeEvent::TurnStarted, at: 0 });
    let short = fixture.domain.live.node("agent-a").unwrap().short_id;
    let sessions = FakeSessions::listing(&[&short]);
    fixture.client.use_sessions(sessions.clone());
    let expected = fixture_json("requests.json");
    let request = &expected["close"];
    let bearer = fixture.domain.bind_agent("agent-a").unwrap().expose_for_child_environment().to_owned();

    let (status, answer) = fixture.local_send(request["path"].as_str().unwrap(), &request["body"]).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_covers(&request["response"], &answer, "close");
    assert_eq!(answer["interactive"], false);
    let calls = sessions.calls.lock().unwrap().clone();
    assert!(calls.iter().any(|call| call == &["rm".to_string(), short.clone()]), "{calls:?}");
    assert!(!fixture.domain.state().unwrap().agents[&"agent-a".into()].phase.holds_slot(), "the domain sees the session gone");
    assert!(fixture.domain.resolve_bearer(&bearer).is_err(), "its capability is revoked");
    let runtime = fixture.runtime_json().await;
    assert!(runtime["nodes"].as_array().unwrap().iter().all(|node| node["session_id"] != "agent-a"), "what was observed of it is forgotten");
}

#[tokio::test]
async fn a_node_the_user_closed_leaves_the_tasks_in_progress_stays_gone_after_a_restart_and_is_not_revived() {
    let scratch_directory = crate::test_support::Directory::new();
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    observe_sessions(&fixture);
    fixture.domain.live.observe(&hook("agent-a", "Stop", json!({})));
    fixture.command(Actor::System, "connect-a", Command::MoveNode { agent_id: "agent-a".into(), event: alondra_core::domain::NodeEvent::TurnStarted, at: 0 });
    let short = fixture.domain.live.node("agent-a").unwrap().short_id;
    fixture.client.use_sessions(FakeSessions::listing(&[&short]));
    assert!(in_progress(&fixture, "task-1").await, "its task is in progress before it is closed");

    let (status, answer) = fixture.local_send("/agents/agent-a/close", &json!({})).await;
    assert_eq!(status, StatusCode::OK, "{answer}");

    assert!(!in_progress(&fixture, "task-1").await, "closing takes its task out of those in progress at once");
    let restarted = Live::persisted_in(&fixture.home);
    assert!(restarted.is_closed("agent-a"), "the daemon remembers the close after a restart");
    assert_eq!(restarted.closed_sessions(), ["agent-a"]);
    assert!(!Live::persisted_in(&scratch_directory.parent().unwrap().join("alondra-no-such-home")).is_closed("agent-a"));
}

#[tokio::test]
async fn a_node_closed_with_alondra_close_is_never_revived_while_or_after_it_is_removed() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    observe_sessions(&fixture);
    fixture.domain.live.observe(&hook("agent-a", "Stop", json!({})));
    fixture.command(Actor::System, "connect-a", Command::MoveNode { agent_id: "agent-a".into(), event: alondra_core::domain::NodeEvent::TurnStarted, at: 0 });
    let connection = fixture.domain.connect_node(serde_json::from_value(json!({"session_id": "agent-a", "version": "2.1.287"})).unwrap(), 1);
    let short = fixture.domain.live.node("agent-a").unwrap().short_id;
    let sessions = Arc::new(DroppingSessions { inner: FakeSessions::listing(&[&short]), domain: fixture.domain.clone(),
        session: "agent-a".into(), connection, free_to_revive: Default::default() });
    fixture.client.use_sessions(sessions.clone());
    assert!(fixture.domain.state().unwrap().tasks.values().any(|task| task.agent_id == Some("agent-a".into()) && !task.state.terminal()),
        "the node has an open task, which is what the revival restarts for");

    let (status, answer) = fixture.local_send("/agents/agent-a/close", &json!({})).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(*sessions.free_to_revive.lock().unwrap(), [false], "the revival left the session being closed alone");
    crate::node_revival::revive(&fixture.domain, "agent-a", &REVIVAL).await;
    assert!(!crate::node_revival::needed(&fixture.domain, "agent-a"), "and never brings it back afterwards");
}

#[tokio::test]
async fn closing_an_interactive_session_only_closes_it() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    observe_sessions(&fixture);
    fixture.command(Actor::System, "connect-b", Command::MoveNode { agent_id: "agent-b".into(), event: alondra_core::domain::NodeEvent::TurnStarted, at: 0 });
    let sessions = FakeSessions::listing(&[]);
    fixture.client.use_sessions(sessions.clone());
    let (status, answer) = fixture.local_send("/agents/agent-b/close", &json!({})).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["interactive"], true);
    assert!(answer["output"].as_str().unwrap().contains("interactive session"));
    assert!(sessions.calls.lock().unwrap().iter().all(|call| call[0] == "agents"), "claude rm is never run for a terminal session");
    assert!(!fixture.domain.state().unwrap().agents[&"agent-b".into()].phase.holds_slot());
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_a_node_that_runs_through_the_sdk_is_refused_because_it_ends_with_its_task() {
    let fixture = Fixture::start().await;
    let sdk = crate::sdk_owner::tests::SdkLaunches::enable(&fixture.domain, &fixture.home);
    let worktree = fixture.home.join("worktree");
    std::fs::create_dir(&worktree).unwrap();
    fixture.user("request-sdk", Command::CreateRequest { request_id: "request-sdk".into(), body: "Write the release notes".into(), auto_merge: false, maintenance: false, message: false });
    fixture.orchestrator("task-sdk", Command::CreateTask { plan_items: None,
        task_id: "task-sdk".into(), request_id: "request-sdk".into(), title: "Write the release notes".into(),
        agent_id: None, review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", worktree.to_str().unwrap())),
    });
    let launched = crate::sdk_owner::launch(&fixture.domain, crate::sdk_owner::Launch {
        task: "task-sdk".into(), name: "Release notes".into(), directory: worktree.to_str().unwrap().into(),
        prompt: "Write the release notes".into(), plugin: "/synthetic/plugin".into(), options: Default::default(),
        model: None, account: alondra_core::accounts::AccountId::default_account(),
    }).await.unwrap();
    let agent = AgentId::from(launched["agent_id"].as_str().unwrap());
    sdk.until_answered("task-sdk").await;
    let sessions = FakeSessions::listing(&[]);
    fixture.client.use_sessions(sessions.clone());

    let (status, answer) = fixture.local_send(&format!("/agents/{}/close", agent.0), &json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["code"], "close_refused", "{answer}");
    assert!(answer["error"].as_str().unwrap().contains("ends with its task"), "{answer}");
    assert!(sessions.calls.lock().unwrap().is_empty(), "claude is never asked about an SDK node");
    assert!(fixture.domain.owns_sdk_host(&agent), "its host goes on with the task");
    assert_eq!(fixture.domain.state().unwrap().tasks[&"task-sdk".into()].agent_id, Some(agent));
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
    let (status, answer) = fixture.local_send("/agents/agent-a/close", &json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(answer["reasons"], json!([{"kind": "busy"}]), "{answer}");
    assert_eq!(answer["code"], "close_refused", "a refusal is named so the apps say why: {answer}");
    assert!(answer["error"].as_str().unwrap().contains("still has something pending"));

    fixture.domain.live.observe(&hook("agent-a", "Stop", json!({})));
    let (status, answer) = fixture.local_send("/agents/agent-a/close", &json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(answer["reasons"], json!([{"kind": "worktree", "detail": "2 uncommitted file(s) in /w"}]), "force never discards work: {answer}");
    assert!(sessions.calls.lock().unwrap().iter().all(|call| call[0] != "rm"), "nothing was removed");

    let stuck = Arc::new(FakeSessions { listed: std::sync::Mutex::new(vec![short.clone()]), leftovers: None, calls: Default::default(), stuck: true });
    fixture.client.use_sessions(stuck);
    let (status, answer) = fixture.local_send("/agents/agent-a/close", &json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert!(answer["error"].as_str().unwrap().contains("is still listed by claude agents"), "{answer}");
    assert_eq!(answer["code"], "close_failed");
    let (status, error) = fixture.local_send("/agents/nobody/close", &json!({})).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")));
}

#[test]
fn writes_are_named_exactly() {
    assert_eq!(parse_write("/agents/agent-a/close"), Some(Write::Close("agent-a".into())));
    for path in ["/agents//close", "/agents/a%2Fb/close", "/agents/a/close?x=1", "/agents/a/other", "/close",
        "/agents/a/close/", "/orchestrator/retry"] {
        assert_eq!(parse_write(path), None, "{path}");
    }
}

#[test]
fn conflicts_are_resolved_for_a_pr_named_by_its_repository() {
    let encoded = alondra_core::views::id::path_segment("github.com/bitomule/lumo#163");
    assert_eq!(parse_write(&format!("/pull-requests/{encoded}/resolve-conflicts")),
        Some(Write::ResolveConflicts("github.com/bitomule/lumo#163".into())));
    assert_eq!(parse_write("/pull-requests/pr-163/resolve-conflicts"), Some(Write::ResolveConflicts("pr-163".into())));
    for path in ["/pull-requests/github.com/bitomule/lumo#163/resolve-conflicts", "/pull-requests/a%2Fb/resolve-conflicts"] {
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
    assert_eq!(fixture.get("/files/chunk?path=files%2Fatt-1&offset=0").await, (StatusCode::NOT_FOUND, vec![]));
    for path in ["/files/chunk?path=files%2Fatt-1&offset=0", "/files/chunk?path=a&offset=5&version=1-2-3",
        "/files/chunk?offset=0&path=a%2Fb"] {
        assert!(approved_read_path(path), "{path}");
    }
    for path in ["/files/chunk", "/files/chunk?path=a", "/files/chunk?path=a&offset=5", "/files/chunk?path=&offset=0",
        "/files/chunk?path=a&offset=-1", "/files/chunk?path=a&offset=0&offset=1", "/files/chunk?path=a&offset=0&x=1",
        "/files/chunk?path=a&offset=0&version=v1", "/files/chunk?path=a%00b&offset=0"] {
        assert!(!approved_read_path(path), "{path}");
    }
    for step in ["start", "chunk", "finish", "cancel"] {
        assert_eq!(upload_step(&format!("/uploads/{step}")), Some(step));
    }
    for path in ["/domain/uploads", "/uploads/other", "/uploads/start/", "/uploads"] {
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

    fn close_pr<'a>(&'a self, _: &'a str, _: &'a str, _: u64) -> close::Answer<'a, Result<(), String>> {
        Box::pin(async { Err("closing is not available here".into()) })
    }

    fn viewed_files<'a>(&'a self, _: &'a alondra_core::domain::PullRequest) -> close::Answer<'a, Result<crate::pr_viewed::Marks, String>> {
        Box::pin(async { Err("viewed files are not available here".into()) })
    }

    fn set_viewed<'a>(&'a self, _: &'a str, _: &'a str, _: &'a str, _: bool) -> close::Answer<'a, Result<(), String>> {
        Box::pin(async { Err("viewed files are not available here".into()) })
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
    let bare = fixture.get_json("/options").await;
    assert_eq!(bare["options"].as_array().unwrap().iter().map(|o| o["id"].as_str().unwrap()).collect::<Vec<_>>(), ["pr", "effort"], "without an options file the built-in PR handling and effort are offered");
    let shipped = fixture.get_json(request["path"].as_str().unwrap()).await;
    assert_covers(&request["response"], &shipped, "options");
    assert_eq!((shipped["options"][0]["default"].as_str(), shipped["options"][1]["default"].as_str()), (Some("review"), Some("auto")));

    write_options(&fixture, &format!("[defaults]\npr = \"auto_merge\"\n{USER_OPTION}"));
    let app = fixture.get_json("/options?project=App").await;
    let ids = |value: &Value| value["options"].as_array().unwrap().iter().map(|o| o["id"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
    assert_eq!(ids(&app), ["pr", "effort", "tests"], "{app}");
    let pr = app["options"].as_array().unwrap().iter().find(|o| o["id"] == "pr").unwrap();
    assert_eq!(pr["default"], "auto_merge", "the file's default for PR handling is offered as 8ccd131 did");
    assert!(pr["values"].as_array().unwrap().iter().any(|v| v["id"] == "auto_merge"));
    assert_eq!(ids(&fixture.get_json("/options?project=Web").await), ["pr", "effort", "tests", "deploy"], "a project's own options come with it");
    assert_eq!(fixture.get_json("/options?project=Web").await["project"], "Web");

    std::fs::write(alondra_core::options::path_in(&fixture.home), "this is [not toml").unwrap();
    let broken = fixture.get_json("/options?project=App").await;
    assert_eq!(ids(&broken), ["pr", "effort", "tests"], "a broken file keeps the last good options in use");
    assert!(broken["error"].as_str().is_some_and(|error| error.contains("options.toml")), "and says why: {broken}");
    assert_eq!(fixture.get("/options").await.0, StatusCode::NOT_FOUND);
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

    fixture.command(Actor::System, "register-web", Command::RegisterAgent { agent_id: "agent-web".into(), name: "Web agent".into(), at: 0 });
    let cwd = fixture.home.join("Projects").join("Web");
    fixture.domain.live.observe(&hook("agent-web", "SessionStart", json!({"cwd": cwd.to_str().unwrap()})));
    assert_eq!(names(&fixture.get_json("/projects").await), ["Web", "App"], "the project a session is working in comes first");
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
    fixture.get_json("/issues").await;
    assert_eq!(github.searches(), 1, "the second read is served from memory");
    fixture.get_json("/issues?refresh=1").await;
    assert_eq!(github.searches(), 2, "a refresh asks again");
    assert_eq!(fixture.local_get("/issues?refresh=0").await, (StatusCode::NOT_FOUND, vec![]), "refresh takes only 1");
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

    fixture.orchestrator("task-9", Command::CreateTask { plan_items: None,
        task_id: "task-9".into(), request_id: "request-9".into(), title: "Ship".into(), agent_id: None,
        review_prs: true, dependencies: BTreeSet::new(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan { project: Some("App".into()), ..alondra_core::domain::LaunchPlan::new("node", "/tmp") }), });
    let runtime = fixture.runtime_json().await;
    assert_eq!((runtime["tasks"]["task-9"]["project"].as_str(), runtime["tasks"]["task-9"]["effort"].as_str()), (Some("App"), Some("hard")), "its tasks carry what it asked for");
}

#[tokio::test]
async fn a_request_that_chose_auto_merge_is_recorded_as_one() {
    let fixture = Fixture::start().await;
    for (id, options, auto_merge) in [("auto", json!({"pr": "auto_merge"}), true), ("manual", json!({"pr": "review"}), false)] {
        let (status, outcome) = fixture.send("POST", "/requests", &json!({"request_id": id, "text": "Ship it", "options": options, "attachments": []})).await;
        assert_eq!(status, StatusCode::OK, "{outcome}");
        assert_eq!(fixture.domain.state().unwrap().requests[&id.into()].auto_merge, auto_merge, "{id}");
    }
}

#[tokio::test]
async fn a_new_request_reaches_the_orchestrators_next_batch() {
    let fixture = Fixture::start().await;
    write_options(&fixture, "");
    let request = json!({"request_id": "request-1", "text": "Make the editor readable", "options": {}, "attachments": []});
    let (status, outcome) = fixture.send("POST", "/requests", &request).await;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    assert_eq!(outcome["effects"], json!([{"kind": "notify_orchestrator", "request_id": "request-1"}]));
    let pending = fixture.domain.store.lock().unwrap().pending_messages().unwrap();
    let batch: Vec<_> = pending.iter().filter(|m| m.recipient_kind == "orchestrator").collect();
    assert_eq!(batch.len(), 1, "{pending:?}");
    assert!(batch[0].message.contains("request-1"), "{}", batch[0].message);

    let root = crate::test_support::Directory::new();
    std::fs::create_dir(&root).unwrap();
    let orchestrator = crate::domain_orchestrator::testing::Fixture::over(root, fixture.domain.clone(), fixture.home.to_path_buf());
    orchestrator.tick().await;
    let prompt = orchestrator.prompts().pop().expect("the batch reached Claude");
    assert!(prompt.contains("The user created Request request-1") && prompt.contains("request request-1: Make the editor readable"), "{prompt}");
    assert!(fixture.domain.store.lock().unwrap().pending_messages().unwrap().iter()
        .filter(|m| m.recipient_kind == "orchestrator")
        .all(|m| m.interaction_id.as_deref() == Some("request-unsplit-request-1-1")),
        "the batch was handed; only the reminder of the request it left without a task waits");
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
        async move { fixture.send("POST", "/requests", &body).await }
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
    let unsigned = reqwest::Client::new().post(format!("{}/requests", fixture.base)).json(&body(json!({}))).send().await.unwrap();
    assert_eq!(unsigned.status(), StatusCode::FORBIDDEN);

    let (status, _) = send(json!({"issue": "me/App#3"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fixture.domain.request_launch(&"request-1".into()).unwrap().unwrap().project.as_deref(), Some("App"), "an issue names its project when it is one of the checkouts");
}

#[test]
fn reads_of_new_work_are_named_exactly() {
    for path in ["/options", "/options?project=App", "/options?project=My%20App", "/projects", "/issues", "/issues?refresh=1"] {
        assert!(approved_read_path(path), "{path}");
    }
    for path in ["/options?project=", "/options?other=1", "/options?project=a&project=b", "/projects?x=1", "/issues?refresh=0",
        "/issues?refresh=1&x=1", "/issues/", "/option"] {
        assert!(!approved_read_path(path), "{path}");
    }
    assert_eq!(parse_write("/requests"), Some(Write::NewRequest));
    assert!(atomic_write_path("/requests"));
}

#[tokio::test]
async fn a_message_to_the_orchestrator_is_a_new_request_with_the_default_options() {
    let fixture = Fixture::start().await;
    let file = json!({"id": "att-1", "name": "shot.png", "content_digest": "d".repeat(64), "content_reference": "files/domain-v2/x", "bytes": 3});
    let (status, outcome) = fixture.send("POST", "/orchestrator/messages", &json!({"text": "Plan the week", "attachments": [file]})).await;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    let id = outcome["created_ids"][0].as_str().unwrap().to_owned();
    assert_eq!(fixture.domain.state().unwrap().requests[&id.as_str().into()].body, "Plan the week");
    let launch = fixture.domain.request_launch(&id.as_str().into()).unwrap().unwrap();
    assert_eq!((launch.options.effort.as_str(), launch.attachments.len(), launch.project), ("auto", 1, None));
    let (status, _) = fixture.send("POST", "/orchestrator/messages", &json!({"text": "Again"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fixture.domain.state().unwrap().requests.len(), 2, "each message is its own request");
    for body in [json!({"text": "  "}), json!({"text": "x", "kind": "reply"}), json!({})] {
        assert_eq!(fixture.send("POST", "/orchestrator/messages", &body).await.0, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    assert_eq!(fixture.domain.state().unwrap().requests.len(), 2);
    assert_eq!(parse_write("/orchestrator/messages"), Some(Write::OrchestratorMessage));
    assert!(atomic_write_path("/orchestrator/messages"));
}

#[tokio::test]
async fn a_tasks_history_lists_the_states_it_entered_with_the_words_that_moved_it() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    fixture.agent("agent-a", "start", Command::StartTask { task_id: "task-1".into() });
    fixture.agent("agent-a", "ask", Command::AskQuestion {
        attachments: vec![], interaction_id: "q-1".into(), scope: Scope::Task { task_id: "task-1".into() },
        recipient: Recipient::User, body: "Which colour?\nThe blue or the green one.".into(), blocking: true,
        options: vec![alondra_core::domain::QuestionOption { id: "blue".into(), label: "Blue".into(), description: None }], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
    });
    fixture.user("answer", Command::AnswerQuestion { attachments: vec![], interaction_id: "q-1".into(), option_id: "blue".into(), inputs: Default::default() });
    fixture.user("cancel", Command::CancelTask { task_id: "task-1".into() });
    fixture.command(Actor::System, "end", Command::RecordNodeEnd { task_id: "task-1".into(), end: alondra_core::domain::NodeEnd::Closed, agent_id: Some("agent-a".into()) });
    let expected = fixture_json("requests.json");
    let request = &expected["history"];
    let history = fixture.get_json(request["path"].as_str().unwrap()).await;
    assert_covers(&json!({"history": request["response"]["history"]}), &history, "history");
    assert_eq!(history["brief"], Value::Null, "a v2 task has no brief to give yet, and the key is there for the apps");
    let steps: Vec<(&str, &str)> = history["history"].as_array().unwrap().iter()
        .map(|step| (step["state"].as_str().unwrap(), step["summary"].as_str().unwrap())).collect();
    assert_eq!(steps, [("not_started", ""), ("running", ""), ("needs_you", "Which colour?"), ("running", ""), ("closing", ""), ("cancelled", "")]);
    let times: Vec<i64> = history["history"].as_array().unwrap().iter().map(|step| step["at"].as_i64().unwrap()).collect();
    assert!(times.iter().all(|at| *at > 0) && times.windows(2).all(|pair| pair[0] <= pair[1]), "{times:?}");
    let (status, error) = (fixture.local_get("/tasks/nothing/history").await, ());
    let _ = error;
    assert_eq!(status.0, StatusCode::NOT_FOUND);
    assert_eq!(fixture.get("/tasks/task-1/history").await.0, StatusCode::NOT_FOUND);
    assert!(approved_read_path("/tasks/task-1/history") && !approved_read_path("/tasks/task-1/history?x=1") && !approved_read_path("/tasks//history"));
}

#[tokio::test]
async fn the_reason_for_cancelling_is_recorded_beside_the_cancelled_task() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    let expected = fixture_json("requests.json");
    let request = &expected["cancel_reason"];
    let path = request["path"].as_str().unwrap();
    let (status, error) = fixture.send("PUT", path, &request["body"]).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::CONFLICT, Some("task_not_cancelled")), "a running task has no cancel reason");
    fixture.user("cancel", Command::CancelTask { task_id: "task-1".into() });
    let (status, answer) = fixture.send("PUT", path, &request["body"]).await;
    assert_eq!((status, &answer), (StatusCode::OK, &request["response"]), "{answer}");
    assert_eq!(fixture.runtime_json().await["tasks"]["task-1"]["cancel_reason"], "Superseded");
    assert_eq!(fixture.send("PUT", path, &json!({"reason": "  Replaced by task-2  "})).await.0, StatusCode::OK);
    assert_eq!(fixture.runtime_json().await["tasks"]["task-1"]["cancel_reason"], "Replaced by task-2", "the latest words are kept, trimmed");
    for body in [json!({"reason": "   "}), json!({"reason": "x".repeat(4097)}), json!({"reason": "a", "extra": 1}), json!({})] {
        assert_eq!(fixture.send("PUT", path, &body).await.0, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    assert_eq!(fixture.send("PUT", "/tasks/nothing/cancel-reason", &json!({"reason": "x"})).await.0, StatusCode::NOT_FOUND);
    assert_eq!(fixture.runtime_json().await["tasks"]["task-1"]["cancel_reason"], "Replaced by task-2", "refusals change nothing");
    let unsigned = reqwest::Client::new().put(format!("{}{path}", fixture.base)).json(&json!({"reason": "x"})).send().await.unwrap();
    assert_eq!(unsigned.status(), StatusCode::FORBIDDEN);
    assert_eq!(parse_put("/tasks/task-1/cancel-reason"), Some(Write::CancelReason("task-1".into())));
    assert_eq!(parse_put("/tasks//cancel-reason"), None);
    assert_eq!(parse_write("/tasks/task-1/cancel-reason"), None, "it replaces, so it is a PUT and never a POST");
}

#[tokio::test]
async fn the_presence_ping_is_recorded_for_the_client_that_sent_it() {
    let fixture = Fixture::start().await;
    let expected = fixture_json("requests.json");
    let request = &expected["presence"];
    assert!(fixture.domain.live.last_touched("tui").is_none());
    let (status, answer) = fixture.send("POST", request["path"].as_str().unwrap(), &request["body"]).await;
    assert_eq!((status, &answer), (StatusCode::OK, &request["response"]));
    assert!(fixture.domain.live.last_touched("tui").is_some_and(|at| at > 0));
    assert_eq!(fixture.send("POST", "/activity", &json!({"client": ""})).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(fixture.send("POST", "/activity", &json!({})).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let unsigned = reqwest::Client::new().post(format!("{}/activity", fixture.base)).json(&json!({"client": "x"})).send().await.unwrap();
    assert_eq!(unsigned.status(), StatusCode::FORBIDDEN);
    assert!(fixture.domain.live.last_touched("x").is_none());
}

#[tokio::test]
async fn an_uncertain_delivery_is_resolved_on_this_mac_by_the_paired_device() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    fixture.command(Actor::System, "info-1", Command::PublishInformation {
        attachments: vec![], interaction_id: "info-1".into(), scope: Scope::Task { task_id: "task-1".into() },
        recipient: Recipient::User, body: "FYI".into(), card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Know, "Read the update"),
    });
    let (attempt, message) = {
        let mut store = fixture.domain.store.lock().unwrap();
        let message = store.pending_messages().unwrap().into_iter().find(|item| item.recipient_kind == "user").unwrap();
        let claim = store.claim_message(message.id).unwrap().unwrap();
        store.finish_message(claim.attempt_id, claim.version, alondra_core::delivery::State::Uncertain, Some("push_unknown")).unwrap();
        (claim.attempt_id, message.id)
    };
    let epoch = fixture.domain.state().unwrap().data_epoch.clone();
    let path = format!("/delivery-attempts/{attempt}/resolve");
    let body = |decision: &str, version: u64| json!({"protocol_version": 2, "data_epoch": epoch, "expected_version": version, "decision": decision});
    let unsigned = reqwest::Client::new().post(format!("{}{path}", fixture.base)).json(&body("retry", 2)).send().await.unwrap();
    assert_eq!(unsigned.status(), StatusCode::FORBIDDEN);
    let (status, stale) = fixture.send("POST", &path, &body("retry", 1)).await;
    assert_ne!(status, StatusCode::OK, "a decision on a version that moved is refused: {stale}");
    let (status, resolved) = fixture.send("POST", &path, &body("retry", 2)).await;
    assert_eq!((status, resolved["state"].as_str()), (StatusCode::OK, Some("retryable")), "{resolved}");
    assert!(fixture.domain.store.lock().unwrap().pending_messages().unwrap().iter().any(|item| item.id == message), "a retried message is offered again");
    assert_eq!(fixture.send("POST", "/delivery-attempts/0/resolve", &body("retry", 2)).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(fixture.send("POST", "/delivery-attempts/nothing/resolve", &body("retry", 2)).await.0, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn resolving_conflicts_is_refused_before_anything_is_typed_when_it_cannot_be_asked() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    let expected = fixture_json("requests.json");
    let path = expected["resolve_conflicts"]["path"].as_str().unwrap();
    let (status, error) = fixture.local_send("/pull-requests/nothing/resolve-conflicts", &json!({})).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")));
    let (status, error) = fixture.local_send(path, &json!({"extra": 1})).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_input")));
    fixture.command(Actor::System, "closed", Command::ObservePrState {
        pr_id: "pr-1".into(), head: "0123456789abcdef0123456789abcdef01234567".into(), base: "main".into(),
        state: alondra_core::domain::PrState::Closed, merged_head: None });
    let (status, error) = fixture.local_send(path, &json!({})).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::CONFLICT, Some("pr_not_open")));
    let unsigned = reqwest::Client::new().post(format!("{}{path}", fixture.base)).json(&json!({})).send().await.unwrap();
    assert_eq!(unsigned.status(), StatusCode::NOT_FOUND);
    assert_eq!(parse_write(path), Some(Write::ResolveConflicts("pr-1".into())));
    assert!(!atomic_write_path(path), "typing into a session is never replayed");
}

async fn view_json(fixture: &Fixture, key: &str, lang: &str) -> Value {
    view_json_on(fixture, key, lang, "ios").await
}

async fn view_json_on(fixture: &Fixture, key: &str, lang: &str, client: &str) -> Value {
    let path = format!("/views/{key}?protocol={}&client={client}&lang={lang}", alondra_core::views::VIEWS_PROTOCOL);
    let response = crate::views::routes::relay(&fixture.client, "device:test", &path).await;
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024).await.unwrap()).unwrap()
}

/// Whether the sidebar lists the task among those whose node works on them.
async fn in_progress(fixture: &Fixture, task: &str) -> bool {
    let sidebar = view_json_on(fixture, "sidebar", "en", "mac").await;
    sidebar["sections"].as_array().unwrap().iter()
        .filter(|section| section["id"].as_str().is_some_and(|id| id.starts_with("project:")))
        .flat_map(|section| section["items"].as_array().unwrap()).any(|item| item["id"] == task)
}

/// The facts of one section of a view.
fn section_facts(view: &Value, id: &str) -> Vec<Value> {
    view["sections"].as_array().unwrap().iter().find(|section| section["id"] == id)
        .unwrap_or_else(|| panic!("no {id} section in {view}"))["items"][0]["items"].as_array().unwrap().clone()
}

/// The task's state, then what its node runs in the background, as the task's screen says them.
async fn task_shown(fixture: &Fixture, task: &str, lang: &str) -> Vec<(String, String)> {
    let detail = view_json(fixture, &format!("task:{task}"), lang).await;
    detail["sections"].as_array().unwrap().iter()
        .filter(|section| section["id"] == "header" || section["id"] == "readings")
        .flat_map(|section| section["items"][0]["items"].as_array().unwrap().clone())
        .filter(|fact| ["State", "Estado", "In the background", "En segundo plano"].contains(&fact["label"].as_str().unwrap()))
        .map(|fact| (fact["value"].as_str().unwrap().to_owned(), fact["tone"].as_str().unwrap_or("neutral").to_owned()))
        .collect()
}

#[tokio::test]
async fn the_task_detail_shows_what_its_node_mod_reports() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    observe_sessions(&fixture);
    let mods = &fixture.domain;
    let report = |report: Value| {
        let mut report = report;
        report["session_id"] = json!("agent-a");
        mods.report(serde_json::from_value(report).unwrap(), alondra_core::now_millis());
    };
    let hello = serde_json::from_value(json!({"session_id": "agent-a", "version": "2.1.287"})).unwrap();
    let connection = mods.connect_node(hello, 1);
    report(json!({"event": "turn.start", "turn_id": "t1"}));
    report(json!({"event": "tool.start", "tool": "Bash", "tool_use_id": "u1",
        "input": {"command": "bazel test //crates/..."}}));
    report(json!({"event": "model", "model": "claude-opus-5-5"}));

    let detail = view_json(&fixture, "task:task-1", "es").await;
    let readings = section_facts(&detail, "readings");
    assert!(readings.iter().any(|fact| fact["label"] == "Ejecutando"
        && fact["value"].as_str().is_some_and(|value| value.starts_with("Bash: bazel test //crates/... · desde las "))), "{readings:?}");
    let options = section_facts(&detail, "options");
    assert!(options.iter().any(|fact| fact["label"] == "Modelo" && fact["value"] == "Opus 5.5"), "{options:?}");

    report(json!({"event": "turn.complete", "turn_id": "t1", "answer": "Stopped halfway through the parser", "aborted": true}));
    crate::domain_agent_lifecycle::node_event(&fixture.domain, &AgentId::from("agent-a"), alondra_core::domain::NodeEvent::TurnEnded).unwrap();
    mods.live.mods.disconnect("agent-a", connection);
    let shown = task_shown(&fixture, "task-1", "en").await;
    assert!(!shown.iter().any(|(text, _)| text.contains("Unreachable") || text.contains("conexión")), "a closed connection is never a state: {shown:?}");
    let detail = view_json(&fixture, "task:task-1", "en").await;
    let readings = section_facts(&detail, "readings");
    assert!(!readings.iter().any(|fact| fact["label"] == "Connection"), "{readings:?}");
    assert!(readings.iter().any(|fact| fact["label"] == "Last turn" && fact["value"] == "Interrupted"), "{readings:?}");
    let said = detail["sections"].as_array().unwrap().iter().find(|section| section["id"] == "last_message").unwrap();
    assert_eq!(said["items"][0]["text"], "Stopped halfway through the parser", "the answer is the mod's, not the Stop hook's");
}

#[tokio::test]
async fn a_nodes_state_comes_from_its_mods_events_and_connection_not_from_hooks_or_job_files() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    observe_sessions(&fixture);
    let mods = &fixture.domain;
    let now = alondra_core::now_millis();
    let report = |report: Value, at: i64| {
        let mut report = report;
        report["session_id"] = json!("agent-a");
        mods.report(serde_json::from_value(report).unwrap(), at);
    };
    let state = async |fixture: &Fixture| task_shown(fixture, "task-1", "en").await[0].0.clone();
    let hello: crate::node_reports::Hello = serde_json::from_value(json!({"session_id": "agent-a", "version": "2.1.287"})).unwrap();
    assert_ne!(state(&fixture).await, "Working", "recent hooks alone do not make a node work: only its mod says so");

    let connection = mods.connect_node(hello, now);
    report(json!({"event": "turn.start", "turn_id": "t1"}), now);
    assert_eq!(state(&fixture).await, "Working");

    report(json!({"event": "tool.start", "tool": "Bash", "tool_use_id": "u1", "input": {"command": "sleep 99999"}}), now - 48 * 60 * 60 * 1000);
    assert_eq!(state(&fixture).await, "Working", "a call two days long is still work");
    mods.live.mods.set_shell_alive("agent-a", Some(false), now);
    mods.live.mods.set_shell_alive("agent-a", Some(false), now + 1);
    assert_eq!(state(&fixture).await, "Stalled", "its shell is gone: the call will never end");
    report(json!({"event": "tool.end", "tool_use_id": "u1"}), now);
    assert_eq!(state(&fixture).await, "Working");

    report(json!({"event": "turn.complete", "turn_id": "t1", "answer": "Done."}), now);
    crate::domain_agent_lifecycle::node_event_at(&fixture.domain, &AgentId::from("agent-a"), alondra_core::domain::NodeEvent::TurnEnded, now).unwrap();
    assert_ne!(state(&fixture).await, "Working", "the turn ended, as the Stop hook says: the node is idle");

    report(json!({"event": "turn.start", "turn_id": "t2"}), now);
    mods.live.mods.disconnect("agent-a", connection);
    assert_eq!(state(&fixture).await, "Working", "a node whose connection closed keeps its last factual state while the daemon brings it back");
    crate::domain_agent_lifecycle::node_event(&fixture.domain, &AgentId::from("agent-a"), alondra_core::domain::NodeEvent::SessionLost).unwrap();
    assert!(!in_progress(&fixture, "task-1").await, "its session is gone: its task is not in progress while it waits to be brought back");
    assert_eq!(state(&fixture).await, "Waiting for the machine", "and its screen says it waits");
    crate::domain_agent_lifecycle::node_event(&fixture.domain, &AgentId::from("agent-a"), alondra_core::domain::NodeEvent::RevivalAdmitted).unwrap();
    assert_eq!(state(&fixture).await, "Reconnecting", "the gate let it back: Alondra restarts it");
    assert!(in_progress(&fixture, "task-1").await, "the task of a node being restarted is in progress");
}

/// The last minute of the Mac as `pace` samples it: macOS at critical pressure, or normal with room.
fn memory(fixture: &Fixture, pressure: alondra_core::MemoryPressure, at: &[i64]) {
    for &at in at {
        let machine = alondra_core::Machine { at, memory_free_percent: Some(50), memory_pressure: Some(pressure), cpus: 11, load: [1.0; 3],
            ..alondra_core::Machine::default() };
        crate::machine_watch::pace(&fixture.domain, &machine);
    }
}

/// 13 October: quill's (1d6d53c5) session ended and its restart waited at the admission gate for
/// memory ("waits at the admission gate to come back: Pressure(Warning)", every 30 s), while its task
/// and its screen said «Reconectando». A lost node held by the gate waits for the machine, queued
/// with what holds it, and is brought back only once the gate lets it.
#[tokio::test]
async fn a_lost_node_the_gate_holds_for_memory_waits_for_the_machine_and_comes_back_only_once_admitted() {
    use alondra_core::{MemoryPressure, domain::NodeEvent, domain::NodePhase};
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    observe_sessions(&fixture);
    fixture.agent("agent-a", "start-1", Command::StartTask { task_id: "task-1".into() });
    let agent = AgentId::from("agent-a");
    let hello = serde_json::from_value(json!({"session_id": "agent-a", "version": "2.1.287"})).unwrap();
    let connection = fixture.domain.connect_node(hello, alondra_core::now_millis());
    fixture.domain.live.mods.disconnect("agent-a", connection);
    crate::domain_agent_lifecycle::node_event(&fixture.domain, &agent, NodeEvent::SessionLost).unwrap();
    let now = alondra_core::now_millis();
    memory(&fixture, MemoryPressure::Critical, &[now - 120_000, now - 90_000, now - 61_000]);

    let timing = crate::node_revival::Timing { grace: Duration::from_millis(30) };
    let revival = tokio::spawn({
        let domain = fixture.domain.clone();
        async move { crate::node_revival::revive(&domain, "agent-a", &timing).await }
    });
    let queued = || fixture.domain.live.gate.live(&fixture.domain, alondra_core::now_millis()).unwrap().queued;
    for _ in 0..500 {
        if !queued().is_empty() { break; }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(queued().iter().any(|queued| queued.task_id == "task-1"), "its restart waits at the gate");
    let phase = || fixture.domain.state().unwrap().agents[&agent].phase.clone();
    assert_eq!(phase(), NodePhase::Lost);
    assert!(!revival.is_finished(), "nothing brings it back while the Mac has no room");
    assert!(task_shown(&fixture, "task-1", "es").await[0].0.starts_with("En cola"), "its task's screen says it is queued");
    let held = view_json(&fixture, "tasks", "es").await.to_string();
    assert!(held.contains("En cola · 1.º de 1") && held.contains("El Mac va justo de memoria") && !held.contains("Reconectando"), "{held}");

    memory(&fixture, MemoryPressure::Normal, &[alondra_core::now_millis()]);
    crate::admission::pass(&fixture.domain, &fixture.home).await.unwrap();
    revival.await.unwrap();
    assert!(!queued().iter().any(|queued| queued.task_id == "task-1"), "the gate let it back");
}

/// 25 October: the disk filled with the nodes' builds and the daemon restarted without space. With
/// less room on the disk than one more build needs no node starts, and its task waits for capacity
/// with the free space and what is needed; once the disk has room the gate lets it start.
#[tokio::test]
async fn a_task_waits_for_room_on_the_disk_and_starts_once_the_disk_has_it() {
    use crate::admission::{Hold, Key, Waiting};
    const GB: u64 = 1_000_000_000;
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    fixture.user("title-1", Command::TitleRequest { request_id: "request-1".into(), title: "Editor".into() });
    fixture.orchestrator("task-9", Command::CreateTask { plan_items: None,
        task_id: "task-9".into(), request_id: "request-1".into(), title: "Faster search".into(),
        agent_id: None, review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
    });
    let watch = &fixture.domain.live.machine_watch;
    let ask = || fixture.domain.live.gate.ask(&fixture.domain, Waiting { key: Key::Task("task-9".into()), priority: alondra_core::domain::TaskPriority::Normal, resuming: false, since: 0 }, alondra_core::now_millis());
    watch.measure_disk(crate::disk::Space { free: 6 * GB, purgeable: 0, total: 500 * GB });
    assert_eq!(ask(), Err(Hold::Disk { free: 6 * GB, needed: 10 * GB }), "no build measured yet: 5 GB for one on top of the 5 GB line");
    assert_eq!(task_shown(&fixture, "task-9", "en").await[0].0, "Waiting for capacity · Disk · 6.0 GB free of 10.0 GB needed");
    let screen = view_json(&fixture, "task:task-9", "es").await.to_string();
    assert!(screen.contains("Esperando capacidad") && screen.contains("Disco · 6,0 GB libres de 10,0 GB necesarios"), "{screen}");

    assert!(!watch.measure_disk(crate::disk::Space { free: 11 * GB, purgeable: 0, total: 500 * GB }), "past the line, not past its margin");
    assert!(matches!(ask(), Err(Hold::Disk { .. })));
    assert!(watch.measure_disk(crate::disk::Space { free: 4 * GB, purgeable: 9 * GB, total: 500 * GB }), "purgeable space counts");
    assert_eq!(ask(), Ok(()), "the disk has room for one more build: it starts");
}

#[tokio::test]
async fn a_node_the_daemon_never_heard_a_hook_from_shows_once_its_mod_connects() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    let hello: crate::node_reports::Hello = serde_json::from_value(json!({"session_id": "agent-a", "version": "2.1.287", "working": true})).unwrap();
    assert!(fixture.domain.live.node("agent-a").is_none());
    fixture.domain.connect_node(hello, alondra_core::now_millis());
    assert_eq!(task_shown(&fixture, "task-1", "en").await[0].0, "Working", "a restarted daemon learns the node is mid-turn from its reconnection");
}

#[tokio::test]
async fn the_background_badges_come_from_the_mods_subagent_shell_and_end_of_turn_reports() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    observe_sessions(&fixture);
    let mods = &fixture.domain;
    let now = alondra_core::now_millis();
    let report = |report: Value, at: i64| {
        let mut report = report;
        report["session_id"] = json!("agent-a");
        mods.report(serde_json::from_value(report).unwrap(), at);
    };
    let hello: crate::node_reports::Hello = serde_json::from_value(json!({"session_id": "agent-a", "version": "2.1.287"})).unwrap();
    mods.connect_node(hello, now);
    report(json!({"event": "turn.start", "turn_id": "t1"}), now);
    let badge = async |fixture: &Fixture| {
        task_shown(fixture, "task-1", "en").await.into_iter().map(|(text, _)| text).collect::<Vec<_>>()
    };
    assert!(!badge(&fixture).await.iter().any(|text| text.contains("subagents") || text.contains("shells")));

    report(json!({"event": "subagent.start", "id": "sa-1", "description": "Fix the parser"}), now);
    report(json!({"event": "subagent.start", "id": "sa-2", "description": "Review it"}), now);
    report(json!({"event": "shell.start", "id": "u1", "command": "until false; do sleep 5; done"}), now);
    assert!(badge(&fixture).await.contains(&"2 subagents · 1 shell".to_owned()), "the same text as when the job state listed them");
    assert_eq!(badge(&fixture).await[0], "Working", "a node with background work and no turn still works");

    report(json!({"event": "subagent.end", "id": "sa-1"}), now);
    assert!(badge(&fixture).await.contains(&"1 subagent · 1 shell".to_owned()));

    report(json!({"event": "background", "tasks": [{"id": "b1", "type": "monitor", "description": "tail -f log"}]}), now);
    assert!(badge(&fixture).await.contains(&"1 task".to_owned()), "the list at the end of a turn replaces what was reported before");

    report(json!({"event": "background", "tasks": []}), now);
    assert!(!badge(&fixture).await.iter().any(|text| text.contains("tasks")));
}

#[tokio::test]
async fn a_task_shows_the_subagents_workflows_and_task_list_its_nodes_mod_reports_and_lists_a_subagent_once() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    observe_sessions(&fixture);
    let mods = &fixture.domain;
    let now = alondra_core::now_millis();
    let report = |report: Value, at: i64| {
        let mut report = report;
        report["session_id"] = json!("agent-a");
        mods.report(serde_json::from_value(report).unwrap(), at);
    };
    let hello: crate::node_reports::Hello = serde_json::from_value(json!({"session_id": "agent-a", "version": "2.1.287"})).unwrap();
    mods.connect_node(hello, now);
    report(json!({"event": "subagent.start", "id": "sa-1", "description": "Fix the parser", "type": "Explore", "model": "claude-haiku-4-5"}), now);
    report(json!({"event": "workflow.start", "id": "w1", "name": "review-loop"}), now);
    report(json!({"event": "shell.start", "id": "u1", "command": "cargo build"}), now);
    report(json!({"event": "step.create", "id": "1", "subject": "Read"}), now);
    report(json!({"event": "step.create", "id": "2", "subject": "Fix", "active_form": "Fixing"}), now);
    report(json!({"event": "step.update", "id": "1", "status": "completed"}), now);
    report(json!({"event": "step.update", "id": "2", "status": "in_progress"}), now);

    let detail = view_json(&fixture, "task:task-1", "es").await;
    let section = |id: &str| detail["sections"].as_array().unwrap().iter().find(|section| section["id"] == id).cloned()
        .unwrap_or_else(|| panic!("no {id} section in {detail}"));
    let rows = |id: &str| section(id)["items"].as_array().unwrap().iter()
        .filter(|item| item["type"] == "row").cloned().collect::<Vec<_>>();
    assert_eq!(section("steps")["title"], "Pasos");
    assert_eq!(rows("steps").len(), 2);
    let subagents = rows("subagents");
    assert_eq!((subagents[0]["title"].as_str(), subagents[0]["subtitle"].as_str()), (Some("Fix the parser"), Some("Explore · Haiku 4.5")));
    assert_eq!(rows("workflows")[0]["title"], "review-loop");
    let background = rows("background");
    assert_eq!(background.len(), 1, "a subagent or workflow is listed once, under its own section: {background:?}");
    assert_eq!(background[0]["title"], "cargo build");

}

#[tokio::test]
async fn a_background_shell_whose_process_is_gone_ended_and_stops_counting_as_work() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    let mods = &fixture.domain;
    let now = alondra_core::now_millis();
    let hello: crate::node_reports::Hello = serde_json::from_value(json!({"session_id": "agent-a", "version": "2.1.287"})).unwrap();
    mods.connect_node(hello, now);
    mods.report(serde_json::from_value(json!({"session_id": "agent-a", "event": "shell.start", "id": "u1", "command": "sleep 99999"})).unwrap(),
        now - 3 * 60 * 60 * 1000);
    mods.live.mods.set_shell_alive("agent-a", Some(true), now);
    let shown = task_shown(&fixture, "task-1", "en").await;
    assert!(shown.contains(&("1 shell".into(), "neutral".into())), "three hours on a live shell is work: {shown:?}");
    mods.live.mods.set_shell_alive("agent-a", Some(false), now);
    let shown = task_shown(&fixture, "task-1", "en").await;
    assert!(shown.iter().all(|(label, _)| !label.contains("shell")), "nothing runs once its shell is gone: {shown:?}");
    assert_ne!(shown[0].0, "Working", "a shell whose process is gone is not work");
    let detail = view_json(&fixture, "task:task-1", "en").await;
    let background = detail["sections"].as_array().unwrap().iter().find(|section| section["id"] == "background").cloned().unwrap();
    assert_eq!(background["items"][0]["badges"][0]["text"], "Finished", "it ended when the look found it gone: {background}");
}

#[tokio::test]
async fn a_node_whose_session_ended_with_its_task_open_leaves_the_tasks_in_progress_while_alondra_brings_it_back() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    let domain = &fixture.domain;
    let run = |actor: Actor, id: &str, command: Command| {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: 2, data_epoch: state.data_epoch.clone(), command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    };
    let user = || Actor::User { user_id: "u".into() };
    run(Actor::System, "register-stopped", Command::RegisterAgent { agent_id: "stopped".into(), name: "alondra/stopped".into(), at: 0 });
    run(user(), "request-stopped", Command::CreateRequest { request_id: "request-stopped".into(), body: "Probe".into(), auto_merge: false, maintenance: false, message: false });
    run(Actor::Orchestrator { generation: 0 }, "task-stopped", Command::CreateTask { plan_items: None,
        task_id: "task-stopped".into(), request_id: "request-stopped".into(), title: "Probe".into(), agent_id: Some("stopped".into()),
        review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
    });
    let mods = &domain;
    let connection = mods.connect_node(serde_json::from_value(json!({"session_id": "stopped", "version": "2.1.287"})).unwrap(), alondra_core::now_millis());
    mods.report(serde_json::from_value(json!({"session_id": "stopped", "event": "session.end"})).unwrap(), alondra_core::now_millis());
    mods.live.mods.disconnect("stopped", connection);
    crate::domain_agent_lifecycle::move_instance(domain, &"stopped".into(), alondra_core::domain::NodeEvent::SessionLost,
        &crate::domain_agent_lifecycle::prepare_event(domain, "end-stopped").unwrap()).unwrap();

    assert!(!in_progress(&fixture, "task-stopped").await, "no session, not in progress");
    assert_ne!(task_shown(&fixture, "task-stopped", "es").await[0].0, "Trabajando", "its session ended: its task does not say it works");
}

#[tokio::test]
async fn views_and_changes_say_which_daemon_answered_and_an_unserved_protocol_gets_409_with_it() {
    let fixture = Fixture::start().await;
    let service = json!({"version": env!("CARGO_PKG_VERSION"),
        "capabilities": ["account_presentation", "model_selection", "provider_accounts"],
        "views_protocol": {"min": alondra_core::views::VIEWS_PROTOCOL_MIN, "max": alondra_core::views::VIEWS_PROTOCOL}});
    assert_eq!(view_json(&fixture, "home", "en").await["service"], service);

    let changes = format!("/changes?protocol={}&client=tui&lang=en&wait=0", alondra_core::views::VIEWS_PROTOCOL);
    let response = crate::views::routes::relay_feed(&fixture.client, "device:test", &changes).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(body["service"], service);

    let unserved = alondra_core::views::VIEWS_PROTOCOL + 1;
    for path in [
        format!("/views/home?protocol={unserved}&client=tui&lang=en"),
        format!("/changes?protocol={unserved}&client=tui&lang=en&wait=0"),
    ] {
        let response = if path.starts_with("/views/") {
            crate::views::routes::relay(&fixture.client, "device:test", &path).await
        } else {
            crate::views::routes::relay_feed(&fixture.client, "device:test", &path).await
        };
        assert_eq!(response.status(), StatusCode::CONFLICT, "{path}");
        let body: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap();
        assert_eq!((body["code"].as_str(), &body["service"]), (Some("incompatible_protocol"), &service), "{path}");
    }
}

struct HeldIssueSearch {
    entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Arc<tokio::sync::Semaphore>,
}
impl Github for HeldIssueSearch {
    fn search_issues<'a>(&'a self, _: &'a [String]) -> close::Answer<'a, Result<Value, String>> {
        Box::pin(async move {
            if let Some(entered) = self.entered.lock().unwrap().take() { entered.send(()).unwrap(); }
            self.release.acquire().await.unwrap().forget();
            Ok(json!([]))
        })
    }
    fn issue_view<'a>(&'a self, _: &'a str, _: u64) -> close::Answer<'a, Result<Value, String>> {
        Box::pin(async { Err("unexpected issue detail".into()) })
    }
    fn close_pr<'a>(&'a self, _: &'a str, _: &'a str, _: u64) -> close::Answer<'a, Result<(), String>> {
        Box::pin(async { Err("unexpected PR close".into()) })
    }
    fn viewed_files<'a>(&'a self, _: &'a alondra_core::domain::PullRequest) -> close::Answer<'a, Result<crate::pr_viewed::Marks, String>> {
        Box::pin(async { Err("unexpected viewed files".into()) })
    }
    fn set_viewed<'a>(&'a self, _: &'a str, _: &'a str, _: &'a str, _: bool) -> close::Answer<'a, Result<(), String>> {
        Box::pin(async { Err("unexpected viewed mark".into()) })
    }
}
struct ReleaseIssueSearch(Arc<tokio::sync::Semaphore>);
impl Drop for ReleaseIssueSearch {
    fn drop(&mut self) { self.0.add_permits(2); }
}

#[test]
fn runtime_progress_slow_issue_refreshes_leave_the_view_read_lane_free() {
    use std::future::Future;
    let directory = crate::test_support::Directory::new();
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(alondra_core::options::path_in(&directory), "[issues]\nowners = [\"fixture\"]\n").unwrap();
    let domain = Arc::new(DomainRuntime::open(&directory).unwrap());
    let client = Client::open(domain, &directory, Default::default());
    let (entered, started) = tokio::sync::oneshot::channel();
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    *client.github.lock().unwrap() = Arc::new(HeldIssueSearch {
        entered: std::sync::Mutex::new(Some(entered)), release: release.clone(),
    });
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let release = ReleaseIssueSearch(release);
        let mut first = Box::pin(client.read("/issues?refresh=1"));
        assert!(first.as_mut().poll(&mut std::task::Context::from_waker(std::task::Waker::noop())).is_pending());
        started.await.unwrap();
        let mut second = Box::pin(crate::views::routes::relay(&client, "phone", "/views/issue_picker?protocol=1&client=ios&lang=en&refresh=1"));
        assert!(second.as_mut().poll(&mut std::task::Context::from_waker(std::task::Waker::noop())).is_pending());
        assert_eq!(client.read_work.available(), 2, "two pending GitHub requests must not consume the read lane");
        let home = crate::views::routes::relay(&client, "phone", "/views/home?protocol=1&client=ios&lang=en").await;
        assert_eq!(home.status(), StatusCode::OK, "a real home view completes while GitHub is held");
        drop(release);
        let (first, second) = tokio::join!(first, second);
        assert_eq!(first.unwrap().status(), StatusCode::OK);
        assert_eq!(second.status(), StatusCode::OK);
        println!("github_requests_held=2 home_view=ok physical_read_slots_free=2 requests_drained=true");
    });
}

// The routes of the card tasks waiting for an MCP server wait on: signed devices only, on this Mac
// and through the relay, each with its exact body, and none of them one of the relay's atomic
// command writes.
#[tokio::test]
async fn the_mcp_sign_in_routes_take_only_a_signed_device_and_their_own_body() {
    let fixture = Fixture::start().await;
    let card = "mcp-sign-in-task-1-7";
    for step in ["connect", "return", "reconnect", "without"] {
        let path = format!("/mcp-sign-ins/{card}/{step}");
        assert!(approved_write_path(&path) && !atomic_write_path(&path), "{path} goes through the relay as a write of its own");
        let unsigned = reqwest::Client::new().post(format!("{}{path}", fixture.base)).json(&json!({})).send().await.unwrap();
        assert_eq!(unsigned.status(), StatusCode::FORBIDDEN, "{path}");
        let body = if step == "return" { json!({"address": "http://localhost:49200/callback?code=c&state=s"}) } else { json!({}) };
        let (status, answer) = fixture.send("POST", &path, &body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "no such card: {path} {answer}");
        let extra = if step == "return" { json!({"address": "x", "extra": 1}) } else { json!({"extra": 1}) };
        assert_eq!(fixture.send("POST", &path, &extra).await.0, StatusCode::UNPROCESSABLE_ENTITY, "{path}");
    }
    assert_eq!(fixture.send("POST", &format!("/mcp-sign-ins/{card}/return"), &json!({})).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    for refused in ["/mcp-sign-ins/other-card/connect", "/mcp-sign-ins/mcp-sign-in-t/open", "/mcp-sign-ins/mcp-sign-in-t", "/mcp-sign-ins/mcp sign/connect"] {
        assert_eq!(parse_write(refused), None, "{refused}");
    }
}

const PR_HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

/// A `gh` for `me/app#7` at `PR_HEAD`: `pr view` gives the head, `pr diff` the diff in `diff`, and
/// the GraphQL read of its files `marks`.
fn diff_gh(dir: &std::path::Path, marks: &str) -> crate::domain_github::GhCli {
    use std::os::unix::fs::PermissionsExt;
    let diff = dir.join("diff");
    std::fs::write(&diff, "diff --git a/src/a.rs b/src/a.rs\n+a\ndiff --git a/src/b.rs b/src/b.rs\n+b\n").unwrap();
    let marks_file = dir.join("marks");
    std::fs::write(&marks_file, marks).unwrap();
    let program = dir.join("gh");
    std::fs::write(&program, format!(r#"#!/bin/sh
case "$*" in
  *viewerViewedState*) cat '{marks}';;
  *"pr view"*) printf '%s' '{{"headRefOid":"{PR_HEAD}"}}';;
  *"pr diff"*) cat '{diff}';;
  *) exit 1;;
esac
"#, marks = marks_file.display(), diff = diff.display())).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    crate::domain_github::GhCli::at(program)
}

fn marks_page(head: &str) -> String {
    json!({"data": {"repository": {"pullRequest": {"id": "PR_node", "headRefOid": head, "files": {
        "pageInfo": {"hasNextPage": false, "endCursor": null},
        "nodes": [{"path": "src/a.rs", "viewerViewedState": "VIEWED"}, {"path": "src/b.rs", "viewerViewedState": "DISMISSED"}],
    }}}}}).to_string()
}

async fn diff_body(fixture: &Fixture, gh: &crate::domain_github::GhCli) -> Value {
    let response = crate::domain_api::pull_request_diff_from(&fixture.domain, "pr-1".into(), gh).await;
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
}

#[tokio::test]
async fn the_diff_of_a_pull_request_carries_the_viewed_mark_github_keeps_for_each_file() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    let gh = diff_gh(&fixture.home, &marks_page(PR_HEAD));
    let body = diff_body(&fixture, &gh).await;
    assert_eq!(body["head"], PR_HEAD);
    assert!(body["diff"].as_str().unwrap().contains("src/b.rs"));
    assert_eq!(body["files"], json!([{"path": "src/a.rs", "viewed": "viewed"}, {"path": "src/b.rs", "viewed": "changed"}]));
}

#[tokio::test]
async fn the_diff_comes_without_marks_when_github_gives_none_for_its_head() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    for marks in [marks_page("fedcba9876543210fedcba9876543210fedcba98"), json!({"errors": [{"message": "boom"}]}).to_string(), "not json".into()] {
        let gh = diff_gh(&fixture.home, &marks);
        let body = diff_body(&fixture, &gh).await;
        assert!(body["diff"].as_str().unwrap().contains("src/a.rs"), "{marks}");
        assert!(body.get("files").is_none(), "{marks}: {body}");
    }
}

/// Answers the reads and marks of viewed files from memory, and keeps every mark it is asked for.
struct ViewedGithub {
    marks: Result<crate::pr_viewed::Marks, String>,
    set: std::sync::Mutex<Vec<(String, String, String, bool)>>,
}

impl ViewedGithub {
    fn at(head: &str) -> Arc<Self> {
        use crate::pr_viewed::{Mark, Marks, Viewed};
        Arc::new(Self {
            marks: Ok(Marks { node: "PR_node".into(), head: head.into(), files: vec![Mark { path: "src/a.rs".into(), viewed: Viewed::Unviewed }] }),
            set: Default::default(),
        })
    }
}

impl Github for ViewedGithub {
    fn search_issues<'a>(&'a self, _: &'a [String]) -> close::Answer<'a, Result<Value, String>> {
        Box::pin(async { Err("unexpected issue search".into()) })
    }
    fn issue_view<'a>(&'a self, _: &'a str, _: u64) -> close::Answer<'a, Result<Value, String>> {
        Box::pin(async { Err("unexpected issue detail".into()) })
    }
    fn close_pr<'a>(&'a self, _: &'a str, _: &'a str, _: u64) -> close::Answer<'a, Result<(), String>> {
        Box::pin(async { Err("unexpected PR close".into()) })
    }
    fn viewed_files<'a>(&'a self, _: &'a alondra_core::domain::PullRequest) -> close::Answer<'a, Result<crate::pr_viewed::Marks, String>> {
        Box::pin(async { self.marks.clone() })
    }
    fn set_viewed<'a>(&'a self, host: &'a str, node: &'a str, path: &'a str, viewed: bool) -> close::Answer<'a, Result<(), String>> {
        Box::pin(async move {
            self.set.lock().unwrap().push((host.into(), node.into(), path.into(), viewed));
            Ok(())
        })
    }
}

#[tokio::test]
async fn marking_a_file_viewed_sets_the_github_mark_of_the_head_the_reader_saw() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    let github = ViewedGithub::at(PR_HEAD);
    fixture.client.use_github(github.clone());
    let path = "/pull-requests/pr-1/viewed-files";
    let (status, body) = fixture.local_send(path, &json!({"path": "src/a.rs", "viewed": true, "head": PR_HEAD})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"pr_id": "pr-1", "head": PR_HEAD, "path": "src/a.rs", "viewed": "viewed"}));
    let (status, body) = fixture.local_send(path, &json!({"path": "src/a.rs", "viewed": false, "head": PR_HEAD})).await;
    assert_eq!((status, body["viewed"].as_str()), (StatusCode::OK, Some("unviewed")));
    assert_eq!(*github.set.lock().unwrap(), [
        ("github.com".to_owned(), "PR_node".to_owned(), "src/a.rs".to_owned(), true),
        ("github.com".to_owned(), "PR_node".to_owned(), "src/a.rs".to_owned(), false),
    ]);
    assert_eq!(parse_write(path), Some(Write::FileViewed("pr-1".into())));
    assert_eq!(parse_write("/pull-requests/github.com%2Fme%2Fapp%237/viewed-files"), Some(Write::FileViewed("github.com/me/app#7".into())));
    assert!(approved_write_path(path) && !atomic_write_path(path), "a mark goes through the relay as a write of its own and is never replayed");
}

#[tokio::test]
async fn a_file_is_never_marked_for_a_head_or_a_file_the_reader_did_not_see() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    let path = "/pull-requests/pr-1/viewed-files";
    let mark = |file: &str, head: &str| json!({"path": file, "viewed": true, "head": head});
    let github = ViewedGithub::at(PR_HEAD);
    fixture.client.use_github(github.clone());
    let refused = [
        (fixture.local_send(path, &mark("src/a.rs", "fedcba9876543210fedcba9876543210fedcba98")).await, StatusCode::CONFLICT, "stale_head"),
        (fixture.local_send(path, &mark("src/elsewhere.rs", PR_HEAD)).await, StatusCode::UNPROCESSABLE_ENTITY, "invalid_input"),
        (fixture.local_send(path, &json!({"path": "src/a.rs", "viewed": true, "head": PR_HEAD, "extra": 1})).await, StatusCode::UNPROCESSABLE_ENTITY, "invalid_input"),
        (fixture.local_send(path, &mark("", PR_HEAD)).await, StatusCode::UNPROCESSABLE_ENTITY, "invalid_input"),
        (fixture.local_send("/pull-requests/nothing/viewed-files", &mark("src/a.rs", PR_HEAD)).await, StatusCode::NOT_FOUND, "not_found"),
    ];
    for ((status, error), expected, code) in refused {
        assert_eq!((status, error["code"].as_str()), (expected, Some(code)), "{error}");
    }
    fixture.client.use_github(ViewedGithub::at("fedcba9876543210fedcba9876543210fedcba98"));
    let (status, error) = fixture.local_send(path, &mark("src/a.rs", PR_HEAD)).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::CONFLICT, Some("stale_head")), "GitHub's head moved past the registered one");
    fixture.command(Actor::System, "closed", Command::ObservePrState {
        pr_id: "pr-1".into(), head: PR_HEAD.into(), base: "main".into(),
        state: alondra_core::domain::PrState::Closed, merged_head: None });
    fixture.client.use_github(github.clone());
    let (status, error) = fixture.local_send(path, &mark("src/a.rs", PR_HEAD)).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::CONFLICT, Some("pr_not_open")));
    assert!(github.set.lock().unwrap().is_empty(), "nothing reached GitHub");
    let unsigned = reqwest::Client::new().post(format!("{}{path}", fixture.base)).json(&mark("src/a.rs", PR_HEAD)).send().await.unwrap();
    assert_eq!(unsigned.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_screen_builds_reads_and_sends_a_crash_report_through_the_macs_own_cli() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::start().await;
    let cli = fixture.home.join("fake-alondra");
    std::fs::write(&cli, "#!/bin/sh\ncase \"$*\" in *fails*) echo 'Error: GitHub said no' >&2; exit 1;; esac\n\
        printf '{\"args\":\"%s\",\"home\":\"%s\",\"lang\":\"%s\",\"stdin\":\"%s\"}' \"$*\" \"$ALONDRA_HOME\" \"$ALONDRA_LANG\" \"$(cat)\"\n").unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    fixture.client.use_report_cli(cli);
    let post = |path: &'static str, body: Value| {
        let request = fixture.signed("POST", path, Some(&body));
        async move {
            let response = request.send().await.unwrap();
            (response.status(), response.json::<Value>().await.unwrap())
        }
    };

    let (status, built) = post("/reports", json!({"description": "It stopped after the Mac slept", "lang": "es"})).await;
    assert_eq!(status, StatusCode::OK, "{built}");
    assert_eq!(built["args"], "--json --no-input report --description It stopped after the Mac slept");
    assert_eq!(built["home"], fixture.home.display().to_string());
    assert_eq!((built["lang"].as_str(), built["failure"].as_str()), (Some("es"), Some("other")), "a report from Settings goes as «other»");
    let (_, built) = post("/reports", json!({"failure": "session-ended", "description": "  "})).await;
    assert_eq!((built["args"].as_str(), built["failure"].as_str(), built["lang"].as_str()),
        (Some("--json --no-input report"), Some("session-ended"), Some("en")));
    assert_eq!(built["stdin"], "");
    let (_, built) = post("/reports", json!({"app_section": "the app section"})).await;
    assert_eq!((built["args"].as_str(), built["stdin"].as_str()),
        (Some("--json --no-input report --app-section -"), Some("the app section")), "the app's section goes to the CLI on stdin");
    let (status, refused) = post("/reports", json!({"failure": "crash"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");

    let (status, sent) = post("/reports/r-20261027-0000abcd/send", json!({"failure": "not-resumed"})).await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    assert_eq!(sent["args"], "--json --no-input report send r-20261027-0000abcd --kind not-resumed");
    let (_, sent) = post("/reports/r-20261027-0000abcd/send", json!({})).await;
    assert_eq!(sent["args"], "--json --no-input report send r-20261027-0000abcd --kind other");
    assert_eq!(parse_write("/reports/fails/send"), None, "only a report id is sent");
    std::fs::write(fixture.home.join("fake-alondra"), "#!/bin/sh\necho 'Error: GitHub said no' >&2\nexit 1\n").unwrap();
    let (status, failed) = post("/reports/r-20261027-0000abcd/send", json!({})).await;
    assert_ne!(status, StatusCode::OK);
    assert_eq!(failed["message"], "GitHub said no", "{failed}");

    let dir = fixture.home.join("reports/r-20261027-0000abcd");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("manifest.json"), json!({"sections": [{"name": "daemon.log", "bytes": 9, "sha256": "x"}]}).to_string()).unwrap();
    std::fs::write(dir.join("daemon.log"), "the log\n").unwrap();
    std::fs::write(dir.join("pseudonyms.json"), r#"{"tarea-1": "t-secret"}"#).unwrap();
    let (status, file) = fixture.get("/reports/r-20261027-0000abcd/files/daemon.log").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(serde_json::from_slice::<Value>(&file).unwrap()["text"], "the log\n");
    let (status, page) = fixture.get("/reports/r-20261027-0000abcd/files/daemon.log?offset=4").await;
    assert_eq!(status, StatusCode::OK);
    let page = serde_json::from_slice::<Value>(&page).unwrap();
    assert_eq!((page["text"].as_str(), page["next"].is_null(), page["bytes"].as_u64()), (Some("log\n"), true, Some(8)));
    assert_eq!(parse_read("/reports/r-20261027-0000abcd/files/daemon.log?offset=x"), None);
    for path in ["/reports/r-20261027-0000abcd/files/pseudonyms.json", "/reports/r-20261027-0000abcd/files/sending.json"] {
        let (status, body) = fixture.get(path).await;
        assert_ne!(status, StatusCode::OK, "{path}");
        assert!(!String::from_utf8_lossy(&body).contains("t-secret"), "{path}");
    }
    assert!(approved_read_path("/reports/r-20261027-0000abcd/files/daemon.log"), "the relay carries it too");
    for path in ["/reports/r-1/files/daemon.log", "/reports/r-20261027-0000abcd/files/..", "/reports/r-20261027-0000abcd/files/a/b"] {
        assert!(!approved_read_path(path), "{path}");
    }
}
