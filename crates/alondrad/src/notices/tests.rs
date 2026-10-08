use super::notices;
use crate::domain_delivery::deliver_from_runtime;
use crate::domain_runtime::DomainRuntime;
use crate::push::Push;
use alondra_core::domain::{
    Actor, CardKind, CardSpec, Command, CommandEnvelope, QuestionOption, Recipient, Scope,
};
use alondra_core::views::Lang;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const FIXTURE: &str = "fixtures/views/local-notices.json";

struct Fixture {
    home: crate::test_support::Directory,
    domain: Arc<DomainRuntime>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

impl Fixture {
    fn new() -> Self {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        let fixture = Self { home, domain };
        fixture.command(Actor::System, "agent", Command::RegisterAgent { agent_id: "agent-a".into(), name: "A".into() });
        fixture.command(Actor::User { user_id: "u".into() }, "request", Command::CreateRequest {
            request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        fixture.command(Actor::User { user_id: "u".into() }, "task", Command::CreateTask {
            task_id: "task-a".into(), request_id: "request".into(), title: "Task".into(),
            agent_id: Some("agent-a".into()), review_prs: false, dependencies: Default::default(), stack: None });
        fixture
    }

    fn command(&self, actor: Actor, id: &str, command: Command) {
        let state = self.domain.state().unwrap();
        self.domain.execute(&actor, &CommandEnvelope {
            protocol_version: 2, data_epoch: state.data_epoch.clone(), command_id: id.into(),
            expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    fn ask(&self, card: &str) {
        self.command(Actor::Agent { agent_id: "agent-a".into() }, card, Command::AskQuestion {
            attachments: vec![], interaction_id: card.into(), scope: Scope::Task { task_id: "task-a".into() },
            recipient: Recipient::User, body: "Choose".into(), blocking: true,
            options: ["yes", "no"].map(|id| QuestionOption { id: id.into(), label: id.into(), description: None, inputs: vec![] }).into(),
            card: CardSpec::new(CardKind::Decide, "Pick the parser").why("The tests need one."),
        });
    }

    async fn deliver(&self, card: &str) -> String {
        let client = crate::client_routes::Client::open(self.domain.clone(), &self.home, Default::default());
        let message = self.domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .find(|message| message.interaction_id.as_deref() == Some(card) && message.recipient_kind == "user")
            .unwrap();
        deliver_from_runtime(&self.domain, &self.home, &Push::open(&self.home).unwrap(), None, &client, message.clone(), Duration::ZERO)
            .await.unwrap();
        self.domain.store.lock().unwrap().delivery_evidence_page(0).unwrap()
            .into_iter().find(|item| item.message_id == message.id).unwrap().state
    }

    fn notices(&self) -> Value {
        let mut body = notices(&self.domain, Lang::En).unwrap();
        assert_eq!(body["data_epoch"], json!(self.domain.state().unwrap().data_epoch));
        body["data_epoch"] = json!("fixture-epoch");
        body
    }
}

#[tokio::test]
async fn a_card_is_noticed_once_its_push_passes_the_gate_and_leaves_when_answered() {
    let fixture = Fixture::new();
    fixture.ask("question-a");
    assert_eq!(fixture.notices()["notices"], json!([]), "a card the gate has not let through yet is not noticed");
    assert_eq!(fixture.notices()["open"], json!(["question-a"]));

    assert_eq!(fixture.deliver("question-a").await, "handed");
    let body = fixture.notices();
    let notice = &body["notices"][0];
    assert_eq!(notice["card"], "question-a");
    assert_eq!(notice["title"], "Pick the parser");
    assert_eq!(notice["body"], "The tests need one.");
    assert_eq!(notice["thread"], "task-a", "grouped by Task like the phone's push");
    assert_eq!(notice["level"], "active");
    if std::env::var_os("ALONDRA_WRITE_NOTICES_FIXTURE").is_some() {
        let root = std::env::var("BUILD_WORKSPACE_DIRECTORY").unwrap();
        let mut text = serde_json::to_string_pretty(&body).unwrap();
        text.push('\n');
        std::fs::write(std::path::Path::new(&root).join(FIXTURE), text).unwrap();
    }
    let fixture_body: Value = serde_json::from_str(include_str!("../../../../fixtures/views/local-notices.json")).unwrap();
    assert_eq!(body, fixture_body, "{FIXTURE} is what the daemon sends the Mac app; regenerate it with \
        ALONDRA_WRITE_NOTICES_FIXTURE=1 bazelisk run //crates/alondrad:alondrad_test -- notices");

    fixture.command(Actor::User { user_id: "u".into() }, "answer", Command::AnswerQuestion {
        attachments: vec![], interaction_id: "question-a".into(), option_id: "yes".into(), inputs: Default::default() });
    assert_eq!(fixture.notices()["open"], json!([]), "an answered card leaves the queue, so its notification goes");
    assert_eq!(fixture.notices()["notices"], json!([]));
}

#[tokio::test]
async fn a_card_put_again_unchanged_is_not_noticed_twice() {
    let fixture = Fixture::new();
    fixture.ask("question-a");
    assert_eq!(fixture.deliver("question-a").await, "handed");
    fixture.command(Actor::Agent { agent_id: "agent-a".into() }, "withdraw", Command::Withdraw { interaction_id: "question-a".into() });
    fixture.ask("question-b");
    assert_eq!(fixture.deliver("question-b").await, "discarded");
    let body = fixture.notices();
    assert_eq!(body["open"], json!(["question-b"]));
    assert_eq!(body["notices"], json!([]), "the user was already told about this card");
}

#[tokio::test]
async fn a_card_withdrawn_before_its_push_left_is_never_noticed() {
    let fixture = Fixture::new();
    fixture.ask("question-a");
    fixture.command(Actor::Agent { agent_id: "agent-a".into() }, "withdraw", Command::Withdraw { interaction_id: "question-a".into() });
    assert_eq!(fixture.deliver("question-a").await, "discarded");
    assert_eq!(fixture.notices(), json!({"data_epoch": "fixture-epoch", "notices": [], "open": []}));
}

#[tokio::test]
async fn only_a_signed_device_on_this_mac_reads_notices() {
    let fixture = Fixture::new();
    drop(DomainRuntime::open(&fixture.home).unwrap());
    let state = crate::domain_bootstrap::DomainDaemon::open(&fixture.home).unwrap();
    let auth = state.auth();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = crate::domain_bootstrap::router(state)
        .layer(axum::middleware::from_fn_with_state(auth, crate::auth::guard));
    let server = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
    });
    let client = reqwest::Client::new();
    let unsigned = client.get(format!("{base}/notices")).send().await.unwrap();
    assert_eq!(unsigned.status(), reqwest::StatusCode::FORBIDDEN, "an unsigned local caller is refused");
    let proxied = client.get(format!("{base}/notices")).header("x-forwarded-for", "10.0.0.2").send().await.unwrap();
    assert!(!proxied.status().is_success(), "a request from off this Mac is refused");
    server.abort();
}
