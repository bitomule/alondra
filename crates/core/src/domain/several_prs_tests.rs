//! A task owns every PR its node opens, in any repository, and its delivery is those PRs: nothing
//! else can reach the user while one is open. The incident that asked for this reads, step by
//! step, in `the_second_pr_of_the_mobile_menu_fix_reaches_the_user_with_its_merge`. Each PR of the
//! delivery is its own card, and the task closes once none is open and one merged: a delivery of
//! nine PRs on one card is `a_delivery_of_several_prs_is_one_card_per_pr_each_with_only_its_merge`.
use super::*;
use crate::views::{Client, Lang, LiveSnapshot, RenderContext, Transport};
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
    must(&mut s, Actor::System, Command::AdmitExecution { provider: crate::accounts::Provider::Claude, task_id: TASK.into(), agent_id: AGENT.into(),
        conversation: "conversation".into(), instance: "instance".into(), host: "sdk-host".into(), model: None, account: None, leaves_limit: None });
    let LifecycleRecord::Planned { input } = s.lifecycle_journal[&TaskId::from(TASK)][0].clone() else { panic!("planned first") };
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: TASK.into(), generation: 1, facts: vec![
        RuntimeObservation::ResourceAvailable,
        RuntimeObservation::TurnStarted { turn: TURN.into(), input },
    ] });
    s
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
    crate::domain::ReadIndex::build(s).cards().iter().map(|item| {
        let InboxSubject::Interaction { interaction_id, pr_id } = &item.subject;
        pr_id.as_ref().map_or_else(|| interaction_id.0.clone(), |pr| pr.0.clone())
    }).collect()
}

fn find_section<'a>(view: &'a serde_json::Value, id: &str) -> Option<&'a serde_json::Value> {
    view["sections"].as_array()?.iter().find(|section| section["id"] == id)
}

fn settled_notice(s: &DomainState, key: &str) -> Option<String> {
    let view = view_json(s, key);
    (view["view"] == crate::views::SETTLED_VIEW).then(|| view["empty"]["text"].as_str().unwrap().to_owned())
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

fn card_actions(s: &DomainState, card: &str) -> Vec<(String, String)> {
    merges_in(s, &format!("card:{card}"))
}

fn view_json(s: &DomainState, key: &str) -> serde_json::Value {
    let index = crate::domain::ReadIndex::build(s);
    let view = crate::views::build(key, RenderContext {
        domain: s, index: &index, live: &LiveSnapshot::default(), client: Client::Mac, lang: Lang::Es,
        transport: Transport::Loopback, cursor: "", rendered_at: "",
    }).unwrap();
    serde_json::to_value(view).unwrap()
}

fn merges_in(s: &DomainState, key: &str) -> Vec<(String, String)> {
    let mut found = vec![];
    collect_actions(&view_json(s, key), &mut found);
    found
}

#[test]
fn a_pull_request_screen_offers_only_the_merge_of_its_own_pr() {
    let s = two_open_prs();
    for pr in [PUBLIC, CLOUD] {
        let merges = merges_in(&s, &format!("pull_request:{pr}"));
        assert_eq!(merges.len(), 1, "{pr}: {merges:?}");
        assert!(merges[0].0.starts_with(&format!("merge:{pr}@")), "{merges:?}");
    }
}

#[test]
fn merging_a_pr_takes_its_own_card_away_and_the_others_stay() {
    let mut s = two_open_prs();
    let merge = |pr: &str, head: &str| crate::views::action_id::ActionId::new(crate::views::action_id::Verb::Merge, pr, 1, Some(format!("{head}-final"))).unwrap();
    assert!(crate::views::resolves_card(&s, &merge(PUBLIC, "11111111")), "the merge settles the card it is on");
    must(&mut s, user(), Command::AuthorizeMerge {
        authorization_id: "first".into(), presentation_id: "final".into(), pr_id: PUBLIC.into(), expected_head: FIRST_HEAD.into(), method: MergeMethod::Squash,
    });
    assert_eq!(cards(&s), [PUBLIC, CLOUD], "a card stays until GitHub says its PR merged");
    merged(&mut s, PUBLIC, FIRST_HEAD);
    assert_eq!(cards(&s), [CLOUD]);
    assert_eq!(settled_notice(&s, &format!("card:{PUBLIC}")), Some("Ya tiene merge".into()), "a phone still showing it closes it");
    assert_eq!(card_actions(&s, CLOUD).len(), 1);
    assert_eq!(card_actions(&s, "final").len(), 1, "an old link to the delivery opens the card of the PR still open");
}

#[test]
fn a_delivery_of_several_prs_is_one_card_per_pr_each_with_only_its_merge() {
    let s = two_open_prs();
    assert_eq!(cards(&s), [PUBLIC, CLOUD]);
    let index = crate::domain::ReadIndex::build(&s);
    assert_eq!(crate::views::inbox_badge(&index), 2, "the badge counts the cards the user sees");
    assert_eq!(crate::views::waiting_cards(&index), ["final"], "one notice for the delivery, not one per PR");
    for item in index.cards() {
        assert!(!item.available_actions.iter().any(|action| action.kind == "accept_delivery"), "accepting would merge every PR from one card");
        assert_eq!(item.available_actions.iter().filter(|action| action.kind == "authorize_merge").count(), 1);
    }
    for (pr, head, name) in [(PUBLIC, "11111111", "alondra#17"), (CLOUD, "66666666", "storefront#6")] {
        let merges = card_actions(&s, pr);
        assert_eq!(merges.len(), 1, "{pr}: {merges:?}");
        assert!(merges[0].0.starts_with(&format!("merge:{pr}@")) && merges[0].0.contains(&format!("~{head}-final")), "{merges:?}");
        assert_eq!(merges[0].1, "Squash and merge");
        let view = view_json(&s, &format!("card:{pr}"));
        assert!(view["title"].as_str().unwrap().ends_with(name), "each card names its PR: {}", view["title"]);
        let diff = find_section(&view, "diff").expect("each card shows its PR's diff");
        assert!(diff.to_string().contains(&crate::views::id::path_segment(pr)), "{diff}");
    }
}

#[test]
fn a_question_about_one_pr_keeps_the_other_card_and_the_working_task_visible() {
    let mut s = two_open_prs();
    must(&mut s, user(), Command::AuthorizeMerge {
        authorization_id: "other-pr".into(), presentation_id: "final".into(), pr_id: CLOUD.into(),
        expected_head: CLOUD_HEAD.into(), method: MergeMethod::Squash,
    });
    must(&mut s, user(), Command::SubmitFeedback {
        presentation_id: "final".into(), targets: vec![pr(PUBLIC, FIRST_HEAD)],
        body: "Explain the menu change".into(), attachments: vec![], instruction: Instruction::Question, merge_failure: None,
    });
    assert_eq!(cards(&s), [CLOUD], "the other PR still needs its own decision");
    assert!(s.merge_authorizations[&AuthorizationId::from("other-pr")].valid);
    assert_eq!(card_actions(&s, CLOUD).len(), 1);
    let index = ReadIndex::build(&s);
    for key in [format!("card:{PUBLIC}"), format!("pull_request:{PUBLIC}")] {
        let view = crate::views::build(&key, RenderContext {
            domain: &s, index: &index, live: &LiveSnapshot::default(), client: Client::Tui, lang: Lang::Es,
            transport: Transport::Loopback, cursor: "cursor", rendered_at: "2026-11-01T10:00:00Z",
        }).unwrap();
        let actions: Vec<_> = view.sections.iter().flat_map(|section| &section.items).flat_map(|item| match item {
            crate::views::Item::Card(card) => card.actions.iter().chain(&card.more).map(|action| action.id.as_str()).collect::<Vec<_>>(),
            crate::views::Item::Actions(bar) => bar.actions.iter().map(|action| action.id.as_str()).collect(),
            _ => vec![],
        }).collect();
        assert!(!actions.iter().any(|action| action.contains(CLOUD)), "a detail never offers another PR's action: {actions:?}");
    }

    must(&mut s, Actor::System, Command::TellNode {
        task_id: TASK.into(), text: "Explain the menu change".into(), from_user: false, answers: None,
    });
    let input = s.lifecycle_of(&TASK.into()).unwrap().pending_inputs()[0].as_str().to_owned();
    must(&mut s, Actor::System, Command::RecordRuntimeFacts {
        task_id: TASK.into(), generation: 1, facts: vec![RuntimeObservation::TurnStarted { turn: "answer".into(), input }],
    });
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::TitleRequest {
        request_id: "r".into(), title: "Update the phone menu".into(),
    });
    for id in ["queued-one", "queued-two"] {
        must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
            task_id: id.into(), request_id: "r".into(), title: id.into(), agent_id: None, review_prs: false,
            plan_items: None, continues: None, dependencies: Default::default(), stack: None,
            launch: Some(LaunchPlan::new("node", "/synthetic/repository")),
        });
    }
    let index = ReadIndex::build(&s);
    assert_eq!(index.task(&TASK.into()).unwrap().situation, Situation::Working(Doing::Turn));
    for client in [Client::Tui, Client::Mac, Client::Ios] {
        let view = crate::views::build("tasks", RenderContext {
            domain: &s, index: &index, live: &LiveSnapshot::default(), client, lang: Lang::Es,
            transport: Transport::Loopback, cursor: "cursor", rendered_at: "2026-11-01T10:00:00Z",
        }).unwrap();
        fn visits(rows: &[crate::views::Item], id: &str) -> usize {
            fn row_visits(row: &crate::views::Row, id: &str) -> usize {
                usize::from(row.id == id) + row.children.iter().map(|child| row_visits(child, id)).sum::<usize>()
            }
            rows.iter().map(|item| match item { crate::views::Item::Row(row) => row_visits(row, id), _ => 0 }).sum()
        }
        let active = view.sections.iter().find(|section| section.id == "active").unwrap();
        for task in s.tasks.values().filter(|task| !task.over() && !task.archived) {
            assert_eq!(visits(&active.items, &task.id.0), 1, "{client:?}: {} belongs to exactly one block", task.id);
        }
    }
}

#[test]
fn a_delivery_of_three_prs_is_three_cards_and_the_task_closes_with_the_last() {
    let mut s = two_open_prs();
    let docs = "github.com/bitomule/docs#3";
    let docs_head = "3333333333333333333333333333333333333333";
    register(&mut s, "bitomule/docs", 3, docs_head);
    must(&mut s, node(), result("final-3", vec![pr(PUBLIC, "")], true));
    assert_eq!(cards(&s), [PUBLIC, CLOUD, docs]);
    for card in [PUBLIC, CLOUD, docs] {
        assert_eq!(card_actions(&s, card).len(), 1, "{card} offers only its own merge");
    }
    merged(&mut s, CLOUD, CLOUD_HEAD);
    assert_eq!(cards(&s), [PUBLIC, docs]);
    observe(&mut s, docs, docs_head, PrState::Closed);
    assert_eq!(cards(&s), [PUBLIC]);
    assert!(!s.finished(&s.tasks[&TASK.into()]));
    merged(&mut s, PUBLIC, FIRST_HEAD);
    assert!(s.finished(&s.tasks[&TASK.into()]));
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
fn a_rebase_preserves_the_acceptance_record_and_requires_approval_of_the_new_head() {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    must(&mut s, node(), result("before-rebase", vec![pr(PUBLIC, FIRST_HEAD)], true));
    let targets = vec![pr(PUBLIC, FIRST_HEAD)];
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "before-rebase".into(), targets: targets.clone() });
    let accepted_at = match &s.interactions[&InteractionId::from("before-rebase")].payload {
        InteractionPayload::Delivery { acceptance: Some(acceptance), .. } => acceptance.at_revision,
        _ => panic!("the acceptance was recorded"),
    };
    must(&mut s, Actor::System, Command::ObservePrState {
        pr_id: PUBLIC.into(), head: FIRST_HEAD.into(), base: "develop".into(), state: PrState::Open, merged_head: None,
    });
    assert!(s.merge_authorizations.values().any(|authorization| authorization.valid), "retargeting without changing the head preserves approval");
    let rebased = "4".repeat(40);
    let outcome = exec(&mut s, Actor::System, Command::ObservePrState {
        pr_id: PUBLIC.into(), head: rebased.clone(), base: "develop".into(), state: PrState::Open, merged_head: None,
    }).unwrap();
    assert_eq!(outcome.superseded_acceptances.len(), 1);
    assert_eq!(outcome.superseded_acceptances[0].accepted_at, accepted_at);
    assert!(s.merge_authorizations.values().all(|authorization| !authorization.valid));
    let before = s.clone();
    assert!(exec(&mut s, user(), Command::AcceptDelivery { presentation_id: "before-rebase".into(), targets }).is_err());
    assert_eq!(s, before);
    must(&mut s, node(), result("after-rebase", vec![pr(PUBLIC, &rebased)], true));
    assert_eq!(cards(&s), ["after-rebase"]);
    assert_eq!(card_actions(&s, "after-rebase").len(), 1);
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "after-rebase".into(), targets: vec![pr(PUBLIC, &rebased)] });
    assert!(s.merge_authorizations.values().any(|authorization| authorization.valid && authorization.head == rebased));
    let InteractionPayload::Delivery { acceptance: Some(earlier), .. } = &s.interactions[&InteractionId::from("before-rebase")].payload else { panic!("the earlier acceptance remains in history") };
    assert_eq!(earlier.at_revision, accepted_at);
    assert!(!earlier.valid);
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
        authorization_id: "first".into(), presentation_id: "final".into(), pr_id: PUBLIC.into(), expected_head: FIRST_HEAD.into(), method: MergeMethod::Squash,
    });
    merged(&mut s, PUBLIC, FIRST_HEAD);
    assert!(!s.finished(&s.tasks[&TASK.into()]), "one PR is still open");
    assert_eq!(s.unmerged_targets(&TASK.into()), 1);

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
    register(&mut s, "acme/storefront", 6, CLOUD_HEAD);
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
fn a_pr_merged_by_hand_on_github_counts_as_merged_wherever_the_task_is() {
    let mut delivered = two_open_prs();
    merged(&mut delivered, CLOUD, CLOUD_HEAD);
    assert_eq!(card_actions(&delivered, "final").len(), 1, "only the PR still open offers its merge");
    merged(&mut delivered, PUBLIC, FIRST_HEAD);
    assert!(delivered.finished(&delivered.tasks[&TASK.into()]), "nobody pressed anything in Alondra");

    let mut undelivered = at_work();
    register(&mut undelivered, "acme/storefront", 6, CLOUD_HEAD);
    merged(&mut undelivered, CLOUD, CLOUD_HEAD);
    must(&mut undelivered, node(), result("final", text(), true));

    let mut unread = at_work();
    opened(&mut unread);
    must(&mut unread, Actor::System, Command::RecordOpenedPr {
        task_id: TASK.into(), host: "github.com".into(), repository: "acme/storefront".into(), number: 6,
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
fn work_waiting_after_a_merge_reads_waiting_on_every_client_and_in_the_header() {
    use crate::views::{Item, Row};
    let mut s = at_work();
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::TitleRequest {
        request_id: "r".into(), title: "Update the package".into(),
    });
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
        task_id: "package".into(), request_id: "r".into(), title: "Publish the package".into(), agent_id: None,
        review_prs: false, plan_items: None, continues: None, dependencies: BTreeSet::new(), stack: None,
        launch: Some(LaunchPlan::new("node", "/synthetic/repository")),
    });
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    must(&mut s, node(), Command::DeclarePostMergeWork {
        task_id: TASK.into(), reason: Some("Check the installed package".into()), waits_for: vec![],
    });
    must(&mut s, node(), result("final", vec![pr(PUBLIC, FIRST_HEAD)], true));
    merged(&mut s, PUBLIC, FIRST_HEAD);
    must(&mut s, node(), Command::DeclarePostMergeWork {
        task_id: TASK.into(), reason: Some("Check the installed package".into()),
        waits_for: vec![WaitTarget::Task { task_id: "package".into() }],
    });
    assert!(s.lifecycle_of(&TASK.into()).unwrap().turn().is_some());
    assert_eq!(ReadIndex::build(&s).task(&TASK.into()).unwrap().situation, Situation::Working(Doing::AfterMerge),
        "the provider's real turn is still running");
    turn_ends(&mut s);
    assert!(matches!(s.lifecycle_of(&TASK.into()).unwrap().awaits(), Some(alondra_domain::Await::Tasks(_))),
        "the SDK boundary records what the idle task waits for");
    fn collect<'a>(row: &'a Row, id: &str, found: &mut Vec<&'a Row>) {
        if row.id == id { found.push(row); }
        for child in &row.children { collect(child, id, found); }
    }
    let reopened: DomainState = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
    for state in [&s, &reopened] {
        let index = ReadIndex::build(state);
        assert_eq!(index.task(&TASK.into()).unwrap().situation, Situation::Waiting(WaitsFor::Tasks));
        assert_eq!(index.activity().working, 0);
        assert_eq!(index.activity().tasks(), 2);
        let live = LiveSnapshot::default();
        for client in [Client::Ios, Client::Mac, Client::Tui] {
            for lang in [Lang::En, Lang::Es] {
                let view = crate::views::build("tasks", RenderContext {
                    domain: state, index: &index, live: &live, client, lang, transport: Transport::Loopback,
                    cursor: "cursor", rendered_at: "",
                }).unwrap();
                let active = view.sections.iter().find(|section| section.id == "active").unwrap();
                for task in state.tasks.values().filter(|task| !task.over()) {
                    let mut found = Vec::new();
                    for item in &active.items { if let Item::Row(row) = item { collect(row, &task.id.0, &mut found); } }
                    assert_eq!(found.len(), 1, "{client:?}/{lang:?}: {} appears once", task.id);
                }
                let block = active.items.iter().find_map(|item| match item {
                    Item::Row(row) if row.id == "block:waiting" => Some(row), _ => None,
                }).unwrap();
                let mut found = Vec::new();
                collect(block, TASK, &mut found);
                assert_eq!(found.len(), 1, "{client:?}/{lang:?}: the dependency belongs in Waiting");
                let shown = serde_json::to_string(found[0]).unwrap();
                assert!(shown.contains("Publish the package"), "{shown}");
                assert_eq!(found[0].badges[0].text, if lang == Lang::Es { "En espera" } else { "Waiting" });
                assert_eq!(view.status.counts.iter().find(|count| count.id == "working").unwrap().value, 0);
            }
        }
    }
    let ended = exec(&mut s, user(), Command::CancelTask { task_id: "package".into(), reason: None }).unwrap();
    let notice = ended.effects.iter().find_map(|effect| match effect {
        DomainEffect::NotifyAgent { task_id: Some(task), message, .. } if task.0 == TASK => Some(message.clone()),
        _ => None,
    }).expect("ending the dependency notifies its waiting node");
    must(&mut s, Actor::System, Command::TellNode {
        task_id: TASK.into(), text: notice, from_user: false, answers: None,
    });
    assert!(!matches!(ReadIndex::build(&s).task(&TASK.into()).unwrap().situation, Situation::Waiting(WaitsFor::Tasks)),
        "a dependency that ended no longer holds the task");
}

#[test]
fn a_confirmed_merge_retires_the_review_card_and_its_for_you_label_on_every_client() {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    must(&mut s, node(), result("final", vec![pr(PUBLIC, FIRST_HEAD)], true));
    turn_ends(&mut s);
    assert_eq!(cards(&s), ["final"]);
    merged(&mut s, PUBLIC, FIRST_HEAD);
    let index = ReadIndex::build(&s);
    assert!(!index.task(&TASK.into()).unwrap().for_you());
    assert_eq!(index.activity().needs_you, 0);
    assert!(cards(&s).is_empty(), "GitHub already confirmed the reviewed head merged");
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        for lang in [Lang::En, Lang::Es] {
            for key in ["home", "tasks", "task:t"] {
                if client == Client::Mac && key == "home" { continue; }
                let view = crate::views::build(key, RenderContext {
                    domain: &s, index: &index, live: &LiveSnapshot::default(), client, lang,
                    transport: Transport::Loopback, cursor: "cursor", rendered_at: "",
                }).unwrap();
                assert_eq!(view.status.counts.iter().find(|count| count.id == "needs_you").unwrap().value, 0);
                assert!(view.sections.iter().all(|section| section.card_ids.is_empty()), "{client:?}/{lang:?}/{key}: no review card remains");
                if key == "tasks" {
                    let shown = serde_json::to_value(&view).unwrap();
                    let active = find_section(&shown, "active").unwrap();
                    assert!(!active.to_string().contains("block:for-you"), "{active}");
                }
            }
        }
    }
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

#[test]
fn replying_to_a_second_pr_keeps_both_replies_with_the_working_node() {
    fn target(pr_id: &str, head: &str) -> FeedbackTarget {
        FeedbackTarget::PullRequest { pr_id: pr_id.into(), head: head.into() }
    }
    fn submit(s: &mut DomainState, pr_id: &str, head: &str, body: &str) {
        must(s, user(), Command::SubmitFeedback {
            presentation_id: "final".into(), targets: vec![pr(pr_id, head)], body: body.into(),
            attachments: vec![], instruction: Instruction::Reply, merge_failure: None,
        });
        must(s, Actor::System, Command::TellNode {
            task_id: TASK.into(), text: body.into(), from_user: false, answers: None,
        });
    }
    fn assert_pending(s: &DomainState) {
        let p = &s.interactions[&InteractionId::from("final")];
        for (pr_id, head) in [(PUBLIC, FIRST_HEAD), (CLOUD, CLOUD_HEAD)] {
            assert!(s.pending_reply(p, &target(pr_id, head)), "{pr_id} is still unanswered");
            let action = s.card_for_pr(&s.item(p), &pr_id.into()).unwrap().available_actions.into_iter()
                .find(|action| action.kind == "authorize_merge").unwrap();
            assert!(!action.enabled);
            assert_eq!(action.disabled_reason.as_deref(), Some("pending_user_reply"));
        }
    }
    fn assert_returned(s: &DomainState) {
        let p = &s.interactions[&InteractionId::from("final")];
        for (pr_id, head) in [(PUBLIC, FIRST_HEAD), (CLOUD, CLOUD_HEAD)] {
            assert_eq!(s.handed_back_for(p, &target(pr_id, head)), None, "{pr_id} is no longer in hand");
        }
        assert_eq!(cards(s), [PUBLIC, CLOUD], "a pause or rest returns both cards");
        assert_pending(s);
    }

    let mut s = two_open_prs();
    submit(&mut s, PUBLIC, FIRST_HEAD, "Explain the menu change");
    let input = s.lifecycle_of(&TASK.into()).unwrap().pending_inputs()[0].as_str().to_owned();
    must(&mut s, Actor::System, Command::RecordRuntimeFacts {
        task_id: TASK.into(), generation: 1,
        facts: vec![RuntimeObservation::TurnStarted { turn: "answer-both".into(), input }],
    });
    assert_eq!(cards(&s), [CLOUD], "replying to the first PR leaves the other card");
    let index = ReadIndex::build(&s);
    let other = index.cards().iter().find(|item| matches!(&item.subject,
        InboxSubject::Interaction { pr_id: Some(pr_id), .. } if pr_id.0 == CLOUD)).unwrap();
    let merge = other.available_actions.iter().find(|action| action.kind == "authorize_merge").unwrap();
    assert!(merge.enabled, "the untouched PR retains its merge action");
    assert_eq!(merge.pr_id.as_ref().map(|pr| pr.0.as_str()), Some(CLOUD));
    assert_eq!(merge.expected_head.as_deref(), Some(CLOUD_HEAD));

    let mut later = s.clone();
    must(&mut later, Actor::System, Command::RecordTurnBoundary {
        task_id: TASK.into(), generation: 1, turn: "answer-both".into(), failure: None, failed: None,
    });
    assert_eq!(cards(&later), [PUBLIC, CLOUD], "A returns before there is any reply about B");
    assert!(!later.open_reply(&later.interactions[&InteractionId::from("final")]));
    submit(&mut later, CLOUD, CLOUD_HEAD, "A new reply about the storefront");
    let input = later.lifecycle_of(&TASK.into()).unwrap().pending_inputs()[0].as_str().to_owned();
    must(&mut later, Actor::System, Command::RecordRuntimeFacts {
        task_id: TASK.into(), generation: 1,
        facts: vec![RuntimeObservation::TurnStarted { turn: "answer-later".into(), input }],
    });
    let p = &later.interactions[&InteractionId::from("final")];
    let InteractionPayload::Delivery { feedback, .. } = &p.payload else { panic!("delivery") };
    let last = feedback.last().unwrap();
    assert_eq!(last.reply_window_start, Some(last.at_revision), "a reply after the prior return starts its own window");
    assert!(last.at_revision > feedback[0].at_revision);
    assert!(later.open_reply(p));
    assert_eq!(later.handed_back_for(p, &target(PUBLIC, FIRST_HEAD)), None, "the new B turn must not hide the returned A card");
    assert_eq!(later.handed_back_for(p, &target(CLOUD, CLOUD_HEAD)), Some(Instruction::Reply));
    assert_eq!(cards(&later), [PUBLIC]);
    assert_pending(&later);
    assert_eq!(ReadIndex::build(&later).task(&TASK.into()).unwrap().situation, Situation::Working(Doing::Turn));

    submit(&mut s, CLOUD, CLOUD_HEAD, "Explain the storefront change");
    let input = s.lifecycle_of(&TASK.into()).unwrap().pending_inputs()[0].as_str().to_owned();
    must(&mut s, Actor::System, Command::RecordRuntimeFacts {
        task_id: TASK.into(), generation: 1, facts: vec![RuntimeObservation::InputConsumed { input }],
    });
    let p = &s.interactions[&InteractionId::from("final")];
    assert!(s.open_reply(p));
    assert_eq!(s.handed_back(p), Some(Instruction::Reply), "the global lifecycle obligation is unchanged");
    let InteractionPayload::Delivery { feedback, .. } = &p.payload else { panic!("delivery") };
    let first_window = feedback[0].at_revision;
    assert!(feedback.iter().all(|reply| reply.reply_window_start == Some(first_window)), "consecutive replies share the same visibility window");
    assert!(feedback.iter().all(|reply| s.told_since(&TASK.into(), reply.at_revision)), "both inputs have real lifecycle stamps");
    assert!(s.lifecycle_of(&TASK.into()).unwrap().pending_inputs().is_empty(), "the turn has consumed both replies");
    for (pr_id, head) in [(PUBLIC, FIRST_HEAD), (CLOUD, CLOUD_HEAD)] {
        assert_eq!(s.handed_back_for(p, &target(pr_id, head)), Some(Instruction::Reply), "{pr_id} remains in hand");
    }
    assert!(cards(&s).is_empty(), "both PR replies belong to the current turn");
    assert_pending(&s);
    let mut legacy_json = serde_json::to_value(&s).unwrap();
    for feedback in legacy_json["interactions"]["final"]["payload"]["feedback"].as_array_mut().unwrap() {
        feedback.as_object_mut().unwrap().remove("reply_window_start");
    }
    let legacy: DomainState = serde_json::from_value(legacy_json).unwrap();
    let p = &legacy.interactions[&InteractionId::from("final")];
    assert_eq!(legacy.handed_back_for(p, &target(PUBLIC, FIRST_HEAD)), None, "legacy feedback never revives an older scoped reply");
    assert_eq!(legacy.handed_back_for(p, &target(CLOUD, CLOUD_HEAD)), Some(Instruction::Reply));
    assert_eq!(cards(&legacy), [PUBLIC]);
    assert_pending(&legacy);
    let index = ReadIndex::build(&s);
    assert_eq!(index.task(&TASK.into()).unwrap().situation, Situation::Working(Doing::Turn));
    for client in [Client::Tui, Client::Mac, Client::Ios] {
        let view = crate::views::build("tasks", RenderContext {
            domain: &s, index: &index, live: &LiveSnapshot::default(), client, lang: Lang::Es,
            transport: Transport::Loopback, cursor: "cursor", rendered_at: "2026-11-01T10:00:00Z",
        }).unwrap();
        fn count(row: &crate::views::Row) -> usize {
            usize::from(row.id == TASK) + row.children.iter().map(count).sum::<usize>()
        }
        let active = view.sections.iter().find(|section| section.id == "active").unwrap();
        let places: Vec<_> = active.items.iter().filter_map(|item| match item {
            crate::views::Item::Row(row) => Some(row), _ => None,
        }).flat_map(|block| std::iter::repeat(block.id.as_str()).take(count(block))).collect();
        assert_eq!(places, ["block:working"], "{client:?}: the task belongs to exactly one working block");
        assert_eq!(active.count, Some(1));
    }

    let mut paused = s.clone();
    must(&mut paused, Actor::System, Command::MoveNode {
        agent_id: AGENT.into(), event: NodeEvent::Paused { pause: Pause {
            reason: PauseReason::Memory, since: 2, cut: None, turn_since: None,
            cleanup: PauseCleanup::Done, released: false,
        } },
    });
    assert!(paused.open_reply(&paused.interactions[&InteractionId::from("final")]), "a pause uses node_attends without rewriting the delivery's decision");
    assert_returned(&paused);

    must(&mut s, Actor::System, Command::RecordTurnBoundary {
        task_id: TASK.into(), generation: 1, turn: "answer-both".into(), failure: None, failed: None,
    });
    assert!(s.lifecycle_of(&TASK.into()).unwrap().working_on_reply().is_none());
    assert!(!s.open_reply(&s.interactions[&InteractionId::from("final")]), "return_replied_delivery closes the visibility window at TurnEnded");
    assert_returned(&s);

    must(&mut s, Actor::System, Command::TellNode {
        task_id: TASK.into(), text: "An unrelated instruction".into(), from_user: false, answers: None,
    });
    let input = s.lifecycle_of(&TASK.into()).unwrap().pending_inputs()[0].as_str().to_owned();
    must(&mut s, Actor::System, Command::RecordRuntimeFacts {
        task_id: TASK.into(), generation: 1,
        facts: vec![RuntimeObservation::TurnStarted { turn: "unrelated".into(), input }],
    });
    assert!(!s.open_reply(&s.interactions[&InteractionId::from("final")]), "a later input cannot reopen the reply window");
    assert_returned(&s);
}
