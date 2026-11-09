//! A task that carries on another (its work moved to another provider) stays in that task's
//! request: the request keeps counting its work while the continuation lives, its outcome is the
//! continuation's, and the worktree it takes over is never left behind nor taken away from it.
use super::*;
use crate::request_outcome::DoneCard;
use std::collections::BTreeSet;

const WORKTREE: &str = "/repo/.claude/worktrees/t";

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}
fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}
fn exec(s: &mut DomainState, a: Actor, c: Command) -> Result<CommandOutcome, DomainError> {
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision, command: c,
    };
    s.execute(&a, &envelope)
}
fn ok(s: &mut DomainState, a: Actor, c: Command) -> CommandOutcome {
    let shown = format!("{c:?}");
    exec(s, a, c).unwrap_or_else(|error| panic!("{shown} was refused: {error}"))
}
fn request(s: &mut DomainState, id: &str) {
    ok(s, user(), Command::CreateRequest { request_id: id.into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    ok(s, orchestrator(), Command::TitleRequest { request_id: id.into(), title: format!("Request {id}") });
}
fn create(id: &str, request: &str, continues: Option<&str>) -> Command {
    Command::CreateTask {
        task_id: id.into(), request_id: request.into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", WORKTREE)), plan_items: None,
        continues: continues.map(TaskId::from),
    }
}
fn node(task: &str) -> AgentId {
    format!("node-{task}").into()
}
fn start(s: &mut DomainState, task: &str) {
    ok(s, Actor::System, Command::RegisterAgent { agent_id: node(task), name: node(task).0, at: 0 });
    ok(s, orchestrator(), Command::AssignTask { task_id: task.into(), agent_id: node(task), model: None });
    ok(s, Actor::Agent { agent_id: node(task) }, Command::StartTask { task_id: task.into() });
}
/// The task the user asked for in request `r`, working in its worktree, and request `h` that asks
/// to move it to another provider.
fn working() -> DomainState {
    let mut s = DomainState::new("epoch");
    request(&mut s, "r");
    request(&mut s, "h");
    ok(&mut s, orchestrator(), create("t", "r", None));
    start(&mut s, "t");
    ok(&mut s, Actor::System, Command::RecordTaskWorktree {
        task_id: "t".into(), path: WORKTREE.into(), origin: WorktreeOrigin::Task, branch: Some("worktree-t".into()),
    });
    s
}
fn targets() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text { text: "Done".into() }]
}
fn deliver(s: &mut DomainState, task: &str) {
    let id = format!("final-{task}");
    ok(s, Actor::Agent { agent_id: node(task) }, Command::PresentResult {
        checklist: vec![], attachments: vec![], task_id: task.into(), interaction_id: id.as_str().into(), body: "Done".into(),
        targets: targets(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review it", &targets(), true),
    });
    ok(s, user(), Command::AcceptDelivery { presentation_id: id.as_str().into(), targets: targets() });
}
fn release(s: &mut DomainState, task: &str, end: WorktreeEnd) -> Result<CommandOutcome, DomainError> {
    exec(s, Actor::System, Command::ReleaseTaskWorktree { task_id: task.into(), path: WORKTREE.into(), end })
}
fn end_node(s: &mut DomainState, task: &str) {
    ok(s, Actor::System, Command::RecordNodeEnd { task_id: task.into(), end: NodeEnd::Closed, agent_id: None });
}
/// The original task cancelled and closed, its worktree left to the task that goes on in it.
fn cancel_original(s: &mut DomainState) {
    ok(s, user(), Command::CancelTask { task_id: "t".into() });
    release(s, "t", WorktreeEnd::HandedOver).unwrap();
    end_node(s, "t");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Cancelled);
}
fn finish(s: &mut DomainState, task: &str) {
    deliver(s, task);
    release(s, task, WorktreeEnd::Removed).unwrap();
    end_node(s, task);
    assert_eq!(s.tasks[&task.into()].state, TaskState::Done);
}
fn tasks_over(outcome: &CommandOutcome, request: &str) -> bool {
    outcome.effects.iter().any(|e| matches!(e, DomainEffect::RequestTasksOver { request_id, .. } if request_id.0 == request))
}
fn close(s: &mut DomainState, request: &str) -> Result<CommandOutcome, DomainError> {
    exec(s, orchestrator(), Command::CloseRequest { request_id: request.into(), answer: None, withdrawn: false, title: None, headline: None, folded_into: None })
}
fn done_card(s: &DomainState, request: &str) -> DoneCard {
    DoneCard::from_body(&s.interactions[&DoneCard::id(request).into()].body).unwrap()
}

#[test]
fn a_continuation_goes_in_the_request_of_the_task_it_carries_on() {
    let mut s = working();
    let before = s.clone();
    let error = exec(&mut s, orchestrator(), create("t-claude", "h", Some("t"))).unwrap_err();
    assert_eq!(error.code, "continuation_request");
    assert!(error.message.contains("request r"), "{}", error.message);
    assert_eq!(s, before, "a refused continuation changed the state");
    ok(&mut s, orchestrator(), create("t-claude", "r", Some("t")));
    assert_eq!(s.tasks[&"t-claude".into()].continues, Some("t".into()));
}

#[test]
fn a_continuation_not_started_yet_keeps_the_request_open_once_its_original_is_cancelled() {
    let mut s = working();
    ok(&mut s, orchestrator(), create("t-claude", "r", Some("t")));
    let cancelled = ok(&mut s, user(), Command::CancelTask { task_id: "t".into() });
    assert!(!tasks_over(&cancelled, "r"));
    release(&mut s, "t", WorktreeEnd::HandedOver).unwrap();
    let ended = ok(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: None });
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Cancelled);
    assert!(!tasks_over(&ended, "r"), "the request still has work in the continuation");
    assert!(!s.requests[&"r".into()].closed);
    assert_eq!(close(&mut s, "r").unwrap_err().code, "task_not_active", "a request with a live continuation does not close");
}

#[test]
fn a_continuation_takes_the_worktree_and_the_plan_of_the_task_it_carries_on() {
    let mut s = working();
    const PLAN: &str = "# Build it\n\n- [ ] Build it\n";
    ok(&mut s, Actor::System, Command::HoldHook {
        interaction_id: "plan".into(), task_id: "t".into(), agent_id: node("t"), hold: Hold::Plan,
        body: PLAN.into(), options: Hold::Plan.options(),
        card: CardSpec::new(CardKind::Review, "Build it").content(ContentKind::Plan, PLAN),
    });
    ok(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "plan".into(), option_id: Hold::APPROVE.into(), inputs: Default::default() });
    let checklist = s.checklists[&"t".into()].clone();
    assert_eq!(checklist.items.len(), 1);
    ok(&mut s, orchestrator(), create("t-claude", "r", Some("t")));
    let continuation = &s.tasks[&"t-claude".into()];
    assert_eq!(continuation.worktrees.get(WORKTREE), Some(&TaskWorktree {
        origin: WorktreeOrigin::Inherited, branch: Some("worktree-t".into()), discard: false, end: None,
    }));
    assert_eq!(s.checklists.get(&"t-claude".into()), Some(&checklist));
    assert!(!s.checklists.contains_key(&"t".into()));
    let mut both = create("t-codex", "r", Some("t-claude"));
    if let Command::CreateTask { plan_items, .. } = &mut both {
        *plan_items = Some(PlanItems { from_task: "t-claude".into(), items: vec!["1".into()] });
    }
    let refused = exec(&mut s, orchestrator(), both).unwrap_err();
    assert_eq!(refused.subject.as_deref(), Some("plan_items"));
}

#[test]
fn a_worktree_is_handed_over_only_to_an_open_task_that_works_in_it() {
    let mut s = working();
    ok(&mut s, user(), Command::CancelTask { task_id: "t".into() });
    assert_eq!(release(&mut s, "t", WorktreeEnd::HandedOver).unwrap_err().code, "conflict", "nobody else works in it");
    let mut s = working();
    ok(&mut s, orchestrator(), create("t-claude", "r", Some("t")));
    cancel_original(&mut s);
    assert_eq!(s.tasks[&"t".into()].worktrees[WORKTREE].end, Some(WorktreeEnd::HandedOver));
    assert_eq!(s.tasks[&"t-claude".into()].worktrees[WORKTREE].end, None, "the continuation keeps working in it");
    assert_eq!(s.worktree_held_by_other(WORKTREE, &"t".into()), Some(&"t-claude".into()));
}

#[test]
fn a_finished_continuation_ends_its_request_as_done() {
    let mut s = working();
    ok(&mut s, orchestrator(), create("t-claude", "r", Some("t")));
    cancel_original(&mut s);
    start(&mut s, "t-claude");
    finish(&mut s, "t-claude");
    close(&mut s, "r").unwrap();
    let card = done_card(&s, "r");
    assert_eq!(card.tasks.iter().map(|t| (t.id.as_str(), t.cancelled)).collect::<Vec<_>>(), vec![("t-claude", false)]);
    assert_eq!(card.text(crate::views::Lang::Es).0, "Hecho");
}

#[test]
fn a_cancelled_continuation_ends_its_request_as_not_done() {
    let mut s = working();
    ok(&mut s, orchestrator(), create("t-claude", "r", Some("t")));
    cancel_original(&mut s);
    start(&mut s, "t-claude");
    ok(&mut s, user(), Command::CancelTask { task_id: "t-claude".into() });
    release(&mut s, "t-claude", WorktreeEnd::Removed).unwrap();
    end_node(&mut s, "t-claude");
    close(&mut s, "r").unwrap();
    assert_eq!(done_card(&s, "r").text(crate::views::Lang::Es).0, "No se hizo: se canceló su tarea");
}

#[test]
fn several_continuations_of_one_task_all_count_as_its_work() {
    let mut s = working();
    ok(&mut s, orchestrator(), create("t-claude", "r", Some("t")));
    ok(&mut s, orchestrator(), create("t-sonnet", "r", Some("t")));
    assert_eq!(s.tasks[&"t-sonnet".into()].worktrees[WORKTREE].origin, WorktreeOrigin::Inherited);
    cancel_original(&mut s);
    start(&mut s, "t-claude");
    deliver(&mut s, "t-claude");
    release(&mut s, "t-claude", WorktreeEnd::HandedOver).unwrap();
    let ended = ok(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t-claude".into(), end: NodeEnd::Closed, agent_id: None });
    assert!(!tasks_over(&ended, "r"), "the other continuation still works");
    start(&mut s, "t-sonnet");
    finish(&mut s, "t-sonnet");
    close(&mut s, "r").unwrap();
    let card = done_card(&s, "r");
    assert_eq!(card.tasks.len(), 2);
    assert_eq!(card.text(crate::views::Lang::Es).0, "Hecho");
}

#[test]
fn a_task_created_before_continuations_reads_back() {
    let recorded = serde_json::json!({
        "kind": "create_task", "task_id": "t", "request_id": "r", "title": "Work", "agent_id": null, "review_prs": false,
        "dependencies": [], "stack": null,
    });
    let command: Command = serde_json::from_value(recorded).unwrap();
    assert!(matches!(command, Command::CreateTask { continues: None, .. }));
    let task: Task = serde_json::from_value(serde_json::to_value(&working().tasks[&"t".into()]).unwrap()).unwrap();
    assert_eq!(task.continues, None);
}
