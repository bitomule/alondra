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

#[test]
fn a_confirmed_close_never_offers_an_impossible_retry_after_repeated_host_loss() {
    let mut s = accepted_in_an_open_turn();
    must(&mut s, node(NODE), Command::ConfirmCleanup { task_id: TASK.into(), discard: Vec::new() });
    let journal = s.lifecycle_journal.clone();
    for _ in 0..67 {
        let outcome = must(&mut s, Actor::System, Command::RecordHostLost {
            task_id: TASK.into(), reason: "Synthetic owner mismatch".into(), quiet: false,
        });
        assert!(!notifies_user(&outcome));
        assert_eq!(s.lifecycle_journal, journal, "closing does not queue another resume");
        assert!(ReadIndex::build(&s).waiting().is_empty());
        super::lifecycle_tests::assert_aggregate_rules(&s);
    }
}


#[test]
fn confirming_cleanup_retires_only_the_obsolete_recovery_decision() {
    {
        let mut s = accepted_in_an_open_turn();
        must(&mut s, Actor::System, Command::RecordHostLost { task_id: TASK.into(), reason: "The host stopped".into(), quiet: false });
        let card = s.lifecycle_of(&TASK.into()).unwrap().cards()[0].0.clone();
        let before = s.lifecycle_of(&TASK.into()).unwrap();
        let pending = before.pending_inputs().iter().map(|input| input.as_str().to_owned()).collect::<Vec<_>>();
        let activity = before.activity();
        must(&mut s, node(NODE), Command::ConfirmCleanup { task_id: TASK.into(), discard: vec![] });
        let after = s.lifecycle_of(&TASK.into()).unwrap();
        assert!(after.cards().is_empty());
        assert_eq!(after.activity(), activity);
        assert_eq!(after.pending_inputs().iter().map(|input| input.as_str().to_owned()).collect::<Vec<_>>(), pending);
        assert!(s.interactions[&card].withdrawn);
        super::lifecycle_tests::assert_aggregate_rules(&s);
    }
}


#[test]
fn an_open_reply_request_prevents_completion_and_cleanup_of_an_accepted_result() {
    let mut s = accepted_in_an_open_turn();
    must(&mut s, user(), Command::CreateCardReply {
        request_id: "reply".into(), interaction_id: "final".into(), body: "Explain the result before closing".into(), auto_merge: false,
    });
    assert!(!s.finished(&s.tasks[&TaskId::from(TASK)]));
    for (actor, command) in [
        (Actor::System, Command::CompleteTask { task_id: TASK.into(), presentation_id: "final".into() }),
        (node(NODE), Command::ConfirmCleanup { task_id: TASK.into(), discard: vec![] }),
    ] {
        let before = s.clone();
        assert_eq!(exec(&mut s, actor, command).unwrap_err().code, "pending_user_reply");
        assert_eq!(s, before);
        assert!(!s.requests[&RequestId::from("reply")].closed);
    }
}

#[test]
fn consuming_a_reply_to_a_merged_pr_does_not_replace_the_answer_it_is_owed() {
    let mut s = world();
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: NODE.into(), name: NODE.into(), at: 0 });
    sdk_turn(&mut s, TASK, NODE);
    must(&mut s, node(NODE), Command::RegisterPr {
        task_id: TASK.into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(), number: 1,
        head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    must(&mut s, node(NODE), Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
    });
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets: targets() });
    assert_eq!(s.pull_requests[&PrId::from("pr")].state, PrState::Open);
    must(&mut s, user(), Command::SubmitFeedback {
        presentation_id: "final".into(), targets: targets(), body: "Explain the change".into(), attachments: vec![],
        instruction: Instruction::Reply, merge_failure: None,
    });
    must(&mut s, Actor::System, Command::TellNode {
        task_id: TASK.into(), text: "Explain the change".into(), from_user: false, answers: None,
    });
    let input = s.lifecycle_of(&TASK.into()).unwrap().pending_inputs()[0].as_str().to_owned();
    must(&mut s, Actor::System, Command::RecordRuntimeFacts {
        task_id: TASK.into(), generation: 1, facts: vec![RuntimeObservation::InputConsumed { input }],
    });
    let turn = s.lifecycle_of(&TASK.into()).unwrap().turn().unwrap().as_str().to_owned();
    must(&mut s, Actor::System, Command::RecordTurnBoundary {
        task_id: TASK.into(), generation: 1, turn, failure: None, failed: None,
    });
    must(&mut s, Actor::System, Command::ObservePrState {
        pr_id: "pr".into(), head: "head1".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()),
    });
    assert!(!s.finished(&s.tasks[&TaskId::from(TASK)]), "consuming the input did not deliver an answer");
    assert!(!s.in_cleanup(&s.tasks[&TaskId::from(TASK)]));
    assert!(s.interactions[&InteractionId::from("final")].body != "Explain the change");
}

#[test]
fn confirming_cleanup_retires_a_host_loss_that_reused_an_unheard_card() {
    let mut s = accepted_in_an_open_turn();
    let task_id = TaskId::from(TASK);
    must(&mut s, node(NODE), Command::DeclarePostMergeWork {
        task_id: task_id.clone(),
        reason: Some("Publish the release".into()),
        waits_for: vec![],
    });
    must(&mut s, Actor::System, Command::RecordHostEnded {
        task_id: task_id.clone(), generation: 1, resumes: false,
        ended_work: vec![], by_alondra: false,
    });
    let ended = s.lifecycle_of(&task_id).unwrap();
    let card = ended.cards().into_iter().find_map(|(id, reason)|
        (reason == alondra_domain::WaitReason::Unheard).then(|| id.clone()))
        .expect("the ended host leaves the declared work on an unheard card");
    assert_eq!(ended.card_owner(&card), Some(alondra_domain::Owner::User));
    assert_eq!(s.interactions[&card].system_card, Some(SystemCard::SessionEnded));
    assert!(!retakes(&s.interactions[&card]));

    must(&mut s, Actor::System, Command::RecordHostLost {
        task_id: task_id.clone(), reason: "The replacement host could not start".into(), quiet: false,
    });
    let before = s.lifecycle_of(&task_id).unwrap();
    assert_eq!(before.cards(), [(&card, alondra_domain::WaitReason::Unheard)],
        "host loss reuses the actual obligation instead of creating a pause");
    assert_eq!(before.card_owner(&card), Some(alondra_domain::Owner::User));
    assert_eq!(s.interactions[&card].system_card, Some(SystemCard::SessionEnded));
    assert!(retakes(&s.interactions[&card]));
    let pending = before.pending_inputs().into_iter().cloned().collect::<Vec<_>>();
    let activity = before.activity();
    let execution = before.execution().cloned();

    let outcome = must(&mut s, node(NODE), Command::ConfirmCleanup {
        task_id: task_id.clone(), discard: vec![],
    });
    let after = s.lifecycle_of(&task_id).unwrap();
    assert!(after.cards().is_empty());
    assert_eq!(after.pending_inputs().into_iter().cloned().collect::<Vec<_>>(), pending);
    assert_eq!(after.activity(), activity);
    assert_eq!(after.execution(), execution.as_ref());
    assert!(after.post_merge().is_none(), "the cleanup ends the declared work");
    assert!(s.interactions[&card].withdrawn);
    assert!(!notifies_user(&outcome));
    assert!(s.lifecycle_journal[&task_id].iter().any(|record|
        matches!(record, LifecycleRecord::PostMergeEnded { fact: PostMergeEnd::Cleanup, .. })));
    super::lifecycle_tests::assert_aggregate_rules(&s);
}
