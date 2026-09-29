//! Each route is exercised the way the TUI calls it: a signed request with the body of
//! `fixtures/client-v2`, over the real router, against a real domain database.
use super::*;
use crate::auth::Auth;
use crate::domain_bootstrap::V2Only;
use alondra_core::domain_v2::{
    Actor, Command, CommandEnvelope, CommandOutcome, PROTOCOL_VERSION, Recipient, Scope,
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

struct Github(Value);

impl PrView for Github {
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
    fixture.user("close-2", Command::CloseRequest { request_id: "request-2".into() });
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
    fixture.client.stamp().unwrap();
    let github = Github(json!({"title": "Editor covers readable text", "state": "OPEN", "mergeable": "CONFLICTING",
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
    for key in expected["requests"]["request-2"].as_object().unwrap().keys() {
        assert!(runtime["requests"]["request-2"].get(key).is_some(), "requests.request-2.{key} is always present, null when unknown");
    }
    assert_eq!(runtime["requests"]["request-2"]["answer"], "Nothing to change: it already works");
    assert_eq!(runtime["requests"]["request-2"]["withdrawn"], false);
    assert_eq!(runtime["requests"]["request-1"]["answer"], Value::Null);
    assert_covers(&expected["pull_requests"]["pr-1"], &runtime["pull_requests"]["pr-1"], "pull_requests.pr-1");
    assert_eq!(runtime["pull_requests"]["pr-1"]["title"], "Editor covers readable text");
    assert_eq!(runtime["pull_requests"]["pr-1"]["conflicts"], true);
    assert_eq!(runtime["pull_requests"]["pr-1"]["checks"].as_array().unwrap().len(), 2);
    assert_eq!(runtime["pull_requests"]["pr-1"]["set_aside"], false);
    let _: alondra_core::pr::Check = serde_json::from_value(runtime["pull_requests"]["pr-1"]["checks"][0].clone()).unwrap();
    assert!(runtime["inbox_times"].as_object().unwrap().is_empty(), "work that predates the first look has no time");
}

#[tokio::test]
async fn times_are_kept_from_the_first_look_and_survive_a_restart() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    fixture.client.stamp().unwrap();
    let before = fixture.get_json("/v2/runtime").await;
    assert_eq!(before["tasks"]["task-1"]["state_since"], Value::Null, "a change from before the daemon kept times has no time");
    assert_eq!(before["requests"]["request-1"]["created_at"], Value::Null);

    fixture.user("request-3", Command::CreateRequest { request_id: "request-3".into(), body: "Later".into() });
    fixture.agent("agent-a", "ask", Command::PublishInformation {
        attachments: vec![], interaction_id: "info-1".into(),
        scope: Scope::Task { task_id: "task-1".into() }, recipient: Recipient::User, body: "FYI".into(),
    });
    fixture.user("cancel", Command::CancelTask { task_id: "task-1".into() });
    fixture.client.stamp().unwrap();
    let after = fixture.get_json("/v2/runtime").await;
    assert!(after["requests"]["request-3"]["created_at"].as_i64().unwrap() > 0);
    assert!(after["tasks"]["task-1"]["state_since"].as_i64().unwrap() > 0, "a change after the first look is timed");
    assert!(after["inbox_times"]["interaction:info-1"].as_i64().unwrap() > 0);

    let reopened = Client::open(fixture.domain.clone(), &fixture.home, Default::default());
    assert_eq!(reopened.sidecar.snapshot(), fixture.client.sidecar.snapshot(), "times are kept on disk");
    reopened.sidecar.set_aside("pr-1", true).unwrap();
    assert!(Client::open(fixture.domain.clone(), &fixture.home, Default::default()).sidecar.snapshot().set_aside.contains("pr-1"));
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

async fn refusal(fixture: &Fixture, path: &str, body: Value) -> (StatusCode, Value) {
    fixture.send("POST", path, &body).await
}

fn seed_information(fixture: &Fixture) {
    fixture.agent("agent-a", "info-1", Command::PublishInformation {
        attachments: vec![], interaction_id: "info-1".into(),
        scope: Scope::Task { task_id: "task-1".into() }, recipient: Recipient::User, body: "FYI".into(),
    });
}

fn outbox_for(fixture: &Fixture, agent: Option<&str>) -> Vec<crate::domain_store::OutboxMessage> {
    fixture.domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
        .filter(|message| message.agent_id.as_deref() == agent && message.recipient_kind != "user")
        .collect()
}

#[tokio::test]
async fn a_message_to_an_agent_is_one_command_with_its_read_mark_and_outbox_row() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    seed_information(&fixture);
    let expected = fixture_json("requests.json");
    let request = &expected["message"];
    let path = request["path"].as_str().unwrap();

    let (status, answer) = fixture.send("POST", path, &request["body"]).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_covers(&json!({"message_id": 0}), &answer, "message");
    assert!(answer.get("delivery_state").is_none(), "nothing has been handed over yet, so no state is claimed: {answer}");
    let queued = outbox_for(&fixture, Some("agent-a"));
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].id, answer["message_id"].as_i64().unwrap());
    assert!(queued[0].message.starts_with("The user requests changes on https://github.com/me/app/pull/7:\n\nRename it"), "{}", queued[0].message);
    assert!(queued[0].message.contains("- shot.png: files/domain-v2/x"), "{}", queued[0].message);
    assert_eq!(queued[0].task_id.as_deref(), Some("task-1"));

    let unread = fixture.domain.projection().unwrap().counts.unread_information;
    let (status, _) = fixture.send("POST", "/v2/agents/agent-a/messages",
        &json!({"text": "Thanks", "attachments": [], "kind": "reply", "mark_read": true, "interaction_id": "info-1"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fixture.domain.projection().unwrap().counts.unread_information, unread - 1, "the reply marked the information read in the same command");
    assert_eq!(outbox_for(&fixture, Some("agent-a")).len(), 2);

    let (status, _) = fixture.send("POST", "/v2/agents/agent-a/messages", &json!({"text": "Just so you know", "attachments": []})).await;
    assert_eq!(status, StatusCode::OK, "a plain message carries no kind");
    let (status, _) = fixture.send("POST", "/v2/agents/agent-a/messages",
        &json!({"text": "", "attachments": [], "kind": "more_context", "mark_read": false, "interaction_id": "info-1"})).await;
    assert_eq!(status, StatusCode::OK, "more context needs no words");
    assert_eq!(fixture.domain.state().unwrap().messages.len(), 4);

    let refused = |body: Value, path: &'static str| refusal(&fixture, path, body);
    let (status, error) = refused(json!({"text": "hi"}), "/v2/agents/nobody/messages").await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")));
    let (status, error) = refused(json!({"text": "hi", "kind": "pr_changes"}), "/v2/agents/agent-a/messages").await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_input")), "a PR message names its PR");
    let (status, error) = refused(json!({"text": "hi", "kind": "pr_changes", "pr_id": "pr-1"}), "/v2/agents/agent-b/messages").await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::FORBIDDEN, Some("not_assigned")), "another agent's PR is not its business");
    let (status, error) = refused(json!({"text": "hi", "unknown": 1}), "/v2/agents/agent-a/messages").await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_input")));
    let (status, _) = refused(json!({"text": "  "}), "/v2/agents/agent-a/messages").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(fixture.domain.state().unwrap().messages.len(), 4, "a refusal writes nothing");
    let unsigned = reqwest::Client::new().post(format!("{}/v2/agents/agent-a/messages", fixture.base)).json(&json!({"text": "hi"})).send().await.unwrap();
    assert_eq!(unsigned.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_message_to_the_orchestrator_is_plain_and_waits_in_its_outbox() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    let (status, answer) = fixture.send("POST", "/v2/orchestrator/messages", &json!({"text": "Plan the week", "attachments": []})).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let queued = outbox_for(&fixture, None);
    assert_eq!((queued.len(), queued[0].recipient_kind.as_str(), queued[0].message.as_str()), (1, "orchestrator", "Plan the week"));
    assert_eq!(queued[0].id, answer["message_id"].as_i64().unwrap());
    let (status, error) = fixture.send("POST", "/v2/orchestrator/messages", &json!({"text": "why", "kind": "more_context", "interaction_id": "info-1"})).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_input")));
}

#[tokio::test]
async fn a_message_sent_again_over_the_same_transport_request_is_the_same_message() {
    let fixture = Fixture::start().await;
    seed_work(&fixture);
    let body = json!({"text": "Once", "attachments": []});
    let digest = [7u8; 32];
    let send = || fixture.client.send_message("device:phone", Some("agent-a"), &body, Some(("phone", "request-1", &digest)), &|| Ok(())).unwrap();
    let first = send();
    let id = first.body["message_id"].as_i64().unwrap();
    {
        let mut store = fixture.domain.store.lock().unwrap();
        let claim = store.claim_message(id).unwrap().unwrap();
        assert!(store.finish_message(claim.attempt_id, claim.version, alondra_core::delivery::State::Handed, None).unwrap());
    }
    let again = send();
    assert_eq!(again.body["message_id"], first.body["message_id"]);
    assert_eq!(again.body["delivery_state"], "handed", "the retry learns what became of the first send");
    assert_eq!(fixture.domain.state().unwrap().messages.len(), 1, "a retry never writes a second message");
    let revoked = fixture.client.send_message("device:phone", Some("agent-a"), &body, Some(("phone", "request-2", &digest)), &|| Err(anyhow::anyhow!("revoked"))).unwrap();
    assert_eq!(revoked.status, StatusCode::FORBIDDEN, "a revoked device sends nothing");
    assert_eq!(fixture.domain.state().unwrap().messages.len(), 1);
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

struct FakeOrchestrator(std::sync::Mutex<Vec<(Step, String)>>);

impl OrchestratorControl for FakeOrchestrator {
    fn status(&self) -> Option<Value> {
        Some(json!({"phase": "failed", "can_retry": true, "can_cancel": true, "active_run_id": "run-7", "queue_depth": 2, "last_error": "The batch stopped halfway"}))
    }

    fn recover(&self, step: Step, run_id: &str) -> Result<(), DomainError> {
        if run_id != "run-7" {
            return Err(DomainError::new("conflict", "that run is not the active one"));
        }
        self.0.lock().unwrap().push((step, run_id.to_owned()));
        Ok(())
    }
}

#[tokio::test]
async fn the_orchestrator_recovery_steps_reach_its_runner_with_the_run_they_name() {
    let fixture = Fixture::start().await;
    let (status, error) = fixture.send("POST", "/v2/orchestrator/retry", &json!({"run_id": "run-7"})).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::CONFLICT, Some("orchestrator_unavailable")), "no runner, no recovery");
    assert!(fixture.get_json("/v2/runtime").await["orchestrator"].is_null());

    let runner = Arc::new(FakeOrchestrator(Default::default()));
    fixture.client.attach_orchestrator(runner.clone());
    let expected = fixture_json("requests.json");
    let retry = &expected["orchestrator_retry"];
    let (status, answer) = fixture.send("POST", retry["path"].as_str().unwrap(), &retry["body"]).await;
    assert_eq!((status, &answer), (StatusCode::OK, &retry["response"]));
    for step in ["cancel", "acknowledge"] {
        assert_eq!(fixture.send("POST", &format!("/v2/orchestrator/{step}"), &json!({"run_id": "run-7"})).await.0, StatusCode::OK);
    }
    assert_eq!(*runner.0.lock().unwrap(), vec![(Step::Retry, "run-7".to_string()), (Step::Cancel, "run-7".into()), (Step::Acknowledge, "run-7".into())]);
    let (status, error) = fixture.send("POST", "/v2/orchestrator/retry", &json!({"run_id": "run-1"})).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::CONFLICT, Some("conflict")));
    assert_eq!(fixture.send("POST", "/v2/orchestrator/retry", &json!({})).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(runner.0.lock().unwrap().len(), 3, "a refused call reaches nobody");
    let runtime = fixture.get_json("/v2/runtime").await;
    assert_covers(&expected_runtime()["orchestrator"], &runtime["orchestrator"], "orchestrator");
    assert_eq!(runtime["orchestrator"]["active_run_id"], "run-7");
}

fn expected_runtime() -> Value {
    fixture_json("runtime.json")
}

#[test]
fn writes_are_named_exactly() {
    for (path, write) in [
        ("/v2/agents/agent-a/messages", Write::AgentMessage("agent-a".into())),
        ("/v2/orchestrator/messages", Write::OrchestratorMessage),
        ("/v2/agents/agent-a/close", Write::Close("agent-a".into())),
        ("/v2/orchestrator/cancel", Write::Recover(Step::Cancel)),
        ("/v2/orchestrator/acknowledge", Write::Recover(Step::Acknowledge)),
        ("/v2/orchestrator/retry", Write::Recover(Step::Retry)),
    ] {
        assert_eq!(parse_write(path), Some(write), "{path}");
    }
    for path in ["/v2/orchestrator", "/v2/orchestrator/other", "/v2/orchestrator/messages/", "/v2/agents//messages",
        "/v2/agents/a%2Fb/close", "/v2/agents/a/messages?x=1", "/v2/agents/a/other", "/v2/messages"] {
        assert_eq!(parse_write(path), None, "{path}");
    }
    assert!(atomic_write_path("/v2/agents/a/messages") && atomic_write_path("/v2/orchestrator/messages"));
    assert!(!atomic_write_path("/v2/agents/a/close") && !atomic_write_path("/v2/orchestrator/retry"));
}
