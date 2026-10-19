use super::*;
use alondra_core::domain::*;
use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

struct Home(crate::test_support::Directory);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(),
        expected_revision: state.revision, command,
    }, None, || Ok(())).unwrap();
}

/// The task waits in Closing for its node; Alondra records it closed and the task is done.
fn end_node(domain: &DomainRuntime) {
    assert_eq!(task(domain), TaskState::Closing);
    command(domain, Actor::System, "node-end", Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: None });
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn targets() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEAD.into() }]
}

fn now() -> Merger {
    let mut merger = Merger::with(Duration::ZERO, Duration::from_secs(3600));
    merger.first_retry = Duration::ZERO;
    merger
}

/// A node's Task with its PR #7 presented as the final result, which GitHub reports open with
/// its checks passed.
fn presented(_tag: &str, auto_merge: bool) -> (Home, Arc<DomainRuntime>, FakeGithub) {
    let home = Home(crate::test_support::Directory::new());
    std::fs::create_dir(&home.0).unwrap();
    let domain = Arc::new(DomainRuntime::open(&home.0).unwrap());
    command(&domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
    command(&domain, user(), "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge, maintenance: false, message: false });
    command(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None,
        task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()),
        review_prs: true, dependencies: BTreeSet::new(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
    });
    command(&domain, Actor::Agent { agent_id: "a".into() }, "pr", Command::RegisterPr {
        task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
        number: 7, head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash, MergeMethod::Merge],
    });
    command(&domain, Actor::Agent { agent_id: "a".into() }, "present", Command::PresentResult {
        checklist: Vec::new(),
        attachments: vec![], task_id: "t".into(), interaction_id: "d".into(), body: "Ready".into(),
        targets: targets(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
    });
    let github = FakeGithub::new();
    github.open(7, HEAD, Checks::Passed);
    (home, domain, github)
}

fn accept(domain: &DomainRuntime) {
    command(domain, user(), "accept", Command::AcceptDelivery { presentation_id: "d".into(), targets: targets() });
}

fn told(domain: &DomainRuntime) -> Vec<String> {
    domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
        .filter(|message| message.agent_id.as_deref() == Some("a")).map(|message| message.message).collect()
}

fn task(domain: &DomainRuntime) -> TaskState {
    domain.state().unwrap().tasks[&TaskId::from("t")].state
}

fn feedback(domain: &DomainRuntime) -> Vec<String> {
    match &domain.state().unwrap().interactions[&InteractionId::from("d")].payload {
        InteractionPayload::Delivery { feedback, .. } => feedback.iter().map(|f| f.body.clone()).collect(),
        _ => vec![],
    }
}

fn accepted(domain: &DomainRuntime) -> bool {
    matches!(&domain.state().unwrap().interactions[&InteractionId::from("d")].payload,
        InteractionPayload::Delivery { acceptance: Some(a), .. } if a.valid)
}

fn problems(domain: &DomainRuntime) -> Vec<Interaction> {
    domain.state().unwrap().interactions.values().cloned().filter(|card| card.card.kind == CardKind::Problem).collect()
}

#[tokio::test]
async fn an_accepted_delivery_is_merged_by_alondra_and_the_node_is_only_asked_to_clean_up() {
    let (_home, domain, github) = presented("accept", false);
    let mut merger = now();
    assert!(!merger.merge_due(&domain, &github).await.unwrap());
    assert!(github.merged.lock().unwrap().is_empty(), "nothing is merged before the user accepts");
    accept(&domain);
    assert_eq!(task(&domain), TaskState::WaitingDependency, "accepted, waiting for Alondra's merge");
    merger.merge_due(&domain, &github).await.unwrap();
    assert_eq!(*github.merged.lock().unwrap(), vec![(7, HEAD.to_owned(), MergeMethod::Squash)]);
    let state = domain.state().unwrap();
    assert_eq!(state.pull_requests[&PrId::from("pr")].merged_head.as_deref(), Some(HEAD));
    assert_eq!(state.merge_authorizations.values().next().unwrap().attempt, MergeAttemptState::Confirmed);
    end_node(&domain);
    assert_eq!(task(&domain), TaskState::Done);
    assert_eq!(told(&domain), vec![cleanup_request("Task", &"t".into(), &[])]);
    merger.merge_due(&domain, &github).await.unwrap();
    assert_eq!(github.merged.lock().unwrap().len(), 1, "a merged PR is not merged again");
}

#[tokio::test]
async fn the_users_merge_button_is_merged_with_the_method_they_chose() {
    let (_home, domain, github) = presented("authorize", false);
    command(&domain, user(), "authorize", Command::AuthorizeMerge { authorization_id: "auth".into(),
        presentation_id: "d".into(), pr_id: "pr".into(), expected_head: HEAD.into(), method: MergeMethod::Merge });
    now().merge_due(&domain, &github).await.unwrap();
    assert_eq!(*github.merged.lock().unwrap(), vec![(7, HEAD.to_owned(), MergeMethod::Merge)]);
    end_node(&domain);
    assert_eq!(task(&domain), TaskState::Done);
}

#[tokio::test]
async fn auto_merge_waits_for_the_checks_then_claims_durably_and_merges() {
    let (_home, domain, github) = presented("auto", true);
    github.open(7, HEAD, Checks::Pending);
    let mut merger = now();
    assert!(merger.merge_due(&domain, &github).await.unwrap(), "pending checks are looked at again");
    assert!(github.merged.lock().unwrap().is_empty());
    assert!(domain.state().unwrap().merge_authorizations.is_empty(), "nothing claimed while checks run");
    assert_eq!(task(&domain), TaskState::ForYourReview);
    github.open(7, HEAD, Checks::Passed);
    assert!(!merger.merge_due(&domain, &github).await.unwrap());
    assert_eq!(*github.merged.lock().unwrap(), vec![(7, HEAD.to_owned(), MergeMethod::Squash)]);
    let state = domain.state().unwrap();
    let authorization = state.merge_authorizations.values().next().unwrap();
    assert_eq!((authorization.user_id.as_str(), authorization.attempt), (Feedback::ALONDRA, MergeAttemptState::Confirmed));
    assert!(state.pull_requests[&PrId::from("pr")].review.as_ref().unwrap().decisions.is_empty(), "no review is made up");
    end_node(&domain);
    assert_eq!(task(&domain), TaskState::Done);
    assert_eq!(told(&domain), vec![cleanup_request("Task", &"t".into(), &[])]);
}

/// PR #765 was accepted and never merged: its task waited on another one only to commit, and
/// every merge step was refused with dependency_not_done.
#[tokio::test]
async fn a_task_that_waited_on_another_to_start_has_its_accepted_pr_merged() {
    let (_home, domain, github) = presented("waited", false);
    command(&domain, user(), "title", Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
    command(&domain, Actor::Orchestrator { generation: 0 }, "fix", Command::CreateTask { plan_items: None,
        task_id: "fix".into(), request_id: "r".into(), title: "Fix main".into(), agent_id: None,
        review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
    });
    command(&domain, Actor::Orchestrator { generation: 0 }, "wait", Command::SetDependencies {
        task_id: "t".into(), dependencies: BTreeSet::from(["fix".into()]), stack: None,
    });
    accept(&domain);
    now().merge_due(&domain, &github).await.unwrap();
    assert_eq!(*github.merged.lock().unwrap(), vec![(7, HEAD.to_owned(), MergeMethod::Squash)]);
    end_node(&domain);
    assert_eq!(task(&domain), TaskState::Done);
}

#[tokio::test]
async fn an_auto_merge_waits_for_the_task_that_reviews_it_and_merges_once_it_approves() {
    let (_home, domain, github) = presented("reviewed", true);
    let reviewer = || Actor::Agent { agent_id: "rev".into() };
    command(&domain, Actor::System, "reviewer-agent", Command::RegisterAgent { agent_id: "rev".into(), name: "Reviewer".into(), at: 0 });
    command(&domain, user(), "title", Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
    command(&domain, Actor::Orchestrator { generation: 0 }, "reviewer", Command::CreateTask { plan_items: None,
        task_id: "review".into(), request_id: "r".into(), title: "Review".into(), agent_id: Some("rev".into()),
        review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
    });
    command(&domain, user(), "relations", Command::SetRelations {
        task_id: "review".into(), delivered_dependencies: BTreeSet::from(["t".into()]), blocks: BTreeSet::from(["t".into()]),
    });
    command(&domain, reviewer(), "start", Command::StartTask { task_id: "review".into() });
    command(&domain, reviewer(), "present-review", Command::PresentResult {
        checklist: Vec::new(),
        attachments: vec![], task_id: "review".into(), interaction_id: "rd".into(), body: "Looked at it".into(),
        targets: vec![DeliveryTarget::Text { text: "The review".into() }], is_final: true,
        nothing_to_review: false,
        card: CardSpec::delivery("Review the result", &[], true),
    });
    let mut merger = now();
    assert!(!merger.merge_due(&domain, &github).await.unwrap());
    assert!(github.merged.lock().unwrap().is_empty(), "nothing is merged while its review is open");
    assert!(domain.state().unwrap().merge_authorizations.is_empty());
    command(&domain, reviewer(), "approve", Command::RecordVerdict {
        task_id: "review".into(), presentation_id: "rd".into(), verdict: Verdict::Approved { summary: "fine".into(), open_issues: false, serious_findings: 0 },
    });
    merger.merge_due(&domain, &github).await.unwrap();
    assert_eq!(*github.merged.lock().unwrap(), vec![(7, HEAD.to_owned(), MergeMethod::Squash)], "approved: auto-merge goes ahead");
}

#[tokio::test]
async fn an_auto_merge_goes_through_the_claims_checks() {
    let (_home, domain, github) = presented("auto-base", true);
    github.statuses.lock().unwrap().insert(7, Ok(Status { state: PrState::Open, head: HEAD.into(), base: "develop".into(), checks: Checks::Passed, merge_state: MergeState::Clean }));
    assert!(now().merge_due(&domain, &github).await.unwrap());
    assert!(github.merged.lock().unwrap().is_empty(), "GitHub's base differs from the registered one: the claim refuses");
    assert_eq!(domain.state().unwrap().merge_authorizations.values().next().unwrap().attempt, MergeAttemptState::Unclaimed);
}

#[tokio::test]
async fn feedback_given_while_alondra_reads_github_stops_the_merge() {
    let (_home, domain, github) = presented("raced", false);
    accept(&domain);
    let racing = domain.clone();
    *github.on_read.lock().unwrap() = Some(Box::new(move || command(&racing, user(), "changes", Command::SubmitFeedback {
        attachments: vec![], presentation_id: "d".into(), targets: targets(), body: "One more fix".into(), instruction: Instruction::Changes,
    })));
    now().merge_due(&domain, &github).await.unwrap();
    assert!(github.merged.lock().unwrap().is_empty());
    assert_eq!(task(&domain), TaskState::Running);
}

#[tokio::test]
async fn a_cancel_after_alondra_claimed_the_merge_stops_it() {
    let (_home, domain, github) = presented("cancelled", false);
    github.merges.lock().unwrap().insert(7, Err("Required status check \"build\" is expected.".into()));
    accept(&domain);
    let mut merger = now();
    merger.merge_due(&domain, &github).await.unwrap();
    assert_eq!(domain.state().unwrap().merge_authorizations.values().next().unwrap().attempt, MergeAttemptState::Claimed);
    github.merges.lock().unwrap().remove(&7);
    let racing = domain.clone();
    *github.on_read.lock().unwrap() = Some(Box::new(move || command(&racing, user(), "cancel", Command::CancelTask { task_id: "t".into() })));
    merger.merge_due(&domain, &github).await.unwrap();
    assert!(github.merged.lock().unwrap().is_empty(), "the delivery is read again right before merging");
}

/// A repo with no CI and no required checks: GitHub says CLEAN and nothing is waited for.
#[tokio::test]
async fn no_required_checks_and_a_clean_pr_merge_at_once() {
    let (_home, domain, github) = presented("no-checks", true);
    github.open(7, HEAD, Checks::None);
    assert!(!now().merge_due(&domain, &github).await.unwrap(), "nothing waits");
    assert_eq!(github.merged.lock().unwrap().len(), 1);
    assert!(!github.calls.lock().unwrap().iter().any(|call| call.starts_with("required")), "CLEAN needs no look at the rules");
}

#[tokio::test]
async fn pending_required_checks_that_have_not_reported_make_it_wait() {
    let (_home, domain, github) = presented("required-pending", true);
    github.open_blocked(7, HEAD, Checks::None);
    github.required.lock().unwrap().insert(7, Ok(vec!["build".into()]));
    assert!(now().merge_due(&domain, &github).await.unwrap());
    assert!(github.merged.lock().unwrap().is_empty());
    assert!(!github.calls.lock().unwrap().iter().any(|call| call.starts_with("merge")), "merge is not even tried");
}

#[tokio::test]
async fn blocked_without_required_checks_is_left_to_githubs_answer() {
    let (_home, domain, github) = presented("blocked-other", true);
    github.open_blocked(7, HEAD, Checks::None);
    github.merges.lock().unwrap().insert(7, Err("Pull request #7 is not mergeable: the base branch policy prohibits the merge. A review is required".into()));
    let mut merger = Merger::with(Duration::from_secs(3600), Duration::from_secs(3600));
    merger.merge_due(&domain, &github).await.unwrap();
    assert!(github.calls.lock().unwrap().iter().any(|call| call == "merge 7"), "no required check holds it, so the merge is tried");
}

#[tokio::test]
async fn rules_github_does_not_answer_make_it_wait_instead_of_merging_blind() {
    let (_home, domain, github) = presented("rules-unreadable", true);
    github.open_blocked(7, HEAD, Checks::None);
    github.required.lock().unwrap().insert(7, Err("error connecting to api.github.com".into()));
    assert!(now().merge_due(&domain, &github).await.unwrap());
    assert!(github.merged.lock().unwrap().is_empty());
}

/// A `gh` that answers like GitHub does for a private repository without a paid plan: no
/// rulesets, and a 403 for the classic branch protection. `view` is its answer to `gh pr view`
/// until `gh pr merge` runs; after that the PR reads as merged.
fn private_repo_gh(home: &Home, view: &str) -> (GhCli, std::path::PathBuf) {
    let program = home.0.join("gh");
    let log = home.0.join("calls");
    let merged = home.0.join("merged");
    std::fs::write(&program, format!(r#"#!/bin/sh
printf '%s\n' "$*" >> {log}
case "$*" in
  *protection*) echo 'gh: Upgrade to GitHub Pro or make this repository public to enable this feature. (HTTP 403)' >&2; exit 1;;
  *rules/branches*) echo '[]';;
  *isDraft*) echo false;;
  *"pr merge"*) touch {merged};;
  *statusCheckRollup*) if [ -f {merged} ]; then printf '%s' '{{"state":"MERGED","headRefOid":"{HEAD}","baseRefName":"main","statusCheckRollup":[]}}'; else printf '%s' '{view}'; fi;;
esac
"#, log = log.display(), merged = merged.display())).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    (GhCli::at(program), log)
}

fn private_repo_pr(home: &Home, merge_state: &str, mergeable: &str) -> (GhCli, std::path::PathBuf) {
    private_repo_gh(home, &format!(r#"{{"state":"OPEN","headRefOid":"{HEAD}","baseRefName":"main","statusCheckRollup":[],"mergeable":"{mergeable}","mergeStateStatus":"{merge_state}"}}"#))
}

#[tokio::test]
async fn an_accepted_pr_is_not_closing_until_github_says_it_is_merged() {
    let (_home, domain, github) = presented("closing-after-merge", false);
    github.open(7, HEAD, Checks::Pending);
    accept(&domain);
    let mut merger = now();
    assert!(merger.merge_due(&domain, &github).await.unwrap(), "the merge waits for the checks");
    assert_ne!(task(&domain), TaskState::Closing, "accepted is not merged");
    let state = domain.state().unwrap();
    assert_eq!(state.attention(&state.tasks[&TaskId::from("t")]), Attention::Wait(WaitReason::Merging { pr: 7 }));
    github.open(7, HEAD, Checks::Passed);
    merger.merge_due(&domain, &github).await.unwrap();
    assert_eq!(task(&domain), TaskState::Closing, "GitHub confirmed the merge");
}

/// The #778 case: accepted while it conflicted with #772, with no checks run and the base's
/// branch protection unreadable (403).
#[tokio::test]
async fn a_conflicting_pr_goes_back_to_its_node_to_resolve_conflicts_with_main() {
    for (merge_state, mergeable) in [("DIRTY", "CONFLICTING"), ("DIRTY", "UNKNOWN"), ("UNKNOWN", "CONFLICTING")] {
        let (home, domain, _) = presented("conflicting", false);
        let (gh, log) = private_repo_pr(&home, merge_state, mergeable);
        accept(&domain);
        let mut merger = Merger::with(Duration::from_secs(3600), Duration::from_secs(3600));
        assert!(!merger.merge_due(&domain, &gh).await.unwrap(), "nothing is left waiting ({merge_state}, {mergeable})");
        assert_eq!(task(&domain), TaskState::Running, "the node resolves the conflicts");
        let feedback = feedback(&domain);
        assert!(feedback.len() == 1 && feedback[0].contains("conflicts with main"), "{feedback:?}");
        merger.merge_due(&domain, &gh).await.unwrap();
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(!calls.contains("pr merge") && !calls.contains("protection"), "{calls}");
        assert_eq!(self::feedback(&domain).len(), 1, "handed back once");
    }
}

#[tokio::test]
async fn branch_protection_github_refuses_to_show_counts_as_no_required_checks() {
    let home = Home(crate::test_support::Directory::new());
    std::fs::create_dir(&home.0).unwrap();
    let (gh, _) = private_repo_gh(&home, "{}");
    let pr = PullRequest { id: "pr".into(), task_id: "t".into(), host: "github".into(), repository: "owner/repo".into(),
        number: 7, head: HEAD.into(), base: "main".into(), state: PrState::Open, merged_head: None,
        allowed_methods: vec![MergeMethod::Squash], review: None };
    assert_eq!(gh.required_checks(&pr, "main").await, Ok(vec![]));
}

/// A blocked PR with no checks in a private repository without a paid plan: the 403 on its
/// branch protection does not hold the merge, which is decided on the first pass.
#[tokio::test]
async fn a_403_on_branch_protection_is_decided_by_the_merge_state_and_never_retried() {
    let (home, domain, _) = presented("protection-403", false);
    let (gh, log) = private_repo_pr(&home, "BLOCKED", "MERGEABLE");
    accept(&domain);
    let mut merger = Merger::with(Duration::from_secs(3600), Duration::from_secs(3600));
    assert!(!merger.merge_due(&domain, &gh).await.unwrap(), "nothing is left waiting");
    assert_eq!(domain.state().unwrap().pull_requests[&PrId::from("pr")].state, PrState::Merged);
    assert_eq!(task(&domain), TaskState::Closing);
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(calls.matches("protection").count(), 1, "{calls}");
}

#[test]
fn required_checks_come_from_rulesets_and_from_classic_protection() {
    let rules = serde_json::json!([
        {"type": "pull_request", "parameters": {}},
        {"type": "required_status_checks", "parameters": {"required_status_checks": [{"context": "build"}, {"context": "lint", "integration_id": 1}]}}
    ]);
    assert_eq!(ruleset_checks(&rules), vec!["build", "lint"]);
    assert!(ruleset_checks(&serde_json::json!([{"type": "non_fast_forward"}])).is_empty());
    let classic = serde_json::json!({"contexts": ["test"], "checks": [{"context": "test", "app_id": null}, {"context": "ui", "app_id": 3}]});
    assert_eq!(classic_checks(&classic), vec!["test", "test", "ui"]);
}

/// The #744 sequence: accepted (Alondra waits on the checks), then Merge pressed again with another
/// method. The press must be looked at at once, not after the wait the first one left behind.
#[tokio::test]
async fn pressing_merge_again_while_the_checks_wait_is_acted_on_at_once() {
    let (_home, domain, github) = presented("pressed", false);
    github.open(7, HEAD, Checks::Pending);
    accept(&domain);
    assert_eq!(task(&domain), TaskState::WaitingDependency);
    let mut merger = Merger::with(Duration::from_secs(3600), Duration::from_secs(3600));
    assert!(merger.merge_due(&domain, &github).await.unwrap(), "the checks are pending: Alondra waits");
    github.open(7, HEAD, Checks::Passed);
    assert!(merger.merge_due(&domain, &github).await.unwrap(), "the same authorization waits out its poll");
    assert!(github.merged.lock().unwrap().is_empty());
    command(&domain, user(), "again", Command::AuthorizeMerge { authorization_id: "again".into(),
        presentation_id: "d".into(), pr_id: "pr".into(), expected_head: HEAD.into(), method: MergeMethod::Merge });
    merger.merge_due(&domain, &github).await.unwrap();
    assert_eq!(*github.merged.lock().unwrap(), vec![(7, HEAD.to_owned(), MergeMethod::Merge)],
        "the new press merges with its method without waiting for the next poll");
    merger.merge_due(&domain, &github).await.unwrap();
    assert_eq!(merger.tracked(), 0, "and nothing is left behind once it is merged");
}

#[tokio::test]
async fn a_restarted_merger_picks_up_an_authorization_it_never_saw() {
    let (_home, domain, github) = presented("restart", false);
    accept(&domain);
    now().merge_due(&domain, &github).await.unwrap();
    assert_eq!(github.merged.lock().unwrap().len(), 1, "a fresh merger needs nothing but the domain to merge what was authorized");
}

#[tokio::test]
async fn github_waiting_for_required_checks_keeps_the_acceptance() {
    let (_home, domain, github) = presented("required", false);
    github.merges.lock().unwrap().insert(7, Err("Pull request #7 is not mergeable: Required status check \"build\" is expected.".into()));
    accept(&domain);
    let mut merger = now();
    assert!(merger.merge_due(&domain, &github).await.unwrap());
    assert!(merger.merge_due(&domain, &github).await.unwrap());
    assert!(feedback(&domain).is_empty() && accepted(&domain));
    assert_eq!(task(&domain), TaskState::WaitingDependency);
    github.merges.lock().unwrap().remove(&7);
    merger.merge_due(&domain, &github).await.unwrap();
    end_node(&domain);
    assert_eq!(task(&domain), TaskState::Done, "Alondra's own claim carries on after a restart-free wait");
}

#[tokio::test]
async fn a_merge_queue_is_waited_on_and_only_githubs_merged_counts() {
    let (_home, domain, github) = presented("queue", false);
    github.queue.lock().unwrap().insert(7, true);
    accept(&domain);
    let mut merger = now();
    assert!(merger.merge_due(&domain, &github).await.unwrap());
    assert_eq!(github.merged.lock().unwrap().len(), 1);
    assert_eq!(domain.state().unwrap().pull_requests[&PrId::from("pr")].state, PrState::Open, "queued is not merged");
    assert!(merger.merge_due(&domain, &github).await.unwrap());
    assert_eq!(github.merged.lock().unwrap().len(), 1, "a queued PR is not merged again");
    github.statuses.lock().unwrap().get_mut(&7).unwrap().as_mut().unwrap().state = PrState::Merged;
    merger.merge_due(&domain, &github).await.unwrap();
    end_node(&domain);
    assert_eq!(task(&domain), TaskState::Done);
}

#[tokio::test]
async fn a_pr_the_merge_queue_drops_goes_back_to_the_node() {
    let (_home, domain, github) = presented("dropped", false);
    github.queue.lock().unwrap().insert(7, true);
    accept(&domain);
    let mut merger = now();
    merger.merge_due(&domain, &github).await.unwrap();
    github.queue.lock().unwrap().insert(7, false);
    merger.merge_due(&domain, &github).await.unwrap();
    assert_eq!(task(&domain), TaskState::Running);
    assert!(feedback(&domain)[0].contains("GitHub's merge queue removed it"), "{:?}", feedback(&domain));
}

#[tokio::test]
async fn a_pr_the_queue_merges_while_alondra_checks_it_is_recorded_not_handed_back() {
    let (_home, domain, github) = presented("queue-race", false);
    github.queue.lock().unwrap().insert(7, true);
    accept(&domain);
    let mut merger = now();
    merger.merge_due(&domain, &github).await.unwrap();
    github.merged_by_queue.lock().unwrap().insert(7);
    merger.merge_due(&domain, &github).await.unwrap();
    assert!(feedback(&domain).is_empty(), "{:?}", feedback(&domain));
    end_node(&domain);
    assert_eq!(task(&domain), TaskState::Done);
}

#[tokio::test]
async fn a_merge_refused_for_access_raises_one_card_while_reads_work_until_a_merge_succeeds() {
    let (_home, domain, github) = presented("no-access", false);
    github.merges.lock().unwrap().insert(7, Err("GraphQL: must have admin rights to Repository. (mergePullRequest)".into()));
    accept(&domain);
    let mut merger = now();
    for _ in 0..2 {
        merger.merge_due(&domain, &github).await.unwrap();
    }
    assert!(problems(&domain).is_empty());
    merger.merge_due(&domain, &github).await.unwrap();
    merger.merge_due(&domain, &github).await.unwrap();
    let raised = problems(&domain);
    assert_eq!(raised.len(), 1, "the successful reads in between do not clear it");
    assert!(raised[0].card.need.starts_with("Alondra can't merge PR #7: GraphQL: must have admin rights"), "{}", raised[0].card.need);
    assert!(!raised[0].withdrawn);
    assert_eq!(task(&domain), TaskState::WaitingDependency, "not handed back: only the user can grant access");
    github.merges.lock().unwrap().remove(&7);
    merger.merge_due(&domain, &github).await.unwrap();
    assert!(problems(&domain).iter().all(|card| card.withdrawn));
    end_node(&domain);
    assert_eq!(task(&domain), TaskState::Done);
}

#[tokio::test]
async fn cards_and_state_go_once_the_pr_is_no_longer_due() {
    let (_home, domain, github) = presented("leaves", true);
    github.open(7, HEAD, Checks::ActionRequired(vec!["deploy".into()]));
    let mut merger = now();
    merger.merge_due(&domain, &github).await.unwrap();
    assert_eq!(problems(&domain).iter().filter(|card| !card.withdrawn).count(), 1);
    command(&domain, user(), "cancel", Command::CancelTask { task_id: "t".into() });
    merger.merge_due(&domain, &github).await.unwrap();
    assert!(problems(&domain).iter().all(|card| card.withdrawn), "a cancelled task keeps no card");
    assert_eq!(merger.tracked(), 0);
}

#[tokio::test]
async fn failing_checks_go_back_once_and_cancelled_or_skipped_ones_do_not() {
    let (_home, domain, github) = presented("auto-red", true);
    github.open(7, HEAD, Checks::Failed(vec!["lint".into()]));
    let mut merger = now();
    merger.merge_due(&domain, &github).await.unwrap();
    merger.merge_due(&domain, &github).await.unwrap();
    assert!(github.merged.lock().unwrap().is_empty());
    assert_eq!(task(&domain), TaskState::Running);
    let feedback = feedback(&domain);
    assert_eq!(feedback.len(), 1, "{feedback:?}");
    assert_eq!(merge_failure(&feedback[0]), Some((7, "its checks failed (lint)")));
    assert_eq!(told(&domain), feedback, "the node gets the reason as it is");
}

#[test]
fn only_failed_checks_fail_and_action_required_asks_the_user() {
    let checks = |list: serde_json::Value| rollup(&serde_json::json!({"statusCheckRollup": list}));
    let run = |conclusion: &str| serde_json::json!({"__typename": "CheckRun", "name": conclusion, "status": "COMPLETED", "conclusion": conclusion});
    assert_eq!(checks(serde_json::json!([])), Checks::None);
    for passing in ["SUCCESS", "NEUTRAL", "SKIPPED", "CANCELLED", "STALE"] {
        assert_eq!(checks(serde_json::json!([run(passing)])), Checks::Passed, "{passing}");
    }
    for failing in ["FAILURE", "TIMED_OUT", "STARTUP_FAILURE"] {
        assert_eq!(checks(serde_json::json!([run(failing)])), Checks::Failed(vec![failing.into()]), "{failing}");
    }
    assert_eq!(checks(serde_json::json!([run("ACTION_REQUIRED")])), Checks::ActionRequired(vec!["ACTION_REQUIRED".into()]));
    assert_eq!(checks(serde_json::json!([{"name": "ui", "status": "IN_PROGRESS"}, run("SUCCESS")])), Checks::Pending);
    assert_eq!(checks(serde_json::json!([{"context": "ci/legacy", "state": "PENDING"}])), Checks::Pending);
    assert_eq!(checks(serde_json::json!([{"context": "ci/legacy", "state": "ERROR"}])), Checks::Failed(vec!["ci/legacy".into()]));
}

#[tokio::test]
async fn checks_that_need_someone_on_github_raise_one_card_and_wait() {
    let (_home, domain, github) = presented("action", true);
    github.open(7, HEAD, Checks::ActionRequired(vec!["deploy".into()]));
    let mut merger = now();
    assert!(merger.merge_due(&domain, &github).await.unwrap());
    assert!(merger.merge_due(&domain, &github).await.unwrap());
    let raised = problems(&domain);
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].card.need, "Act on the checks of PR #7 on GitHub");
    assert_eq!(task(&domain), TaskState::ForYourReview, "not handed back");
    github.open(7, HEAD, Checks::Passed);
    merger.merge_due(&domain, &github).await.unwrap();
    assert!(problems(&domain).iter().all(|card| card.withdrawn));
    end_node(&domain);
    assert_eq!(task(&domain), TaskState::Done);
}

#[tokio::test]
async fn a_merge_github_refuses_hands_the_task_back_with_the_reason() {
    let (_home, domain, github) = presented("refused", false);
    github.merges.lock().unwrap().insert(7, Err("Pull request owner/repo#7 is not mergeable: the merge commit cannot be cleanly created".into()));
    accept(&domain);
    let mut merger = now();
    merger.merge_due(&domain, &github).await.unwrap();
    assert_eq!(task(&domain), TaskState::Running, "the node fixes it");
    assert_eq!(domain.state().unwrap().merge_authorizations.values().next().unwrap().attempt, MergeAttemptState::Failed);
    let feedback = feedback(&domain);
    assert!(feedback[0].contains("the merge commit cannot be cleanly created") && feedback[0].contains("present the result again"),
        "{feedback:?}");
    merger.merge_due(&domain, &github).await.unwrap();
    assert_eq!(self::feedback(&domain).len(), 1, "a handed back delivery is not retried");
}

#[tokio::test]
async fn a_pr_github_says_is_already_merged_is_recorded_not_handed_back() {
    let (_home, domain, github) = presented("already", false);
    github.merges.lock().unwrap().insert(7, Err("Pull request owner/repo#7 was already merged".into()));
    accept(&domain);
    let mut merger = now();
    merger.merge_due(&domain, &github).await.unwrap();
    github.statuses.lock().unwrap().get_mut(&7).unwrap().as_mut().unwrap().state = PrState::Merged;
    merger.merge_due(&domain, &github).await.unwrap();
    assert!(feedback(&domain).is_empty());
    end_node(&domain);
    assert_eq!(task(&domain), TaskState::Done);
}

const SOMETHING_WENT_WRONG: &str =
    "GraphQL: Something went wrong while executing your query. This may be the result of a timeout, or it could be a GitHub bug.";

/// A `gh` that reports PR #7 open at `HEAD` with its checks passed, and whose `gh pr merge` fails
/// with `error` its first `failures` times, then merges it. Returns it and the file each merge
/// try is logged to.
fn scripted_gh(home: &Home, failures: u32, error: &str) -> (GhCli, std::path::PathBuf) {
    let program = home.0.join("gh");
    let tries = home.0.join("merge-tries");
    let merged = home.0.join("merged");
    std::fs::write(&program, format!(r#"#!/bin/sh
case "$*" in
  *"pr merge"*)
    echo try >> '{tries}'
    if [ "$(wc -l < '{tries}')" -le {failures} ]; then echo '{error}' >&2; exit 1; fi
    touch '{merged}';;
  *isDraft*) echo false;;
  *statusCheckRollup*)
    state=OPEN; [ -e '{merged}' ] && state=MERGED
    printf '{{"state":"%s","headRefOid":"{HEAD}","baseRefName":"main","mergeStateStatus":"CLEAN","statusCheckRollup":[{{"name":"build","status":"COMPLETED","conclusion":"SUCCESS"}}]}}' "$state";;
esac
"#, tries = tries.display(), merged = merged.display())).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    (GhCli::at(program), tries)
}

fn tries(log: &std::path::Path) -> usize {
    std::fs::read_to_string(log).map_or(0, |log| log.lines().count())
}

#[tokio::test]
async fn a_merge_github_fails_on_its_side_is_retried_and_merged_without_the_node() {
    let (home, domain, _) = presented("transient-then-merged", false);
    let (gh, log) = scripted_gh(&home, 2, SOMETHING_WENT_WRONG);
    accept(&domain);
    let mut merger = now();
    for _ in 0..3 {
        merger.merge_due(&domain, &gh).await.unwrap();
    }
    assert_eq!(tries(&log), 3);
    assert!(feedback(&domain).is_empty(), "{:?}", feedback(&domain));
    assert_eq!(domain.state().unwrap().pull_requests[&PrId::from("pr")].state, PrState::Merged);
    end_node(&domain);
    assert_eq!(task(&domain), TaskState::Done);
}

#[tokio::test]
async fn a_merge_github_keeps_failing_on_its_side_goes_back_after_the_last_try() {
    let (home, domain, _) = presented("transient-forever", false);
    let (gh, log) = scripted_gh(&home, u32::MAX, SOMETHING_WENT_WRONG);
    accept(&domain);
    let mut merger = now();
    for _ in 1..TRANSIENT_MERGE_TRIES {
        assert!(merger.merge_due(&domain, &gh).await.unwrap());
    }
    assert_eq!(task(&domain), TaskState::WaitingDependency, "still Alondra's to merge");
    assert!(feedback(&domain).is_empty());
    merger.merge_due(&domain, &gh).await.unwrap();
    assert_eq!(tries(&log), TRANSIENT_MERGE_TRIES as usize);
    assert_eq!(task(&domain), TaskState::Running);
    let feedback = feedback(&domain);
    assert!(feedback[0].contains("GitHub kept failing after 6 tries") && feedback[0].contains("Something went wrong"), "{feedback:?}");
    merger.merge_due(&domain, &gh).await.unwrap();
    assert_eq!(tries(&log), TRANSIENT_MERGE_TRIES as usize, "a handed back delivery is not retried");
}

#[tokio::test]
async fn a_merge_github_refuses_goes_back_at_once_with_its_reason() {
    let (home, domain, _) = presented("refused-at-once", false);
    let (gh, log) = scripted_gh(&home, u32::MAX, "Pull request owner/repo#7 is not mergeable: the merge commit cannot be cleanly created.");
    accept(&domain);
    now().merge_due(&domain, &gh).await.unwrap();
    assert_eq!(tries(&log), 1);
    assert_eq!(task(&domain), TaskState::Running);
    assert!(feedback(&domain)[0].contains("the merge commit cannot be cleanly created"), "{:?}", feedback(&domain));
}

#[tokio::test]
async fn a_transient_merge_failure_waits_its_backoff_not_the_poll() {
    let (_home, domain, github) = presented("backoff", false);
    github.merges.lock().unwrap().insert(7, Err("HTTP 502: Bad Gateway (https://api.github.com/graphql)".into()));
    accept(&domain);
    let mut merger = Merger::with(Duration::from_secs(3600), Duration::from_secs(3600));
    merger.merge_due(&domain, &github).await.unwrap();
    assert!(merger.pause() <= FIRST_MERGE_RETRY && merger.pause() > FIRST_MERGE_RETRY - Duration::from_secs(5), "{:?}", merger.pause());
    merger.merge_due(&domain, &github).await.unwrap();
    assert_eq!(*github.calls.lock().unwrap(), ["merge 7"], "not tried again before its backoff");
}

#[test]
fn transient_merge_retries_double_from_thirty_seconds() {
    let waits: Vec<u64> = (1..TRANSIENT_MERGE_TRIES).map(|failures| merge_retry_wait(FIRST_MERGE_RETRY, failures).as_secs()).collect();
    assert_eq!(waits, [30, 60, 120, 240, 480]);
}

#[test]
fn github_failing_on_its_side_is_transient_and_a_real_refusal_is_not() {
    for reason in [SOMETHING_WENT_WRONG, "HTTP 502: Bad Gateway", "HTTP 503: Service Unavailable", "gh pr did not finish within 120 s",
        "error connecting to api.github.com"]
    {
        assert!(transient(reason), "{reason}");
    }
    for reason in ["Pull request owner/repo#7 is not mergeable: the merge commit cannot be cleanly created.",
        "Pull request owner/repo#7 is still a draft", "GraphQL: Base branch was modified. Review and try the merge again."]
    {
        assert!(!transient(reason), "{reason}");
    }
}

#[tokio::test]
async fn gh_that_cannot_work_raises_one_card_until_it_works_again() {
    let (_home, domain, github) = presented("logged-out", false);
    accept(&domain);
    github.statuses.lock().unwrap().insert(7, Err("To get started with GitHub CLI, please run:  gh auth login".into()));
    let mut merger = now();
    for _ in 0..2 {
        merger.merge_due(&domain, &github).await.unwrap();
    }
    assert!(problems(&domain).is_empty());
    for _ in 0..3 {
        merger.merge_due(&domain, &github).await.unwrap();
    }
    let raised = problems(&domain);
    assert_eq!(raised.len(), 1, "one card however long it fails");
    assert!(raised[0].card.need.starts_with("Alondra can't merge PR #7: To get started"), "{}", raised[0].card.need);
    assert_eq!(raised[0].scope, Scope::Task { task_id: "t".into() });
    assert_eq!(task(&domain), TaskState::WaitingDependency, "the acceptance stands");
    github.open(7, HEAD, Checks::Passed);
    merger.merge_due(&domain, &github).await.unwrap();
    assert!(problems(&domain).iter().all(|card| card.withdrawn));
    end_node(&domain);
    assert_eq!(task(&domain), TaskState::Done);
}

#[tokio::test]
async fn a_waiting_pr_is_read_from_github_at_most_once_per_poll() {
    let (_home, domain, github) = presented("throttle", true);
    github.open(7, HEAD, Checks::Pending);
    let mut merger = Merger::with(Duration::from_secs(3600), Duration::from_secs(3600));
    for _ in 0..5 {
        merger.merge_due(&domain, &github).await.unwrap();
    }
    assert_eq!(github.reads.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn a_failing_loop_backs_off_up_to_five_minutes() {
    let mut backoff = FIRST_BACKOFF;
    let mut seen = vec![];
    for _ in 0..10 {
        seen.push(backoff.as_secs());
        backoff = next_backoff(backoff);
    }
    assert_eq!(seen, [5, 10, 20, 40, 80, 160, 300, 300, 300, 300]);
}

#[tokio::test]
async fn the_merge_loop_survives_a_storage_failure() {
    let (_home, domain, github) = presented("storage", false);
    let worker = tokio::spawn(run(domain.clone(), Arc::new(github), Merger::default()));
    domain.store.lock().unwrap().conn_for_test().execute_batch("DROP TABLE domain_meta").ok();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!worker.is_finished(), "a failed read is logged and retried, never the end of merging");
    worker.abort();
}

#[tokio::test]
async fn gh_merges_with_the_method_only_at_the_presented_head_and_reads_status_and_queue() {
    let home = Home(crate::test_support::Directory::new());
    std::fs::create_dir(&home.0).unwrap();
    let program = home.0.join("gh");
    let log = home.0.join("calls");
    std::fs::write(&program, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\ncase \"$*\" in *statusCheckRollup*) printf '%s' '{{\"state\":\"OPEN\",\"headRefOid\":\"{HEAD}\",\"baseRefName\":\"main\",\"statusCheckRollup\":[{{\"name\":\"build\",\"status\":\"COMPLETED\",\"conclusion\":\"SUCCESS\"}},{{\"name\":\"lint\",\"status\":\"COMPLETED\",\"conclusion\":\"FAILURE\"}}]}}';; *graphql*) echo true;; esac\n", log.display())).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let gh = GhCli::at(program);
    let pr = PullRequest { id: "pr".into(), task_id: "t".into(), host: "github".into(), repository: "owner/repo".into(),
        number: 7, head: HEAD.into(), base: "main".into(), state: PrState::Open, merged_head: None,
        allowed_methods: vec![MergeMethod::Squash], review: None };
    let status = gh.status(&pr).await.unwrap();
    assert_eq!((status.state, status.head.as_str(), status.checks), (PrState::Open, HEAD, Checks::Failed(vec!["lint".into()])));
    gh.merge(&pr, HEAD, MergeMethod::Squash).await.unwrap();
    assert!(gh.in_queue(&pr).await.unwrap());
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains(&format!("pr merge https://github.com/owner/repo/pull/7 --squash --match-head-commit {HEAD}")), "{calls}");
    assert!(calls.contains("api graphql --hostname github.com") && calls.contains("number=7"), "{calls}");
    assert!(!calls.contains("--delete-branch") && !calls.contains("--auto"), "{calls}");
}

const DRAFT_REFUSAL: &str = "PR #7 is a draft: mark it ready with gh pr ready before delivering";

#[tokio::test]
async fn a_final_delivery_of_a_draft_pr_is_refused_with_the_ready_instruction() {
    let (_home, domain, github) = presented("draft", false);
    github.drafts.lock().unwrap().insert(7, Ok(true));
    let refusal = draft_refusal(&domain.state().unwrap(), &targets(), &github).await.unwrap();
    assert_eq!(refusal.message, DRAFT_REFUSAL);
}

#[tokio::test]
async fn a_final_delivery_of_a_ready_pr_is_accepted() {
    let (_home, domain, github) = presented("ready-pr", false);
    github.drafts.lock().unwrap().insert(7, Ok(false));
    assert_eq!(draft_refusal(&domain.state().unwrap(), &targets(), &github).await, None);
}

#[tokio::test]
async fn when_github_cannot_say_if_the_pr_is_a_draft_the_delivery_is_accepted_and_the_merger_marks_it_ready_first() {
    let (_home, domain, github) = presented("draft-unknown", false);
    github.drafts.lock().unwrap().insert(7, Err("network down".into()));
    assert_eq!(draft_refusal(&domain.state().unwrap(), &targets(), &github).await, None);
    accept(&domain);
    now().merge_due(&domain, &github).await.unwrap();
    assert_eq!(*github.calls.lock().unwrap(), vec!["ready 7".to_owned(), "merge 7".to_owned()]);
}

#[tokio::test]
async fn when_marking_the_pr_ready_fails_nothing_is_merged_and_the_real_reason_is_kept() {
    let (_home, domain, github) = presented("ready-fails", false);
    github.drafts.lock().unwrap().insert(7, Ok(true));
    github.readies.lock().unwrap().insert(7, Err("GraphQL: Pull request is in a state that cannot be marked ready".into()));
    accept(&domain);
    now().merge_due(&domain, &github).await.unwrap();
    assert!(github.merged.lock().unwrap().is_empty());
    assert!(feedback(&domain).iter().any(|body| body.contains("cannot be marked ready")), "{:?}", feedback(&domain));
}

#[tokio::test]
async fn a_pr_github_says_is_ready_is_merged_without_marking_it_ready() {
    let (_home, domain, github) = presented("merge-ready", false);
    github.drafts.lock().unwrap().insert(7, Ok(false));
    accept(&domain);
    now().merge_due(&domain, &github).await.unwrap();
    assert_eq!(*github.calls.lock().unwrap(), vec!["merge 7".to_owned()]);
}

#[tokio::test]
async fn a_draft_pr_is_marked_ready_before_it_is_merged() {
    let (_home, domain, github) = presented("merge-draft", false);
    github.drafts.lock().unwrap().insert(7, Ok(true));
    accept(&domain);
    now().merge_due(&domain, &github).await.unwrap();
    assert_eq!(*github.calls.lock().unwrap(), vec!["ready 7".to_owned(), "merge 7".to_owned()]);
}
