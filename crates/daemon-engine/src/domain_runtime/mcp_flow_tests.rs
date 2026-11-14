//! Synthetic producer flows through the same DomainRuntime used by the v2 MCP HTTP route.
use super::*;
use crate::locks::Poisonable as _;
use crate::domain_authority::Principal;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

struct Cleanup(crate::test_support::Directory);
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
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        let runtime = DomainRuntime::open(&home).unwrap();
        let state = runtime.state().unwrap();
        runtime.execute(&Actor::System, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: "register-fixture-agent".into(),
            expected_revision: state.revision,
            command: Command::RegisterAgent {
                agent_id: "agent-a".into(),
                name: "Fixture agent".into(), at: 0
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
        Self { runtime, home: home.to_path_buf(), _cleanup: Cleanup(home), manager, agent, sequence: 0 }
    }

    fn envelope(&mut self, mut command: Value) -> CommandEnvelope {
        if let Some(command) = command.as_object_mut() { alondra_core::domain::derive_wire_card(command); }
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

    /// The task waits in Closing for its node; Alondra records it closed and the task is done.
    fn end_node(&mut self, task: &str) {
        assert_eq!(self.runtime.state().unwrap().tasks[&task.into()].state, TaskState::Closing);
        let envelope = self.envelope(json!({"kind":"record_node_end", "task_id":task, "end":"closed"}));
        self.runtime.execute(&Actor::System, &envelope, None, || Ok(())).unwrap();
    }

    fn task(&mut self, id: &str, review_prs: bool) {
        self.manager(json!({"kind":"create_request", "request_id":format!("request-{id}"),
            "body":format!("Work on {id}")})).unwrap();
        self.manager(json!({"kind":"create_task", "task_id":id,
            "request_id":format!("request-{id}"), "title":format!("Task {id}"),
            "agent_id":null, "review_prs":review_prs,
            "dependencies":[], "stack":null, "launch":{"name":format!("app/{id}"), "cwd":"/tmp"}})).unwrap();
        self.user(json!({"kind":"assign_task", "task_id":id, "agent_id":"agent-a"})).unwrap();
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
        "presentation_id":"pr-delivery"})).unwrap_err()), "forbidden");
    let state = fixture.runtime.state().unwrap();
    assert_eq!(state.tasks[&"pr-task".into()].pr_id.as_ref().unwrap().0, "pr-1");
    assert_ne!(state.tasks[&"pr-task".into()].state, TaskState::Done);
    assert_eq!(domain_code(fixture.manager(json!({"kind":"cancel_task", "task_id":"pr-task"})).unwrap_err()), "open_pr_delivered");
    fixture.user(json!({"kind":"cancel_task", "task_id":"pr-task"})).unwrap();

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
    fixture.end_node("doc-task");
    assert_eq!(fixture.runtime.state().unwrap().tasks[&"doc-task".into()].state, TaskState::Done);

    fixture.task("answer-task", false);
    fixture.agent(json!({"kind":"ask_question", "interaction_id":"question-1",
        "scope":{"kind":"task", "task_id":"answer-task"},
        "recipient":{"kind":"user"}, "body":"Which answer?", "blocking":true,
        "options":[{"id":"yes", "label":"Yes"}]})).unwrap();
    fixture.user(json!({"kind":"answer_question", "interaction_id":"question-1",
        "option_id":"yes", "inputs":{}})).unwrap();
    let answer_target = json!({"kind":"text", "text":"The requested answer"});
    fixture.agent(json!({"kind":"present_result", "task_id":"answer-task",
        "interaction_id":"answer-delivery", "body":"The requested answer",
        "targets":[answer_target.clone()], "is_final":true})).unwrap();
    assert_eq!(domain_code(fixture.agent(json!({"kind":"complete_task",
        "task_id":"answer-task", "presentation_id":"answer-delivery"})).unwrap_err()),
        "forbidden");
    fixture.user(json!({"kind":"accept_delivery", "presentation_id":"answer-delivery",
        "targets":[answer_target]})).unwrap();
    fixture.end_node("answer-task");
    let state = fixture.runtime.state().unwrap();
    assert_eq!(state.tasks[&"answer-task".into()].state, TaskState::Done);
    assert!(state.interactions.contains_key(&"question-1".into()));
}

fn decision(id: &str, task: &str, options: Value) -> Value {
    json!({"kind":"ask_question", "interaction_id":id, "scope":{"kind":"task", "task_id":task},
        "recipient":{"kind":"user"}, "body":"Which name?", "blocking":false, "options":options,
        "card":{"kind":"decide", "title":"Release name", "need":"Pick a name", "why":"The release needs one"}})
}

fn names() -> Value {
    json!([{"id":"a", "label":"Atlas"}, {"id":"b", "label":"Borealis"}])
}

fn problems(fixture: &Fixture) -> Vec<Interaction> {
    fixture.runtime.state().unwrap().interactions.values().cloned()
        .filter(|i| i.card.kind == CardKind::Problem).collect()
}

fn refused_notices(fixture: &Fixture) -> Vec<Interaction> {
    fixture.runtime.state().unwrap().interactions.values().cloned()
        .filter(|i| matches!(i.system_card, Some(SystemCard::RefusedCards { .. }))).collect()
}

/// Case 4 of the card audit: «… is stuck: Alondra refused its card» reached the user. The
/// orchestrator hears of it, and tells the node what to change.
#[test]
fn three_rejections_of_the_same_card_tell_the_orchestrator_never_the_user() {
    let mut fixture = Fixture::new();
    fixture.task("t1", false);
    let one_option = json!([{"id":"a", "label":"Atlas"}]);
    for _ in 0..2 {
        assert_eq!(domain_code(fixture.agent(decision("stuck", "t1", one_option.clone())).unwrap_err()), "invalid_input");
    }
    assert!(refused_notices(&fixture).is_empty(), "two rejections are still the node's to fix");
    assert_eq!(domain_code(fixture.agent(decision("stuck", "t1", one_option.clone())).unwrap_err()), "invalid_input",
        "a rejection stays a rejection: it never becomes an answer");
    let told = refused_notices(&fixture);
    assert_eq!(told.len(), 1);
    let notice = &told[0];
    assert_eq!((&notice.author, &notice.recipient), (&Author::System, &Recipient::Orchestrator));
    assert!(matches!(notice.payload, InteractionPayload::Information { .. }));
    assert!(notice.body.contains("Fixture agent") && notice.body.contains("3 times") && notice.body.contains("invalid_input"), "{}", notice.body);
    assert!(problems(&fixture).is_empty(), "the user gets no card for it");
    assert!(!fixture.runtime.state().unwrap().interactions.contains_key(&"stuck".into()), "the refused card was never created");
    fixture.agent(decision("stuck", "t1", one_option)).unwrap_err();
    assert_eq!(refused_notices(&fixture).len(), 1, "one notice per stuck card");
}

#[test]
fn the_node_asking_after_it_was_stuck_withdraws_the_notice() {
    let mut fixture = Fixture::new();
    fixture.task("t1", false);
    let one_option = json!([{"id":"a", "label":"Atlas"}]);
    for _ in 0..3 {
        fixture.agent(decision("stuck", "t1", one_option.clone())).unwrap_err();
    }
    assert!(refused_notices(&fixture).iter().all(|p| !p.withdrawn) && refused_notices(&fixture).len() == 1);
    fixture.agent(decision("other-card", "t1", names())).unwrap();
    assert!(refused_notices(&fixture).iter().all(|p| p.withdrawn), "the node is handling it");
}

#[test]
fn rejections_of_different_cards_or_broken_by_a_success_do_not_add_up() {
    let mut fixture = Fixture::new();
    fixture.task("t1", false);
    let one_option = json!([{"id":"a", "label":"Atlas"}]);
    for id in ["first", "second", "third"] {
        fixture.agent(decision(id, "t1", one_option.clone())).unwrap_err();
    }
    assert!(refused_notices(&fixture).is_empty(), "three different cards are three different mistakes");
    for _ in 0..2 {
        fixture.agent(decision("fixed", "t1", one_option.clone())).unwrap_err();
    }
    fixture.agent(decision("fixed", "t1", names())).unwrap();
    for _ in 0..2 {
        fixture.agent(decision("fixed", "t1", names())).unwrap_err();
    }
    assert!(refused_notices(&fixture).is_empty(), "a card that went through starts the count again");
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
        .filter(|i| i.card.effective_title() == "Release name").count();
    assert_eq!(cards, 1);
}

#[test]
fn the_orchestrator_gives_a_task_to_a_fresh_node_and_cannot_hand_the_same_node_a_second_one() {
    let mut fixture = Fixture::new();
    fixture.manager(json!({"kind":"create_request", "request_id":"request-o", "body":"Work"})).unwrap();
    fixture.manager(json!({"kind":"title_request", "request_id":"request-o", "title":"Work"})).unwrap();
    let create = |id: &str| json!({"kind":"create_task", "task_id":id, "request_id":"request-o", "title":id,
        "agent_id":"agent-a", "review_prs":false, "dependencies":[], "stack":null, "launch":{"name":format!("app/{id}"), "cwd":"/tmp"}});
    fixture.manager(create("first")).unwrap();
    assert_eq!(domain_code(fixture.manager(create("second")).unwrap_err()), "agent_busy", "still on its first task");
    fixture.manager(json!({"kind":"cancel_task", "task_id":"first"})).unwrap();
    assert_eq!(domain_code(fixture.manager(create("second")).unwrap_err()), "agent_used", "a node works on one task for its whole life");
}

#[test]
fn a_checked_delivery_rejects_a_changed_pr_without_consuming_its_receipt() {
    let mut fixture = Fixture::new();
    fixture.task("privacy-task", false);
    let original = "a".repeat(40);
    let changed = "b".repeat(40);
    let register = |head: &str| json!({"kind":"register_pr", "task_id":"privacy-task", "pr_id":"privacy-pr",
        "host":"github.com", "repository":"example/project", "number":7,
        "head":head, "base":"main", "allowed_methods":["squash"]});
    fixture.agent(register(&original)).unwrap();
    let mut envelope = fixture.envelope(json!({"kind":"present_result", "task_id":"privacy-task",
        "interaction_id":"privacy-delivery", "body":"Ready",
        "targets":[{"kind":"pull_request", "pr_id":"privacy-pr", "head":""}], "is_final":true}));
    envelope.expected_revision = 0;
    fixture.agent(register(&changed)).unwrap();
    let revision = fixture.runtime.state().unwrap().revision;
    let guard = |state: &DomainState| {
        if state.pull_requests[&PrId::from("privacy-pr")].head != original {
            Err(DomainError::new("public_content_review_changed", "The reviewed PR changed"))
        } else {
            Ok(())
        }
    };
    let error = fixture.runtime.execute_bearer_checked(&fixture.agent, &envelope, guard).unwrap_err();
    assert_eq!(domain_code(error), "public_content_review_changed");
    let state = fixture.runtime.state().unwrap();
    assert_eq!(state.revision, revision);
    assert!(!state.interactions.contains_key(&"privacy-delivery".into()));
    fixture.agent(register(&original)).unwrap();
    let accepted = fixture.runtime.execute_bearer_checked(&fixture.agent, &envelope, guard).unwrap();
    assert!(fixture.runtime.state().unwrap().interactions.contains_key(&"privacy-delivery".into()));
    let replayed = fixture.runtime.execute_bearer_checked(&fixture.agent, &envelope, guard).unwrap();
    assert_eq!(replayed, accepted, "the refused attempt must not consume or rewrite the receipt");
}

fn digest_of(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn attachment_of(name: &str, path: &std::path::Path, bytes: &[u8]) -> Value {
    json!({"id": name, "name": name, "content_digest": digest_of(bytes),
        "content_reference": path.display().to_string(), "bytes": bytes.len()})
}

fn delivery_with(fixture: &mut Fixture, id: &str, attachments: Value) -> Result<CommandOutcome, StoreError> {
    fixture.agent(json!({"kind":"present_result", "task_id":"files-task", "interaction_id":id,
        "body":"Report ready", "targets":[{"kind":"text","text":"Report ready"}], "is_final":false,
        "attachments": attachments}))
}

fn open_worktree(fixture: &mut Fixture) -> PathBuf {
    fixture.task("files-task", false);
    let worktree = fixture.home.join("worktree");
    std::fs::create_dir_all(&worktree).unwrap();
    assert!(std::process::Command::new("git").args(["init", "-q"]).arg(&worktree).status().unwrap().success());
    let envelope = fixture.envelope(json!({"kind":"record_task_worktree", "task_id":"files-task",
        "path":worktree.display().to_string(), "origin":"task"}));
    fixture.runtime.execute(&Actor::System, &envelope, None, || Ok(())).unwrap();
    worktree
}

fn read_back(fixture: &Fixture, interaction: &str) -> Result<Vec<u8>, anyhow::Error> {
    let state = fixture.runtime.state().unwrap();
    let stored = &state.interactions[&interaction.into()].attachments[0];
    let chunk = fixture.runtime.stored_reads.attachment(&fixture.home, &stored.content_reference, &stored.content_digest, 0, None)?;
    use base64::Engine as _;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(chunk["data"].as_str().expect("chunk data")).unwrap())
}

#[test]
fn a_card_still_opens_its_attachment_after_the_worktree_file_changes_and_goes() {
    let mut fixture = Fixture::new();
    let worktree = open_worktree(&mut fixture);
    let report = b"The validation report as presented";
    let source = worktree.join("report.md");
    std::fs::write(&source, report).unwrap();
    delivery_with(&mut fixture, "card-1", json!([attachment_of("report.md", &source, report)])).unwrap();
    std::fs::write(&source, b"Regenerated after presenting").unwrap();
    assert_eq!(read_back(&fixture, "card-1").unwrap(), report);
    std::fs::remove_dir_all(&worktree).unwrap();
    assert_eq!(read_back(&fixture, "card-1").unwrap(), report);
}

#[test]
fn a_card_whose_attachment_changed_before_presenting_is_refused_then() {
    let mut fixture = Fixture::new();
    let worktree = open_worktree(&mut fixture);
    let source = worktree.join("report.md");
    std::fs::write(&source, b"First").unwrap();
    let described = attachment_of("report.md", &source, b"First");
    std::fs::write(&source, b"Second").unwrap();
    let error = delivery_with(&mut fixture, "card-2", json!([described])).unwrap_err();
    assert_eq!(domain_code(error), "content_unavailable");
    assert!(!fixture.runtime.state().unwrap().interactions.contains_key(&"card-2".into()));
}

#[test]
fn a_card_recorded_before_cards_owned_their_files_gets_a_copy_while_the_file_matches() {
    let mut fixture = Fixture::new();
    let worktree = open_worktree(&mut fixture);
    let report = b"Recorded with a path in the worktree";
    let source = worktree.join("report.md");
    std::fs::write(&source, report).unwrap();
    delivery_with(&mut fixture, "card-3", json!([attachment_of("report.md", &source, report)])).unwrap();
    fixture.runtime.store.lock_or_exit("the domain store").keep_attachment_at("card-3", "report.md", &source.display().to_string()).unwrap();
    let (kept, lost) = fixture.runtime.keep_attachments().unwrap();
    assert_eq!((kept, lost.len()), (1, 0), "{lost:?}");
    std::fs::remove_dir_all(&worktree).unwrap();
    assert_eq!(read_back(&fixture, "card-3").unwrap(), report);
}

fn aged(path: &std::path::Path, days: u64) {
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 24 * 60 * 60);
    std::fs::File::open(path).unwrap().set_modified(old).unwrap();
}

fn swept(fixture: &Fixture) -> std::collections::BTreeMap<String, bool> {
    let state = fixture.runtime.state().unwrap();
    crate::files_retention::sweep(&fixture.home, &state, std::time::SystemTime::now())
        .entries.into_iter().map(|entry| (entry.path, entry.remove)).collect()
}

#[test]
fn the_sweep_keeps_what_an_open_task_or_card_needs_and_lists_the_rest() {
    let mut fixture = Fixture::new();
    fixture.task("sweep-task", false);
    let files = fixture.home.join("files");
    for (path, days) in [("results/sweep-task/r.md", 40), ("tasks/sweep-task/s.txt", 40), ("images/shot.png", 40), ("images/orphan.png", 40)] {
        std::fs::create_dir_all(files.join(path).parent().unwrap()).unwrap();
        std::fs::write(files.join(path), path).unwrap();
        aged(&files.join(path), days);
    }
    fixture.agent(json!({"kind":"present_result", "task_id":"sweep-task", "interaction_id":"sweep-card",
        "body":"See ![shot](/files?path=files/images/shot.png)", "targets":[{"kind":"text","text":"x"}], "is_final":false})).unwrap();
    let open = swept(&fixture);
    assert_eq!(open["results/sweep-task"], false, "its task is open");
    assert_eq!(open["tasks/sweep-task"], false, "its task is open");
    assert_eq!(open["images/shot.png"], false, "its card is in the inbox");
    assert_eq!(open["images/orphan.png"], true, "nobody names it and it is old");

    fixture.agent(json!({"kind":"present_result", "task_id":"sweep-task", "interaction_id":"sweep-final",
        "body":"Done", "targets":[{"kind":"text","text":"Done"}], "is_final":true})).unwrap();
    fixture.user(json!({"kind":"accept_delivery", "presentation_id":"sweep-final", "targets":[{"kind":"text","text":"Done"}]})).unwrap();
    fixture.end_node("sweep-task");
    fixture.user(json!({"kind":"accept_delivery", "presentation_id":"sweep-card", "targets":[{"kind":"text","text":"x"}]})).ok();
    let closed = swept(&fixture);
    assert_eq!(closed["tasks/sweep-task"], true, "scratch goes with its task");
    assert_eq!(closed["results/sweep-task"], true, "the saved result is older than two weeks");
    assert_eq!(closed["images/orphan.png"], true);
}
