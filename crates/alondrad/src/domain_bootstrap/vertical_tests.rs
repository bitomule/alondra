//! Local HTTP producer/read integration over a fresh SQL store. User decisions use
//! the runtime boundary because local HTTP user mutations require user presence.
use super::*;
use crate::auth::Auth;
use crate::domain_authority::Principal;
use axum::http::StatusCode;
use std::net::SocketAddr;
use alondra_core::domain::{Actor, Command, CommandEnvelope, DeliveryTarget, InteractionPayload, PrId, PROTOCOL_VERSION, TaskState};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

struct Cleanup(crate::test_support::Directory);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct ServerGuard(Option<tokio::task::JoinHandle<()>>);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        if let Some(server) = &self.0 {
            server.abort();
        }
    }
}

struct Fixture {
    home: PathBuf,
    base: String,
    server: ServerGuard,
    client: reqwest::Client,
    domain: Arc<DomainRuntime>,
    key: SigningKey,
    device_id: String,
    orchestrator: String,
    agent: String,
    next_id: u64,
    pr_view: Arc<crate::client_routes::FakePrView>,
    work: Arc<crate::client_routes::Client>,
    // Dropped after the runtime, including when an assertion panics.
    _cleanup: Cleanup,
}

impl Fixture {
    async fn start() -> Self {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        let store = DomainRuntime::open(&home).unwrap();
        let epoch = store.state().unwrap().data_epoch.0.clone();
        drop(store);
        let auth = Auth::open(&home).unwrap();
        let key = SigningKey::from_slice(&[13u8; 32]).unwrap();
        let pairing = auth.new_pairing_code();
        let device = auth
            .pair(
                &pairing,
                "vertical-fixture",
                &STANDARD.encode(key.verifying_key().to_sec1_bytes()),
            )
            .unwrap();
        let state = DomainDaemon::open(&home).unwrap();
        let domain = state.domain();
        let pr_view = Arc::new(crate::client_routes::FakePrView::default());
        state.client().use_pr_view(pr_view.clone());
        let work = state.client();
        let registration = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: epoch.clone().into(),
            command_id: "register-vertical-agent".into(),
            expected_revision: domain.state().unwrap().revision,
            command: Command::RegisterAgent {
                agent_id: "agent-a".into(),
                name: "Fixture agent".into(), at: 0
            },
        };
        domain
            .execute(&Actor::System, &registration, None, || Ok(()))
            .unwrap();
        let orchestrator = domain
            .issue(Principal::Orchestrator {
                generation: 0,
                execution_id: "vertical-fixture".into(),
            })
            .unwrap()
            .expose_for_child_environment()
            .to_owned();
        let agent = domain
            .issue(Principal::Agent {
                session_id: "agent-a".into(),
                task_binding: None,
                provider_agent_id: None,
            })
            .unwrap()
            .expose_for_child_environment()
            .to_owned();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(state).layer(axum::middleware::from_fn_with_state(
            Arc::new(Auth::open(&home).unwrap()),
            crate::auth::guard,
        ));
        let server = ServerGuard(Some(tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        })));
        Self {
            home: home.to_path_buf(),
            base,
            server,
            client: reqwest::Client::new(),
            domain,
            key,
            device_id: device.id,
            orchestrator,
            agent,
            next_id: 0,
            pr_view,
            work,
            _cleanup: Cleanup(home),
        }
    }

    fn envelope(&mut self, mut command: Value) -> Value {
        if let Some(command) = command.as_object_mut() { alondra_core::domain::derive_wire_card(command); }
        self.next_id += 1;
        let state = self.domain.state().unwrap();
        json!({"protocol_version":PROTOCOL_VERSION,"data_epoch":state.data_epoch,
            "expected_revision":state.revision,
            "command_id":format!("vertical-{}",self.next_id),"command":command})
    }

    /// The task waits in Closing for its node; Alondra records it closed and the task is done.
    fn end_node(&mut self, task: &str) {
        assert_eq!(self.domain.state().unwrap().tasks[&task.into()].state, TaskState::Closing);
        let envelope: CommandEnvelope = serde_json::from_value(self.envelope(json!({"kind":"record_node_end", "task_id":task, "end":"closed"}))).unwrap();
        self.domain.execute(&Actor::System, &envelope, None, || Ok(())).unwrap();
    }

    async fn post(&self, bearer: &str, envelope: &Value) -> (StatusCode, Value) {
        let command = envelope["command"].as_object().unwrap();
        let kind = command["kind"].as_str().unwrap();
        let task = command.get("task_id").and_then(Value::as_str);
        let interaction = command.get("interaction_id").or_else(|| command.get("presentation_id")).and_then(Value::as_str);
        let (method, path) = match kind {
            "create_request" => ("POST", "/requests".to_owned()),
            "create_task" => ("POST", format!("/requests/{}/tasks", command["request_id"].as_str().unwrap())),
            "title_request" => ("POST", format!("/requests/{}/title", command["request_id"].as_str().unwrap())),
            "register_pr" => ("POST", format!("/tasks/{}/pull-requests", task.unwrap())),
            "present_result" => ("POST", format!("/tasks/{}/results", task.unwrap())),
            "complete_task" => ("POST", format!("/tasks/{}/completion", task.unwrap())),
            "ask_question" => ("POST", "/questions".to_owned()),
            "answer_question" => ("POST", format!("/interactions/{}/answers", interaction.unwrap())),
            "accept_delivery" => ("POST", format!("/interactions/{}/acceptance", interaction.unwrap())),
            "create_document" => ("POST", format!("/tasks/{}/documents", task.unwrap())),
            "decide_document_review" => ("PUT", format!("/tasks/{}/documents/{}/review", task.unwrap(), command["document_id"].as_str().unwrap())),
            _ => panic!("vertical test command has no resource route: {kind}"),
        };
        let mut body = serde_json::Map::new();
        for key in ["data_epoch", "command_id", "expected_revision"] {
            body.insert(key.to_owned(), envelope[key].clone());
        }
        for (key, value) in command {
            if key != "kind" && !(key == "request_id" && path != "/requests")
                && !(key == "task_id" && path.starts_with("/tasks/")) {
                body.insert(key.clone(), value.clone());
            }
        }
        if kind == "create_request" && !body.contains_key("auto_merge") { body.insert("auto_merge".into(), json!(false)); }
        let response = self.client.request(method.parse().unwrap(), format!("{}{}", self.base, path))
            .bearer_auth(bearer).json(&Value::Object(body)).send().await.unwrap();
        let status = response.status();
        (status, response.json().await.unwrap())
    }

    async fn producer(&mut self, orchestrator: bool, command: Value) -> Value {
        let envelope = self.envelope(command);
        let bearer = if orchestrator {
            &self.orchestrator
        } else {
            &self.agent
        };
        let (status, body) = self.post(bearer, &envelope).await;
        assert_eq!(status, StatusCode::OK, "command={} body={body}", envelope["command"]["kind"]);
        body
    }

    fn user(&mut self, command: Value) {
        let envelope: CommandEnvelope = serde_json::from_value(self.envelope(command)).unwrap();
        self.domain
            .execute(
                &Actor::User {
                    user_id: "fixture-user".into(),
                },
                &envelope,
                None,
                || Ok(()),
            )
            .unwrap();
    }

    async fn get_signed(&self, path: &str) -> (StatusCode, Value) {
        let timestamp = alondra_core::now_millis().to_string();
        let message = crate::auth::signed_message("GET", path, &timestamp, b"");
        let signature: Signature = self.key.sign(message.as_bytes());
        let response = self
            .client
            .get(format!("{}{}", self.base, path))
            .header("x-alondra-device", &self.device_id)
            .header("x-alondra-timestamp", timestamp)
            .header(
                "x-alondra-signature",
                STANDARD.encode(signature.to_der().as_bytes()),
            )
            .send()
            .await
            .unwrap();
        let status = response.status();
        (status, response.json().await.unwrap())
    }

    async fn task(&mut self, id: &str, review_prs: bool) {
        self.producer(
            true,
            json!({"kind":"create_request", "request_id":format!("request-{id}"),
            "body":format!("Work on {id}")}),
        )
        .await;
        let cwd = self.home.to_str().unwrap().to_owned();
        self.producer(true, json!({"kind":"create_task", "task_id":id,
            "request_id":format!("request-{id}"), "title":format!("Task {id}"),
            "agent_id":null, "review_prs":review_prs,
            "dependencies":[], "stack":null, "launch":{"name":format!("app/{id}"), "cwd":cwd, "model":"sonnet", "reason":"clear change"}})).await;
        self.user(json!({"kind":"assign_task", "task_id":id, "agent_id":"agent-a"}));
    }

    async fn finish(mut self) {
        let server = self.server.0.take().unwrap();
        server.abort();
        let _ = server.await;
        drop(self.domain);
    }
}

#[tokio::test]
async fn http_producer_answer_and_document_are_persisted() {
    let mut fixture = Fixture::start().await;
    fixture.task("answer-task", false).await;
    fixture
        .producer(
            false,
            json!({"kind":"ask_question", "interaction_id":"question-1",
        "scope":{"kind":"task", "task_id":"answer-task"}, "recipient":{"kind":"user"},
        "body":"Which answer?", "blocking":true,
        "options":[{"id":"yes", "label":"Yes"}]}),
        )
        .await;
    assert!(fixture.domain.state().unwrap().interactions.contains_key(&"question-1".into()));
    fixture.user(
        json!({"kind":"answer_question", "interaction_id":"question-1",
        "option_id":"yes", "inputs":{}}),
    );
    let answered_state = fixture.domain.state().unwrap();
    let answered = &answered_state.interactions[&"question-1".into()];
    assert_eq!(serde_json::to_value(&answered.payload).unwrap()["answer"]["option_id"], "yes");
    let text = json!({"kind":"text", "text":"The requested answer"});
    fixture
        .producer(
            false,
            json!({"kind":"present_result", "task_id":"answer-task",
        "interaction_id":"answer-delivery", "body":"The requested answer",
        "targets":[text.clone()], "is_final":true}),
        )
        .await;
    fixture.user(
        json!({"kind":"accept_delivery", "presentation_id":"answer-delivery",
        "targets":[text]}),
    );
    fixture.end_node("answer-task");
    assert_eq!(
        fixture.domain.state().unwrap().tasks[&"answer-task".into()].state,
        TaskState::Done
    );

    fixture.task("doc-task", false).await;
    let source = fixture.home.join("files/source");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    let content = b"A synthetic design document";
    std::fs::write(&source, content).unwrap();
    let digest: String = Sha256::digest(content)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    fixture
        .producer(
            false,
            json!({"kind":"create_document", "task_id":"doc-task",
        "document_id":"doc-1", "title":"Design", "revision_id":"doc-v1",
        "content_digest":digest, "content_reference":"files/source", "role":"result"}),
        )
        .await;
    let (status, chunk) = fixture
        .get_signed("/document-revisions/doc-v1/content?offset=0")
        .await;
    assert_eq!(status, StatusCode::OK, "{chunk}");
    assert_eq!(chunk["revision_id"], "doc-v1");
    assert_eq!(chunk["content_digest"], digest);
    assert_eq!(
        URL_SAFE_NO_PAD
            .decode(chunk["data"].as_str().unwrap())
            .unwrap(),
        content
    );
    let document = json!({"kind":"document_revision", "document_id":"doc-1",
        "revision_id":"doc-v1"});
    fixture
        .producer(
            false,
            json!({"kind":"present_result", "task_id":"doc-task",
        "interaction_id":"doc-delivery", "body":"Document ready",
        "targets":[document], "is_final":true}),
        )
        .await;
    fixture.user(
        json!({"kind":"decide_document_review", "task_id":"doc-task",
        "document_id":"doc-1", "revision_id":"doc-v1", "accepted":true}),
    );
    fixture.end_node("doc-task");
    assert_eq!(fixture.domain.state().unwrap().tasks[&"doc-task".into()].state, TaskState::Done);
    let reopened = DomainRuntime::open(&fixture.home).unwrap();
    let persisted = reopened.state().unwrap();
    assert_eq!(
        persisted.tasks[&"answer-task".into()].state,
        TaskState::Done
    );
    assert_eq!(persisted.tasks[&"doc-task".into()].state, TaskState::Done);
    assert!(persisted.document_revisions.contains_key(&"doc-v1".into()));
    drop(reopened);
    fixture.finish().await;
}

#[tokio::test]
async fn http_pr_flow_replays_same_receipt_and_rejects_old_epoch() {
    let mut fixture = Fixture::start().await;
    fixture.task("pr-task", true).await;
    let envelope = fixture.envelope(json!({"kind":"register_pr", "task_id":"pr-task",
        "pr_id":"pr-1", "host":"github.com", "repository":"example/project",
        "number":7, "head":"0123456789abcdef0123456789abcdef01234567", "base":"main",
        "allowed_methods":["squash"]}));
    let (status, first) = fixture.post(&fixture.agent, &envelope).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let revision = first["revision"].clone();
    let (status, mut replay) = fixture.post(&fixture.agent, &envelope).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["replayed"], json!(true), "the same command is answered as already recorded: {replay}");
    assert!(first.get("replayed").is_none(), "{first}");
    replay.as_object_mut().unwrap().remove("replayed");
    assert_eq!(replay, first, "with the outcome and effect it had the first time");
    assert_eq!(
        fixture.domain.state().unwrap().revision,
        revision.as_u64().unwrap()
    );
    let mut stale = fixture.envelope(json!({"kind":"present_result", "task_id":"pr-task",
        "interaction_id":"pr-delivery", "body":"Code ready", "is_final":true,
        "targets":[{"kind":"pull_request", "pr_id":"pr-1", "head":"0123456789abcdef0123456789abcdef01234567"}]}));
    stale["data_epoch"] = json!("old-epoch");
    let (status, rejected) = fixture.post(&fixture.agent, &stale).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{rejected}");
    assert_eq!(rejected["code"], "forbidden");
    assert_eq!(
        fixture.domain.state().unwrap().revision,
        revision.as_u64().unwrap()
    );
    fixture
        .producer(
            false,
            json!({"kind":"present_result", "task_id":"pr-task",
        "interaction_id":"pr-delivery", "body":"Code ready", "is_final":true,
        "targets":[{"kind":"pull_request", "pr_id":"pr-1", "head":"0123456789abcdef0123456789abcdef01234567"}]}),
        )
        .await;
    let state = fixture.domain.state().unwrap();
    assert_eq!(state.pull_requests.len(), 1);
    assert_eq!(state.pull_requests[&"github.com/example/project#7".into()].task_id.0, "pr-task");
    assert_eq!(state.pull_requests[&"github.com/example/project#7".into()].head, "0123456789abcdef0123456789abcdef01234567");
    assert!(state.interactions.contains_key(&"pr-delivery".into()));
    assert_ne!(
        fixture.domain.state().unwrap().tasks[&"pr-task".into()].state,
        TaskState::Done
    );
    let reopened = DomainRuntime::open(&fixture.home).unwrap();
    let persisted = reopened.state().unwrap();
    assert_eq!(persisted.pull_requests[&"github.com/example/project#7".into()].task_id.0, "pr-task");
    assert!(persisted.interactions.contains_key(&"pr-delivery".into()));
    drop(reopened);
    fixture.finish().await;
}

fn user_pushes(domain: &DomainRuntime) -> Vec<Option<String>> {
    domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
        .filter(|message| message.recipient_kind == "user")
        .map(|message| message.interaction_id)
        .collect()
}

#[tokio::test]
async fn http_presenting_a_conflicting_pr_is_refused_before_any_card_or_push() {
    let mut fixture = Fixture::start().await;
    fixture.task("pr-task", true).await;
    fixture.producer(false, json!({"kind":"register_pr", "task_id":"pr-task",
        "pr_id":"pr-1", "host":"github.com", "repository":"example/project",
        "number":7, "head":"0123456789abcdef0123456789abcdef01234567", "base":"main", "allowed_methods":["squash"]})).await;
    let pushes_before = user_pushes(&fixture.domain);
    fixture.pr_view.answer(Some(json!({"state":"OPEN", "headRefOid":"0123456789abcdef0123456789abcdef01234567", "baseRefName":"main",
        "mergeable":"CONFLICTING", "mergeStateStatus":"DIRTY"})));
    let present = json!({"kind":"present_result", "task_id":"pr-task",
        "interaction_id":"pr-delivery", "body":"Code ready", "is_final":true,
        "targets":[{"kind":"pull_request", "pr_id":"pr-1", "head":"0123456789abcdef0123456789abcdef01234567"}]});
    let envelope = fixture.envelope(present.clone());
    let (status, refused) = fixture.post(&fixture.agent, &envelope).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["code"], "pr_conflicts");
    assert!(refused["message"].as_str().unwrap().contains("git push --force-with-lease"), "{refused}");
    assert!(!fixture.domain.state().unwrap().interactions.contains_key(&"pr-delivery".into()), "no card");
    assert_eq!(user_pushes(&fixture.domain), pushes_before, "no push");

    fixture.pr_view.answer(Some(json!({"state":"OPEN", "headRefOid":"0123456789abcdef0123456789abcdef01234567", "baseRefName":"main",
        "mergeable":"MERGEABLE", "mergeStateStatus":"CLEAN"})));
    fixture.producer(false, present).await;
    let state = fixture.domain.state().unwrap();
    assert!(state.interactions.contains_key(&"pr-delivery".into()));
    assert!(user_pushes(&fixture.domain).contains(&Some("pr-delivery".into())), "the push follows the card");
    fixture.finish().await;
}

const PUSHED: &str = "1111111111111111111111111111111111111111";
const ELSEWHERE: &str = "2222222222222222222222222222222222222222";
const REGISTERED: &str = "0123456789abcdef0123456789abcdef01234567";

impl Fixture {
    async fn registered_pr(&mut self) {
        self.task("pr-task", true).await;
        self.producer(false, json!({"kind":"register_pr", "task_id":"pr-task",
            "pr_id":"pr-1", "host":"github.com", "repository":"example/project",
            "number":7, "head":REGISTERED, "base":"main", "allowed_methods":["squash"]})).await;
    }

    fn pr_id(&self) -> PrId {
        self.domain.state().unwrap().tasks[&"pr-task".into()].pr_id.clone().unwrap()
    }

    /// Sends a presentation as the MCP server does, with no revision of its own: Alondra checks it
    /// against the state it commits on.
    async fn present_as_mcp(&mut self, target: Value) -> (StatusCode, Value) {
        let mut envelope = self.envelope(present_pr(target));
        envelope["expected_revision"] = json!(0);
        self.post(&self.agent, &envelope).await
    }

    fn presented_head(&self) -> String {
        let state = self.domain.state().unwrap();
        let InteractionPayload::Delivery { targets, .. } = &state.interactions[&"pr-delivery".into()].payload else { panic!("a delivery") };
        let [DeliveryTarget::PullRequest { head, .. }] = targets.as_slice() else { panic!("one PR target: {targets:?}") };
        head.clone()
    }
}

fn present_pr(target: Value) -> Value {
    json!({"kind":"present_result", "task_id":"pr-task", "interaction_id":"pr-delivery",
        "body":"Code ready", "is_final":true, "targets":[target]})
}

fn github_at(head: &str) -> Value {
    json!({"state":"OPEN", "headRefOid":head, "baseRefName":"main", "mergeable":"MERGEABLE", "mergeStateStatus":"CLEAN"})
}

/// GitHub as the PR watcher sees it: while the presentation waits on GitHub, the watcher records
/// the commit the node just pushed.
struct WatcherRecordsPush(Arc<DomainRuntime>, PrId);

impl crate::client_routes::PrView for WatcherRecordsPush {
    fn view<'a>(&'a self, _: &'a str) -> crate::client_routes::Viewed<'a> {
        if self.0.state().unwrap().pull_requests[&self.1].head != PUSHED {
            crate::domain_github::execute_system(&self.0, Command::ObservePrState {
                pr_id: self.1.clone(), head: PUSHED.into(), base: "main".into(),
                state: alondra_core::domain::PrState::Open, merged_head: None,
            }).unwrap();
        }
        Box::pin(async { None })
    }
}

#[tokio::test]
async fn http_a_pr_presented_without_a_head_while_the_watcher_records_a_push_shows_the_pushed_commit() {
    let mut fixture = Fixture::start().await;
    fixture.registered_pr().await;
    fixture.work.use_pr_view(Arc::new(WatcherRecordsPush(fixture.domain.clone(), fixture.pr_id())));
    let (status, body) = fixture.present_as_mcp(json!({"kind":"pull_request", "pr_id":"pr-1"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(fixture.presented_head(), PUSHED);
    fixture.finish().await;
}

#[tokio::test]
async fn http_a_head_pushed_before_alondra_saw_it_is_presented_once_github_reports_it() {
    let mut fixture = Fixture::start().await;
    fixture.registered_pr().await;
    fixture.pr_view.answer(Some(github_at(PUSHED)));
    let (status, body) = fixture.present_as_mcp(json!({"kind":"pull_request", "pr_id":"pr-1", "head":PUSHED})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(fixture.presented_head(), PUSHED);
    assert_eq!(fixture.domain.state().unwrap().pull_requests[&fixture.pr_id()].head, PUSHED);
    fixture.finish().await;
}

#[tokio::test]
async fn http_a_head_the_pr_is_not_at_on_github_is_refused_naming_the_commit_github_reports() {
    let mut fixture = Fixture::start().await;
    fixture.registered_pr().await;
    fixture.pr_view.answer(Some(github_at(ELSEWHERE)));
    let (status, refused) = fixture.present_as_mcp(json!({"kind":"pull_request", "pr_id":"pr-1", "head":PUSHED})).await;
    assert_ne!(status, StatusCode::OK, "{refused}");
    assert_eq!(refused["code"], "pr_mismatch");
    assert!(refused["message"].as_str().unwrap().contains(&format!("PR #7 is at {ELSEWHERE}, not {PUSHED}")), "{refused}");
    assert!(!fixture.domain.state().unwrap().interactions.contains_key(&"pr-delivery".into()), "no card");
    fixture.finish().await;
}

#[tokio::test]
async fn http_presenting_a_pr_github_cannot_report_still_creates_the_card() {
    let mut fixture = Fixture::start().await;
    fixture.task("pr-task", true).await;
    fixture.producer(false, json!({"kind":"register_pr", "task_id":"pr-task",
        "pr_id":"pr-1", "host":"github.com", "repository":"example/project",
        "number":7, "head":"0123456789abcdef0123456789abcdef01234567", "base":"main", "allowed_methods":["squash"]})).await;
    fixture.producer(false, json!({"kind":"present_result", "task_id":"pr-task",
        "interaction_id":"pr-delivery", "body":"Code ready", "is_final":true,
        "targets":[{"kind":"pull_request", "pr_id":"pr-1", "head":"0123456789abcdef0123456789abcdef01234567"}]})).await;
    assert!(fixture.domain.state().unwrap().interactions.contains_key(&"pr-delivery".into()));
    fixture.finish().await;
}

#[tokio::test]
async fn http_an_accepted_pr_is_merged_by_alondra_and_a_node_can_neither_claim_nor_complete() {
    use crate::domain_merges::{Checks, FakeGithub, Merger};
    use alondra_core::domain::MergeAttemptState;
    use std::time::Duration;
    let mut fixture = Fixture::start().await;
    fixture.task("merge-task", true).await;
    let head = "0123456789abcdef0123456789abcdef01234567";
    fixture.producer(false, json!({"kind":"register_pr", "task_id":"merge-task", "pr_id":"pr-7",
        "host":"github.com", "repository":"example/project", "number":7, "head":head, "base":"main",
        "allowed_methods":["squash"]})).await;
    fixture.producer(false, json!({"kind":"present_result", "task_id":"merge-task",
        "interaction_id":"delivery-7", "body":"Code ready", "is_final":true,
        "targets":[{"kind":"pull_request", "pr_id":"pr-7", "head":head}]})).await;
    let claim = fixture.client.post(format!("{}/merge-authorizations/claim", fixture.base)).bearer_auth(&fixture.agent)
        .json(&json!({"remote_head":head, "remote_base":"main", "base_verified":true})).send().await.unwrap();
    assert!(claim.status().is_client_error(), "nodes have no merge claim any more: {}", claim.status());
    let completion = fixture.envelope(json!({"kind":"complete_task", "task_id":"merge-task", "presentation_id":"delivery-7"}));
    let (status, body) = fixture.post(&fixture.agent, &completion).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::FORBIDDEN, Some("forbidden")), "{body}");
    fixture.user(json!({"kind":"accept_delivery", "presentation_id":"delivery-7",
        "targets":[{"kind":"pull_request", "pr_id":"github.com/example/project#7", "head":head}]}));
    let github = FakeGithub::new();
    github.open(7, head, Checks::Passed);
    Merger::with(Duration::ZERO, Duration::from_secs(60)).merge_due(&fixture.domain, &github).await.unwrap();
    assert_eq!(github.merged.lock().unwrap().len(), 1);
    let state = fixture.domain.state().unwrap();
    assert_eq!(state.merge_authorizations.values().next().unwrap().attempt, MergeAttemptState::Confirmed);
    assert_eq!(state.pull_requests[&"github.com/example/project#7".into()].merged_head.as_deref(), Some(head));
    fixture.end_node("merge-task");
    assert_eq!(fixture.domain.state().unwrap().tasks[&"merge-task".into()].state, TaskState::Done);
    fixture.finish().await;
}

#[tokio::test]
async fn http_the_orchestrator_creates_every_task_with_its_plan() {
    let mut fixture = Fixture::start().await;
    let epoch = fixture.domain.state().unwrap().data_epoch.clone();
    let register = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: epoch, command_id: "register-b".into(),
        expected_revision: fixture.domain.state().unwrap().revision,
        command: Command::RegisterAgent { agent_id: "agent-b".into(), name: "Fresh node".into(), at: 0 },
    };
    fixture.domain.execute(&Actor::System, &register, None, || Ok(())).unwrap();
    fixture.producer(true, json!({"kind":"create_request", "request_id":"request-plan", "body":"Work in two steps"})).await;
    fixture.producer(true, json!({"kind":"title_request", "request_id":"request-plan", "title":"Two steps"})).await;
    let task = |id: &str, agent: Value, dependencies: Value, launch: Value| {
        let mut command = json!({"kind":"create_task", "task_id":id, "request_id":"request-plan", "title":id,
            "agent_id":agent, "review_prs":false, "dependencies":dependencies, "stack":null});
        if !launch.is_null() { command["launch"] = launch; }
        command
    };
    let cwd = fixture.home.to_str().unwrap().to_owned();
    let plan = |name: &str| json!({"name": name, "cwd": cwd, "model": "sonnet", "reason": "clear change"});
    let envelope = fixture.envelope(task("first", json!("agent-b"), json!([]), Value::Null));
    let orchestrator = fixture.orchestrator.clone();
    let (status, body) = fixture.post(&orchestrator, &envelope).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("launch_plan_required")), "{body}");
    assert!(fixture.domain.state().unwrap().tasks.get(&"first".into()).is_none(), "even a task for a fresh node needs its plan");
    fixture.producer(true, task("first", json!("agent-b"), json!([]), plan("app/first"))).await;

    let envelope = fixture.envelope(task("second", json!("agent-b"), json!([]), plan("app/second")));
    let (status, body) = fixture.post(&orchestrator, &envelope).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("agent_busy")), "{body}");

    let envelope = fixture.envelope(task("later", Value::Null, json!(["first"]), Value::Null));
    let (status, body) = fixture.post(&orchestrator, &envelope).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("launch_plan_required")), "{body}");
    assert!(fixture.domain.state().unwrap().tasks.get(&"later".into()).is_none(), "nothing was created");

    fixture.producer(true, task("later", Value::Null, json!(["first"]), plan("app/later"))).await;
    let stored = fixture.domain.launch_plan(&"later".into()).unwrap().unwrap();
    assert_eq!((stored.name.as_str(), stored.model.as_deref()), ("app/later", Some("sonnet")));
}

#[tokio::test]
async fn http_a_repository_with_capitals_registers_one_lowercase_pr_whichever_spelling_comes() {
    let mut fixture = Fixture::start().await;
    let head = "0123456789abcdef0123456789abcdef01234567";
    fixture.task("parcel-task", false).await;
    let register = |repository: &str| json!({"kind":"register_pr", "task_id":"parcel-task", "pr_id":"pr-65",
        "host":"github.com", "repository":repository, "number":65, "head":head, "base":"main", "allowed_methods":["squash"]});
    fixture.producer(false, register("acmecorp-private/Parcel")).await;
    let again = fixture.envelope(register("acmecorp-private/parcel"));
    let (status, refused) = fixture.post(&fixture.agent, &again).await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("no_effect")), "the other spelling is the PR already registered: {refused}");

    let state = fixture.domain.state().unwrap();
    let parcel = PrId::from("github.com/acmecorp-private/parcel#65");
    assert_eq!(state.pull_requests.keys().collect::<Vec<_>>(), vec![&parcel]);
    assert_eq!(state.pull_requests[&parcel].repository, "acmecorp-private/parcel");
    assert_eq!(state.tasks[&"parcel-task".into()].pr_id.as_ref(), Some(&parcel));
    fixture.finish().await;
}

#[tokio::test]
async fn http_a_pr_number_another_repository_already_used_is_registered_and_presented_as_its_own() {
    let mut fixture = Fixture::start().await;
    let head = "0123456789abcdef0123456789abcdef01234567";
    fixture.task("lumo-task", false).await;
    let skills: CommandEnvelope = serde_json::from_value(fixture.envelope(json!({"kind":"register_agent",
        "agent_id":"agent-b", "name":"Skills agent", "at":0}))).unwrap();
    fixture.domain.execute(&Actor::System, &skills, None, || Ok(())).unwrap();
    fixture.producer(true, json!({"kind":"create_request", "request_id":"request-skills", "body":"Work on skills"})).await;
    let cwd = fixture.home.to_str().unwrap().to_owned();
    fixture.producer(true, json!({"kind":"create_task", "task_id":"skills-task", "request_id":"request-skills",
        "title":"Task skills", "agent_id":null, "review_prs":false, "dependencies":[], "stack":null,
        "launch":{"name":"app/skills", "cwd":cwd, "model":"sonnet", "reason":"clear change"}})).await;
    fixture.user(json!({"kind":"assign_task", "task_id":"skills-task", "agent_id":"agent-b"}));
    let legacy: CommandEnvelope = serde_json::from_value(fixture.envelope(json!({"kind":"register_pr",
        "task_id":"skills-task", "pr_id":"pr-163", "host":"github.com", "repository":"acme/skills",
        "number":163, "head":head, "base":"main", "allowed_methods":["squash"]}))).unwrap();
    fixture.domain.execute(&Actor::Agent { agent_id: "agent-b".into() }, &legacy, None, || Ok(())).unwrap();

    fixture.producer(false, json!({"kind":"register_pr", "task_id":"lumo-task", "pr_id":"pr-163",
        "host":"github.com", "repository":"acme/lumo", "number":163, "head":head, "base":"main",
        "allowed_methods":["squash"]})).await;
    fixture.producer(false, json!({"kind":"present_result", "task_id":"lumo-task",
        "interaction_id":"lumo-delivery", "body":"Code ready", "is_final":true,
        "targets":[{"kind":"pull_request", "pr_id":"pr-163"}]})).await;

    let state = fixture.domain.state().unwrap();
    let lumo = PrId::from("github.com/acme/lumo#163");
    assert_eq!(state.pull_requests[&lumo].task_id.0, "lumo-task");
    assert_eq!(state.pull_requests[&PrId::from("pr-163")].repository, "acme/skills");
    assert_eq!(state.tasks[&"lumo-task".into()].pr_id.as_ref(), Some(&lumo));
    let InteractionPayload::Delivery { targets, .. } = &state.interactions[&"lumo-delivery".into()].payload else {
        panic!("the result is a delivery");
    };
    assert_eq!(targets, &vec![DeliveryTarget::PullRequest { pr_id: lumo, head: head.into() }],
        "the node's pr-163 is its own Lumo PR, not skills' #163");
    fixture.finish().await;
}
