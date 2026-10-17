//! The aggregate rules: for a task with a journal, nothing but a lifecycle decision moves its
//! state, node, start or ending; a cancellation or a completion ends only with its receipts; and
//! a DTO edited by hand is refused, never repaired.
use super::*;
use alondra_domain::Event;
use std::collections::{BTreeMap, BTreeSet};

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}
fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}
fn node(id: &str) -> Actor {
    Actor::Agent { agent_id: id.into() }
}
fn exec(s: &mut DomainState, actor: Actor, command: Command) -> Result<CommandOutcome, DomainError> {
    s.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision, command,
    })
}
fn must(s: &mut DomainState, actor: Actor, command: Command) -> CommandOutcome {
    let shown = format!("{command:?}");
    match exec(s, actor, command) {
        Ok(outcome) => outcome,
        Err(error) => panic!("{shown} was refused: {error}"),
    }
}
fn task(s: &mut DomainState, id: &str, agent: Option<&str>) {
    must(s, orchestrator(), Command::CreateTask {
        task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: agent.map(Into::into), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")), plan_items: None,
    });
}
fn world() -> DomainState {
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
fn kinds(s: &DomainState, task: &str) -> Vec<String> {
    s.lifecycle_journal[&TaskId::from(task)].iter().map(|record| {
        let value = serde_json::to_value(record).unwrap();
        value["kind"].as_str().unwrap().to_owned()
    }).collect()
}
fn lifecycle(s: &DomainState, task: &str) -> (TaskState, bool, Option<AgentId>, Option<TaskEnding>) {
    let t = &s.tasks[&TaskId::from(task)];
    (t.state, t.started, t.agent_id.clone(), t.ending)
}
fn deliver(s: &mut DomainState, task: &str, agent: &str, id: &str) {
    must(s, node(agent), Command::PresentResult {
        attachments: vec![], task_id: task.into(), interaction_id: id.into(), body: "Done".into(), is_final: true, nothing_to_review: false,
        targets: vec![DeliveryTarget::Text { text: "Done".into() }], checklist: vec![],
        card: CardSpec::delivery("Review the result", &[], true),
    });
}

/// The texts the store keeps for a journal's inputs.
fn writes_texts(id: &TaskId, records: &[LifecycleRecord]) -> BTreeMap<alondra_domain::InputId, String> {
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
    again.settle_nodes();
    for (id, task) in &s.tasks {
        assert_eq!(&again.tasks[id], task, "derive_states moved the task {id} without a decision");
    }
    assert_eq!(again.agents, s.agents, "settle_nodes moved a node without a decision");
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

#[test]
fn cancelling_a_task_without_a_node_ends_only_with_the_cleanup_receipt() {
    let mut s = world();
    task(&mut s, "t", None);
    assert_eq!(kinds(&s, "t"), ["planned"]);
    assert_eq!(lifecycle(&s, "t"), (TaskState::NotStarted, false, None, None));

    must(&mut s, user(), Command::CancelTask { task_id: "t".into() });
    assert_eq!(lifecycle(&s, "t"), (TaskState::Closing, false, None, Some(TaskEnding::Cancelling)));
    assert_eq!(kinds(&s, "t"), ["planned", "finished"]);
    let t = &s.tasks[&"t".into()];
    assert!(t.over() && t.cancelled() && !t.state.terminal(), "over for the fleet, not yet terminal");
    assert_aggregate_rules(&s);

    let refused = |s: &mut DomainState, actor: Actor, command: Command| exec(s, actor, command).unwrap_err().code;
    assert_eq!(refused(&mut s, orchestrator(), Command::AssignTask { task_id: "t".into(), agent_id: "a".into(), model: None }), "task_ending");
    assert_eq!(refused(&mut s, user(), Command::ReopenTask { task_id: "t".into() }), "task_not_active");
    assert_eq!(refused(&mut s, user(), Command::ArchiveTask { task_id: "t".into(), archived: true }), "task_not_active");
    assert_eq!(refused(&mut s, user(), Command::CancelTask { task_id: "t".into() }), "task_not_active");
    assert_eq!(refused(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: None }), "not_closing", "no node ended");

    must(&mut s, Actor::System, Command::RecordTaskCleanup { task_id: "t".into() });
    assert_eq!(lifecycle(&s, "t"), (TaskState::Cancelled, false, None, None));
    assert_eq!(kinds(&s, "t"), ["planned", "finished", "drained"]);
    assert_eq!(refused(&mut s, Actor::System, Command::RecordTaskCleanup { task_id: "t".into() }), "not_closing", "the receipt is one");
    assert_aggregate_rules(&s);

    must(&mut s, user(), Command::ReopenTask { task_id: "t".into() });
    assert_eq!(lifecycle(&s, "t"), (TaskState::NotStarted, false, None, None));
    assert!(!s.tasks[&"t".into()].started_without_node());
    assert_eq!(kinds(&s, "t").last().map(String::as_str), Some("reopened"));
    assert_aggregate_rules(&s);
}

#[test]
fn cancelling_a_task_with_a_node_is_cancelled_only_with_the_node_end_receipt() {
    let mut s = world();
    task(&mut s, "t", Some("a"));
    assert_eq!(lifecycle(&s, "t"), (TaskState::NotStarted, false, Some("a".into()), None));
    assert_eq!(kinds(&s, "t"), ["planned", "admitted"]);
    must(&mut s, node("a"), Command::StartTask { task_id: "t".into() });
    assert_eq!(lifecycle(&s, "t"), (TaskState::Running, true, Some("a".into()), None));
    assert_eq!(kinds(&s, "t"), ["planned", "admitted", "resource_available", "turn_started"]);
    must(&mut s, Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::TurnStarted, at: 1 });
    assert!(s.slots().iter().any(|agent| agent.id.0 == "a"));
    assert_aggregate_rules(&s);

    let out = must(&mut s, user(), Command::CancelTask { task_id: "t".into() });
    assert_eq!(lifecycle(&s, "t"), (TaskState::Closing, true, Some("a".into()), Some(TaskEnding::Cancelling)));
    assert!(s.tasks[&"t".into()].wrap_up.close_cancelled);
    assert!(out.effects.iter().any(|e| matches!(e, DomainEffect::NotifyAgent { agent_id, .. } if agent_id.0 == "a")), "the node is told");
    assert_aggregate_rules(&s);

    assert_eq!(exec(&mut s, Actor::System, Command::RecordTaskCleanup { task_id: "t".into() }).unwrap_err().code, "conflict", "the node's end is the receipt");
    assert_eq!(exec(&mut s, Actor::System, Command::ReleaseTask { task_id: "t".into(), reason: "closed".into() }).unwrap_err().code, "task_not_active");

    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: Some("a".into()) });
    assert_eq!(lifecycle(&s, "t"), (TaskState::Cancelled, true, Some("a".into()), None));
    let tail: Vec<String> = kinds(&s, "t").into_iter().rev().take(3).collect();
    assert_eq!(tail, ["drained", "drained", "finished"]);
    assert_aggregate_rules(&s);
}

#[test]
fn a_task_completes_only_once_its_cleanup_and_its_node_are_drained() {
    let mut s = world();
    task(&mut s, "t", Some("a"));
    must(&mut s, node("a"), Command::StartTask { task_id: "t".into() });
    deliver(&mut s, "t", "a", "d1");
    assert_eq!(lifecycle(&s, "t"), (TaskState::ForYourReview, true, Some("a".into()), None));
    assert_eq!(kinds(&s, "t").last().map(String::as_str), Some("presented"));
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "d1".into(), targets: vec![DeliveryTarget::Text { text: "Done".into() }] });
    assert_eq!(lifecycle(&s, "t"), (TaskState::Closing, true, Some("a".into()), None), "accepted and closing, revocable until its node is over");
    assert!(s.tasks[&"t".into()].wrap_up.asked_at.is_some(), "the node is asked to clean up");
    assert_eq!(kinds(&s, "t").last().map(String::as_str), Some("presented"), "no acceptance is recorded that feedback could undo");
    assert_aggregate_rules(&s);

    must(&mut s, node("a"), Command::ConfirmCleanup { task_id: "t".into() });
    assert_eq!(lifecycle(&s, "t").0, TaskState::Closing, "the node's session is still open");
    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: Some("a".into()) });
    assert_eq!(lifecycle(&s, "t"), (TaskState::Done, true, Some("a".into()), None));
    let tail: Vec<String> = kinds(&s, "t").into_iter().rev().take(4).collect();
    assert_eq!(tail, ["drained", "finished", "accepted", "execution_released"], "the node's end is the receipt: its execution goes, then acceptance, end and cleanup in one decision");
    assert_aggregate_rules(&s);
}

#[test]
fn a_delivered_task_whose_node_closed_is_done_as_soon_as_the_user_accepts() {
    let mut s = world();
    task(&mut s, "t", Some("a"));
    must(&mut s, node("a"), Command::StartTask { task_id: "t".into() });
    deliver(&mut s, "t", "a", "d1");
    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: Some("a".into()) });
    assert_eq!(lifecycle(&s, "t"), (TaskState::ForYourReview, false, None, None), "the delivery stands without its node");
    assert_eq!(kinds(&s, "t").last().map(String::as_str), Some("execution_released"));
    assert_aggregate_rules(&s);
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "d1".into(), targets: vec![DeliveryTarget::Text { text: "Done".into() }] });
    assert_eq!(lifecycle(&s, "t"), (TaskState::Done, false, None, None), "nothing is left to drain: done at once");
    let tail: Vec<String> = kinds(&s, "t").into_iter().rev().take(3).collect();
    assert_eq!(tail, ["drained", "finished", "accepted"]);
    assert_aggregate_rules(&s);
}

#[test]
fn a_withdrawn_delivery_hands_the_task_back_and_the_next_turn_consumes_the_input() {
    let mut s = world();
    task(&mut s, "t", Some("a"));
    must(&mut s, node("a"), Command::StartTask { task_id: "t".into() });
    deliver(&mut s, "t", "a", "d1");
    must(&mut s, node("a"), Command::Withdraw { interaction_id: "d1".into() });
    assert_eq!(lifecycle(&s, "t"), (TaskState::Running, true, Some("a".into()), None));
    assert_eq!(kinds(&s, "t").last().map(String::as_str), Some("handed_back"));
    assert_aggregate_rules(&s);
    must(&mut s, Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::TurnStarted, at: 1 });
    assert_eq!(kinds(&s, "t").last().map(String::as_str), Some("input_consumed"));
    deliver(&mut s, "t", "a", "d2");
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "d2".into(), targets: vec![DeliveryTarget::Text { text: "Done".into() }] });
    assert_eq!(lifecycle(&s, "t").0, TaskState::Closing);
    assert_aggregate_rules(&s);
}

#[test]
fn a_node_that_closes_on_an_open_task_releases_its_execution_and_a_new_node_is_admitted() {
    let mut s = world();
    task(&mut s, "t", Some("a"));
    must(&mut s, node("a"), Command::StartTask { task_id: "t".into() });
    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Gone, agent_id: Some("a".into()) });
    assert_eq!(lifecycle(&s, "t"), (TaskState::NotStarted, false, None, None));
    assert!(!s.tasks[&"t".into()].started_without_node());
    assert_aggregate_rules(&s);
    must(&mut s, Actor::System, Command::AssignTask { task_id: "t".into(), agent_id: "b".into(), model: None });
    assert_eq!(lifecycle(&s, "t"), (TaskState::NotStarted, false, Some("b".into()), None));
    must(&mut s, node("b"), Command::StartTask { task_id: "t".into() });
    assert_eq!(lifecycle(&s, "t"), (TaskState::Running, true, Some("b".into()), None));
    must(&mut s, Actor::System, Command::ReleaseTask { task_id: "t".into(), reason: "the user closed it".into() });
    assert_eq!(lifecycle(&s, "t"), (TaskState::NotStarted, false, None, None));
    assert_aggregate_rules(&s);
}

#[test]
fn a_dto_edited_by_hand_is_refused_and_never_repaired() {
    let mut s = world();
    task(&mut s, "t", Some("a"));
    let json = serde_json::to_string(&s).unwrap();
    let mut edited: DomainState = serde_json::from_str(&json).unwrap();
    assert_eq!(edited, s, "the journal survives the roundtrip as data");
    edited.tasks.get_mut(&"t".into()).unwrap().state = TaskState::Cancelled;
    let error = edited.validate_journal().unwrap_err();
    assert_eq!(error.code, "lifecycle_mismatch");
    assert!(error.message.contains("the aggregate rules"), "{}", error.message);
    assert_eq!(exec(&mut edited, user(), Command::RetitleTask { task_id: "t".into(), title: "Renamed".into() }).unwrap_err().code, "lifecycle_mismatch",
        "no command goes through on a task whose DTO contradicts its journal");

    let mut edited: DomainState = serde_json::from_str(&json).unwrap();
    edited.tasks.get_mut(&"t".into()).unwrap().agent_id = Some("b".into());
    assert_eq!(edited.validate_journal().unwrap_err().code, "lifecycle_mismatch");

    let mut edited: DomainState = serde_json::from_str(&json).unwrap();
    edited.lifecycle_journal.get_mut(&"t".into()).unwrap().push(LifecycleRecord::Reopened { input: "forged".into() });
    assert_eq!(edited.validate_journal().unwrap_err().code, "lifecycle_journal", "a record the aggregate refuses makes the journal unreadable");
}

#[test]
fn a_task_without_a_journal_is_refused_and_moves_nowhere() {
    let mut s = world();
    let unjournaled = Task {
        id: "old".into(), request_id: "r".into(), agent_id: Some("a".into()), title: "Old".into(), state: TaskState::Running, revision: 0,
        started: true, review_prs: false, final_presentation: None, pr_id: None, dependencies: BTreeSet::new(), stack: None,
        documents: BTreeMap::new(), archived: false, model: None, delivered_dependencies: Default::default(), blocks: Default::default(),
        verdict: None, review_rounds: 0, wrap_up: WrapUp::default(), priority: Default::default(), ending: None,
    };
    s.tasks.insert(unjournaled.id.clone(), unjournaled);
    assert_eq!(s.validate_journal().unwrap_err().code, "lifecycle_journal", "a loaded state with a task and no journal is refused");
    let before = s.clone();
    assert_eq!(exec(&mut s, user(), Command::CancelTask { task_id: "old".into() }).unwrap_err().code, "lifecycle_journal");
    assert_eq!(s, before, "no old writer cancels it");
}

#[test]
fn a_journal_whose_store_event_was_changed_cannot_be_read() {
    let mut s = world();
    task(&mut s, "t", Some("a"));
    must(&mut s, node("a"), Command::StartTask { task_id: "t".into() });
    let id = TaskId::from("t");
    let writes = journal_writes(&id, &s.lifecycle_journal[&id], 0).unwrap();
    let initial = writes[0].input.clone().unwrap();
    let mut events: Vec<_> = writes.into_iter().filter_map(|write| write.event).collect();
    let Event::Admit { node, conversation, host, .. } = events[0].clone() else { panic!("the first event admits the node") };
    events[0] = Event::Admit { node, conversation, host, instance: alondra_domain::InstanceId::new("another-instance").unwrap() };
    assert_eq!(records_from_events(&id, &initial, &events, &BTreeMap::new()).unwrap_err().code, "lifecycle_journal");
}

#[test]
fn an_orphan_reviewer_is_cancelled_through_its_lifecycle() {
    let mut s = world();
    task(&mut s, "work", Some("a"));
    must(&mut s, node("a"), Command::StartTask { task_id: "work".into() });
    task(&mut s, "review", Some("b"));
    must(&mut s, orchestrator(), Command::SetRelations { task_id: "review".into(), blocks: BTreeSet::from([TaskId::from("work")]), delivered_dependencies: BTreeSet::new() });
    must(&mut s, node("b"), Command::StartTask { task_id: "review".into() });
    must(&mut s, user(), Command::CancelTask { task_id: "work".into() });
    assert_eq!(lifecycle(&s, "review"), (TaskState::Closing, true, Some("b".into()), Some(TaskEnding::Cancelling)), "nothing is left to review");
    assert_eq!(kinds(&s, "review").last().map(String::as_str), Some("finished"));
    assert_aggregate_rules(&s);
}

#[test]
fn an_sdk_execution_is_admitted_by_its_own_command_and_records_only_its_own_facts() {
    let mut s = world();
    task(&mut s, "t", None);
    let admit = |host: &str| Command::AdmitExecution {
        task_id: "t".into(), agent_id: "a".into(), conversation: "conversation".into(), instance: "instance".into(), host: host.into(),
    };
    assert_eq!(exec(&mut s, Actor::System, admit(alondra_domain::CLAUDE_BG_HOST)).unwrap_err().code, "invalid_input", "the claude --bg node is assigned instead");
    assert_eq!(exec(&mut s, user(), admit("sdk-host")).unwrap_err().code, "forbidden");
    must(&mut s, Actor::System, admit("sdk-host"));
    assert_eq!(lifecycle(&s, "t"), (TaskState::NotStarted, false, Some("a".into()), None));
    let LifecycleRecord::Planned { input } = s.lifecycle_journal[&TaskId::from("t")][0].clone() else { panic!("planned first") };
    let facts = |generation: u64| Command::RecordRuntimeFacts {
        task_id: "t".into(), generation,
        facts: vec![RuntimeObservation::ResourceAvailable, RuntimeObservation::TurnStarted { turn: "turn-1".into(), input: input.clone() }],
    };
    let stale = facts(2);
    assert_eq!(exec(&mut s, Actor::System, stale).unwrap_err().code, "stale_execution");
    let current = facts(1);
    must(&mut s, Actor::System, current);
    assert_eq!(lifecycle(&s, "t"), (TaskState::Running, true, Some("a".into()), None), "the observed resource starts it");
    assert_eq!(kinds(&s, "t"), ["planned", "admitted", "resource_available", "turn_started"]);
    must(&mut s, node("a"), Command::StartTask { task_id: "t".into() });
    assert_eq!(kinds(&s, "t").len(), 4, "the node's start reports nothing the SDK did not observe");
    assert_aggregate_rules(&s);
}

/// An SDK task whose observed turn started: admitted, resource available, the planned input taken.
fn sdk_turn(s: &mut DomainState, id: &str, node: &str) -> String {
    sdk_turn_named(s, id, node, "turn-1")
}
fn sdk_turn_named(s: &mut DomainState, id: &str, node: &str, turn: &str) -> String {
    task(s, id, None);
    must(s, Actor::System, Command::AdmitExecution {
        task_id: id.into(), agent_id: node.into(), conversation: format!("conversation-{id}"), instance: format!("instance-{id}"), host: "sdk-host".into(),
    });
    let LifecycleRecord::Planned { input } = s.lifecycle_journal[&TaskId::from(id)][0].clone() else { panic!("planned first") };
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: id.into(), generation: 1, facts: vec![
        RuntimeObservation::ResourceAvailable,
        RuntimeObservation::TurnStarted { turn: turn.into(), input },
    ] });
    format!("{TURN_ENDED_PREFIX}{id}-1-{turn}")
}
fn holds_slot(s: &DomainState, id: &str) -> bool {
    replay(&id.into(), &s.lifecycle_journal[&TaskId::from(id)]).unwrap().holds_slot()
}

#[test]
fn a_turn_that_ends_with_nothing_to_explain_the_next_step_asks_the_user_once() {
    let mut s = world();
    let card = sdk_turn(&mut s, "t", "a");
    let ended = must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into() });
    let created = &s.interactions[&InteractionId::from(card.as_str())];
    assert_eq!(created.card.need, "“Task t” stopped without delivering or asking: tell it how to go on");
    assert!(matches!(&created.payload, InteractionPayload::Question { options, answer: None }
        if options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>() == [CARRY_ON, CANCEL_TASK]));
    assert!(ended.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { interaction_id, .. } if interaction_id.0 == card)), "the user hears of it");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
    assert_eq!(s.tasks[&TaskId::from("t")].final_presentation, None, "a native result is not a delivery");
    assert!(!s.tasks[&TaskId::from("t")].over(), "nor the end of the task");
    assert_eq!(exec(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into() }).unwrap_err().code,
        "conflict", "the same turn end does not ask twice");
    assert_eq!(s.interactions.keys().filter(|id| id.0.starts_with(TURN_ENDED_PREFIX)).count(), 1);
    assert_aggregate_rules(&s);
}

// The native Claude names its turns `claude-message:<uuid>`; the card is still one a client can
// open and answer, whose view key and actions the views' grammar takes.
#[test]
fn a_native_turn_ends_with_a_card_clients_can_name() {
    let mut s = world();
    let turn = "claude-message:3272f577-e432-452d-9331-2bb86fc8686c";
    sdk_turn_named(&mut s, "t", "a", turn);
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: turn.into() });
    let card = s.interactions.keys().find(|id| id.0.starts_with(TURN_ENDED_PREFIX)).unwrap().clone();
    assert_eq!(card.0, "turn-ended-t-1-claude-message-3272f577-e432-452d-9331-2bb86fc8686c");
    assert!(crate::views::id::valid(&card.0), "{card:?} is a valid view id");
    assert_aggregate_rules(&s);
}

#[test]
fn a_turn_that_ends_behind_work_of_its_own_asks_nothing_and_keeps_its_slot() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkStarted { turn: "turn-1".into(), work: "subagent".into() },
    ] });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into() });
    assert!(!s.interactions.keys().any(|id| id.0.starts_with(TURN_ENDED_PREFIX)), "its own work explains the next step");
    assert!(holds_slot(&s, "t"), "the work still running keeps the slot");
    assert_eq!(kinds(&s, "t").last().map(String::as_str), Some("turn_ended"), "no work end is made up");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
    assert_aggregate_rules(&s);
}

#[test]
fn carrying_on_tells_the_task_how_to_go_on_and_cancelling_ends_it() {
    let mut s = world();
    let card = sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into() });
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.as_str().into(), option_id: CARRY_ON.into(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text("Add the tests first".into()))]) });
    let Some(LifecycleRecord::Answered { text: Some(text), .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("answered") };
    assert!(text.starts_with("Go on with your task") && text.ends_with("Add the tests first"), "{text}");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
    assert_aggregate_rules(&s);

    let mut s = world();
    let card = sdk_turn(&mut s, "u", "b");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "u".into(), generation: 1, turn: "turn-1".into() });
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.as_str().into(), option_id: CANCEL_TASK.into(), inputs: BTreeMap::new() });
    assert_eq!(lifecycle(&s, "u").0, TaskState::Closing);
    assert_eq!(lifecycle(&s, "u").3, Some(TaskEnding::Cancelling));
    assert_aggregate_rules(&s);
}
