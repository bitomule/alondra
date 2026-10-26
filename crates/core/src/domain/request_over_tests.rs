//! An open Request whose last open Task ends goes to the orchestrator once, so it closes the
//! Request or creates its next Tasks: nobody has to ask.
use super::*;
use std::collections::BTreeSet;

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}
fn agent() -> Actor {
    Actor::Agent { agent_id: "a".into() }
}
fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}
fn ok(s: &mut DomainState, a: Actor, c: Command) -> CommandOutcome {
    let envelope = CommandEnvelope {
        protocol_version: 2,
        data_epoch: s.data_epoch.clone(),
        command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision,
        command: c,
    };
    s.execute(&a, &envelope).unwrap()
}
fn told(outcome: &CommandOutcome) -> Vec<&RequestId> {
    outcome.effects.iter().filter_map(|e| match e {
        DomainEffect::RequestTasksOver { request_id, .. } => Some(request_id),
        _ => None,
    }).collect()
}
fn task(id: &str, request: &str) -> Command {
    Command::CreateTask {
        plan_items: None, task_id: id.into(), request_id: request.into(), title: format!("Task {id}"), agent_id: None,
        review_prs: false, dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp/project")),
    }
}
fn text() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text { text: "The design".into() }]
}
fn request(message: bool) -> DomainState {
    let mut s = DomainState::new("epoch");
    ok(&mut s, Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "Node".into(), at: 0 });
    ok(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Several Claude accounts".into(), auto_merge: false, maintenance: false, message });
    s
}
fn delivered(s: &mut DomainState, task_id: &str, id: &str) {
    ok(s, orchestrator(), Command::AssignTask { task_id: task_id.into(), agent_id: "a".into(), model: None });
    ok(s, agent(), Command::PresentResult {
        checklist: vec![], attachments: vec![], task_id: task_id.into(), interaction_id: id.into(), body: "The design".into(),
        targets: text(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the design", &text(), true),
    });
}
fn accept(s: &mut DomainState, id: &str) -> CommandOutcome {
    ok(s, user(), Command::AcceptDelivery { presentation_id: id.into(), targets: text() })
}
fn node_closed(s: &mut DomainState, task_id: &str) -> CommandOutcome {
    ok(s, Actor::System, Command::RecordNodeEnd { task_id: task_id.into(), end: NodeEnd::Closed, agent_id: None })
}

#[test]
fn the_last_task_done_hands_its_open_request_to_the_orchestrator_once() {
    let mut s = request(false);
    assert!(told(&ok(&mut s, orchestrator(), Command::TitleRequest { request_id: "r".into(), title: "Accounts".into() })).is_empty(),
        "a request with no tasks yet is still being defined");
    ok(&mut s, orchestrator(), task("design", "r"));
    delivered(&mut s, "design", "final");
    assert!(told(&accept(&mut s, "final")).is_empty(), "an accepted task still closing has not ended");
    assert_eq!(told(&node_closed(&mut s, "design")), [&RequestId::from("r")]);
    assert_eq!(s.tasks[&"design".into()].state, TaskState::Done);
    assert!(!s.requests[&"r".into()].closed);
    assert!(told(&ok(&mut s, orchestrator(), Command::TitleRequest { request_id: "r".into(), title: "Claude accounts".into() })).is_empty(),
        "a later change does not tell it again");
}

#[test]
fn a_task_ending_while_another_is_open_tells_nothing_until_that_one_ends() {
    let mut s = request(false);
    ok(&mut s, orchestrator(), Command::TitleRequest { request_id: "r".into(), title: "Accounts".into() });
    ok(&mut s, orchestrator(), task("design", "r"));
    ok(&mut s, orchestrator(), task("build", "r"));
    delivered(&mut s, "design", "final");
    assert!(told(&accept(&mut s, "final")).is_empty());
    assert!(told(&node_closed(&mut s, "design")).is_empty());
    let cancelled = ok(&mut s, user(), Command::CancelTask { task_id: "build".into() });
    assert!(cancelled.effects.contains(&DomainEffect::RequestTasksOver { request_id: "r".into(), task_id: "build".into() }),
        "a cancelled task ends it as well, and the message names it: {:?}", cancelled.effects);
}

#[test]
fn a_task_reopened_and_done_again_tells_the_orchestrator_again() {
    let mut s = request(false);
    ok(&mut s, orchestrator(), task("design", "r"));
    delivered(&mut s, "design", "final");
    accept(&mut s, "final");
    assert_eq!(told(&node_closed(&mut s, "design")).len(), 1);
    assert!(told(&ok(&mut s, user(), Command::ReopenTask { task_id: "design".into() })).is_empty());
    delivered(&mut s, "design", "again");
    accept(&mut s, "again");
    assert_eq!(told(&node_closed(&mut s, "design")), [&RequestId::from("r")]);
}

#[test]
fn a_closed_request_tells_nothing() {
    let mut s = request(false);
    ok(&mut s, orchestrator(), task("design", "r"));
    assert_eq!(told(&ok(&mut s, user(), Command::CancelTask { task_id: "design".into() })).len(), 1);
    let close = ok(&mut s, orchestrator(), Command::CloseRequest { request_id: "r".into(), answer: Some("Not needed after all".into()), withdrawn: false, title: None, headline: None });
    assert!(told(&close).is_empty());
    assert!(s.requests[&"r".into()].closed);

    let mut s = request(true);
    ok(&mut s, orchestrator(), task("launch", "r"));
    assert!(told(&ok(&mut s, user(), Command::CancelTask { task_id: "launch".into() })).is_empty(),
        "a message closes itself when its last task ends");
    assert!(s.requests[&"r".into()].closed);
}
