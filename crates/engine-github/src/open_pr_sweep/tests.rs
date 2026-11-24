use super::*;
use alondra_domain_rules::*;
use crate::github_world::GithubWorld;
use alondra_engine_ports::github::{Closed, Gh, GhFailure, Observed, PrObserver, Read};
use std::sync::Mutex;

fn command(domain: &DomainRuntime, actor: Actor, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
        command_id: format!("test-{}", state.revision).into(), expected_revision: state.revision, command,
    }, None, || Ok(())).unwrap();
}

fn user() -> Actor { Actor::User { user_id: "u".into() } }

fn task(domain: &DomainRuntime, id: &str, number: Option<u64>) {
    command(domain, Actor::System, Command::RegisterAgent { agent_id: id.into(), name: id.into(), at: 0 });
    command(domain, user(), Command::CreateRequest { request_id: id.into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    command(domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { priority: Default::default(),
        task_id: id.into(), request_id: id.into(), title: "Work".into(), agent_id: Some(id.into()), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/fixture")), plan_items: None, continues: None,
    });
    if let Some(number) = number {
        command(domain, Actor::Agent { agent_id: id.into() }, Command::RegisterPr {
            task_id: id.into(), pr_id: hosted_pr_id("github.com", "owner/repo", number), host: "github.com".into(),
            repository: "owner/repo".into(), number, head: "1".repeat(40), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
        });
    }
}

/// GitHub's search of the user's open PRs and the branch of each, over the world that answers the
/// rest; every read the sweep makes is recorded.
struct Authored {
    world: Arc<GithubWorld>,
    prs: Mutex<Vec<(String, u64, String)>>,
    calls: Mutex<Vec<String>>,
}

impl Gh for Authored {
    fn view<'a>(&'a self, url: &'a str) -> Read<'a, Option<serde_json::Value>> { self.world.view(url) }
    fn authored_open_prs(&self) -> Read<'_, Result<Vec<AuthoredPr>, GhFailure>> {
        self.calls.lock().unwrap().push("search prs".into());
        let found = self.prs.lock().unwrap().iter().map(|(repository, number, _)| AuthoredPr {
            repository: repository.clone(), number: *number, url: format!("https://github.com/{repository}/pull/{number}"),
        }).collect();
        Box::pin(async { Ok(found) })
    }
    fn head_ref<'a>(&'a self, url: &'a str) -> Read<'a, Result<String, GhFailure>> {
        self.calls.lock().unwrap().push(format!("pr view {url} headRefName"));
        let branch = self.prs.lock().unwrap().iter()
            .find(|(repository, number, _)| url == format!("https://github.com/{repository}/pull/{number}"))
            .map(|(_, _, branch)| branch.clone()).expect("the branch of a PR the search found");
        Box::pin(async { Ok(branch) })
    }
}

impl PrObserver for Authored {
    fn observe<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Observed<'a> {
        self.calls.lock().unwrap().push(format!("pr view {number} status"));
        self.world.observe(host, repository, number)
    }
    fn close<'a>(&'a self, host: &'a str, repository: &'a str, number: u64, expected: &'a alondra_domain_rules::ProviderPrKey, comment: &'a str) -> Closed<'a> {
        self.world.close(host, repository, number, expected, comment)
    }
}

struct Fixture {
    root: crate::test_support::Directory,
    domain: DomainRuntime,
    options: OptionsFile,
    github: Arc<Authored>,
    gh: GitHub,
}

impl Fixture {
    fn new() -> Self {
        let root = crate::test_support::Directory::new();
        std::fs::create_dir_all(root.join("home")).unwrap();
        let domain = crate::unwired(&root.join("home")).unwrap();
        let projects = root.join("projects");
        std::fs::create_dir_all(projects.join("checkout/.git")).unwrap();
        std::fs::write(projects.join("checkout/.git/config"), "[remote \"origin\"]\nurl = https://github.com/owner/repo.git\n").unwrap();
        let options = OptionsFile { projects_root: Some(projects.to_string_lossy().into()), ..Default::default() };
        let world = Arc::new(GithubWorld::new());
        let github = Arc::new(Authored { world: world.clone(), prs: Mutex::default(), calls: Mutex::default() });
        let gh = GitHub { observer: github.clone(), merger: world.clone(), images: world, reads: github.clone() };
        Self { root, domain, options, github, gh }
    }
    /// GitHub's search finds PR `number` of `repo`, open from `branch`.
    fn result(&self, number: u64, repo: &str, branch: &str) {
        self.github.prs.lock().unwrap().push((repo.into(), number, branch.into()));
        self.github.world.open_pr_at(number, branch, "main", &"1".repeat(40));
    }
    async fn sweep(&self) { sweep(&self.domain, &self.gh, &self.options, &self.root).await.unwrap(); }
    fn calls(&self) -> String { self.github.calls.lock().unwrap().join("\n") }
}

/// Task `id` registered PR `number`, was cancelled, and GitHub closed the PR: nothing is pending for it.
fn cancelled_with_closed_pr(f: &Fixture, id: &str, number: u64) {
    task(&f.domain, id, Some(number));
    command(&f.domain, user(), Command::CancelTask { task_id: id.into(), reason: None });
    command(&f.domain, Actor::System, Command::RecordNodeEnd { task_id: id.into(), end: NodeEnd::Closed, agent_id: None });
    command(&f.domain, Actor::System, Command::ObservePrState { pr_id: hosted_pr_id("github.com", "owner/repo", number),
        head: "1".repeat(40), base: "main".into(), state: PrState::Closed, merged_head: None });
}

fn closing(f: &Fixture, id: &str) -> bool {
    matches!(f.domain.state().unwrap().tasks[&id.into()].open_pr, Some(OpenPrPending::Close { .. }))
}

/// Alondra handed task `id`'s open PR to the user as a delivery.
fn handed_to_user(f: &Fixture, id: &str) -> bool {
    let state = f.domain.state().unwrap();
    let task = &state.tasks[&id.into()];
    task.state == TaskState::ForYourReview && task.final_presentation.is_some() && task.open_pr.is_none()
}

#[tokio::test]
async fn release_branches_are_excluded_and_prefixes_are_configurable() {
    let mut f = Fixture::new();
    cancelled_with_closed_pr(&f, "closed", 117);
    f.result(117, "owner/repo", "release-plz-2026");
    f.sweep().await;
    assert!(!closing(&f, "closed"));
    assert!(!handed_to_user(&f, "closed"));
    f.options.pr_sweep_ignore_branch_prefixes = vec!["ship-".into()];
    f.sweep().await;
    assert!(handed_to_user(&f, "closed"));
}

#[tokio::test]
async fn a_live_tasks_registered_pr_is_skipped_without_reading_its_branch() {
    let f = Fixture::new();
    task(&f.domain, "live", Some(7));
    f.result(7, "owner/repo", "feature");
    let before = f.domain.state().unwrap();
    f.sweep().await;
    assert_eq!(f.calls(), "search prs");
    assert_eq!(f.domain.state().unwrap(), before);
}

#[tokio::test]
async fn a_live_tasks_worktree_branch_is_skipped() {
    let f = Fixture::new();
    cancelled_with_closed_pr(&f, "closed", 7);
    task(&f.domain, "live", None);
    f.result(7, "owner/repo", "worktree-live");
    f.sweep().await;
    assert!(!closing(&f, "closed") && !handed_to_user(&f, "closed"));
}

/// A PR opened from another Mac or by hand is not this daemon's: the sweep never reads its branch,
/// never adopts it and never asks anyone to, now, on a later sweep or after a restart.
#[tokio::test]
async fn a_pr_no_task_registered_is_never_read_or_adopted() {
    let f = Fixture::new();
    cancelled_with_closed_pr(&f, "closed", 8);
    f.result(7, "owner/repo", "feature");
    f.result(8, "owner/repo", "worktree-closed");
    let before = f.domain.state().unwrap();
    f.sweep().await;
    f.sweep().await;
    let home = f.root.join("home");
    drop(f.domain);
    let domain = crate::unwired(&home).unwrap();
    sweep(&domain, &f.gh, &f.options, &f.root).await.unwrap();
    let state = domain.state().unwrap();
    assert_eq!(state.requests, before.requests, "no request asks to adopt it");
    let new: Vec<_> = state.interactions.values().filter(|card| !before.interactions.contains_key(&card.id)).collect();
    assert!(!new.is_empty() && new.iter().all(|card| !card.body.contains("pull/7") && !card.body.contains("#7")),
        "only the registered PR reaches the user: {new:?}");
    assert!(state.pull_requests.values().all(|p| p.number != 7), "no task takes it");
    assert_eq!(state.tasks[&"closed".into()].state, TaskState::ForYourReview, "the registered PR is still handled");
    let calls = f.github.calls.lock().unwrap().join("\n");
    assert!(!calls.contains("pull/7") && !calls.contains("pr view 7"), "nothing about PR 7 is read beyond the search: {calls}");
    assert_eq!(calls.lines().filter(|c| c.contains("pull/8 headRefName")).count(), 1);
}

#[tokio::test]
async fn a_repository_outside_the_projects_root_is_ignored() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.root.join("outside/.git")).unwrap();
    std::fs::write(f.root.join("outside/.git/config"), "[remote \"origin\"]\nurl = https://github.com/owner/outside.git\n").unwrap();
    f.result(7, "owner/outside", "feature");
    f.sweep().await;
    assert_eq!(f.calls(), "search prs");
}

fn adopt(f: &Fixture, task_id: &str, checks: PrChecks) {
    command(&f.domain, Actor::System, Command::AdoptOpenPr { task_id: task_id.into(), pr_id: hosted_pr_id("github.com", "owner/repo", 7),
        branch: "feature".into(), status: OpenPrStatus { state: PrState::Open, head: "1".repeat(40), base: "main".into(),
            mergeable: PrMergeable::Clean, checks } });
}

#[tokio::test]
async fn adopting_a_closed_tasks_open_pr_reopens_and_delivers_it() {
    let f = Fixture::new();
    task(&f.domain, "closed", Some(7));
    command(&f.domain, user(), Command::CancelTask { task_id: "closed".into(), reason: None });
    command(&f.domain, Actor::System, Command::RecordNodeEnd { task_id: "closed".into(), end: NodeEnd::Closed, agent_id: None });
    command(&f.domain, Actor::System, Command::ObservePrState { pr_id: hosted_pr_id("github.com", "owner/repo", 7),
        head: "1".repeat(40), base: "main".into(), state: PrState::Closed, merged_head: None });
    // GitHub reopening a PR recreates the persisted orphan from before close_task existed.
    assert_eq!(f.domain.state().unwrap().tasks[&"closed".into()].state, TaskState::Cancelled);
    adopt(&f, "closed", PrChecks::Passed);
    let state = f.domain.state().unwrap();
    let task = &state.tasks[&"closed".into()];
    assert_eq!(task.state, TaskState::ForYourReview);
    assert!(task.agent_id.is_none());
    assert!(task.final_presentation.is_some());
    assert!(task.open_pr.is_none());
}

/// F-GH-1 and the user's decision of 2026-11-22: Alondra closes a cancelled task's PR once; when
/// someone reopens it on GitHub, the sweep hands it to the user and nothing closes it again, also
/// after a restart.
#[tokio::test]
async fn a_cancelled_tasks_pr_reopened_after_alondra_closed_it_goes_to_the_user_and_is_never_closed_again() {
    let f = Fixture::new();
    task(&f.domain, "closed", Some(7));
    f.result(7, "owner/repo", "feature");
    command(&f.domain, user(), Command::CancelTask { task_id: "closed".into(), reason: None });
    command(&f.domain, Actor::System, Command::RecordNodeEnd { task_id: "closed".into(), end: NodeEnd::Closed, agent_id: None });
    let mut failures = crate::domain_github::ObservationFailures::default();
    crate::domain_github::reconcile(&f.domain, &*f.github, &mut failures).await.unwrap();
    assert_eq!(f.github.world.closes(), [(7, "Cancelled in Alondra: Work".to_owned())]);
    f.github.world.reopen(7);
    f.sweep().await;
    assert!(handed_to_user(&f, "closed"), "{:?}", f.domain.state().unwrap().tasks[&"closed".into()]);
    crate::domain_github::reconcile(&f.domain, &*f.github, &mut failures).await.unwrap();
    f.sweep().await;
    let home = f.root.join("home");
    drop(f.domain);
    let domain = crate::unwired(&home).unwrap();
    crate::domain_github::reconcile(&domain, &*f.github, &mut failures).await.unwrap();
    sweep(&domain, &f.gh, &f.options, &f.root).await.unwrap();
    assert_eq!(f.github.world.closes().len(), 1, "closed once");
    assert_eq!(f.github.world.state_of(7), PrState::Open);
    assert_eq!(domain.state().unwrap().tasks[&"closed".into()].state, TaskState::ForYourReview);
}

#[tokio::test]
async fn a_new_task_can_take_a_closed_tasks_pr_and_keep_the_old_delivery() {
    let f = Fixture::new();
    task(&f.domain, "old", Some(7));
    command(&f.domain, Actor::Agent { agent_id: "old".into() }, Command::PresentResult {
        attachments: vec![], task_id: "old".into(), interaction_id: "old-delivery".into(), body: "Ready".into(),
        is_final: true, nothing_to_review: false, targets: vec![DeliveryTarget::PullRequest {
            pr_id: hosted_pr_id("github.com", "owner/repo", 7), head: "1".repeat(40),
        }], checklist: vec![], card: CardSpec::delivery("Review the result", &[], true),
    });
    command(&f.domain, user(), Command::CancelTask { task_id: "old".into(), reason: None });
    command(&f.domain, Actor::System, Command::RecordNodeEnd { task_id: "old".into(), end: NodeEnd::Closed, agent_id: None });
    command(&f.domain, Actor::System, Command::ObservePrState { pr_id: hosted_pr_id("github.com", "owner/repo", 7),
        head: "1".repeat(40), base: "main".into(), state: PrState::Closed, merged_head: None });
    task(&f.domain, "new", Some(7));
    let pr_id = hosted_pr_id("github.com", "owner/repo", 7);
    let state = f.domain.state().unwrap();
    assert_eq!(state.pull_requests[&pr_id].task_id, TaskId::from("new"));
    assert!(state.tasks[&"old".into()].pr_id.is_none());
    assert!(state.pr_task_history[&pr_id].contains(&"old".into()));
    assert!(state.interactions.contains_key(&"old-delivery".into()));
    state.validate().unwrap();
    drop(state);
    let home = f.root.join("home");
    drop(f.domain);
    let domain = crate::unwired(&home).unwrap();
    let state = domain.state().unwrap();
    state.validate().unwrap();
    assert_eq!(state.pull_requests[&pr_id].task_id, TaskId::from("new"));
    assert!(state.pr_task_history[&pr_id].contains(&"old".into()));
    assert!(state.interactions.contains_key(&"old-delivery".into()));
}

#[tokio::test]
async fn the_users_pending_close_is_not_undone_by_a_sweep() {
    let f = Fixture::new();
    task(&f.domain, "closed", Some(7));
    command(&f.domain, user(), Command::CancelTask { task_id: "closed".into(), reason: None });
    command(&f.domain, Actor::System, Command::RecordNodeEnd { task_id: "closed".into(), end: NodeEnd::Closed, agent_id: None });
    f.result(7, "owner/repo", "feature");
    f.sweep().await;
    assert_eq!(f.calls(), "search prs");
    assert!(matches!(f.domain.state().unwrap().tasks[&"closed".into()].open_pr, Some(OpenPrPending::Close { .. })));
}

#[test]
fn sweep_prefix_defaults_read_old_options_and_allow_an_empty_override() {
    let old: OptionsFile = toml::from_str("").unwrap();
    assert_eq!(old.pr_sweep_ignore_branch_prefixes, ["release-plz-"]);
    let explicit: OptionsFile = toml::from_str("pr_sweep_ignore_branch_prefixes = []").unwrap();
    assert!(explicit.pr_sweep_ignore_branch_prefixes.is_empty());
}

#[tokio::test]
async fn adopting_delivers_pending_checks_without_starting_a_node() {
    let f = Fixture::new();
    task(&f.domain, "closed", Some(7));
    command(&f.domain, user(), Command::CancelTask { task_id: "closed".into(), reason: None });
    command(&f.domain, Actor::System, Command::RecordNodeEnd { task_id: "closed".into(), end: NodeEnd::Closed, agent_id: None });
    command(&f.domain, Actor::System, Command::ObservePrState { pr_id: hosted_pr_id("github.com", "owner/repo", 7),
        head: "1".repeat(40), base: "main".into(), state: PrState::Closed, merged_head: None });
    adopt(&f, "closed", PrChecks::Pending { names: vec!["test".into()] });
    let state = f.domain.state().unwrap();
    let task = &state.tasks[&"closed".into()];
    assert!(task.open_pr.is_none());
    assert!(task.final_presentation.is_some());
    assert_eq!(task.state, TaskState::ForYourReview);
    assert!(task.agent_id.is_none());
    let delivery = &state.interactions[task.final_presentation.as_ref().unwrap()];
    assert_eq!(alondra_domain_rules::open_pr_delivery::OpenPrDelivery::from_body(&delivery.body).unwrap().readiness,
        alondra_domain_rules::open_pr_delivery::PrReadiness::ChecksRunning { names: vec!["test".into()] });

}

#[tokio::test]
async fn an_adopted_pr_returns_to_the_launch_queue_if_its_merge_fails() {
    let f = Fixture::new();
    task(&f.domain, "closed", Some(7));
    command(&f.domain, user(), Command::CancelTask { task_id: "closed".into(), reason: None });
    command(&f.domain, Actor::System, Command::RecordNodeEnd { task_id: "closed".into(), end: NodeEnd::Closed, agent_id: None });
    command(&f.domain, Actor::System, Command::ObservePrState { pr_id: hosted_pr_id("github.com", "owner/repo", 7),
        head: "1".repeat(40), base: "main".into(), state: PrState::Closed, merged_head: None });
    adopt(&f, "closed", PrChecks::Passed);
    let state = f.domain.state().unwrap();
    let delivery = state.tasks[&"closed".into()].final_presentation.clone().unwrap();
    let InteractionPayload::Delivery { targets, .. } = &state.interactions[&delivery].payload else { panic!("a delivery") };
    command(&f.domain, Actor::System, Command::SubmitFeedback {
        attachments: vec![], presentation_id: delivery.clone(), targets: targets.clone(),
        body: merge_failed_text(7, "https://github.com/owner/repo/pull/7", "main", "review required"),
        instruction: Instruction::Conflicts, merge_failure: Some(MergeFailure { number: 7, reason: "review required".into() }),
    });
    let state = f.domain.state().unwrap();
    let task = &state.tasks[&"closed".into()];
    assert_eq!(task.state, TaskState::NotStarted);
    assert!(task.agent_id.is_none());
    assert!(f.domain.store.lock().unwrap().launch_queue().unwrap().iter().any(|row| row.task_id == task.id),
        "a failed merge needs a new node, even though this task had one before it was closed");
}

#[test]
fn checkouts_with_a_gitfile_use_their_common_repository() {
    let f = Fixture::new();
    let projects = f.root.join("projects");
    let gitdir = f.root.join("common/worktrees/task");
    std::fs::create_dir_all(&gitdir).unwrap();
    std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
    std::fs::write(f.root.join("common/config"), "[remote \"origin\"]\nurl = git@github.com:owner/worktree.git\n").unwrap();
    std::fs::create_dir_all(projects.join("worktree")).unwrap();
    std::fs::write(projects.join("worktree/.git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
    assert_eq!(checkout_repositories(&projects), BTreeSet::from(["owner/repo".into(), "owner/worktree".into()]));
}

/// The name a moved repository had is only an alias of its records: a pull request GitHub finds
/// there now is another one, and no round of the sweep reads it again for nothing.
#[tokio::test]
async fn a_pr_at_the_old_name_of_a_moved_repository_costs_no_read() {
    let f = Fixture::new();
    cancelled_with_closed_pr(&f, "closed", 7);
    command(&f.domain, Actor::System, Command::ObservePrAddress { pr_id: hosted_pr_id("github.com", "owner/repo", 7),
        read: PrRead::Address { repository: "owner/repo".into() }, read_started_at: 1,
        verdict: PrAddressVerdict::Confirmed { key: ProviderPrKey("key".into()), locator: ProviderLocator("locator".into()), current_repository: "owner/moved".into() } });
    f.result(7, "owner/repo", "feature");
    let before = f.domain.state().unwrap();
    f.sweep().await;
    f.sweep().await;
    assert_eq!(f.calls(), "search prs\nsearch prs", "only the searches: nothing of the pull request at the old name is read");
    assert_eq!(f.domain.state().unwrap(), before);
}
