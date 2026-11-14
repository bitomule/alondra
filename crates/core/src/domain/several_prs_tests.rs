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
fn audit_case_5_a_stored_delivery_of_a_closed_pr_has_no_card_or_button() {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    must(&mut s, node(), result("final", vec![pr(PUBLIC, FIRST_HEAD)], true));
    // Old snapshots can contain a final delivery whose PR closed before it was recorded.
    s.pull_requests.get_mut(&PUBLIC.into()).unwrap().state = PrState::Closed;
    assert!(cards(&s).is_empty());
    assert!(settled_notice(&s, "card:final").is_some());
}

#[test]
fn a_pr_cited_in_a_delivery_must_be_one_of_its_targets() {
    for citation in ["https://github.com/acme/storefront/pull/14", "acme/storefront#14", "**https://github.com/acme/storefront/pull/14**", "_acme/storefront#14_"] {
        let mut s = at_work();
        let mut command = result("cited-result", vec![DeliveryTarget::Text { text: "Report".into() }], true);
        if let Command::PresentResult { body, .. } = &mut command { *body = format!("Finished: {citation}"); }
        let error = exec(&mut s, node(), command).unwrap_err();
        assert_eq!(error.code, "cited_pr_not_target", "{citation}");
        assert_eq!(error.contract.unwrap().rule, ContractRule::CitedPrNotTarget);
    }
}

#[test]
fn the_two_repository_incident_delivers_the_registered_pr_after_the_first_closes() {
    let mut s = at_work();
    register(&mut s, "acme/app", 65, FIRST_HEAD);
    register(&mut s, "acme/site", 14, CLOUD_HEAD);
    let old = hosted_pr_id("github.com", "acme/app", 65);
    let site = hosted_pr_id("github.com", "acme/site", 14);
    observe(&mut s, &old.0, FIRST_HEAD, PrState::Closed);
    let mut command = result("two-repositories", vec![pr(&site.0, CLOUD_HEAD)], true);
    if let Command::PresentResult { body, .. } = &mut command {
        *body = "Documentation: https://github.com/acme/site/pull/14 (acme/site#14).".into();
    }
    must(&mut s, node(), command);
    assert_eq!(cards(&s), ["two-repositories"]);
    let actions = card_actions(&s, "two-repositories");
    assert_eq!(actions.len(), 1);
    assert!(actions[0].0.starts_with("merge:two-repositories@"), "only the current delivery's merge is offered");
    assert_eq!(s.open_prs(&s.tasks[&TASK.into()]).count(), 1);
}

#[test]
fn citations_in_each_displayed_delivery_field_are_checked_and_targets_in_other_repositories_match() {
    for field in ["body", "title", "need", "why", "content", "summary", "text"] {
        let mut s = at_work();
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
fn two_prs_at_the_same_commit_each_have_only_their_own_validated_merge() {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    register(&mut s, "acme/storefront", 6, FIRST_HEAD);
    must(&mut s, node(), result("final", vec![pr(PUBLIC, FIRST_HEAD)], true));
    let index = ReadIndex::build(&s);
    for item in index.cards() {
        let InboxSubject::Interaction { pr_id: Some(pr), .. } = &item.subject else { panic!("one card per PR") };
        let merges: Vec<_> = item.available_actions.iter().filter(|a| a.kind == "authorize_merge").collect();
        assert_eq!(merges.len(), 1);
        assert_eq!(merges[0].pr_id.as_ref(), Some(pr));
        assert!(merges[0].enabled);
        assert_eq!(card_actions(&s, &pr.0).len(), 1);
    }
}

#[test]
fn commit_evidence_links_use_the_repository_of_the_pr_being_reviewed() {
    let mut s = two_open_prs();
    s.checklist_marks.insert("final".into(), vec![ChecklistMark { item: "1".into(), evidence: vec![Evidence {
        kind: EvidenceKind::Commit, value: CLOUD_HEAD.into(), detail: None,
    }] }]);
    let view = view_json(&s, &format!("card:{CLOUD}"));
    let plan = find_section(&view, "plan").unwrap().to_string();
    assert!(plan.contains(&format!("https://github.com/acme/storefront/commit/{CLOUD_HEAD}")), "{plan}");
    assert!(!plan.contains("https://github.com/bitomule/alondra/commit/"));
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

#[test]
fn a_new_report_after_merge_remains_readable_without_finishing_publication() {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    register(&mut s, "acme/storefront", 6, CLOUD_HEAD);
    observe(&mut s, PUBLIC, FIRST_HEAD, PrState::Closed);
    must(&mut s, node(), Command::DeclarePostMergeWork { task_id: TASK.into(), reason: Some("Publish and verify the release".into()), waits_for: vec![] });
    merged(&mut s, CLOUD, CLOUD_HEAD);
    let mut report = result("publication-report", vec![pr(CLOUD, CLOUD_HEAD)], true);
    if let Command::PresentResult { body, card, .. } = &mut report {
        *body = "The release archive is ready for publication.".into();
        card.content.as_mut().unwrap().body = body.clone();
    }
    must(&mut s, node(), report);
    s.checklist_marks.insert("publication-report".into(), vec![ChecklistMark { item: "1".into(), evidence: vec![Evidence {
        kind: EvidenceKind::Commit, value: CLOUD_HEAD.into(), detail: None,
    }] }]);
    let view = view_json(&s, "card:publication-report");
    assert_eq!(view["view"], "card");
    assert!(find_section(&view, "content").unwrap().to_string().contains("The release archive is ready for publication."));
    assert!(view.to_string().contains("accept:publication-report@"));
    assert!(!view.to_string().contains("merge:publication-report@"));
    let index = ReadIndex::build(&s);
    let item = index.item(&s, &s.interactions[&"publication-report".into()]);
    assert!(item.available_actions.iter().any(|action| action.kind == "accept_delivery" && action.enabled));
    assert!(find_section(&view, "plan").unwrap().to_string().contains(&format!("https://github.com/acme/storefront/commit/{CLOUD_HEAD}")));
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "publication-report".into(), targets: vec![pr(CLOUD, CLOUD_HEAD)] });
    assert!(s.tasks[&TASK.into()].wrap_up.post_merge_work.is_some());
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
fn a_closed_pr_link_says_closed_even_when_another_pr_of_the_delivery_merged() {
    let mut s = two_open_prs();
    merged(&mut s, PUBLIC, FIRST_HEAD);
    observe(&mut s, CLOUD, CLOUD_HEAD, PrState::Closed);
    assert_eq!(settled_notice(&s, &format!("card:{CLOUD}")), Some("Cerrada sin hacer merge".into()));
    assert_eq!(settled_notice(&s, &format!("card:{PUBLIC}")), Some("Ya tiene merge".into()));
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
