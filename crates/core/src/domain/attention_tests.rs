//! What is a card, and when does the user hear about it: `attention`, the two task relations and
//! the transitions that turn a wait into a card and one push.
use super::*;
use std::collections::BTreeSet;

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}
fn agent(id: &str) -> Actor {
    Actor::Agent { agent_id: id.into() }
}
fn exec(s: &mut DomainState, a: Actor, c: Command) -> Result<CommandOutcome, DomainError> {
    s.execute(&a, &CommandEnvelope {
        protocol_version: 2,
        data_epoch: s.data_epoch.clone(),
        command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision,
        command: c,
    })
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
fn state_of(s: &DomainState, task: &str) -> TaskState {
    s.tasks[&task.into()].state
}
fn notified_user(outcome: &CommandOutcome) -> Vec<String> {
    outcome
        .effects
        .iter()
        .filter_map(|e| match e {
            DomainEffect::NotifyUser { interaction_id, .. } => Some(interaction_id.0.clone()),
            _ => None,
        })
        .collect()
}
fn notified_agent(outcome: &CommandOutcome, who: &str) -> usize {
    outcome.effects.iter().filter(|e| matches!(e, DomainEffect::NotifyAgent { agent_id, .. } if agent_id.0 == who)).count()
}

/// Two tasks, `a` the work and `b` its reviewer, each with its own node; nothing related yet.
fn pair() -> DomainState {
    let mut s = DomainState::new("epoch");
    for agent_id in ["wa", "wb"] {
        ok(&mut s, Actor::System, Command::RegisterAgent { agent_id: agent_id.into(), name: agent_id.into(), at: 0 });
    }
    ok(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Build it".into(), auto_merge: false, maintenance: false, message: false });
    ok(&mut s, user(), Command::TitleRequest { request_id: "r".into(), title: "Build it".into() });
    for (id, node) in [("a", "wa"), ("b", "wb")] {
        ok(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
            task_id: id.into(), request_id: "r".into(), title: id.to_uppercase(), agent_id: Some(node.into()),
            review_prs: false, dependencies: BTreeSet::new(), stack: None,
            launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")),
        });
    }
    s
}
fn review_relation(s: &mut DomainState) {
    ok(s, user(), Command::SetRelations {
        task_id: "b".into(),
        delivered_dependencies: BTreeSet::from(["a".into()]),
        blocks: BTreeSet::from(["a".into()]),
    });
}
fn present(s: &mut DomainState, who: &str, task: &str, id: &str) -> CommandOutcome {
    ok(s, agent(who), Command::PresentResult {
        attachments: vec![], task_id: task.into(), interaction_id: id.into(), body: "Done".into(),
        targets: vec![DeliveryTarget::Text { text: "The result".into() }],
        is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
    })
}
fn verdict(s: &mut DomainState, presentation: &str, verdict: Verdict) -> Result<CommandOutcome, DomainError> {
    exec(s, agent("wb"), Command::RecordVerdict { task_id: "b".into(), presentation_id: presentation.into(), verdict })
}
fn changes(text: &str) -> Verdict {
    Verdict::ChangesRequested { text: text.into(), serious_findings: Some(1) }
}
fn approved(summary: &str) -> Verdict {
    Verdict::Approved { summary: summary.into(), open_issues: false, serious_findings: 0 }
}
/// The review is under way: `a` delivered, `b` started and delivered its own result.
fn reviewing(s: &mut DomainState) {
    review_relation(s);
    present(s, "wa", "a", "da");
    ok(s, agent("wb"), Command::StartTask { task_id: "b".into() });
    present(s, "wb", "b", "db");
}

#[test]
fn a_delivery_waits_for_the_task_that_blocks_it_and_has_no_card_until_it_approves() {
    let mut s = pair();
    review_relation(&mut s);
    let delivered = present(&mut s, "wa", "a", "da");
    assert_eq!(state_of(&s, "a"), TaskState::AwaitingBlocker);
    assert_eq!(s.attention(&s.tasks[&"a".into()]), Attention::Wait(WaitReason::BlockedBy { task: "b".into() }));
    assert!(notified_user(&delivered).is_empty(), "no push while a task blocks it");
    assert!(!s.item(&s.interactions[&"da".into()]).pending);
    assert!(s.acting().is_empty());
    ok(&mut s, agent("wb"), Command::StartTask { task_id: "b".into() });
    present(&mut s, "wb", "b", "db");
    assert!(s.acting().is_empty(), "the reviewer's own delivery is a verdict, never a card");
    assert_eq!(state_of(&s, "b"), TaskState::Running);
    let outcome = verdict(&mut s, "db", approved("4 fixed")).unwrap();
    assert_eq!(notified_user(&outcome), ["da"], "exactly one card and one push");
    assert_eq!(state_of(&s, "a"), TaskState::ForYourReview);
    assert!(s.item(&s.interactions[&"da".into()]).pending);
    assert!(matches!(s.tasks[&"b".into()].state, TaskState::Closing | TaskState::Done), "the reviewer closes by itself");
    assert_eq!(s.tasks[&"b".into()].verdict, Some(approved("4 fixed")));
}

#[test]
fn a_review_that_asks_for_changes_sends_the_work_back_and_stays_open_for_the_next_round() {
    let mut s = pair();
    reviewing(&mut s);
    let outcome = verdict(&mut s, "db", changes("Handle the empty list")).unwrap();
    assert_eq!(notified_agent(&outcome, "wa"), 1, "the node of the work gets the findings");
    assert!(notified_user(&outcome).is_empty());
    assert_eq!(state_of(&s, "a"), TaskState::Running);
    assert_eq!(s.task_handed_back(&"a".into()), Some(Instruction::Changes));
    let InteractionPayload::Delivery { feedback, .. } = &s.interactions[&"da".into()].payload else { panic!() };
    assert_eq!((feedback[0].body.as_str(), feedback[0].user_id.as_str()), ("Handle the empty list", Feedback::ALONDRA));
    assert_eq!(state_of(&s, "b"), TaskState::Running, "the reviewer stays open with its node");
    assert!(s.acting().is_empty());
    let again = present(&mut s, "wa", "a", "da2");
    assert_eq!(notified_agent(&again, "wb"), 1, "the reviewer hears about the next delivery");
    assert!(notified_user(&again).is_empty());
    assert_eq!(state_of(&s, "a"), TaskState::AwaitingBlocker);
    present(&mut s, "wb", "b", "db2");
    let outcome = verdict(&mut s, "db2", approved("fixed")).unwrap();
    assert_eq!(notified_user(&outcome), ["da2"]);
    assert_eq!(state_of(&s, "a"), TaskState::ForYourReview);
}

#[test]
fn after_three_rounds_the_reviewers_last_word_closes_the_review_with_its_open_issues() {
    let mut s = pair();
    reviewing(&mut s);
    for round in 1..=MAX_REVIEW_ROUNDS {
        verdict(&mut s, &format!("db{}", if round == 1 { String::new() } else { round.to_string() }), changes("Still wrong")).unwrap();
        assert_eq!(s.tasks[&"a".into()].review_rounds, round, "the rounds are counted on the task under review");
        present(&mut s, "wa", "a", &format!("da{}", round + 1));
        present(&mut s, "wb", "b", &format!("db{}", round + 1));
    }
    let last = format!("db{}", MAX_REVIEW_ROUNDS + 1);
    let outcome = verdict(&mut s, &last, changes("Two failures remain")).unwrap();
    assert_eq!(notified_user(&outcome), [format!("da{}", MAX_REVIEW_ROUNDS + 1)]);
    assert_eq!(s.tasks[&"b".into()].verdict, Some(Verdict::Approved { summary: "Two failures remain".into(), open_issues: true, serious_findings: 1 }));
    assert_eq!(state_of(&s, "a"), TaskState::ForYourReview);
}

#[test]
fn a_verdict_needs_a_task_that_blocks_another_and_its_current_final_delivery() {
    let mut s = pair();
    present(&mut s, "wb", "b", "db");
    assert_eq!(refused(&mut s, agent("wb"), Command::RecordVerdict { task_id: "b".into(), presentation_id: "db".into(), verdict: approved("ok") }), "invalid_input");
    review_relation(&mut s);
    assert_eq!(refused(&mut s, agent("wb"), Command::RecordVerdict { task_id: "b".into(), presentation_id: "other".into(), verdict: approved("ok") }), "acceptance_stale");
    assert_eq!(refused(&mut s, agent("wa"), Command::RecordVerdict { task_id: "b".into(), presentation_id: "db".into(), verdict: approved("ok") }), "not_assigned");
    assert_eq!(refused(&mut s, agent("wb"), Command::RecordVerdict { task_id: "b".into(), presentation_id: "db".into(), verdict: changes(" ") }), "invalid_input");
}

#[test]
fn delivered_starts_a_task_at_the_final_delivery_and_done_only_when_it_closes() {
    let mut s = pair();
    ok(&mut s, user(), Command::SetRelations {
        task_id: "b".into(), delivered_dependencies: BTreeSet::from(["a".into()]), blocks: BTreeSet::new(),
    });
    assert_eq!(state_of(&s, "b"), TaskState::WaitingDependency);
    assert_eq!(refused(&mut s, agent("wb"), Command::StartTask { task_id: "b".into() }), "dependency_not_delivered");
    present(&mut s, "wa", "a", "da");
    assert_eq!(state_of(&s, "b"), TaskState::NotStarted, "ready as soon as `a` delivers");
    ok(&mut s, agent("wb"), Command::StartTask { task_id: "b".into() });
    assert_eq!(state_of(&s, "a"), TaskState::ForYourReview, "a plain delivered dependency blocks nothing");

    let mut s = pair();
    ok(&mut s, user(), Command::SetDependencies { task_id: "b".into(), dependencies: BTreeSet::from(["a".into()]), stack: None });
    present(&mut s, "wa", "a", "da");
    assert_eq!(state_of(&s, "b"), TaskState::WaitingDependency, "Done still waits for the task to finish");
}

#[test]
fn relations_reject_cycles_and_deadlocks_but_allow_the_review_pair() {
    let mut s = pair();
    review_relation(&mut s);
    assert_eq!(refused(&mut s, user(), Command::SetRelations {
        task_id: "a".into(), delivered_dependencies: BTreeSet::new(), blocks: BTreeSet::from(["b".into()]),
    }), "dependency_cycle", "two tasks cannot block each other");
    assert_eq!(refused(&mut s, user(), Command::SetRelations {
        task_id: "a".into(), delivered_dependencies: BTreeSet::from(["b".into()]), blocks: BTreeSet::new(),
    }), "dependency_cycle", "starting after each other's delivery is a cycle");
    assert_eq!(refused(&mut s, user(), Command::SetRelations {
        task_id: "b".into(), delivered_dependencies: BTreeSet::new(), blocks: BTreeSet::from(["b".into()]),
    }), "dependency_cycle");
    assert_eq!(refused(&mut s, user(), Command::SetDependencies {
        task_id: "b".into(), dependencies: BTreeSet::from(["a".into()]), stack: None,
    }), "dependency_cycle", "a task cannot wait for `a` to be done while it blocks it");
}

#[test]
fn a_question_that_holds_a_delivery_back_hides_its_card_until_it_is_answered() {
    let mut s = pair();
    present(&mut s, "wa", "a", "da");
    assert_eq!(s.attention(&s.tasks[&"a".into()]), Attention::Act(CardRef { interaction: "da".into(), revision: s.interactions[&"da".into()].revision }));
    let mut card = CardSpec::new(CardKind::Decide, "Pick a name").why("The result waits for it");
    card.blocks = vec![CardTarget::Delivery { interaction_id: "da".into() }];
    let asked = ok(&mut s, agent("wa"), Command::AskQuestion {
        attachments: vec![], interaction_id: "q".into(), scope: Scope::Task { task_id: "a".into() },
        recipient: Recipient::User, body: "Which name?".into(), blocking: false,
        options: vec![
            QuestionOption { id: "x".into(), label: "X".into(), description: None, inputs: vec![] },
            QuestionOption { id: "y".into(), label: "Y".into(), description: None, inputs: vec![] },
        ],
        card,
    });
    assert_eq!(notified_user(&asked), ["q"]);
    assert_eq!(s.attention(&s.tasks[&"a".into()]), Attention::Wait(WaitReason::Answer { need: "Pick a name".into() }));
    assert!(!s.item(&s.interactions[&"da".into()]).pending);
    let answered = ok(&mut s, user(), Command::AnswerQuestion {
        attachments: vec![], interaction_id: "q".into(), option_id: "x".into(), inputs: Default::default(),
    });
    assert_eq!(notified_user(&answered), ["da"], "one card, one push, when the answer lifts the hold");
    assert!(s.item(&s.interactions[&"da".into()]).pending);
}

#[test]
fn nothing_notifies_twice_and_a_rebuilt_state_does_not_notify_again() {
    let mut s = pair();
    reviewing(&mut s);
    let outcome = verdict(&mut s, "db", approved("ok")).unwrap();
    assert_eq!(notified_user(&outcome), ["da"]);
    let mut restarted: DomainState = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    for state in [&mut s, &mut restarted] {
        let later = ok(state, user(), Command::RetitleTask { task_id: "a".into(), title: "A, renamed".into() });
        assert!(notified_user(&later).is_empty(), "an unrelated command never repeats the push");
    }
    assert_eq!(restarted.acting(), s.acting());
}

#[test]
fn old_states_without_the_new_fields_and_in_review_still_load() {
    assert_eq!(serde_json::from_str::<TaskState>("\"in_review\"").unwrap(), TaskState::ForYourReview);
    assert_eq!(serde_json::to_string(&TaskState::AwaitingBlocker).unwrap(), "\"awaiting_blocker\"");
    let s = pair();
    let json = serde_json::to_value(&s.tasks[&"a".into()]).unwrap();
    for field in ["blocks", "delivered_dependencies", "verdict", "review_rounds"] {
        assert!(json.get(field).is_none(), "{field} is omitted while empty, so old readers see the same row");
    }
    let back: Task = serde_json::from_value(json).unwrap();
    assert_eq!(back, s.tasks[&"a".into()]);
}

#[test]
fn every_state_of_the_fixtures_lists_only_cards_with_an_action_to_take() {
    let mut s = pair();
    review_relation(&mut s);
    let mut steps: Vec<DomainState> = vec![s.clone()];
    present(&mut s, "wa", "a", "da");
    steps.push(s.clone());
    ok(&mut s, agent("wb"), Command::StartTask { task_id: "b".into() });
    present(&mut s, "wb", "b", "db");
    steps.push(s.clone());
    verdict(&mut s, "db", changes("Fix it")).unwrap();
    steps.push(s.clone());
    present(&mut s, "wa", "a", "da2");
    steps.push(s.clone());
    present(&mut s, "wb", "b", "db2");
    verdict(&mut s, "db2", approved("fine")).unwrap();
    steps.push(s);
    for state in steps {
        for item in state.projection().inbox.iter().filter(|item| item.pending) {
            assert!(item.blocked_by.is_empty(), "{} is listed while held back", item.id);
            assert!(
                item.available_actions.iter().any(|action| action.enabled),
                "{} is listed with nothing the user can do",
                item.id
            );
        }
    }
}

// ---- Review of PR #742: one test per point ----

fn register_pr(s: &mut DomainState, who: &str, task: &str) {
    ok(s, agent(who), Command::RegisterPr {
        task_id: task.into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
        number: 1, head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
}
fn pr_target() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }]
}
fn present_pr(s: &mut DomainState, task: &str, who: &str, id: &str) -> CommandOutcome {
    ok(s, agent(who), Command::PresentResult {
        attachments: vec![], task_id: task.into(), interaction_id: id.into(), body: "Done".into(), targets: pr_target(),
        is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
    })
}
fn merged_outside(s: &mut DomainState) -> CommandOutcome {
    ok(s, Actor::System, Command::ObservePrState {
        pr_id: "pr".into(), head: "head1".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()),
    })
}

#[test]
fn nothing_is_merged_or_accepted_while_a_review_of_the_task_is_open() {
    let mut s = pair();
    ok(&mut s, user(), Command::CreateRequest { request_id: "auto".into(), body: "Auto".into(), auto_merge: true, maintenance: false, message: false });
    ok(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
        task_id: "c".into(), request_id: "auto".into(), title: "C".into(), agent_id: None, review_prs: false,
        dependencies: BTreeSet::new(), stack: None,
        launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")),
    });
    ok(&mut s, Actor::System, Command::RegisterAgent { agent_id: "wc".into(), name: "wc".into(), at: 0 });
    ok(&mut s, user(), Command::AssignTask { task_id: "c".into(), agent_id: "wc".into(), model: None });
    ok(&mut s, user(), Command::SetRelations {
        task_id: "b".into(), delivered_dependencies: BTreeSet::from(["c".into()]), blocks: BTreeSet::from(["c".into()]),
    });
    register_pr(&mut s, "wc", "c");
    present_pr(&mut s, "c", "wc", "dc");
    let authorize = || Command::AuthorizeMerge {
        authorization_id: "auto-auth".into(), presentation_id: "dc".into(), pr_id: "pr".into(),
        expected_head: "head1".into(), method: MergeMethod::Squash,
    };
    assert_eq!(refused(&mut s, Actor::System, authorize()), "blocked", "Alondra does not auto-merge under an open review");
    assert_eq!(
        refused(&mut s, user(), Command::AcceptDelivery { presentation_id: "dc".into(), targets: pr_target() }),
        "blocked",
        "nor does an acceptance go through"
    );
    ok(&mut s, agent("wb"), Command::StartTask { task_id: "b".into() });
    present(&mut s, "wb", "b", "db");
    verdict(&mut s, "db", approved("fine")).unwrap();
    ok(&mut s, Actor::System, authorize());
}

#[test]
fn an_approval_only_counts_for_the_delivery_the_reviewer_saw() {
    let mut s = pair();
    reviewing(&mut s);
    let again = present(&mut s, "wa", "a", "da2");
    assert_eq!(notified_agent(&again, "wb"), 1, "the reviewer is told about the new delivery");
    assert_eq!(refused(&mut s, agent("wb"), Command::RecordVerdict {
        task_id: "b".into(), presentation_id: "db".into(), verdict: approved("fine"),
    }), "acceptance_stale");
    assert_eq!(refused(&mut s, agent("wb"), Command::RecordVerdict {
        task_id: "b".into(), presentation_id: "db".into(), verdict: changes("old findings"),
    }), "acceptance_stale");
    assert!(s.acting().is_empty(), "the old approval never lets the new delivery through");
    present(&mut s, "wb", "b", "db2");
    let outcome = verdict(&mut s, "db2", approved("fine")).unwrap();
    assert_eq!(notified_user(&outcome), ["da2"]);
}

#[test]
fn a_repeated_verdict_neither_duplicates_the_feedback_nor_spends_another_round() {
    let mut s = pair();
    reviewing(&mut s);
    verdict(&mut s, "db", changes("Handle the empty list")).unwrap();
    let before = s.clone();
    let repeated = verdict(&mut s, "db", changes("Handle the empty list, again")).unwrap();
    assert!(repeated.effects.is_empty() && !repeated.changed);
    assert_eq!(s, before);
    let InteractionPayload::Delivery { feedback, .. } = &s.interactions[&"da".into()].payload else { panic!() };
    assert_eq!(feedback.len(), 1);
    assert_eq!(s.tasks[&"a".into()].review_rounds, 1);
    assert_eq!(refused(&mut s, agent("wb"), Command::RecordVerdict {
        task_id: "b".into(), presentation_id: "db".into(), verdict: approved("never mind"),
    }), "invalid_input", "a different verdict on the same result is not a repeat");
}

#[test]
fn a_cancelled_reviewer_lets_the_reviewed_task_through_with_one_card_and_one_push() {
    let mut s = pair();
    reviewing(&mut s);
    let outcome = ok(&mut s, user(), Command::CancelTask { task_id: "b".into() });
    assert_eq!(notified_user(&outcome), ["da"]);
    assert_eq!(state_of(&s, "a"), TaskState::ForYourReview);
    assert!(s.item(&s.interactions[&"da".into()]).pending);
}

#[test]
fn a_cancelled_reviewed_task_cancels_its_reviewer_and_never_gives_the_user_its_delivery() {
    let mut s = pair();
    reviewing(&mut s);
    let outcome = ok(&mut s, user(), Command::CancelTask { task_id: "a".into() });
    assert_eq!(state_of(&s, "b"), TaskState::Cancelled);
    assert_eq!(notified_agent(&outcome, "wb"), 1, "the reviewer's node is told");
    assert!(notified_user(&outcome).is_empty());
    assert!(s.acting().is_empty());
    assert!(!s.item(&s.interactions[&"db".into()]).pending, "the reviewer's delivery is never a card");
}

#[test]
fn a_pr_merged_outside_cancels_the_reviewer_too() {
    let mut s = pair();
    review_relation(&mut s);
    register_pr(&mut s, "wa", "a");
    present_pr(&mut s, "a", "wa", "da");
    ok(&mut s, agent("wb"), Command::StartTask { task_id: "b".into() });
    present(&mut s, "wb", "b", "db");
    let outcome = merged_outside(&mut s);
    assert_eq!(state_of(&s, "b"), TaskState::Cancelled);
    assert_eq!(notified_agent(&outcome, "wb"), 1);
    assert!(s.acting().is_empty());
}

#[test]
fn a_reviewer_cannot_hold_a_pull_request() {
    let mut s = pair();
    review_relation(&mut s);
    assert_eq!(refused(&mut s, agent("wb"), Command::RegisterPr {
        task_id: "b".into(), pr_id: "pr-b".into(), host: "github.com".into(), repository: "owner/repo".into(),
        number: 2, head: "head2".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    }), "invalid_input");
    let mut s = pair();
    register_pr(&mut s, "wb", "b");
    assert_eq!(refused(&mut s, user(), Command::SetRelations {
        task_id: "b".into(), delivered_dependencies: BTreeSet::from(["a".into()]), blocks: BTreeSet::from(["a".into()]),
    }), "invalid_input");
}

#[test]
fn only_an_open_blocker_puts_a_task_in_awaiting_blocker_and_other_waits_show_their_own_reason() {
    let mut s = pair();
    present(&mut s, "wa", "a", "da");
    let mut card = CardSpec::new(CardKind::Decide, "Pick a name").why("The result waits for it");
    card.blocks = vec![CardTarget::Delivery { interaction_id: "da".into() }];
    ok(&mut s, agent("wa"), Command::AskQuestion {
        attachments: vec![], interaction_id: "q".into(), scope: Scope::Task { task_id: "a".into() },
        recipient: Recipient::User, body: "Which?".into(), blocking: false,
        options: vec![
            QuestionOption { id: "x".into(), label: "X".into(), description: None, inputs: vec![] },
            QuestionOption { id: "y".into(), label: "Y".into(), description: None, inputs: vec![] },
        ],
        card,
    });
    assert_eq!(s.attention(&s.tasks[&"a".into()]), Attention::Wait(WaitReason::Answer { need: "Pick a name".into() }));
    assert_ne!(state_of(&s, "a"), TaskState::AwaitingBlocker, "a question is not a task blocking it");
    let mut s = pair();
    review_relation(&mut s);
    present(&mut s, "wa", "a", "da");
    assert_eq!(state_of(&s, "a"), TaskState::AwaitingBlocker);
}

#[test]
fn the_node_resolving_conflicts_is_named_with_its_pr() {
    let mut s = pair();
    register_pr(&mut s, "wa", "a");
    present_pr(&mut s, "a", "wa", "da");
    ok(&mut s, Actor::System, Command::SubmitFeedback {
        attachments: vec![], presentation_id: "da".into(), targets: pr_target(), body: "Conflicts".into(), instruction: Instruction::Conflicts,
    });
    assert_eq!(s.attention(&s.tasks[&"a".into()]), Attention::Wait(WaitReason::NodeFixing { node: "wa".into(), pr: 1 }));
}

#[test]
fn the_rounds_are_counted_per_reviewed_task_across_all_its_reviewers() {
    let mut s = pair();
    ok(&mut s, Actor::System, Command::RegisterAgent { agent_id: "wc".into(), name: "wc".into(), at: 0 });
    ok(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
        task_id: "c".into(), request_id: "r".into(), title: "C".into(), agent_id: Some("wc".into()),
        review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")),
    });
    for reviewer in ["b", "c"] {
        ok(&mut s, user(), Command::SetRelations {
            task_id: reviewer.into(), delivered_dependencies: BTreeSet::from(["a".into()]), blocks: BTreeSet::from(["a".into()]),
        });
    }
    present(&mut s, "wa", "a", "da");
    for (who, task) in [("wb", "b"), ("wc", "c")] {
        ok(&mut s, agent(who), Command::StartTask { task_id: task.into() });
        present(&mut s, who, task, &format!("d{task}"));
    }
    verdict(&mut s, "db", changes("one")).unwrap();
    ok(&mut s, agent("wc"), Command::RecordVerdict { task_id: "c".into(), presentation_id: "dc".into(), verdict: changes("two") });
    assert_eq!(s.tasks[&"a".into()].review_rounds, 1, "two reviewers in the same round are one round");
    present(&mut s, "wa", "a", "da2");
    present(&mut s, "wb", "b", "db2");
    present(&mut s, "wc", "c", "dc2");
    let outcome = verdict(&mut s, "db2", approved("fine")).unwrap();
    assert!(notified_user(&outcome).is_empty(), "A waits for every reviewer");
    ok(&mut s, agent("wc"), Command::RecordVerdict { task_id: "c".into(), presentation_id: "dc2".into(), verdict: approved("fine too") });
    assert_eq!(state_of(&s, "a"), TaskState::ForYourReview);
}

#[test]
fn a_review_of_a_task_that_is_already_over_is_refused_instead_of_cancelling_the_reviewer() {
    let mut s = pair();
    ok(&mut s, user(), Command::CancelTask { task_id: "a".into() });
    assert_eq!(refused(&mut s, user(), Command::SetRelations {
        task_id: "b".into(), delivered_dependencies: BTreeSet::new(), blocks: BTreeSet::from(["a".into()]),
    }), "invalid_input");
    assert_eq!(state_of(&s, "b"), TaskState::NotStarted, "the would-be reviewer is left alone");
}

#[test]
fn reopening_a_reviewer_whose_task_is_over_is_refused_and_does_not_cancel_it_in_the_same_command() {
    let mut s = pair();
    reviewing(&mut s);
    ok(&mut s, user(), Command::CancelTask { task_id: "a".into() });
    assert_eq!(state_of(&s, "b"), TaskState::Cancelled);
    assert_eq!(refused(&mut s, user(), Command::ReopenTask { task_id: "b".into() }), "invalid_input");
    assert_eq!(state_of(&s, "b"), TaskState::Cancelled);
}

#[test]
fn a_delivery_held_back_by_a_question_waits_in_its_waiting_state() {
    let mut s = pair();
    present(&mut s, "wa", "a", "da");
    let mut card = CardSpec::new(CardKind::Decide, "Pick a name").why("The result waits for it");
    card.blocks = vec![CardTarget::Delivery { interaction_id: "da".into() }];
    ok(&mut s, agent("wa"), Command::AskQuestion {
        attachments: vec![], interaction_id: "q".into(), scope: Scope::Task { task_id: "a".into() },
        recipient: Recipient::User, body: "Which?".into(), blocking: false,
        options: vec![
            QuestionOption { id: "x".into(), label: "X".into(), description: None, inputs: vec![] },
            QuestionOption { id: "y".into(), label: "Y".into(), description: None, inputs: vec![] },
        ],
        card,
    });
    assert_eq!(state_of(&s, "a"), TaskState::WaitingDependency);
    ok(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "q".into(), option_id: "x".into(), inputs: Default::default() });
    assert_eq!(state_of(&s, "a"), TaskState::ForYourReview, "answered: the user can act");
}

#[test]
fn a_task_cannot_block_one_that_depends_on_it() {
    let mut s = pair();
    ok(&mut s, user(), Command::SetRelations { task_id: "b".into(), delivered_dependencies: BTreeSet::from(["a".into()]), blocks: BTreeSet::new() });
    assert_eq!(refused(&mut s, user(), Command::SetRelations {
        task_id: "a".into(), delivered_dependencies: BTreeSet::new(), blocks: BTreeSet::from(["b".into()]),
    }), "dependency_cycle");
}

#[test]
fn asking_for_changes_needs_the_count_of_serious_findings() {
    let mut s = pair();
    reviewing(&mut s);
    let before = s.clone();
    let error = verdict(&mut s, "db", Verdict::ChangesRequested { text: "Handle the empty list".into(), serious_findings: None }).unwrap_err();
    assert_eq!((error.code.as_str(), error.subject.as_deref()), ("invalid_input", Some("serious_findings")));
    assert_eq!(s, before);
    verdict(&mut s, "db", Verdict::ChangesRequested { text: "Handle the empty list".into(), serious_findings: Some(0) }).unwrap();
    assert_eq!(state_of(&s, "a"), TaskState::Running, "0 is a count");
}

#[test]
fn a_verdict_recorded_before_the_count_reads_as_no_serious_findings() {
    let changes: Verdict = serde_json::from_str(r#"{"kind":"changes_requested","text":"Fix it"}"#).unwrap();
    let approved: Verdict = serde_json::from_str(r#"{"kind":"approved","summary":"Fine"}"#).unwrap();
    assert_eq!((changes.serious_findings(), approved.serious_findings()), (0, 0));
    assert_eq!(serde_json::to_string(&approved).unwrap(), r#"{"kind":"approved","summary":"Fine"}"#, "a zero count is not written");
}
