use super::*;
use crate::domain_github::parse;
use alondra_core::domain::{PrId, TaskId};

fn pr(number: u64) -> PullRequest {
    PullRequest {
        id: PrId::from("pr"), task_id: TaskId::from("t"), host: "github.com".into(), repository: "owner/repo".into(), number,
        head: String::new(), base: String::new(), state: PrState::Open, merged_head: None, allowed_methods: vec![], review: None,
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
    world.retarget(&pr(8), "main").await.unwrap();
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
    assert!(world.retarget(&pr(8), "main").await.is_err());
    assert_eq!(world.gh_calls(), ["edit 8 --base main"]);
    assert_eq!(world.base_of(8), "csv");
}

#[tokio::test]
async fn a_pr_merged_on_a_stale_base_lands_there_as_github_lets_it() {
    let (world, parent, child) = stack();
    world.merge(&pr(7), &parent, MergeMethod::Squash).await.unwrap();
    world.merge(&pr(8), &child, MergeMethod::Squash).await.unwrap();
    assert_eq!(world.merged_into(7).as_deref(), Some("main"));
    assert_eq!(world.merged_into(8).as_deref(), Some("csv"), "the child merged into the branch of the parent, which main never receives");
    assert_eq!(world.state_of(8), PrState::Merged);
    assert_eq!(world.observe("github.com", "owner/repo", 8).await.unwrap().state, PrState::Merged);
    assert_eq!(world.gh_calls(), ["merge 7", "merge 8"]);
}

#[tokio::test]
async fn a_pr_retargeted_before_merging_lands_in_the_default_branch() {
    let (world, parent, child) = stack();
    world.merge(&pr(7), &parent, MergeMethod::Squash).await.unwrap();
    world.retarget(&pr(8), "main").await.unwrap();
    world.merge(&pr(8), &child, MergeMethod::Squash).await.unwrap();
    assert_eq!(world.merged_into(8).as_deref(), Some("main"));
    assert_eq!(world.gh_calls(), ["merge 7", "edit 8 --base main", "merge 8"]);
}

#[tokio::test]
async fn github_moves_dependants_of_a_merged_branch_only_when_it_deletes_the_branch() {
    for deletes in [false, true] {
        let (world, parent, _) = stack();
        world.delete_head_on_merge(deletes);
        world.merge(&pr(7), &parent, MergeMethod::Squash).await.unwrap();
        assert_eq!(world.base_of(8), if deletes { "main" } else { "csv" });
        assert!(world.retargets().is_empty(), "what GitHub does itself is not a call of Alondra's");
    }
}

#[tokio::test]
async fn a_merge_is_refused_when_the_head_moved_the_pr_is_closed_or_conflicts() {
    let (world, parent, _) = stack();
    world.push_commit(7);
    assert!(world.merge(&pr(7), &parent, MergeMethod::Squash).await.is_err(), "the head is no longer the one asked");
    let head = world.status(&pr(7)).await.unwrap().head;
    world.set_merge_state(7, MergeState::Conflicting);
    assert!(world.merge(&pr(7), &head, MergeMethod::Squash).await.is_err());
    world.set_merge_state(7, MergeState::Clean);
    world.close_externally(7);
    assert!(world.merge(&pr(7), &head, MergeMethod::Squash).await.is_err());
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
    world.close("github.com", "owner/repo", 8, "Superseded").await.unwrap();
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
