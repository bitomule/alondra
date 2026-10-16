//! A final result that proposes a plan is never accepted and archived by accident: the user
//! either approves it, which hands the orchestrator a Request whose one Task starts with that plan
//! approved, or only archives it.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

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
fn ok(s: &mut DomainState, a: Actor, c: Command) -> CommandOutcome {
    exec(s, a, c).unwrap()
}
fn refused(s: &mut DomainState, a: Actor, c: Command) -> String {
    let before = s.clone();
    let error = exec(s, a, c).unwrap_err();
    assert_eq!(*s, before, "a rejected command changed state");
    error.code
}
fn create_task(task: &str, request: &str, agent: Option<&str>) -> Command {
    Command::CreateTask {
        task_id: task.into(), request_id: request.into(), title: format!("Task {task}"), agent_id: agent.map(Into::into),
        review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(LaunchPlan::new("node", "/tmp/redraft")),
    }
}
fn fixture() -> DomainState {
    let mut s = DomainState::new("epoch");
    ok(&mut s, Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "Research".into(), at: 0 });
    ok(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "How do other apps index the whole library?".into(), auto_merge: false, maintenance: false, message: false });
    ok(&mut s, orchestrator(), create_task("research", "r", Some("a")));
    s
}
const PLAN: &str = "# Index the whole library\n\n- [ ] Store feature prints in Core Data\n- [ ] Index changes in the background\n";
fn text() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text { text: "Report".into() }]
}
fn result(id: &str, is_final: bool, targets: Vec<DeliveryTarget>, plan: Option<&str>) -> Command {
    let mut card = CardSpec::delivery("Viable: index once and follow changes", &targets, is_final);
    card.plan = plan.map(str::to_owned);
    Command::PresentResult {
        checklist: vec![], attachments: vec![], task_id: "research".into(), interaction_id: id.into(),
        body: "Viable: index once and follow changes".into(), targets, is_final, nothing_to_review: false, card,
    }
}
fn delivered_with_plan() -> DomainState {
    let mut s = fixture();
    ok(&mut s, agent(), result("report", true, text(), Some(PLAN)));
    s
}
fn decide(choice: PlanChoice) -> Command {
    Command::DecideProposedPlan { presentation_id: "report".into(), targets: text(), choice }
}
fn accepted(s: &DomainState, id: &str) -> bool {
    matches!(&s.interactions[&InteractionId::from(id)].payload, InteractionPayload::Delivery { acceptance: Some(a), .. } if a.valid)
}

#[test]
fn a_result_that_proposes_a_plan_cannot_be_accepted_without_saying_whether_to_build_it() {
    let mut s = delivered_with_plan();
    let accept = Command::AcceptDelivery { presentation_id: "report".into(), targets: text() };
    assert_eq!(refused(&mut s, user(), accept), "plan_choice_required");
    assert!(!accepted(&s, "report"));
    let kinds: Vec<String> = inbox_items(&s).into_iter().flat_map(|item| item.available_actions)
        .filter(|action| action.presentation_id.as_ref() == Some(&InteractionId::from("report")))
        .map(|action| action.kind).collect();
    assert!(kinds.contains(&"implement_plan".to_owned()) && kinds.contains(&"archive_plan".to_owned()), "{kinds:?}");
    assert!(!kinds.contains(&"accept_delivery".to_owned()), "{kinds:?}");
}

#[test]
fn approving_the_plan_closes_the_research_and_hands_the_orchestrator_a_request_holding_that_plan() {
    let mut s = delivered_with_plan();
    let outcome = ok(&mut s, user(), decide(PlanChoice::Implement));
    assert!(accepted(&s, "report"), "the research is accepted and closes as any accepted result");
    let request = RequestId::from("implement-report");
    assert_eq!(s.requests[&request].body, PLAN, "the request carries the whole plan");
    assert_eq!(s.request_plans[&request], ApprovedPlan { task_id: "research".into(), presentation_id: "report".into() });
    assert!(outcome.effects.contains(&DomainEffect::NotifyOrchestrator { interaction_id: None, request_id: Some(request.clone()) }),
        "the orchestrator is told at once: {:?}", outcome.effects);
    s.validate().unwrap();
}

#[test]
fn only_archiving_closes_the_research_and_creates_nothing_to_build() {
    let mut s = delivered_with_plan();
    let outcome = ok(&mut s, user(), decide(PlanChoice::Archive));
    assert!(accepted(&s, "report"));
    assert_eq!(s.requests.len(), 1, "no new request");
    assert!(s.request_plans.is_empty());
    assert!(!outcome.effects.iter().any(|effect| matches!(effect, DomainEffect::NotifyOrchestrator { .. })), "{:?}", outcome.effects);
}

#[test]
fn the_one_task_that_implements_an_approved_plan_starts_with_that_plan_as_its_scope() {
    let mut s = delivered_with_plan();
    ok(&mut s, user(), decide(PlanChoice::Implement));
    ok(&mut s, orchestrator(), Command::TitleRequest { request_id: "implement-report".into(), title: "Index the whole library".into() });
    ok(&mut s, orchestrator(), create_task("build", "implement-report", None));
    let checklist = &s.checklists[&TaskId::from("build")];
    assert_eq!(checklist.plan, InteractionId::from("report"));
    let points: Vec<&str> = checklist.items.iter().map(|item| item.text.as_str()).collect();
    assert_eq!(points, ["Store feature prints in Core Data", "Index changes in the background"]);
    assert_eq!(refused(&mut s, orchestrator(), create_task("build-2", "implement-report", None)), "approved_plan_has_task");
    s.validate().unwrap();
}

#[test]
fn a_task_under_an_ordinary_request_starts_with_no_scope() {
    let mut s = fixture();
    ok(&mut s, orchestrator(), Command::TitleRequest { request_id: "r".into(), title: "Library index".into() });
    ok(&mut s, orchestrator(), create_task("other", "r", None));
    assert!(!s.checklists.contains_key(&TaskId::from("other")));
}

#[test]
fn a_plan_goes_only_on_a_final_result_without_a_pull_request_and_with_checklist_lines() {
    let mut s = fixture();
    assert_eq!(refused(&mut s, agent(), result("progress", false, text(), Some(PLAN))), "invalid_input");
    assert_eq!(refused(&mut s, agent(), result("phases", true, text(), Some("1. Index\n2. Ship"))), "plan_without_checklist");
    ok(&mut s, agent(), Command::RegisterPr {
        task_id: "research".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "o/r".into(),
        number: 1, head: "h".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    let pr = vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "h".into() }];
    assert_eq!(refused(&mut s, agent(), result("pr", true, pr, Some(PLAN))), "invalid_input");
}

#[test]
fn a_result_without_a_plan_is_accepted_as_before_and_cannot_be_decided_as_one() {
    let mut s = fixture();
    ok(&mut s, agent(), result("report", true, text(), None));
    assert_eq!(refused(&mut s, user(), decide(PlanChoice::Implement)), "invalid_input");
    ok(&mut s, user(), Command::AcceptDelivery { presentation_id: "report".into(), targets: text() });
    assert!(accepted(&s, "report"));
}

#[test]
fn asking_for_a_plan_yields_a_plan_card_whose_approval_starts_the_work_in_the_same_task() {
    let mut s = fixture();
    let card = CardSpec::new(CardKind::Review, "Index the whole library").content(ContentKind::Plan, PLAN);
    ok(&mut s, Actor::System, Command::HoldHook {
        interaction_id: "plan".into(), task_id: "research".into(), agent_id: "a".into(), hold: Hold::Plan,
        body: PLAN.into(), options: Hold::Plan.options(), card,
    });
    ok(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "plan".into(), option_id: Hold::APPROVE.into(), inputs: BTreeMap::new() });
    assert_eq!(s.checklists[&TaskId::from("research")].plan, InteractionId::from("plan"));
    assert!(!s.tasks[&TaskId::from("research")].state.terminal(), "the task goes on to implement");
    assert_eq!(s.requests.len(), 1, "no other request: the same task builds it");
}
