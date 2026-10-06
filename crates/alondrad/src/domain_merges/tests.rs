use super::*;
use crate::domain_github::PrFacts;
use alondra_core::domain::*;
use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch, command_id: id.into(),
        expected_revision: state.revision, command,
    }, None, || Ok(())).unwrap();
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn targets() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEAD.into() }]
}

/// A node's Task with its PR #7 presented as the final result.
fn presented(tag: &str, auto_merge: bool) -> (Home, DomainRuntime, FakeGithub) {
    let home = Home(std::env::temp_dir().join(format!("alondra-merges-{tag}-{}", crate::auth::random_hex(8))));
    std::fs::create_dir(&home.0).unwrap();
    let domain = DomainRuntime::open(&home.0).unwrap();
    command(&domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into() });
    command(&domain, user(), "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge });
    command(&domain, user(), "task", Command::CreateTask {
        task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()),
        review_prs: true, dependencies: BTreeSet::new(), stack: None,
    });
    command(&domain, Actor::Agent { agent_id: "a".into() }, "pr", Command::RegisterPr {
        task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
        number: 7, head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash, MergeMethod::Merge],
    });
    command(&domain, Actor::Agent { agent_id: "a".into() }, "present", Command::PresentResult {
        attachments: vec![], task_id: "t".into(), interaction_id: "d".into(), body: "Ready".into(),
        targets: targets(), is_final: true, card: CardSpec::delivery("Review the result", &[], true),
    });
    let github = FakeGithub::new();
    github.observer.set(7, Ok(PrFacts { head: HEAD.into(), base: "main".into(), state: PrState::Open, merged_head: None }));
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

#[tokio::test]
async fn an_accepted_delivery_is_merged_by_alondra_and_the_node_is_told() {
    let (_home, domain, github) = presented("accept", false);
    let mut failures = BTreeMap::new();
    assert!(!merge_due(&domain, &github, &mut failures).await.unwrap());
    assert!(github.merged.lock().unwrap().is_empty(), "nothing is merged before the user accepts");
    accept(&domain);
    assert_eq!(task(&domain), TaskState::Closing);
    merge_due(&domain, &github, &mut failures).await.unwrap();
    assert_eq!(*github.merged.lock().unwrap(), vec![(7, HEAD.to_owned(), MergeMethod::Squash)]);
    let state = domain.state().unwrap();
    assert_eq!(state.pull_requests[&PrId::from("pr")].merged_head.as_deref(), Some(HEAD));
    assert_eq!(state.merge_authorizations.values().next().unwrap().attempt, MergeAttemptState::Confirmed);
    assert_eq!(task(&domain), TaskState::Done);
    let told = told(&domain);
    assert!(told[0].contains("Alondra is merging its pull request"), "{told:?}");
    assert_eq!(told.last().unwrap(), &merged_notice(7, 0));
    merge_due(&domain, &github, &mut failures).await.unwrap();
    assert_eq!(github.merged.lock().unwrap().len(), 1, "a merged PR is not merged again");
}

#[tokio::test]
async fn the_users_merge_button_is_merged_with_the_method_they_chose() {
    let (_home, domain, github) = presented("authorize", false);
    command(&domain, user(), "authorize", Command::AuthorizeMerge { authorization_id: "auth".into(),
        presentation_id: "d".into(), pr_id: "pr".into(), expected_head: HEAD.into(), method: MergeMethod::Merge });
    merge_due(&domain, &github, &mut BTreeMap::new()).await.unwrap();
    assert_eq!(*github.merged.lock().unwrap(), vec![(7, HEAD.to_owned(), MergeMethod::Merge)]);
    assert_eq!(task(&domain), TaskState::Done);
}

#[tokio::test]
async fn auto_merge_waits_for_the_checks_then_merges() {
    let (_home, domain, github) = presented("auto", true);
    github.checks.lock().unwrap().insert(7, Ok(Checks::Pending));
    let mut failures = BTreeMap::new();
    assert!(merge_due(&domain, &github, &mut failures).await.unwrap(), "pending checks are looked at again");
    assert!(github.merged.lock().unwrap().is_empty());
    assert_eq!(task(&domain), TaskState::InReview);
    github.checks.lock().unwrap().insert(7, Ok(Checks::Passed));
    assert!(!merge_due(&domain, &github, &mut failures).await.unwrap());
    assert_eq!(*github.merged.lock().unwrap(), vec![(7, HEAD.to_owned(), MergeMethod::Squash)]);
    assert_eq!(task(&domain), TaskState::Done);
    assert_eq!(told(&domain), vec![merged_notice(7, 0)]);
}

#[tokio::test]
async fn failing_checks_of_an_auto_merge_go_back_to_the_node_once() {
    let (_home, domain, github) = presented("auto-red", true);
    github.checks.lock().unwrap().insert(7, Ok(Checks::Failed(vec!["lint".into()])));
    let mut failures = BTreeMap::new();
    merge_due(&domain, &github, &mut failures).await.unwrap();
    merge_due(&domain, &github, &mut failures).await.unwrap();
    assert!(github.merged.lock().unwrap().is_empty());
    assert_eq!(task(&domain), TaskState::Running);
    let feedback = feedback(&domain);
    assert_eq!(feedback.len(), 1, "{feedback:?}");
    assert!(feedback[0].starts_with("Alondra could not merge pull request #7 (https://github.com/owner/repo/pull/7): its checks failed (lint)"),
        "{}", feedback[0]);
    assert_eq!(told(&domain), feedback, "the node gets the reason as it is");
}

#[tokio::test]
async fn a_merge_github_refuses_hands_the_task_back_with_the_reason() {
    let (_home, domain, github) = presented("refused", false);
    github.merges.lock().unwrap().insert(7, Err("Pull request owner/repo#7 is not mergeable: the merge commit cannot be cleanly created".into()));
    accept(&domain);
    let mut failures = BTreeMap::new();
    merge_due(&domain, &github, &mut failures).await.unwrap();
    assert_eq!(task(&domain), TaskState::Running, "the node fixes it");
    let state = domain.state().unwrap();
    assert_eq!(state.merge_authorizations.values().next().unwrap().attempt, MergeAttemptState::Failed);
    let feedback = feedback(&domain);
    assert!(feedback[0].contains("the merge commit cannot be cleanly created") && feedback[0].contains("present the result again"),
        "{feedback:?}");
    merge_due(&domain, &github, &mut failures).await.unwrap();
    assert_eq!(self::feedback(&domain).len(), 1, "a handed back delivery is not retried");
}

#[tokio::test]
async fn github_not_answering_is_retried_before_the_task_goes_back() {
    let (_home, domain, github) = presented("transient", false);
    github.merges.lock().unwrap().insert(7, Err("error connecting to api.github.com".into()));
    accept(&domain);
    let mut failures = BTreeMap::new();
    assert!(merge_due(&domain, &github, &mut failures).await.unwrap());
    assert!(merge_due(&domain, &github, &mut failures).await.unwrap());
    assert_eq!(task(&domain), TaskState::Closing, "still Alondra's to merge");
    merge_due(&domain, &github, &mut failures).await.unwrap();
    assert_eq!(task(&domain), TaskState::Running);
}

#[tokio::test]
async fn gh_merges_with_the_method_only_at_the_presented_head_and_reads_the_checks() {
    let home = Home(std::env::temp_dir().join(format!("alondra-merges-gh-{}", crate::auth::random_hex(8))));
    std::fs::create_dir(&home.0).unwrap();
    let program = home.0.join("gh");
    let log = home.0.join("calls");
    std::fs::write(&program, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\ncase \"$*\" in *statusCheckRollup*) printf '%s' '{{\"statusCheckRollup\":[{{\"name\":\"build\",\"status\":\"COMPLETED\",\"conclusion\":\"SUCCESS\"}},{{\"name\":\"lint\",\"status\":\"COMPLETED\",\"conclusion\":\"FAILURE\"}}]}}';; esac\n", log.display())).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let gh = GhCli::at(program);
    let pr = PullRequest { id: "pr".into(), task_id: "t".into(), host: "github".into(), repository: "owner/repo".into(),
        number: 7, head: HEAD.into(), base: "main".into(), state: PrState::Open, merged_head: None,
        allowed_methods: vec![MergeMethod::Squash], review: None };
    assert_eq!(gh.checks(&pr).await.unwrap(), Checks::Failed(vec!["lint".into()]));
    gh.merge(&pr, HEAD, MergeMethod::Squash).await.unwrap();
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains(&format!("pr merge https://github.com/owner/repo/pull/7 --squash --match-head-commit {HEAD}")), "{calls}");
    assert!(!calls.contains("--delete-branch") && !calls.contains("--auto"), "{calls}");
}

#[test]
fn checks_that_still_run_wait_and_no_checks_pass() {
    assert_eq!(rollup(&serde_json::json!({"statusCheckRollup": []})), Checks::Passed);
    assert_eq!(rollup(&serde_json::json!({"statusCheckRollup": [{"name": "ui", "status": "IN_PROGRESS"}]})), Checks::Pending);
}
