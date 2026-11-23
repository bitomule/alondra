//! The notice an auto-merged pull request gives its node, which alondrad's API words.
use crate::domain_runtime::DomainRuntime;
use alondra_domain_rules::*;
use std::collections::BTreeSet;

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

fn open() -> (crate::test_support::Directory, DomainRuntime) {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir(&home).unwrap();
    let domain = DomainRuntime::open_unwired_at(&home, &crate::domain_store::active_path(&home)).unwrap();
    (home, domain)
}

fn command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
        command_id: id.into(), expected_revision: state.revision, command,
    }, None, || Ok(())).unwrap();
}

/// Task `t` of a request that merges by itself or not, whose node registered PR 7 and presented
/// it as its final result.
fn presented(domain: &DomainRuntime, auto_merge: bool) {
    command(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
    command(domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge, maintenance: false, message: false });
    command(domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
        task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()),
        review_prs: true, dependencies: BTreeSet::new(), stack: None,
        launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    command(domain, Actor::Agent { agent_id: "a".into() }, "pr", register());
    command(domain, Actor::Agent { agent_id: "a".into() }, "present", Command::PresentResult {
        checklist: Vec::new(),
        attachments: vec![], task_id: "t".into(), interaction_id: "d".into(), body: "Ready".into(),
        targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEAD.into() }], is_final: true, nothing_to_review: false,
        card: CardSpec::delivery("Review the result", &[], true),
    });
}

fn register() -> Command {
    Command::RegisterPr {
        task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
        number: 7, head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    }
}

#[test]
fn the_auto_merge_notice_says_alondra_merges_without_protocol_or_git_metadata() {
    let (_home, domain) = open();
    presented(&domain, true);
    let notice = crate::domain_api::auto_merge_notice(&domain, &register()).unwrap();
    assert!(notice.contains("Alondra merges it") && notice.contains("do not merge it yourself"), "{notice}");
    for internal in ["github.com", "owner/repo", HEAD, "task_id", "revision", "authorization", "claim"] {
        assert!(!notice.contains(internal), "{internal} leaked in {notice}");
    }
}

#[test]
fn only_a_request_that_merges_by_itself_gets_the_auto_merge_notice() {
    for auto_merge in [true, false] {
        let (_home, domain) = open();
        presented(&domain, auto_merge);
        assert_eq!(crate::domain_api::auto_merge_notice(&domain, &register()).is_some(), auto_merge);
    }
}
