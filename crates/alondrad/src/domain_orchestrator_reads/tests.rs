use super::*;
use alondra_core::domain::{Actor, Command, CommandEnvelope, QuestionOption, Scope, PROTOCOL_VERSION};
use std::collections::BTreeSet;
use std::path::PathBuf;

struct Fixture(crate::test_support::Directory, DomainRuntime);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(domain: &DomainRuntime, actor: Actor, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
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
        options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None }], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question") });
}

/// Agent A runs Task t1 in App; t2 builds on t1 and nobody has it yet.
fn world() -> Fixture {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = DomainRuntime::open(&home).unwrap();
    for (id, name) in [("agent-a", "Instance A"), ("agent-b", "Instance B")] {
        run(&domain, Actor::System, Command::RegisterAgent { agent_id: id.into(), name: name.into(), at: 0 });
    }
    run(&domain, user(), Command::CreateRequest { request_id: "r".into(), body: "Make the editor readable".into(), auto_merge: false, maintenance: false, message: false });
    run(&domain, user(), Command::TitleRequest { request_id: "r".into(), title: "Readable editor".into() });
    let task = |id: &str, agent: Option<&str>, deps: &[&str]| Command::CreateTask { plan_items: None, task_id: id.into(), request_id: "r".into(),
        title: format!("Title {id}"), agent_id: agent.map(Into::into), review_prs: false,
        dependencies: deps.iter().map(|d| TaskId::from(*d)).collect::<BTreeSet<_>>(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), };
    run(&domain, Actor::Orchestrator { generation: 0 }, task("t1", Some("agent-a"), &[]));
    run(&domain, Actor::Orchestrator { generation: 0 }, task("t2", None, &["t1"]));
    domain.live.observe(&json!({"session_id": "agent-a", "hook_event_name": "UserPromptSubmit", "cwd": "/Users/me/Projects/App", "prompt": "go"}));
    domain.report(serde_json::from_value(json!({"session_id": "agent-a", "event": "model", "model": "claude-sonnet-5-5"})).unwrap(), 1);
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
fn the_fleet_shows_the_orchestrator_the_relations_the_verdict_and_the_rounds_of_each_task() {
    let f = world();
    run(&f.1, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, task_id: "t3".into(), request_id: "r".into(), title: "Review".into(), agent_id: Some("agent-b".into()),
        review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
    run(&f.1, user(), Command::SetRelations {
        task_id: "t3".into(), delivered_dependencies: BTreeSet::from(["t1".into()]), blocks: BTreeSet::from(["t1".into()]),
    });
    let fleet = fleet(&f.1, json!({}), json!(null)).unwrap();
    let find = |id: &str| fleet["tasks"].as_array().unwrap().iter().find(|t| t["id"] == id).unwrap().clone();
    let (reviewer, reviewed) = (find("t3"), find("t1"));
    assert_eq!((reviewer["delivered_dependencies"].clone(), reviewer["blocks"].clone()), (json!(["t1"]), json!(["t1"])));
    assert_eq!((reviewed["blocked_by"].clone(), reviewed["review_rounds"].clone(), reviewed["verdict"].clone()), (json!(["t3"]), json!(0), Value::Null));
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

/// 13 October: the orchestrator read the release node as `unclear`, "idle with nothing to show",
/// while it waited for its own release script. Its reads say what the node machine says: the node
/// waits for its command, named, and a task whose node the machine paused is paused, not working.
#[test]
fn the_orchestrator_reads_the_node_machine_s_state_never_an_older_guess() {
    let f = world();
    let work = alondra_core::domain::Background { work: alondra_core::BackgroundKind::Shell, label: "./deploy/brew/release.sh 0.2.67".into(), since: 1, others: 0, gone: None };
    run(&f.1, Actor::System, Command::MoveNode { agent_id: "agent-a".into(), event: alondra_core::domain::NodeEvent::TurnEndedBehind { work }, at: 2 });
    let waiting = fleet(&f.1, json!({}), json!(null)).unwrap();
    let node = &waiting["nodes"][0];
    assert_eq!(node["state"], "waiting_background");
    assert!(node["evidence"].as_str().unwrap().contains("`./deploy/brew/release.sh 0.2.67`"), "{node}");
    let t1 = waiting["tasks"].as_array().unwrap().iter().find(|t| t["id"] == "t1").unwrap().clone();
    assert_eq!(t1["activity"], "waiting_background", "beside its own state, what its node does for it");

    let pause = alondra_core::domain::Pause { reason: alondra_core::domain::PauseReason::Declared { reason: "The build queue is full".into() },
        since: 3, cut: None, turn_since: None, cleanup: Default::default(), released: false };
    run(&f.1, Actor::System, Command::MoveNode { agent_id: "agent-a".into(), event: alondra_core::domain::NodeEvent::Paused { pause }, at: 3 });
    let paused = fleet(&f.1, json!({}), json!(null)).unwrap();
    assert_eq!(paused["nodes"][0]["state"], "paused");
    let t1 = paused["tasks"].as_array().unwrap().iter().find(|t| t["id"] == "t1").unwrap().clone();
    assert_eq!(t1["activity"], "paused_by_machine");
}

fn apply(state: &mut DomainState, actor: Actor, command: Command) {
    let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
        command_id: format!("c-{}", state.revision).into(), expected_revision: state.revision, command };
    state.execute(&actor, &envelope).unwrap();
}

/// 22 October: “simpool: un simulador reutilizado no arrastra el estado del teclado” asked David
/// and ended its turn. Its turn-ended card stayed out of David's sight, but the fleet the
/// orchestrator and Claude read listed it as an open question, “…” stopped without del…. A card
/// the node's wait covers waits on nobody; once nothing covers it, it does.
#[test]
fn the_fleet_lists_no_turn_ended_card_while_the_node_waits_and_lists_it_once_nothing_explains_the_stop() {
    use alondra_core::domain::{LifecycleRecord, RuntimeObservation, TURN_ENDED_PREFIX};
    let mut state = DomainState::new("epoch");
    apply(&mut state, Actor::System, Command::RegisterAgent { agent_id: "agent-b".into(), name: "Instance B".into(), at: 0 });
    apply(&mut state, user(), Command::CreateRequest { request_id: "r".into(), body: "Keyboard".into(), auto_merge: false, maintenance: false, message: false });
    apply(&mut state, Actor::Orchestrator { generation: 0 }, Command::TitleRequest { request_id: "r".into(), title: "Keyboard".into() });
    apply(&mut state, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, task_id: "t3".into(), request_id: "r".into(), title: "Keyboard".into(),
        agent_id: None, review_prs: false, dependencies: BTreeSet::new(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")) });
    apply(&mut state, Actor::System, Command::AdmitExecution { task_id: "t3".into(), agent_id: "agent-b".into(), conversation: "conversation-t3".into(),
        instance: "instance-t3".into(), host: "sdk-host".into(), model: None, account: None });
    let LifecycleRecord::Planned { input } = state.lifecycle_journal[&TaskId::from("t3")][0].clone() else { panic!("planned first") };
    apply(&mut state, Actor::System, Command::RecordRuntimeFacts { task_id: "t3".into(), generation: 1, facts: vec![
        RuntimeObservation::ResourceAvailable, RuntimeObservation::TurnStarted { turn: "turn-1".into(), input },
    ] });
    apply(&mut state, Actor::Agent { agent_id: "agent-b".into() }, Command::AskQuestion { attachments: vec![], interaction_id: "q-keyboard".into(),
        scope: Scope::Task { task_id: "t3".into() }, recipient: Recipient::User, body: "Close without changes in simpool?".into(), blocking: true,
        options: vec![QuestionOption { id: "close".into(), label: "Close".into(), description: None },
            QuestionOption { id: "reset".into(), label: "Reset the keyboard".into(), description: None }],
        card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Decide, "Choose what simpool does with the keyboard") });
    apply(&mut state, Actor::System, Command::RecordTurnBoundary { task_id: "t3".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    let card = format!("{TURN_ENDED_PREFIX}t3-1-turn-1");
    assert!(state.interactions.contains_key(&card.as_str().into()), "the lifecycle keeps its card, out of sight");
    let open = |state: &DomainState| -> Vec<String> { open_questions(state).iter().map(|q| q["id"].as_str().unwrap().to_owned()).collect() };
    assert_eq!(open(&state), ["q-keyboard"], "the node waits on David's answer, not on a stop");

    apply(&mut state, Actor::Agent { agent_id: "agent-b".into() }, Command::Withdraw { interaction_id: "q-keyboard".into() });
    assert_eq!(open(&state), [card], "with nothing to wait for, the stop waits on someone");
}
