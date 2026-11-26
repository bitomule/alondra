//! Decisions Claude asks for through a task's SDK execution: what Alondra decides by itself, the
//! cards the user decides the rest on, and what each answer tells Claude.
use alondra_domain_model::*;
use alondra_views as views;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn a_request_aborted_after_its_turn_ended_asks_how_the_task_goes_on() {
    use alondra_views::{Client, RenderContext, Transport, build, live::LiveSnapshot, text::Lang};
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, bash(6, "make deploy"));
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    assert_eq!(held(&s), vec![card_of(6)], "the request explains the next step");
    let outcome = must(&mut s, Actor::System, Command::RecordDecisionAborted { task_id: "t".into(), generation: 1, epoch: 6 });
    let cancelled = decision_cancelled_card_id(&"t".into(), 1, 6);
    assert_eq!(held(&s), vec![cancelled.0.clone()]);
    assert!(alondra_core::id::valid(&cancelled.0));
    assert!(outcome.effects.iter().any(|effect| matches!(effect, DomainEffect::NotifyUser { interaction_id, .. } if *interaction_id == cancelled)));
    assert_aggregate_rules(&s);
    let index = ReadIndex::build(&s);
    let live = LiveSnapshot::default();
    let view = build("task:t", RenderContext { domain: &s, index: &index, live: &live, client: Client::Tui, lang: Lang::Es,
        transport: Transport::Loopback, cursor: "test", rendered_at: "" }).unwrap();
    let view = serde_json::to_string(&view).unwrap();
    assert!(view.contains("dejó de esperar tu decisión"), "{view}");
    must(&mut s, user(), answer(&cancelled.0, CARRY_ON, None));
    assert!(matches!(last(&s, "t"), LifecycleRecord::Answered { .. }));
    assert_aggregate_rules(&s);
}

/// The notification of an SDK permission card answers the same request the card does: its buttons
/// are the card's own options, bound to the card's id and revision, which carry the request's
/// generation and epoch.
#[test]
fn the_push_of_an_sdk_permission_card_offers_its_answers_bound_to_the_same_request() {
    use alondra_views::{Client, Lang, LiveSnapshot, RenderContext, Transport, build, push_actions};
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, bash(7, "make test"));
    let card = card_of(7);
    let read = ReadSnapshot::build(std::sync::Arc::new(s.clone()));
    let pushed = push_actions(&read, &card, Lang::En).unwrap();
    let options: Vec<&str> = pushed.iter().filter_map(|action| action.option.as_deref()).collect();
    assert_eq!(options, ["allow", "deny", "always"], "{pushed:?}");
    let view = build(&format!("card:{card}"), RenderContext {
        domain: &s, index: read.index(), live: &LiveSnapshot::default(), client: Client::Ios, lang: Lang::En,
        transport: Transport::Relay, cursor: "", rendered_at: "",
    }).unwrap();
    let rendered = serde_json::to_string(&view).unwrap();
    for action in pushed.iter().filter(|action| action.option.is_some()) {
        assert!(action.id.starts_with(&format!("ans:{card}@")), "bound to the card and its revision: {}", action.id);
        assert!(rendered.contains(&action.id), "the card offers the same action: {}", action.id);
    }
}

fn card_prompt(hold: Hold, body: &str) -> DecisionPrompt {
    let options = hold.options_offering(hold == Hold::Permission);
    let card = match hold {
        Hold::Permission => CardSpec::new(CardKind::Authorize, body),
        Hold::Plan => CardSpec::new(CardKind::Review, "Review the plan").content(ContentKind::Plan, body),
        Hold::Question | Hold::Elicitation => CardSpec::new(CardKind::Provide, body),
    };
    DecisionPrompt::Card { body: body.into(), options, card }
}

fn request(epoch: u64, hold: Hold, tool: Option<&str>, input: serde_json::Value, mcp_server: Option<McpServerOrigin>, prompt: DecisionPrompt) -> Command {
    Command::RecordDecisionRequest { task_id: "t".into(), generation: 1, epoch, hold, tool: tool.map(Into::into), input, mcp_server, prompt }
}

fn bash(epoch: u64, command: &str) -> Command {
    request(epoch, Hold::Permission, Some("Bash"), json!({"command": command}), None, card_prompt(Hold::Permission, &format!("Run {command}")))
}

fn answer(card: &str, option: &str, note: Option<&str>) -> Command {
    let inputs = note.map(|note| BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text(note.into()))])).unwrap_or_default();
    Command::AnswerQuestion { attachments: vec![], interaction_id: card.into(), option_id: option.into(), inputs }
}

fn card_of(epoch: u64) -> String {
    decision_card_id(&"t".into(), 1, epoch).0
}

fn held(s: &DomainState) -> Vec<String> {
    replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap().cards().into_iter().map(|(card, _)| card.0.clone()).collect()
}

fn plan(checked: bool) -> String {
    if checked { "# Plan\n\n- [ ] Add the parser\n- [ ] Test it\n".into() } else { "# Plan\n\nAdd the parser and test it.".into() }
}

pub(super) fn user() -> Actor {
    Actor::User { user_id: "u".into() }
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

pub(super) fn last(s: &DomainState, task: &str) -> LifecycleRecord {
    s.lifecycle_journal[&TaskId::from(task)].last().unwrap().clone()
}
