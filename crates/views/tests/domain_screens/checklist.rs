//! The approved checklist stays owed through PR review, merge and session recovery; only a
//! final delivery with evidence for every point can complete the task.
use alondra_domain_rules::*;
use alondra_views as views;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn the_pr_review_shows_unproved_points_as_pending_instead_of_completed() {
    use alondra_views::{Client, Lang, LiveSnapshot, RenderContext, Transport};
    let s = release_ready_for_review();
    let index = ReadIndex::build(&s);
    for (lang, subtitle, count) in [(Lang::En, "Pending after merge", "3 of 6 done"), (Lang::Es, "Pendiente tras el merge", "3 de 6 hechos")] {
        let view = alondra_views::build("card:release-review", RenderContext {
            domain: &s, index: &index, live: &LiveSnapshot::default(), client: Client::Mac, lang,
            transport: Transport::Loopback, cursor: "", rendered_at: "",
        }).unwrap();
        let json = serde_json::to_value(view).unwrap();
        let plan = json["sections"].as_array().unwrap().iter().find(|section| section["id"] == "plan").unwrap();
        assert!(plan["title"].as_str().unwrap().contains(count));
        let rows = plan["items"].as_array().unwrap();
        assert_eq!(rows.len(), 6);
        for number in 4..=6 {
            let pending = rows.iter().find(|row| row["id"] == format!("plan-pending-{number}")).unwrap();
            assert_eq!(pending["icon"], "schedule");
            assert_eq!(pending["subtitle"], subtitle);
        }
    }
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn agent() -> Actor {
    Actor::Agent { agent_id: "a".into() }
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

fn fixture() -> DomainState {
    let mut s = DomainState::new("epoch");
    ok(&mut s, Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "Node A".into(), at: 0 });
    ok(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Build it".into(), auto_merge: false, maintenance: false, message: false });
    ok(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
        task_id: "t".into(), request_id: "r".into(), title: "Build it".into(), agent_id: Some("a".into()),
        review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    s
}

fn plan(id: &str, body: &str) -> Command {
    Command::HoldHook {
        interaction_id: id.into(), task_id: "t".into(), agent_id: "a".into(), hold: Hold::Plan,
        body: body.into(), options: Hold::Plan.options(),
        card: CardSpec::new(CardKind::Review, "Build it").content(ContentKind::Plan, body),
    }
}

fn answer(id: &str, option: &str) -> Command {
    let inputs = if option == Hold::REQUEST_CHANGES {
        BTreeMap::from([(Hold::NOTE.to_owned(), InputValue::Text("Split it".into()))])
    } else {
        BTreeMap::new()
    };
    Command::AnswerQuestion { attachments: vec![], interaction_id: id.into(), option_id: option.into(), inputs }
}

fn approved(s: &mut DomainState, id: &str, body: &str) {
    ok(s, Actor::System, plan(id, body));
    ok(s, user(), answer(id, Hold::APPROVE));
}

fn commit(value: &str) -> Evidence {
    Evidence { kind: EvidenceKind::Commit, value: value.into(), detail: None }
}

fn mark(item: &str, evidence: Vec<Evidence>) -> ChecklistMark {
    ChecklistMark { item: item.into(), evidence }
}

fn deliver(id: &str, is_final: bool, checklist: Vec<ChecklistMark>, attachments: Vec<AttachmentRef>) -> Command {
    let targets = vec![DeliveryTarget::Text { text: "Done".into() }];
    Command::PresentResult {
        checklist, attachments, task_id: "t".into(), interaction_id: id.into(), body: "Done".into(),
        card: CardSpec::delivery("Done", &targets, is_final), targets, is_final, nothing_to_review: false,
    }
}

const RELEASE_PLAN: &str = "- [ ] Implement the release\n- [ ] Test the release\n- [ ] Prepare the pull request\n- [ ] Publish the release\n- [ ] Install the release\n- [ ] Report the installed version\n";

fn release_pr() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::PullRequest { pr_id: "release-pr".into(), head: "abcdef0".into() }]
}

fn release_task() -> DomainState {
    let mut s = fixture();
    approved(&mut s, "release-plan", RELEASE_PLAN);
    ok(&mut s, agent(), Command::StartTask { task_id: "t".into() });
    ok(&mut s, agent(), Command::RegisterPr {
        task_id: "t".into(), pr_id: "release-pr".into(), host: "github.com".into(), repository: "acme/build-tool".into(), number: 49,
        head: "abcdef0".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    s
}

fn release_delivery() -> Command {
    let mut ready = deliver("release-review", true, (1..=3).map(|n| mark(&n.to_string(), vec![commit("abcdef0")])).collect(), vec![]);
    if let Command::PresentResult { targets, .. } = &mut ready { *targets = release_pr(); }
    ready
}

fn release_ready_for_review() -> DomainState {
    let mut s = release_task();
    ok(&mut s, agent(), Command::DeclarePostMergeWork {
        task_id: "t".into(), reason: Some("Publish, install and report the release after merge".into()), waits_for: vec![],
    });
    ok(&mut s, agent(), release_delivery());
    s
}
