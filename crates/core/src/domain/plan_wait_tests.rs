//! A Task that waits for another takes no plan: its node's plan is denied without a card, a plan
//! already open is not approved, and only the orchestrator ends the wait.
use super::lifecycle_tests::{assert_aggregate_rules, exec, last, must, refused_unchanged, sdk_turn, task, user, world};
use super::*;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

const PLAN: &str = "# Offer Pause\n\n- [ ] Show Pause on a running task\n- [ ] Test it\n";

fn depend(s: &mut DomainState, on: &[&str]) {
    must(s, Actor::Orchestrator { generation: 0 }, Command::SetDependencies {
        task_id: "t".into(), dependencies: on.iter().map(|id| TaskId::from(*id)).collect::<BTreeSet<_>>(), stack: None, reason: None,});
}
fn sdk_plan(epoch: u64) -> Command {
    Command::RecordDecisionRequest {
        task_id: "t".into(), generation: 1, epoch, hold: Hold::Plan, tool: Some("ExitPlanMode".into()), input: json!({}), mcp_server: None,
        prompt: DecisionPrompt::Card {
            body: PLAN.into(), options: Hold::Plan.options(),
            card: CardSpec::new(CardKind::Review, "Offer Pause").content(ContentKind::Plan, PLAN),
        },
    }
}
fn hook_plan(id: &str) -> Command {
    Command::HoldHook {
        interaction_id: id.into(), task_id: "t".into(), agent_id: "a".into(), hold: Hold::Plan, body: PLAN.into(), options: Hold::Plan.options(),
        card: CardSpec::new(CardKind::Review, "Offer Pause").content(ContentKind::Plan, PLAN),
    }
}
fn answer(card: &str, option: &str) -> Command {
    let inputs = if option == Hold::REQUEST_CHANGES {
        BTreeMap::from([(Hold::NOTE.to_owned(), InputValue::Text("Wait for it".into()))])
    } else {
        BTreeMap::new()
    };
    Command::AnswerQuestion { attachments: vec![], interaction_id: card.into(), option_id: option.into(), inputs }
}
fn state(s: &DomainState) -> TaskState {
    s.tasks[&TaskId::from("t")].state
}
fn waiting_world() -> DomainState {
    let mut s = world();
    task(&mut s, "d", Some("b"));
    sdk_turn(&mut s, "t", "a");
    depend(&mut s, &["d"]);
    assert_eq!(state(&s), TaskState::WaitingDependency);
    s
}

#[test]
fn a_plan_presented_while_the_task_waits_is_denied_without_a_card_and_says_who_ends_the_wait() {
    let mut s = waiting_world();
    let cards = s.interactions.len();
    must(&mut s, Actor::System, sdk_plan(3));
    let LifecycleRecord::DecidedByPolicy { epoch: 3, decision: DecisionRecord::Deny { message } } = last(&s, "t") else {
        panic!("the plan is denied by Alondra, not put to the user")
    };
    assert!(message.contains("“Task d”") && message.contains("ask the orchestrator to remove it"), "{message}");
    assert_eq!(s.interactions.len(), cards, "no plan card while the task waits");
    assert!(!s.checklists.contains_key(&TaskId::from("t")));
    assert_eq!(state(&s), TaskState::WaitingDependency);
    assert_aggregate_rules(&s);
}

#[test]
fn a_hooked_plan_of_a_waiting_task_is_refused() {
    let mut s = waiting_world();
    let error = exec(&mut s, Actor::System, hook_plan("p")).unwrap_err();
    assert_eq!(error.code, "task_waiting");
    assert!(error.message.contains("“Task d”"), "{}", error.message);
    assert!(!s.interactions.contains_key(&InteractionId::from("p")));
}

/// The case of 20 October: the plan card was open when the task started to wait, and the user
/// approved it while the task still showed it waited.
#[test]
fn a_plan_open_when_the_task_starts_to_wait_is_not_approved_until_the_orchestrator_ends_the_wait() {
    let mut s = world();
    task(&mut s, "d", Some("b"));
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, sdk_plan(3));
    let card = decision_card_id(&"t".into(), 1, 3).0;
    depend(&mut s, &["d"]);
    assert_eq!(state(&s), TaskState::WaitingDependency);
    let before = s.clone();
    let error = exec(&mut s, user(), answer(&card, Hold::APPROVE)).unwrap_err();
    assert_eq!(error.code, "task_waiting");
    assert_eq!(error.refusal, Some(Refusal::WaitsForTask { title: "Task d".into() }));
    assert_eq!(s, before, "a refused approval changes nothing");
    assert_eq!(crate::views::refusal_text(error.refusal.as_ref().unwrap(), crate::views::Lang::Es),
        "Esta tarea espera a «Task d»: su plan se podrá aprobar cuando termine esa espera.");
    depend(&mut s, &[]);
    assert_ne!(state(&s), TaskState::WaitingDependency);
    must(&mut s, user(), answer(&card, Hold::APPROVE));
    assert_eq!(last(&s, "t"), LifecycleRecord::Decided { card: card.as_str().into(), epoch: 3, decision: DecisionRecord::Allow });
    assert!(s.checklists.contains_key(&TaskId::from("t")));
    assert_aggregate_rules(&s);
}

#[test]
fn changes_to_a_plan_can_be_requested_while_the_task_waits() {
    let mut s = world();
    task(&mut s, "d", Some("b"));
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, sdk_plan(3));
    let card = decision_card_id(&"t".into(), 1, 3).0;
    depend(&mut s, &["d"]);
    must(&mut s, user(), answer(&card, Hold::REQUEST_CHANGES));
    assert!(matches!(last(&s, "t"), LifecycleRecord::Decided { decision: DecisionRecord::Deny { .. }, .. }));
    assert_eq!(state(&s), TaskState::WaitingDependency);
    assert_eq!(refused_unchanged(&mut s, user(), answer(&card, Hold::APPROVE)), "question_already_answered");
    assert_aggregate_rules(&s);
}

#[test]
fn once_the_orchestrator_ends_the_wait_the_node_presents_its_plan_to_the_user() {
    let mut s = waiting_world();
    depend(&mut s, &[]);
    must(&mut s, Actor::System, sdk_plan(4));
    let card = decision_card_id(&"t".into(), 1, 4);
    assert_eq!(s.holds.get(&card), Some(&Hold::Plan));
    assert!(s.waited_tasks(&"t".into()).is_empty());
    assert_aggregate_rules(&s);
}
