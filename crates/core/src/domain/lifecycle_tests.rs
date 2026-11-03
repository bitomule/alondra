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
pub(super) fn deliver(s: &mut DomainState, task: &str, agent: &str, id: &str) {
    must(s, node(agent), Command::PresentResult {
        attachments: vec![], task_id: task.into(), interaction_id: id.into(), body: "Done".into(), is_final: true, nothing_to_review: false,
        targets: vec![DeliveryTarget::Text { text: "Done".into() }], checklist: vec![],
        card: CardSpec::delivery("Review the result", &[], true),
    });
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
    task(&mut s, "other", None);
    assert_eq!(kinds(&s, "t"), ["planned"]);
    assert_eq!(lifecycle(&s, "t"), (TaskState::NotStarted, false, None, None));

    must(&mut s, user(), Command::CancelTask { task_id: "t".into() });
    assert_eq!(lifecycle(&s, "t"), (TaskState::Closing, false, None, Some(TaskEnding::Cancelling)));
    assert_eq!(kinds(&s, "t"), ["planned", "finished"]);
    let t = &s.tasks[&"t".into()];
    assert!(t.over() && t.cancelled() && !t.state.terminal(), "over for the queue, not yet terminal");
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
        verdict: None, review_rounds: 0, wrap_up: WrapUp::default(), priority: Default::default(), ending: None, stalls: 0, stall_closing: None, continues: None, worktrees: Default::default(), provider: Default::default(), selection: None, pending_selection: None, account: None, allowed: Default::default(), project: None,
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
    let admit = |host: &str| Command::AdmitExecution { provider: Default::default(),
        task_id: "t".into(), agent_id: "a".into(), conversation: "conversation".into(), instance: "instance".into(), host: host.into(), model: None, account: None, leaves_limit: None,
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
    sdk_turn_on(s, id, node, turn, None)
}
/// An SDK task whose observed turn started on `account`.
fn sdk_turn_on(s: &mut DomainState, id: &str, node: &str, turn: &str, account: Option<&str>) -> String {
    sdk_turn_on_provider(s,crate::accounts::Provider::Claude,id,node,turn,account)
}
fn sdk_turn_on_provider(s: &mut DomainState, provider: crate::accounts::Provider, id: &str, node: &str, turn: &str, account: Option<&str>) -> String {
    task(s, id, None);
    must(s, Actor::System, Command::AdmitExecution { provider,
        task_id: id.into(), agent_id: node.into(), conversation: format!("conversation-{id}"), instance: format!("instance-{id}"), host: "sdk-host".into(), model: None,
        account: account.map(|account| crate::accounts::AccountId::parse(account).unwrap()), leaves_limit: None,
    });
    let LifecycleRecord::Planned { input } = s.lifecycle_journal[&TaskId::from(id)][0].clone() else { panic!("planned first") };
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: id.into(), generation: 1, facts: vec![
        RuntimeObservation::ResourceAvailable,
        RuntimeObservation::TurnStarted { turn: turn.into(), input },
    ] });
    format!("{TURN_ENDED_PREFIX}{id}-1-{turn}")
}
/// Ends `turn` of the SDK task `id` with nothing that explains its next step, which wakes its
/// node, and starts `next`, the turn that wake opens: ending that one too opens the case for the
/// orchestrator whose id this returns.
pub(super) fn woken(s: &mut DomainState, id: &str, turn: &str, next: &str) -> String {
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: id.into(), generation: 1, turn: turn.into(), failure: None, failed: None });
    let LifecycleRecord::TurnEnded { wake: Some(input), text: Some(text), .. } = last(s, id) else { panic!("the node is woken: {:?}", last(s, id)) };
    assert_eq!(text, WAKE_INSTRUCTION);
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: id.into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: next.into(), input },
    ] });
    format!("{STALLED_PREFIX}{id}-1-{next}")
}
/// An SDK task whose `turn` is the one Alondra woke its node for after an empty turn before it:
/// ending it with nothing that explains the next step opens the case for the orchestrator whose
/// id this returns.
pub(super) fn sdk_woken_turn_named(s: &mut DomainState, id: &str, node: &str, turn: &str) -> String {
    sdk_turn_named(s, id, node, "turn-0");
    woken(s, id, "turn-0", turn)
}
pub(super) fn sdk_woken_turn(s: &mut DomainState, id: &str, node: &str) -> String {
    sdk_woken_turn_named(s, id, node, "turn-1")
}
/// An SDK task whose `turn-1` the user interrupted: its end asks the user how it goes on, on the
/// card whose id this returns.
pub(super) fn interrupted_turn(s: &mut DomainState, id: &str, node: &str) -> String {
    let card = sdk_turn(s, id, node);
    must(s, user(), Command::InterruptTurn { agent_id: node.into() });
    card
}
fn holds_slot(s: &DomainState, id: &str) -> bool {
    replay(&id.into(), &s.lifecycle_journal[&TaskId::from(id)]).unwrap().holds_slot()
}

#[test]
fn a_turn_that_ends_with_nothing_to_explain_the_next_step_goes_to_the_orchestrator_once() {
    let mut s = world();
    let case = sdk_woken_turn(&mut s, "t", "a");
    let ended = must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    let created = &s.interactions[&InteractionId::from(case.as_str())];
    assert!(is_stall_case(created), "{created:?}");
    assert!(matches!(&created.payload, InteractionPayload::Question { options, answer: None }
        if options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>() == [STALL_INSTRUCT, STALL_CLOSE]));
    assert!(ended.effects.iter().any(|e| matches!(e, DomainEffect::NotifyOrchestrator { interaction_id: Some(id), .. } if id.0 == case)), "the orchestrator hears of it");
    assert!(!ended.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { .. })), "the user never does");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running, "Alondra is on it, not the user");
    assert_eq!(s.tasks[&TaskId::from("t")].final_presentation, None, "a native result is not a delivery");
    assert!(!s.tasks[&TaskId::from("t")].over(), "nor the end of the task");
    assert_eq!(exec(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None }).unwrap_err().code,
        "conflict", "the same turn end does not open two cases");
    assert_eq!(s.interactions.keys().filter(|id| id.0.starts_with(STALLED_PREFIX)).count(), 1);
    assert!(!s.interactions.values().any(|i| i.recipient == Recipient::User), "no card for the user");
    assert_aggregate_rules(&s);
}

/// The node asked the user during its turn: the card that asks how it goes on waits out of sight
/// behind that question, and nobody hears of it. The answer told to the node ends its wait; a
/// question the node withdraws uncovers it, as nothing then explains the next step.
/// The node asked the user during its turn: the turn waits on that question, no card says it
/// stopped, and nobody hears of it. The answer told to the node ends its wait; a question the node
/// withdraws unanswered leaves the turn empty, so the node is woken.
#[test]
fn a_turn_that_ends_with_the_nodes_question_open_shows_only_that_question() {
    let mut s = world();
    let unheard = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    let ask = |s: &mut DomainState, id: &str| {
        let options = ["Short", "Long"].map(|label| QuestionOption { id: label.to_lowercase(), label: label.into(), description: None }).to_vec();
        must(s, node("a"), Command::AskQuestion { attachments: vec![], interaction_id: id.into(), scope: Scope::Task { task_id: "t".into() },
            recipient: Recipient::User, body: "Pick the name".into(), blocking: true, options, card: CardSpec::derived(CardKind::Decide, "Pick the name") });
    };
    let inbox = |s: &DomainState| super::read_index::inbox_items(s).into_iter().filter(|item| item.pending)
        .filter_map(|item| item.id.strip_prefix("interaction:").map(str::to_owned)).collect::<Vec<_>>();
    ask(&mut s, "q");
    let ended = must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    assert!(!s.interactions.contains_key(&unheard), "no card says it stopped");
    assert_eq!(s.lifecycle_of(&"t".into()).unwrap().awaits(), Some(&alondra_domain::Await::User("q".into())), "the turn waits on its question");
    assert!(!ended.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { interaction_id, .. } if *interaction_id == unheard)), "nobody hears of it");
    assert!(inbox(&s).contains(&"q".to_owned()) && !inbox(&s).contains(&unheard.0), "{:?}", inbox(&s));
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);

    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "q".into(), option_id: "short".into(), inputs: BTreeMap::new() });
    assert!(s.lifecycle_of(&"t".into()).unwrap().awaits().is_some(), "the answer is on its way: the turn still waits on it");
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Short.".into(), from_user: false });
    assert_eq!(s.lifecycle_of(&"t".into()).unwrap().awaits(), None, "the answer told to the node ends its wait");
    assert!(!s.interactions[&InteractionId::from("q")].withdrawn, "the question keeps its answer");
    assert_aggregate_rules(&s);

    let mut withdrawn = world();
    let unheard = InteractionId::from(sdk_turn(&mut withdrawn, "t", "a").as_str());
    ask(&mut withdrawn, "q");
    must(&mut withdrawn, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    must(&mut withdrawn, node("a"), Command::Withdraw { interaction_id: "q".into() });
    assert!(!inbox(&withdrawn).contains(&unheard.0), "{:?}", inbox(&withdrawn));
    assert!(matches!(last(&withdrawn, "t"), LifecycleRecord::AwaitEnded { wake: Some(_), .. }), "with its question withdrawn nothing explains its next step: the node is woken");
    assert_aggregate_rules(&withdrawn);
}

/// No session phase moves for a node of the Agent SDK: it takes a slot while its turn runs and
/// none once the turn that delivered ends.
#[test]
fn a_node_of_the_sdk_takes_a_slot_only_while_its_task_runs() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let takes_slot = |s: &DomainState| s.slots().iter().any(|agent| agent.id.0 == "a");
    assert!(takes_slot(&s), "its turn runs");
    deliver(&mut s, "t", "a", "result");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    assert!(!takes_slot(&s), "its turn ended with its delivery: {:?}", s.agents[&AgentId::from("a")].phase);
    assert_aggregate_rules(&s);
}

/// Nodes of the Agent SDK whose turn stays open on a plan or a question for the user hold no slot,
/// as the log's "waiting on the user" says: only the one at work does. Answered, a node takes its
/// slot back at once, whatever else works.
#[test]
fn nodes_of_the_sdk_waiting_on_the_user_inside_their_turn_hold_no_slot_until_answered() {
    let mut s = world();
    let plans = ["p1", "p2", "p3"];
    let questions = ["q1", "q2"];
    for id in plans.iter().chain(&questions).chain(&["busy"]) {
        let node = format!("node-{id}");
        must(&mut s, Actor::System, Command::RegisterAgent { agent_id: node.as_str().into(), name: node.clone(), at: 0 });
        sdk_turn(&mut s, id, &node);
    }
    for id in plans {
        must(&mut s, Actor::System, Command::RecordDecisionRequest {
            task_id: id.into(), generation: 1, epoch: 1, hold: Hold::Plan, tool: Some("ExitPlanMode".into()), input: serde_json::json!({}), mcp_server: None,
            prompt: DecisionPrompt::Card { body: "- [ ] Fix it".into(), options: Hold::Plan.options(), card: CardSpec::new(CardKind::Review, "Fix it").content(ContentKind::Plan, "- [ ] Fix it") },
        });
    }
    for id in questions {
        let options = ["Short", "Long"].map(|label| QuestionOption { id: label.to_lowercase(), label: label.into(), description: None }).to_vec();
        must(&mut s, node(&format!("node-{id}")), Command::AskQuestion { attachments: vec![], interaction_id: format!("ask-{id}").into(),
            scope: Scope::Task { task_id: id.into() }, recipient: Recipient::User, body: "Pick the name".into(), blocking: true, options,
            card: CardSpec::derived(CardKind::Decide, "Pick the name") });
    }
    let slots = |s: &DomainState| s.slots().iter().map(|agent| agent.id.0.clone()).filter(|id| id.starts_with("node-")).collect::<Vec<_>>();
    for id in plans.iter().chain(&questions) {
        assert!(s.node_waits_on_user(&AgentId::from(format!("node-{id}").as_str())), "{id} waits on the user inside its turn");
        assert!(holds_slot(&s, id), "{id}'s turn is still open");
    }
    assert_eq!(slots(&s), ["node-busy"], "only the node at work holds a slot");

    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: decision_card_id(&"p1".into(), 1, 1),
        option_id: Hold::APPROVE.into(), inputs: BTreeMap::new() });
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "ask-q1".into(), option_id: "short".into(), inputs: BTreeMap::new() });
    assert_eq!(slots(&s), ["node-busy", "node-p1", "node-q1"], "answered, they work again");
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

/// The card view of `card` in `lang`.
fn card_shown(s: &DomainState, card: &InteractionId, lang: crate::views::text::Lang) -> String {
    use crate::views::{Client, RenderContext, Transport, build, live::LiveSnapshot};
    let index = ReadIndex::build(s);
    let live = LiveSnapshot::default();
    let view = build(&format!("card:{card}"), RenderContext { domain: s, index: &index, live: &live, client: Client::Tui, lang,
        transport: Transport::Loopback, cursor: "test", rendered_at: "" }).unwrap();
    serde_json::to_string(&view).unwrap()
}

#[test]
fn a_turn_the_user_interrupted_ends_on_a_card_that_says_so() {
    use crate::views::text::Lang;
    let mut s = world();
    let card = sdk_turn(&mut s, "t", "a");
    must(&mut s, user(), Command::InterruptTurn { agent_id: "a".into() });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
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
fn a_turn_that_ends_on_its_own_twice_says_nothing_to_the_user() {
    use crate::views::text::Lang;
    let mut s = world();
    sdk_woken_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    for lang in [Lang::En, Lang::Es] {
        let shown = turn_end_shown(&s, "t", lang);
        for said in ["without delivering or asking", "sin entregar ni preguntar", "what next?", "¿cómo sigue?"] {
            assert!(!shown.contains(said), "{lang:?}: {shown}");
        }
    }
}

// The card of 2026-10-21 named a task with a long title and was cut before it said what it asks,
// in English. The title is shortened instead: the card says all it asks, in the viewer's language,
// within what a notification's title shows.
#[test]
fn a_card_naming_a_task_with_a_long_title_shortens_the_title_and_says_all_it_asks_in_each_language() {
    use crate::views::text::Lang;
    const TITLE: &str = "Parcel: quitar la barrera del login y los permisos de GitHub en el onboarding";
    let mut s = world();
    let card = interrupted_turn(&mut s, "t", "a");
    s.tasks.get_mut(&TaskId::from("t")).unwrap().title = TITLE.into();
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    let stored = &s.interactions[&InteractionId::from(card.as_str())].card.need;
    assert!(stored.starts_with("You interrupted “Parcel: quitar la") && stored.ends_with("…”: tell it how to go on"), "{stored}");
    assert!(stored.chars().count() <= CardSpec::TITLED_NEED_MAX, "{stored}");
    let en = turn_end_shown(&s, "t", Lang::En);
    assert!(en.contains(stored.as_str()), "{en}");
    let es_need = fit_title(TITLE, CardSpec::TITLED_NEED_MAX, |title| format!("Lo interrumpiste: dile a «{title}» cómo seguir"));
    assert!(es_need.starts_with("Lo interrumpiste: dile a «Parcel: quitar") && es_need.ends_with("…» cómo seguir"), "{es_need}");
    assert!(es_need.chars().count() <= CardSpec::TITLED_NEED_MAX, "{es_need}");
    let es = turn_end_shown(&s, "t", Lang::Es);
    assert!(es.contains(es_need.as_str()) && !es.contains("interrupted"), "{es}");
}

#[test]
fn interrupting_a_turn_does_not_change_how_a_later_turn_ends() {
    let mut s = world();
    let first = sdk_turn(&mut s, "t", "a");
    must(&mut s, user(), Command::InterruptTurn { agent_id: "a".into() });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: first.as_str().into(), option_id: CARRY_ON.into(), inputs: BTreeMap::new() });
    let Some(LifecycleRecord::Answered { input, .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("answered") };
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: "turn-2".into(), input },
    ] });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-2".into(), failure: None, failed: None });
    assert!(!s.interactions.contains_key(&turn_ended_card_id(&"t".into(), 1, "turn-2")), "a later empty turn is not an interrupt: it wakes the node");
    assert!(matches!(last(&s, "t"), LifecycleRecord::TurnEnded { wake: Some(_), .. }), "{:?}", last(&s, "t"));
    assert_aggregate_rules(&s);
}

// The native Claude names its turns `claude-message:<uuid>`; the card is still one a client can
// open and answer, whose view key and actions the views' grammar takes.
#[test]
fn a_native_turn_ends_with_a_card_clients_can_name() {
    let mut s = world();
    let turn = "claude-message:3272f577-e432-452d-9331-2bb86fc8686c";
    sdk_woken_turn_named(&mut s, "t", "a", turn);
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: turn.into(), failure: None, failed: None });
    let card = s.interactions.keys().find(|id| id.0.starts_with(STALLED_PREFIX)).unwrap().clone();
    assert_eq!(card.0, "stalled-t-1-claude-message-3272f577-e432-452d-9331-2bb86fc8686c");
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
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
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
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    assert!(!s.interactions.contains_key(&InteractionId::from(card.as_str())), "work of its own is something new: the empty turn wakes the node");
    assert!(matches!(last(&s, "t"), LifecycleRecord::TurnEnded { wake: Some(_), .. }));
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
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
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
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
    woken(&mut s, "t", "turn-2", "turn-3");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-3".into(), failure: None, failed: None });
    assert_eq!(s.interactions.keys().filter(|id| id.0.starts_with(STALLED_PREFIX)).map(|id| id.0.as_str()).collect::<Vec<_>>(),
        [format!("{STALLED_PREFIX}t-1-turn-3")], "one case, from the woken turn that rested");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running, "the orchestrator settles it, not the user");
    assert_eq!(kinds(&s, "t")[4..], ["work_started", "turn_ended", "work_ended", "turn_resumed", "turn_ended", "turn_started", "turn_ended"]);
    let journal = s.lifecycle_journal[&TaskId::from("t")].clone();
    assert!(replay(&TaskId::from("t"), &journal).is_ok(), "the journal replays");
    assert_aggregate_rules(&s);
}

// A subagent hands its report back after the turn ended and Claude takes it up by itself: the card
// that asked how the task goes on is withdrawn, the task runs, and the end of that turn asks again.
#[test]
fn a_turn_claude_opens_by_itself_withdraws_the_card_that_asked_how_it_goes_on() {
    let mut s = world();
    let card = sdk_woken_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    assert!(s.open(&s.interactions[&InteractionId::from(card.as_str())]));
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnUnprompted { turn: "turn-2".into() },
    ] });
    assert!(s.interactions[&InteractionId::from(card.as_str())].withdrawn, "the case no longer holds");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-2".into(), failure: None, failed: None });
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running, "its empty end is a case for the orchestrator again");
    assert_eq!(kinds(&s, "t")[6..], ["turn_ended", "turn_unprompted", "turn_ended"]);
    assert!(replay(&TaskId::from("t"), &s.lifecycle_journal[&TaskId::from("t")]).is_ok(), "the journal replays");
    assert_aggregate_rules(&s);
}

// The user writes to the node instead of answering the card its ended turn left: that card asked
// how the task goes on, and it is answered, so the delivery that follows is not held behind it.
#[test]
fn telling_a_resting_node_how_to_go_on_retires_the_card_that_asked_it() {
    let mut s = world();
    let card = interrupted_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Deliver the parser split".into(), from_user: false });
    assert!(s.interactions[&InteractionId::from(card.as_str())].withdrawn, "the card no longer waits on the user");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
    assert_aggregate_rules(&s);
}

#[test]
fn carrying_on_tells_the_task_how_to_go_on_and_cancelling_ends_it() {
    let mut s = world();
    let card = interrupted_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.as_str().into(), option_id: CARRY_ON.into(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text("Add the tests first".into()))]) });
    let Some(LifecycleRecord::Answered { text: Some(text), .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("answered") };
    assert!(text.starts_with("Go on with your task") && text.ends_with("Add the tests first"), "{text}");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
    assert_aggregate_rules(&s);

    let mut s = world();
    let card = interrupted_turn(&mut s, "u", "b");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "u".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.as_str().into(), option_id: CANCEL_TASK.into(), inputs: BTreeMap::new() });
    assert_eq!(lifecycle(&s, "u").0, TaskState::Closing);
    assert_eq!(lifecycle(&s, "u").3, Some(TaskEnding::Cancelling));
    assert_aggregate_rules(&s);
}

fn fail_turn(s: &mut DomainState, id: &str, turn: &str, error: &str, status: Option<u16>) -> CommandOutcome {
    must(s, Actor::System, Command::RecordTurnBoundary {
        task_id: id.into(), generation: 1, turn: turn.into(),
        failure: Some(SdkApiFailure { error: error.into(), status, resets_at: None }), failed: None })
}
fn sign_in_cards(s: &DomainState) -> Vec<&Interaction> {
    s.interactions.values().filter(|i| i.id.0.starts_with(SIGNED_OUT_PREFIX) && s.open(i)).collect()
}
/// The cards of Claude's holds Waiting for You lists.
fn listed_holds(s: &DomainState) -> Vec<String> {
    super::read_index::inbox_items(s).into_iter().filter(|item| item.pending).map(|item| {
        let InboxSubject::Interaction { interaction_id } = item.subject;
        interaction_id.0
    }).filter(|id| s.claude_hold(&InteractionId::from(id.as_str())).is_some()).collect()
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
        failure: Some(SdkApiFailure { error: "rate_limit".into(), status: Some(429), resets_at: Some(4_102_444_800) }), failed: None });
    assert_eq!((notices(&outage), notices(&usage)), (0, 0), "never announced");
    assert_eq!(listed_holds(&s).len(), 2, "one card for the outage, one for the limit: {:?}", listed_holds(&s));
    assert_eq!(held(&s), [("t".into(), ClaudeHold::Outage), ("u".into(), ClaudeHold::Outage), ("v".into(), ClaudeHold::Usage)]);
    let limit = s.claude_held().into_iter().find(|held| held.hold == ClaudeHold::Usage).unwrap();
    assert_eq!(limit.resets_at, Some(4_102_444_800), "the reset Claude gave");
    assert_eq!(exec(&mut s, Actor::System, Command::ProbeClaudeAccess { provider: crate::accounts::Provider::Claude, hold: ClaudeHold::SignedOut, account: None }).unwrap_err().code, "invalid_input",
        "only the user's answer probes the sign-in");
    assert_eq!(exec(&mut s, user(), Command::ProbeClaudeAccess { provider: crate::accounts::Provider::Claude, hold: ClaudeHold::Outage, account: None }).unwrap_err().code, "forbidden");
    must(&mut s, Actor::System, Command::ProbeClaudeAccess { provider: crate::accounts::Provider::Claude, hold: ClaudeHold::Outage, account: None });
    assert_eq!(held(&s), [("u".into(), ClaudeHold::Outage), ("v".into(), ClaudeHold::Usage)], "one probe, the oldest");
    let resumed_text = |s: &DomainState, id: &str| s.lifecycle_journal[&TaskId::from(id)].iter().rev()
        .find_map(|record| match record { LifecycleRecord::Resumed { text, .. } => text.clone(), _ => None });
    assert_eq!(resumed_text(&s, "t").as_deref(), Some(GO_ON), "the API answers again");
    must(&mut s, Actor::System, Command::ProbeClaudeAccess { provider: crate::accounts::Provider::Claude, hold: ClaudeHold::Usage, account: None });
    assert_eq!(held(&s), [("u".into(), ClaudeHold::Outage)]);
    let renewed = resumed_text(&s, "v").expect("the limit's task is told to go on");
    assert!(renewed.starts_with("Your usage limit has renewed: Claude answers again. Go on with your task now"), "{renewed}");
    assert!(renewed.contains("never wait for one with sleep, cron or a background loop"), "{renewed}");
    assert_eq!(exec(&mut s, Actor::System, Command::ProbeClaudeAccess { provider: crate::accounts::Provider::Claude, hold: ClaudeHold::Usage, account: None }).unwrap_err().code, "not_found");
    assert_aggregate_rules(&s);
}

fn usage_turn(s: &mut DomainState, id: &str, resets_at: u64) {
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: id.into(), generation: 1, turn: "turn-1".into(),
        failure: Some(SdkApiFailure { error: "rate_limit".into(), status: Some(429), resets_at: Some(resets_at) }), failed: None });
}
// 25 Oct, t-codex-support-design: the limit paused the task, then work of its own ended and
// Claude opened a turn by itself that hit the limit again. The domain refused that second pause,
// so the whole end of the turn went unrecorded and the SDK owner stopped.
#[test]
fn a_turn_that_hits_the_limit_while_the_limit_already_holds_its_task_ends_on_the_same_wait() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkStarted { turn: "turn-1".into(), work: "claude-task:bash".into() },
    ] });
    usage_turn(&mut s, "t", 4_102_444_800);
    let usage = alondra_domain::LiveState::WaitingCapacity(alondra_domain::Capacity::Usage);
    assert_eq!(s.live_state(&"t".into()), Some(usage.clone()));
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnUnprompted { turn: "turn-2".into() },
        RuntimeObservation::WorkEnded { work: "claude-task:bash".into(), continuation: "claude-notification:bash".into() },
        RuntimeObservation::WorkContinued { continuation: "claude-notification:bash".into() },
    ] });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-2".into(),
        failure: Some(SdkApiFailure { error: "rate_limit".into(), status: Some(429), resets_at: Some(4_102_444_800) }), failed: None });
    assert_eq!(kinds(&s, "t").last().map(String::as_str), Some("turn_ended"), "the end of the turn is recorded");
    assert_eq!(s.live_state(&"t".into()), Some(usage), "the task still waits for the usage limit");
    assert_eq!(held(&s), [("t".into(), ClaudeHold::Usage)], "on one card");
    must(&mut s, Actor::System, Command::ProbeClaudeAccess { provider: crate::accounts::Provider::Claude, hold: ClaudeHold::Usage, account: None });
    assert!(matches!(s.live_state(&"t".into()), Some(alondra_domain::LiveState::Working(_))), "the reset lets it go on");
    assert_aggregate_rules(&s);
}

// The API failed, and Claude's next turn of its own met the usage limit: the limit is what holds
// the task now, on its own card, and the reset lets it go on.
#[test]
fn a_turn_the_limit_ends_while_the_api_holds_its_task_waits_for_the_limit_instead() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let outage = |s: &DomainState| s.interactions.values().find(|i| i.id.0.starts_with(OUTAGE_PREFIX)).unwrap().clone();
    fail_turn(&mut s, "t", "turn-1", "overloaded", Some(529));
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnUnprompted { turn: "turn-2".into() },
    ] });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-2".into(),
        failure: Some(SdkApiFailure { error: "rate_limit".into(), status: Some(429), resets_at: Some(4_102_444_800) }), failed: None });
    assert!(outage(&s).withdrawn, "the API answered, with the limit");
    assert_eq!(held(&s), [("t".into(), ClaudeHold::Usage)]);
    assert_eq!(s.live_state(&"t".into()), Some(alondra_domain::LiveState::WaitingCapacity(alondra_domain::Capacity::Usage)));
    must(&mut s, Actor::System, Command::ProbeClaudeAccess { provider: crate::accounts::Provider::Claude, hold: ClaudeHold::Usage, account: None });
    assert!(held(&s).is_empty());
    assert_aggregate_rules(&s);
}

fn accounts_seen(s: &mut DomainState, states: &[(&str, crate::accounts::AccountState)]) {
    let accounts = states.iter().map(|(id, state)| crate::accounts::AccountStatus {
        provider: crate::accounts::Provider::Claude, id: crate::accounts::AccountId::parse(id).unwrap(), name: id.to_uppercase(), state: *state,
        native_identity: None, email: None, plan: None, billing: Default::default(), sign_ins: 0,
    }).collect();
    must(s, Actor::System, Command::RecordAccounts { accounts, reserve: crate::usage::default_reserve() });
}
fn account_state(s: &DomainState, id: &str) -> (crate::accounts::AccountState, u64) {
    let status = s.accounts.iter().find(|status| status.id.as_str() == id).unwrap();
    (status.state, status.sign_ins)
}

// Two accounts ran out: Claude answering a task of one says nothing of the other's limit, so only
// the tasks of the account that answered go on. Before, any answer let every held task go on, and
// those of an account still out of usage failed again at once.
#[test]
fn claude_answering_on_one_account_lets_only_that_accounts_held_tasks_go_on() {
    let mut s = world();
    for node in ["c", "d"] {
        must(&mut s, Actor::System, Command::RegisterAgent { agent_id: node.into(), name: node.into(), at: 0 });
    }
    sdk_turn_on(&mut s, "t", "a", "turn-1", None);
    sdk_turn_on(&mut s, "u", "b", "turn-1", None);
    sdk_turn_on(&mut s, "v", "c", "turn-1", Some("work"));
    sdk_turn_on(&mut s, "w", "d", "turn-1", Some("work"));
    for id in ["t", "u"] {
        usage_turn(&mut s, id, 4_102_444_800);
    }
    for id in ["v", "w"] {
        usage_turn(&mut s, id, 4_102_448_400);
    }
    assert_eq!(listed_holds(&s).len(), 2, "one card per account's limit: {:?}", listed_holds(&s));
    let probed = |s: &DomainState| s.claude_held().into_iter().map(|held| (held.task.0, held.account.to_string())).collect::<Vec<_>>();
    must(&mut s, Actor::System, Command::ProbeClaudeAccess { provider: crate::accounts::Provider::Claude, hold: ClaudeHold::Usage, account: Some(crate::accounts::AccountId::parse("work").unwrap()) });
    assert_eq!(probed(&s), [("t".into(), "default".into()), ("u".into(), "default".into()), ("w".into(), "work".into())], "the work account's reset probes one of its own");
    turn_on_resumed(&mut s, "v", "turn-2");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "v".into(), generation: 1, facts: vec![RuntimeObservation::ModelAnswered] });
    assert_eq!(held(&s), [("t".into(), ClaudeHold::Usage), ("u".into(), ClaudeHold::Usage)], "the default account is still out: its tasks wait");
    assert_eq!(kinds(&s, "w").last().map(String::as_str), Some("resumed"), "the rest of the work account goes on");
    assert_eq!(listed_holds(&s).len(), 1, "the default account's card is still listed");
    assert_aggregate_rules(&s);
}

fn admit_off_limit(s: &mut DomainState, id: &str, node: &str, account: &str, card: &InteractionId) -> Result<CommandOutcome, DomainError> {
    exec(s, Actor::System, Command::AdmitExecution { provider: Default::default(),
        task_id: id.into(), agent_id: node.into(), conversation: format!("conversation-{node}"), instance: format!("instance-{node}"), host: "sdk-host".into(),
        model: None, account: Some(crate::accounts::AccountId::parse(account).unwrap()), leaves_limit: Some(card.clone()),
    })
}

// A legacy SDK quota hold cannot authorize another account; normal recovery keeps its binding.
#[test]
fn an_existing_legacy_execution_cannot_leave_its_accounts_limit_on_another_account() {
    use crate::accounts::AccountState::{Disabled,SignedIn,SignedOut};
    let mut s=world();must(&mut s,Actor::System,Command::RegisterAgent{agent_id:"m".into(),name:"m".into(),at:0});
    accounts_seen(&mut s,&[("default",SignedIn),("work",SignedIn),("off",Disabled),("out",SignedOut)]);
    sdk_turn_on(&mut s,"t","a","turn-1",Some("work"));usage_turn(&mut s,"t",4_102_444_800);
    let card=s.held_by_limit(&"t".into()).unwrap().card;let journal=s.lifecycle_journal.clone();let revision=s.revision;
    assert!(s.tasks[&TaskId::from("t")].selection.is_none());
    for account in ["default","off","out","ghost"]{
        assert_eq!(admit_off_limit(&mut s,"t","m",account,&card).unwrap_err().code,"active_account_immutable");
        assert_eq!(s.revision,revision);assert_eq!(s.lifecycle_journal,journal);
    }
    assert_eq!(admit_off_limit(&mut s,"t","m","work",&card).unwrap_err().code,"invalid_input","same account does not bypass its hold");
    assert_eq!(s.task_account(&"t".into()),crate::accounts::AccountId::parse("work").unwrap());
    assert_eq!(s.tasks[&TaskId::from("t")].agent_id,Some("a".into()));assert!(!s.interactions[&card].withdrawn);
    must(&mut s,Actor::System,Command::ProbeClaudeAccess{provider:crate::accounts::Provider::Claude,hold:ClaudeHold::Usage,account:Some(crate::accounts::AccountId::parse("work").unwrap())});
    turn_on_resumed(&mut s,"t","turn-2");
    must(&mut s,Actor::System,Command::RecordRuntimeFacts{task_id:"t".into(),generation:1,facts:vec![RuntimeObservation::ModelAnswered]});
    assert_eq!(s.task_account(&"t".into()),crate::accounts::AccountId::parse("work").unwrap());
    assert_eq!(s.tasks[&TaskId::from("t")].agent_id,Some("a".into()));assert!(s.claude_held().is_empty());assert_aggregate_rules(&s);
}

// A refused account transfer does not lose a pending user question or its answer.
#[test]
fn a_question_the_node_asked_stays_open_when_an_account_transfer_is_refused() {
    use crate::accounts::AccountState::SignedIn;
    let mut s = world();
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: "m".into(), name: "m".into(), at: 0 });
    accounts_seen(&mut s, &[("default", SignedIn), ("work", SignedIn)]);
    sdk_turn_on(&mut s, "t", "a", "turn-1", Some("work"));
    let options = ["Short", "Long"].map(|label| QuestionOption { id: label.to_lowercase(), label: label.into(), description: None }).to_vec();
    must(&mut s, node("a"), Command::AskQuestion { attachments: vec![], interaction_id: "q".into(), scope: Scope::Task { task_id: "t".into() },
        recipient: Recipient::User, body: "Pick the name".into(), blocking: true, options, card: CardSpec::derived(CardKind::Decide, "Pick the name") });
    usage_turn(&mut s, "t", 4_102_444_800);
    let card = s.held_by_limit(&"t".into()).expect("only the limit holds its execution").card;
    assert_eq!(admit_off_limit(&mut s,"t","m","default",&card).unwrap_err().code,"active_account_immutable");
    assert!(!s.interactions[&InteractionId::from("q")].withdrawn, "the question is still the user's to answer");
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "q".into(), option_id: "short".into(), inputs: BTreeMap::new() });
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Short.".into(), from_user: false });
    assert_aggregate_rules(&s);
}

#[test]
fn the_api_answering_on_any_account_lets_every_task_the_outage_held_go_on() {
    let mut s = world();
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: "c".into(), name: "c".into(), at: 0 });
    sdk_turn_on(&mut s, "t", "a", "turn-1", None);
    sdk_turn_on(&mut s, "u", "b", "turn-1", Some("work"));
    sdk_turn_on(&mut s, "v", "c", "turn-1", Some("work"));
    fail_turn(&mut s, "t", "turn-1", "overloaded", Some(529));
    fail_turn(&mut s, "u", "turn-1", "overloaded", Some(529));
    usage_turn(&mut s, "v", 4_102_448_400);
    assert_eq!(listed_holds(&s).len(), 2, "the API is one card for every account: {:?}", listed_holds(&s));
    must(&mut s, Actor::System, Command::ProbeClaudeAccess { provider: crate::accounts::Provider::Claude, hold: ClaudeHold::Outage, account: None });
    turn_on_resumed(&mut s, "t", "turn-2");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![RuntimeObservation::ModelAnswered] });
    assert_eq!(held(&s), [("v".into(), ClaudeHold::Usage)], "the API works for the work account too; its limit still stands");
    assert_aggregate_rules(&s);
}

#[test]
fn an_expired_sign_in_leaves_its_account_signed_out_until_claude_answers_on_it() {
    use crate::accounts::AccountState::{SignedIn, SignedOut};
    let mut s = world();
    accounts_seen(&mut s, &[("default", SignedIn), ("work", SignedIn)]);
    sdk_turn_on(&mut s, "t", "a", "turn-1", Some("work"));
    sdk_turn_on(&mut s, "u", "b", "turn-1", None);
    let expired = fail_turn(&mut s, "t", "turn-1", "authentication_failed", Some(401));
    assert_eq!(notices(&expired), 1, "the user hears of it");
    assert_eq!(account_state(&s, "work"), (SignedOut, 0), "no new work starts on it");
    assert_eq!(account_state(&s, "default"), (SignedIn, 0), "the other account is not touched");
    let en = turn_end_shown(&s, "t", crate::views::text::Lang::En);
    assert!(en.contains("Sign in to the Claude account WORK on the Mac"), "{en}");
    assert!(en.contains(r#"Open Terminal on the Mac, run alondra accounts login \"WORK\", then press I signed in."#), "the command to run: {en}");
    let es = turn_end_shown(&s, "t", crate::views::text::Lang::Es);
    assert!(es.contains(r#"ejecuta alondra accounts login \"WORK\""#), "{es}");
    let other = fail_turn(&mut s, "u", "turn-1", "authentication_failed", Some(401));
    assert_eq!(notices(&other), 1, "another account's sign-in is news of its own");
    assert_eq!(listed_holds(&s).len(), 2, "one card per account");
    let index = super::read_index::ReadIndex::build(&s);
    assert_eq!(index.accounts().iter().find(|a| a.id.as_str() == "work").map(|a| a.state), Some(SignedOut), "every screen reads it");
    accounts_seen(&mut s, &[("default", SignedIn), ("work", SignedIn)]);
    assert_eq!(account_state(&s, "work"), (SignedOut, 0), "its sign-in check does not undo what Claude said while a task waits on it");
    let card = s.claude_held().into_iter().find(|held| held.task.0 == "t").unwrap().card;
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card, option_id: SIGNED_IN.into(), inputs: BTreeMap::new() });
    turn_on_resumed(&mut s, "t", "turn-2");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![RuntimeObservation::ModelAnswered] });
    assert_eq!(account_state(&s, "work"), (SignedIn, 1), "signed in again: its credential's next generation");
    assert_eq!(held(&s), [("u".into(), ClaudeHold::SignedOut)], "the default account's sign-in still waits");
    accounts_seen(&mut s, &[("default", SignedIn), ("work", SignedIn)]);
    assert_eq!(account_state(&s, "work"), (SignedIn, 1), "the generation is kept");
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
    let ended = must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: false, ended_work: vec![], by_alondra: false });
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
fn a_turn_that_stays_open_after_claude_accepted_its_interrupt_can_be_forced_to_stop_once() {
    use crate::views::text::Lang;
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let force = || Command::ForceStopTurn { agent_id: "a".into() };
    let accepted = || Command::RecordInterruptAccepted { task_id: "t".into(), generation: 1, turn: "turn-1".into() };
    assert_eq!(refused_unchanged(&mut s, user(), force()), "conflict", "an interrupt comes first");
    assert_eq!(exec(&mut s.clone(), user(), force()).unwrap_err().refusal, Some(Refusal::InterruptNotAccepted));
    must(&mut s, user(), Command::InterruptTurn { agent_id: "a".into() });
    assert_eq!(refused_unchanged(&mut s, user(), force()), "conflict", "until Claude accepts the interrupt");
    assert_eq!(refused_unchanged(&mut s, user(), accepted()), "forbidden", "only Alondra records what Claude did");
    must(&mut s, Actor::System, accepted());
    assert_eq!(last(&s, "t"), LifecycleRecord::InterruptAccepted { turn: "turn-1".into() });
    assert_eq!(refused_unchanged(&mut s, node("a"), force()), "forbidden");
    let revision = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap().revision();
    must(&mut s, user(), force());
    assert_eq!(last(&s, "t"), LifecycleRecord::ControlRequested {
        control: format!("control-t-{}", revision + 1),
        operation: ControlRecord::ForceStop { turn: "turn-1".into() },
    });
    assert_eq!(exec(&mut s.clone(), user(), force()).unwrap_err().refusal, Some(Refusal::AlreadyStopping), "once");
    assert_aggregate_rules(&s);

    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: false, ended_work: vec![], by_alondra: false });
    let card = s.interactions.keys().find(|id| id.0.starts_with(SESSION_ENDED_PREFIX)).expect("the stopped task asks").clone();
    assert_eq!(s.interactions[&card].card.need, "You interrupted “Task t”: tell it how to go on", "Claude exiting ends the turn as the user's interrupt");
    assert_eq!(s.interactions[&card].card.why, INTERRUPTED_WHY);
    assert_eq!(options(&s, &card), [CARRY_ON, CANCEL_TASK]);
    let en = turn_end_shown(&s, "t", Lang::En);
    assert!(en.contains("You interrupted “Task t”: tell it how to go on") && !en.contains("when its session closed"), "{en}");
    let es = turn_end_shown(&s, "t", Lang::Es);
    assert!(es.contains("Lo interrumpiste: dile a «Task t» cómo seguir") && !es.contains("al cerrarse su sesión"), "{es}");
    answer(&mut s, &card, CARRY_ON);
    assert!(matches!(s.lifecycle_journal[&TaskId::from("t")].last(), Some(LifecycleRecord::Answered { .. })), "the next message goes to the resumed session");
    assert_aggregate_rules(&s);
}

#[test]
fn an_interrupt_accepted_after_its_turn_ended_records_nothing() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, user(), Command::InterruptTurn { agent_id: "a".into() });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    assert_eq!(refused_unchanged(&mut s, Actor::System, Command::RecordInterruptAccepted { task_id: "t".into(), generation: 1, turn: "turn-1".into() }), "conflict");
    assert_eq!(refused_unchanged(&mut s, user(), Command::ForceStopTurn { agent_id: "a".into() }), "conflict", "nothing runs to stop");
}

#[test]
fn a_host_that_ends_while_its_task_waits_on_the_user_adds_no_card() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: false, ended_work: vec![], by_alondra: false });
    assert!(!s.interactions.keys().any(|id| id.0.starts_with(SESSION_ENDED_PREFIX)), "its turn card already asks");
    assert_eq!(kinds(&s, "t").last().map(String::as_str), Some("host_ended"));
    assert_aggregate_rules(&s);
}

/// A cancelled SDK task whose node was told how to settle its worktrees after the cancel: the
/// input that message became.
fn cancelled_mid_turn(s: &mut DomainState) -> String {
    sdk_turn(s, "t", "a");
    must(s, user(), Command::CancelTask { task_id: "t".into() });
    must(s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Settle your worktrees".into(), from_user: false });
    let LifecycleRecord::Told { input, .. } = last(s, "t") else { panic!("told: {:?}", last(s, "t")) };
    input
}

fn open_cards(s: &DomainState) -> Vec<String> {
    s.interactions.values().filter(|i| s.open(i)).map(|i| i.id.0.clone()).collect()
}

// 3 November: the cancelled «Live Activity 3» could never close. Its host had ended, and the
// lifecycle of a cancelled task refused every fact of its host, so recording that end failed on each
// attempt and the user got a card that nothing they pressed could settle.
#[test]
fn a_cancelled_task_whose_host_ends_records_the_end_asks_nobody_and_is_cancelled_with_its_node() {
    let mut s = world();
    let told = cancelled_mid_turn(&mut s);
    let before = open_cards(&s);
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: false, ended_work: vec![], by_alondra: false });
    assert_eq!(kinds(&s, "t").last().map(String::as_str), Some("host_ended"));
    must(&mut s, Actor::System, Command::RecordDoubtfulHandoff { task_id: "t".into(), generation: 1, input: told, text: "Settle your worktrees".into(), resumes: false, by_alondra: false });
    assert_eq!(open_cards(&s), before, "a task that is closing has nobody to ask how it goes on");
    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Gone, agent_id: Some("a".into()) });
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Cancelled);
    assert_aggregate_rules(&s);
}

#[test]
fn a_cancelled_task_resends_what_its_ended_host_never_took_to_the_host_that_resumes_it() {
    let mut s = world();
    let told = cancelled_mid_turn(&mut s);
    let before = open_cards(&s);
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: true, ended_work: vec![], by_alondra: false });
    must(&mut s, Actor::System, Command::ResendUntakenInput { task_id: "t".into(), generation: 1, input: told.clone(), text: "Settle your worktrees".into() });
    let pending = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap().pending_inputs().into_iter().map(|input| input.as_str().to_owned()).collect::<Vec<_>>();
    assert!(!pending.contains(&told) && pending.len() == 1, "the message waits as a new input for the resumed host: {pending:?}");
    assert_eq!(open_cards(&s), before);
    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Gone, agent_id: Some("a".into()) });
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Cancelled);
    assert_aggregate_rules(&s);
}

/// A task whose host ended while the user's answer to its turn card was handed and not yet read.
fn doubtful(s: &mut DomainState, written: &str) -> (InteractionId, String) {
    let turn = interrupted_turn(s, "t", "a");
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    must(s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: turn.as_str().into(), option_id: CARRY_ON.into(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text(written.into()))]) });
    let Some(LifecycleRecord::Answered { input, text: Some(text), .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("answered") };
    must(s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: false, ended_work: vec![], by_alondra: false });
    must(s, Actor::System, Command::RecordDoubtfulHandoff { task_id: "t".into(), generation: 1, input: input.clone(), text: text.clone(), resumes: false, by_alondra: false });
    let card = s.interactions.keys().find(|id| id.0.starts_with(HANDOFF_DOUBT_PREFIX)).expect("the doubt asks the user").clone();
    (card, text)
}

#[test]
fn a_message_the_ended_host_may_not_have_read_is_shown_whole_and_sent_again_only_on_the_users_word() {
    let mut s = world();
    let (card, text) = doubtful(&mut s, "Add the tests first");
    let doubt = &s.interactions[&card];
    assert_eq!(doubt.card.need, "“Task t” closed before confirming it read your message: send it again?");
    assert_eq!(doubt.card.content.as_ref().map(|content| content.body.as_str()), Some(text.as_str()), "the card carries the whole message");
    assert_eq!(options(&s, &card), [RESEND, TAKE_AS_RECEIVED]);
    assert_eq!(exec(&mut s, Actor::System, Command::RecordDoubtfulHandoff { task_id: "t".into(), generation: 1, input: "other".into(), text: "other".into(), resumes: false, by_alondra: false })
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
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
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
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    must(&mut s, user(), Command::InterruptTurn { agent_id: "a".into() });
    assert!(matches!(last(&s, "t"), LifecycleRecord::ControlRequested { operation: ControlRecord::Interrupt { turn: None }, .. }));
    assert_aggregate_rules(&s);
}

// 22 October: a task whose background command ended waited for Claude to wake up with its
// notification, which never came, and nothing could stop it. That wait, and input Claude was handed
// and has not taken, are interrupted without naming a turn.
#[test]
fn interrupting_a_task_that_waits_for_its_background_work_or_its_input_names_no_turn() {
    let mut waking = world();
    sdk_turn(&mut waking, "t", "a");
    must(&mut waking, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkStarted { turn: "turn-1".into(), work: "claude-task:bash".into() },
    ] });
    must(&mut waking, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    must(&mut waking, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkEnded { work: "claude-task:bash".into(), continuation: "claude-notification:bash".into() },
    ] });
    let mut handed = world();
    task(&mut handed, "t", None);
    must(&mut handed, Actor::System, Command::AdmitExecution { provider: Default::default(),
        task_id: "t".into(), agent_id: "a".into(), conversation: "conversation-t".into(), instance: "instance-t".into(), host: "sdk-host".into(), model: None,
        account: None, leaves_limit: None,
    });
    must(&mut handed, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![RuntimeObservation::ResourceAvailable] });
    for (mut s, activity) in [(waking, alondra_domain::ActivityView::Continuation), (handed, alondra_domain::ActivityView::Starting)] {
        assert_eq!(s.lifecycle_of(&TaskId::from("t")).unwrap().activity(), activity);
        must(&mut s, user(), Command::InterruptTurn { agent_id: "a".into() });
        assert!(matches!(last(&s, "t"), LifecycleRecord::ControlRequested { operation: ControlRecord::Interrupt { turn: None }, .. }), "{activity:?}");
        assert_eq!(s.lifecycle_of(&TaskId::from("t")).unwrap().activity(), activity, "the interrupt reaches Claude; what it ends comes back as Claude's facts");
        assert_aggregate_rules(&s);
    }
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
    let card = sdk_woken_turn(&mut s, "t", "a");
    let compact = || Command::CompactConversation { task_id: "t".into() };
    assert_eq!(refused_unchanged(&mut s, Actor::System, compact()), "conflict", "a turn runs");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
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

fn mcp_server(name: &str, status: McpStatus) -> McpServerState {
    McpServerState { name: name.into(), status, error: None, source: None, transport: None, url: None, traits: Default::default() }
}

#[test]
fn a_node_keeps_how_its_mcp_servers_are_as_its_claude_reports_them() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let report = |servers: Vec<McpServerState>| Command::RecordMcpServers { task_id: "t".into(), generation: 1, servers };
    assert_eq!(refused_unchanged(&mut s, user(), report(vec![])), "forbidden", "only what Claude reported");
    assert_eq!(refused_unchanged(&mut s, Actor::System, Command::RecordMcpServers { task_id: "t".into(), generation: 2, servers: vec![] }), "stale_execution");
    let mut leaky = mcp_server("linear", McpStatus::Connected);
    leaky.url = Some("https://mcp.example.com/mcp?key=secret".into());
    assert_eq!(refused_unchanged(&mut s, Actor::System, report(vec![leaky])), "invalid_input", "an address with credentials is never kept");
    let mut remote = mcp_server("linear", McpStatus::Failed);
    (remote.transport, remote.url, remote.error) = (Some("http".into()), Some("https://mcp.linear.app/mcp".into()), Some("unreachable".into()));
    must(&mut s, Actor::System, report(vec![remote.clone(), mcp_server("files", McpStatus::Connected)]));
    must(&mut s, Actor::System, report(vec![mcp_server("linear", McpStatus::Failed), mcp_server("files", McpStatus::Connected)]));
    assert_eq!(s.mcp[&AgentId::from("a")].servers[0], remote, "init's report keeps how the server is reached and its unchanged error");
    must(&mut s, Actor::System, report(vec![mcp_server("linear", McpStatus::NeedsAuth)]));
    let linear = &s.mcp[&AgentId::from("a")].servers;
    assert_eq!((linear.len(), linear[0].status, linear[0].error.clone(), linear[0].url.clone()), (1, McpStatus::NeedsAuth, None, remote.url.clone()), "a changed status drops the old error");
    assert_eq!(McpStatus::of("a-status-from-a-later-claude"), McpStatus::Unknown);
}

#[test]
fn claude_forgets_only_a_sign_in_it_keeps_to_a_remote_server() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let sign_out = |server: &str| Command::ClearMcpSignIn { task_id: "t".into(), server: server.into() };
    assert_eq!(refused_unchanged(&mut s, Actor::System, sign_out("linear")), "not_found", "a server Claude never reported");
    let remote = |name: &str, status: McpStatus, transport: &str, source: &str| McpServerState {
        transport: Some(transport.into()), source: Some(source.into()),
        traits: alondra_foundation::McpTraits { sign_out: matches!(transport, "http" | "sse") && source != "claudeai", ..Default::default() },
        ..mcp_server(name, status)
    };
    must(&mut s, Actor::System, Command::RecordMcpServers { task_id: "t".into(), generation: 1, servers: vec![
        remote("linear", McpStatus::Connected, "http", "user"), remote("events", McpStatus::NeedsAuth, "sse", "project"),
        remote("files", McpStatus::Connected, "stdio", "project"), remote("claude.ai Gmail", McpStatus::Connected, "claudeai-proxy", "claudeai"),
        remote("off", McpStatus::Disabled, "http", "user"),
    ] });
    assert_eq!(refused_unchanged(&mut s, user(), sign_out("linear")), "forbidden");
    assert_eq!(refused_unchanged(&mut s, Actor::System, sign_out("files")), "conflict", "a local server keeps no sign-in");
    assert_eq!(refused_unchanged(&mut s, Actor::System, sign_out("claude.ai Gmail")), "conflict", "a claude.ai connector is signed out on claude.ai");
    assert_eq!(refused_unchanged(&mut s, Actor::System, sign_out("off")), "conflict", "turned off");
    must(&mut s, Actor::System, sign_out("linear"));
    assert!(matches!(last(&s, "t"), LifecycleRecord::ControlRequested { operation: ControlRecord::ClearMcpSignIn { server }, .. } if server == "linear"));
    assert_eq!(refused_unchanged(&mut s, Actor::System, sign_out("events")), "conflict", "no sign-in to clear while it needs one");
    assert_aggregate_rules(&s);
}

#[test]
fn only_a_server_that_needs_it_is_signed_in_to_and_its_return_closes_the_sign_in_once() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let sign_in = |server: &str| Command::StartMcpSignIn { task_id: "t".into(), server: server.into() };
    assert_eq!(refused_unchanged(&mut s, Actor::System, sign_in("linear")), "not_found", "a server Claude never reported");
    must(&mut s, Actor::System, Command::RecordMcpServers { task_id: "t".into(), generation: 1, servers: vec![
        mcp_server("linear", McpStatus::NeedsAuth), mcp_server("files", McpStatus::Connected), mcp_server("off", McpStatus::Disabled),
    ] });
    assert_eq!(refused_unchanged(&mut s, user(), sign_in("linear")), "forbidden");
    assert_eq!(refused_unchanged(&mut s, Actor::System, sign_in("files")), "conflict", "already connected");
    assert_eq!(refused_unchanged(&mut s, Actor::System, sign_in("off")), "conflict", "turned off");
    must(&mut s, Actor::System, sign_in("linear"));
    assert!(matches!(last(&s, "t"), LifecycleRecord::ControlRequested { operation: ControlRecord::StartMcpSignIn { server }, .. } if server == "linear"));
    must(&mut s, Actor::System, Command::ReconnectMcp { task_id: "t".into(), server: "files".into() });
    assert!(matches!(last(&s, "t"), LifecycleRecord::ControlRequested { operation: ControlRecord::ReconnectMcp { server }, .. } if server == "files"));
    assert_eq!(refused_unchanged(&mut s, Actor::System, Command::ReconnectMcp { task_id: "t".into(), server: "missing".into() }), "not_found");

    let complete = |state: &str| Command::CompleteMcpSignIn { task_id: "t".into(), server: "linear".into(), state: state.into() };
    assert_eq!(refused_unchanged(&mut s, Actor::System, complete("s1")), "not_found", "no sign-in open yet");
    let browser = McpSignIn::Browser { url: "https://linear.app/oauth/authorize?state=s1".into(), callback_port: Some(49200), state: Some("s1".into()) };
    let opened = |sign_in: McpSignIn| Command::RecordMcpSignIn { task_id: "t".into(), generation: 1, server: "linear".into(), sign_in };
    assert_eq!(refused_unchanged(&mut s, Actor::System, opened(McpSignIn::Browser { url: "http://auth.example.com/authorize".into(), callback_port: None, state: None })), "invalid_input", "the user signs in only on an https page");
    assert_eq!(refused_unchanged(&mut s, Actor::System, opened(McpSignIn::Browser { url: "https://linear.app/a".into(), callback_port: Some(49200), state: None })), "invalid_input");
    must(&mut s, Actor::System, opened(browser));
    assert_eq!(s.mcp[&AgentId::from("a")].sign_ins["linear"].callback_port, Some(49200));
    assert_eq!(refused_unchanged(&mut s, user(), complete("s1")), "forbidden");
    assert_eq!(refused_unchanged(&mut s, Actor::System, complete("another")), "conflict", "another sign-in's return");
    must(&mut s, Actor::System, complete("s1"));
    assert!(s.mcp[&AgentId::from("a")].sign_ins.is_empty());
    assert_eq!(refused_unchanged(&mut s, Actor::System, complete("s1")), "not_found", "a return is taken once");
    assert_aggregate_rules(&s);
}

#[test]
fn a_sign_in_ends_when_its_server_connects_or_claude_refuses_or_needs_no_browser() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let servers = |status: McpStatus| Command::RecordMcpServers { task_id: "t".into(), generation: 1, servers: vec![mcp_server("linear", status)] };
    let opened = |sign_in: McpSignIn| Command::RecordMcpSignIn { task_id: "t".into(), generation: 1, server: "linear".into(), sign_in };
    let browser = || McpSignIn::Browser { url: "https://linear.app/oauth/authorize".into(), callback_port: Some(49200), state: Some("s".into()) };
    must(&mut s, Actor::System, servers(McpStatus::NeedsAuth));
    must(&mut s, Actor::System, opened(browser()));
    must(&mut s, Actor::System, servers(McpStatus::NeedsAuth));
    assert!(s.mcp[&AgentId::from("a")].sign_ins.contains_key("linear"), "still waiting for the user");
    must(&mut s, Actor::System, servers(McpStatus::Connected));
    assert!(s.mcp[&AgentId::from("a")].sign_ins.is_empty(), "Claude connected it: nothing left to finish");
    must(&mut s, Actor::System, opened(browser()));
    must(&mut s, Actor::System, opened(McpSignIn::Refused { error: "Server is disabled".into() }));
    assert!(s.mcp[&AgentId::from("a")].sign_ins.is_empty());
    must(&mut s, Actor::System, opened(browser()));
    must(&mut s, Actor::System, opened(McpSignIn::Connected));
    assert!(s.mcp[&AgentId::from("a")].sign_ins.is_empty());
}

/// The input Alondra queued itself for the task, with its words.
fn queued_unasked(s: &DomainState, id: &str) -> (String, String) {
    match s.lifecycle_journal[&TaskId::from(id)].last().cloned() {
        Some(LifecycleRecord::HostEnded { resume: Some(input), text: Some(text), .. }) => (input, text),
        other => panic!("Alondra told the session to go on: {other:?}"),
    }
}

// A host's end is not the user's decision: when a host that resumes the same session takes its
// place, the cut task goes on by itself, as a session reopens with `resume` on its next turn.
// Before, every such task held on «was interrupted when its session closed» until someone pressed
// «Carry on».
#[test]
fn a_host_that_ends_mid_turn_and_is_replaced_lets_the_task_go_on_without_a_card() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let ended = must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: true, ended_work: vec![], by_alondra: false });
    assert!(!s.interactions.keys().any(|id| id.0.starts_with(SESSION_ENDED_PREFIX)), "no card");
    assert!(ended.effects.iter().all(|e| !matches!(e, DomainEffect::NotifyUser { .. })), "and no notice");
    let (input, text) = queued_unasked(&s, "t");
    assert!(text.starts_with("Your session was cut off when its host stopped, and Alondra resumed it."), "{text}");
    let replayed = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap();
    assert_eq!(replayed.pending_inputs(), [&alondra_domain::InputId::new(input).unwrap()], "the resumed session is told to go on");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
    assert_aggregate_rules(&s);
}

/// An SDK task whose delivery the user replied to: the node's turn on the reply left a background
/// command running when it ended.
fn answering_the_reply_with_background_work(s: &mut DomainState) {
    sdk_turn(s, "t", "a");
    deliver(s, "t", "a", "result");
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    must(s, user(), Command::SubmitFeedback { merge_failure: None, attachments: vec![], presentation_id: "result".into(),
        targets: vec![DeliveryTarget::Text { text: "Done".into() }], body: "Why this way?".into(), instruction: Instruction::Reply });
    must(s, Actor::System, Command::TellNode { task_id: "t".into(), text: Instruction::Reply.message("Why this way?"), from_user: false });
    let replayed = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap();
    let reply = replayed.pending_inputs()[0].as_str().to_owned();
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: "turn-2".into(), input: reply },
        RuntimeObservation::WorkStarted { turn: "turn-2".into(), work: "claude-task:snapshots".into() },
    ] });
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-2".into(), failure: None, failed: None });
    assert_eq!(s.task_handed_back(&"t".into()), Some(Instruction::Reply), "the user waits on the node's answer");
}

// t-parcel-open-issues-widget: the user replied to its delivery, the node's turn on the reply left
// `bazel test` running in the background, and a daemon upgrade replaced its host. The delivery was
// under review, so nothing told the resumed session to go on and it waited 12 h for a notification
// no process would send. A host's end that cuts what the node does for the user resumes it, and
// the session is told the background work ended.
#[test]
fn a_host_that_ends_while_the_node_works_on_the_users_reply_resumes_it_and_names_the_work_that_ended() {
    let mut s = world();
    answering_the_reply_with_background_work(&mut s);
    let ended = must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: true,
        ended_work: vec!["b5sa3a3rd (bazelisk test //App/Tests:ParcelSnapshotTests)".into()], by_alondra: false });
    assert!(!s.interactions.keys().any(|id| id.0.starts_with(SESSION_ENDED_PREFIX)), "no card");
    assert!(ended.effects.iter().all(|e| !matches!(e, DomainEffect::NotifyUser { .. })), "and no notice");
    let (input, text) = queued_unasked(&s, "t");
    assert_eq!(text, "Your session was cut off when its host stopped, and Alondra resumed it. Check where you were and go on with your task: \
        deliver your result, or ask the user through a card when you need them.\n\nThis background work ended with that host and will not report back:\n\
        - b5sa3a3rd (bazelisk test //App/Tests:ParcelSnapshotTests)\nRun again what you still need and wait for its result.");
    let replayed = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap();
    assert_eq!(replayed.pending_inputs(), [&alondra_domain::InputId::new(input).unwrap()], "the resumed session is told to go on");
    assert_eq!(replayed.phase(), alondra_domain::TaskPhase::Delivered, "the delivery stays the one the user reviews");
    assert_aggregate_rules(&s);
}

// Found capturing the TUI for t-parcel-open-issues-widget: an SDK node ended its turn on the user's
// reply without presenting again, and the delivery never came back: the task read «Delivered»
// with nothing in the user's inbox. Its node phase never moves with its turns; the end of the
// turn that leaves its session resting on the delivery is the fact that brings it back.
#[test]
fn a_reply_the_node_of_the_sdk_answered_brings_the_same_delivery_back_once_its_session_rests() {
    let mut s = world();
    answering_the_reply_with_background_work(&mut s);
    assert_eq!(s.task_handed_back(&"t".into()), Some(Instruction::Reply), "its background work still runs for the reply");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkEnded { work: "claude-task:snapshots".into(), continuation: "claude-notification:snapshots".into() },
        RuntimeObservation::TurnResumed { turn: "turn-3".into(), continuation: "claude-notification:snapshots".into() },
    ] });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-3".into(), failure: None, failed: None });
    assert_eq!((s.tasks[&TaskId::from("t")].state, s.task_handed_back(&"t".into())), (TaskState::ForYourReview, None),
        "the same delivery waits for the user again");
    assert_aggregate_rules(&s);
}

// t-duo-parcel: the user replied to its delivery and the node's turn on the reply left two
// background commands running. Until it presented again, two hours later, the task read
// «Delivered, waits for your review» with the delivery out of the user's queue, while its node
// was working on the reply: either a card is in front of the user, or the node works.
#[test]
fn a_node_of_the_sdk_working_in_the_background_on_a_reply_reads_as_working_with_the_delivery_out_of_the_queue() {
    use alondra_domain::{LiveState, Step, Worker};
    let mut s = world();
    answering_the_reply_with_background_work(&mut s);
    assert_eq!(s.live_state(&"t".into()), Some(LiveState::Working(Worker::OwnWork)), "its background work runs for the reply");
    assert!(!pending_cards(&s).contains(&"result".to_owned()));
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkEnded { work: "claude-task:snapshots".into(), continuation: "claude-notification:snapshots".into() },
    ] });
    assert_eq!(s.live_state(&"t".into()), Some(LiveState::Working(Worker::Alondra(Step::Waking))), "its end wakes the node");
    assert!(!pending_cards(&s).contains(&"result".to_owned()));
    assert_aggregate_rules(&s);
}

// A domain whose node rests after it took the reply, with the delivery still out of the user's
// queue, as a release that never gave it back left it: the delivery is the user's again, on read.
#[test]
fn a_delivery_held_by_a_reply_its_resting_node_is_done_with_is_the_users_again() {
    let mut s = world();
    answering_the_reply_with_background_work(&mut s);
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkEnded { work: "claude-task:snapshots".into(), continuation: "claude-notification:snapshots".into() },
        RuntimeObservation::TurnResumed { turn: "turn-3".into(), continuation: "claude-notification:snapshots".into() },
    ] });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-3".into(), failure: None, failed: None });
    let reply = s.interactions[&InteractionId::from("result")].clone();
    let InteractionPayload::Delivery { feedback, .. } = &reply.payload else { panic!("a delivery") };
    s.interactions.get_mut(&InteractionId::from("result")).unwrap().revision = feedback.last().unwrap().at_revision;
    assert_eq!(s.task_handed_back(&"t".into()), None, "its node rests on nothing");
    assert_eq!(s.live_state(&"t".into()), alondra_domain::Several::new([InteractionId::from("result")]).map(alondra_domain::LiveState::WaitingUser));
    assert!(pending_cards(&s).contains(&"result".to_owned()), "the delivery is in front of the user: {:?}", pending_cards(&s));
}

/// An SDK task delivered while its turn runs; with `feedback`, that turn ended, the user asked it
/// of the delivery, and the node's turn on it runs.
fn delivered_through_the_sdk(s: &mut DomainState, feedback: Option<Instruction>) {
    sdk_turn(s, "t", "a");
    deliver(s, "t", "a", "result");
    let Some(instruction) = feedback else { return };
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    must(s, user(), Command::SubmitFeedback { merge_failure: None, attachments: vec![], presentation_id: "result".into(),
        targets: vec![DeliveryTarget::Text { text: "Done".into() }], body: "Do not guess the state".into(), instruction });
    must(s, Actor::System, Command::TellNode { task_id: "t".into(), text: instruction.message("Do not guess the state"), from_user: false });
    let replayed = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap();
    let told = replayed.pending_inputs()[0].as_str().to_owned();
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: "turn-2".into(), input: told },
    ] });
}
fn pending_cards(s: &DomainState) -> Vec<String> {
    super::read_index::inbox_items(s).into_iter().filter(|item| item.pending)
        .filter_map(|item| item.id.strip_prefix("interaction:").map(str::to_owned)).collect()
}
fn claude_asks(s: &mut DomainState, epoch: u64) -> Result<CommandOutcome, DomainError> {
    let options = ["Hide it", "Show it"].map(|label| QuestionOption { id: label.to_lowercase(), label: label.into(), description: None }).to_vec();
    exec(s, Actor::System, Command::RecordDecisionRequest {
        task_id: "t".into(), generation: 1, epoch, hold: Hold::Question, tool: Some("AskUserQuestion".into()), input: serde_json::json!({}), mcp_server: None,
        prompt: DecisionPrompt::Card { body: "What does the iPhone show without a state?".into(), options,
            card: CardSpec::new(CardKind::Decide, "Pick what the iPhone shows without a state") },
    })
}

// t-remove-fleet-3-ios: the user requested changes on its delivery and the node, working on them,
// asked the user a question. The domain took that delivery back a second time, the lifecycle refused
// it, and the question was denied without reaching the user.
#[test]
fn a_question_asked_while_working_on_requested_changes_reaches_the_user() {
    let mut s = world();
    delivered_through_the_sdk(&mut s, Some(Instruction::Changes));
    claude_asks(&mut s, 7).unwrap_or_else(|error| panic!("the question was refused: {error}"));
    let card = decision_card_id(&"t".into(), 1, 7);
    assert!(pending_cards(&s).contains(&card.0), "the user gets the question: {:?}", pending_cards(&s));
    assert!(s.interactions[&InteractionId::from("result")].withdrawn, "the delivery is not the user's to review while the node asks");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
    claude_asks(&mut s, 8).unwrap_or_else(|error| panic!("the second question was refused: {error}"));
    let handed_back = s.lifecycle_journal[&TaskId::from("t")].iter().filter(|record| matches!(record, LifecycleRecord::HandedBack { .. })).count();
    assert_eq!(handed_back, 1, "the delivery is taken back once");
    assert!(pending_cards(&s).contains(&decision_card_id(&"t".into(), 1, 8).0));
    assert_aggregate_rules(&s);
}

#[test]
fn a_question_asked_after_a_delivery_nobody_answered_withdraws_it() {
    let mut s = world();
    delivered_through_the_sdk(&mut s, None);
    claude_asks(&mut s, 7).unwrap_or_else(|error| panic!("the question was refused: {error}"));
    let card = decision_card_id(&"t".into(), 1, 7);
    assert!(pending_cards(&s).contains(&card.0), "the user gets the question: {:?}", pending_cards(&s));
    assert!(s.interactions[&InteractionId::from("result")].withdrawn, "the delivery is withdrawn");
    assert!(matches!(s.lifecycle_journal[&TaskId::from("t")].last(), Some(LifecycleRecord::HandedBack { delivery, .. }) if delivery.0 == "result"));
    assert_eq!(s.tasks[&TaskId::from("t")].final_presentation, None);
    assert_aggregate_rules(&s);
}

// Without a host that resumes the session, nothing would ever answer the reply: the delivery is the
// user's again, as when the node ends its turn on it.
#[test]
fn a_host_that_ends_while_the_node_works_on_the_users_reply_with_no_resume_gives_the_delivery_back() {
    let mut s = world();
    answering_the_reply_with_background_work(&mut s);
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: false, ended_work: vec![], by_alondra: false });
    assert!(matches!(s.lifecycle_journal[&TaskId::from("t")].last(), Some(LifecycleRecord::HostEnded { resume: None, .. })));
    assert_eq!((s.tasks[&TaskId::from("t")].state, s.task_handed_back(&"t".into())), (TaskState::ForYourReview, None),
        "the same delivery waits for the user again");
    assert_aggregate_rules(&s);
}

// A delivery the node rests on waits for the user: its host's end wakes nothing.
#[test]
fn a_host_that_ends_on_a_delivery_the_node_rests_on_tells_it_nothing() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    deliver(&mut s, "t", "a", "result");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: true, ended_work: vec![], by_alondra: false });
    assert!(matches!(s.lifecycle_journal[&TaskId::from("t")].last(), Some(LifecycleRecord::HostEnded { resume: None, .. })));
    assert!(!s.interactions.keys().any(|id| id.0.starts_with(SESSION_ENDED_PREFIX)));
    assert_aggregate_rules(&s);
}

// A session that ends again before a turn of it ended would only end the same way: the second
// time the task asks the user, and says why.
#[test]
fn a_resumed_session_that_ends_again_before_a_turn_ends_asks_the_user_with_the_reason() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: true, ended_work: vec![], by_alondra: false });
    let (input, _) = queued_unasked(&s, "t");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: "turn-2".into(), input },
    ] });
    let again = must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: true, ended_work: vec![], by_alondra: false });
    let card = s.interactions.keys().find(|id| id.0.starts_with(SESSION_ENDED_PREFIX)).expect("the task asks this time").clone();
    assert_eq!(s.interactions[&card].card.why, SESSION_ENDED_AGAIN_WHY);
    assert_eq!(options(&s, &card), [CARRY_ON, CANCEL_TASK]);
    assert!(again.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { interaction_id, .. } if *interaction_id == card)));
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
    assert_aggregate_rules(&s);
}

// A turn that ended between two ends of the host is progress: the next end resumes by itself again.
#[test]
fn a_resumed_session_that_finished_a_turn_is_resumed_again_without_a_card() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: true, ended_work: vec![], by_alondra: false });
    let (input, _) = queued_unasked(&s, "t");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: "turn-2".into(), input },
    ] });
    let case = woken(&mut s, "t", "turn-2", "turn-2b");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-2b".into(), failure: None, failed: None });
    must(&mut s, orchestrator(), Command::SettleStall { case_id: case.as_str().into(), task_id: "t".into(), outcome: StallOutcome::Instruct { text: "Present it".into() } });
    let Some(LifecycleRecord::Answered { input, .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("answered") };
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: "turn-3".into(), input },
    ] });
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: true, ended_work: vec![], by_alondra: false });
    queued_unasked(&s, "t");
    assert!(!s.interactions.keys().any(|id| id.0.starts_with(SESSION_ENDED_PREFIX)));
    assert_aggregate_rules(&s);
}

// 2026-10-27, a Codex node: the session a restart resumed worked for 8 minutes in one turn, then
// its host ended because its owner failed on an error of Alondra's own, with a message handed and
// not read yet. An end Alondra caused says nothing about the session: it resumes and gets the
// message again, and nobody is asked. A third end in a row with no finished turn asks the user, so
// an error of Alondra's own that repeats does not resume the session for ever.
#[test]
fn a_resumed_session_alondra_ends_again_goes_on_with_its_message_until_its_ends_repeat() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let ended = |s: &mut DomainState| must(s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: true, ended_work: vec![], by_alondra: true });
    let doubted = |s: &mut DomainState, input: String| must(s, Actor::System, Command::RecordDoubtfulHandoff { task_id: "t".into(), generation: 1, input,
        text: "Keep the journal's old rows".into(), resumes: true, by_alondra: true });
    ended(&mut s);
    let (input, _) = queued_unasked(&s, "t");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: "turn-2".into(), input },
    ] });
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Keep the journal's old rows".into(), from_user: false });
    let told = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap().pending_inputs()[0].as_str().to_owned();
    ended(&mut s);
    let again = doubted(&mut s, told);
    assert!(again.effects.iter().all(|e| !matches!(e, DomainEffect::NotifyUser { .. })), "nobody is asked");
    assert!(!s.interactions.keys().any(|id| id.0.starts_with(HANDOFF_DOUBT_PREFIX) || id.0.starts_with(SESSION_ENDED_PREFIX)));
    let Some(LifecycleRecord::DoubtResent { resent, text: Some(text), .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("the message goes again") };
    assert!(text.ends_with("Keep the journal's old rows"), "{text}");
    ended(&mut s);
    let third = doubted(&mut s, resent);
    let card = s.interactions.keys().find(|id| id.0.starts_with(HANDOFF_DOUBT_PREFIX)).expect("the third end in a row asks").clone();
    assert!(third.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { interaction_id, .. } if *interaction_id == card)));
    assert_aggregate_rules(&s);
}

// The message handed to the host that ended may have reached Claude or not. The resumed session
// gets it again with a note that its conversation may already have it: Claude reads its own
// transcript, so the fact is there, and nobody is asked.
#[test]
fn a_message_the_ended_host_may_not_have_read_goes_again_to_the_resumed_session_flagged() {
    let mut s = world();
    let turn = interrupted_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: turn.as_str().into(), option_id: CARRY_ON.into(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text("Add the tests first".into()))]) });
    let Some(LifecycleRecord::Answered { input, text: Some(text), .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("answered") };
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: true, ended_work: vec![], by_alondra: false });
    let doubted = must(&mut s, Actor::System, Command::RecordDoubtfulHandoff { task_id: "t".into(), generation: 1, input, text: text.clone(), resumes: true, by_alondra: false });
    assert!(doubted.effects.iter().all(|e| !matches!(e, DomainEffect::NotifyUser { .. })), "nobody is asked");
    assert!(!s.interactions.keys().any(|id| id.0.starts_with(HANDOFF_DOUBT_PREFIX) || id.0.starts_with(SESSION_ENDED_PREFIX)));
    let Some(LifecycleRecord::DoubtResent { resent, text: Some(again), .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("sent again") };
    assert!(again.starts_with("Your session was cut off when its host stopped") && again.ends_with(&text), "{again}");
    let replayed = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap();
    assert_eq!(replayed.pending_inputs(), [&alondra_domain::InputId::new(resent).unwrap()]);
    assert_aggregate_rules(&s);
}

// A session that never opened read nothing: however many hosts end before it opens, its mission
// goes again to the next one by itself, with one note, and nobody is asked.
#[test]
fn a_mission_whose_session_never_opened_goes_again_after_every_host_that_ends() {
    let mut s = world();
    task(&mut s, "t", None);
    must(&mut s, Actor::System, Command::AdmitExecution { provider: Default::default(),
        task_id: "t".into(), agent_id: "a".into(), conversation: "conversation-t".into(), instance: "instance-t".into(), host: "sdk-host".into(), model: None,
        account: None, leaves_limit: None,
    });
    let LifecycleRecord::Planned { input: mut pending } = s.lifecycle_journal[&TaskId::from("t")][0].clone() else { panic!("planned first") };
    let mut text = "Write the release notes".to_owned();
    for _ in 0..3 {
        must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: true, ended_work: vec![], by_alondra: false });
        let doubted = must(&mut s, Actor::System, Command::RecordDoubtfulHandoff { task_id: "t".into(), generation: 1, input: pending.clone(), text: text.clone(), resumes: true, by_alondra: false });
        assert!(doubted.effects.iter().all(|e| !matches!(e, DomainEffect::NotifyUser { .. })), "nobody is asked");
        let Some(LifecycleRecord::DoubtResent { resent, text: Some(again), .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("sent again") };
        assert_eq!(again.matches("Your session was cut off when its host stopped").count(), 1, "{again}");
        assert!(again.ends_with("Write the release notes"), "{again}");
        let replayed = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap();
        assert_eq!(replayed.resource(), Some(alondra_domain::ResourceEvidence::Reserved));
        assert_eq!(replayed.pending_inputs(), [&alondra_domain::InputId::new(resent.clone()).unwrap()]);
        (pending, text) = (resent, again);
    }
    assert!(!s.interactions.keys().any(|id| id.0.starts_with(HANDOFF_DOUBT_PREFIX)));
    assert_aggregate_rules(&s);
}

// An execution no host runs, because the daemon could not take it over, holds on a card that says
// why. «Try again» lets it go on, told to carry on; «Cancel the task» ends it.
#[test]
fn a_task_whose_execution_is_lost_holds_on_a_card_with_the_reason_until_the_user_retries_or_cancels() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let lost = must(&mut s, Actor::System, Command::RecordHostLost { task_id: "t".into(), reason: "its host refused to be taken over and did not stop: pid 7 is still alive".into() });
    let card = s.interactions.keys().find(|id| id.0.starts_with(HOST_LOST_PREFIX)).expect("the task has a card").clone();
    let shown = &s.interactions[&card];
    assert_eq!(shown.card.need, "“Task t” stopped: Alondra could not resume its session");
    assert_eq!(shown.card.content.as_ref().map(|content| content.body.as_str()), Some("Its host refused to be taken over and did not stop: pid 7 is still alive"), "the details carry the reason");
    assert_eq!(options(&s, &card), [RETAKE, CANCEL_TASK]);
    assert!(lost.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { interaction_id, .. } if *interaction_id == card)));
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
    must(&mut s, Actor::System, Command::RecordHostLost { task_id: "t".into(), reason: "again".into() });
    assert_eq!(s.interactions.keys().filter(|id| id.0.starts_with(HOST_LOST_PREFIX)).count(), 1, "one card while it waits");
    answer(&mut s, &card, RETAKE);
    assert!(matches!(s.lifecycle_journal[&TaskId::from("t")].last(),
        Some(LifecycleRecord::Resumed { card: resumed, text: Some(text), .. }) if *resumed == card && text.starts_with("Your session was cut off")));
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
    assert_aggregate_rules(&s);

    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: false, ended_work: vec![], by_alondra: false });
    let asked = s.interactions.keys().find(|id| id.0.starts_with(SESSION_ENDED_PREFIX)).expect("the task asks how to go on").clone();
    must(&mut s, Actor::System, Command::RecordHostLost { task_id: "t".into(), reason: "launching the host that resumes the session failed".into() });
    let open: Vec<&InteractionId> = s.interactions.values().filter(|i| s.open(i)).map(|i| &i.id).collect();
    assert_eq!(open, [&asked], "the card it waited on becomes the one that says it could not resume: one card for the task");
    assert_eq!(options(&s, &asked), [RETAKE, CANCEL_TASK]);
    assert_eq!(s.interactions[&asked].card.content.as_ref().map(|content| content.body.as_str()), Some("Launching the host that resumes the session failed"));
    answer(&mut s, &asked, RETAKE);
    assert!(matches!(s.lifecycle_journal[&TaskId::from("t")].last(),
        Some(LifecycleRecord::Answered { card, text: Some(text), .. }) if *card == asked && text.starts_with("Your session was cut off")));
    assert_aggregate_rules(&s);

    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordHostLost { task_id: "t".into(), reason: "the node plugin was not found".into() });
    let card = s.interactions.keys().find(|id| id.0.starts_with(HOST_LOST_PREFIX)).unwrap().clone();
    answer(&mut s, &card, CANCEL_TASK);
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Closing, "it ends, closing its execution first");
    assert_aggregate_rules(&s);
}

// A turn whose result is not a success ends as failed, with its code. Here such a turn
// waits for no one: its card names the error, and an interrupt the user asked for still comes first.
#[test]
fn a_turn_claude_ended_as_failed_asks_how_to_go_on_and_names_its_error() {
    use crate::views::text::Lang;
    let mut s = world();
    let card = sdk_turn(&mut s, "t", "a");
    let ended = must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: Some("error_max_turns".into()) });
    let failed = &s.interactions[&InteractionId::from(card.as_str())];
    assert_eq!(failed.card.need, "Claude's turn ended with an error (`error_max_turns`): tell it how to go on");
    assert_eq!(options(&s, &card.as_str().into()), [CARRY_ON, CANCEL_TASK]);
    assert!(ended.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { interaction_id, .. } if interaction_id.0 == card)));
    assert!(held(&s).is_empty(), "nothing waits for the API");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
    let en = turn_end_shown(&s, "t", Lang::En);
    assert!(en.contains("Claude's turn ended with an error (`error_max_turns`): tell it how to go on"), "{en}");
    let es = turn_end_shown(&s, "t", Lang::Es);
    assert!(es.contains("El turno de Claude terminó con un error (`error_max_turns`): dile cómo seguir"), "{es}");
    assert!(es.contains("Dile cómo seguir, escríbele qué hacer o cancela la tarea."), "{es}");
    assert_aggregate_rules(&s);

    let mut s = world();
    let card = sdk_turn(&mut s, "t", "a");
    must(&mut s, user(), Command::InterruptTurn { agent_id: "a".into() });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: Some("error_during_execution".into()) });
    assert_eq!(s.interactions[&InteractionId::from(card.as_str())].card.why, INTERRUPTED_WHY, "the interrupt explains the end");

    let mut s = world();
    let card = sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(),
        failure: Some(SdkApiFailure { error: "billing_error".into(), status: Some(400), resets_at: None }), failed: Some("success_is_error".into()) });
    assert_eq!(s.interactions[&InteractionId::from(card.as_str())].card.why, REFUSED_WHY, "the API's failure comes first");
}

/// A task whose node rests on its turn card, told `text` and waiting for Claude to read it.
fn told(s: &mut DomainState, text: &str) -> String {
    let card = interrupted_turn(s, "t", "a");
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    must(s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.as_str().into(), option_id: CARRY_ON.into(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text(text.into()))]) });
    let Some(LifecycleRecord::Answered { input, .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("answered") };
    input
}
fn dropped(s: &mut DomainState) -> (InteractionId, String) {
    let input = told(s, "Add the tests first");
    let Some(LifecycleRecord::Answered { text: Some(text), .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("answered") };
    let outcome = must(s, Actor::System, Command::RecordDroppedInput { task_id: "t".into(), generation: 1, input, text: text.clone() });
    let card = s.interactions.keys().find(|id| id.0.starts_with(DROPPED_PREFIX)).expect("the drop asks the user").clone();
    assert!(outcome.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { interaction_id, .. } if *interaction_id == card)));
    (card, text)
}

#[test]
fn a_message_claude_never_reads_leaves_the_queue_and_asks_whether_to_send_it_again() {
    use crate::views::text::Lang;
    let mut s = world();
    let (card, text) = dropped(&mut s);
    let replayed = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap();
    assert!(replayed.pending_inputs().is_empty(), "nothing waits for Claude to read it");
    let asked = &s.interactions[&card];
    assert_eq!(asked.card.need, "Claude didn't get to read your message");
    assert_eq!(asked.card.content.as_ref().map(|content| content.body.as_str()), Some(text.as_str()), "the card carries the whole message");
    assert_eq!(options(&s, &card), [SEND_AGAIN, DISCARD]);
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
    let es = turn_end_shown(&s, "t", Lang::Es);
    assert!(es.contains("Claude no llegó a leer tu mensaje"), "{es}");
    let shown = {
        use crate::views::{Client, RenderContext, Transport, build, live::LiveSnapshot};
        let index = ReadIndex::build(&s);
        let live = LiveSnapshot::default();
        let view = build(&format!("card:{}", card.0), RenderContext { domain: &s, index: &index, live: &live, client: Client::Tui, lang: Lang::Es,
            transport: Transport::Loopback, cursor: "test", rendered_at: "" }).unwrap();
        serde_json::to_string(&view).unwrap()
    };
    assert!(shown.contains("Enviar otra vez") && shown.contains("Descartar") && shown.contains("Add the tests first"), "{shown}");
    assert_eq!(exec(&mut s, Actor::System, Command::RecordDroppedInput { task_id: "t".into(), generation: 1, input: "other".into(), text: "other".into() })
        .unwrap_err().code, "conflict", "an input that is not pending is never dropped");
    assert_aggregate_rules(&s);

    answer(&mut s, &card, SEND_AGAIN);
    let Some(LifecycleRecord::Answered { input: again, text: Some(sent), .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("sent again") };
    assert_eq!(sent, text, "the same words go again");
    let replayed = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap();
    assert_eq!(replayed.pending_inputs(), [&alondra_domain::InputId::new(again).unwrap()]);
    assert!(!s.open(&s.interactions[&card]), "the card is answered");
    assert_aggregate_rules(&s);
}

#[test]
fn a_message_claude_never_reads_can_be_let_go_and_the_orchestrator_settles_how_it_goes_on() {
    let mut s = world();
    let (card, _) = dropped(&mut s);
    answer(&mut s, &card, DISCARD);
    assert!(matches!(last(&s, "t"), LifecycleRecord::DroppedDiscarded { stalled: true, .. }));
    let replayed = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap();
    assert!(replayed.pending_inputs().is_empty());
    let next = s.interactions.values().find(|i| is_stall_case(i) && s.open(i)).expect("the orchestrator settles how it goes on").id.clone();
    assert_eq!(options(&s, &next), [STALL_INSTRUCT, STALL_CLOSE]);
    assert!(!s.interactions.values().any(|i| i.id.0.starts_with(TURN_ENDED_PREFIX) && s.open(i)), "no card asks the user");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
    assert_aggregate_rules(&s);

    let mut s = world();
    let (card, _) = dropped(&mut s);
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.clone(), option_id: DISCARD.into(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text("Write the docs instead".into()))]) });
    assert!(matches!(last(&s, "t"), LifecycleRecord::Answered { text: Some(text), .. } if text == "Write the docs instead"), "what they wrote goes in its place");
    assert_aggregate_rules(&s);
}

/// An SDK task whose node declared work after the merge and whose PR was accepted and merged while
/// its turn runs that work.
fn working_after_the_merge(s: &mut DomainState) {
    sdk_turn(s, "t", "a");
    must(s, node("a"), Command::RegisterPr {
        task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(), number: 1,
        head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    must(s, node("a"), Command::DeclarePostMergeWork { task_id: "t".into(), reason: Some("release.sh publishes 0.3.0".into()), waits_for: vec![] });
    let targets = vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }];
    must(s, node("a"), Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: "t".into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets.clone(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
    });
    must(s, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets });
    must(s, Actor::System, Command::ObservePrState {
        pr_id: "pr".into(), head: "head1".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()),
    });
    assert!(s.post_merge_owed(&s.tasks[&TaskId::from("t")]));
}

/// The sandbox's «Publish the release once it is merged», 24 October: its PR merged while its node
/// ran the release, and the task read «Delivered», as if it waited for a review. It works, after
/// the merge, on the phone, the Mac and the terminal.
#[test]
fn a_task_of_the_sdk_that_works_after_its_merge_reads_so_on_every_client() {
    use crate::views::{Client, RenderContext, Transport, build, live::LiveSnapshot, text::Lang};
    let mut s = world();
    working_after_the_merge(&mut s);
    assert_eq!(s.live_state(&"t".into()), Some(alondra_domain::LiveState::Working(alondra_domain::Worker::AfterMerge)));
    let index = ReadIndex::build(&s);
    let live = LiveSnapshot::default();
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        for (lang, said) in [(Lang::En, "Finishing after the merge"), (Lang::Es, "Terminando tras el merge")] {
            let view = build("task:t", RenderContext { domain: &s, index: &index, live: &live, client, lang,
                transport: Transport::Loopback, cursor: "test", rendered_at: "" }).unwrap();
            let shown = serde_json::to_string(&view).unwrap();
            assert!(shown.contains(said) && !shown.contains("\"Delivered\"") && !shown.contains("\"Entregada\""), "{client:?}: {shown}");
        }
    }
    assert_aggregate_rules(&s);
}

/// The user forces the running turn of `t` to stop and its host ends.
fn force_stopped(s: &mut DomainState) -> (CommandOutcome, InteractionId) {
    must(s, user(), Command::InterruptTurn { agent_id: "a".into() });
    must(s, Actor::System, Command::RecordInterruptAccepted { task_id: "t".into(), generation: 1, turn: "turn-1".into() });
    must(s, user(), Command::ForceStopTurn { agent_id: "a".into() });
    let ended = must(s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: true, ended_work: vec![], by_alondra: false });
    let card = s.interactions.keys().find(|id| id.0.starts_with(SESSION_ENDED_PREFIX)).expect("the stopped work after the merge asks").clone();
    (ended, card)
}

#[test]
fn a_host_force_stopped_during_the_work_after_the_merge_tells_the_user_and_offers_the_ways_out() {
    use crate::views::text::Lang;
    let mut s = world();
    working_after_the_merge(&mut s);
    let (ended, card) = force_stopped(&mut s);
    assert_eq!(s.interactions[&card].card.need, "You stopped “Task t” during its work after the merge: tell it how to go on");
    assert_eq!(s.interactions[&card].card.why, POST_MERGE_STOPPED_WHY);
    assert_eq!(s.interactions[&card].card.content.as_ref().map(|content| content.body.as_str()), Some("release.sh publishes 0.3.0"));
    assert_eq!(options(&s, &card), [CARRY_ON, CLOSE_TASK]);
    assert!(ended.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { interaction_id, .. } if *interaction_id == card)), "the user hears of it");
    assert!(s.open(&s.interactions[&card]));
    let es = turn_end_shown(&s, "t", Lang::Es);
    assert!(es.contains("Paraste «Task t» durante su trabajo tras el merge: dile cómo seguir"), "{es}");
    let shown = card_shown(&s, &card, Lang::Es);
    assert!(shown.contains("\"label\":\"Cerrar la tarea\"") && shown.contains("\"label\":\"Que siga\""), "{shown}");
    assert_aggregate_rules(&s);

    answer(&mut s, &card, CARRY_ON);
    let Some(LifecycleRecord::Answered { text: Some(text), .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("the answer goes to the resumed session") };
    assert!(text.contains("release.sh publishes 0.3.0") && text.contains("post_merge_work"), "{text}");
    assert!(s.post_merge_owed(&s.tasks[&TaskId::from("t")]), "carrying on keeps the work owed");
    assert_aggregate_rules(&s);
}

#[test]
fn closing_the_task_on_that_card_ends_the_work_after_the_merge_and_the_task_finishes_with_its_node() {
    let mut s = world();
    working_after_the_merge(&mut s);
    let (_, card) = force_stopped(&mut s);
    answer(&mut s, &card, CLOSE_TASK);
    assert!(!s.post_merge_owed(&s.tasks[&TaskId::from("t")]));
    assert!(!s.open(&s.interactions[&card]));
    assert!(matches!(s.lifecycle_journal[&TaskId::from("t")].last(), Some(LifecycleRecord::PostMergeEnded { fact: PostMergeEnd::UserDecision, .. })));
    assert_aggregate_rules(&s);
    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: Some("a".into()) });
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Done);
    assert_aggregate_rules(&s);
}

#[test]
fn a_host_that_crashes_during_the_work_after_the_merge_resumes_it_or_asks() {
    let mut s = world();
    working_after_the_merge(&mut s);
    let ended = must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: true, ended_work: vec![], by_alondra: false });
    assert!(!s.interactions.keys().any(|id| id.0.starts_with(SESSION_ENDED_PREFIX)), "a resumed session goes on by itself");
    assert!(!ended.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { .. })));
    let Some(LifecycleRecord::HostEnded { resume: Some(_), text: Some(text), .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("resumed") };
    assert!(text.contains("release.sh publishes 0.3.0") && text.contains("post_merge_work"), "{text}");
    assert_aggregate_rules(&s);

    let mut s = world();
    working_after_the_merge(&mut s);
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: false, ended_work: vec![], by_alondra: false });
    let card = s.interactions.keys().find(|id| id.0.starts_with(SESSION_ENDED_PREFIX)).expect("no session takes its place: the user is asked").clone();
    assert_eq!(s.interactions[&card].card.why, POST_MERGE_ENDED_WHY);
    assert_eq!(options(&s, &card), [CARRY_ON, CLOSE_TASK]);
    assert_aggregate_rules(&s);
}

#[test]
fn a_pause_recorded_before_pauses_named_their_owner_waits_on_who_its_card_names() {
    let task = TaskId::from("t");
    let paused = |card: &str| -> alondra_domain::TaskLifecycle {
        let journal: Vec<LifecycleRecord> = [r#"{"kind":"planned","input":"Start"}"#.to_owned(), format!(r#"{{"kind":"paused","card":"{card}"}}"#)]
            .iter()
            .map(|json| serde_json::from_str(json).expect("a journal of an earlier build decodes"))
            .collect();
        replay(&task, &journal).expect("it replays")
    };
    let waits = |card: &str| paused(card).live();
    assert_eq!(waits("claude-usage-limit-t-3"), Some(alondra_domain::LiveState::WaitingCapacity(alondra_domain::Capacity::Usage)));
    assert_eq!(waits("claude-usage-limit-until-1767225600-t-3"), Some(alondra_domain::LiveState::WaitingCapacity(alondra_domain::Capacity::Usage)));
    assert_eq!(waits("claude-outage-t-3"), Some(alondra_domain::LiveState::WaitingCapacity(alondra_domain::Capacity::ClaudeApi)));
    for card in ["claude-signed-out-t-3", "host-lost-t-3", "mcp-sign-in-t-3"] {
        assert_eq!(waits(card), alondra_domain::Several::new([InteractionId(card.into())]).map(alondra_domain::LiveState::WaitingUser), "{card} waits on the user");
    }
}

#[test]
fn a_pause_keeps_its_owner_in_the_journal() {
    let record = LifecycleRecord::Paused { card: "claude-signed-out-t-3".into(), owner: Some(PauseOwner::Usage) };
    let json = serde_json::to_string(&record).unwrap();
    assert_eq!(json, r#"{"kind":"paused","card":"claude-signed-out-t-3","owner":"usage"}"#);
    let journal = [LifecycleRecord::Planned { input: "Start".into() }, serde_json::from_str(&json).unwrap()];
    assert_eq!(replay(&TaskId::from("t"), &journal).unwrap().live(), Some(alondra_domain::LiveState::WaitingCapacity(alondra_domain::Capacity::Usage)), "the owner it names wins over its card");
}


fn mixed_default_accounts(s: &mut DomainState, codex_state: crate::accounts::AccountState) {
    use crate::accounts::*;
    let accounts=[Provider::Claude,Provider::Codex].into_iter().map(|provider|AccountStatus {provider,id:AccountId::default_account(),name:format!("{provider:?} personal"),state:if provider==Provider::Codex {codex_state} else {AccountState::SignedIn},native_identity: None, email:None,plan:None,billing:AccountBilling::default(),sign_ins:0}).collect();
    must(s,Actor::System,Command::RecordAccounts {accounts,reserve:crate::usage::default_reserve()});
}

#[test]
fn provider_qualified_sign_out_grouping_and_answers_never_cross_equal_default_slugs() {
    use crate::accounts::{AccountState,Provider};
    let mut s=world(); mixed_default_accounts(&mut s,AccountState::SignedIn);
    sdk_turn_on_provider(&mut s,Provider::Claude,"t","a","turn-1",None);
    sdk_turn_on_provider(&mut s,Provider::Codex,"u","b","turn-1",None);
    fail_turn(&mut s,"u","turn-1","authentication_failed",Some(401));
    let state=|s:&DomainState,provider|s.accounts.iter().find(|account|account.provider==provider).unwrap().state;
    assert_eq!(state(&s,Provider::Claude),AccountState::SignedIn); assert_eq!(state(&s,Provider::Codex),AccountState::SignedOut);
    mixed_default_accounts(&mut s,AccountState::SignedIn);
    assert_eq!(state(&s,Provider::Claude),AccountState::SignedIn,"a held Code sign-in never marks Claude signed out");
    assert_eq!(state(&s,Provider::Codex),AccountState::SignedOut,"a cached native sign-in does not prove the held Code account works");
    fail_turn(&mut s,"t","turn-1","authentication_failed",Some(401));
    assert_eq!(listed_holds(&s).len(),2,"one sign-in card per qualified default account");
    let code=s.claude_held().into_iter().find(|held|held.provider==Provider::Codex).unwrap().card;
    must(&mut s,user(),Command::AnswerQuestion {attachments:vec![],interaction_id:code,option_id:SIGNED_IN.into(),inputs:BTreeMap::new()});
    turn_on_resumed(&mut s,"u","turn-2");
    must(&mut s,Actor::System,Command::RecordRuntimeFacts {task_id:"u".into(),generation:1,facts:vec![RuntimeObservation::ModelAnswered]});
    assert_eq!(state(&s,Provider::Codex),AccountState::SignedIn); assert_eq!(state(&s,Provider::Claude),AccountState::SignedOut);
    assert_eq!(held(&s),[("t".into(),ClaudeHold::SignedOut)]);
}

#[test]
fn a_codex_outage_probe_and_answer_do_not_release_claudes_outage() {
    use crate::accounts::{AccountState,Provider};
    let mut s=world(); mixed_default_accounts(&mut s,AccountState::SignedIn);
    sdk_turn_on_provider(&mut s,Provider::Claude,"t","a","turn-1",None); sdk_turn_on_provider(&mut s,Provider::Codex,"u","b","turn-1",None);
    fail_turn(&mut s,"t","turn-1","overloaded_error",Some(503)); fail_turn(&mut s,"u","turn-1","overloaded_error",Some(503));
    assert_eq!(listed_holds(&s).len(),2,"outages of different services never group together");
    must(&mut s,Actor::System,Command::ProbeClaudeAccess {provider:Provider::Codex,hold:ClaudeHold::Outage,account:None});
    assert_eq!(held(&s),[("t".into(),ClaudeHold::Outage)]);
    turn_on_resumed(&mut s,"u","turn-2");
    must(&mut s,Actor::System,Command::RecordRuntimeFacts {task_id:"u".into(),generation:1,facts:vec![RuntimeObservation::ModelAnswered]});
    assert_eq!(held(&s),[("t".into(),ClaudeHold::Outage)]);
}

#[test]
fn quota_never_changes_the_provider_of_a_started_legacy_execution_even_when_default_slugs_match() {
    use crate::accounts::{AccountState,Provider};
    let mut s=world();mixed_default_accounts(&mut s,AccountState::SignedIn);sdk_turn(&mut s,"t","a");usage_turn(&mut s,"t",4_102_444_800);
    let card=s.held_by_limit(&"t".into()).unwrap().card;
    let move_to=|provider|Command::AdmitExecution{provider,task_id:"t".into(),agent_id:"a".into(),conversation:"new-conversation".into(),instance:"new-instance".into(),host:"alondra-host".into(),model:None,account:None,leaves_limit:Some(card.clone())};
    assert_eq!(exec(&mut s,Actor::System,move_to(Provider::Claude)).unwrap_err().code,"invalid_input");
    let records=s.lifecycle_journal.clone();
    assert_eq!(exec(&mut s,Actor::System,move_to(Provider::Codex)).unwrap_err().code,"active_account_immutable");
    assert_eq!(s.lifecycle_journal,records);assert_eq!(s.tasks[&TaskId::from("t")].account_key(),crate::accounts::AccountKey::native(Provider::Claude));
    assert!(s.held_by_limit(&"t".into()).is_some());assert!(!s.interactions[&card].withdrawn);
}


#[test]
fn legacy_probe_wire_defaults_to_claude_and_codex_is_explicit() {
    let legacy=Command::ProbeClaudeAccess {provider:crate::accounts::Provider::Claude,hold:ClaudeHold::Usage,account:None};
    let value=serde_json::to_value(&legacy).unwrap(); assert!(value.get("provider").is_none()); assert_eq!(serde_json::from_value::<Command>(value).unwrap(),legacy);
    let code=Command::ProbeClaudeAccess {provider:crate::accounts::Provider::Codex,hold:ClaudeHold::Usage,account:None};
    assert_eq!(serde_json::to_value(&code).unwrap()["provider"],"codex");
}


#[test]
fn a_proven_new_identity_does_not_inherit_a_previous_identity_auth_hold() {
    use crate::accounts::{AccountKey, AccountState, Provider};
    for provider in [Provider::Claude, Provider::Codex] {
        let mut state = world();
        mixed_default_accounts(&mut state, AccountState::SignedIn);
        let key = AccountKey::native(provider);
        let mut seen = state.accounts.clone();
        seen.iter_mut().find(|account| account.provider == provider).unwrap().email = Some("first@example.invalid".into());
        must(&mut state, Actor::System, Command::RecordAccounts { accounts: seen, reserve: crate::usage::default_reserve() });
        sdk_turn_on_provider(&mut state, provider, "t", "a", "turn-1", None);
        fail_turn(&mut state, "t", "turn-1", "authentication_failed", Some(401));
        let hold = state.claude_held().into_iter().find(|held| held.task == TaskId::from("t")).unwrap().card;
        state.interactions.get_mut(&hold).unwrap().card.cause = None;
        let journal = state.lifecycle_journal[&TaskId::from("t")].clone();
        let mut seen = state.accounts.clone();
        let account = seen.iter_mut().find(|account| account.provider == provider).unwrap();
        account.state = AccountState::Unavailable; account.email = None;
        must(&mut state, Actor::System, Command::RecordAccounts { accounts: seen, reserve: crate::usage::default_reserve() });
        let mut seen = state.accounts.clone();
        seen.iter_mut().find(|account| account.provider == provider).unwrap().state = AccountState::SignedIn;
        must(&mut state, Actor::System, Command::RecordAccounts { accounts: seen, reserve: crate::usage::default_reserve() });
        let account = state.accounts.iter().find(|account| account.provider == provider).unwrap();
        assert_eq!((account.state, account.sign_ins), (AccountState::SignedOut, 0), "a transient unavailable observation cannot turn cached sign-in into recovery proof");
        let mut seen = state.accounts.clone();
        let account = seen.iter_mut().find(|account| account.provider == provider).unwrap();
        account.state = AccountState::SignedIn;
        account.email = Some("second@example.invalid".into());
        must(&mut state, Actor::System, Command::RecordAccounts { accounts: seen, reserve: crate::usage::default_reserve() });
        let account = state.accounts.iter().find(|account| account.provider == provider).unwrap();
        assert_eq!((account.state, account.sign_ins), (AccountState::SignedIn, 1));
        let seen = state.accounts.clone();
        must(&mut state, Actor::System, Command::RecordAccounts { accounts: seen, reserve: crate::usage::default_reserve() });
        assert_eq!(state.accounts.iter().find(|account| account.provider == provider).unwrap().state, AccountState::SignedIn);
        assert_eq!(state.lifecycle_journal[&TaskId::from("t")], journal, "an identity change never resumes or transfers the old node");
        assert!(!state.interactions[&hold].withdrawn);
        assert!(state.interactions[&hold].card.cause.is_some(), "a legacy hold gains its old credential cause only when identity replacement is proven");
        assert!(state.claude_held().is_empty(), "new identity probes cannot pick the old identity's task");
        let mut seen = state.accounts.clone();
        let account = seen.iter_mut().find(|account| account.provider == provider).unwrap();
        account.state = AccountState::Unavailable; account.email = None;
        must(&mut state, Actor::System, Command::RecordAccounts { accounts: seen, reserve: crate::usage::default_reserve() });
        let mut seen = state.accounts.clone();
        seen.iter_mut().find(|account| account.provider == provider).unwrap().state = AccountState::SignedIn;
        must(&mut state, Actor::System, Command::RecordAccounts { accounts: seen, reserve: crate::usage::default_reserve() });
        assert_eq!(state.accounts.iter().find(|account| account.provider == provider).unwrap().state, AccountState::SignedIn, "an unavailable B probe cannot reapply A's hold");
        assert_eq!(exec(&mut state, Actor::System, Command::ProbeClaudeAccess { provider, hold: ClaudeHold::SignedOut, account: Some(key.account.clone()) }).unwrap_err().code, "invalid_input", "automatic probes never resume authentication holds; user confirmation is required");
        assert_eq!(exec(&mut state, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: hold.clone(), option_id: SIGNED_IN.into(), inputs: BTreeMap::new() }).unwrap_err().code, "account_credentials_changed");
        assert_eq!(state.lifecycle_journal[&TaskId::from("t")], journal);
        assert_eq!(state.tasks[&TaskId::from("t")].account_key(), key);
    }
}


#[test]
fn same_identity_reauthentication_keeps_its_usage_wait_attached_to_the_existing_session() {
    use crate::accounts::{AccountKey, AccountState, Provider};
    for provider in [Provider::Claude, Provider::Codex] {
        let mut state = world(); mixed_default_accounts(&mut state, AccountState::SignedIn);
        let key = AccountKey::native(provider);
        let mut seen = state.accounts.clone();
        seen.iter_mut().find(|account| account.provider == provider).unwrap().email = Some("same@example.invalid".into());
        must(&mut state, Actor::System, Command::RecordAccounts { accounts: seen, reserve: crate::usage::default_reserve() });
        sdk_turn_on_provider(&mut state, provider, "t", "a", "turn-1", None);
        usage_turn(&mut state, "t", 1_800_000_000);
        must(&mut state, Actor::System, Command::RecordAccountUsage { provider, account: key.account.clone(), reading: crate::usage::UsageReading {
            windows: vec![crate::usage::WindowReading { kind: crate::usage::WindowKind::Weekly, used: crate::usage::Share::FULL, exhausted: true, resets_at: None }], plan: None, complete: false }, at: 1, day: "2026-10-26".into() });
        let execution = state.lifecycle_of(&TaskId::from("t")).unwrap().execution().cloned();
        let quota = state.account_usage.clone();
        let mut seen = state.accounts.clone();
        seen.iter_mut().find(|account| account.provider == provider).unwrap().state = AccountState::SignedOut;
        must(&mut state, Actor::System, Command::RecordAccounts { accounts: seen, reserve: crate::usage::default_reserve() });
        let mut seen = state.accounts.clone();
        seen.iter_mut().find(|account| account.provider == provider).unwrap().state = AccountState::SignedIn;
        must(&mut state, Actor::System, Command::RecordAccounts { accounts: seen, reserve: crate::usage::default_reserve() });
        assert_eq!(state.accounts.iter().find(|account| account.provider == provider).unwrap().sign_ins, 1);
        assert_eq!(state.claude_held().len(), 1, "same-identity renewal never orphans the current usage wait");
        assert_eq!(state.account_usage, quota, "reauthentication is not evidence the quota reset");
        must(&mut state, Actor::System, Command::ProbeClaudeAccess { provider, hold: ClaudeHold::Usage, account: Some(key.account.clone()) });
        assert_eq!(state.lifecycle_of(&TaskId::from("t")).unwrap().execution().cloned(), execution);
        assert_eq!(state.tasks[&TaskId::from("t")].account_key(), key);
    }
}

#[test]
fn typed_codex_holds_keep_creation_time_paid_authority_when_the_account_policy_changes() {
    use crate::accounts::{AccountBilling, AccountKey, AccountState, Provider};
    use crate::model_selection::{ModelProfile, TaskSelection};
    for originally_paid in [false, true] {
        let mut state = world(); mixed_default_accounts(&mut state, AccountState::SignedIn);
        let key = AccountKey::native(Provider::Codex);
        let billing = |paid| if paid { AccountBilling::PayPerUse } else { AccountBilling::default() };
        let record_policy = |state: &mut DomainState, paid| {
            let mut accounts = state.accounts.clone();
            accounts.iter_mut().find(|account| account.provider == Provider::Codex).unwrap().billing = billing(paid);
            must(state, Actor::System, Command::RecordAccounts { accounts, reserve: crate::usage::default_reserve() });
        };
        let record_creation = |state: &mut DomainState, task: &str, paid| {
            let mut selection = TaskSelection::automatic(ModelProfile::General);
            selection.account = Some(key.clone());
            if paid { selection.paid_account_grant = Some(key.clone()); }
            must(state, Actor::System, Command::RecordTaskSelection { task_id: task.into(), selection, pending: None });
        };
        record_policy(&mut state, originally_paid);
        sdk_turn_on_provider(&mut state, Provider::Codex, "t", "a", "turn-1", None);
        record_creation(&mut state, "t", originally_paid);
        usage_turn(&mut state, "t", 4_102_444_800);
        let card = state.interactions.values().find(|card| matches!(&card.scope, Scope::Task { task_id } if task_id.0 == "t") && ClaudeHold::of(card) == Some(ClaudeHold::Usage)).unwrap().id.clone();
        let journal = state.lifecycle_journal[&TaskId::from("t")].clone();
        let selection = state.tasks[&TaskId::from("t")].selection.clone();
        record_policy(&mut state, !originally_paid);
        assert!(state.claude_held().is_empty(), "a different current spending policy never authorizes recovery of this existing node");
        assert!(!state.grouped_away(&state.interactions[&card]), "the existing card remains independently visible");
        assert_eq!(exec(&mut state, Actor::System, Command::ProbeClaudeAccess { provider: Provider::Codex, hold: ClaudeHold::Usage, account: Some(key.account.clone()) }).unwrap_err().code, "not_found");
        sdk_turn_on_provider(&mut state, Provider::Codex, "u", "b", "turn-1", None);
        record_creation(&mut state, "u", !originally_paid);
        must(&mut state, Actor::System, Command::RecordRuntimeFacts { task_id: "u".into(), generation: 1, facts: vec![RuntimeObservation::ModelAnswered] });
        assert!(!state.interactions[&card].withdrawn, "a new node's answer cannot recover the old financial context");
        assert_eq!(state.lifecycle_journal[&TaskId::from("t")], journal);
        assert_eq!(state.tasks[&TaskId::from("t")].selection, selection, "no new paid consent or account pin is created");
        usage_turn(&mut state, "u", 4_102_444_800);
        let probe = exec(&mut state, Actor::System, Command::ProbeClaudeAccess { provider: Provider::Codex, hold: ClaudeHold::Usage, account: Some(key.account.clone()) });
        if originally_paid { assert!(probe.is_ok(), "the independently authorized managed target remains probeable"); }
        else { assert_eq!(probe.unwrap_err().code, "not_found", "the newer paid target also remains standalone"); }
        assert!(!state.interactions[&card].withdrawn, "a probe never substitutes another session's API credential identity");
        assert_eq!(state.lifecycle_journal[&TaskId::from("t")], journal);
        record_policy(&mut state, originally_paid);
        assert_eq!(state.claude_held().iter().any(|held| held.task == TaskId::from("t")), !originally_paid, "restored managed policy permits native proof; an original paid grant never proves grouped identity");
        assert!(!state.interactions[&card].withdrawn);
        assert_eq!(state.lifecycle_journal[&TaskId::from("t")], journal);
    }
}

#[test]
fn a_managed_codex_answer_never_heals_an_original_paid_target_with_the_same_public_labels() {
    use crate::accounts::{AccountBilling, AccountKey, AccountState, Provider};
    use crate::model_selection::{ModelProfile, TaskSelection};
    let mut state = world(); mixed_default_accounts(&mut state, AccountState::SignedIn);
    let key = AccountKey::native(Provider::Codex);
    let mut accounts = state.accounts.clone();
    let account = accounts.iter_mut().find(|account| account.provider == Provider::Codex).unwrap();
    account.email = Some("retained@example.invalid".into()); account.native_identity = Some("retained-workspace".into()); account.billing = AccountBilling::PayPerUse;
    must(&mut state, Actor::System, Command::RecordAccounts { accounts, reserve: crate::usage::default_reserve() });
    sdk_turn_on_provider(&mut state, Provider::Codex, "u", "b", "managed-turn", None);
    let mut managed = TaskSelection::automatic(ModelProfile::General); managed.account = Some(key.clone()); managed.paid_account_grant = Some(key.clone());
    must(&mut state, Actor::System, Command::RecordTaskSelection { task_id: "u".into(), selection: managed, pending: None });
    sdk_turn_on_provider(&mut state, Provider::Codex, "t", "a", "turn-1", None);
    let mut paid = TaskSelection::automatic(ModelProfile::General); paid.account = Some(key.clone()); paid.paid_account_grant = Some(key.clone());
    must(&mut state, Actor::System, Command::RecordTaskSelection { task_id: "t".into(), selection: paid, pending: None });
    usage_turn(&mut state, "t", 4_102_444_800);
    let card = state.interactions.values().find(|card| matches!(&card.scope, Scope::Task { task_id } if task_id.0 == "t") && ClaudeHold::of(card) == Some(ClaudeHold::Usage)).unwrap().id.clone();
    let journal = state.lifecycle_journal[&TaskId::from("t")].clone(); let selection = state.tasks[&TaskId::from("t")].selection.clone();
    assert!(!state.grouped_away(&state.interactions[&card]));
    must(&mut state, Actor::System, Command::RecordRuntimeFacts { task_id: "u".into(), generation: 1, facts: vec![RuntimeObservation::ModelAnswered] });
    assert!(!state.interactions[&card].withdrawn, "a managed source seal and equal Pay policy cannot identify an older API-key target");
    assert_eq!(exec(&mut state, Actor::System, Command::ProbeClaudeAccess { provider: Provider::Codex, hold: ClaudeHold::Usage, account: Some(key.account.clone()) }).unwrap_err().code, "not_found");
    assert_eq!(state.lifecycle_journal[&TaskId::from("t")], journal);
    assert_eq!(state.tasks[&TaskId::from("t")].selection, selection, "the original spending grant remains untouched");
}


#[test]
fn an_already_started_legacy_background_node_cannot_change_account_or_provider_at_sdk_handover(){
    use crate::accounts::{AccountId,Provider};
    let mut s=world();task(&mut s,"t",Some("a"));
    must(&mut s,Actor::System,Command::MoveNode{agent_id:"a".into(),event:NodeEvent::TurnStarted,at:1});
    assert!(s.tasks[&TaskId::from("t")].started);assert!(s.tasks[&TaskId::from("t")].selection.is_none());
    for (provider,account) in [(Provider::Codex,None),(Provider::Claude,Some(AccountId::parse("work").unwrap()))]{
        let revision=s.revision;let journal=s.lifecycle_journal.clone();
        let result=exec(&mut s,Actor::System,Command::AdmitExecution{provider,task_id:"t".into(),agent_id:"b".into(),conversation:"session".into(),instance:"instance".into(),host:"alondra-host".into(),model:None,account,leaves_limit:None});
        assert_eq!(result.unwrap_err().code,"active_account_immutable");assert_eq!(s.revision,revision);assert_eq!(s.lifecycle_journal,journal);
    }
    assert_aggregate_rules(&s);
}

#[test]
fn native_request_rejection_keeps_its_selection_and_catalog_and_waits_for_explicit_user_action() {
    use crate::accounts::{AccountKey, AccountState, Provider};
    use crate::model_selection::{AccountModelCatalog, CatalogModel, ModelKey, ModelProfile, ModelSelection, TaskSelection};
    use crate::views::text::Lang;
    for (code, why, english, spanish) in [
        ("codex_turn_failed", NATIVE_FAILED_WHY, "Codex's turn ended with an error", "El turno de Codex terminó con un error"),
        ("codex_request_rejected", NATIVE_REQUEST_REJECTED_WHY, "Codex rejected this task's request", "Codex rechazó la solicitud de esta tarea"),
        ("codex_context_exceeded", NATIVE_CONTEXT_EXCEEDED_WHY, "exceeds the Codex model's context window", "supera la ventana de contexto del modelo de Codex"),
    ] {
        let mut state = world(); mixed_default_accounts(&mut state, AccountState::SignedIn);
        let account = AccountKey::native(Provider::Codex);
        must(&mut state, Actor::System, Command::RecordModelCatalog { catalog: AccountModelCatalog { account: account.clone(), revision: "native-request-fixture".into(), measured_at: 1, native_default: Some("fixture-model".into()), models: vec![CatalogModel { model: "fixture-model".into(), display_name: "Fixture model".into(), reasoning_efforts: vec![], default_reasoning_effort: None, context_window: None, input_modalities: vec!["text".into()], is_default: true, hidden: false }] } });
        sdk_turn_on_provider(&mut state, Provider::Codex, "t", "a", "turn-1", None);
        let mut intent = TaskSelection::automatic(ModelProfile::General); intent.requested = Some(ModelKey { provider: Provider::Codex, model: "fixture-model".into() }); intent.account = Some(account.clone());
        intent.effective = Some(ModelSelection { model: intent.requested.clone().unwrap(), reasoning_effort: None, catalog_revision: "native-request-fixture".into() });
        must(&mut state, Actor::System, Command::RecordTaskSelection { task_id: "t".into(), selection: intent.clone(), pending: None });
        let accounts = state.accounts.clone(); let catalog = state.model_catalogs.clone(); let usage = state.account_usage.clone();
        let execution = state.lifecycle_of(&"t".into()).unwrap().execution().cloned();
        must(&mut state, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: Some(code.into()) });
        let card = turn_ended_card_id(&"t".into(), 1, "turn-1");
        assert_eq!(state.interactions[&card].card.why, why); assert_eq!(options(&state, &card), [CARRY_ON, CANCEL_TASK]);
        assert_eq!(state.tasks[&TaskId::from("t")].selection.as_ref(), Some(&intent)); assert_eq!(state.tasks[&TaskId::from("t")].account_key(), account);
        assert_eq!(state.accounts, accounts); assert_eq!(state.model_catalogs, catalog); assert_eq!(state.account_usage, usage); assert!(state.claude_held().is_empty());
        let lifecycle = state.lifecycle_of(&"t".into()).unwrap(); assert_eq!(lifecycle.execution().cloned(), execution); assert!(lifecycle.pending_inputs().is_empty(), "no automatic retry input or account/model fallback is created");
        let shown = turn_end_shown(&state, "t", Lang::En); assert!(shown.contains(english), "{shown}"); assert!(!shown.contains("Claude's turn ended"));
        let shown = turn_end_shown(&state, "t", Lang::Es); assert!(shown.contains(spanish), "{shown}");
        assert_aggregate_rules(&state);
    }
}

#[test]
fn a_known_codex_source_never_heals_an_unknown_original_target_but_own_node_success_still_recovers() {
    use crate::accounts::{AccountState, Provider};
    use crate::model_selection::{ModelProfile, TaskSelection};
    let mut state = world(); mixed_default_accounts(&mut state, AccountState::SignedIn);
    assert!(state.accounts.iter().find(|account| account.provider == Provider::Codex).unwrap().native_identity.is_none());
    sdk_turn_on_provider(&mut state, Provider::Codex, "t", "a", "turn-1", None);
    must(&mut state, Actor::System, Command::RecordTaskSelection { task_id: "t".into(), selection: TaskSelection::automatic(ModelProfile::General), pending: None });
    usage_turn(&mut state, "t", 4_102_444_800);
    let target = state.held_by_limit(&"t".into()).unwrap().card; let journal = state.lifecycle_journal[&TaskId::from("t")].clone();
    let mut accounts = state.accounts.clone(); accounts.iter_mut().find(|account| account.provider == Provider::Codex).unwrap().native_identity = Some("later-known-account".into());
    must(&mut state, Actor::System, Command::RecordAccounts { accounts, reserve: crate::usage::default_reserve() });
    sdk_turn_on_provider(&mut state, Provider::Codex, "u", "b", "turn-1", None);
    must(&mut state, Actor::System, Command::RecordTaskSelection { task_id: "u".into(), selection: TaskSelection::automatic(ModelProfile::General), pending: None });
    must(&mut state, Actor::System, Command::RecordRuntimeFacts { task_id: "u".into(), generation: 1, facts: vec![RuntimeObservation::ModelAnswered] });
    assert!(!state.interactions[&target].withdrawn, "a known source cannot identify another task's unknown original workspace");
    assert_eq!(state.lifecycle_journal[&TaskId::from("t")], journal);
    usage_turn(&mut state, "u", 4_102_444_800); let own = state.held_by_limit(&"u".into()).unwrap().card;
    must(&mut state, Actor::System, Command::RecordRuntimeFacts { task_id: "u".into(), generation: 1, facts: vec![RuntimeObservation::ModelAnswered] });
    assert!(state.interactions[&own].withdrawn, "own-node success retains the existing financial and credential fences");
    assert!(!state.interactions[&target].withdrawn); assert_eq!(state.lifecycle_journal[&TaskId::from("t")], journal);
    assert_aggregate_rules(&state);
}

#[test]
fn a_message_claude_never_read_offers_a_crash_report_as_not_delivered() {
    let mut s = world();
    dropped(&mut s);
    assert!(s.report_failure_action(&"t".into()).enabled);
    assert_eq!(s.task_failure(&"t".into()), Some(ReportedFailure::MessageNotDelivered));
}

/// t-duo-parcel, 25 October: its node presented its PR and went on working, in that turn, in the
/// turn its background work woke and in the one the user's message started. The task's live state
/// is work while any of them runs and waits for the review only once none does.
#[test]
fn a_delivery_under_review_reads_working_while_a_turn_runs_and_waits_for_the_user_once_none_does() {
    use alondra_domain::{LiveState, Several, Step, Worker};
    let mut s = DomainState::new("epoch");
    let mut journal = vec![
        LifecycleRecord::Planned { input: "input:t:1".into() },
        LifecycleRecord::Admitted {
            node: "a".into(),
            execution: Some(ExecutionIdentity { conversation: "conversation".into(), instance: "instance".into(), host: "alondra-sdk".into() }),
        },
        LifecycleRecord::ResourceAvailable,
        LifecycleRecord::TurnStarted { turn: "presenting".into(), input: "input:t:1".into() },
        LifecycleRecord::Presented { delivery: "result-3".into() },
    ];
    let review = Several::new([InteractionId::from("result-3")]).map(LiveState::WaitingUser);
    let ended = |turn: &str| LifecycleRecord::TurnEnded { turn: turn.into(), unheard_card: format!("unheard-{turn}").into(), wake: None, text: None, awaits: None, stalled: false };
    for (records, live, why) in [
        (vec![], Some(LiveState::Working(Worker::Turn)), "the turn that presented it goes on"),
        (vec![LifecycleRecord::WorkStarted { turn: "presenting".into(), work: "tests".into() }, ended("presenting")], review.clone(),
            "only its own work runs, after the turn"),
        (vec![LifecycleRecord::WorkEnded { work: "tests".into(), continuation: "done".into() }], Some(LiveState::Working(Worker::Alondra(Step::Waking))),
            "the work's end wakes a turn"),
        (vec![LifecycleRecord::TurnResumed { turn: "woken".into(), continuation: "done".into() }], Some(LiveState::Working(Worker::Turn)), "the woken turn runs"),
        (vec![ended("woken")], review.clone(), "nothing runs"),
        (vec![LifecycleRecord::Told { input: "input:t:2".into(), text: Some("Also bump the changelog".into()) }],
            Some(LiveState::Working(Worker::Alondra(Step::Delivering))), "the message goes to the node"),
        (vec![LifecycleRecord::TurnStarted { turn: "told".into(), input: "input:t:2".into() }], Some(LiveState::Working(Worker::Turn)), "the message's turn runs"),
        (vec![ended("told")], review, "nothing runs again"),
    ] {
        journal.extend(records);
        s.lifecycle_journal.insert("t".into(), journal.clone());
        assert_eq!(s.live_state(&"t".into()), live, "{why}");
    }
}
