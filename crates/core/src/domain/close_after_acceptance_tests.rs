//! A task whose delivery was accepted only closes. Its node may still need the user during the
//! cleanup Alondra asked for, but it plans nothing; once it confirmed that cleanup it opens no
//! card, never reads as working although its last turn is still open, and no host takes it over.
use super::lifecycle_tests::{exec, must, node, sdk_turn, user, world};
use super::*;
use alondra_domain::ActivityView;
use std::collections::BTreeMap;

const TASK: &str = "closing";
const NODE: &str = "n";

fn targets() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }]
}

/// A node presented its PR inside a turn that stays open, the user accepted it and GitHub merged
/// it, so Alondra asked the node to clean up.
fn accepted_in_an_open_turn() -> DomainState {
    let mut s = world();
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: NODE.into(), name: NODE.into(), at: 0 });
    sdk_turn(&mut s, TASK, NODE);
    let _ = exec(&mut s, node(NODE), Command::StartTask { task_id: TASK.into() });
    must(&mut s, node(NODE), Command::RegisterPr {
        task_id: TASK.into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(), number: 1,
        head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    must(&mut s, node(NODE), Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
    });
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets: targets() });
    must(&mut s, Actor::System, Command::ObservePrState {
        pr_id: "pr".into(), head: "head1".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()),
    });
    assert!(s.in_cleanup(&s.tasks[&TaskId::from(TASK)]), "the node was asked to clean up");
    s
}

fn permission(s: &mut DomainState, epoch: u64) -> CommandOutcome {
    must(s, Actor::System, Command::RecordDecisionRequest {
        task_id: TASK.into(), generation: 1, epoch, hold: Hold::Permission, tool: Some("Bash".into()),
        input: serde_json::json!({"command": "rm -rf /Users/ana/.cache/probe"}), mcp_server: None,
        prompt: DecisionPrompt::Card { body: "Delete the probe".into(), options: Hold::Permission.options(),
            card: CardSpec::new(CardKind::Authorize, "Delete the probe") },
    })
}

fn plan(s: &mut DomainState, epoch: u64) -> CommandOutcome {
    must(s, Actor::System, Command::RecordDecisionRequest {
        task_id: TASK.into(), generation: 1, epoch, hold: Hold::Plan, tool: Some("ExitPlanMode".into()), input: serde_json::json!({}), mcp_server: None,
        prompt: DecisionPrompt::Card { body: "- [ ] Fix it".into(), options: Hold::Plan.options(),
            card: CardSpec::new(CardKind::Review, "Fix it").content(ContentKind::Plan, "- [ ] Fix it") },
    })
}

fn notifies_user(outcome: &CommandOutcome) -> bool {
    outcome.effects.iter().any(|effect| matches!(effect, DomainEffect::NotifyUser { .. }))
}

fn denied_by_policy(s: &DomainState, epoch: u64) -> Option<String> {
    s.lifecycle_journal[&TaskId::from(TASK)].iter().rev().find_map(|record| match record {
        LifecycleRecord::DecidedByPolicy { epoch: decided, decision: DecisionRecord::Deny { message } } if *decided == epoch => Some(message.clone()),
        _ => None,
    })
}

#[test]
fn an_accepted_task_plans_nothing_and_asks_nothing_once_its_cleanup_is_confirmed() {
    let mut s = accepted_in_an_open_turn();

    let asked = permission(&mut s, 1);
    assert!(s.interactions.contains_key(&decision_card_id(&TASK.into(), 1, 1)), "a permission its cleanup needs is asked");
    assert!(notifies_user(&asked));

    let planned = plan(&mut s, 2);
    assert!(!s.interactions.contains_key(&decision_card_id(&TASK.into(), 1, 2)), "an accepted task presents no plan");
    assert!(!notifies_user(&planned));
    assert_eq!(denied_by_policy(&s, 2), Some(plan_after_acceptance_notice()));

    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: decision_card_id(&TASK.into(), 1, 1),
        option_id: Hold::ALLOW.into(), inputs: BTreeMap::new() });
    must(&mut s, node(NODE), Command::ConfirmCleanup { task_id: TASK.into(), discard: Vec::new() });

    let late = permission(&mut s, 3);
    assert!(!s.interactions.contains_key(&decision_card_id(&TASK.into(), 1, 3)), "a closing node opens no card");
    assert!(!notifies_user(&late), "no push leaves for a card the close would retire");
    assert_eq!(denied_by_policy(&s, 3), Some(node_closing_notice()));

    let options = vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None }];
    let refused = exec(&mut s, node(NODE), Command::AskQuestion {
        attachments: vec![], interaction_id: "late-question".into(), scope: Scope::Task { task_id: TASK.into() },
        recipient: Recipient::User, body: "Anything else?".into(), blocking: true, options: options.clone(),
        card: CardSpec::question("Anything else?", &options),
    }).unwrap_err();
    assert_eq!(refused.code, CLOSING);
}

#[test]
fn a_task_whose_cleanup_is_confirmed_reads_as_closing_and_is_never_taken_over() {
    let mut s = accepted_in_an_open_turn();
    let mut asked_to_clean = s.clone();
    must(&mut asked_to_clean, Actor::System, Command::RecordRuntimeFacts { task_id: TASK.into(), generation: 1, facts: vec![RuntimeObservation::TakenOver] });

    must(&mut s, node(NODE), Command::ConfirmCleanup { task_id: TASK.into(), discard: Vec::new() });
    let task = s.tasks[&TaskId::from(TASK)].clone();
    assert_eq!(s.lifecycle_of(&task.id).unwrap().activity(), ActivityView::Working, "its last turn is still open");
    assert!(s.node_closing(&task));
    assert_eq!(ReadIndex::build(&s).task(&task.id).unwrap().situation, Situation::Delivered(Delivery::Closing));

    let journal = s.lifecycle_journal.clone();
    let refused = exec(&mut s, Actor::System, Command::RecordRuntimeFacts {
        task_id: TASK.into(), generation: 1, facts: vec![RuntimeObservation::TakenOver],
    }).unwrap_err();
    assert_eq!(refused.code, CLOSING);
    assert_eq!(s.lifecycle_journal, journal);
}
