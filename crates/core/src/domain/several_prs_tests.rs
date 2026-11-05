//! A task owns every PR its node opens, in any repository, and its delivery is those PRs: nothing
//! else can reach the user while one is open. The incident that asked for this reads, step by
//! step, in `the_second_pr_of_the_mobile_menu_fix_reaches_the_user_with_its_merge`.
use super::*;
use crate::views::{Client, Lang, LiveSnapshot, RenderContext, Transport};
use std::collections::BTreeSet;

const AGENT: &str = "a";
const TASK: &str = "t";
const PUBLIC: &str = "github.com/bitomule/alondra#17";
const CLOUD: &str = "github.com/bitomule/alondra-cloud#6";
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
        task_id: TASK.into(), request_id: "r".into(), title: "Fix the phone menu".into(), agent_id: Some(AGENT.into()), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    must(&mut s, node(), Command::StartTask { task_id: TASK.into() });
    must(&mut s, Actor::System, Command::MoveNode { agent_id: AGENT.into(), event: NodeEvent::TurnStarted, at: 1 });
    s
}

fn register(s: &mut DomainState, repository: &str, number: u64, head: &str) {
    must(s, node(), Command::RegisterPr {
        task_id: TASK.into(), pr_id: hosted_pr_id("github.com", repository, number), host: "github.com".into(), repository: repository.into(),
        number, head: head.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
}

fn opened(s: &mut DomainState) {
    must(s, Actor::System, Command::NotePrOpened {
        task_id: TASK.into(), host: "github.com".into(), repository: "bitomule/alondra-cloud".into(), number: 6,
    });
}

fn read(s: &mut DomainState) {
    must(s, Actor::System, Command::RecordOpenedPr {
        task_id: TASK.into(), host: "github.com".into(), repository: "bitomule/alondra-cloud".into(), number: 6,
        head: CLOUD_HEAD.into(), base: "main".into(), state: PrState::Open, merged_head: None, allowed_methods: vec![MergeMethod::Squash],
    });
}

fn merged(s: &mut DomainState, pr: &str, head: &str) {
    must(s, Actor::System, Command::ObservePrState {
        pr_id: pr.into(), head: head.into(), base: "main".into(), state: PrState::Merged, merged_head: Some(head.into()),
    });
}

fn result(id: &str, targets: Vec<DeliveryTarget>, is_final: bool) -> Command {
    Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: id.into(), body: "Done".into(),
        targets, is_final, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], is_final),
    }
}

fn text() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text { text: "What you need to do: merge alondra-cloud PR #6".into() }]
}

fn pr(id: &str, head: &str) -> DeliveryTarget {
    DeliveryTarget::PullRequest { pr_id: id.into(), head: head.into() }
}

fn refusal(s: &mut DomainState, command: Command) -> String {
    exec(s, node(), command).unwrap_err().code
}

fn card_actions(s: &DomainState, card: &str) -> Vec<(String, String)> {
    merges_in(s, &format!("card:{card}"))
}

fn merges_in(s: &DomainState, key: &str) -> Vec<(String, String)> {
    let index = crate::domain::ReadIndex::build(s);
    let view = crate::views::build(key, RenderContext {
        domain: s, index: &index, live: &LiveSnapshot::default(), client: Client::Mac, lang: Lang::Es,
        transport: Transport::Loopback, cursor: "", rendered_at: "",
    }).unwrap();
    let json = serde_json::to_value(view).unwrap();
    let mut found = vec![];
    collect_actions(&json, &mut found);
    found
}

#[test]
fn a_pull_request_screen_offers_only_the_merge_of_its_own_pr() {
    let s = two_open_prs();
    for (pr, name) in [(PUBLIC, "alondra#17"), (CLOUD, "alondra-cloud#6")] {
        let merges = merges_in(&s, &format!("pull_request:{pr}"));
        assert_eq!(merges.len(), 1, "{pr}: {merges:?}");
        assert!(merges[0].1.ends_with(name), "{merges:?}");
    }
}

#[test]
fn only_the_last_open_pr_takes_the_card_with_its_merge() {
    let mut s = two_open_prs();
    let merge = |pr: &str, head: &str| crate::views::action_id::ActionId::new(crate::views::action_id::Verb::Merge, pr, 1, Some(format!("{head}-final"))).unwrap();
    assert!(!crate::views::resolves_card(&s, &merge(CLOUD, "66666666")), "the other PR still waits on this card");
    merged(&mut s, PUBLIC, FIRST_HEAD);
    assert!(crate::views::resolves_card(&s, &merge(CLOUD, "66666666")));
}

fn collect_actions(value: &serde_json::Value, found: &mut Vec<(String, String)>) {
    match value {
        serde_json::Value::Object(map) => {
            if let (Some(id), Some(label)) = (map.get("id").and_then(|id| id.as_str()), map.get("label").and_then(|label| label.as_str()))
                && id.starts_with("merge:")
            {
                found.push((id.to_owned(), label.to_owned()));
            }
            map.values().for_each(|value| collect_actions(value, found));
        }
        serde_json::Value::Array(list) => list.iter().for_each(|value| collect_actions(value, found)),
        _ => {}
    }
}

#[test]
fn the_second_pr_of_the_mobile_menu_fix_reaches_the_user_with_its_merge() {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    must(&mut s, node(), Command::DeclarePostMergeWork { task_id: TASK.into(), reason: Some("check the deployed site".into()), waits_for: vec![] });
    must(&mut s, node(), result("result-1", vec![pr(PUBLIC, FIRST_HEAD)], true));
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "result-1".into(), targets: vec![pr(PUBLIC, FIRST_HEAD)] });
    merged(&mut s, PUBLIC, FIRST_HEAD);

    opened(&mut s);
    assert_eq!(refusal(&mut s, result("final-1", text(), true)), "pr_being_read", "a PR the node opened holds any result back until it is read");

    read(&mut s);
    let task = &s.tasks[&TASK.into()];
    assert_eq!(task.pr_ids().map(|id| id.0.as_str()).collect::<Vec<_>>(), [PUBLIC, CLOUD], "the task owns both PRs");
    assert!(task.opened_prs.is_empty());

    assert_eq!(refusal(&mut s, result("final-1", text(), true)), "pr_missing", "the text card that asked the user to merge by hand");
    assert_eq!(refusal(&mut s, result("final-1", text(), false)), "pr_needs_final", "a notice cannot carry the open PR either");
    assert!(!s.interactions.contains_key(&"final-1".into()));

    must(&mut s, node(), result("final-2", vec![pr(CLOUD, "")], true));
    let actions = card_actions(&s, "final-2");
    assert_eq!(actions.len(), 1, "one merge for the one open PR: {actions:?}");
    assert_eq!(actions[0].1, "Squash and merge");

    must(&mut s, user(), Command::AuthorizeMerge {
        authorization_id: "merge".into(), presentation_id: "final-2".into(), pr_id: CLOUD.into(), expected_head: CLOUD_HEAD.into(), method: MergeMethod::Squash,
    });
    merged(&mut s, CLOUD, CLOUD_HEAD);
    assert!(s.finished(&s.tasks[&TASK.into()]), "merging it settles the task");
}

/// A task with two open PRs in two repositories, both in its final delivery.
pub(crate) fn two_open_prs() -> DomainState {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    register(&mut s, "bitomule/alondra-cloud", 6, CLOUD_HEAD);
    must(&mut s, node(), result("final", vec![pr(PUBLIC, "")], true));
    s
}

#[test]
fn a_delivery_of_a_pr_carries_every_open_pr_and_each_has_its_own_merge() {
    let mut s = two_open_prs();
    let InteractionPayload::Delivery { targets, .. } = &s.interactions[&"final".into()].payload else { panic!("a delivery") };
    assert_eq!(*targets, vec![pr(PUBLIC, FIRST_HEAD), pr(CLOUD, CLOUD_HEAD)]);

    let mut labels: Vec<String> = card_actions(&s, "final").into_iter().map(|(_, label)| label).collect();
    labels.sort();
    assert_eq!(labels, ["Squash and merge · alondra#17", "Squash and merge · alondra-cloud#6"]);

    must(&mut s, user(), Command::AuthorizeMerge {
        authorization_id: "first".into(), presentation_id: "final".into(), pr_id: PUBLIC.into(), expected_head: FIRST_HEAD.into(), method: MergeMethod::Squash,
    });
    assert_eq!(card_actions(&s, "final").len(), 2, "the card stays while the first merge runs");
    merged(&mut s, PUBLIC, FIRST_HEAD);
    assert!(!s.finished(&s.tasks[&TASK.into()]), "one PR is still open");
    assert_eq!(s.unmerged_targets(&TASK.into()), 1);
    assert_eq!(card_actions(&s, "final").len(), 1, "only the PR still open offers its merge");

    must(&mut s, user(), Command::AuthorizeMerge {
        authorization_id: "second".into(), presentation_id: "final".into(), pr_id: CLOUD.into(), expected_head: CLOUD_HEAD.into(), method: MergeMethod::Squash,
    });
    merged(&mut s, CLOUD, CLOUD_HEAD);
    assert!(s.finished(&s.tasks[&TASK.into()]));
}

#[test]
fn accepting_merges_every_pr_and_a_task_with_one_pr_keeps_its_card() {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    must(&mut s, node(), result("final", vec![pr(PUBLIC, "")], true));
    let actions = card_actions(&s, "final");
    assert_eq!(actions.iter().map(|(_, label)| label.as_str()).collect::<Vec<_>>(), ["Squash and merge"]);
    assert!(actions[0].0.starts_with("merge:final@"), "{actions:?}");

    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    register(&mut s, "bitomule/alondra-cloud", 6, CLOUD_HEAD);
    let both = vec![pr(PUBLIC, FIRST_HEAD), pr(CLOUD, CLOUD_HEAD)];
    must(&mut s, node(), result("final", both.clone(), true));
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets: both });
    assert_eq!(s.merge_authorizations.values().filter(|a| a.valid).count(), 2);
}

#[test]
fn a_pr_registered_after_the_delivery_holds_the_task_open_and_tells_the_node() {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    must(&mut s, node(), result("final", vec![pr(PUBLIC, "")], true));
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets: vec![pr(PUBLIC, FIRST_HEAD)] });
    opened(&mut s);
    let outcome = exec(&mut s, Actor::System, Command::RecordOpenedPr {
        task_id: TASK.into(), host: "github.com".into(), repository: "bitomule/alondra-cloud".into(), number: 6,
        head: CLOUD_HEAD.into(), base: "main".into(), state: PrState::Open, merged_head: None, allowed_methods: vec![MergeMethod::Squash],
    }).unwrap();
    assert!(outcome.effects.iter().any(|effect| matches!(effect, DomainEffect::NotifyAgent { message, .. } if message.contains("alondra-cloud#6"))), "{:?}", outcome.effects);
    merged(&mut s, PUBLIC, FIRST_HEAD);
    let task = &s.tasks[&TASK.into()];
    assert!(!s.finished(task), "the other PR is still open");
    assert_eq!(s.undelivered_pr(task).map(|pr| pr.number), Some(6));
    assert_eq!(s.empty_turn(&TASK.into()), EmptyTurn::UnpresentedPr { number: 6 });
}

#[test]
fn a_pr_merged_by_hand_on_github_counts_as_merged_wherever_the_task_is() {
    let mut delivered = two_open_prs();
    merged(&mut delivered, CLOUD, CLOUD_HEAD);
    assert_eq!(card_actions(&delivered, "final").len(), 1, "only the PR still open offers its merge");
    merged(&mut delivered, PUBLIC, FIRST_HEAD);
    assert!(delivered.finished(&delivered.tasks[&TASK.into()]), "nobody pressed anything in Alondra");

    let mut undelivered = at_work();
    register(&mut undelivered, "bitomule/alondra-cloud", 6, CLOUD_HEAD);
    merged(&mut undelivered, CLOUD, CLOUD_HEAD);
    must(&mut undelivered, node(), result("final", text(), true));

    let mut unread = at_work();
    opened(&mut unread);
    must(&mut unread, Actor::System, Command::RecordOpenedPr {
        task_id: TASK.into(), host: "github.com".into(), repository: "bitomule/alondra-cloud".into(), number: 6,
        head: CLOUD_HEAD.into(), base: "main".into(), state: PrState::Merged, merged_head: Some(CLOUD_HEAD.into()), allowed_methods: vec![MergeMethod::Squash],
    });
    must(&mut unread, node(), result("final", text(), true));
    assert_eq!(unread.merged_pr(&unread.tasks[&TASK.into()]).map(|pr| pr.number), Some(6));

    let mut after_delivery = at_work();
    register(&mut after_delivery, "bitomule/alondra", 17, FIRST_HEAD);
    must(&mut after_delivery, node(), result("result-1", vec![pr(PUBLIC, "")], true));
    merged(&mut after_delivery, PUBLIC, FIRST_HEAD);
    opened(&mut after_delivery);
    read(&mut after_delivery);
    assert!(!after_delivery.finished(&after_delivery.tasks[&TASK.into()]), "the PR is still open");
    merged(&mut after_delivery, CLOUD, CLOUD_HEAD);
    assert!(after_delivery.finished(&after_delivery.tasks[&TASK.into()]), "merged by hand, it never needed a delivery");
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
        task_id: "other".into(), pr_id: CLOUD.into(), host: "github.com".into(), repository: "bitomule/alondra-cloud".into(),
        number: 6, head: CLOUD_HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    }).unwrap_err();
    assert_eq!(taken.code, "pr_owned_by_other_task");
}

#[test]
fn the_node_can_register_a_pr_alondra_only_noted_and_a_closed_pr_is_not_corrected() {
    let mut s = at_work();
    opened(&mut s);
    register(&mut s, "bitomule/alondra-cloud", 6, CLOUD_HEAD);
    let task = &s.tasks[&TASK.into()];
    assert!(task.opened_prs.is_empty(), "register_pr settles the note");
    assert_eq!(task.pr_ids().count(), 1);
    merged(&mut s, CLOUD, CLOUD_HEAD);
    let moved = Command::RegisterPr {
        task_id: TASK.into(), pr_id: CLOUD.into(), host: "github.com".into(), repository: "bitomule/alondra-cloud".into(),
        number: 6, head: FIRST_HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    };
    assert_eq!(refusal(&mut s, moved), "pr_not_open");
}

#[test]
fn nothing_to_review_and_a_plan_cannot_stand_in_for_an_open_pr() {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra-cloud", 6, CLOUD_HEAD);
    let Command::PresentResult { checklist, attachments, task_id, interaction_id, body, targets, is_final, card, .. } = result("final", text(), true) else { unreachable!() };
    let silent = Command::PresentResult { checklist, attachments, task_id, interaction_id, body, targets, is_final, nothing_to_review: true, card };
    assert_eq!(refusal(&mut s, silent), "pr_missing");
}
