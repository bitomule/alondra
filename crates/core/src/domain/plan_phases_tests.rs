//! A plan that does not fit one PR is approved in phases, one Task each: every new Task takes its
//! phase's points from the Task that holds them, so no point is lost or owned twice.
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
fn exec(s: &mut DomainState, a: Actor, c: Command) -> Result<CommandOutcome, DomainError> {
    let envelope = CommandEnvelope {
        protocol_version: 2,
        data_epoch: s.data_epoch.clone(),
        command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision,
        command: c,
    };
    s.execute(&a, &envelope)
}
fn ok(s: &mut DomainState, a: Actor, c: Command) {
    exec(s, a, c).unwrap();
}
fn refused(s: &mut DomainState, a: Actor, c: Command) -> String {
    let before = s.clone();
    let error = exec(s, a, c).unwrap_err();
    assert_eq!(*s, before, "a rejected command changed state");
    error.code
}
fn task(id: &str, request: &str, agent: Option<&str>, dependencies: &[&str], plan_items: Option<PlanItems>) -> Command {
    Command::CreateTask {
        task_id: id.into(), request_id: request.into(), title: format!("Phase {id}"), agent_id: agent.map(Into::into),
        review_prs: false, dependencies: dependencies.iter().map(|d| TaskId::from(*d)).collect::<BTreeSet<_>>(), stack: None,
        launch: Some(LaunchPlan::new("node", "/tmp/project")), plan_items, continues: None,
    }
}
fn take(from: &str, items: &[&str]) -> Option<PlanItems> {
    Some(PlanItems { from_task: from.into(), items: items.iter().map(|i| (*i).to_owned()).collect() })
}
fn points(s: &DomainState, task: &str) -> Vec<String> {
    s.checklists.get(&TaskId::from(task)).map(|c| c.items.iter().map(|i| i.id.clone()).collect()).unwrap_or_default()
}
const PHASED: &str = "# Fast views\n\n## Phase 1: guards\n- [ ] Guards for every command\n- [ ] Guards tested against apply\n\n\
## Phase 2: read index\nDepends on: phase 1\n- [ ] Read index\n- [ ] Index matches the state\n\n\
## Phase 3: notifications\nDepends on: phase 2\n- [ ] Notifications read the index\n\n\
## Phase 4: API\nDepends on: phase 2\n- [ ] API reads the index\n";

fn planned() -> DomainState {
    let mut s = DomainState::new("epoch");
    ok(&mut s, Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "Planner".into(), at: 0 });
    ok(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Make the views fast".into(), auto_merge: false, maintenance: false, message: false });
    ok(&mut s, orchestrator(), Command::TitleRequest { request_id: "r".into(), title: "Fast views".into() });
    ok(&mut s, orchestrator(), task("plan", "r", Some("a"), &[], None));
    ok(&mut s, Actor::System, Command::HoldHook {
        interaction_id: "p".into(), task_id: "plan".into(), agent_id: "a".into(), hold: Hold::Plan,
        body: PHASED.into(), options: Hold::Plan.options(),
        card: CardSpec::new(CardKind::Review, "Fast views").content(ContentKind::Plan, PHASED),
    });
    ok(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "p".into(), option_id: Hold::APPROVE.into(), inputs: Default::default() });
    s
}

#[test]
fn each_phase_takes_its_points_and_the_planning_task_keeps_only_its_own() {
    let mut s = planned();
    assert_eq!(points(&s, "plan"), ["1", "2", "3", "4", "5", "6"]);
    ok(&mut s, orchestrator(), task("index", "r", None, &["plan"], take("plan", &["3", "4"])));
    ok(&mut s, orchestrator(), task("notifications", "r", None, &["index"], take("plan", &["5"])));
    ok(&mut s, orchestrator(), task("api", "r", None, &["index"], take("plan", &["6"])));
    assert_eq!(points(&s, "plan"), ["1", "2"]);
    assert_eq!(points(&s, "index"), ["3", "4"]);
    assert_eq!(points(&s, "notifications"), ["5"]);
    assert_eq!(points(&s, "api"), ["6"]);
    let plan = InteractionId::from("p");
    assert!(["plan", "index", "notifications", "api"].iter().all(|t| s.checklists[&TaskId::from(*t)].plan == plan), "every phase keeps the approved plan");
    assert_eq!(s.checklists[&TaskId::from("api")].items[0].text, "API reads the index", "a point keeps its text and its number in the plan");
    for sibling in ["notifications", "api"] {
        assert_eq!(s.tasks[&TaskId::from(sibling)].dependencies, BTreeSet::from([TaskId::from("index")]), "{sibling} waits only for the phase it needs");
    }
    s.validate().unwrap();
}

#[test]
fn the_planning_task_delivers_by_marking_only_the_points_left_to_it() {
    let mut s = planned();
    ok(&mut s, orchestrator(), task("rest", "r", None, &["plan"], take("plan", &["3", "4", "5", "6"])));
    let commit = || vec![Evidence { kind: EvidenceKind::Commit, value: "3f2a1c9".into(), detail: None }];
    let targets = vec![DeliveryTarget::Text { text: "Guards".into() }];
    ok(&mut s, agent(), Command::PresentResult {
        checklist: vec![ChecklistMark { item: "1".into(), evidence: commit() }, ChecklistMark { item: "2".into(), evidence: commit() }],
        attachments: vec![], task_id: "plan".into(), interaction_id: "d".into(), body: "Guards".into(),
        card: CardSpec::delivery("Guards", &targets, true), targets, is_final: true, nothing_to_review: false,
    });
}

#[test]
fn points_are_taken_once_by_number_and_the_holder_always_keeps_some() {
    let mut s = planned();
    assert_eq!(refused(&mut s, orchestrator(), task("x", "r", None, &[], take("plan", &["9"]))), "plan_items_unknown");
    assert_eq!(refused(&mut s, orchestrator(), task("x", "r", None, &[], take("plan", &["3", "3"]))), "plan_items_unknown");
    assert_eq!(refused(&mut s, orchestrator(), task("x", "r", None, &[], take("plan", &[]))), "plan_items_unknown");
    assert_eq!(refused(&mut s, orchestrator(), task("x", "r", None, &[], take("plan", &["1", "2", "3", "4", "5", "6"]))), "plan_items_take_all");
    ok(&mut s, orchestrator(), task("index", "r", None, &["plan"], take("plan", &["3", "4"])));
    assert_eq!(refused(&mut s, orchestrator(), task("x", "r", None, &[], take("plan", &["3"]))), "plan_items_unknown", "a point already taken is no longer there");
    assert_eq!(refused(&mut s, orchestrator(), task("x", "r", None, &[], take("missing", &["5"]))), "invalid_input");
}

#[test]
fn points_come_only_from_a_task_of_the_same_request_that_has_not_delivered() {
    let mut s = planned();
    ok(&mut s, user(), Command::CreateRequest { request_id: "other".into(), body: "Something else".into(), auto_merge: false, maintenance: false, message: false });
    assert_eq!(refused(&mut s, orchestrator(), task("x", "other", None, &[], take("plan", &["3"]))), "invalid_input");
    ok(&mut s, orchestrator(), task("plain", "r", None, &[], None));
    assert_eq!(refused(&mut s, orchestrator(), task("x", "r", None, &[], take("plain", &["1"]))), "invalid_input", "a task with no approved plan has no points to give");
    let commit = || vec![Evidence { kind: EvidenceKind::Commit, value: "3f2a1c9".into(), detail: None }];
    let targets = vec![DeliveryTarget::Text { text: "All".into() }];
    ok(&mut s, agent(), Command::PresentResult {
        checklist: (1..=6).map(|i| ChecklistMark { item: i.to_string(), evidence: commit() }).collect(),
        attachments: vec![], task_id: "plan".into(), interaction_id: "d".into(), body: "All".into(),
        card: CardSpec::delivery("All", &targets, true), targets, is_final: true, nothing_to_review: false,
    });
    assert_eq!(refused(&mut s, orchestrator(), task("x", "r", None, &[], take("plan", &["3"]))), "plan_items_unavailable");
}

#[test]
fn a_report_s_approved_plan_is_built_by_one_task_per_phase() {
    let mut s = DomainState::new("epoch");
    ok(&mut s, Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "Research".into(), at: 0 });
    ok(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Can the views be fast?".into(), auto_merge: false, maintenance: false, message: false });
    ok(&mut s, orchestrator(), task("research", "r", Some("a"), &[], None));
    let targets = vec![DeliveryTarget::Text { text: "Report".into() }];
    let mut card = CardSpec::delivery("Viable in phases", &targets, true);
    card.plan = Some(PHASED.into());
    ok(&mut s, agent(), Command::PresentResult {
        checklist: vec![], attachments: vec![], task_id: "research".into(), interaction_id: "report".into(),
        body: "Viable in phases".into(), targets: targets.clone(), is_final: true, nothing_to_review: false, card,
    });
    ok(&mut s, user(), Command::DecideProposedPlan { presentation_id: "report".into(), targets, choice: PlanChoice::Implement });
    ok(&mut s, orchestrator(), Command::TitleRequest { request_id: "implement-report".into(), title: "Fast views".into() });
    ok(&mut s, orchestrator(), task("guards", "implement-report", None, &["research"], None));
    assert_eq!(points(&s, "guards"), ["1", "2", "3", "4", "5", "6"], "the first task takes the whole plan");
    assert_eq!(refused(&mut s, orchestrator(), task("index", "implement-report", None, &["guards"], None)), "approved_plan_has_task");
    ok(&mut s, orchestrator(), task("index", "implement-report", None, &["research", "guards"], take("guards", &["3", "4", "5", "6"])));
    assert_eq!(points(&s, "guards"), ["1", "2"]);
    assert_eq!(points(&s, "index"), ["3", "4", "5", "6"]);
    s.validate().unwrap();
}
