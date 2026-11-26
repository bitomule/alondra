use super::*;
use alondra_engine_ports::github::pr_facts as parse;
use alondra_domain_model::{PrId, TaskId};
use alondra_engine_ports::github::PrObserver as _;

fn pr(number: u64) -> PullRequest {
    PullRequest {
        id: PrId::from("pr"), task_id: TaskId::from("t"), host: "github.com".into(), repository: "owner/repo".into(), number,
        head: String::new(), base: String::new(), state: PrState::Open, merged_head: None, allowed_methods: vec![], review: None, aliases: Default::default(), address_read_at: 0, identity: Default::default(), tracking: Default::default(), missing: None, relocation_asked_at: None,
    }
}

/// PR 7 from `csv` into main, and PR 8 stacked on `csv`.
fn stack() -> (GithubWorld, String, String) {
    let world = GithubWorld::new();
    let parent = world.open_pr(7, "csv", "main");
    let child = world.open_pr(8, "pdf", "csv");
    (world, parent, child)
}

#[tokio::test]
async fn retargeting_a_stacked_pr_records_the_edit_and_moves_its_base() {
    let (world, _, _) = stack();
    world.retarget(&pr(8), &world.key_of(8), "main").await.unwrap();
    assert_eq!(world.gh_calls(), ["edit 8 --base main"]);
    assert_eq!(world.retargets(), [(8, "main".to_owned())]);
    assert_eq!(world.base_of(8), "main");
    assert_eq!(world.observe("github.com", "owner/repo", 8).await.unwrap().base, "main");
    assert_eq!(world.status(&pr(8)).await.unwrap().base, "main");
}

#[tokio::test]
async fn a_closed_pr_cannot_be_retargeted_but_the_attempt_is_recorded() {
    let (world, _, _) = stack();
    world.close_externally(8);
    assert!(world.retarget(&pr(8), &world.key_of(8), "main").await.is_err());
    assert_eq!(world.gh_calls(), ["edit 8 --base main"]);
    assert_eq!(world.base_of(8), "csv");
}

#[tokio::test]
async fn a_pr_merged_on_a_stale_base_lands_there_as_github_lets_it() {
    let (world, parent, child) = stack();
    world.merge(&pr(7), &world.key_of(7), &parent, MergeMethod::Squash).await.unwrap();
    world.merge(&pr(8), &world.key_of(8), &child, MergeMethod::Squash).await.unwrap();
    assert_eq!(world.merged_into(7).as_deref(), Some("main"));
    assert_eq!(world.merged_into(8).as_deref(), Some("csv"), "the child merged into the branch of the parent, which main never receives");
    assert_eq!(world.state_of(8), PrState::Merged);
    assert_eq!(world.observe("github.com", "owner/repo", 8).await.unwrap().state, PrState::Merged);
    assert_eq!(world.gh_calls(), ["merge 7", "merge 8"]);
}

#[tokio::test]
async fn a_pr_retargeted_before_merging_lands_in_the_default_branch() {
    let (world, parent, child) = stack();
    world.merge(&pr(7), &world.key_of(7), &parent, MergeMethod::Squash).await.unwrap();
    world.retarget(&pr(8), &world.key_of(8), "main").await.unwrap();
    world.merge(&pr(8), &world.key_of(8), &child, MergeMethod::Squash).await.unwrap();
    assert_eq!(world.merged_into(8).as_deref(), Some("main"));
    assert_eq!(world.gh_calls(), ["merge 7", "edit 8 --base main", "merge 8"]);
}

#[tokio::test]
async fn github_moves_dependants_of_a_merged_branch_only_when_it_deletes_the_branch() {
    for deletes in [false, true] {
        let (world, parent, _) = stack();
        world.delete_head_on_merge(deletes);
        world.merge(&pr(7), &world.key_of(7), &parent, MergeMethod::Squash).await.unwrap();
        assert_eq!(world.base_of(8), if deletes { "main" } else { "csv" });
        assert!(world.retargets().is_empty(), "what GitHub does itself is not a call of Alondra's");
    }
}

#[tokio::test]
async fn a_merge_is_refused_when_the_head_moved_the_pr_is_closed_or_conflicts() {
    let (world, parent, _) = stack();
    world.push_commit(7);
    assert!(world.merge(&pr(7), &world.key_of(7), &parent, MergeMethod::Squash).await.is_err(), "the head is no longer the one asked");
    let head = world.status(&pr(7)).await.unwrap().head;
    world.set_merge_state(7, MergeState::Conflicting);
    assert!(world.merge(&pr(7), &world.key_of(7), &head, MergeMethod::Squash).await.is_err());
    world.set_merge_state(7, MergeState::Clean);
    world.close_externally(7);
    assert!(world.merge(&pr(7), &world.key_of(7), &head, MergeMethod::Squash).await.is_err());
    assert!(world.merges().is_empty());
}

#[tokio::test]
async fn another_person_merging_or_closing_is_what_the_observer_reports() {
    let (world, parent, _) = stack();
    world.merge_externally(7);
    world.close_externally(8);
    let merged = world.observe("github.com", "owner/repo", 7).await.unwrap();
    assert_eq!((merged.state, merged.merged_head), (PrState::Merged, Some(parent)));
    assert_eq!(world.observe("github.com", "owner/repo", 8).await.unwrap().state, PrState::Closed);
    assert!(world.gh_calls().is_empty(), "nothing here was Alondra's call");
}

#[tokio::test]
async fn closing_through_the_observer_leaves_the_comment_and_a_closed_pr() {
    let (world, _, _) = stack();
    world.close("github.com", "owner/repo", 8, &world.key_of(8), "Superseded").await.unwrap();
    assert_eq!(world.closes(), [(8, "Superseded".to_owned())]);
    assert_eq!(world.state_of(8), PrState::Closed);
    assert_eq!(world.gh_calls(), ["close 8"]);
}

#[tokio::test]
async fn the_view_reads_back_as_the_same_pull_request_the_observer_reports() {
    let (world, _, child) = stack();
    world.set_checks(8, Checks::Failed(vec!["lint".into()]));
    let view = world.view("https://github.com/owner/repo/pull/8").await.unwrap();
    let read = parse(&view).unwrap();
    let observed = world.observe("github.com", "owner/repo", 8).await.unwrap();
    assert_eq!((read.head.as_str(), read.base.as_str(), read.state), (child.as_str(), "csv", PrState::Open));
    assert_eq!((read.mergeable, read.checks), (observed.mergeable, observed.checks));
    assert_eq!(view["headRefName"], "pdf");
    assert!(world.view("https://github.com/owner/repo/pull/99").await.is_none());
}

#[tokio::test]
async fn the_description_the_images_are_read_from_is_the_one_the_view_shows() {
    let (world, _, _) = stack();
    world.set_body(8, "Before and after");
    assert_eq!(world.body_html("github.com", "owner/repo", 8).await.unwrap(), "Before and after");
    assert_eq!(world.view("https://github.com/owner/repo/pull/8").await.unwrap()["body"], "Before and after");
    assert!(world.body_html("github.com", "owner/repo", 99).await.is_err());
}

/// What an answer of GitHub says about which pull request is at an address, in the terms the
/// adapter decides on: who answered and where it lives now, nothing for certain, or no answer.
#[derive(Debug, PartialEq, Eq)]
enum Answered {
    /// The pull request known before (`same`) or another one, living now at `repository`.
    PullRequest { same: bool, repository: String },
    NotFound,
    NoAnswer,
}

fn answered(read: Result<ReadIdentity, GhFailure>, known: &ProviderPrKey) -> Answered {
    match read {
        Ok(read) => Answered::PullRequest { same: read.key == *known, repository: read.repository },
        Err(failure) if failure.not_found() => Answered::NotFound,
        Err(_) => Answered::NoAnswer,
    }
}

/// The contract of the GitHub double: for every situation the identity of a pull request depends
/// on (a moved repository, its old name taken by another one, a deleted repository, a number that
/// is not there, a network that does not answer, a pull request found by its node), the double
/// answers what `gh` answers on GitHub, as the adapter reads it. GitHub's answers are the ones
/// read on 22 November 2026 for a moved public repository, with invented names.
#[tokio::test]
async fn the_github_double_answers_identity_as_github_does() {
    const KEY: &str = "15890129";
    let view = |key: &str, url: &str| -> Result<ReadIdentity, GhFailure> {
        let value = serde_json::json!({"state": "OPEN", "headRefOid": "1".repeat(40), "baseRefName": "main", "headRefName": "csv-export",
            "fullDatabaseId": key, "id": "PR_kwDOAOXXVc4A8nbR", "url": url});
        Ok(parse(&value).unwrap().identity.unwrap())
    };
    let gone = |message: &str| -> Result<ReadIdentity, GhFailure> {
        let body = serde_json::json!({"data": {"repository": null}, "errors": [{"type": "NOT_FOUND", "message": message}]});
        let answer = alondra_engine_ports::github::HttpAnswer { status: 200, headers: Default::default(), body: body.to_string().into_bytes() };
        Err(GhFailure::NotFound(alondra_github_gh::cli::missing(&answer).unwrap()))
    };
    let github = ProviderPrKey(KEY.into());
    let situations: [(&str, Result<ReadIdentity, GhFailure>, fn(&GithubWorld)); 5] = [
        ("moved", view(KEY, "https://github.com/lumo-org/lumo/pull/42"), |world| world.move_repository("ana/lumo", "lumo-org/lumo")),
        ("old name taken", view("15890777", "https://github.com/ana/lumo/pull/42"), |world| {
            world.move_repository("ana/lumo", "lumo-org/lumo");
            world.reuse_name("ana/lumo");
            world.open_pr_in("ana/lumo", 42, "other", "main");
        }),
        ("deleted", gone("Could not resolve to a Repository with the name 'ana/lumo'."), |world| world.delete_repository("ana/lumo")),
        ("old name taken, no such number", gone("Could not resolve to a PullRequest with the number of 42."), |world| {
            world.move_repository("ana/lumo", "lumo-org/lumo");
            world.reuse_name("ana/lumo");
        }),
        ("no network", Err(GhFailure::Exit(Some(1), "error connecting to api.github.com".into())), |world| world.set_unreachable(true)),
    ];
    for (situation, real, change) in situations {
        let world = GithubWorld::new();
        world.set_repository("ana/lumo");
        world.open_pr(42, "csv-export", "main");
        let known = world.key_of(42);
        change(&world);
        let double = world.observe("github.com", "ana/lumo", 42).await
            .and_then(|facts| facts.identity.ok_or_else(|| GhFailure::Unreadable("no identity".into())));
        assert_eq!(answered(double, &known), answered(real, &github), "{situation}");
    }

    let node = |body: serde_json::Value| alondra_github_gh::cli::located(&alondra_engine_ports::github::HttpAnswer { status: 200, headers: Default::default(),
        body: body.to_string().into_bytes() });
    let found = node(serde_json::json!({"data": {"node": {"fullDatabaseId": KEY, "id": "PR_kwDOAOXXVc4A8nbR", "url": "https://github.com/lumo-org/lumo/pull/42",
        "headRefName": "csv-export"}}}));
    let gone = node(serde_json::json!({"data": {"node": null}, "errors": [{"type": "NOT_FOUND", "message": "Could not resolve to a node"}]}));
    let world = GithubWorld::new();
    world.set_repository("ana/lumo");
    world.open_pr(42, "csv-export", "main");
    let (known, locator) = {
        let read = world.observe("github.com", "ana/lumo", 42).await.unwrap().identity.unwrap();
        (read.key, read.locator)
    };
    world.move_repository("ana/lumo", "lumo-org/lumo");
    assert_eq!(answered(world.relocate("github.com", &locator).await, &known), answered(found, &github), "found by its node");
    world.delete_repository("lumo-org/lumo");
    assert_eq!(answered(world.relocate("github.com", &locator).await, &known), answered(gone, &github), "a node that is gone");
}

/// A change aimed at one pull request never reaches another one at the same address.
#[tokio::test]
async fn a_change_for_another_key_changes_nothing() {
    let world = GithubWorld::new();
    world.set_repository("ana/lumo");
    let head = world.open_pr(42, "csv-export", "main");
    let stale = world.key_of(42);
    world.move_repository("ana/lumo", "lumo-org/lumo");
    world.reuse_name("ana/lumo");
    let other = world.open_pr_in("ana/lumo", 42, "other", "main");
    let mut at_old = pr(42);
    at_old.repository = "ana/lumo".into();
    for refused in [
        world.close("github.com", "ana/lumo", 42, &stale, "Cancelled").await,
        world.merge(&at_old, &stale, &other, MergeMethod::Squash).await,
        world.retarget(&at_old, &stale, "develop").await,
        world.ready(&at_old, &stale).await,
    ] {
        assert!(matches!(refused, Err(GhFailure::IdentityMismatch(_))), "{refused:?}");
    }
    assert!(world.gh_calls().is_empty() && world.closes().is_empty());
    assert_eq!(world.state_in("ana/lumo", 42), Some(PrState::Open));
    let mut at_new = pr(42);
    at_new.repository = "lumo-org/lumo".into();
    world.merge(&at_new, &stale, &head, MergeMethod::Squash).await.unwrap();
    assert_eq!(world.state_in("lumo-org/lumo", 42), Some(PrState::Merged));
}
