//! The aggregate rules: for a task with a journal, nothing but a lifecycle decision moves its
//! state, node, start or ending; a cancellation or a completion ends only with its receipts; and
//! a DTO edited by hand is refused, never repaired.
use super::*;
use alondra_domain::Event;
use std::collections::{BTreeMap, BTreeSet};

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
    must(s, orchestrator(), Command::CreateTask {
        task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: agent.map(Into::into), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")), plan_items: None,
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
pub(super) fn kinds(s: &DomainState, task: &str) -> Vec<String> {
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

    must(&mut s, node("a"), Command::ConfirmCleanup { task_id: "t".into(), discard: Vec::new() });
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
        verdict: None, review_rounds: 0, wrap_up: WrapUp::default(), priority: Default::default(), ending: None, worktrees: Default::default(),
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
pub(super) fn sdk_turn(s: &mut DomainState, id: &str, node: &str) -> String {
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
    let ended = must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None });
    let created = &s.interactions[&InteractionId::from(card.as_str())];
    assert_eq!(created.card.need, "“Task t” stopped without delivering or asking: tell it how to go on");
    assert!(matches!(&created.payload, InteractionPayload::Question { options, answer: None }
        if options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>() == [CARRY_ON, CANCEL_TASK]));
    assert!(ended.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { interaction_id, .. } if interaction_id.0 == card)), "the user hears of it");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
    assert_eq!(s.tasks[&TaskId::from("t")].final_presentation, None, "a native result is not a delivery");
    assert!(!s.tasks[&TaskId::from("t")].over(), "nor the end of the task");
    assert_eq!(exec(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None }).unwrap_err().code,
        "conflict", "the same turn end does not ask twice");
    assert_eq!(s.interactions.keys().filter(|id| id.0.starts_with(TURN_ENDED_PREFIX)).count(), 1);
    assert_aggregate_rules(&s);
}

/// The screen of task `id` in `lang`.
fn turn_end_shown(s: &DomainState, id: &str, lang: crate::views::text::Lang) -> String {
    use crate::views::{Client, RenderContext, Transport, build, live::LiveSnapshot};
    let index = ReadIndex::build(s);
    let live = LiveSnapshot::default();
    let view = build(&format!("task:{id}"), RenderContext { domain: s, index: &index, live: &live, client: Client::Tui, lang,
        transport: Transport::Loopback, cursor: "test", rendered_at: "" }).unwrap();
    serde_json::to_string(&view).unwrap()
}

#[test]
fn a_turn_the_user_interrupted_ends_on_a_card_that_says_so() {
    use crate::views::text::Lang;
    let mut s = world();
    let card = sdk_turn(&mut s, "t", "a");
    must(&mut s, user(), Command::InterruptTurn { agent_id: "a".into() });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None });
    let asked = &s.interactions[&InteractionId::from(card.as_str())];
    assert_eq!(asked.card.need, "You interrupted “Task t”: tell it how to go on");
    assert_eq!(asked.card.why, INTERRUPTED_WHY);
    assert!(matches!(&asked.payload, InteractionPayload::Question { options, answer: None }
        if options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>() == [CARRY_ON, CANCEL_TASK]));
    let en = turn_end_shown(&s, "t", Lang::En);
    assert!(en.contains("You interrupted “Task t”: tell it how to go on"), "{en}");
    assert!(en.contains("You stopped its turn. Tell it to carry on, write what to do, or cancel the task."), "{en}");
    assert!(!en.contains("stopped without delivering"), "{en}");
    let es = turn_end_shown(&s, "t", Lang::Es);
    assert!(es.contains("Lo interrumpiste: dile a «Task t» cómo seguir"), "{es}");
    assert!(es.contains("Paraste su turno. Dile que siga, escríbele qué hacer o cancela la tarea."), "{es}");
    assert!(!es.contains("paró sin entregar"), "{es}");
    assert_aggregate_rules(&s);
}

#[test]
fn a_turn_that_ends_on_its_own_says_it_stopped_without_delivering_or_asking() {
    use crate::views::text::Lang;
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None });
    let en = turn_end_shown(&s, "t", Lang::En);
    assert!(en.contains("“Task t” stopped without delivering or asking: tell it how to go on"), "{en}");
    assert!(en.contains("Its turn ended with nothing that explains its next step."), "{en}");
    assert!(!en.contains("You interrupted"), "{en}");
    let es = turn_end_shown(&s, "t", Lang::Es);
    assert!(es.contains("«Task t» paró sin entregar ni preguntar: dile cómo seguir"), "{es}");
    assert!(es.contains("Su turno terminó sin nada que explique qué hace después."), "{es}");
    assert!(!es.contains("interrumpiste"), "{es}");
}

#[test]
fn interrupting_a_turn_does_not_change_how_a_later_turn_ends() {
    let mut s = world();
    let first = sdk_turn(&mut s, "t", "a");
    must(&mut s, user(), Command::InterruptTurn { agent_id: "a".into() });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None });
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: first.as_str().into(), option_id: CARRY_ON.into(), inputs: BTreeMap::new() });
    let Some(LifecycleRecord::Answered { input, .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("answered") };
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: "turn-2".into(), input },
    ] });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-2".into(), failure: None });
    let later = &s.interactions[&turn_ended_card_id(&"t".into(), 1, "turn-2")];
    assert_eq!(later.card.need, "“Task t” stopped without delivering or asking: tell it how to go on");
    assert_ne!(later.card.why, INTERRUPTED_WHY);
    assert_aggregate_rules(&s);
}

// The native Claude names its turns `claude-message:<uuid>`; the card is still one a client can
// open and answer, whose view key and actions the views' grammar takes.
#[test]
fn a_native_turn_ends_with_a_card_clients_can_name() {
    let mut s = world();
    let turn = "claude-message:3272f577-e432-452d-9331-2bb86fc8686c";
    sdk_turn_named(&mut s, "t", "a", turn);
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: turn.into(), failure: None });
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
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None });
    assert!(!s.interactions.keys().any(|id| id.0.starts_with(TURN_ENDED_PREFIX)), "its own work explains the next step");
    assert!(holds_slot(&s, "t"), "the work still running keeps the slot");
    assert_eq!(kinds(&s, "t").last().map(String::as_str), Some("turn_ended"), "no work end is made up");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
    assert_aggregate_rules(&s);
}

// Claude Code tracks a foreground command that runs for seconds as its own work. It ends inside
// the turn that waits for it, which goes on with it and then rests on its card.
#[test]
fn work_that_ends_inside_its_turn_is_continued_by_it_and_the_turn_ends_on_one_card() {
    let mut s = world();
    let card = sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkStarted { turn: "turn-1".into(), work: "claude-task:bash".into() },
        RuntimeObservation::WorkEnded { work: "claude-task:bash".into(), continuation: "claude-notification:bash".into() },
        RuntimeObservation::WorkContinued { continuation: "claude-notification:bash".into() },
    ] });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None });
    assert!(s.interactions.contains_key(&InteractionId::from(card.as_str())), "the turn asks how to go on");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
    assert_eq!(kinds(&s, "t")[4..], ["work_started", "work_ended", "work_continued", "turn_ended"]);
    assert_eq!(exec(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkContinued { continuation: "decision-1-7".into() },
    ] }).unwrap_err().code, "invalid_input", "a decision's continuation is the domain's to consume");
    assert_aggregate_rules(&s);
}

// A background subagent that ends after its turn wakes Claude, which takes it up in a turn of its
// own: that turn, not the one the subagent outlived, rests on the task's one card.
#[test]
fn work_that_ends_after_its_turn_wakes_a_turn_of_its_own_that_ends_on_one_card() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkStarted { turn: "turn-1".into(), work: "claude-task:agent".into() },
    ] });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None });
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkEnded { work: "claude-task:agent".into(), continuation: "claude-notification:agent".into() },
    ] });
    assert!(holds_slot(&s, "t"), "the continuation Claude still owes keeps the slot");
    assert_eq!(exec(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnResumed { turn: "turn-2".into(), continuation: "claude-notification:other".into() },
    ] }).unwrap_err().code, "conflict", "a turn takes up only a continuation the task owes");
    assert_eq!(exec(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnResumed { turn: "turn-2".into(), continuation: "decision-1-7".into() },
    ] }).unwrap_err().code, "invalid_input", "a decision's continuation is the domain's to consume");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnResumed { turn: "turn-2".into(), continuation: "claude-notification:agent".into() },
    ] });
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-2".into(), failure: None });
    assert_eq!(s.interactions.keys().filter(|id| id.0.starts_with(TURN_ENDED_PREFIX)).map(|id| id.0.as_str()).collect::<Vec<_>>(),
        [format!("{TURN_ENDED_PREFIX}t-1-turn-2")], "one card, from the turn that rested");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
    assert_eq!(kinds(&s, "t")[4..], ["work_started", "turn_ended", "work_ended", "turn_resumed", "turn_ended"]);
    let journal = s.lifecycle_journal[&TaskId::from("t")].clone();
    assert!(replay(&TaskId::from("t"), &journal).is_ok(), "the journal replays");
    assert_aggregate_rules(&s);
}

// The user writes to the node instead of answering the card its ended turn left: that card asked
// how the task goes on, and it is answered, so the delivery that follows is not held behind it.
#[test]
fn telling_a_resting_node_how_to_go_on_retires_the_card_that_asked_it() {
    let mut s = world();
    let card = sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None });
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Deliver the parser split".into() });
    assert!(s.interactions[&InteractionId::from(card.as_str())].withdrawn, "the card no longer waits on the user");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
    assert_aggregate_rules(&s);
}

#[test]
fn carrying_on_tells_the_task_how_to_go_on_and_cancelling_ends_it() {
    let mut s = world();
    let card = sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None });
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.as_str().into(), option_id: CARRY_ON.into(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text("Add the tests first".into()))]) });
    let Some(LifecycleRecord::Answered { text: Some(text), .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("answered") };
    assert!(text.starts_with("Go on with your task") && text.ends_with("Add the tests first"), "{text}");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
    assert_aggregate_rules(&s);

    let mut s = world();
    let card = sdk_turn(&mut s, "u", "b");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "u".into(), generation: 1, turn: "turn-1".into(), failure: None });
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.as_str().into(), option_id: CANCEL_TASK.into(), inputs: BTreeMap::new() });
    assert_eq!(lifecycle(&s, "u").0, TaskState::Closing);
    assert_eq!(lifecycle(&s, "u").3, Some(TaskEnding::Cancelling));
    assert_aggregate_rules(&s);
}

fn fail_turn(s: &mut DomainState, id: &str, turn: &str, error: &str, status: Option<u16>) -> CommandOutcome {
    must(s, Actor::System, Command::RecordTurnBoundary {
        task_id: id.into(), generation: 1, turn: turn.into(),
        failure: Some(SdkApiFailure { error: error.into(), status, resets_at: None }),
    })
}
fn sign_in_cards(s: &DomainState) -> Vec<&Interaction> {
    s.interactions.values().filter(|i| i.id.0.starts_with(SIGNED_OUT_PREFIX) && s.open(i)).collect()
}
/// The cards of Claude's holds Waiting for You lists.
fn listed_holds(s: &DomainState) -> Vec<String> {
    super::read_index::inbox_items(s).into_iter().filter(|item| item.pending).map(|item| {
        let InboxSubject::Interaction { interaction_id } = item.subject;
        interaction_id.0
    }).filter(|id| ClaudeHold::of(&InteractionId::from(id.as_str())).is_some()).collect()
}
fn notices(outcome: &CommandOutcome) -> usize {
    outcome.effects.iter().filter(|e| matches!(e, DomainEffect::NotifyUser { .. })).count()
}
fn held(s: &DomainState) -> Vec<(String, ClaudeHold)> {
    s.claude_held().into_iter().map(|held| (held.task.0, held.hold)).collect()
}
/// The task goes on with the input its resume gave it: a new turn starts on it.
fn turn_on_resumed(s: &mut DomainState, id: &str, turn: &str) {
    let Some(LifecycleRecord::Resumed { input, .. }) = s.lifecycle_journal[&TaskId::from(id)].last().cloned() else { panic!("{id} was not resumed") };
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: id.into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: turn.into(), input },
    ] });
}

// 19 Oct: Claude's sign-in expired and every node looked like an API outage. A logout is a fact
// of its own: every task waits on a card of its own, as the store requires, and Waiting for You
// lists them as one, announced once; never the generic card that tells a task to go on.
#[test]
fn tasks_signed_out_of_claude_wait_on_cards_listed_and_announced_as_one() {
    let mut s = world();
    let generic_t = sdk_turn(&mut s, "t", "a");
    let generic_u = sdk_turn(&mut s, "u", "b");
    let first = fail_turn(&mut s, "t", "turn-1", "authentication_failed", Some(401));
    let second = fail_turn(&mut s, "u", "turn-1", "authentication_failed", None);
    assert_eq!(sign_in_cards(&s).len(), 2, "each task waits on a card of its own");
    assert_eq!(sign_in_cards(&s)[0].card.need, "Sign in to Claude outside Alondra");
    assert_eq!((notices(&first), notices(&second)), (1, 0), "announced once");
    assert_eq!(listed_holds(&s).len(), 1, "Waiting for You lists one: {:?}", listed_holds(&s));
    for generic in [generic_t, generic_u] {
        assert!(!s.interactions.contains_key(&InteractionId::from(generic.as_str())), "no generic card");
    }
    assert_eq!(held(&s), [("t".into(), ClaudeHold::SignedOut), ("u".into(), ClaudeHold::SignedOut)]);
    assert_aggregate_rules(&s);
}

#[test]
fn signing_in_probes_one_task_and_the_model_answering_lets_every_held_task_go_on() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    sdk_turn(&mut s, "u", "b");
    fail_turn(&mut s, "t", "turn-1", "authentication_failed", Some(401));
    fail_turn(&mut s, "u", "turn-1", "authentication_failed", Some(401));
    let listed = listed_holds(&s).pop().unwrap();
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: listed.as_str().into(), option_id: SIGNED_IN.into(), inputs: BTreeMap::new() });
    assert_eq!(held(&s), [("u".into(), ClaudeHold::SignedOut)], "one task probes");
    assert_eq!(kinds(&s, "t").last().map(String::as_str), Some("resumed"));
    assert_eq!(listed_holds(&s), Vec::<String>::new(), "nothing to press while the probe runs");
    turn_on_resumed(&mut s, "t", "turn-2");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![RuntimeObservation::ModelAnswered] });
    assert_eq!(held(&s), [], "Claude answered: every held task goes on");
    assert_eq!(kinds(&s, "u").last().map(String::as_str), Some("resumed"));
    assert!(sign_in_cards(&s).is_empty(), "no card is left open");
    assert_aggregate_rules(&s);
}

#[test]
fn a_probe_that_fails_again_lists_the_one_card_again() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    sdk_turn(&mut s, "u", "b");
    fail_turn(&mut s, "t", "turn-1", "authentication_failed", Some(401));
    fail_turn(&mut s, "u", "turn-1", "authentication_failed", Some(401));
    let listed = listed_holds(&s).pop().unwrap();
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: listed.as_str().into(), option_id: SIGNED_IN.into(), inputs: BTreeMap::new() });
    turn_on_resumed(&mut s, "t", "turn-2");
    let again = fail_turn(&mut s, "t", "turn-2", "authentication_failed", Some(401));
    assert_eq!(notices(&again), 0, "the user already knows");
    assert_eq!(listed_holds(&s).len(), 1, "one card to press again");
    assert_eq!(held(&s), [("t".into(), ClaudeHold::SignedOut), ("u".into(), ClaudeHold::SignedOut)]);
    assert_aggregate_rules(&s);
}

#[test]
fn an_outage_or_the_usage_limit_is_listed_once_unannounced_and_a_probe_lets_exactly_one_task_go_on() {
    let mut s = world();
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: "c".into(), name: "c".into(), at: 0 });
    sdk_turn(&mut s, "t", "a");
    sdk_turn(&mut s, "u", "b");
    sdk_turn(&mut s, "v", "c");
    let outage = fail_turn(&mut s, "t", "turn-1", "overloaded", Some(529));
    fail_turn(&mut s, "u", "turn-1", "server_error", Some(500));
    let usage = must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "v".into(), generation: 1, turn: "turn-1".into(),
        failure: Some(SdkApiFailure { error: "rate_limit".into(), status: Some(429), resets_at: Some(4_102_444_800) }) });
    assert_eq!((notices(&outage), notices(&usage)), (0, 0), "never announced");
    assert_eq!(listed_holds(&s).len(), 2, "one card for the outage, one for the limit: {:?}", listed_holds(&s));
    assert_eq!(held(&s), [("t".into(), ClaudeHold::Outage), ("u".into(), ClaudeHold::Outage), ("v".into(), ClaudeHold::Usage)]);
    let limit = s.claude_held().into_iter().find(|held| held.hold == ClaudeHold::Usage).unwrap();
    assert_eq!(usage_resets_at(&limit.card), Some(4_102_444_800), "the reset Claude gave");
    assert_eq!(exec(&mut s, Actor::System, Command::ProbeClaudeAccess { hold: ClaudeHold::SignedOut }).unwrap_err().code, "invalid_input",
        "only the user's answer probes the sign-in");
    assert_eq!(exec(&mut s, user(), Command::ProbeClaudeAccess { hold: ClaudeHold::Outage }).unwrap_err().code, "forbidden");
    must(&mut s, Actor::System, Command::ProbeClaudeAccess { hold: ClaudeHold::Outage });
    assert_eq!(held(&s), [("u".into(), ClaudeHold::Outage), ("v".into(), ClaudeHold::Usage)], "one probe, the oldest");
    must(&mut s, Actor::System, Command::ProbeClaudeAccess { hold: ClaudeHold::Usage });
    assert_eq!(held(&s), [("u".into(), ClaudeHold::Outage)]);
    assert_eq!(exec(&mut s, Actor::System, Command::ProbeClaudeAccess { hold: ClaudeHold::Usage }).unwrap_err().code, "not_found");
    assert_aggregate_rules(&s);
}

#[test]
fn a_refusal_signing_in_cannot_fix_asks_on_the_tasks_own_card_with_its_reason() {
    for (error, status, reason) in [
        ("authentication_failed", Some(403), "the Claude account refuses this request"),
        ("billing_error", Some(400), "the Claude account has a billing problem"),
    ] {
        let mut s = world();
        let card = sdk_turn(&mut s, "t", "a");
        fail_turn(&mut s, "t", "turn-1", error, status);
        let asked = &s.interactions[&InteractionId::from(card.as_str())];
        assert_eq!(asked.card.need, format!("“Task t” stopped: {reason}"));
        assert_eq!(asked.card.why, REFUSED_WHY);
        assert!(sign_in_cards(&s).is_empty(), "{error} {status:?} is not a logout");
        assert_eq!(listed_holds(&s), Vec::<String>::new());
        assert_eq!(held(&s), []);
        assert_aggregate_rules(&s);
    }
}

// What David sees with two tasks signed out: one card in Waiting for You, which counts them, and
// the task it does not show reads what it waits for.
#[test]
fn the_screens_show_one_sign_in_card_and_name_the_wait_of_the_other_task() {
    use crate::views::{Client, RenderContext, Transport, build, live::LiveSnapshot, text::Lang};
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    sdk_turn(&mut s, "u", "b");
    fail_turn(&mut s, "t", "turn-1", "authentication_failed", Some(401));
    fail_turn(&mut s, "u", "turn-1", "authentication_failed", Some(401));
    let index = ReadIndex::build(&s);
    let live = LiveSnapshot::default();
    let render = |key: &str| {
        let view = build(key, RenderContext { domain: &s, index: &index, live: &live, client: Client::Tui, lang: Lang::Es,
            transport: Transport::Loopback, cursor: "test", rendered_at: "" }).unwrap();
        serde_json::to_string(&view).unwrap()
    };
    let home = render("home");
    assert!(home.contains("\"id\":\"waiting\",\"title\":\"Te espera\",\"count\":1,"), "one card in Waiting for You: {home}");
    assert!(home.contains("La sesión de Claude Code caducó y esperan 2 tareas."), "{home}");
    let other = render("task:u");
    assert!(other.contains("Espera a que inicies sesión en Claude"), "{other}");
}

fn options(s: &DomainState, card: &InteractionId) -> Vec<String> {
    let InteractionPayload::Question { options, .. } = &s.interactions[card].payload else { panic!("a question") };
    options.iter().map(|option| option.id.clone()).collect()
}
fn answer(s: &mut DomainState, card: &InteractionId, option: &str) {
    must(s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.clone(), option_id: option.into(), inputs: BTreeMap::new() });
}

#[test]
fn a_host_that_ends_mid_turn_asks_once_how_the_task_goes_on() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let ended = must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1 });
    let card = s.interactions.keys().find(|id| id.0.starts_with(SESSION_ENDED_PREFIX)).expect("the interrupted task asks").clone();
    assert_eq!(s.interactions[&card].card.need, "“Task t” was interrupted when its session closed: tell it how to go on");
    assert_eq!(options(&s, &card), [CARRY_ON, CANCEL_TASK]);
    assert!(ended.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { interaction_id, .. } if *interaction_id == card)));
    assert!(!holds_slot(&s, "t"), "nothing of the ended host runs");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
    answer(&mut s, &card, CARRY_ON);
    assert!(matches!(s.lifecycle_journal[&TaskId::from("t")].last(), Some(LifecycleRecord::Answered { .. })), "the answer goes to the resumed session");
    assert_aggregate_rules(&s);
}

#[test]
fn a_host_that_ends_while_its_task_waits_on_the_user_adds_no_card() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None });
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1 });
    assert!(!s.interactions.keys().any(|id| id.0.starts_with(SESSION_ENDED_PREFIX)), "its turn card already asks");
    assert_eq!(kinds(&s, "t").last().map(String::as_str), Some("host_ended"));
    assert_aggregate_rules(&s);
}

/// A task whose host ended while the user's answer to its turn card was handed and not yet read.
fn doubtful(s: &mut DomainState, written: &str) -> (InteractionId, String) {
    let turn = sdk_turn(s, "t", "a");
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None });
    must(s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: turn.as_str().into(), option_id: CARRY_ON.into(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text(written.into()))]) });
    let Some(LifecycleRecord::Answered { input, text: Some(text), .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("answered") };
    must(s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1 });
    must(s, Actor::System, Command::RecordDoubtfulHandoff { task_id: "t".into(), generation: 1, input: input.clone(), text: text.clone() });
    let card = s.interactions.keys().find(|id| id.0.starts_with(HANDOFF_DOUBT_PREFIX)).expect("the doubt asks the user").clone();
    (card, text)
}

#[test]
fn a_message_the_ended_host_may_not_have_read_is_shown_whole_and_sent_again_only_on_the_users_word() {
    let mut s = world();
    let (card, text) = doubtful(&mut s, "Add the tests first");
    let doubt = &s.interactions[&card];
    assert_eq!(doubt.card.need, "“Task t” may not have got your message: send it again?");
    assert_eq!(doubt.card.content.as_ref().map(|content| content.body.as_str()), Some(text.as_str()), "the card carries the whole message");
    assert_eq!(options(&s, &card), [RESEND, TAKE_AS_RECEIVED]);
    assert_eq!(exec(&mut s, Actor::System, Command::RecordDoubtfulHandoff { task_id: "t".into(), generation: 1, input: "other".into(), text: "other".into() })
        .unwrap_err().code, "conflict", "an input that is not pending is never doubted");
    answer(&mut s, &card, RESEND);
    let Some(LifecycleRecord::HandoffSettled { resent: Some(resent), text: Some(again), .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("settled") };
    assert_eq!(again, text, "the same words go again");
    let replayed = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap();
    assert_eq!(replayed.pending_inputs(), [&alondra_domain::InputId::new(resent).unwrap()]);
    assert!(!s.interactions.keys().any(|id| id.0.starts_with(SESSION_ENDED_PREFIX)), "the message explains the next step");
    assert_aggregate_rules(&s);
}

#[test]
fn a_message_taken_as_received_is_not_sent_again_and_the_task_asks_how_to_go_on() {
    let mut s = world();
    let (card, _) = doubtful(&mut s, "Add the tests first");
    answer(&mut s, &card, TAKE_AS_RECEIVED);
    assert!(matches!(s.lifecycle_journal[&TaskId::from("t")].last(), Some(LifecycleRecord::HandoffSettled { resent: None, text: None, .. })));
    let replayed = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap();
    assert!(replayed.pending_inputs().is_empty(), "nothing is sent and nothing is recorded as read");
    let next = s.interactions.keys().find(|id| id.0.starts_with(SESSION_ENDED_PREFIX)).expect("the task asks how to go on").clone();
    assert_eq!(options(&s, &next), [CARRY_ON, CANCEL_TASK]);
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
    assert_aggregate_rules(&s);
}

#[test]
fn what_the_user_writes_on_the_card_of_a_doubtful_message_is_sent_in_its_place() {
    let mut s = world();
    let (card, _) = doubtful(&mut s, "Add the tests first");
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.clone(), option_id: TAKE_AS_RECEIVED.into(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text("Skip the tests, write the docs".into()))]) });
    assert!(matches!(s.lifecycle_journal[&TaskId::from("t")].last(),
        Some(LifecycleRecord::HandoffSettled { resent: Some(_), text: Some(text), .. }) if text == "Skip the tests, write the docs"));
    assert!(!s.interactions.keys().any(|id| id.0.starts_with(SESSION_ENDED_PREFIX)), "what they wrote explains the next step");
    assert_aggregate_rules(&s);
}

pub(super) fn last(s: &DomainState, task: &str) -> LifecycleRecord {
    s.lifecycle_journal[&TaskId::from(task)].last().unwrap().clone()
}
pub(super) fn refused_unchanged(s: &mut DomainState, actor: Actor, command: Command) -> String {
    let before = s.clone();
    let code = exec(s, actor, command).unwrap_err().code;
    assert_eq!(*s, before, "a refused command changed the state");
    code
}

#[test]
fn the_user_interrupts_exactly_what_the_nodes_sdk_task_runs() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let interrupt = |agent: &str| Command::InterruptTurn { agent_id: agent.into() };
    assert_eq!(refused_unchanged(&mut s, node("a"), interrupt("a")), "forbidden");
    assert_eq!(refused_unchanged(&mut s, orchestrator(), interrupt("a")), "forbidden");
    assert_eq!(refused_unchanged(&mut s, user(), interrupt("ghost")), "not_found");
    assert_eq!(refused_unchanged(&mut s, user(), interrupt("b")), "no_task", "b runs nothing");
    let revision = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap().revision();
    let state = s.tasks[&TaskId::from("t")].state;
    must(&mut s, user(), interrupt("a"));
    assert_eq!(last(&s, "t"), LifecycleRecord::ControlRequested {
        control: format!("control-t-{}", revision + 1),
        operation: ControlRecord::Interrupt { turn: Some("turn-1".into()) },
    });
    assert_eq!(s.tasks[&TaskId::from("t")].state, state, "the request alone stops nothing");
    must(&mut s, Actor::System, interrupt("a"));
    assert_aggregate_rules(&s);
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None });
    assert_eq!(refused_unchanged(&mut s, user(), interrupt("a")), "conflict", "nothing runs to interrupt");

    let mut s = world();
    task(&mut s, "u", Some("b"));
    must(&mut s, node("b"), Command::StartTask { task_id: "u".into() });
    assert_eq!(refused_unchanged(&mut s, user(), interrupt("b")), "no_task", "a claude --bg node is not interrupted through the SDK");
}

#[test]
fn interrupting_the_work_a_task_runs_on_its_own_names_no_turn() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkStarted { turn: "turn-1".into(), work: "subagent".into() },
    ] });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None });
    must(&mut s, user(), Command::InterruptTurn { agent_id: "a".into() });
    assert!(matches!(last(&s, "t"), LifecycleRecord::ControlRequested { operation: ControlRecord::Interrupt { turn: None }, .. }));
    assert_aggregate_rules(&s);
}

#[test]
fn only_alondra_sets_the_model_and_mode_or_reads_mcp_status_of_an_sdk_task() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let model = |model: &str| Command::SetModel { task_id: "t".into(), model: model.into() };
    assert_eq!(refused_unchanged(&mut s, user(), model("claude-opus")), "forbidden");
    assert_eq!(refused_unchanged(&mut s, node("a"), model("claude-opus")), "forbidden");
    for invalid in ["", "  ", "two\nlines"] {
        assert_eq!(refused_unchanged(&mut s, Actor::System, model(invalid)), "conflict", "{invalid:?}");
    }
    must(&mut s, Actor::System, model("claude-opus"));
    assert!(matches!(last(&s, "t"), LifecycleRecord::ControlRequested { operation: ControlRecord::SetModel { model }, .. } if model == "claude-opus"));
    must(&mut s, Actor::System, Command::SetPermissionMode { task_id: "t".into(), mode: PermissionModeRecord::DontAsk });
    assert!(matches!(last(&s, "t"), LifecycleRecord::ControlRequested { operation: ControlRecord::SetPermissionMode { mode: PermissionModeRecord::DontAsk }, .. }));
    must(&mut s, Actor::System, Command::ReadMcpStatus { task_id: "t".into() });
    assert!(matches!(last(&s, "t"), LifecycleRecord::ControlRequested { operation: ControlRecord::ReadMcpStatus, .. }));
    assert_aggregate_rules(&s);
    assert_eq!(refused_unchanged(&mut s, Actor::System, Command::ReadMcpStatus { task_id: "missing".into() }), "not_found");
    task(&mut s, "u", Some("b"));
    assert_eq!(refused_unchanged(&mut s, Actor::System, Command::ReadMcpStatus { task_id: "u".into() }), "no_task");
    for mode in ["acceptEdits", "accept_edits", "auto", "bypass_permissions", "bypassPermissions"] {
        let wire = format!(r#"{{"kind":"set_permission_mode","task_id":"t","mode":"{mode}"}}"#);
        assert!(serde_json::from_str::<Command>(&wire).is_err(), "{mode} cannot be expressed");
    }
    assert!(serde_json::from_str::<Command>(r#"{"kind":"set_permission_mode","task_id":"t","mode":"dont_ask"}"#).is_ok());
}

#[test]
fn compacting_a_resting_conversation_keeps_the_card_it_waits_on() {
    let mut s = world();
    let card = sdk_turn(&mut s, "t", "a");
    let compact = || Command::CompactConversation { task_id: "t".into() };
    assert_eq!(refused_unchanged(&mut s, Actor::System, compact()), "conflict", "a turn runs");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None });
    assert_eq!(refused_unchanged(&mut s, user(), compact()), "forbidden");
    must(&mut s, Actor::System, compact());
    let LifecycleRecord::Compacted { input } = last(&s, "t") else { panic!("compacted") };
    let id = TaskId::from("t");
    assert_eq!(writes_texts(&id, &s.lifecycle_journal[&id]).get(&alondra_domain::InputId::new(input.as_str()).unwrap()).map(String::as_str), Some(COMPACT));
    let lifecycle = replay(&id, &s.lifecycle_journal[&id]).unwrap();
    assert_eq!(lifecycle.cards().iter().map(|(card, _)| card.0.clone()).collect::<Vec<_>>(), vec![card.clone()], "the card it waits on stays");
    assert!(matches!(&s.interactions[&InteractionId::from(card.as_str())].payload, InteractionPayload::Question { answer: None, .. }));
    assert_eq!(refused_unchanged(&mut s, Actor::System, compact()), "conflict", "its compaction is already pending");
    assert_aggregate_rules(&s);
}
