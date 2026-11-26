//! A Task that waits for another takes no plan: its node's plan is denied without a card, a plan
//! already open is not approved, and only the orchestrator ends the wait.
use alondra_domain_model::*;
use alondra_views as views;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use alondra_domain_rules::Apply as _;

/// The case of 20 October: the plan card was open when the task started to wait, and the user
/// approved it while the task still showed it waited.
#[test]
fn a_plan_open_when_the_task_starts_to_wait_is_not_approved_until_the_orchestrator_ends_the_wait() {
    let mut s = world();
    task(&mut s, "d", Some("b"));
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, sdk_plan(3));
    let card = decision_card_id(&"t".into(), 1, 3).0;
    depend(&mut s, &["d"]);
    assert_eq!(state(&s), TaskState::WaitingDependency);
    let before = s.clone();
    let error = exec(&mut s, user(), answer(&card, Hold::APPROVE)).unwrap_err();
    assert_eq!(error.code, "task_waiting");
    assert_eq!(error.refusal, Some(Refusal::WaitsForTask { title: "Task d".into() }));
    assert_eq!(s, before, "a refused approval changes nothing");
    assert_eq!(alondra_views::refusal_text(error.refusal.as_ref().unwrap(), alondra_core::text::Lang::Es),
        "Esta tarea espera a «Task d»: su plan se podrá aprobar cuando termine esa espera.");
    depend(&mut s, &[]);
    assert_ne!(state(&s), TaskState::WaitingDependency);
    must(&mut s, user(), answer(&card, Hold::APPROVE));
    assert_eq!(last(&s, "t"), LifecycleRecord::Decided { card: card.as_str().into(), epoch: 3, decision: DecisionRecord::Allow });
    assert!(s.checklists.contains_key(&TaskId::from("t")));
    assert_aggregate_rules(&s);
}

const PLAN: &str = "# Offer Pause\n\n- [ ] Show Pause on a running task\n- [ ] Test it\n";

fn depend(s: &mut DomainState, on: &[&str]) {
    must(s, Actor::Orchestrator { generation: 0 }, Command::SetDependencies {
        task_id: "t".into(), dependencies: on.iter().map(|id| TaskId::from(*id)).collect::<BTreeSet<_>>(), stack: None, reason: None,});
}

fn sdk_plan(epoch: u64) -> Command {
    Command::RecordDecisionRequest {
        task_id: "t".into(), generation: 1, epoch, hold: Hold::Plan, tool: Some("ExitPlanMode".into()), input: json!({}), mcp_server: None,
        prompt: DecisionPrompt::Card {
            body: PLAN.into(), options: Hold::Plan.options(),
            card: CardSpec::new(CardKind::Review, "Offer Pause").content(ContentKind::Plan, PLAN),
        },
    }
}

fn answer(card: &str, option: &str) -> Command {
    let inputs = if option == Hold::REQUEST_CHANGES {
        BTreeMap::from([(Hold::NOTE.to_owned(), InputValue::Text("Wait for it".into()))])
    } else {
        BTreeMap::new()
    };
    Command::AnswerQuestion { attachments: vec![], interaction_id: card.into(), option_id: option.into(), inputs }
}

fn state(s: &DomainState) -> TaskState {
    s.tasks[&TaskId::from("t")].state
}

fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}

pub(super) fn node(id: &str) -> Actor {
    Actor::Agent { agent_id: id.into() }
}

pub(super) fn exec(s: &mut DomainState, actor: Actor, command: Command) -> Result<CommandOutcome, DomainError> {
    s.seed_mergeable_for_presented(&command);
    s.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision, command,
    })
}

pub(super) fn must(s: &mut DomainState, actor: Actor, command: Command) -> CommandOutcome {
    let shown = format!("{command:?}");
    match exec(s, actor, command) {
        Ok(outcome) => outcome,
        Err(error) => panic!("{shown} was refused: {error}"),
    }
}

pub(super) fn task(s: &mut DomainState, id: &str, agent: Option<&str>) {
    must(s, orchestrator(), Command::CreateTask { priority: Default::default(),
        task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: agent.map(Into::into), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")), plan_items: None, continues: None,
    });
}

pub(super) fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

pub(super) fn world() -> DomainState {
    let mut s = DomainState::new("epoch");
    for agent in ["a", "b"] {
        must(&mut s, Actor::System, Command::RegisterAgent { agent_id: agent.into(), name: agent.into(), at: 0 });
    }
    must(&mut s, user(), Command::CreateRequest {
        request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
    });
    must(&mut s, orchestrator(), Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
    s
}

fn lifecycle(s: &DomainState, task: &str) -> (TaskState, bool, Option<AgentId>, Option<TaskEnding>) {
    let t = &s.tasks[&TaskId::from(task)];
    (t.state, t.started, t.agent_id.clone(), t.ending)
}

/// The texts the store keeps for a journal's inputs.
fn writes_texts(id: &TaskId, records: &Journal) -> BTreeMap<alondra_domain::InputId, String> {
    journal_writes(id, records, 0).unwrap().into_iter()
        .filter_map(|write| Some((write.input?, write.text?)))
        .collect()
}

/// After every accepted command, the DTO of every task agrees with the replay of its journal,
/// running the derivations again moves nothing (only decisions do), and the events the store keeps
/// for each journal give back exactly its records.
pub(super) fn assert_aggregate_rules(s: &DomainState) {
    s.validate_journal().unwrap_or_else(|error| panic!("{error}"));
    let mut again = s.clone();
    again.derive_states();
    for (id, task) in &s.tasks {
        assert_eq!(&again.tasks[id], task, "derive_states moved the task {id} without a decision");
    }
    for card in s.interactions.values() {
        assert!(!(card.withdrawn && DomainState::answered_question(card)), "the card {} is answered and withdrawn, which the store refuses", card.id);
    }
    for (id, records) in &s.lifecycle_journal {
        let writes = journal_writes(id, records, 0).unwrap();
        let initial = writes[0].input.clone().expect("the journal is created with its initial input");
        let events: Vec<_> = writes.into_iter().filter_map(|write| write.event).collect();
        let texts = writes_texts(id, records);
        assert_eq!(&records_from_events(id, &initial, &events, &texts).unwrap(), records, "the store's events of {id} do not give back its records");
    }
    for (review, held) in &s.plan_reviews {
        for task in held.open() {
            let lifecycle = replay(task, &s.lifecycle_journal[task]).unwrap();
            assert!(lifecycle.plan_reviews().iter().any(|(id, _)| *id == review), "the lifecycle of {task} does not hold it for the review {review}");
        }
    }
}

/// An SDK task whose observed turn started: admitted, resource available, the planned input taken.
pub(super) fn sdk_turn(s: &mut DomainState, id: &str, node: &str) -> String {
    sdk_turn_named(s, id, node, "turn-1")
}

fn sdk_turn_named(s: &mut DomainState, id: &str, node: &str, turn: &str) -> String {
    sdk_turn_on(s, id, node, turn, None)
}

/// An SDK task whose observed turn started on `account`.
fn sdk_turn_on(s: &mut DomainState, id: &str, node: &str, turn: &str, account: Option<&str>) -> String {
    sdk_turn_on_provider(s,alondra_core::accounts::Provider::Claude,id,node,turn,account)
}

fn sdk_turn_on_provider(s: &mut DomainState, provider: alondra_core::accounts::Provider, id: &str, node: &str, turn: &str, account: Option<&str>) -> String {
    task(s, id, None);
    must(s, Actor::System, Command::AdmitExecution { provider,
        task_id: id.into(), agent_id: node.into(), conversation: format!("conversation-{id}"), instance: format!("instance-{id}"), host: "sdk-host".into(), model: None,
        account: account.map(|account| alondra_core::accounts::AccountId::parse(account).unwrap()), leaves_limit: None,
    });
    let LifecycleRecord::Planned { input } = s.lifecycle_journal[&TaskId::from(id)][0].clone() else { panic!("planned first") };
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: id.into(), generation: 1, facts: vec![
        RuntimeObservation::ResourceAvailable,
        RuntimeObservation::TurnStarted { turn: turn.into(), input },
    ] });
    format!("{TURN_ENDED_PREFIX}{id}-1-{turn}")
}

fn held(s: &DomainState) -> Vec<(String, ClaudeHold)> {
    s.claude_held().into_iter().map(|held| (held.task.0, held.hold)).collect()
}

pub(super) fn last(s: &DomainState, task: &str) -> LifecycleRecord {
    s.lifecycle_journal[&TaskId::from(task)].last().unwrap().clone()
}
