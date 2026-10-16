//! The aggregate rules: for a task with a journal, nothing but a lifecycle decision moves its
//! state, node, start or ending; a cancellation or a completion ends only with its receipts; and
//! a DTO edited by hand is refused, never repaired.
use super::*;
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

/// After every accepted command, the DTO of every owned task agrees with the replay of its
/// journal, and running the derivations again moves nothing: only decisions do.
pub(super) fn assert_aggregate_rules(s: &DomainState) {
    s.validate_journal().unwrap_or_else(|error| panic!("{error}"));
    let mut again = s.clone();
    again.derive_states();
    again.settle_nodes();
    for (id, task) in &s.tasks {
        if s.owned(id) {
            assert_eq!(&again.tasks[id], task, "derive_states moved the owned task {id} without a decision");
        }
    }
    assert_eq!(again.agents, s.agents, "settle_nodes moved a node without a decision");
    for (review, held) in &s.plan_reviews {
        for task in held.open().filter(|task| s.owned(task)) {
            let lifecycle = replay(task, &s.lifecycle_journal[task]).unwrap();
            assert!(lifecycle.plan_reviews().iter().any(|(id, _)| *id == review), "the lifecycle of {task} does not hold it for the review {review}");
        }
    }
}

#[test]
fn cancelling_a_task_without_a_node_ends_only_with_the_cleanup_receipt() {
    let mut s = world();
    task(&mut s, "t", None);
    assert!(s.owned(&"t".into()));
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
fn a_task_created_before_the_journal_keeps_the_old_writers() {
    let mut s = world();
    let legacy = Task {
        id: "old".into(), request_id: "r".into(), agent_id: Some("a".into()), title: "Old".into(), state: TaskState::Running, revision: 0,
        started: true, review_prs: false, final_presentation: None, pr_id: None, dependencies: BTreeSet::new(), stack: None,
        documents: BTreeMap::new(), archived: false, model: None, delivered_dependencies: Default::default(), blocks: Default::default(),
        verdict: None, review_rounds: 0, wrap_up: WrapUp::default(), priority: Default::default(), ending: None,
    };
    s.tasks.insert(legacy.id.clone(), legacy);
    s.derive_states();
    s.settle_nodes();
    s.validate().unwrap();
    assert!(!s.owned(&"old".into()));
    must(&mut s, user(), Command::CancelTask { task_id: "old".into() });
    assert_eq!(lifecycle(&s, "old"), (TaskState::Cancelled, true, Some("a".into()), None), "cancelled at once, as before the journal");
    assert!(!s.lifecycle_journal.contains_key(&TaskId::from("old")));
    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "old".into(), end: NodeEnd::Closed, agent_id: Some("a".into()) });
    must(&mut s, user(), Command::ReopenTask { task_id: "old".into() });
    assert_eq!(lifecycle(&s, "old"), (TaskState::NotStarted, false, None, None));
    assert_aggregate_rules(&s);
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
