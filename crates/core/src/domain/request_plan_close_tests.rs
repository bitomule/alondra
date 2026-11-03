//! A request whose tasks built every point of its approved plan closes by itself, with the Done
//! card summing up what each task delivered; anything less still goes to the orchestrator.
use super::*;
use std::collections::BTreeSet;

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
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
fn task(id: &str, agent: &str, plan_items: Option<PlanItems>) -> Command {
    Command::CreateTask {
        task_id: id.into(), request_id: "r".into(), title: format!("Phase {id}"), agent_id: Some(agent.into()),
        review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(LaunchPlan::new("node", "/tmp/project")), plan_items, continues: None,
    }
}
const PHASED: &str = "# Fast views\n\n## Phase 1: guards\n- [ ] Guards for every command\n\n## Phase 2: read index\n- [ ] Read index\n- [ ] Index matches the state\n";

fn planned(message: bool) -> DomainState {
    let mut s = DomainState::new("epoch");
    for agent in ["a", "b"] {
        ok(&mut s, Actor::System, Command::RegisterAgent { agent_id: agent.into(), name: "Node".into(), at: 0 });
    }
    ok(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Make the views fast".into(), auto_merge: false, maintenance: false, message });
    ok(&mut s, orchestrator(), Command::TitleRequest { request_id: "r".into(), title: "Fast views".into() });
    ok(&mut s, orchestrator(), task("guards", "a", None));
    ok(&mut s, Actor::System, Command::HoldHook {
        interaction_id: "p".into(), task_id: "guards".into(), agent_id: "a".into(), hold: Hold::Plan,
        body: PHASED.into(), options: Hold::Plan.options(),
        card: CardSpec::new(CardKind::Review, "Fast views").content(ContentKind::Plan, PHASED),
    });
    ok(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "p".into(), option_id: Hold::APPROVE.into(), inputs: Default::default() });
    ok(&mut s, orchestrator(), task("index", "b", Some(PlanItems { from_task: "guards".into(), items: vec!["2".into(), "3".into()] })));
    s
}
fn finish(s: &mut DomainState, task_id: &str, agent: &str, points: &[&str], result: &str) -> CommandOutcome {
    let commit = || vec![Evidence { kind: EvidenceKind::Commit, value: "3f2a1c9".into(), detail: None }];
    let targets = vec![DeliveryTarget::Text { text: result.into() }];
    let presentation = format!("{task_id}-final");
    ok(s, Actor::Agent { agent_id: agent.into() }, Command::PresentResult {
        checklist: points.iter().map(|p| ChecklistMark { item: (*p).into(), evidence: commit() }).collect(),
        attachments: vec![], task_id: task_id.into(), interaction_id: presentation.as_str().into(), body: format!("# {result}\n\nDetails."),
        card: CardSpec::delivery("Review it", &targets, true), targets: targets.clone(), is_final: true, nothing_to_review: false,
    });
    ok(s, user(), Command::AcceptDelivery { presentation_id: presentation.as_str().into(), targets });
    ok(s, Actor::System, Command::RecordNodeEnd { task_id: task_id.into(), end: NodeEnd::Closed, agent_id: None })
}
fn handed_to_orchestrator(outcome: &CommandOutcome) -> bool {
    outcome.effects.iter().any(|e| matches!(e, DomainEffect::RequestTasksOver { .. }))
}
fn done_card(s: &DomainState) -> Option<crate::request_outcome::DoneCard> {
    s.interactions.get(&InteractionId(crate::request_outcome::DoneCard::id("r")))
        .and_then(|i| crate::request_outcome::DoneCard::from_body(&i.body))
}

#[test]
fn a_request_whose_tasks_built_every_phase_of_its_plan_closes_with_their_results() {
    let mut s = planned(false);
    finish(&mut s, "guards", "a", &["1"], "Every command has its guard");
    assert!(!s.requests[&"r".into()].closed, "a phase is still open");
    let last = finish(&mut s, "index", "b", &["2", "3"], "Screens read the index");
    assert_eq!(s.tasks[&"index".into()].state, TaskState::Done);
    assert!(s.requests[&"r".into()].closed, "the request closes by itself");
    assert!(!handed_to_orchestrator(&last), "the orchestrator is not asked to close it: {:?}", last.effects);
    let card = done_card(&s).expect("the user gets the Done card");
    assert_eq!(card.summary.as_deref(), Some("Phase guards: Every command has its guard\nPhase index: Screens read the index"));
    assert_eq!(card.tasks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["Phase guards", "Phase index"]);
    assert!(last.effects.contains(&DomainEffect::NotifyUser { task_id: None, interaction_id: InteractionId(crate::request_outcome::DoneCard::id("r")) }));
    s.validate().unwrap();
}

#[test]
fn a_phase_no_finished_task_delivered_leaves_the_request_to_the_orchestrator() {
    let mut s = planned(false);
    finish(&mut s, "guards", "a", &["1"], "Every command has its guard");
    let cancelled = ok(&mut s, user(), Command::CancelTask { task_id: "index".into() });
    let ended = ok(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "index".into(), end: NodeEnd::Closed, agent_id: None });
    assert_eq!(s.tasks[&"index".into()].state, TaskState::Cancelled);
    assert!(!s.requests[&"r".into()].closed);
    assert!(handed_to_orchestrator(&cancelled) || handed_to_orchestrator(&ended),
        "the orchestrator decides what comes of the undelivered phase: {:?} {:?}", cancelled.effects, ended.effects);
    assert!(done_card(&s).is_none());
}

#[test]
fn a_request_without_an_approved_plan_still_goes_to_the_orchestrator() {
    let mut s = DomainState::new("epoch");
    ok(&mut s, Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "Node".into(), at: 0 });
    ok(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Make the views fast".into(), auto_merge: false, maintenance: false, message: false });
    ok(&mut s, orchestrator(), Command::TitleRequest { request_id: "r".into(), title: "Fast views".into() });
    ok(&mut s, orchestrator(), task("guards", "a", None));
    let last = finish(&mut s, "guards", "a", &[], "Every command has its guard");
    assert!(!s.requests[&"r".into()].closed);
    assert!(handed_to_orchestrator(&last));
    assert!(done_card(&s).is_none());
}

#[test]
fn a_message_with_a_delivered_plan_closes_as_before_without_a_card() {
    let mut s = planned(true);
    finish(&mut s, "guards", "a", &["1"], "Every command has its guard");
    let last = finish(&mut s, "index", "b", &["2", "3"], "Screens read the index");
    let request = &s.requests[&"r".into()];
    assert!(request.closed && request.answer.is_none(), "a message closes with no answer of Alondra's");
    assert!(!handed_to_orchestrator(&last));
    assert!(done_card(&s).is_none(), "a message gets no Done card");
}
