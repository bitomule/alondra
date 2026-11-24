//! Review repros for PR identity phase 4: a moved repository must not change what Alondra does
//! with a task's other pull requests, and a pull request pushed to before its first read is still
//! the task's own.
use crate::domain_github::tests::open;
use crate::domain_github::{ObservationFailures, catch_up_task_using, reconcile};
use crate::domain_merges::Merger;
use crate::domain_runtime::DomainRuntime;
use crate::github_world::GithubWorld;
use alondra_domain_rules::*;
use std::collections::BTreeSet;
use std::time::Duration;

const H7: &str = "7777777777777777777777777777777777777777";
const H8: &str = "8888888888888888888888888888888888888888";

fn command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(),
        expected_revision: state.revision, command,
    }, None, || Ok(())).unwrap();
}

/// A read already confirmed which pull request `number` of the world is: merging is only
/// authorized for a pull request Alondra can check.
fn confirmed(domain: &DomainRuntime, world: &GithubWorld, pr_id: &str, number: u64) {
    let key = world.key_of(number);
    command(domain, Actor::System, &format!("confirmed-{pr_id}"), Command::ObservePrAddress {
        pr_id: pr_id.into(), read: PrRead::Address { repository: "owner/repo".into() }, read_started_at: 1,
        verdict: PrAddressVerdict::Confirmed { locator: ProviderLocator(format!("PR_node{}", key.0)), key, current_repository: "owner/repo".into() },
    });
}

fn mergeable(domain: &DomainRuntime, pr_id: &str, head: &str) {
    command(domain, Actor::System, &format!("verdict-{pr_id}"), Command::ObserveMergeVerdict {
        pr_id: pr_id.into(), head: head.into(), verdict: MergeVerdict::mergeable() });
}

fn merger() -> Merger {
    let mut merger = Merger::with(Duration::ZERO, Duration::from_secs(3600));
    merger.first_retry = Duration::ZERO;
    merger
}

fn task(domain: &DomainRuntime, task_id: &str, agent: &str) {
    command(domain, Actor::System, &format!("agent-{agent}"), Command::RegisterAgent { agent_id: agent.into(), name: agent.into(), at: 0 });
    command(domain, Actor::Orchestrator { generation: 0 }, &format!("task-{task_id}"), Command::CreateTask { priority: Default::default(),
        plan_items: None, continues: None, task_id: task_id.into(), request_id: "r".into(), title: "Task".into(), agent_id: Some(agent.into()),
        review_prs: true, dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")) });
}

fn register(domain: &DomainRuntime, task_id: &str, agent: &str, pr_id: &str, number: u64, head: &str, base: &str) {
    command(domain, Actor::Agent { agent_id: agent.into() }, &format!("register-{pr_id}"), Command::RegisterPr {
        task_id: task_id.into(), pr_id: pr_id.into(), host: "github.com".into(), repository: "owner/repo".into(),
        number, head: head.into(), base: base.into(), allowed_methods: vec![MergeMethod::Squash],
    });
}

/// One task with two pull requests: #7 from `feature-7` into main, already merged, and #8 from
/// `feature-8` stacked on `feature-7` (no stack declared), merge authorized by the user. Then the
/// repository moves to `owner/moved`, with `feature-7` kept (GitHub does not delete it).
#[tokio::test]
async fn a_sibling_merged_before_its_repository_moved_still_gets_its_dependant_retargeted() {
    for moved in [false, true] {
        let world = GithubWorld::new();
        world.open_pr_at(7, "feature-7", "main", H7);
        world.open_pr_at(8, "feature-8", "feature-7", H8);
        let (_fixture, domain) = open("review-sibling");
        command(&domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(),
            auto_merge: false, maintenance: false, message: false });
        task(&domain, "t", "a");
        register(&domain, "t", "a", "pr7", 7, H7, "main");
        register(&domain, "t", "a", "pr8", 8, H8, "feature-7");
        confirmed(&domain, &world, "pr7", 7);
        confirmed(&domain, &world, "pr8", 8);
        mergeable(&domain, "pr8", H8);
        command(&domain, Actor::System, "merged-7", Command::ObservePrState { pr_id: "pr7".into(), head: H7.into(), base: "main".into(),
            state: PrState::Merged, merged_head: Some(H7.into()) });
        command(&domain, Actor::Agent { agent_id: "a".into() }, "present", Command::PresentResult { checklist: vec![], attachments: vec![],
            task_id: "t".into(), interaction_id: "d".into(), body: "Ready".into(),
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr8".into(), head: H8.into() }], is_final: true, nothing_to_review: false,
            card: CardSpec::delivery("Review the result", &[], true) });
        command(&domain, Actor::User { user_id: "u".into() }, "authorize", Command::AuthorizeMerge { authorization_id: "auth".into(),
            presentation_id: "d".into(), pr_id: "pr8".into(), expected_head: H8.into(), method: MergeMethod::Squash, despite_pending_message: false });

        world.merge_externally(7);
        if moved {
            world.move_repository("owner/repo", "owner/moved");
        }
        merger().merge_due(&domain, &world).await.unwrap();
        assert_eq!(world.retargets(), [(8, "main".to_owned())], "moved = {moved}: #8 is moved onto main before it merges; calls {:?}", world.gh_calls());
        assert_eq!(world.merged_into(8).as_deref(), Some("main"), "moved = {moved}: #8 lands in main, not in its merged sibling's branch");
    }
}

/// Task `t`'s PR #7 merged before the repository moved; task `down` is stacked on it with PR #8
/// on `parent-feature`, merge authorized. After the move the stack still merges.
#[tokio::test]
async fn a_stack_whose_parent_merged_before_its_repository_moved_still_merges() {
    for moved in [false, true] {
        let world = GithubWorld::new();
        world.open_pr_at(7, "parent-feature", "main", H7);
        world.open_pr_at(8, "child-feature", "parent-feature", H8);
        let (_fixture, domain) = open("review-stack");
        command(&domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(),
            auto_merge: false, maintenance: false, message: false });
        command(&domain, Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "r".into(), title: "Stacked changes".into() });
        task(&domain, "t", "a");
        register(&domain, "t", "a", "pr", 7, H7, "main");
        command(&domain, Actor::Agent { agent_id: "a".into() }, "present", Command::PresentResult { checklist: vec![], attachments: vec![],
            task_id: "t".into(), interaction_id: "d".into(), body: "Ready".into(),
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: H7.into() }], is_final: true, nothing_to_review: false,
            card: CardSpec::delivery("Review the result", &[], true) });
        command(&domain, Actor::Agent { agent_id: "a".into() }, "ready-parent", Command::PublishStackReadiness {
            task_id: "t".into(), presentation_id: "d".into(), pr_id: "pr".into(), head: H7.into(), ready: true });
        command(&domain, Actor::System, "agent-b", Command::RegisterAgent { agent_id: "b".into(), name: "B".into(), at: 0 });
        command(&domain, Actor::Orchestrator { generation: 0 }, "task-down", Command::CreateTask { priority: Default::default(),
            task_id: "down".into(), request_id: "r".into(), title: "Downstream".into(), agent_id: Some("b".into()),
            review_prs: true, dependencies: BTreeSet::new(), plan_items: None, continues: None,
            stack: Some(StackDependency { upstream_task: "t".into(), presentation_id: "d".into(), pr_id: "pr".into(), head: H7.into() }),
            launch: Some(LaunchPlan::new("node", "/synthetic/repository")) });
        register(&domain, "down", "b", "pr-down", 8, H8, "parent-feature");
        confirmed(&domain, &world, "pr", 7);
        confirmed(&domain, &world, "pr-down", 8);
        mergeable(&domain, "pr-down", H8);
        command(&domain, Actor::Agent { agent_id: "b".into() }, "present-down", Command::PresentResult { checklist: vec![], attachments: vec![],
            task_id: "down".into(), interaction_id: "d-down".into(), body: "Downstream ready".into(),
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr-down".into(), head: H8.into() }], is_final: true, nothing_to_review: false,
            card: CardSpec::delivery("Review the change", &[], true) });
        command(&domain, Actor::User { user_id: "u".into() }, "authorize-down", Command::AuthorizeMerge { authorization_id: "auth-down".into(),
            presentation_id: "d-down".into(), pr_id: "pr-down".into(), expected_head: H8.into(), method: MergeMethod::Squash, despite_pending_message: false });
        command(&domain, Actor::System, "parent-merged", Command::ObservePrState { pr_id: "pr".into(), head: H7.into(), base: "main".into(),
            state: PrState::Merged, merged_head: Some(H7.into()) });

        world.merge_externally(7);
        if moved {
            world.move_repository("owner/repo", "owner/moved");
        }
        let mut merger = merger();
        for _ in 0..3 {
            merger.merge_due(&domain, &world).await.unwrap();
        }
        assert_eq!(world.merged_into(8).as_deref(), Some("main"), "moved = {moved}: the stack is retargeted and merged; calls {:?}", world.gh_calls());
    }
}

fn own_worktree_on(root: &crate::test_support::Directory, domain: &DomainRuntime, branch: &str) -> std::path::PathBuf {
    let dir = alondra_engine_doubles::worktrees::testing::task_worktree(root, "worktree");
    command(domain, Actor::System, "worktree", Command::RecordTaskWorktree {
        task_id: "t".into(), path: dir.display().to_string(), origin: WorktreeOrigin::Task, branch: Some(branch.into()),
    });
    dir
}

/// A task registers its PR at the head GitHub has, then its node pushes again before Alondra's
/// first read. The address still answers with that same pull request, from the branch of the
/// task's worktree: it must not become one Alondra cannot check.
#[tokio::test]
async fn a_push_between_registering_and_the_first_read_keeps_the_pull_request_checkable() {
    let world = GithubWorld::new();
    let head = world.open_pr(42, "csv-export", "main");
    let (_fixture, domain) = open("review-pushed");
    let checkout = crate::test_support::Directory::new();
    command(&domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(),
        auto_merge: false, maintenance: false, message: false });
    task(&domain, "t", "a");
    own_worktree_on(&checkout, &domain, "csv-export");
    register(&domain, "t", "a", "pr", 42, &head, "main");
    world.push_commit(42);
    reconcile(&domain, &world, &mut ObservationFailures::default()).await.unwrap();
    let pr = domain.state().unwrap().pull_requests[&PrId::from("pr")].clone();
    assert!(pr.identity.is_verified(), "the task's own pull request, pushed to once more, is still checkable: {:?}", pr.identity);
}

/// The same push, read before the task's branch was known: the record cannot be checked, and a
/// read that matches the branch once it is known gives it its identity back.
#[tokio::test]
async fn an_unchecked_record_is_recovered_by_a_read_that_matches_the_task_branch() {
    let world = GithubWorld::new();
    let head = world.open_pr(42, "csv-export", "main");
    let (_fixture, domain) = open("review-recovered");
    let checkout = crate::test_support::Directory::new();
    command(&domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(),
        auto_merge: false, maintenance: false, message: false });
    task(&domain, "t", "a");
    register(&domain, "t", "a", "pr", 42, &head, "main");
    let pushed = world.push_commit(42);
    let mut failures = ObservationFailures::default();
    reconcile(&domain, &world, &mut failures).await.unwrap();
    assert!(domain.state().unwrap().pull_requests[&PrId::from("pr")].identity.is_unresolvable(), "nothing says whose pull request this is yet");
    own_worktree_on(&checkout, &domain, "csv-export");
    reconcile(&domain, &world, &mut failures).await.unwrap();
    let pr = domain.state().unwrap().pull_requests[&PrId::from("pr")].clone();
    assert!(pr.identity.is_verified(), "the branch matches the task's: {:?}", pr.identity);
    assert_eq!(pr.head, pushed, "and its commit is current");
}

/// The open pull request of a task's branch is found at the address its repository moved to. It
/// is the record the task already has, matched by which pull request it is, not recorded again.
#[tokio::test]
async fn the_open_pr_of_a_branch_at_a_moved_address_is_the_record_the_task_has() {
    let world = std::sync::Arc::new(GithubWorld::new());
    let head = world.open_pr(42, "csv-export", "main");
    let (_fixture, domain) = open("review-branch");
    let checkout = crate::test_support::Directory::new();
    command(&domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(),
        auto_merge: false, maintenance: false, message: false });
    task(&domain, "t", "a");
    register(&domain, "t", "a", "pr", 42, &head, "main");
    reconcile(&domain, &*world, &mut ObservationFailures::default()).await.unwrap();
    assert!(domain.state().unwrap().pull_requests[&PrId::from("pr")].identity.is_verified());

    world.move_repository("owner/repo", "owner/moved");
    let dir = own_worktree_on(&checkout, &domain, "csv-export");
    world.check_out(&dir, "csv-export");
    let github = alondra_engine_ports::github::GitHub::of(world.clone());
    catch_up_task_using(&domain, &github, &*world, &"t".into(), true).await.unwrap();

    let state = domain.state().unwrap();
    assert_eq!(state.pull_requests.len(), 1, "one record for the one pull request: {:?}", state.pull_requests.keys().collect::<Vec<_>>());
    let pr = &state.pull_requests[&PrId::from("pr")];
    assert_eq!((pr.repository.as_str(), pr.aliases.iter().map(String::as_str).collect::<Vec<_>>()), ("owner/moved", vec!["owner/repo"]));
}
