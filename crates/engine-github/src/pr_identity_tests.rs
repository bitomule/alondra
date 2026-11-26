use super::*;
use crate::domain_github::tests::{Fixture, open};
use crate::domain_github::{ObservationFailures, reconcile};
use crate::github_world::GithubWorld;
use alondra_domain_model::{
    Actor, CommandEnvelope, GONE_STRIKES, MergeMethod, PROTOCOL_VERSION, PrId, PrState, PrTracking, ProviderLocator, STRIKE_GAP, UnresolvableReason,
};
use std::time::Duration;

const OLD: &str = "ana/lumo";
const NEW: &str = "lumo-org/lumo";

fn command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
        command_id: id.into(), expected_revision: state.revision, command,
    }, None, || Ok(())).unwrap();
}

/// A task with PR #42 of `ana/lumo` registered at `head`, no read of GitHub yet.
fn registered(tag: &str, head: &str) -> (Fixture, DomainRuntime, PrId) {
    let (fixture, domain) = open(tag);
    command(&domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
    command(&domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(),
        auto_merge: false, maintenance: false, message: false });
    command(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
        task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()), review_prs: true,
        dependencies: Default::default(), stack: None, launch: Some(alondra_domain_model::LaunchPlan::new("node", "/tmp")) });
    let pr_id = alondra_domain_model::hosted_pr_id("github.com", OLD, 42);
    command(&domain, Actor::Agent { agent_id: "a".into() }, "pr", Command::RegisterPr {
        task_id: "t".into(), pr_id: pr_id.clone(), host: "github.com".into(), repository: OLD.into(), number: 42,
        head: head.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    (fixture, domain, pr_id)
}

fn record(domain: &DomainRuntime, pr: &PrId) -> PullRequest {
    domain.state().unwrap().pull_requests[pr].clone()
}

fn read(key: &str, repository: &str, head_ref: &str) -> ReadIdentity {
    ReadIdentity { key: ProviderPrKey(key.into()), locator: ProviderLocator(format!("node-{key}")), repository: repository.into(), head_ref: head_ref.into() }
}

/// The rules that tell, from one answer of GitHub, whether it is the record's pull request.
#[test]
fn the_adapter_decides_which_pull_request_an_answer_is() {
    let head = "1".repeat(40);
    let mut pr = PullRequest::registered("pr".into(), "t".into(), "github.com".into(), OLD.into(), 42, head.clone(), "main".into(),
        PrState::Open, None, vec![MergeMethod::Squash]);
    let branches: BTreeSet<String> = ["csv-export".to_owned()].into();
    let confirmed = |key: &str, at: &str| PrAddressVerdict::Confirmed { key: ProviderPrKey(key.into()), locator: ProviderLocator(format!("node-{key}")),
        current_repository: at.into() };
    let other = "2".repeat(40);
    let cases = [
        (read("1", NEW, "other"), other.as_str(), confirmed("1", NEW), "a redirect to another name is the same pull request"),
        (read("1", OLD, "csv-export"), other.as_str(), confirmed("1", OLD), "the same address from the task's branch"),
        (read("1", OLD, "other"), head.as_str(), confirmed("1", OLD), "the same address at the record's head"),
        (read("1", OLD, "other"), other.as_str(), PrAddressVerdict::NamesAnotherPr, "the same address, another branch and head: a reused name"),
    ];
    for (answer, at, expected, why) in &cases {
        assert_eq!(verdict(&pr, &branches, OLD, Ok((Some(answer), at))), *expected, "{why}");
    }
    pr.identity = PrIdentity::Verified { key: ProviderPrKey("1".into()), locator: ProviderLocator("node-1".into()) };
    assert_eq!(verdict(&pr, &branches, OLD, Ok((Some(&read("1", NEW, "x")), &other))), confirmed("1", NEW), "a known key is all that counts");
    assert_eq!(verdict(&pr, &branches, OLD, Ok((Some(&read("2", OLD, "csv-export")), &head))), PrAddressVerdict::NamesAnotherPr,
        "another key is another pull request, whatever its branch or head");
    assert_eq!(verdict(&pr, &branches, OLD, Ok((None, &head))), PrAddressVerdict::Unreachable, "an answer that names no pull request confirms nothing");
    let gone = GhFailure::NotFound("Could not resolve to a Repository".into());
    assert_eq!(verdict(&pr, &branches, OLD, Err(&gone)), PrAddressVerdict::NotFound);
    for failure in [GhFailure::Exit(Some(1), "API rate limit exceeded".into()), GhFailure::TimedOut("gh pr view".into()), GhFailure::Exit(Some(4), "gh auth login".into())] {
        assert_eq!(verdict(&pr, &branches, OLD, Err(&failure)), PrAddressVerdict::Unreachable, "{failure}");
    }
}

/// T9: the old name of a moved repository that another repository took is not an alias. A record
/// read before only by its address cannot tell it is another pull request's, so nobody can check
/// it; one whose key and locator are known is found again where its repository went.
#[test]
fn a_reused_old_name_is_not_an_alias() {
    alondra_core::clock::VirtualRuntime::new(1_000_000).unwrap().block_on(a_reused_old_name_is_not_an_alias_at_a_virtual_time());
}

async fn a_reused_old_name_is_not_an_alias_at_a_virtual_time() {
    let world = GithubWorld::new();
    world.set_repository(OLD);
    let head = world.open_pr(42, "csv-export", "main");
    world.move_repository(OLD, NEW);
    world.reuse_name(OLD);
    world.open_pr_in(OLD, 42, "unrelated", "develop");
    let (_fixture, domain, pr) = registered("t9-unverified", &head);
    reconcile(&domain, &world, &mut ObservationFailures::default()).await.unwrap();
    let unverified = record(&domain, &pr);
    assert!(matches!(unverified.identity, PrIdentity::Unresolvable { key: None, locator: None, reason: UnresolvableReason::AddressReused, .. }), "{:?}", unverified.identity);
    assert!(unverified.aliases.is_empty(), "the reused name is no alias: {:?}", unverified.aliases);
    assert_eq!((unverified.repository.as_str(), unverified.base.as_str()), (OLD, "main"), "nothing of the other pull request becomes the record's");

    let world = GithubWorld::new();
    world.set_repository(OLD);
    let head = world.open_pr(42, "csv-export", "main");
    let (_fixture, domain, pr) = registered("t9-verified", &head);
    reconcile(&domain, &world, &mut ObservationFailures::default()).await.unwrap();
    assert_eq!(record(&domain, &pr).identity, PrIdentity::Verified { key: world.key_of(42), locator: ProviderLocator(format!("PR_node{}", world.key_of(42).0)) });
    world.move_repository(OLD, NEW);
    world.reuse_name(OLD);
    world.open_pr_in(OLD, 42, "unrelated", "develop");
    alondra_core::clock::advance(std::time::Duration::from_secs(1)).await;
    reconcile(&domain, &world, &mut ObservationFailures::default()).await.unwrap();
    let found = record(&domain, &pr);
    assert_eq!((found.repository.as_str(), found.aliases.iter().map(String::as_str).collect::<Vec<_>>()), (NEW, vec![OLD]),
        "found by its locator where its repository went");
    assert!(found.identity.is_verified() && found.relocation_asked_at.is_none(), "{found:?}");
    assert_eq!(found.base, "main", "the other pull request at the old name never became this one");
    assert!(world.gh_calls().is_empty(), "nothing was changed on GitHub");
}

/// T5: the task of a pull request is cancelled, and before Alondra closes it its repository moves
/// and someone takes the old name with a pull request of the same number. Nothing closes that
/// other pull request: the address answers with another key, so the record is looked for by its
/// locator and only the real pull request, at its new address, is closed.
#[tokio::test]
async fn a_close_never_reaches_a_pr_whose_identity_does_not_match() {
    let world = GithubWorld::new();
    world.set_repository(OLD);
    let head = world.open_pr(42, "csv-export", "main");
    let (_fixture, domain, pr) = registered("t5", &head);
    reconcile(&domain, &world, &mut ObservationFailures::default()).await.unwrap();
    assert!(record(&domain, &pr).identity.is_verified());
    command(&domain, Actor::User { user_id: "u".into() }, "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
    world.move_repository(OLD, NEW);
    world.reuse_name(OLD);
    world.open_pr_in(OLD, 42, "unrelated", "develop");

    let mut failures = ObservationFailures::default();
    reconcile(&domain, &world, &mut failures).await.unwrap();
    assert_eq!(world.state_in(OLD, 42), Some(PrState::Open), "the pull request that took the old name stays open");
    assert!(world.closes().is_empty(), "no close reached any pull request: {:?}", world.gh_calls());
    assert!(world.attempts().is_empty(), "Alondra did not even try to close it with a key GitHub's answer there did not give: {:?}", world.attempts());
    assert_eq!(record(&domain, &pr).repository, NEW, "the record was found where its repository went");

    reconcile(&domain, &world, &mut failures).await.unwrap();
    assert_eq!(world.closes(), [(42, "Cancelled in Alondra: Task".to_owned())]);
    assert_eq!(world.attempts(), [(NEW.to_owned(), 42, world.key_of(42).0)], "the close named the key read at its new address");
    assert_eq!((world.state_in(NEW, 42), world.state_in(OLD, 42)), (Some(PrState::Closed), Some(PrState::Open)));
    assert_eq!(record(&domain, &pr).state, PrState::Closed);
}

/// A network that does not answer changes nothing of a record, and a deleted repository counts
/// definitive answers until, an hour apart each, nobody can check it.
#[tokio::test]
async fn an_unreachable_github_counts_nothing_and_a_deleted_repository_counts_its_answers() {
    let world = GithubWorld::new();
    world.set_repository(OLD);
    let head = world.open_pr(42, "csv-export", "main");
    let (_fixture, domain, pr) = registered("unreachable", &head);
    world.set_unreachable(true);
    for _ in 0..5 {
        reconcile(&domain, &world, &mut ObservationFailures::default()).await.unwrap();
    }
    let untouched = record(&domain, &pr);
    assert!(untouched.identity.is_unverified() && untouched.missing.is_none(), "{untouched:?}");
    world.set_unreachable(false);
    world.delete_repository(OLD);
    reconcile(&domain, &world, &mut ObservationFailures::default()).await.unwrap();
    assert_eq!(record(&domain, &pr).missing.map(|missing| missing.strikes), Some(1));
    let before = domain.state().unwrap().revision;
    reconcile(&domain, &world, &mut ObservationFailures::default()).await.unwrap();
    assert_eq!(domain.state().unwrap().revision, before, "another miss within the hour writes nothing");
    assert!(record(&domain, &pr).tracking == PrTracking::Tracked);
}

/// A repository that is gone costs several calls to GitHub per read: once nobody can check the
/// record, it is read again once an hour, not on every round.
#[test]
fn a_gone_repository_is_read_once_an_hour_not_every_round() {
    alondra_core::clock::VirtualRuntime::new(1_000_000).unwrap().block_on(a_gone_repository_is_read_once_an_hour_at_a_virtual_time());
}

async fn a_gone_repository_is_read_once_an_hour_at_a_virtual_time() {
    let hour = Duration::from_millis(STRIKE_GAP as u64 + 1);
    let world = GithubWorld::new();
    world.set_repository(OLD);
    let head = world.open_pr(42, "csv-export", "main");
    let (_fixture, domain, pr) = registered("gone-hourly", &head);
    let mut failures = ObservationFailures::default();
    world.delete_repository(OLD);
    for _ in 0..GONE_STRIKES {
        alondra_core::clock::sleep(hour).await;
        reconcile(&domain, &world, &mut failures).await.unwrap();
    }
    assert!(matches!(record(&domain, &pr).identity, PrIdentity::Unresolvable { reason: UnresolvableReason::Gone, .. }));
    reconcile(&domain, &world, &mut failures).await.unwrap();
    let read = world.observations().len();
    for _ in 0..5 {
        reconcile(&domain, &world, &mut failures).await.unwrap();
    }
    assert_eq!(world.observations().len(), read, "no read of a repository nobody can reach within the hour");
    alondra_core::clock::sleep(hour).await;
    reconcile(&domain, &world, &mut failures).await.unwrap();
    assert_eq!(world.observations().len(), read + 1, "one read an hour later, in case it came back");
}
