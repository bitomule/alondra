//! Local HTTP producer/read integration over a fresh SQL store. User decisions use
//! the runtime boundary because local HTTP user mutations require user presence.
use super::*;
use crate::auth::Auth;
use crate::domain_authority::Principal;
use axum::http::StatusCode;
use std::net::SocketAddr;
use alondra_core::domain_v2::{Actor, Command, CommandEnvelope, PROTOCOL_VERSION, TaskState};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

struct Cleanup(PathBuf);
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
    github: Arc<crate::domain_github::FakeObserver>,
    // Dropped after the runtime, including when an assertion panics.
    _cleanup: Cleanup,
}

impl Fixture {
    async fn start() -> Self {
        let home = std::env::temp_dir().join(format!(
            "alondra-v2-vertical-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir(&home).unwrap();
        let store = DomainRuntime::open(&home).unwrap();
        let epoch = store.state().unwrap().data_epoch.0;
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
        let github = Arc::new(crate::domain_github::FakeObserver::new());
        let state = V2Only::open_with(&home, Box::new(github.clone())).unwrap();
        let domain = state.domain();
        let registration = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: epoch.clone().into(),
            command_id: "register-vertical-agent".into(),
            expected_revision: domain.state().unwrap().revision,
            command: Command::RegisterAgent {
                agent_id: "agent-a".into(),
                name: "Fixture agent".into(),
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
            home: home.clone(),
            base,
            server,
            client: reqwest::Client::new(),
            domain,
            key,
            device_id: device.id,
            orchestrator,
            agent,
            next_id: 0,
            github,
            _cleanup: Cleanup(home),
        }
    }

    fn envelope(&mut self, command: Value) -> Value {
        self.next_id += 1;
        let state = self.domain.state().unwrap();
        json!({"protocol_version":PROTOCOL_VERSION,"data_epoch":state.data_epoch,
            "expected_revision":state.revision,
            "command_id":format!("vertical-{}",self.next_id),"command":command})
    }

    async fn post(&self, bearer: &str, envelope: &Value) -> (StatusCode, Value) {
        let command = envelope["command"].as_object().unwrap();
        let kind = command["kind"].as_str().unwrap();
        let task = command.get("task_id").and_then(Value::as_str);
        let interaction = command.get("interaction_id").or_else(|| command.get("presentation_id")).and_then(Value::as_str);
        let (method, path) = match kind {
            "create_request" => ("POST", "/requests".to_owned()),
            "create_task" => ("POST", format!("/requests/{}/tasks", command["request_id"].as_str().unwrap())),
            "register_pr" => ("POST", format!("/tasks/{}/pull-requests", task.unwrap())),
            "present_result" => ("POST", format!("/tasks/{}/results", task.unwrap())),
            "complete_task" => ("POST", format!("/tasks/{}/completion", task.unwrap())),
            "ask_question" => ("POST", "/questions".to_owned()),
            "answer_question" => ("POST", format!("/interactions/{}/answers", interaction.unwrap())),
            "accept_delivery" => ("POST", format!("/interactions/{}/acceptance", interaction.unwrap())),
            "create_document" => ("POST", format!("/tasks/{}/documents", task.unwrap())),
            "decide_document_review" => ("PUT", format!("/tasks/{}/documents/{}/review", task.unwrap(), command["document_id"].as_str().unwrap())),
            "claim_merge_authorization" => ("POST", "/merge-authorizations/claim".to_owned()),
            _ => panic!("vertical test command has no resource route: {kind}"),
        };
        let mut body = serde_json::Map::new();
        for key in ["data_epoch", "command_id", "expected_revision"] {
            body.insert(key.to_owned(), envelope[key].clone());
        }
        for (key, value) in command {
            if key != "kind" && !(key == "request_id" && path != "/requests")
                && !(key == "task_id" && (path.starts_with("/tasks/") || kind == "claim_merge_authorization"))
                && !(key == "authorization_id" && kind == "claim_merge_authorization") {
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
        self.producer(
            true,
            json!({"kind":"create_task", "task_id":id,
            "request_id":format!("request-{id}"), "title":format!("Task {id}"),
            "agent_id":"agent-a", "review_prs":review_prs,
            "dependencies":[], "stack":null}),
        )
        .await;
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
        "options":[{"id":"yes", "label":"Yes", "inputs":[]}]}),
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
        "number":7, "head":"0123456789abcdef", "base":"main",
        "allowed_methods":["squash"]}));
    let (status, first) = fixture.post(&fixture.agent, &envelope).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let revision = first["revision"].clone();
    let (status, replay) = fixture.post(&fixture.agent, &envelope).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay, first);
    assert_eq!(
        fixture.domain.state().unwrap().revision,
        revision.as_u64().unwrap()
    );
    let mut stale = fixture.envelope(json!({"kind":"present_result", "task_id":"pr-task",
        "interaction_id":"pr-delivery", "body":"Code ready", "is_final":true,
        "targets":[{"kind":"pull_request", "pr_id":"pr-1", "head":"0123456789abcdef"}]}));
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
        "targets":[{"kind":"pull_request", "pr_id":"pr-1", "head":"0123456789abcdef"}]}),
        )
        .await;
    let state = fixture.domain.state().unwrap();
    assert_eq!(state.pull_requests.len(), 1);
    assert_eq!(state.pull_requests[&"pr-1".into()].task_id.0, "pr-task");
    assert_eq!(state.pull_requests[&"pr-1".into()].head, "0123456789abcdef");
    assert!(state.interactions.contains_key(&"pr-delivery".into()));
    assert_ne!(
        fixture.domain.state().unwrap().tasks[&"pr-task".into()].state,
        TaskState::Done
    );
    let reopened = DomainRuntime::open(&fixture.home).unwrap();
    let persisted = reopened.state().unwrap();
    assert_eq!(persisted.pull_requests[&"pr-1".into()].task_id.0, "pr-task");
    assert!(persisted.interactions.contains_key(&"pr-delivery".into()));
    drop(reopened);
    fixture.finish().await;
}

#[tokio::test]
async fn http_merge_claim_is_admitted_only_with_github_confirmation_and_the_merge_is_observed() {
    use crate::domain_github::PrFacts;
    use alondra_core::domain_v2::{MergeAttemptState, PrState};
    let mut fixture = Fixture::start().await;
    fixture.task("claim-task", true).await;
    let head = "0123456789abcdef0123456789abcdef01234567";
    fixture.producer(false, json!({"kind":"register_pr", "task_id":"claim-task", "pr_id":"pr-7",
        "host":"github.com", "repository":"example/project", "number":7, "head":head, "base":"main",
        "allowed_methods":["squash"]})).await;
    fixture.producer(false, json!({"kind":"present_result", "task_id":"claim-task",
        "interaction_id":"delivery-7", "body":"Code ready", "is_final":true,
        "targets":[{"kind":"pull_request", "pr_id":"pr-7", "head":head}]})).await;
    fixture.user(json!({"kind":"authorize_merge", "authorization_id":"auth-7", "presentation_id":"delivery-7",
        "pr_id":"pr-7", "expected_head":head, "method":"squash"}));
    let claim = json!({"kind":"claim_merge_authorization", "task_id":"claim-task", "authorization_id":"auth-7",
        "remote_head":head, "remote_base":"main", "base_verified":true});
    let envelope = fixture.envelope(claim.clone());
    let (status, body) = fixture.post(&fixture.agent, &envelope).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "external_read_failed", "GitHub unavailable: the claim is not admitted");
    fixture.github.set(7, Ok(PrFacts { head: "89abcdef0123456789abcdef0123456789abcdef".into(),
        base: "main".into(), state: PrState::Open, merged_head: None }));
    let (status, body) = fixture.post(&fixture.agent, &envelope).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "stale_head", "the Agent's head claim must match GitHub");
    assert_eq!(fixture.domain.state().unwrap().merge_authorizations[&"auth-7".into()].attempt,
        MergeAttemptState::Unclaimed);
    fixture.github.set(7, Ok(PrFacts { head: head.into(), base: "main".into(), state: PrState::Open, merged_head: None }));
    let state = fixture.domain.state().unwrap();
    let authorization = &state.merge_authorizations[&"auth-7".into()];
    assert!(authorization.valid);
    assert_eq!(authorization.head, head);
    assert_eq!(state.tasks[&authorization.task_id].agent_id.as_ref().map(|agent| agent.0.as_str()), Some("agent-a"));
    let (status, body) = fixture.post(&fixture.agent, &envelope).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"message":"Merge the pull request you presented using squash."}));
    for internal in ["data_epoch", "revision", "authorization_id", "task_id", "pr_id"] {
        assert!(body.get(internal).is_none(), "{internal} leaked in {body}");
    }
    assert_eq!(fixture.domain.state().unwrap().merge_authorizations[&"auth-7".into()].attempt,
        MergeAttemptState::Claimed);
    let (status, replay) = fixture.post(&fixture.agent, &envelope).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay, body, "the exact claim replays its receipt");
    fixture.github.set(7, Ok(PrFacts { head: head.into(), base: "main".into(), state: PrState::Merged,
        merged_head: Some(head.into()) }));
    crate::domain_github::reconcile(&fixture.domain, &fixture.github).await.unwrap();
    let state = fixture.domain.state().unwrap();
    assert_eq!(state.merge_authorizations[&"auth-7".into()].attempt, MergeAttemptState::Confirmed);
    assert_eq!(state.pull_requests[&"pr-7".into()].merged_head.as_deref(), Some(head));
    assert_eq!(state.tasks[&"claim-task".into()].state, TaskState::Done, "the confirmed merge of the authorized head completes it");
    fixture.finish().await;
}
