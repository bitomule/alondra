use super::*;
use alondra_core::domain::{Actor, Command, CommandEnvelope, QuestionOption, Scope, PROTOCOL_VERSION};
use std::collections::BTreeSet;
use std::path::PathBuf;

struct Fixture(PathBuf, DomainRuntime);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(domain: &DomainRuntime, actor: Actor, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch,
        command_id: format!("c-{}", state.revision).into(), expected_revision: state.revision, command }, None, || Ok(())).unwrap();
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn agent_a() -> Actor {
    Actor::Agent { agent_id: "agent-a".into() }
}

fn ask(domain: &DomainRuntime, id: &str, recipient: Recipient, scope: Scope, actor: Actor) {
    run(domain, actor, Command::AskQuestion { attachments: vec![], interaction_id: id.into(), scope, recipient,
        body: format!("Question {id}\nwith detail"), blocking: false,
        options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None, inputs: vec![] }], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question") });
}

/// Agent A runs Task t1 in App; t2 builds on t1 and nobody has it yet.
fn world() -> Fixture {
    let home = std::env::temp_dir().join(format!("alondra-orchestrator-reads-{}", crate::auth::random_hex(8)));
    std::fs::create_dir_all(&home).unwrap();
    let domain = DomainRuntime::open(&home).unwrap();
    for (id, name) in [("agent-a", "Instance A"), ("agent-b", "Instance B")] {
        run(&domain, Actor::System, Command::RegisterAgent { agent_id: id.into(), name: name.into() });
    }
    run(&domain, user(), Command::CreateRequest { request_id: "r".into(), body: "Make the editor readable".into(), auto_merge: false });
    run(&domain, user(), Command::TitleRequest { request_id: "r".into(), title: "Readable editor".into() });
    let task = |id: &str, agent: Option<&str>, deps: &[&str]| Command::CreateTask { task_id: id.into(), request_id: "r".into(),
        title: format!("Title {id}"), agent_id: agent.map(Into::into), review_prs: false,
        dependencies: deps.iter().map(|d| TaskId::from(*d)).collect::<BTreeSet<_>>(), stack: None };
    run(&domain, user(), task("t1", Some("agent-a"), &[]));
    run(&domain, user(), task("t2", None, &["t1"]));
    domain.live.observe(&json!({"session_id": "agent-a", "hook_event_name": "UserPromptSubmit", "cwd": "/Users/me/Projects/App", "prompt": "go"}));
    domain.live.mods.apply(serde_json::from_value(json!({"session_id": "agent-a", "event": "model", "model": "claude-sonnet-5-5"})).unwrap(), 1);
    domain.live.observe(&json!({"session_id": "stranger", "hook_event_name": "UserPromptSubmit", "cwd": "/Users/me/Projects/App", "prompt": "go"}));
    Fixture(home, domain)
}

#[test]
fn the_fleet_lists_live_agents_what_waits_on_someone_and_the_requests() {
    let f = world();
    let task_scope = Scope::Task { task_id: "t1".into() };
    ask(&f.1, "q-open", Recipient::User, task_scope.clone(), agent_a());
    ask(&f.1, "q-gone", Recipient::User, task_scope.clone(), agent_a());
    run(&f.1, agent_a(), Command::Withdraw { interaction_id: "q-gone".into() });
    ask(&f.1, "q-orch", Recipient::Orchestrator, task_scope.clone(), agent_a());
    run(&f.1, Actor::System, Command::PublishInformation { attachments: vec![], interaction_id: "info".into(), scope: task_scope,
        recipient: Recipient::User, body: "Found the colour table".into(), card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Know, "Read the update") });
    let fleet = fleet(&f.1, json!({"cpus": 10}), json!(null)).unwrap();
    let nodes = fleet["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1, "only sessions the domain knows as Agents: {nodes:?}");
    assert_eq!((nodes[0]["name"].as_str(), nodes[0]["project"].as_str()), (Some("Instance A"), Some("App")));
    assert_eq!(nodes[0]["model"], "claude-sonnet-5-5");
    let open: Vec<&str> = fleet["open_questions"].as_array().unwrap().iter().map(|q| q["id"].as_str().unwrap()).collect();
    assert_eq!(open, ["q-open", "q-orch"], "a withdrawn question waits on nobody");
    assert_eq!(fleet["open_questions"][1]["to_orchestrator"], true);
    assert_eq!(fleet["open_questions"][0]["title"], "Question q-open");
    let report = &fleet["reports"][0];
    assert_eq!((report["id"].as_str(), report["read"].as_bool(), report["node_name"].as_str()), (Some("info"), Some(false), Some("Alondra")));
    assert_eq!(fleet["orchestrator_messages"].as_array().unwrap().len(), 2, "the new request and the question for the orchestrator are on their way");
    assert_eq!(fleet["requests"][0]["state"], "working");
    let t2 = fleet["tasks"].as_array().unwrap().iter().find(|t| t["id"] == "t2").unwrap();
    assert_eq!((t2["depends_on"].clone(), t2["assignee"].clone()), (json!(["t1"]), Value::Null));
    assert_eq!(fleet["machine"]["cpus"], 10);
}

#[test]
fn a_node_is_found_by_name_or_id_with_its_questions_results_and_undelivered_messages() {
    let f = world();
    ask(&f.1, "q", Recipient::User, Scope::Task { task_id: "t1".into() }, agent_a());
    run(&f.1, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "q".into(), option_id: "yes".into(), inputs: Default::default() });
    f.1.held.lock().unwrap().entry("agent-a".into()).or_default().push("Use the new API".into());
    let by_name = node(&f.1, "Instance A").unwrap();
    assert_eq!(by_name, node(&f.1, "agent-a").unwrap());
    assert_eq!(by_name["node"]["name"], "Instance A");
    assert_eq!(by_name["node"]["model"], "claude-sonnet-5-5");
    assert_eq!(by_name["questions"][0]["status"], "answered");
    assert_eq!(by_name["messages"].as_array().unwrap().len(), 1, "the answer waits for its next turn");
    assert_eq!(by_name["messages_held_for_turn_end"], 1);
    let offline = node(&f.1, "Instance B").unwrap();
    assert_eq!((offline["node"]["alive"].as_bool(), offline["node"]["name"].as_str()), (Some(false), Some("Instance B")));
    assert_eq!(node(&f.1, "nobody").unwrap_err().code, "not_found");
}
