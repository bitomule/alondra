//! A task owns every PR its node opens, in any repository, and its delivery is those PRs: nothing
//! else can reach the user while one is open. The incident that asked for this reads, step by
//! step, in `the_second_pr_of_the_mobile_menu_fix_reaches_the_user_with_its_merge`. Each PR of the
//! delivery is its own card, and the task closes once none is open and one merged: a delivery of
//! nine PRs on one card is `a_delivery_of_several_prs_is_one_card_per_pr_each_with_only_its_merge`.
use super::*;
use std::collections::BTreeSet;

const AGENT: &str = "a";
const TASK: &str = "t";
const PUBLIC: &str = "github.com/bitomule/alondra#17";
const CLOUD: &str = "github.com/acme/storefront#6";
const FIRST_HEAD: &str = "1111111111111111111111111111111111111111";
const CLOUD_HEAD: &str = "6666666666666666666666666666666666666666";

fn exec(s: &mut DomainState, actor: Actor, command: Command) -> Result<CommandOutcome, DomainError> {
    s.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision, command,
    })
}

fn must(s: &mut DomainState, actor: Actor, command: Command) {
    let shown = format!("{command:?}");
    if let Err(error) = exec(s, actor, command) {
        panic!("{shown} was refused: {error}");
    }
}

fn node() -> Actor {
    Actor::Agent { agent_id: AGENT.into() }
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn at_work() -> DomainState {
    let mut s = DomainState::new("epoch");
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: AGENT.into(), name: "Node".into(), at: 0 });
    must(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
        task_id: TASK.into(), request_id: "r".into(), title: "Fix the phone menu".into(), agent_id: None, review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    must(&mut s, Actor::System, Command::AdmitExecution { provider: alondra_core::accounts::Provider::Claude, task_id: TASK.into(), agent_id: AGENT.into(),
        conversation: "conversation".into(), instance: "instance".into(), host: "sdk-host".into(), model: None, account: None, leaves_limit: None });
    let LifecycleRecord::Planned { input } = s.lifecycle_journal[&TaskId::from(TASK)][0].clone() else { panic!("planned first") };
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: TASK.into(), generation: 1, facts: vec![
        RuntimeObservation::ResourceAvailable,
        RuntimeObservation::TurnStarted { turn: TURN.into(), input },
    ] });
    s
}

#[test]
fn a_closed_pr_is_never_a_delivery_target() {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    observe(&mut s, PUBLIC, FIRST_HEAD, PrState::Closed);
    let error = exec(&mut s, node(), result("closed-result", vec![pr(PUBLIC, FIRST_HEAD)], true)).unwrap_err();
    assert_eq!(error.code, "pr_not_open");
    assert_eq!(error.contract.unwrap().rule, ContractRule::PrNotOpen);
    assert!(s.open_prs(&s.tasks[&TASK.into()]).next().is_none());
}

#[test]
fn a_tasks_own_pr_cited_in_a_final_result_must_be_one_of_its_targets() {
    for citation in ["https://github.com/acme/storefront/pull/14", "acme/storefront#14", "**https://github.com/acme/storefront/pull/14**", "_acme/storefront#14_"] {
        let mut s = at_work();
        register(&mut s, "acme/storefront", 14, CLOUD_HEAD);
        let mut command = result("cited-result", vec![DeliveryTarget::Text { text: "Report".into() }], true);
        if let Command::PresentResult { body, .. } = &mut command { *body = format!("Finished: {citation}"); }
        let error = exec(&mut s, node(), command).unwrap_err();
        assert_eq!(error.code, "cited_pr_not_target", "{citation}");
        assert_eq!(error.contract.unwrap().rule, ContractRule::CitedPrNotTarget);
    }
}

#[test]
fn an_unregistered_pr_is_allowed_as_context_in_a_final_result() {
    for citation in ["https://github.com/acme/app/pull/143", "acme/app#143"] {
        let mut s = at_work();
        let mut command = result("context", text(), true);
        if let Command::PresentResult { body, card, .. } = &mut command {
            *body = format!("Keeps the changes from {citation}.");
            card.content.as_mut().unwrap().body = body.clone();
        }
        must(&mut s, node(), command);
        assert!(s.interactions.contains_key(&"context".into()));
    }
}

fn register_context_pr(s: &mut DomainState) {
    must(s, Actor::Orchestrator { generation: 0 }, Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
    must(s, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
        task_id: "other".into(), request_id: "r".into(), title: "Fix permissions".into(), agent_id: None,
        review_prs: false, plan_items: None, continues: None, dependencies: BTreeSet::new(), stack: None,
        launch: Some(LaunchPlan::new("node", "/synthetic/repository")),
    });
    must(s, Actor::System, Command::NotePrOpened {
        task_id: "other".into(), host: "github.com".into(), repository: "acme/app".into(), number: 143,
    });
    must(s, Actor::System, Command::RecordOpenedPr {
        task_id: "other".into(), host: "github.com".into(), repository: "acme/app".into(), number: 143,
        head: FIRST_HEAD.into(), base: "main".into(), state: PrState::Open, merged_head: None, allowed_methods: vec![MergeMethod::Squash],
    });
    assert_eq!(s.pull_requests[&hosted_pr_id("github.com", "acme/app", 143)].task_id, TaskId::from("other"));
}

#[test]
fn another_tasks_pr_is_allowed_as_context_in_a_final_result() {
    let mut s = at_work();
    register_context_pr(&mut s);
    let mut command = result("context", text(), true);
    if let Command::PresentResult { body, card, .. } = &mut command {
        *body = "Keeps the permissions fix from https://github.com/acme/app/pull/143 (acme/app#143).".into();
        card.content.as_mut().unwrap().body = body.clone();
    }
    must(&mut s, node(), command);
}

#[test]
fn a_proposed_plan_can_cite_another_tasks_pr_in_its_text_and_plan() {
    let mut s = at_work();
    register_context_pr(&mut s);
    let mut command = result("plan", text(), true);
    if let Command::PresentResult { body, card, .. } = &mut command {
        *body = "The permissions fix in https://github.com/acme/app/pull/143 is preserved.".into();
        card.content.as_mut().unwrap().body = body.clone();
        card.plan = Some("- [ ] Preserve the permissions fix in acme/app#143.".into());
    }
    must(&mut s, node(), command);
    assert!(s.interactions[&InteractionId::from("plan")].card.plan.is_some());
}

#[test]
fn a_proposed_plan_can_cite_its_own_closed_pr_in_its_plan() {
    let mut s = at_work();
    register(&mut s, "acme/app", 65, FIRST_HEAD);
    let old = hosted_pr_id("github.com", "acme/app", 65);
    observe(&mut s, &old.0, FIRST_HEAD, PrState::Closed);
    let mut command = result("plan", text(), true);
    if let Command::PresentResult { body, card, .. } = &mut command {
        *body = "The replacement follows the closed PR https://github.com/acme/app/pull/65 (acme/app#65).".into();
        card.content.as_mut().unwrap().body = body.clone();
        card.plan = Some("- [ ] Replace the closed PR https://github.com/acme/app/pull/65 (acme/app#65).".into());
    }
    must(&mut s, node(), command);
}

#[test]
fn progress_can_cite_its_own_closed_pr_without_delivering_it() {
    let mut s = at_work();
    register(&mut s, "acme/app", 65, FIRST_HEAD);
    let old = hosted_pr_id("github.com", "acme/app", 65);
    observe(&mut s, &old.0, FIRST_HEAD, PrState::Closed);
    let mut command = result("progress", text(), false);
    if let Command::PresentResult { body, card, .. } = &mut command {
        *body = "Closed https://github.com/acme/app/pull/65 (acme/app#65); the replacement is still being prepared.".into();
        card.content.as_mut().unwrap().body = body.clone();
    }
    must(&mut s, node(), command);
    assert!(s.tasks[&TaskId::from(TASK)].final_presentation.is_none());
}

#[test]
fn a_final_result_cannot_cite_its_own_closed_pr_while_delivering_another() {
    for citation in ["https://github.com/acme/app/pull/65", "acme/app#65"] {
        let mut s = at_work();
        register(&mut s, "acme/app", 65, FIRST_HEAD);
        register(&mut s, "acme/site", 14, CLOUD_HEAD);
        let old = hosted_pr_id("github.com", "acme/app", 65);
        let site = hosted_pr_id("github.com", "acme/site", 14);
        observe(&mut s, &old.0, FIRST_HEAD, PrState::Closed);
        let mut command = result("misdirected", vec![pr(&site.0, CLOUD_HEAD)], true);
        if let Command::PresentResult { body, card, .. } = &mut command {
            *body = format!("Finished: {citation}.");
            card.content.as_mut().unwrap().body = body.clone();
        }
        let error = exec(&mut s, node(), command).unwrap_err();
        assert_eq!(error.code, "cited_pr_not_target");
        assert_eq!(error.contract.unwrap().rule, ContractRule::CitedPrNotTarget);
        assert!(!s.interactions.contains_key(&"misdirected".into()));
    }
}

#[test]
fn citations_in_each_displayed_delivery_field_are_checked_and_targets_in_other_repositories_match() {
    for field in ["body", "title", "need", "why", "content", "summary", "text"] {
        let mut s = at_work();
        register(&mut s, "acme/site", 14, CLOUD_HEAD);
        let mut command = result("citation", vec![DeliveryTarget::Text { text: "Report".into() }], true);
        if let Command::PresentResult { body, card, targets, .. } = &mut command {
            let citation = "https://github.com/acme/site/pull/14/files#diff-x".to_owned();
            match field {
                "body" => *body = citation,
                "title" => card.title = Some(citation),
                "need" => card.need = citation,
                "why" => card.why = citation,
                "content" => card.content.as_mut().unwrap().body = citation,
                "summary" => card.content.as_mut().unwrap().summary = Some(citation),
                "text" => targets[0] = DeliveryTarget::Text { text: citation },
                _ => unreachable!(),
            }
        }
        assert_eq!(refusal(&mut s, command), "cited_pr_not_target", "{field}");
    }
}

#[test]
fn the_release_still_owes_its_declared_publication_after_its_pr_card_retires() {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    must(&mut s, node(), Command::DeclarePostMergeWork { task_id: TASK.into(), reason: Some("Publish and verify the release".into()), waits_for: vec![] });
    must(&mut s, node(), result("release", vec![pr(PUBLIC, FIRST_HEAD)], true));
    merged(&mut s, PUBLIC, FIRST_HEAD);
    assert!(cards(&s).is_empty());
    let task = &s.tasks[&TASK.into()];
    assert!(!task.over(), "retiring the review does not finish publication");
    assert!(task.wrap_up.post_merge_work.is_some());
    assert!(s.lifecycle_of(&TASK.into()).unwrap().post_merge().is_some());
    must(&mut s, node(), Command::DeclarePostMergeWork { task_id: TASK.into(), reason: None, waits_for: vec![] });
    assert!(s.tasks[&TASK.into()].wrap_up.post_merge_work.is_none());
}

const TURN: &str = "turn-1";

/// The node's turn ends after what it did in it.
fn turn_ends(s: &mut DomainState) {
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: TASK.into(), generation: 1, turn: TURN.into(), failure: None, failed: None });
}

fn register(s: &mut DomainState, repository: &str, number: u64, head: &str) {
    must(s, node(), Command::RegisterPr {
        task_id: TASK.into(), pr_id: hosted_pr_id("github.com", repository, number), host: "github.com".into(), repository: repository.into(),
        number, head: head.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
}

fn opened(s: &mut DomainState) {
    must(s, Actor::System, Command::NotePrOpened {
        task_id: TASK.into(), host: "github.com".into(), repository: "acme/storefront".into(), number: 6,
    });
}

fn read(s: &mut DomainState) {
    must(s, Actor::System, Command::RecordOpenedPr {
        task_id: TASK.into(), host: "github.com".into(), repository: "acme/storefront".into(), number: 6,
        head: CLOUD_HEAD.into(), base: "main".into(), state: PrState::Open, merged_head: None, allowed_methods: vec![MergeMethod::Squash],
    });
}

fn merged(s: &mut DomainState, pr: &str, head: &str) {
    observe(s, pr, head, PrState::Merged);
}

fn observed(pr: &str, head: &str, state: PrState) -> Command {
    Command::ObservePrState {
        pr_id: pr.into(), head: head.into(), base: "main".into(), state, merged_head: (state == PrState::Merged).then(|| head.into()),
    }
}

fn observe(s: &mut DomainState, pr: &str, head: &str, state: PrState) {
    must(s, Actor::System, observed(pr, head, state));
}

/// Waiting for You, by the id each card is opened with.
fn cards(s: &DomainState) -> Vec<String> {
    crate::ReadIndex::build(s).cards().iter().map(|item| {
        let InboxSubject::Interaction { interaction_id, pr_id } = &item.subject;
        pr_id.as_ref().map_or_else(|| interaction_id.0.clone(), |pr| pr.0.clone())
    }).collect()
}

fn result(id: &str, targets: Vec<DeliveryTarget>, is_final: bool) -> Command {
    Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: id.into(), body: "Done".into(),
        targets, is_final, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], is_final),
    }
}

fn text() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text { text: "What you need to do: merge storefront PR #6".into() }]
}

fn pr(id: &str, head: &str) -> DeliveryTarget {
    DeliveryTarget::PullRequest { pr_id: id.into(), head: head.into() }
}

fn refusal(s: &mut DomainState, command: Command) -> String {
    exec(s, node(), command).unwrap_err().code
}

#[test]
fn a_task_closes_once_every_pr_is_merged_or_closed_and_one_merged() {
    for (first, second) in [(PrState::Merged, PrState::Closed), (PrState::Closed, PrState::Merged)] {
        let mut s = two_open_prs();
        observe(&mut s, PUBLIC, FIRST_HEAD, first);
        assert!(!s.finished(&s.tasks[&TASK.into()]), "a PR is still open");
        assert_eq!(cards(&s), [CLOUD]);
        let outcome = exec(&mut s, Actor::System, observed(CLOUD, CLOUD_HEAD, second)).unwrap();
        assert!(s.finished(&s.tasks[&TASK.into()]), "{first:?} then {second:?}: nothing is left to merge");
        assert!(cards(&s).is_empty());
        assert!(outcome.effects.iter().any(|effect| matches!(effect, DomainEffect::NotifyAgent { message, .. } if message.contains("clean"))), "the node cleans up: {:?}", outcome.effects);
    }
    let mut s = two_open_prs();
    let outcome = exec(&mut s, Actor::System, observed(PUBLIC, FIRST_HEAD, PrState::Closed)).unwrap();
    assert!(outcome.effects.iter().any(|effect| matches!(effect, DomainEffect::NotifyAgent { message, .. }
        if *message == closed_notice(17, 1))), "{:?}", outcome.effects);
    observe(&mut s, CLOUD, CLOUD_HEAD, PrState::Closed);
    assert!(s.tasks[&TASK.into()].cancelled(), "with nothing merged and nothing open the task is cancelled");
}

#[test]
fn a_task_whose_three_prs_were_all_closed_on_github_is_cancelled_with_no_card_left() {
    let mut s = two_open_prs();
    let docs = "github.com/bitomule/docs#3";
    let docs_head = "3333333333333333333333333333333333333333";
    register(&mut s, "bitomule/docs", 3, docs_head);
    must(&mut s, node(), result("final-3", vec![pr(PUBLIC, "")], true));
    observe(&mut s, PUBLIC, FIRST_HEAD, PrState::Closed);
    observe(&mut s, docs, docs_head, PrState::Closed);
    assert!(!s.tasks[&TASK.into()].over(), "one PR is still open");
    assert_eq!(cards(&s), [CLOUD]);
    observe(&mut s, CLOUD, CLOUD_HEAD, PrState::Closed);
    assert!(s.tasks[&TASK.into()].cancelled());
    assert!(cards(&s).is_empty(), "no card is left without a way out");
}

#[test]
fn a_pr_the_node_closed_before_presenting_anything_leaves_the_task_running() {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    observe(&mut s, PUBLIC, FIRST_HEAD, PrState::Closed);
    assert!(!s.tasks[&TASK.into()].over(), "the node can still open another one");
}

#[test]
fn a_pr_opened_after_the_delivery_gets_its_own_card_once_presented_again() {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    must(&mut s, node(), result("final", vec![pr(PUBLIC, "")], true));
    assert_eq!(cards(&s), ["final"], "one PR, one card");
    opened(&mut s);
    read(&mut s);
    assert!(!s.finished(&s.tasks[&TASK.into()]));
    must(&mut s, node(), result("final-2", vec![pr(PUBLIC, "")], true));
    assert_eq!(cards(&s), [PUBLIC, CLOUD], "presented again, the new PR has its own card");
    merged(&mut s, CLOUD, CLOUD_HEAD);
    assert_eq!(cards(&s), [PUBLIC]);
    merged(&mut s, PUBLIC, FIRST_HEAD);
    assert!(s.finished(&s.tasks[&TASK.into()]));
}

#[test]
fn a_pr_with_new_commits_takes_every_card_back_until_the_node_presents_again() {
    let mut s = two_open_prs();
    observe(&mut s, CLOUD, "7777777777777777777777777777777777777777", PrState::Open);
    assert!(cards(&s).is_empty(), "the delivery no longer shows what the PR is");
    must(&mut s, node(), result("final-2", vec![pr(PUBLIC, "")], true));
    assert_eq!(cards(&s), [PUBLIC, CLOUD]);
}

#[test]
fn an_external_merge_of_another_head_does_not_settle_the_old_delivery() {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    must(&mut s, node(), result("final", vec![pr(PUBLIC, FIRST_HEAD)], true));
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets: vec![pr(PUBLIC, FIRST_HEAD)] });
    let changed = "2222222222222222222222222222222222222222";
    observe(&mut s, PUBLIC, changed, PrState::Merged);
    assert!(!s.finished(&s.tasks[&TaskId::from(TASK)]), "a different merged head does not answer the delivery the user reviewed");
    assert!(s.interactions[&InteractionId::from("final")].withdrawn);
    assert!(cards(&s).is_empty(), "the old card cannot authorize the merged code");
    assert!(s.merge_authorizations.values().all(|authorization| !authorization.valid));
}

#[test]
fn a_merged_pr_card_stays_settled_when_a_late_push_is_observed() {
    let mut s = two_open_prs();
    merged(&mut s, PUBLIC, FIRST_HEAD);
    let before = cards(&s);
    let late = exec(&mut s, Actor::System, observed(PUBLIC, "2222222222222222222222222222222222222222", PrState::Open));
    assert_eq!(late.unwrap_err().code, "invalid_input");
    assert_eq!(cards(&s), before);
    assert_eq!(cards(&s), [CLOUD]);
}

#[test]
fn an_external_merge_resolves_the_conflicts_the_merger_reported() {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    must(&mut s, node(), result("final", vec![pr(PUBLIC, FIRST_HEAD)], true));
    must(&mut s, Actor::System, Command::SubmitFeedback {
        presentation_id: "final".into(), targets: vec![pr(PUBLIC, FIRST_HEAD)], body: "The base could not be merged".into(),
        attachments: vec![], instruction: Instruction::Conflicts,
        merge_failure: Some(MergeFailure { number: 17, reason: "conflicts".into() }),
    });
    merged(&mut s, PUBLIC, FIRST_HEAD);
    assert!(s.finished(&s.tasks[&TaskId::from(TASK)]), "resolved merge trouble is not an unanswered user reply");
    assert!(cards(&s).is_empty());
}

#[test]
fn a_split_card_says_which_pr_it_is_and_an_old_card_reads_as_before() {
    let old: InboxSubject = serde_json::from_value(serde_json::json!({"kind": "interaction", "interaction_id": "final"})).unwrap();
    assert_eq!(old, InboxSubject::Interaction { interaction_id: "final".into(), pr_id: None });
    assert_eq!(serde_json::to_value(&old).unwrap(), serde_json::json!({"kind": "interaction", "interaction_id": "final"}));
    let split = InboxSubject::Interaction { interaction_id: "final".into(), pr_id: Some(PUBLIC.into()) };
    assert_eq!(serde_json::to_value(&split).unwrap()["pr_id"], PUBLIC);
}

/// A task with two open PRs in two repositories, both in its final delivery.
pub(crate) fn two_open_prs() -> DomainState {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    register(&mut s, "acme/storefront", 6, CLOUD_HEAD);
    must(&mut s, node(), result("final", vec![pr(PUBLIC, "")], true));
    turn_ends(&mut s);
    s
}

#[test]
fn a_reply_obligation_is_durable_and_scoped_to_the_presented_pr_head() {
    let mut s = two_open_prs();
    let target = FeedbackTarget::PullRequest { pr_id: PUBLIC.into(), head: FIRST_HEAD.into() };
    let feedback_revision = s.revision + 1;
    let p = s.interactions.get_mut(&"final".into()).unwrap();
    let InteractionPayload::Delivery { feedback, .. } = &mut p.payload else { panic!("delivery") };
    feedback.push(Feedback { target: target.clone(), reply_window_start: None, attachments: vec![], user_id: "u".into(), body: "Explain the change".into(),
        at_revision: feedback_revision, instruction: Instruction::Reply, merge_failure: None });
    p.revision = feedback_revision;
    s.revision = feedback_revision;
    must(&mut s, Actor::System, Command::TellNode {
        task_id: TASK.into(), text: "Explain the change".into(), from_user: false, answers: None,
    });
    let input = s.lifecycle_of(&TASK.into()).unwrap().pending_inputs()[0].as_str().to_owned();
    must(&mut s, Actor::System, Command::RecordRuntimeFacts {
        task_id: TASK.into(), generation: 1, facts: vec![RuntimeObservation::TurnStarted { turn: "reply".into(), input }],
    });
    must(&mut s, Actor::System, Command::RecordTurnBoundary {
        task_id: TASK.into(), generation: 1, turn: "reply".into(), failure: None, failed: None,
    });
    let p = &s.interactions[&"final".into()];
    assert_eq!(s.handed_back(p), None, "the temporary visibility predicate no longer holds the reply");
    assert!(s.pending_reply(p, &target), "a resting node has not answered the user");
    assert!(s.pending_reply(p, &FeedbackTarget::Delivery));
    assert!(!s.pending_reply(p, &FeedbackTarget::PullRequest { pr_id: CLOUD.into(), head: CLOUD_HEAD.into() }));
    assert!(!s.pending_reply(p, &FeedbackTarget::PullRequest { pr_id: PUBLIC.into(), head: CLOUD_HEAD.into() }));
}

#[test]
fn a_delivery_of_a_pr_carries_every_open_pr_and_the_task_waits_for_each() {
    let mut s = two_open_prs();
    let InteractionPayload::Delivery { targets, .. } = &s.interactions[&"final".into()].payload else { panic!("a delivery") };
    assert_eq!(*targets, vec![pr(PUBLIC, FIRST_HEAD), pr(CLOUD, CLOUD_HEAD)]);

    must(&mut s, user(), Command::AuthorizeMerge {
        authorization_id: "first".into(), presentation_id: "final".into(), pr_id: PUBLIC.into(), expected_head: FIRST_HEAD.into(), method: MergeMethod::Squash, despite_pending_message: false,
    });
    merged(&mut s, PUBLIC, FIRST_HEAD);
    assert!(!s.finished(&s.tasks[&TASK.into()]), "one PR is still open");
    assert_eq!(s.unmerged_targets(&TASK.into()), 1);

    must(&mut s, user(), Command::AuthorizeMerge {
        authorization_id: "second".into(), presentation_id: "final".into(), pr_id: CLOUD.into(), expected_head: CLOUD_HEAD.into(), method: MergeMethod::Squash, despite_pending_message: false,
    });
    merged(&mut s, CLOUD, CLOUD_HEAD);
    assert!(s.finished(&s.tasks[&TASK.into()]));
}

#[test]
fn a_pr_registered_after_the_delivery_holds_the_task_open_and_tells_the_node() {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    must(&mut s, node(), result("final", vec![pr(PUBLIC, "")], true));
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets: vec![pr(PUBLIC, FIRST_HEAD)] });
    opened(&mut s);
    let outcome = exec(&mut s, Actor::System, Command::RecordOpenedPr {
        task_id: TASK.into(), host: "github.com".into(), repository: "acme/storefront".into(), number: 6,
        head: CLOUD_HEAD.into(), base: "main".into(), state: PrState::Open, merged_head: None, allowed_methods: vec![MergeMethod::Squash],
    }).unwrap();
    assert!(outcome.effects.iter().any(|effect| matches!(effect, DomainEffect::NotifyAgent { message, .. } if message.contains("storefront#6"))), "{:?}", outcome.effects);
    merged(&mut s, PUBLIC, FIRST_HEAD);
    let task = &s.tasks[&TASK.into()];
    assert!(!s.finished(task), "the other PR is still open");
    assert_eq!(s.undelivered_pr(task).map(|pr| pr.number), Some(6));
    assert_eq!(s.empty_turn(&TASK.into()), EmptyTurn::UnpresentedPr { number: 6 });
}

#[test]
fn a_noted_pr_is_read_once_and_a_pr_of_another_task_stays_there() {
    let mut s = at_work();
    opened(&mut s);
    opened(&mut s);
    assert_eq!(s.tasks[&TASK.into()].opened_prs.len(), 1);
    read(&mut s);
    read(&mut s);
    assert_eq!(s.tasks[&TASK.into()].pr_ids().count(), 1);
    opened(&mut s);
    assert!(s.tasks[&TASK.into()].opened_prs.is_empty(), "a PR the task already owns needs no reading");

    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: "b".into(), name: "Other".into(), at: 0 });
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
        task_id: "other".into(), request_id: "r".into(), title: "Other".into(), agent_id: Some("b".into()), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    must(&mut s, Actor::Agent { agent_id: "b".into() }, Command::StartTask { task_id: "other".into() });
    let taken = exec(&mut s, Actor::Agent { agent_id: "b".into() }, Command::RegisterPr {
        task_id: "other".into(), pr_id: CLOUD.into(), host: "github.com".into(), repository: "acme/storefront".into(),
        number: 6, head: CLOUD_HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    }).unwrap_err();
    assert_eq!(taken.code, "pr_owned_by_other_task");
}

#[test]
fn the_node_can_register_a_pr_alondra_only_noted_and_a_closed_pr_is_not_corrected() {
    let mut s = at_work();
    opened(&mut s);
    register(&mut s, "acme/storefront", 6, CLOUD_HEAD);
    let task = &s.tasks[&TASK.into()];
    assert!(task.opened_prs.is_empty(), "register_pr settles the note");
    assert_eq!(task.pr_ids().count(), 1);
    merged(&mut s, CLOUD, CLOUD_HEAD);
    let moved = Command::RegisterPr {
        task_id: TASK.into(), pr_id: CLOUD.into(), host: "github.com".into(), repository: "acme/storefront".into(),
        number: 6, head: FIRST_HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    };
    assert_eq!(refusal(&mut s, moved), "pr_not_open");
}

#[test]
fn nothing_to_review_and_a_plan_cannot_stand_in_for_an_open_pr() {
    let mut s = at_work();
    register(&mut s, "acme/storefront", 6, CLOUD_HEAD);
    let Command::PresentResult { checklist, attachments, task_id, interaction_id, body, targets, is_final, card, .. } = result("final", text(), true) else { unreachable!() };
    let silent = Command::PresentResult { checklist, attachments, task_id, interaction_id, body, targets, is_final, nothing_to_review: true, card };
    assert_eq!(refusal(&mut s, silent), "pr_missing");
}

