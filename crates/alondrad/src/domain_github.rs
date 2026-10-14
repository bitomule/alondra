//! GitHub facts for domain v2 pull requests, read with `gh`. An observation records remote
//! reality; a merged PR settles the final delivery that presented it. Merging is
//! `domain_merges`'s, with the same `gh`.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use alondra_core::domain::{
    Actor, Command, CommandEnvelope, DomainState, MergeAttemptState, MergeMethod,
    clip, CardSpec, PrId, PrState, PullRequest, Scope, PROTOCOL_VERSION,
};
use anyhow::Result;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// What GitHub reports for one PR. `merged_head` is the PR head at merge time,
/// never the merge commit: squash and rebase produce a different commit that
/// can never match the presented and authorized head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrFacts {
    pub head: String,
    pub base: String,
    pub state: PrState,
    pub merged_head: Option<String>,
    pub allowed_methods: Vec<MergeMethod>,
}

type Observed<'a> = Pin<Box<dyn Future<Output = Result<PrFacts, String>> + Send + 'a>>;

pub(crate) trait PrObserver: Send + Sync + 'static {
    fn observe<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Observed<'a>;
}

impl<T: PrObserver> PrObserver for Arc<T> {
    fn observe<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Observed<'a> {
        (**self).observe(host, repository, number)
    }
}

pub(crate) struct GhCli {
    program: PathBuf,
}

impl GhCli {
    pub(crate) fn new() -> Self {
        Self { program: "gh".into() }
    }
    #[cfg(test)]
    pub(crate) fn at(program: PathBuf) -> Self {
        Self { program }
    }
}

impl PrObserver for GhCli {
    fn observe<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Observed<'a> {
        Box::pin(async move {
            let url = format!("https://{}/{repository}/pull/{number}", alondra_core::domain::normalize_pr_host(host));
            let run = tokio::process::Command::new(&self.program)
                .args(["pr", "view", &url, "--json", "state,headRefOid,baseRefName"])
                .kill_on_drop(true)
                .output();
            let output = match tokio::time::timeout(Duration::from_secs(30), run).await {
                Ok(Ok(output)) if output.status.success() => output,
                Ok(Ok(output)) => return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
                Ok(Err(error)) => return Err(error.to_string()),
                Err(_) => return Err("gh pr view did not finish within 30 s".into()),
            };
            let value: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
            let mut facts = parse(&value)?;
            facts.allowed_methods = self.repository_methods(host, repository).await;
            Ok(facts)
        })
    }
}

impl GhCli {
    /// The merge methods the repository settings allow, squash first; empty when GitHub does not answer.
    async fn repository_methods(&self, host: &str, repository: &str) -> Vec<MergeMethod> {
        let url = format!("https://{}/{repository}", alondra_core::domain::normalize_pr_host(host));
        let args = ["repo", "view", url.as_str(), "--json", "mergeCommitAllowed,squashMergeAllowed,rebaseMergeAllowed"];
        let Ok(out) = self.run(&args, None, Duration::from_secs(30)).await else { return vec![] };
        let Ok(value) = serde_json::from_slice::<Value>(&out) else { return vec![] };
        parse_methods(&value)
    }

    pub(crate) async fn run(&self, args: &[&str], dir: Option<&std::path::Path>, limit: Duration) -> Result<Vec<u8>, String> {
        let mut command = tokio::process::Command::new(&self.program);
        command.args(args).kill_on_drop(true);
        if let Some(dir) = dir {
            command.current_dir(dir);
        }
        match tokio::time::timeout(limit, command.output()).await {
            Ok(Ok(output)) if output.status.success() => Ok(output.stdout),
            Ok(Ok(output)) => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err(format!("gh {} did not finish within {} s", args.first().copied().unwrap_or_default(), limit.as_secs())),
        }
    }

    /// The open PR GitHub has for the branch checked out in `dir`, as `owner/repo` and number.
    pub(crate) async fn branch_pr(&self, dir: &std::path::Path, limit: Duration) -> Option<(String, u64)> {
        let out = self.run(&["pr", "view", "--json", "url,state"], Some(dir), limit).await.ok()?;
        let value: Value = serde_json::from_slice(&out).ok()?;
        if value["state"].as_str() != Some("OPEN") {
            return None;
        }
        let (repository, number) = value["url"].as_str()?.split_once("://")?.1.split_once('/')?.1.split_once("/pull/")?;
        Some((repository.to_owned(), number.trim_end_matches('/').parse().ok()?))
    }
}

pub(crate) fn pr_url(host: &str, repository: &str, number: u64) -> String {
    format!("https://{}/{repository}/pull/{number}", alondra_core::domain::normalize_pr_host(host))
}

fn parse_methods(value: &Value) -> Vec<MergeMethod> {
    [
        ("squashMergeAllowed", MergeMethod::Squash),
        ("mergeCommitAllowed", MergeMethod::Merge),
        ("rebaseMergeAllowed", MergeMethod::Rebase),
    ]
    .into_iter()
    .filter(|(key, _)| value[*key].as_bool() == Some(true))
    .map(|(_, method)| method)
    .collect()
}

pub(crate) fn parse(value: &Value) -> Result<PrFacts, String> {
    let head = value["headRefOid"].as_str()
        .filter(|head| head.len() == 40 && head.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or("gh reported no PR head")?;
    let base = value["baseRefName"].as_str().filter(|base| !base.is_empty())
        .ok_or("gh reported no PR base")?;
    let state = match value["state"].as_str() {
        Some("OPEN") => PrState::Open,
        Some("CLOSED") => PrState::Closed,
        Some("MERGED") => PrState::Merged,
        other => return Err(format!("gh reported an unknown PR state {other:?}")),
    };
    Ok(PrFacts {
        head: head.to_owned(),
        base: base.to_owned(),
        merged_head: (state == PrState::Merged).then(|| head.to_owned()),
        state,
        allowed_methods: vec![],
    })
}

pub(crate) fn execute_system(domain: &DomainRuntime, command: Command) -> Result<(), StoreError> {
    let state = domain.state().map_err(StoreError::Storage)?;
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: format!("github-{}", crate::auth::random_hex(16)).into(),
        expected_revision: state.revision,
        command,
    };
    domain.execute(&Actor::System, &envelope, None, || Ok(())).map(|_| ())
}

/// Only registered, open PRs need periodic observation. One final observation
/// records a remote close or merge, then removes the PR from this set.
fn watched(state: &DomainState) -> Vec<PullRequest> {
    state.pull_requests.values().filter(|pr| pr.state == PrState::Open).cloned().collect()
}

/// Bring the domain's view of each watched PR up to date with GitHub. A PR
/// GitHub cannot report is left as it was.
pub(crate) async fn reconcile(
    domain: &DomainRuntime, observer: &impl PrObserver, failures: &mut ObservationFailures,
) -> Result<()> {
    let state = domain.state()?;
    for pr in watched(&state) {
        let facts = match observer.observe(&pr.host, &pr.repository, pr.number).await {
            Ok(facts) => facts,
            Err(reason) => {
                failures.failed(domain, &pr, &reason);
                continue;
            }
        };
        failures.recovered(domain, &pr);
        apply(domain, &state, &pr, &facts)?;
    }
    Ok(())
}

const OBSERVATION_FAILURES_BEFORE_PROBLEM: u32 = 5;

/// Consecutive failed reads per PR. The log records each failure up to the
/// threshold; at the threshold the user gets one Problem and the log stops
/// repeating itself.
#[derive(Default)]
pub(crate) struct ObservationFailures(BTreeMap<PrId, u32>);

impl ObservationFailures {
    fn recovered(&mut self, domain: &DomainRuntime, pr: &PullRequest) {
        let reported = self.0.remove(&pr.id).is_some_and(|count| count >= OBSERVATION_FAILURES_BEFORE_PROBLEM);
        if reported {
            let _ = domain.withdraw_problem(&format!("problem-pr-unreadable-{}", pr.id).into());
        }
    }

    fn failed(&mut self, domain: &DomainRuntime, pr: &PullRequest, reason: &str) {
        let count = self.0.entry(pr.id.clone()).or_default();
        *count += 1;
        let url = format!("{}/{}#{}", pr.host, pr.repository, pr.number);
        if *count < OBSERVATION_FAILURES_BEFORE_PROBLEM {
            log!("GitHub could not report {url}: {reason}");
            return;
        }
        if *count > OBSERVATION_FAILURES_BEFORE_PROBLEM {
            return;
        }
        log!("GitHub could not report {url} {count} times in a row: {reason}; raising a Problem");
        let need = clip(&format!("Alondra cannot read PR {}#{} on GitHub", pr.repository, pr.number), CardSpec::NEED_MAX);
        let why = clip(&format!("{reason} (host \"{}\")", pr.host), CardSpec::WHY_MAX);
        let diagnosis = format!(
            "Alondra asked GitHub about {url} {count} times in a row and every read failed, so it cannot see new pushes, merges or closes of this PR. Last error: {reason}. If the host looks wrong, the node can register the same PR again with the right host."
        );
        let _ = domain.raise_problem(
            &format!("problem-pr-unreadable-{}", pr.id),
            Some(&format!("pr-unreadable:{}", pr.id)),
            Scope::Task { task_id: pr.task_id.clone() },
            need, why, diagnosis, vec![],
        );
    }
}

fn apply(domain: &DomainRuntime, state: &DomainState, pr: &PullRequest, facts: &PrFacts) -> Result<()> {
    let unchanged = pr.head == facts.head && pr.base == facts.base
        && pr.state == facts.state && pr.merged_head == facts.merged_head;
    if !unchanged {
        match execute_system(domain, Command::ObservePrState {
            pr_id: pr.id.clone(), head: facts.head.clone(), base: facts.base.clone(),
            state: facts.state, merged_head: facts.merged_head.clone(),
        }) {
            Ok(()) => (),
            Err(StoreError::Domain(error)) => log!("PR observation for {} refused: {error}", pr.id),
            Err(StoreError::Storage(error)) => return Err(error),
        }
    }
    if !facts.allowed_methods.is_empty() && facts.allowed_methods != pr.allowed_methods {
        match execute_system(domain, Command::ObservePrMergeMethods {
            pr_id: pr.id.clone(), allowed_methods: facts.allowed_methods.clone(),
        }) {
            Ok(()) => (),
            Err(StoreError::Domain(error)) => log!("merge methods observation for {} refused: {error}", pr.id),
            Err(StoreError::Storage(error)) => return Err(error),
        }
    }
    // A claimed attempt cannot succeed any more once GitHub merged another
    // head, closed the PR or moved it to a newer head; a merge at the
    // authorized head is confirmed by the observation itself.
    for authorization in state.merge_authorizations.values().filter(|authorization| {
        authorization.pr_id == pr.id
            && matches!(authorization.attempt, MergeAttemptState::Claimed | MergeAttemptState::Uncertain)
    }) {
        let failed = match facts.state {
            PrState::Closed => true,
            PrState::Merged => facts.merged_head.as_deref() != Some(authorization.head.as_str()),
            PrState::Open => facts.head != authorization.head,
        };
        if failed {
            match execute_system(domain, Command::ObserveMergeAttempt {
                authorization_id: authorization.id.clone(), state: MergeAttemptState::Failed,
            }) {
                Ok(()) => (),
                Err(StoreError::Domain(error)) => log!("merge attempt observation for {} refused: {error}", authorization.id),
                Err(StoreError::Storage(error)) => return Err(error),
            }
        }
    }
    Ok(())
}

pub(crate) async fn run(domain: Arc<DomainRuntime>, observer: Arc<impl PrObserver>, poll: Duration) -> Result<()> {
    let mut changed = domain.subscribe();
    let mut failures = ObservationFailures::default();
    loop {
        let mut open = watched(&*domain.state()?);
        if open.is_empty() {
            // No timer exists while there is no open PR. A registration wakes
            // this receiver because it commits a new domain revision.
            changed.changed().await?;
            continue;
        }
        let mut known: BTreeSet<_> = open.iter().map(|pr| pr.id.clone()).collect();
        reconcile(&domain, &*observer, &mut failures).await?;
        open = watched(&*domain.state()?);
        if open.is_empty() {
            continue;
        }
        let mut interval = tokio::time::interval(poll);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await; // consume the immediate tick after the initial read
        loop {
            tokio::select! {
                change = changed.changed() => { change?; }
                _ = interval.tick() => { reconcile(&domain, &*observer, &mut failures).await?; }
            }
            open = watched(&*domain.state()?);
            if open.is_empty() {
                break;
            }
            let current: BTreeSet<_> = open.iter().map(|pr| pr.id.clone()).collect();
            if !current.is_subset(&known) {
                // A hook or register_pr committed a newly registered PR.
                reconcile(&domain, &*observer, &mut failures).await?;
            }
            known = current;
        }
    }
}

#[cfg(test)]
pub(crate) struct FakeObserver(pub std::sync::Mutex<std::collections::BTreeMap<u64, Result<PrFacts, String>>>);
#[cfg(test)]
impl FakeObserver {
    pub(crate) fn new() -> Self {
        Self(std::sync::Mutex::new(Default::default()))
    }
    pub(crate) fn set(&self, number: u64, facts: Result<PrFacts, String>) {
        self.0.lock().unwrap().insert(number, facts);
    }
}
#[cfg(test)]
impl PrObserver for FakeObserver {
    fn observe<'a>(&'a self, _: &'a str, _: &'a str, number: u64) -> Observed<'a> {
        let facts = self.0.lock().unwrap().get(&number).cloned()
            .unwrap_or_else(|| Err("no such PR".into()));
        Box::pin(async move { facts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::*;
    use std::collections::BTreeSet;
    use std::os::unix::fs::PermissionsExt;

    struct Fixture(crate::test_support::Directory);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The task waits in Closing for its node; Alondra records it closed and the task is done.
    fn end_node(domain: &DomainRuntime) {
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].state, TaskState::Closing);
        command(domain, Actor::System, "node-end", Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: None });
    }

    fn command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER: &str = "89abcdef0123456789abcdef0123456789abcdef";

    fn facts(head: &str, state: PrState) -> PrFacts {
        PrFacts { head: head.into(), base: "main".into(), state,
            merged_head: (state == PrState::Merged).then(|| head.to_owned()), allowed_methods: vec![] }
    }

    /// A Task with a registered PR, a presented final and the user's merge authorization.
    fn authorized(domain: &DomainRuntime) {
        command(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
        command(domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        command(domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask {
            task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()),
            review_prs: true, dependencies: BTreeSet::new(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
        command(domain, Actor::Agent { agent_id: "a".into() }, "pr", Command::RegisterPr {
            task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
            number: 7, head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
        });
        command(domain, Actor::Agent { agent_id: "a".into() }, "present", Command::PresentResult {
            checklist: Vec::new(),
            attachments: vec![], task_id: "t".into(), interaction_id: "d".into(), body: "Ready".into(),
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEAD.into() }], is_final: true, nothing_to_review: false, card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
        });
        command(domain, Actor::User { user_id: "u".into() }, "authorize", Command::AuthorizeMerge {
            authorization_id: "auth".into(), presentation_id: "d".into(), pr_id: "pr".into(),
            expected_head: HEAD.into(), method: MergeMethod::Squash,
        });
    }

    fn presented(domain: &DomainRuntime, auto_merge: bool) {
        command(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
        command(domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge, maintenance: false, message: false });
        command(domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask {
            task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()),
            review_prs: true, dependencies: BTreeSet::new(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
        let register = Command::RegisterPr {
            task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
            number: 7, head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
        };
        command(domain, Actor::Agent { agent_id: "a".into() }, "pr", register.clone());
        assert_eq!(crate::domain_api::auto_merge_notice(domain, &register).is_some(), auto_merge);
        command(domain, Actor::Agent { agent_id: "a".into() }, "present", Command::PresentResult {
            checklist: Vec::new(),
            attachments: vec![], task_id: "t".into(), interaction_id: "d".into(), body: "Ready".into(),
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEAD.into() }], is_final: true, nothing_to_review: false, card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
        });
    }

    #[tokio::test]
    async fn reconcile_replaces_the_methods_the_node_declared_with_the_repository_settings() {
        let (_fixture, domain) = open("methods");
        presented(&domain, false);
        let observer = FakeObserver::new();
        let mut github = facts(HEAD, PrState::Open);
        github.allowed_methods = vec![MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase];
        observer.set(7, Ok(github));
        reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
        assert_eq!(
            domain.state().unwrap().pull_requests[&"pr".into()].allowed_methods,
            vec![MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase],
        );
    }

    #[tokio::test]
    async fn reconcile_keeps_the_node_methods_when_github_does_not_report_any() {
        let (_fixture, domain) = open("methods-silent");
        presented(&domain, false);
        let observer = FakeObserver::new();
        observer.set(7, Ok(facts(HEAD, PrState::Open)));
        reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
        assert_eq!(domain.state().unwrap().pull_requests[&"pr".into()].allowed_methods, vec![MergeMethod::Squash]);
    }

    #[tokio::test]
    async fn gh_reports_the_repository_merge_methods_and_stays_silent_when_it_fails() {
        let (fixture, _) = open("gh-methods");
        let program = fixture.0.join("gh");
        std::fs::write(&program, format!("#!/bin/sh\ncase \"$*\" in *\"repo view\"*owner/repo*) printf '%s' '{{\"mergeCommitAllowed\":true,\"squashMergeAllowed\":false,\"rebaseMergeAllowed\":true}}';; *\"repo view\"*) echo 'boom' >&2; exit 1;; *) printf '%s' '{{\"state\":\"OPEN\",\"headRefOid\":\"{HEAD}\",\"baseRefName\":\"main\"}}';; esac\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let gh = GhCli::at(program);
        let known = gh.observe("github.com", "owner/repo", 7).await.unwrap();
        assert_eq!(known.allowed_methods, vec![MergeMethod::Merge, MergeMethod::Rebase]);
        let unknown = gh.observe("github.com", "other/repo", 7).await.unwrap();
        assert!(unknown.allowed_methods.is_empty());
    }

    #[tokio::test]
    async fn an_observed_merge_settles_the_delivery_with_or_without_auto_merge() {
        for auto_merge in [true, false] {
            let (_fixture, domain) = open("observed-merge");
            presented(&domain, auto_merge);
            let observer = FakeObserver::new();
            observer.set(7, Ok(facts(HEAD, PrState::Merged)));
            reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
            let state = domain.state().unwrap();
            assert_eq!(state.pull_requests[&"pr".into()].state, PrState::Merged);
            end_node(&domain);
            assert_eq!(domain.state().unwrap().tasks[&"t".into()].state, TaskState::Done, "auto_merge {auto_merge}");
        }
    }

    #[test]
    fn the_auto_merge_notice_says_alondra_merges_without_protocol_or_git_metadata() {
        let (_fixture, domain) = open("notice");
        presented(&domain, true);
        let register = Command::RegisterPr {
            task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
            number: 7, head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
        };
        let notice = crate::domain_api::auto_merge_notice(&domain, &register).unwrap();
        assert!(notice.contains("Alondra merges it") && notice.contains("do not merge it yourself"), "{notice}");
        for internal in ["github.com", "owner/repo", HEAD, "task_id", "revision", "authorization", "claim"] {
            assert!(!notice.contains(internal), "{internal} leaked in {notice}");
        }
    }

    fn open(_tag: &str) -> (Fixture, DomainRuntime) {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        (Fixture(home), domain)
    }

    #[tokio::test]
    async fn only_open_registered_prs_are_polled() {
        for (tag, remote) in [("closed-pr", PrState::Closed), ("merged-pr", PrState::Merged)] {
            let (_fixture, domain) = open(tag);
            presented(&domain, false);
            assert_eq!(watched(&domain.state().unwrap()).len(), 1);
            let observer = FakeObserver::new();
            observer.set(7, Ok(facts(HEAD, remote)));
            reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
            assert!(watched(&domain.state().unwrap()).is_empty());
            observer.set(7, Err("should not be queried again".into()));
            let before = domain.state().unwrap().revision;
            reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
            assert_eq!(domain.state().unwrap().revision, before);
        }
    }

    #[tokio::test]
    async fn registration_wakes_an_idle_observer() {
        struct SignallingObserver(tokio::sync::mpsc::UnboundedSender<u64>);
        impl PrObserver for SignallingObserver {
            fn observe<'a>(&'a self, _: &'a str, _: &'a str, number: u64) -> Observed<'a> {
                let _ = self.0.send(number);
                Box::pin(async move { Ok(facts(HEAD, PrState::Open)) })
            }
        }

        let (_fixture, domain) = open("wake");
        let domain = Arc::new(domain);
        let (tx, mut calls) = tokio::sync::mpsc::unbounded_channel();
        let worker = tokio::spawn(run(domain.clone(), Arc::new(SignallingObserver(tx)), Duration::from_secs(60)));
        tokio::task::yield_now().await;
        assert!(calls.try_recv().is_err(), "an empty registry makes no GitHub call");
        presented(&domain, false);
        assert_eq!(tokio::time::timeout(Duration::from_secs(1), calls.recv()).await.unwrap(), Some(7));
        worker.abort();
    }

    #[tokio::test]
    async fn registration_during_reconciliation_is_observed_without_waiting_for_the_interval() {
        struct BlockingObserver {
            calls: tokio::sync::mpsc::UnboundedSender<u64>,
            first_release: std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        }
        impl PrObserver for BlockingObserver {
            fn observe<'a>(&'a self, _: &'a str, _: &'a str, number: u64) -> Observed<'a> {
                let calls = self.calls.clone();
                let release = self.first_release.lock().unwrap().take();
                Box::pin(async move {
                    let _ = calls.send(number);
                    if let Some(release) = release {
                        release
                            .await
                            .map_err(|_| "test released observer was dropped".to_owned())?;
                    }
                    Ok(facts(HEAD, PrState::Open))
                })
            }
        }

        let (_fixture, domain) = open("registration-during-reconcile");
        presented(&domain, false);
        let domain = Arc::new(domain);
        let (calls_tx, mut calls) = tokio::sync::mpsc::unbounded_channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let observer = Arc::new(BlockingObserver {
            calls: calls_tx,
            first_release: std::sync::Mutex::new(Some(release_rx)),
        });
        let worker = tokio::spawn(run(domain.clone(), observer, Duration::from_secs(60)));

        assert_eq!(calls.recv().await, Some(7), "the initial PR observation is in flight");
        command(
            &domain,
            Actor::User { user_id: "u".into() },
            "request-2",
            Command::CreateRequest {
                request_id: "r-2".into(),
                body: "Second task".into(),
                auto_merge: false, maintenance: false, message: false,
            },
        );
        command(&domain, Actor::System, "agent-b", Command::RegisterAgent { agent_id: "b".into(), name: "Instance B".into(), at: 0 });
        command(
            &domain,
            Actor::Orchestrator { generation: 0 },
            "task-2",
            Command::CreateTask {
                task_id: "t-2".into(),
                request_id: "r-2".into(),
                title: "Second task".into(),
                agent_id: Some("b".into()),
                review_prs: false,
                dependencies: BTreeSet::new(),
                stack: None,
                launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
            },
        );
        command(
            &domain,
            Actor::Agent { agent_id: "b".into() },
            "pr-2",
            Command::RegisterPr {
                task_id: "t-2".into(),
                pr_id: "pr-2".into(),
                host: "github.com".into(),
                repository: "owner/repo".into(),
                number: 8,
                head: HEAD.into(),
                base: "main".into(),
                allowed_methods: vec![MergeMethod::Squash],
            },
        );

        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while calls.recv().await != Some(8) {}
        })
        .await
        .expect("the newly registered PR is reconciled after the in-flight older snapshot");
        worker.abort();
        let _ = worker.await;
    }

    #[tokio::test]
    async fn reconcile_records_remote_reality_and_the_authorized_merge_completes_the_task() {
        let (_fixture, domain) = open("reconcile");
        authorized(&domain);
        let observer = FakeObserver::new();
        let before = domain.state().unwrap().revision;
        reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
        assert_eq!(domain.state().unwrap().revision, before, "an unreported PR is left alone");
        observer.set(7, Ok(facts(HEAD, PrState::Open)));
        reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
        assert_eq!(domain.state().unwrap().revision, before, "unchanged facts write nothing");

        command(&domain, Actor::System, "claim", Command::ClaimMergeAuthorization {
            authorization_id: "auth".into(), remote_head: HEAD.into(), remote_base: "main".into(), base_verified: true,
        });
        observer.set(7, Ok(facts(HEAD, PrState::Merged)));
        reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
        let state = domain.state().unwrap();
        let pr = &state.pull_requests[&"pr".into()];
        assert_eq!(pr.state, PrState::Merged);
        assert_eq!(pr.merged_head.as_deref(), Some(HEAD), "the merged head is the PR head, not a merge commit");
        assert_eq!(state.merge_authorizations[&"auth".into()].attempt, MergeAttemptState::Confirmed);
        end_node(&domain);
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].state, TaskState::Done, "the merge of the authorized head completes the Task");
    }

    #[tokio::test]
    async fn a_claimed_attempt_fails_when_github_merged_another_head_or_closed_the_pr() {
        for (tag, remote) in [("other-head", facts(OTHER, PrState::Merged)), ("closed", facts(HEAD, PrState::Closed)),
            ("newer-head", facts(OTHER, PrState::Open))] {
            let (_fixture, domain) = open(tag);
            authorized(&domain);
            domain.store.lock().unwrap().claim_as_node("auth", "a");
            domain.reload();
            let observer = FakeObserver::new();
            observer.set(7, Ok(remote.clone()));
            reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
            let state = domain.state().unwrap();
            assert_eq!(state.pull_requests[&"pr".into()].state, remote.state, "{tag}");
            assert_eq!(state.merge_authorizations[&"auth".into()].attempt, MergeAttemptState::Failed, "{tag}");
            assert_eq!(state.tasks[&"t".into()].wrap_up.asked_at.is_some(), remote.state != PrState::Open,
                "{tag}: a merge or a close settles the accepted delivery, a newer head does not");
        }
    }

    /// PR #727: a node claimed the merge of its first delivery, then resolved conflicts and
    /// delivered a newer head; the claim left behind held every new merge back.
    #[tokio::test]
    async fn a_node_claim_of_a_superseded_delivery_is_reconciled_and_the_user_can_merge_again() {
        let (_fixture, domain) = open("superseded-claim");
        authorized(&domain);
        domain.store.lock().unwrap().claim_as_node("auth", "a");
        domain.reload();
        let observer = FakeObserver::new();
        observer.set(7, Ok(facts(OTHER, PrState::Open)));
        reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
        assert_eq!(domain.state().unwrap().merge_authorizations[&"auth".into()].attempt, MergeAttemptState::Failed);
        command(&domain, Actor::Agent { agent_id: "a".into() }, "present-2", Command::PresentResult {
            checklist: Vec::new(),
            attachments: vec![], task_id: "t".into(), interaction_id: "d2".into(), body: "Conflicts resolved".into(),
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: OTHER.into() }], is_final: true,
            nothing_to_review: false,
            card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
        });
        let projection = domain.state().unwrap().projection();
        let merge = projection.inbox.iter().find(|item| item.id == "interaction:d2").unwrap()
            .available_actions.iter().find(|action| action.kind == "authorize_merge").unwrap();
        assert!(merge.enabled, "{merge:?}");
        command(&domain, Actor::User { user_id: "u".into() }, "authorize-2", Command::AuthorizeMerge {
            authorization_id: "auth-2".into(), presentation_id: "d2".into(), pr_id: "pr".into(),
            expected_head: OTHER.into(), method: MergeMethod::Squash,
        });
    }

    #[tokio::test]
    async fn gh_output_is_parsed_from_a_fake_binary_and_failures_are_reported() {
        let (fixture, _) = open("gh");
        let program = fixture.0.join("gh");
        let log = fixture.0.join("calls");
        std::fs::write(&program, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\ncase \"$*\" in *pull/7*) printf '%s' '{{\"state\":\"MERGED\",\"headRefOid\":\"{HEAD}\",\"baseRefName\":\"main\",\"mergeCommit\":{{\"oid\":\"{OTHER}\"}}}}';; *pull/8*) printf '%s' '{{\"state\":\"OPEN\",\"headRefOid\":\"short\",\"baseRefName\":\"main\"}}';; *) echo 'no pull requests found' >&2; exit 1;; esac\n", log.display())).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let gh = GhCli::at(program);
        let merged = gh.observe("github.com", "owner/repo", 7).await.unwrap();
        assert_eq!(merged, PrFacts { head: HEAD.into(), base: "main".into(), state: PrState::Merged, merged_head: Some(HEAD.into()), allowed_methods: vec![] });
        assert_eq!(gh.observe("github.com", "owner/repo", 8).await.unwrap_err(), "gh reported no PR head");
        assert!(gh.observe("github.com", "owner/repo", 9).await.unwrap_err().contains("no pull requests found"));
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.contains("pr view https://github.com/owner/repo/pull/7 --json state,headRefOid,baseRefName"), "{calls}");
    }

    #[tokio::test]
    async fn a_registration_with_the_bare_host_github_is_polled_at_github_com() {
        let (fixture, _) = open("bare-host");
        let program = fixture.0.join("gh");
        let log = fixture.0.join("calls");
        std::fs::write(&program, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nprintf '%s' '{{\"state\":\"OPEN\",\"headRefOid\":\"{HEAD}\",\"baseRefName\":\"main\"}}'\n", log.display())).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        GhCli::at(program).observe("github", "owner/repo", 7).await.unwrap();
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.contains("pr view https://github.com/owner/repo/pull/7"), "{calls}");
    }

    #[tokio::test]
    async fn a_pr_that_cannot_be_read_five_times_raises_one_problem_and_recovery_withdraws_it() {
        let (_fixture, domain) = open("unreadable");
        presented(&domain, false);
        let observer = FakeObserver::new();
        observer.set(7, Err("error connecting to github".into()));
        let mut failures = ObservationFailures::default();
        let problems = |domain: &DomainRuntime| domain.state().unwrap().interactions.values().cloned()
            .filter(|card| card.card.kind == alondra_core::domain::CardKind::Problem)
            .collect::<Vec<_>>();
        for _ in 0..4 {
            reconcile(&domain, &observer, &mut failures).await.unwrap();
        }
        assert!(problems(&domain).is_empty(), "a few failures are a flaky network");
        for _ in 0..3 {
            reconcile(&domain, &observer, &mut failures).await.unwrap();
        }
        let raised = problems(&domain);
        assert_eq!(raised.len(), 1, "one Problem however long it keeps failing");
        assert_eq!(raised[0].scope, Scope::Task { task_id: "t".into() });
        assert!(raised[0].body.contains("error connecting to github") && raised[0].body.contains("github.com/owner/repo#7"));
        observer.set(7, Ok(facts(HEAD, PrState::Open)));
        reconcile(&domain, &observer, &mut failures).await.unwrap();
        assert!(problems(&domain).iter().all(|card| card.withdrawn), "reading works again");
    }
}
