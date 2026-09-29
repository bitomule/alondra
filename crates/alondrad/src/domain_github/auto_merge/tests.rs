use super::*;
use crate::domain_github::FakeObserver;
use alondra_core::domain_v2::{Actor, CommandEnvelope, PROTOCOL_VERSION, TaskState};
use alondra_core::options::{Choice, OptionsFile, PR};
use alondra_core::pr::Check;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Mutex;

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
const MIN: i64 = 60 * 1000;

struct Fixture(PathBuf, DomainRuntime);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Told(Mutex<Vec<(String, String, String)>>);
impl Tell for Told {
    fn tell<'a>(&'a self, _: &'a DomainRuntime, agent: &'a AgentId, task: &'a TaskId, text: String) -> Told_<'a> {
        self.0.lock().unwrap().push((agent.0.clone(), task.0.clone(), text));
        Box::pin(async {})
    }
}
type Told_<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

fn run(domain: &DomainRuntime, actor: Actor, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch,
        command_id: format!("c-{}", state.revision).into(), expected_revision: state.revision, command }, None, || Ok(())).unwrap();
}

fn agent() -> Actor {
    Actor::Agent { agent_id: "a".into() }
}

/// Agent A works Task t of a Request asked with `pr`, has registered PR 7 and presented it.
fn world(pr: &str) -> Fixture {
    let home = std::env::temp_dir().join(format!("alondra-auto-merge-{}", crate::auth::random_hex(8)));
    std::fs::create_dir_all(&home).unwrap();
    let domain = DomainRuntime::open(&home).unwrap();
    run(&domain, Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "A".into() });
    let options = OptionsFile::default().resolve(None, &BTreeMap::from([(PR.to_owned(), Choice::Value(pr.into()))])).unwrap();
    let launch = crate::domain_store::RequestLaunch { project: None, options, issue: None, issue_text: None, attachments: vec![] };
    let state = domain.state().unwrap();
    domain.execute_with(&Actor::User { user_id: "u".into() }, &CommandEnvelope { protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch, command_id: "request".into(), expected_revision: state.revision,
        command: Command::CreateRequest { request_id: "r".into(), body: "Fix it".into() } }, None, Some(&launch), || Ok(())).unwrap();
    run(&domain, Actor::User { user_id: "u".into() }, Command::CreateTask { task_id: "t".into(), request_id: "r".into(),
        title: "Fix".into(), agent_id: Some("a".into()), review_prs: true, dependencies: BTreeSet::new(), stack: None });
    run(&domain, agent(), Command::RegisterPr { task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(),
        repository: "owner/repo".into(), number: 7, head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash] });
    Fixture(home, domain)
}

fn present(domain: &DomainRuntime) {
    run(domain, agent(), Command::PresentResult { attachments: vec![], task_id: "t".into(), interaction_id: "d".into(),
        body: "Ready".into(), targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEAD.into() }], is_final: true });
}

fn check(state: CheckState) -> Check {
    Check { name: "ci".into(), state, url: None }
}

fn facts(checks: Vec<Check>, mergeable: &str) -> PrFacts {
    PrFacts { head: HEAD.into(), base: "main".into(), state: PrState::Open, merged_head: None, checks,
        mergeable: Some(mergeable.into()), draft: false }
}

fn github(facts: PrFacts) -> FakeObserver {
    let observer = FakeObserver::new();
    observer.set(7, Ok(facts));
    observer
}

fn merges(observer: &FakeObserver) -> Vec<(u64, MergeMethod, String)> {
    observer.1.lock().unwrap().clone()
}

#[tokio::test]
async fn a_presented_pr_whose_checks_passed_is_merged_by_the_system_and_its_agent_told() {
    let f = world("auto_merge");
    present(&f.1);
    let observer = github(facts(vec![check(CheckState::Passed)], "MERGEABLE"));
    let told = Told::default();
    run_once(&f.1, &observer, &mut AutoMerging::default(), &told, 0).await.unwrap();
    assert_eq!(merges(&observer), [(7, MergeMethod::Squash, HEAD.to_owned())]);
    let state = f.1.state().unwrap();
    let authorization = state.merge_authorizations.values().next().unwrap();
    assert_eq!((authorization.user_id.as_str(), authorization.attempt), (AUTO_MERGE_USER, MergeAttemptState::Claimed));
    assert_eq!(*told.0.lock().unwrap(), [("a".to_owned(), "t".to_owned(),
        "[Alondra] Alondra merged https://github.com/owner/repo/pull/7 (squash) once its checks passed.".to_owned())]);
    observer.set(7, Ok(PrFacts { state: PrState::Merged, merged_head: Some(HEAD.into()), ..facts(vec![], "UNKNOWN") }));
    crate::domain_github::reconcile(&f.1, &observer).await.unwrap();
    assert_eq!(f.1.state().unwrap().merge_authorizations.values().next().unwrap().attempt, MergeAttemptState::Confirmed);
    run(&f.1, agent(), Command::CompleteTask { task_id: "t".into(), presentation_id: "d".into() });
    assert_eq!(f.1.state().unwrap().tasks[&"t".into()].state, TaskState::Done);
}

#[tokio::test]
async fn what_only_the_agent_can_fix_is_told_once_per_head_and_nothing_is_merged() {
    for (what, facts, says) in [
        ("failed checks", facts(vec![check(CheckState::Failed)], "MERGEABLE"), "The checks of https://github.com/owner/repo/pull/7 failed (ci)"),
        ("conflicts", facts(vec![check(CheckState::Passed)], "CONFLICTING"), "conflicts with main: bring main into the branch"),
        ("draft", PrFacts { draft: true, ..facts(vec![check(CheckState::Passed)], "MERGEABLE") }, "is a draft. Mark it ready"),
    ] {
        let f = world("auto_merge");
        present(&f.1);
        let observer = github(facts);
        let told = Told::default();
        let mut merging = AutoMerging::default();
        run_once(&f.1, &observer, &mut merging, &told, 0).await.unwrap();
        run_once(&f.1, &observer, &mut merging, &told, MIN).await.unwrap();
        let lines = told.0.lock().unwrap().clone();
        assert_eq!(lines.len(), 1, "{what}: {lines:?}");
        assert!(lines[0].2.contains(says), "{what}: {}", lines[0].2);
        assert!(merges(&observer).is_empty(), "{what}");
        assert!(f.1.state().unwrap().merge_authorizations.is_empty(), "{what}");
    }
}

#[tokio::test]
async fn it_waits_for_running_checks_a_late_start_github_and_the_presentation() {
    let f = world("auto_merge");
    let told = Told::default();
    let mut merging = AutoMerging::default();
    let passed = github(facts(vec![check(CheckState::Passed)], "MERGEABLE"));
    run_once(&f.1, &passed, &mut merging, &told, 0).await.unwrap();
    assert!(merges(&passed).is_empty(), "not presented yet");
    present(&f.1);
    for waiting in [facts(vec![check(CheckState::Running)], "MERGEABLE"), facts(vec![check(CheckState::Passed)], "UNKNOWN")] {
        let observer = github(waiting);
        run_once(&f.1, &observer, &mut merging, &told, 10 * MIN).await.unwrap();
        assert!(merges(&observer).is_empty());
    }
    let unchecked = github(facts(vec![], "MERGEABLE"));
    run_once(&f.1, &unchecked, &mut merging, &told, NO_CHECKS_GRACE - 1).await.unwrap();
    assert!(merges(&unchecked).is_empty(), "checks may not have started yet");
    run_once(&f.1, &unchecked, &mut merging, &told, NO_CHECKS_GRACE).await.unwrap();
    assert_eq!(merges(&unchecked).len(), 1, "a PR without checks merges after the grace");
    assert!(told.0.lock().unwrap().iter().all(|(_, _, text)| text.contains("merged")), "waiting tells nobody");
}

#[tokio::test]
async fn a_request_without_auto_merge_keeps_the_users_authorization() {
    let f = world("review");
    present(&f.1);
    let observer = github(facts(vec![check(CheckState::Passed)], "MERGEABLE"));
    let told = Told::default();
    run_once(&f.1, &observer, &mut AutoMerging::default(), &told, 0).await.unwrap();
    assert!(merges(&observer).is_empty());
    assert!(told.0.lock().unwrap().is_empty());
    assert!(f.1.state().unwrap().merge_authorizations.is_empty());
}

#[tokio::test]
async fn a_refused_merge_fails_its_attempt_tells_the_agent_once_and_retries_later() {
    let f = world("auto_merge");
    present(&f.1);
    let observer = github(facts(vec![check(CheckState::Passed)], "MERGEABLE"));
    *observer.2.lock().unwrap() = Some("Base branch was modified".into());
    let told = Told::default();
    let mut merging = AutoMerging::default();
    run_once(&f.1, &observer, &mut merging, &told, 0).await.unwrap();
    let state = f.1.state().unwrap();
    assert_eq!(state.merge_authorizations.values().next().unwrap().attempt, MergeAttemptState::Failed);
    let lines = told.0.lock().unwrap().clone();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].2.starts_with("[Alondra] Alondra could not merge https://github.com/owner/repo/pull/7: Base branch was modified. It tries again in 5 min"), "{}", lines[0].2);
    run_once(&f.1, &observer, &mut merging, &told, 4 * MIN).await.unwrap();
    assert_eq!(merges(&observer).len(), 1, "not before the wait");
    *observer.2.lock().unwrap() = None;
    run_once(&f.1, &observer, &mut merging, &told, 5 * MIN).await.unwrap();
    assert_eq!(merges(&observer).len(), 2);
    let state = f.1.state().unwrap();
    assert_eq!(state.merge_authorizations.values().filter(|a| a.attempt == MergeAttemptState::Claimed).count(), 1, "a fresh authorization");
    assert_eq!(told.0.lock().unwrap().len(), 2);
}
