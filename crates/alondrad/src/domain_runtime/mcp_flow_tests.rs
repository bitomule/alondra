//! Synthetic producer flows through the same DomainRuntime used by the v2 MCP HTTP route.
use super::*;
use crate::domain_authority::Principal;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

struct Fixture {
    // Drop the SQLite connection before removing its temporary HOME.
    runtime: DomainRuntime,
    _cleanup: Cleanup,
    home: PathBuf,
    manager: String,
    agent: String,
    sequence: u64,
}

impl Fixture {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!(
            "alondra-mcp-domain-flow-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir(&home).unwrap();
        let runtime = DomainRuntime::open(&home).unwrap();
        let state = runtime.state().unwrap();
        runtime.execute(&Actor::System, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch,
            command_id: "register-fixture-agent".into(),
            expected_revision: state.revision,
            command: Command::RegisterAgent {
                agent_id: "agent-a".into(),
                name: "Fixture agent".into(),
            },
        }, None, || Ok(())).unwrap();
        let manager = runtime.issue(Principal::Orchestrator {
            generation: 0,
            execution_id: "fixture-execution".into(),
        }).unwrap().expose_for_child_environment().to_owned();
        let agent = runtime.issue(Principal::Agent {
            session_id: "agent-a".into(),
            task_binding: None,
            provider_agent_id: None,
        }).unwrap().expose_for_child_environment().to_owned();
        Self { runtime, _cleanup: Cleanup(home.clone()), home, manager, agent, sequence: 0 }
    }

    fn envelope(&mut self, mut command: Value) -> CommandEnvelope {
        if let Some(command) = command.as_object_mut() { alondra_core::domain_v2::derive_wire_card(command); }
        self.sequence += 1;
        let state = self.runtime.state().unwrap();
        serde_json::from_value(json!({
            "protocol_version": PROTOCOL_VERSION,
            "data_epoch": state.data_epoch,
            "command_id": format!("mcp-command-{}", self.sequence),
            "expected_revision": state.revision,
            "command": command,
        })).unwrap()
    }

    fn manager(&mut self, command: Value) -> Result<CommandOutcome, StoreError> {
        let envelope = self.envelope(command);
        self.runtime.execute_bearer(&self.manager, &envelope)
    }

    fn agent(&mut self, command: Value) -> Result<CommandOutcome, StoreError> {
        let envelope = self.envelope(command);
        self.runtime.execute_bearer(&self.agent, &envelope)
    }

    fn user(&mut self, command: Value) -> Result<CommandOutcome, StoreError> {
        let envelope = self.envelope(command);
        self.runtime.execute(&Actor::User { user_id: "fixture-user".into() },
            &envelope, None, || Ok(()))
    }

    fn task(&mut self, id: &str, review_prs: bool) {
        self.manager(json!({"kind":"create_request", "request_id":format!("request-{id}"),
            "body":format!("Work on {id}")})).unwrap();
        self.manager(json!({"kind":"create_task", "task_id":id,
            "request_id":format!("request-{id}"), "title":format!("Task {id}"),
            "agent_id":"agent-a", "review_prs":review_prs,
            "dependencies":[], "stack":null})).unwrap();
    }
}

fn domain_code(error: StoreError) -> String {
    match error {
        StoreError::Domain(error) => error.code,
        StoreError::Storage(error) => panic!("unexpected storage error: {error:#}"),
    }
}

#[test]
fn existing_mcp_producers_cover_pr_document_and_answer_and_only_the_users_acceptance_completes() {
    let mut fixture = Fixture::new();

    fixture.task("pr-task", true);
    fixture.agent(json!({"kind":"register_pr", "task_id":"pr-task", "pr_id":"pr-1",
        "host":"github.com", "repository":"example/project", "number":7,
        "head":"0123456789abcdef", "base":"main", "allowed_methods":["squash"]})).unwrap();
    let pr_target = json!({"kind":"pull_request", "pr_id":"pr-1", "head":"0123456789abcdef"});
    fixture.agent(json!({"kind":"present_result", "task_id":"pr-task",
        "interaction_id":"pr-delivery", "body":"Code is ready for review",
        "targets":[pr_target], "is_final":true})).unwrap();
    assert_eq!(domain_code(fixture.agent(json!({"kind":"complete_task", "task_id":"pr-task",
        "presentation_id":"pr-delivery"})).unwrap_err()), "acceptance_missing");
    let state = fixture.runtime.state().unwrap();
    assert_eq!(state.tasks[&"pr-task".into()].pr_id.as_ref().unwrap().0, "pr-1");
    assert_ne!(state.tasks[&"pr-task".into()].state, TaskState::Done);
    fixture.manager(json!({"kind":"cancel_task", "task_id":"pr-task"})).unwrap();

    fixture.task("doc-task", false);
    std::fs::create_dir_all(fixture.home.join("files")).unwrap();
    let content = b"A synthetic design document";
    std::fs::write(fixture.home.join("files/source"), content).unwrap();
    let digest: String = Sha256::digest(content).iter().map(|byte| format!("{byte:02x}")).collect();
    fixture.agent(json!({"kind":"create_document", "task_id":"doc-task",
        "document_id":"doc-1", "title":"Design", "revision_id":"doc-v1",
        "content_digest":digest, "content_reference":"files/source", "role":"result"})).unwrap();
    let doc_target = json!({"kind":"document_revision", "document_id":"doc-1",
        "revision_id":"doc-v1"});
    fixture.agent(json!({"kind":"present_result", "task_id":"doc-task",
        "interaction_id":"doc-delivery", "body":"Document ready",
        "targets":[doc_target], "is_final":true})).unwrap();
    fixture.user(json!({"kind":"decide_document_review", "task_id":"doc-task",
        "document_id":"doc-1", "revision_id":"doc-v1", "accepted":true})).unwrap();
    assert_eq!(fixture.runtime.state().unwrap().tasks[&"doc-task".into()].state, TaskState::Done);

    fixture.task("answer-task", false);
    fixture.agent(json!({"kind":"ask_question", "interaction_id":"question-1",
        "scope":{"kind":"task", "task_id":"answer-task"},
        "recipient":{"kind":"user"}, "body":"Which answer?", "blocking":true,
        "options":[{"id":"yes", "label":"Yes", "inputs":[]}]})).unwrap();
    fixture.user(json!({"kind":"answer_question", "interaction_id":"question-1",
        "option_id":"yes", "inputs":{}})).unwrap();
    let answer_target = json!({"kind":"text", "text":"The requested answer"});
    fixture.agent(json!({"kind":"present_result", "task_id":"answer-task",
        "interaction_id":"answer-delivery", "body":"The requested answer",
        "targets":[answer_target.clone()], "is_final":true})).unwrap();
    assert_eq!(domain_code(fixture.agent(json!({"kind":"complete_task",
        "task_id":"answer-task", "presentation_id":"answer-delivery"})).unwrap_err()),
        "acceptance_missing");
    fixture.user(json!({"kind":"accept_delivery", "presentation_id":"answer-delivery",
        "targets":[answer_target]})).unwrap();
    let state = fixture.runtime.state().unwrap();
    assert_eq!(state.tasks[&"answer-task".into()].state, TaskState::Done);
    assert!(state.interactions.contains_key(&"question-1".into()));
}

fn decision(id: &str, task: &str, options: Value) -> Value {
    json!({"kind":"ask_question", "interaction_id":id, "scope":{"kind":"task", "task_id":task},
        "recipient":{"kind":"user"}, "body":"Which name?", "blocking":false, "options":options,
        "card":{"kind":"decide", "need":"Pick a name", "why":"The release needs one"}})
}

fn names() -> Value {
    json!([{"id":"a", "label":"Atlas", "inputs":[]}, {"id":"b", "label":"Borealis", "inputs":[]}])
}

fn problems(fixture: &Fixture) -> Vec<Interaction> {
    fixture.runtime.state().unwrap().interactions.into_values()
        .filter(|i| i.card.kind == CardKind::Problem).collect()
}

#[test]
fn three_rejections_of_the_same_card_raise_a_problem() {
    let mut fixture = Fixture::new();
    fixture.task("t1", false);
    let one_option = json!([{"id":"a", "label":"Atlas", "inputs":[]}]);
    for _ in 0..2 {
        assert_eq!(domain_code(fixture.agent(decision("stuck", "t1", one_option.clone())).unwrap_err()), "invalid_input");
    }
    assert!(problems(&fixture).is_empty(), "two rejections are still the node's to fix");
    assert_eq!(domain_code(fixture.agent(decision("stuck", "t1", one_option.clone())).unwrap_err()), "invalid_input",
        "a rejection stays a rejection: it never becomes an answer");
    let raised = problems(&fixture);
    assert_eq!(raised.len(), 1);
    let problem = &raised[0];
    assert_eq!(problem.author, Author::System);
    assert_eq!(problem.scope, Scope::Task { task_id: "t1".into() });
    assert!(problem.card.need.contains("Fixture agent"), "{}", problem.card.need);
    assert!(problem.card.why.contains("options") && problem.card.why.contains("invalid_input"), "{}", problem.card.why);
    assert!(problem.body.contains("3 times") && problem.body.contains("stuck"), "{}", problem.body);
    assert!(problem.card.content.as_ref().is_some_and(|c| c.kind == ContentKind::Diagnosis));
    assert!(!fixture.runtime.state().unwrap().interactions.contains_key(&"stuck".into()), "the refused card was never created");
    fixture.agent(decision("stuck", "t1", one_option)).unwrap_err();
    assert_eq!(problems(&fixture).len(), 1, "one Problem per stuck card");
}

#[test]
fn rejections_of_different_cards_or_broken_by_a_success_do_not_add_up() {
    let mut fixture = Fixture::new();
    fixture.task("t1", false);
    let one_option = json!([{"id":"a", "label":"Atlas", "inputs":[]}]);
    for id in ["first", "second", "third"] {
        fixture.agent(decision(id, "t1", one_option.clone())).unwrap_err();
    }
    assert!(problems(&fixture).is_empty(), "three different cards are three different mistakes");
    for _ in 0..2 {
        fixture.agent(decision("fixed", "t1", one_option.clone())).unwrap_err();
    }
    fixture.agent(decision("fixed", "t1", names())).unwrap();
    for _ in 0..2 {
        fixture.agent(decision("fixed", "t1", names())).unwrap_err();
    }
    assert!(problems(&fixture).is_empty(), "a card that went through starts the count again");
}

#[test]
fn a_retried_card_is_the_first_card_and_not_a_second_one() {
    let mut fixture = Fixture::new();
    fixture.task("t1", false);
    let envelope = |fixture: &Fixture| {
        let state = fixture.runtime.state().unwrap();
        serde_json::from_value::<CommandEnvelope>(json!({
            "protocol_version": PROTOCOL_VERSION, "data_epoch": state.data_epoch,
            "command_id": "mcp-ask-retry-card", "expected_revision": 0,
            "command": decision("retry-card", "t1", names()),
        })).unwrap()
    };
    let first = fixture.runtime.execute_bearer(&fixture.agent, &envelope(&fixture)).unwrap();
    fixture.manager(json!({"kind":"create_request", "request_id":"unrelated", "body":"Something else"})).unwrap();
    let again = fixture.runtime.execute_bearer(&fixture.agent, &envelope(&fixture)).unwrap();
    assert_eq!(first.created_ids, again.created_ids);
    let cards = fixture.runtime.state().unwrap().interactions.values()
        .filter(|i| i.card.need == "Pick a name").count();
    assert_eq!(cards, 1);
}
