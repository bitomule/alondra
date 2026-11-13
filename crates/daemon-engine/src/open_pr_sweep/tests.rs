use super::*;
use alondra_core::domain::*;
use std::os::unix::fs::PermissionsExt;

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
    command(domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
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

struct Fixture {
    root: crate::test_support::Directory,
    domain: DomainRuntime,
    options: OptionsFile,
    gh: GhCli,
}

impl Fixture {
    fn new() -> Self {
        let root = crate::test_support::Directory::new();
        std::fs::create_dir_all(root.join("home")).unwrap();
        let domain = DomainRuntime::open(&root.join("home")).unwrap();
        let projects = root.join("projects");
        std::fs::create_dir_all(projects.join("checkout/.git")).unwrap();
        std::fs::write(projects.join("checkout/.git/config"), "[remote \"origin\"]\nurl = https://github.com/owner/repo.git\n").unwrap();
        let options = OptionsFile { projects_root: Some(projects.to_string_lossy().into()), ..Default::default() };
        let program = root.join("gh");
        std::fs::write(&program, format!(r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
if [ "$1 $2" = 'search prs' ]; then
    cat '{}'
elif [ "$1 $2" = 'pr view' ]; then
    case "$5" in
        headRefName) cat '{}' ;;
        *) cat '{}' ;;
    esac
elif [ "$1 $2" = 'repo view' ]; then
    printf '%s' '{{"squashMergeAllowed":true}}'
else
    exit 9
fi
"#, root.join("calls").display(), root.join("search.json").display(), root.join("branch.json").display(), root.join("status.json").display())).unwrap();
        std::fs::write(root.join("status.json"), r#"{"state":"OPEN","headRefOid":"1111111111111111111111111111111111111111","baseRefName":"main","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[]}"#).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { root, domain, options, gh: GhCli::at(program) }
    }
    fn result(&self, number: u64, repo: &str, branch: &str) {
        std::fs::write(self.root.join("search.json"), serde_json::to_vec(&serde_json::json!([{
            "repository": {"nameWithOwner": repo}, "number": number, "url": format!("https://github.com/{repo}/pull/{number}")
        }])).unwrap()).unwrap();
        std::fs::write(self.root.join("branch.json"), serde_json::to_vec(&serde_json::json!({"headRefName":branch})).unwrap()).unwrap();
    }
    async fn sweep(&self) { sweep(&self.domain, &self.gh, &self.options, &self.root).await.unwrap(); }
    fn calls(&self) -> String { std::fs::read_to_string(self.root.join("calls")).unwrap() }
}

#[tokio::test]
async fn release_branches_are_excluded_and_prefixes_are_configurable() {
    let mut f = Fixture::new();
    f.result(117, "owner/repo", "release-plz-2026");
    f.sweep().await;
    assert!(f.domain.state().unwrap().pr_adoptions.is_empty());
    f.options.pr_sweep_ignore_branch_prefixes = vec!["ship-".into()];
    f.sweep().await;
    assert_eq!(f.domain.state().unwrap().pr_adoptions.len(), 1);
}

#[tokio::test]
async fn a_live_tasks_registered_pr_is_skipped_without_reading_its_branch() {
    let f = Fixture::new();
    task(&f.domain, "live", Some(7));
    f.result(7, "owner/repo", "feature");
    f.sweep().await;
    assert_eq!(f.calls().lines().count(), 1);
    assert!(f.domain.state().unwrap().pr_adoptions.is_empty());
}

#[tokio::test]
async fn a_live_tasks_worktree_branch_is_skipped() {
    let f = Fixture::new();
    task(&f.domain, "live", None);
    f.result(7, "owner/repo", "worktree-live");
    f.sweep().await;
    assert!(f.domain.state().unwrap().pr_adoptions.is_empty());
}

#[tokio::test]
async fn two_sweeps_and_a_restart_request_adoption_only_once() {
    let f = Fixture::new();
    f.result(7, "owner/repo", "feature");
    f.sweep().await;
    let before = f.domain.state().unwrap().requests.len();
    f.sweep().await;
    assert_eq!(f.domain.state().unwrap().requests.len(), before);
    assert_eq!(f.calls().lines().filter(|c| c.starts_with("search prs")).count(), 2);
    assert_eq!(f.calls().lines().filter(|c| c.starts_with("pr view")).count(), 1);
    let home = f.root.join("home");
    drop(f.domain);
    let domain = DomainRuntime::open(&home).unwrap();
    sweep(&domain, &f.gh, &f.options, &f.root).await.unwrap();
    let state = domain.state().unwrap();
    assert_eq!(state.requests.len(), before);
    assert_eq!(state.pr_adoptions.len(), 1);
    assert!(state.requests.values().any(|r| r.body.contains("Adopt the PR owner/repo#7: deliver it or close it with the reason")));
}

#[tokio::test]
async fn a_repository_outside_the_projects_root_is_ignored() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.root.join("outside/.git")).unwrap();
    std::fs::write(f.root.join("outside/.git/config"), "[remote \"origin\"]\nurl = https://github.com/owner/outside.git\n").unwrap();
    f.result(7, "owner/outside", "feature");
    f.sweep().await;
    assert_eq!(f.calls().lines().count(), 1);
    assert!(f.domain.state().unwrap().pr_adoptions.is_empty());
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
    assert!(state.pr_adoptions.is_empty());
}

#[tokio::test]
async fn the_sweep_closes_the_open_pr_of_a_cancelled_task_once_and_the_close_survives_a_restart() {
    let f = Fixture::new();
    let pr_id = hosted_pr_id("github.com", "owner/repo", 7);
    task(&f.domain, "closed", Some(7));
    command(&f.domain, user(), Command::CancelTask { task_id: "closed".into(), reason: None });
    command(&f.domain, Actor::System, Command::RecordNodeEnd { task_id: "closed".into(), end: NodeEnd::Closed, agent_id: None });
    command(&f.domain, Actor::System, Command::ObservePrState { pr_id: pr_id.clone(),
        head: "1".repeat(40), base: "main".into(), state: PrState::Closed, merged_head: None });
    f.result(7, "owner/repo", "feature");
    f.sweep().await;
    let state = f.domain.state().unwrap();
    let task = &state.tasks[&"closed".into()];
    assert_eq!(task.open_pr, Some(OpenPrPending::Close { comment: "Cancelled in Alondra: Work".into() }));
    assert_eq!((task.state, state.pull_requests[&pr_id].state), (TaskState::Cancelled, PrState::Open));
    assert!(task.final_presentation.is_none() && state.pr_adoptions.is_empty(), "it is closed, not delivered");
    drop(state);
    f.sweep().await;
    let home = f.root.join("home");
    drop(f.domain);
    let domain = DomainRuntime::open(&home).unwrap();
    assert!(matches!(domain.state().unwrap().tasks[&"closed".into()].open_pr, Some(OpenPrPending::Close { .. })));
    let before = domain.state().unwrap();
    sweep(&domain, &f.gh, &f.options, &f.root).await.unwrap();
    assert_eq!(domain.state().unwrap(), before, "another sweep after restart leaves the pending close alone");
    let calls = std::fs::read_to_string(f.root.join("calls")).unwrap();
    assert_eq!(calls.lines().filter(|call| call.starts_with("pr view")).count(), 2,
        "only the first sweep reads the branch and status");
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
    let domain = DomainRuntime::open(&home).unwrap();
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
    assert_eq!(f.calls().lines().count(), 1);
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
    assert!(!crate::auto_launch::candidate(&state, task));
    let delivery = &state.interactions[task.final_presentation.as_ref().unwrap()];
    assert_eq!(alondra_core::open_pr_delivery::OpenPrDelivery::from_body(&delivery.body).unwrap().readiness,
        alondra_core::open_pr_delivery::PrReadiness::ChecksRunning { names: vec!["test".into()] });

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
