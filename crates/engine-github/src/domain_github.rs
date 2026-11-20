//! GitHub facts for domain v2 pull requests, read with `gh`. An observation records remote
//! reality; a merged PR settles the final delivery that presented it. Merging is
//! `domain_merges`'s, with the same `gh`.
use alondra_core::clock;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use alondra_domain_rules::{
    Command, CommandEnvelope, DomainState, MergeAttemptState, OpenPrPending, OpenPrStatus, OpenedPr,
    clip, hosted_pr_id, CardSpec, PrChecks, PrId, PrMergeable, PrState, PullRequest, Scope, TaskId, PROTOCOL_VERSION,
};
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use alondra_engine_ports::github::GitHub;
pub use alondra_engine_ports::github::{Closed, GhFailure, HttpAnswer, Observed, PrFacts, PrObserver, pr_url};

pub fn execute_system(domain: &DomainRuntime, command: Command) -> Result<(), StoreError> {
    let state = domain.state().map_err(StoreError::Storage)?;
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: format!("github-{}", crate::auth::random_hex(16)).into(),
        expected_revision: state.revision,
        command,
    };
    domain.execute_as_system(envelope).map(|_| ())
}

/// Only registered, open PRs need periodic observation. One final observation
/// records a remote close or merge, then removes the PR from this set.
fn watched(state: &DomainState) -> Vec<PullRequest> {
    state.pull_requests.values().filter(|pr| pr.state == PrState::Open).cloned().collect()
}

/// What the watcher waits on: the open PRs and the ones a node was seen opening.
fn awaited(state: &DomainState) -> BTreeSet<PrId> {
    let opened = state.tasks.values().flat_map(|task| &task.opened_prs).map(|pr| hosted_pr_id(&pr.host, &pr.repository, pr.number));
    watched(state).into_iter().map(|pr| pr.id).chain(opened).collect()
}

fn reading_prs(state: &DomainState) -> BTreeSet<(TaskId, OpenedPr)> {
    state.tasks.values().flat_map(|task| task.opened_prs.iter().cloned().map(|pr| (task.id.clone(), pr))).collect()
}

/// Reads from GitHub each PR a node was seen opening, of `task` or of every task, and makes it its
/// task's. One GitHub cannot report stays noted: its delivery waits, and the next round tries again.
pub async fn read_opened(domain: &DomainRuntime, observer: &impl PrObserver, task: Option<&TaskId>) -> Result<()> {
    let state = domain.state()?;
    let opened: Vec<(TaskId, OpenedPr)> = state.tasks.values().filter(|t| task.is_none_or(|task| *task == t.id))
        .flat_map(|t| t.opened_prs.iter().map(|pr| (t.id.clone(), pr.clone()))).collect();
    for (task_id, pr) in opened {
        let facts = match observer.observe(&pr.host, &pr.repository, pr.number).await {
            Ok(facts) if !facts.allowed_methods.is_empty() || state.task_prs(&state.tasks[&task_id])
                .any(|known| known.host == pr.host && known.repository == pr.repository && known.number == pr.number) => facts,
            Ok(_) => {
                log!("GitHub does not say how {}#{} merges yet: its task keeps waiting for it", pr.repository, pr.number);
                continue;
            }
            Err(reason) => {
                log!("could not read {}#{}, which {task_id} opened: {reason}", pr.repository, pr.number);
                continue;
            }
        };
        match execute_system(domain, Command::RecordOpenedPr {
            task_id, host: pr.host, repository: pr.repository, number: pr.number, head: facts.head, base: facts.base,
            state: facts.state, merged_head: facts.merged_head, allowed_methods: facts.allowed_methods,
        }) {
            Ok(()) => (),
            Err(StoreError::Domain(error)) => log!("the PR a node opened was refused: {error}"),
            Err(StoreError::Storage(error)) => return Err(error),
        }
    }
    Ok(())
}

/// What a node's delivery is judged on, read when it presents: the open PR of each branch the task
/// works on (a final delivery only, for a PR no command showed), what the node was seen opening,
/// and where its registered PRs are now. A closed PR can have reopened; a merged PR cannot.
pub async fn catch_up_task(domain: &DomainRuntime, github: &GitHub, task_id: &TaskId, is_final: bool) -> Result<()> {
    catch_up_task_using(domain, github, &github.observer, task_id, is_final).await
}

/// Branches and the PRs the node opened are read from `github`; registered PRs use the caller's
/// source of current facts.
pub async fn catch_up_task_using(domain: &DomainRuntime, github: &GitHub, source: &impl PrObserver, task_id: &TaskId, is_final: bool) -> Result<()> {
    let state = domain.state()?;
    let Some(task) = state.tasks.get(task_id) else { return Ok(()) };
    if is_final {
        for (path, _) in task.open_worktrees() {
            let Some((host, repository, number)) = github.reads.branch_pr(std::path::Path::new(path)).await else { continue };
            match execute_system(domain, Command::NotePrOpened { task_id: task_id.clone(), host, repository, number }) {
                Ok(()) => (),
                Err(StoreError::Domain(error)) => log!("the open PR of {path} was refused: {error}"),
                Err(StoreError::Storage(error)) => return Err(error),
            }
        }
    }
    read_opened(domain, &github.observer, Some(task_id)).await?;
    let state = domain.state()?;
    let Some(task) = state.tasks.get(task_id) else { return Ok(()) };
    let prs: Vec<&PullRequest> = state.task_prs(task)
        .filter(|pr| pr.state == PrState::Open || (is_final && pr.state == PrState::Closed)).collect();
    if is_final || prs.len() > 1 {
        for pr in prs {
            match source.observe(&pr.host, &pr.repository, pr.number).await {
                Ok(facts) => { apply(domain, &state, pr, &facts)?; }
                Err(reason) if is_final => anyhow::bail!("Could not read {}#{} before the final delivery: {reason}", pr.repository, pr.number),
                Err(_) => (),
            }
        }
    }
    Ok(())
}

/// Bring the domain's view of each watched PR up to date with GitHub. A PR
/// GitHub cannot report is left as it was.
pub async fn reconcile(
    domain: &DomainRuntime, observer: &impl PrObserver, failures: &mut ObservationFailures,
) -> Result<()> {
    deliver_held(domain, observer).await?;
    read_opened(domain, observer, None).await?;
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
        settle(domain, &pr, &facts)?;
        if facts.state == PrState::Open
            && let Some(OpenPrPending::Close { comment }) = pending(&state, &pr)
            && failures.close_due(&pr.id)
        {
            match observer.close(&pr.host, &pr.repository, pr.number, comment).await {
                Ok(()) => {
                    failures.closed(domain, &pr);
                    if let Ok(closed) = observer.observe(&pr.host, &pr.repository, pr.number).await {
                        apply(domain, &*domain.state()?, &pr, &closed)?;
                    }
                }
                Err(reason) => failures.close_failed(domain, &pr, &reason),
            }
        }
    }
    Ok(())
}

/// What Alondra still does with `pr` because its task was closed while it was open.
fn pending<'a>(state: &'a DomainState, pr: &PullRequest) -> Option<&'a OpenPrPending> {
    let task = state.tasks.get(&pr.task_id)?;
    task.open_pr.as_ref().filter(|pending| match pending {
        OpenPrPending::Close { .. } => task.pr_ids().any(|id| *id == pr.id),
        OpenPrPending::Deliver { .. } => state.open_pr_of(&task.id).is_some_and(|open| open.id == pr.id),
    })
}

/// A close an older build left waiting for GitHub reaches the user now: as GitHub reports the PR
/// when it answers, as Alondra last knew it when it does not.
async fn deliver_held(domain: &DomainRuntime, observer: &impl PrObserver) -> Result<()> {
    let state = domain.state()?;
    for pr in watched(&state).into_iter().filter(|pr| matches!(pending(&state, pr), Some(OpenPrPending::Deliver { .. }))) {
        let status = match observer.observe(&pr.host, &pr.repository, pr.number).await {
            Ok(facts) => {
                apply(domain, &*domain.state()?, &pr, &facts)?;
                facts.status()
            }
            Err(reason) => {
                log!("delivering the open PR {} as last known: GitHub did not report it: {reason}", pr.id);
                OpenPrStatus { state: pr.state, head: pr.head.clone(), base: pr.base.clone(), mergeable: PrMergeable::Computing, checks: PrChecks::None }
            }
        };
        settle_status(domain, &pr, status)?;
    }
    Ok(())
}

/// Hands what GitHub reported to the task closed while its PR was open, without waiting for its
/// checks: only the merge depends on them.
fn settle(domain: &DomainRuntime, pr: &PullRequest, facts: &PrFacts) -> Result<()> {
    settle_status(domain, pr, facts.status())
}

fn settle_status(domain: &DomainRuntime, pr: &PullRequest, status: OpenPrStatus) -> Result<()> {
    let state = domain.state()?;
    if !matches!(pending(&state, pr), Some(OpenPrPending::Deliver { .. })) {
        return Ok(());
    }
    match execute_system(domain, Command::SettleOpenPr { task_id: pr.task_id.clone(), status }) {
        Ok(()) => Ok(()),
        Err(StoreError::Domain(error)) => {
            log!("settling the open PR {} refused: {error}", pr.id);
            Ok(())
        }
        Err(StoreError::Storage(error)) => Err(error),
    }
}

fn pending_prs(state: &DomainState) -> BTreeSet<PrId> {
    watched(state).into_iter().filter(|pr| pending(state, pr).is_some()).map(|pr| pr.id).collect()
}

const OBSERVATION_FAILURES_BEFORE_PROBLEM: u32 = 5;
const CLOSE_FAILURES_BEFORE_PROBLEM: u32 = 5;
const CLOSE_WAIT_MAX_ROUNDS: u32 = 32;

/// Consecutive failed reads per PR. The log records each failure up to the
/// threshold; at the threshold the user gets one Problem and the log stops
/// repeating itself. Closes that GitHub refused count apart: each failure
/// doubles the rounds Alondra lets pass before it tries again.
#[derive(Default)]
pub struct ObservationFailures(BTreeMap<PrId, u32>, BTreeMap<PrId, CloseFailure>);

#[derive(Default)]
struct CloseFailure {
    count: u32,
    wait: u32,
}

impl ObservationFailures {
    fn recovered(&mut self, domain: &DomainRuntime, pr: &PullRequest) {
        let reported = self.0.remove(&pr.id).is_some_and(|count| count >= OBSERVATION_FAILURES_BEFORE_PROBLEM);
        if reported {
            let _ = domain.withdraw_problem(&format!("problem-pr-unreadable-{}", pr.slug()).into());
        }
    }

    fn close_due(&mut self, pr: &PrId) -> bool {
        match self.1.get_mut(pr) {
            Some(failure) if failure.wait > 0 => {
                failure.wait -= 1;
                false
            }
            _ => true,
        }
    }

    fn closed(&mut self, domain: &DomainRuntime, pr: &PullRequest) {
        if self.1.remove(&pr.id).is_some_and(|failure| failure.count >= CLOSE_FAILURES_BEFORE_PROBLEM) {
            let _ = domain.withdraw_problem(&format!("problem-pr-close-{}", pr.slug()).into());
        }
    }

    fn close_failed(&mut self, domain: &DomainRuntime, pr: &PullRequest, reason: &str) {
        let failure = self.1.entry(pr.id.clone()).or_default();
        failure.count += 1;
        failure.wait = (1 << (failure.count - 1).min(5)).min(CLOSE_WAIT_MAX_ROUNDS);
        let url = format!("{}/{}#{}", pr.host, pr.repository, pr.number);
        log!("Alondra could not close {url} on GitHub ({} in a row): {reason}", failure.count);
        if failure.count != CLOSE_FAILURES_BEFORE_PROBLEM {
            return;
        }
        let need = clip(&format!("Close PR {}#{} on GitHub", pr.repository, pr.number), CardSpec::NEED_MAX);
        let why = clip(&format!("Its task was cancelled and GitHub refused to close it {} times in a row: {reason}", failure.count), CardSpec::WHY_MAX);
        let diagnosis = format!(
            "The task of {url} was cancelled, so Alondra closes its PR, and GitHub refused it {} times in a row. Last error: {reason}. Alondra keeps trying, less and less often; closing the PR on GitHub yourself also ends this.",
            failure.count);
        let _ = domain.raise_problem(
            &format!("problem-pr-close-{}", pr.slug()), None,
            Some(&format!("pr-close:{}", pr.slug())),
            Scope::Task { task_id: pr.task_id.clone() },
            need, why, diagnosis, vec![],
        );
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
            &format!("problem-pr-unreadable-{}", pr.slug()), None,
            Some(&format!("pr-unreadable:{}", pr.slug())),
            Scope::Task { task_id: pr.task_id.clone() },
            need, why, diagnosis, vec![],
        );
    }
}

/// Records what GitHub reported of one PR, outside the watcher's round.
pub fn observe(domain: &DomainRuntime, pr_id: &PrId, facts: &PrFacts) -> Result<()> {
    let state = domain.state()?;
    let Some(pr) = state.pull_requests.get(pr_id) else { return Ok(()) };
    apply(domain, &state, pr, facts)
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

pub async fn run(domain: Arc<DomainRuntime>, observer: Arc<impl PrObserver>, poll: Duration) -> Result<()> {
    let mut changed = domain.subscribe();
    let mut failures = ObservationFailures::default();
    loop {
        let mut open = awaited(&*domain.state()?);
        if open.is_empty() {
            // No timer exists while there is no open PR. A registration wakes
            // this receiver because it commits a new domain revision.
            changed.changed().await?;
            continue;
        }
        let mut known = open;
        let mut known_pending = pending_prs(&*domain.state()?);
        reconcile(&domain, &*observer, &mut failures).await?;
        let mut known_reading = reading_prs(&*domain.state()?);
        open = awaited(&*domain.state()?);
        if open.is_empty() {
            continue;
        }
        let mut interval = clock::interval(poll);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await; // consume the immediate tick after the initial read
        loop {
            tokio::select! {
                change = changed.changed() => { change?; }
                _ = interval.tick() => { reconcile(&domain, &*observer, &mut failures).await?; }
            }
            open = awaited(&*domain.state()?);
            if open.is_empty() {
                break;
            }
            let current_pending = pending_prs(&*domain.state()?);
            let current_reading = reading_prs(&*domain.state()?);
            if !open.is_subset(&known) || !current_pending.is_subset(&known_pending) || !current_reading.is_subset(&known_reading) {
                // A node opened a PR, a new one was registered, or a task closed while its PR is open.
                reconcile(&domain, &*observer, &mut failures).await?;
            }
            known = open.clone();
            known_reading = reading_prs(&*domain.state()?);
            known_pending = pending_prs(&*domain.state()?);
        }
    }
}

#[cfg(any(test, feature = "test-hooks"))]
/// GitHub from memory: the facts of each PR, the closes it took, the ones it was asked for, and
/// the error it refuses closes with, if any. A close it takes leaves the PR closed.
pub struct FakeObserver(
    pub std::sync::Mutex<std::collections::BTreeMap<u64, Result<PrFacts, String>>>,
    pub std::sync::Mutex<Vec<(u64, String)>>,
    pub std::sync::Mutex<Option<String>>,
    pub std::sync::atomic::AtomicUsize,
);
#[cfg(any(test, feature = "test-hooks"))]
impl FakeObserver {
    pub fn new() -> Self {
        Self(Default::default(), Default::default(), Default::default(), Default::default())
    }
    pub fn set(&self, number: u64, facts: Result<PrFacts, String>) {
        self.0.lock().unwrap().insert(number, facts);
    }
}
#[cfg(any(test, feature = "test-hooks"))]
impl PrObserver for FakeObserver {
    fn observe<'a>(&'a self, _: &'a str, _: &'a str, number: u64) -> Observed<'a> {
        let facts = self.0.lock().unwrap().get(&number).cloned()
            .unwrap_or_else(|| Err("no such PR".into()));
        Box::pin(async move { facts })
    }
    fn close<'a>(&'a self, _: &'a str, _: &'a str, number: u64, comment: &'a str) -> Closed<'a> {
        self.3.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let refused = self.2.lock().unwrap().clone();
        if refused.is_none() {
            self.1.lock().unwrap().push((number, comment.to_owned()));
            if let Some(Ok(facts)) = self.0.lock().unwrap().get_mut(&number) {
                facts.state = PrState::Closed;
            }
        }
        Box::pin(async move { refused.map_or(Ok(()), Err) })
    }
}

#[cfg(any(test, feature = "test-hooks"))]
pub mod tests {
    #[cfg(test)]
    use alondra_store_sqlite::AsSqlite as _;
    use super::*;
    use alondra_domain_rules::*;
    use std::collections::BTreeSet;

    pub struct Fixture(crate::test_support::Directory);
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

    #[cfg(test)]
    use alondra_github_gh::GhCli;

    #[cfg(test)]
    fn github(gh: GhCli) -> GitHub {
        GitHub::of(Arc::new(gh))
    }

    pub const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER: &str = "89abcdef0123456789abcdef0123456789abcdef";

    fn facts(head: &str, state: PrState) -> PrFacts {
        PrFacts { head: head.into(), base: "main".into(), state,
            merged_head: (state == PrState::Merged).then(|| head.to_owned()), allowed_methods: vec![],
            mergeable: PrMergeable::Computing, checks: PrChecks::None }
    }

    /// A Task with a registered PR, a presented final and the user's merge authorization.
    fn authorized(domain: &DomainRuntime) {
        command(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
        command(domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        command(domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, continues: None,
            task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()),
            review_prs: true, dependencies: BTreeSet::new(), stack: None,
            launch: Some(alondra_domain_rules::LaunchPlan::new("node", "/tmp")),
        });
        command(domain, Actor::Agent { agent_id: "a".into() }, "pr", Command::RegisterPr {
            task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
            number: 7, head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
        });
        command(domain, Actor::Agent { agent_id: "a".into() }, "present", Command::PresentResult {
            checklist: Vec::new(),
            attachments: vec![], task_id: "t".into(), interaction_id: "d".into(), body: "Ready".into(),
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEAD.into() }], is_final: true, nothing_to_review: false, card: alondra_domain_rules::CardSpec::delivery("Review the result", &[], true),
        });
        command(domain, Actor::User { user_id: "u".into() }, "authorize", Command::AuthorizeMerge {
            authorization_id: "auth".into(), presentation_id: "d".into(), pr_id: "pr".into(),
            expected_head: HEAD.into(), method: MergeMethod::Squash, despite_pending_message: false,
        });
    }

    pub fn presented(domain: &DomainRuntime, auto_merge: bool) {
        command(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
        command(domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge, maintenance: false, message: false });
        command(domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, continues: None,
            task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()),
            review_prs: true, dependencies: BTreeSet::new(), stack: None,
            launch: Some(alondra_domain_rules::LaunchPlan::new("node", "/tmp")),
        });
        let register = Command::RegisterPr {
            task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
            number: 7, head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
        };
        command(domain, Actor::Agent { agent_id: "a".into() }, "pr", register.clone());
        command(domain, Actor::Agent { agent_id: "a".into() }, "present", Command::PresentResult {
            checklist: Vec::new(),
            attachments: vec![], task_id: "t".into(), interaction_id: "d".into(), body: "Ready".into(),
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEAD.into() }], is_final: true, nothing_to_review: false, card: alondra_domain_rules::CardSpec::delivery("Review the result", &[], true),
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

    fn opened(domain: &DomainRuntime) {
        command(domain, Actor::System, "opened", Command::NotePrOpened {
            task_id: "t".into(), host: "github.com".into(), repository: "owner/site".into(), number: 6,
        });
    }

    #[tokio::test]
    async fn a_pr_a_node_opened_becomes_its_tasks_with_what_github_reports() {
        let (_fixture, domain) = open("opened");
        authorized(&domain);
        opened(&domain);
        assert!(awaited(&domain.state().unwrap()).contains(&hosted_pr_id("github.com", "owner/site", 6)), "the watcher wakes for it");

        let observer = FakeObserver::new();
        observer.set(6, Err("gh: could not resolve host".into()));
        observer.set(7, Ok(facts(HEAD, PrState::Open)));
        reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].opened_prs.len(), 1, "one GitHub cannot report stays noted");

        let mut github = facts(HEAD, PrState::Open);
        observer.set(6, Ok(github.clone()));
        reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].opened_prs.len(), 1, "a PR nobody can merge yet stays noted");

        github.allowed_methods = vec![MergeMethod::Squash];
        observer.set(6, Ok(github));
        reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
        let state = domain.state().unwrap();
        let task = &state.tasks[&"t".into()];
        assert!(task.opened_prs.is_empty());
        assert_eq!(state.task_prs(task).map(|pr| (pr.repository.as_str(), pr.number)).collect::<Vec<_>>(), [("owner/repo", 7), ("owner/site", 6)]);
    }

    #[tokio::test]
    async fn a_noted_pr_and_a_second_pr_survive_a_restart() {
        let (fixture, domain) = open("opened-restart");
        authorized(&domain);
        opened(&domain);
        drop(domain);
        let domain = crate::unwired(&fixture.0).unwrap();
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].opened_prs.len(), 1, "the note outlives the daemon");

        let observer = FakeObserver::new();
        let mut github = facts(HEAD, PrState::Open);
        github.allowed_methods = vec![MergeMethod::Squash];
        observer.set(6, Ok(github.clone()));
        observer.set(7, Ok(github));
        read_opened(&domain, &observer, None).await.unwrap();
        drop(domain);
        let domain = crate::unwired(&fixture.0).unwrap();
        let state = domain.state().unwrap();
        let task = &state.tasks[&"t".into()];
        assert_eq!(task.later_prs, vec![hosted_pr_id("github.com", "owner/site", 6)]);
        assert!(task.opened_prs.is_empty());
    }

    #[tokio::test]
    async fn a_final_delivery_finds_the_open_pr_of_a_branch_no_command_showed() {
        let (fixture, domain) = open("branch-pr");
        authorized(&domain);
        let worktree = alondra_engine_doubles::worktrees::testing::task_worktree(&fixture.0.join("checkout"), "worktree");
        command(&domain, Actor::System, "worktree", Command::RecordTaskWorktree {
            task_id: "t".into(), path: worktree.display().to_string(), origin: alondra_domain_rules::WorktreeOrigin::Entered, branch: None,
        });
        let program = fixture.0.join("gh");
        std::fs::write(&program, format!("#!/bin/sh\ncase \"$*\" in\n\
            *\"--json url,state\"*) printf '%s' '{{\"url\":\"https://github.com/owner/site/pull/6\",\"state\":\"OPEN\"}}' ;;\n\
            *\"repo view\"*) printf '%s' '{{\"mergeCommitAllowed\":false,\"squashMergeAllowed\":true,\"rebaseMergeAllowed\":false}}' ;;\n\
            *) printf '%s' '{{\"state\":\"OPEN\",\"headRefOid\":\"{HEAD}\",\"baseRefName\":\"main\"}}' ;;\nesac\n")).unwrap();
        std::fs::set_permissions(&program, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let gh = GhCli::at(program).with_branch_pr_limit(Duration::from_secs(600));

        catch_up_task(&domain, &github(gh.clone()), &"t".into(), false).await.unwrap();
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].later_prs.len(), 0, "only a final delivery looks at the branches");
        catch_up_task(&domain, &github(gh.clone()), &"t".into(), true).await.unwrap();
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].later_prs, vec![hosted_pr_id("github.com", "owner/site", 6)]);
    }

    #[tokio::test]
    async fn a_final_delivery_rereads_even_its_only_registered_pr() {
        let (fixture, domain) = open("closed-before-result");
        authorized(&domain);
        let program = fixture.0.join("gh");
        std::fs::write(&program, format!("#!/bin/sh\nprintf '%s' '{{\"state\":\"CLOSED\",\"headRefOid\":\"{HEAD}\",\"baseRefName\":\"main\"}}'\n")).unwrap();
        std::fs::set_permissions(&program, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        catch_up_task(&domain, &github(GhCli::at(program)), &"t".into(), true).await.unwrap();
        assert_eq!(domain.state().unwrap().pull_requests[&"pr".into()].state, PrState::Closed);
        assert!(alondra_domain_rules::ReadIndex::build(&domain.state().unwrap()).cards_of(&"d".into()).is_empty());
    }

    #[tokio::test]
    async fn a_final_delivery_rereads_even_its_only_registered_pr_after_it_reopens() {
        let (_fixture, domain) = open("reopened-before-result");
        registered(&domain);
        command(&domain, Actor::System, "closed", Command::ObservePrState {
            pr_id: "pr".into(), head: HEAD.into(), base: "main".into(), state: PrState::Closed, merged_head: None,
        });
        let source = FakeObserver::new();
        source.set(7, Ok(facts(HEAD, PrState::Open)));
        catch_up_task_using(&domain, &github(GhCli::new()), &source, &"t".into(), true).await.unwrap();
        assert_eq!(domain.state().unwrap().pull_requests[&"pr".into()].state, PrState::Open);
        command(&domain, Actor::Agent { agent_id: "a".into() }, "reopened-result", Command::PresentResult {
            checklist: vec![], attachments: vec![], task_id: "t".into(), interaction_id: "reopened-result".into(), body: "Ready".into(),
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEAD.into() }], is_final: true, nothing_to_review: false,
            card: alondra_domain_rules::CardSpec::delivery("Review the result", &[], true),
        });
        assert_eq!(alondra_domain_rules::ReadIndex::build(&domain.state().unwrap()).cards_of(&"reopened-result".into()).len(), 1);
    }

    #[tokio::test]
    async fn a_final_delivery_cannot_use_cached_pr_facts_when_github_fails() {
        let (fixture, domain) = open("unread-before-result");
        authorized(&domain);
        let program = fixture.0.join("gh");
        std::fs::write(&program, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&program, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        assert!(catch_up_task(&domain, &github(GhCli::at(program)), &"t".into(), true).await.is_err());
        assert_eq!(domain.state().unwrap().pull_requests[&"pr".into()].state, PrState::Open);
    }

    #[tokio::test]
    async fn a_close_command_rechecks_its_registered_pr_even_without_merge_methods() {
        let (_fixture, domain) = open("close-command");
        authorized(&domain);
        let before = reading_prs(&domain.state().unwrap());
        command(&domain, Actor::System, "changed", Command::NotePrChanges { task_id: "t".into() });
        assert_eq!(reading_prs(&domain.state().unwrap()).len(), 1);
        assert!(!reading_prs(&domain.state().unwrap()).is_subset(&before), "the watcher wakes even though the set of PR ids has not changed");
        let observer = FakeObserver::new();
        observer.set(7, Ok(facts(HEAD, PrState::Closed)));
        read_opened(&domain, &observer, Some(&"t".into())).await.unwrap();
        let state = domain.state().unwrap();
        assert_eq!(state.pull_requests[&"pr".into()].state, PrState::Closed);
        assert!(state.tasks[&"t".into()].opened_prs.is_empty());
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


    pub fn open(_tag: &str) -> (Fixture, DomainRuntime) {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        let domain = crate::unwired(&home).unwrap();
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
        assert_eq!(clock::timeout(Duration::from_secs(1), calls.recv()).await.unwrap(), Some(7));
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
            Command::CreateTask { plan_items: None, continues: None,
                task_id: "t-2".into(),
                request_id: "r-2".into(),
                title: "Second task".into(),
                agent_id: Some("b".into()),
                review_prs: false,
                dependencies: BTreeSet::new(),
                stack: None,
                launch: Some(alondra_domain_rules::LaunchPlan::new("node", "/tmp")),
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
        clock::timeout(Duration::from_secs(2), async {
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
            domain.store.lock().unwrap().sqlite_mut().claim_as_node("auth", "a");
            domain.reload();
            let observer = FakeObserver::new();
            observer.set(7, Ok(remote.clone()));
            reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
            let state = domain.state().unwrap();
            assert_eq!(state.pull_requests[&"pr".into()].state, remote.state, "{tag}");
            assert_eq!(state.merge_authorizations[&"auth".into()].attempt, MergeAttemptState::Failed, "{tag}");
            let task = &state.tasks[&"t".into()];
            assert_eq!(task.wrap_up.asked_at.is_some(), remote.state == PrState::Closed,
                "{tag}: only a close settles this delivery; another head never inherits acceptance");
            if remote.state != PrState::Closed {
                assert!(!state.finished(task), "{tag}");
                assert!(task.final_presentation.is_none(), "{tag}");
                assert!(state.interactions[&InteractionId::from("d")].withdrawn, "{tag}");
                assert!(!state.merge_authorizations[&AuthorizationId::from("auth")].valid, "{tag}");
            }
            if remote.state == PrState::Merged {
                assert_eq!(state.pull_requests[&PrId::from("pr")].merged_head.as_deref(), Some(OTHER));
            }
        }
    }

    /// PR #727: a node claimed the merge of its first delivery, then resolved conflicts and
    /// delivered a newer head; the claim left behind held every new merge back.
    #[tokio::test]
    async fn a_node_claim_of_a_superseded_delivery_is_reconciled_and_the_user_can_merge_again() {
        let (_fixture, domain) = open("superseded-claim");
        authorized(&domain);
        domain.store.lock().unwrap().sqlite_mut().claim_as_node("auth", "a");
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
            card: alondra_domain_rules::CardSpec::delivery("Review the result", &[], true),
        });
        let read = domain.read();
        let item = read.index().item(read.state(), &read.state().interactions[&"d2".into()]);
        let merge = item.available_actions.iter().find(|action| action.kind == "authorize_merge").unwrap();
        assert!(merge.enabled, "{merge:?}");
        command(&domain, Actor::User { user_id: "u".into() }, "authorize-2", Command::AuthorizeMerge {
            authorization_id: "auth-2".into(), presentation_id: "d2".into(), pr_id: "pr".into(),
            expected_head: OTHER.into(), method: MergeMethod::Squash, despite_pending_message: false,
        });
    }

    #[tokio::test]
    async fn a_pr_that_cannot_be_read_five_times_raises_one_problem_and_recovery_withdraws_it() {
        let (_fixture, domain) = open("unreadable");
        presented(&domain, false);
        let observer = FakeObserver::new();
        observer.set(7, Err("error connecting to github".into()));
        let mut failures = ObservationFailures::default();
        let problems = |domain: &DomainRuntime| domain.state().unwrap().interactions.values().cloned()
            .filter(|card| card.card.kind == alondra_domain_rules::CardKind::Problem)
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

    /// The task `t` of PR #7, registered and never presented.
    fn registered(domain: &DomainRuntime) {
        command(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
        command(domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        command(domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, continues: None,
            task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()),
            review_prs: true, dependencies: BTreeSet::new(), stack: None,
            launch: Some(alondra_domain_rules::LaunchPlan::new("node", "/tmp")),
        });
        command(domain, Actor::Agent { agent_id: "a".into() }, "pr", Command::RegisterPr {
            task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
            number: 7, head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
        });
    }

    /// Task `t` with its PR open, closed by the orchestrator under a build that left the close
    /// waiting for GitHub to deliver the PR.
    fn held_by_an_older_build(fixture: &Fixture) -> DomainRuntime {
        let domain = crate::unwired(&fixture.0).unwrap();
        registered(&domain);
        drop(domain);
        let held = serde_json::to_string(&OpenPrPending::Deliver { result: None }).unwrap();
        rusqlite::Connection::open(crate::domain_store::active_path(&fixture.0)).unwrap()
            .execute("INSERT INTO task_open_prs(task_id,pending) VALUES('t',?1)", [held]).unwrap();
        let domain = crate::unwired(&fixture.0).unwrap();
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].open_pr, Some(OpenPrPending::Deliver { result: None }));
        domain
    }

    #[tokio::test]
    async fn the_orchestrators_close_of_a_task_with_its_pr_open_delivers_it_without_github() {
        let (_fixture, domain) = open("open-pr-deliver-now");
        registered(&domain);
        command(&domain, Actor::Orchestrator { generation: 0 }, "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
        let state = domain.state().unwrap();
        let t = &state.tasks[&"t".into()];
        assert_eq!((t.state, t.open_pr.clone()), (TaskState::ForYourReview, None), "delivered in the close itself");
    }

    #[tokio::test]
    async fn a_close_an_older_build_held_for_github_is_delivered_as_last_known_when_github_does_not_answer() {
        let (fixture, domain) = open("open-pr-held-unread");
        drop(domain);
        let domain = held_by_an_older_build(&fixture);
        let observer = FakeObserver::new();
        observer.set(7, Err("error connecting to github".into()));
        let _ = reconcile(&domain, &observer, &mut ObservationFailures::default()).await;
        let state = domain.state().unwrap();
        let t = &state.tasks[&"t".into()];
        assert_eq!((t.state, t.open_pr.clone()), (TaskState::ForYourReview, None), "an unreadable GitHub holds nothing");
        let card = alondra_domain_rules::open_pr_delivery::OpenPrDelivery::from_body(&state.interactions[t.final_presentation.as_ref().unwrap()].body).unwrap();
        assert_eq!(card.readiness, alondra_domain_rules::open_pr_delivery::PrReadiness::Unconfirmed);
    }

    #[tokio::test]
    async fn the_open_pr_of_a_task_the_orchestrator_cancelled_reaches_the_user_without_waiting_for_its_checks() {
        let (fixture, domain) = open("open-pr-deliver");
        drop(domain);
        let domain = held_by_an_older_build(&fixture);
        let observer = FakeObserver::new();
        let mut github = facts(HEAD, PrState::Open);
        github.checks = PrChecks::Pending { names: vec!["lint".into()] };
        github.mergeable = PrMergeable::Clean;
        observer.set(7, Ok(github));
        reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
        let state = domain.state().unwrap();
        let t = &state.tasks[&"t".into()];
        assert_eq!((t.state, t.open_pr.clone()), (TaskState::ForYourReview, None), "the first read delivers it");
        let card = alondra_domain_rules::open_pr_delivery::OpenPrDelivery::from_body(&state.interactions[t.final_presentation.as_ref().unwrap()].body).unwrap();
        assert_eq!(card.readiness, alondra_domain_rules::open_pr_delivery::PrReadiness::ChecksRunning { names: vec!["lint".into()] });
        assert!(card.text(alondra_view_model::Lang::En).1.contains("lint"));
        assert!(observer.1.lock().unwrap().is_empty(), "Alondra closes no PR it delivers");
    }

    #[tokio::test]
    async fn the_open_pr_of_a_task_with_failing_checks_reaches_the_user_saying_which() {
        let (fixture, domain) = open("open-pr-failed");
        drop(domain);
        let domain = held_by_an_older_build(&fixture);
        let observer = FakeObserver::new();
        let mut github = facts(HEAD, PrState::Open);
        github.checks = PrChecks::Failed { names: vec!["test".into()] };
        github.mergeable = PrMergeable::Clean;
        observer.set(7, Ok(github));
        reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
        let state = domain.state().unwrap();
        let t = &state.tasks[&"t".into()];
        let card = alondra_domain_rules::open_pr_delivery::OpenPrDelivery::from_body(&state.interactions[t.final_presentation.as_ref().unwrap()].body).unwrap();
        assert_eq!(card.readiness, alondra_domain_rules::open_pr_delivery::PrReadiness::ChecksFailed { names: vec!["test".into()] });
    }

    #[tokio::test]
    async fn the_users_cancel_closes_the_pr_on_github_with_the_reason() {
        let (_fixture, domain) = open("open-pr-close");
        registered(&domain);
        command(&domain, Actor::User { user_id: "u".into() }, "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
        let observer = FakeObserver::new();
        observer.set(7, Ok(facts(HEAD, PrState::Open)));
        reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
        assert_eq!(*observer.1.lock().unwrap(), vec![(7, "Cancelled in Alondra: Task".to_owned())]);
        let state = domain.state().unwrap();
        assert_eq!((state.pull_requests[&"pr".into()].state, state.tasks[&"t".into()].open_pr.clone()), (PrState::Closed, None));
    }

    #[tokio::test]
    async fn a_pr_already_closed_on_github_is_not_closed_again() {
        let (_fixture, domain) = open("open-pr-closed-already");
        registered(&domain);
        command(&domain, Actor::User { user_id: "u".into() }, "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
        let observer = FakeObserver::new();
        observer.set(7, Ok(facts(HEAD, PrState::Closed)));
        reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
        assert_eq!(observer.3.load(std::sync::atomic::Ordering::Relaxed), 0, "nothing to close");
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].open_pr, None);
    }

    #[tokio::test]
    async fn a_close_github_keeps_refusing_is_tried_less_and_less_often_and_the_fifth_raises_one_problem() {
        let (_fixture, domain) = open("open-pr-close-refused");
        registered(&domain);
        command(&domain, Actor::User { user_id: "u".into() }, "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
        let observer = FakeObserver::new();
        observer.set(7, Ok(facts(HEAD, PrState::Open)));
        *observer.2.lock().unwrap() = Some("HTTP 502".into());
        let mut failures = ObservationFailures::default();
        let problems = |domain: &DomainRuntime| domain.state().unwrap().interactions.values()
            .filter(|card| card.id.0.starts_with("problem-pr-close-") && !card.withdrawn).count();
        let mut tried = vec![];
        for round in 1..=25 {
            let before = observer.3.load(std::sync::atomic::Ordering::Relaxed);
            reconcile(&domain, &observer, &mut failures).await.unwrap();
            if observer.3.load(std::sync::atomic::Ordering::Relaxed) > before {
                tried.push(round);
            }
            if round == 19 {
                assert_eq!(problems(&domain), 0, "four refusals are a bad moment on GitHub");
            }
        }
        assert_eq!(tried, [1, 3, 6, 11, 20], "each refusal doubles the wait");
        assert_eq!(problems(&domain), 1, "one Problem however long it keeps refusing");
        *observer.2.lock().unwrap() = None;
        for _ in 0..17 {
            reconcile(&domain, &observer, &mut failures).await.unwrap();
        }
        assert_eq!(*observer.1.lock().unwrap(), vec![(7, "Cancelled in Alondra: Task".to_owned())]);
        assert_eq!(domain.state().unwrap().pull_requests[&"pr".into()].state, PrState::Closed);
        assert_eq!(problems(&domain), 0, "the close went through");
    }

    /// The task `t` the user cancelled, whose PR GitHub closed and later reopened: no close is pending.
    fn cancelled_with_its_pr_reopened(domain: &DomainRuntime) {
        registered(domain);
        command(domain, Actor::User { user_id: "u".into() }, "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
        command(domain, Actor::System, "node-end", Command::RecordNodeEnd { task_id: "t".into(), end: alondra_domain_rules::NodeEnd::Closed, agent_id: None });
        command(domain, Actor::System, "closed", Command::ObservePrState { pr_id: "pr".into(), head: HEAD.into(), base: "main".into(), state: PrState::Closed, merged_head: None });
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].open_pr, None);
    }

    fn close_cancelled(domain: &DomainRuntime) {
        command(domain, Actor::System, "close-cancelled", Command::ClosePrOfCancelledTask {
            task_id: "t".into(), pr_id: "pr".into(), branch: "feature".into(), status: facts(HEAD, PrState::Open).status() });
    }

    #[tokio::test]
    async fn the_pending_close_of_a_cancelled_tasks_pr_survives_a_restart_and_closes_it_on_github() {
        let (fixture, domain) = open("cancelled-close-restart");
        cancelled_with_its_pr_reopened(&domain);
        close_cancelled(&domain);
        drop(domain);
        let domain = crate::unwired(&fixture.0).unwrap();
        let state = domain.state().unwrap();
        assert_eq!(state.tasks[&"t".into()].open_pr, Some(OpenPrPending::Close { comment: "Cancelled in Alondra: Task".into() }), "it outlives a restart");
        assert_eq!(state.pull_requests[&"pr".into()].state, PrState::Open);
        let observer = FakeObserver::new();
        observer.set(7, Ok(facts(HEAD, PrState::Open)));
        reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
        assert_eq!(*observer.1.lock().unwrap(), vec![(7, "Cancelled in Alondra: Task".to_owned())]);
        let state = domain.state().unwrap();
        assert_eq!((state.pull_requests[&"pr".into()].state, state.tasks[&"t".into()].open_pr.clone()), (PrState::Closed, None));
        assert_eq!(state.tasks[&"t".into()].state, TaskState::Cancelled);
        assert!(state.tasks[&"t".into()].final_presentation.is_none());
        reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
        assert_eq!(observer.3.load(std::sync::atomic::Ordering::Relaxed), 1, "a confirmed close is not repeated");
    }

    #[tokio::test]
    async fn a_close_github_refuses_for_a_cancelled_task_is_retried_across_a_restart_until_it_goes_through() {
        let (fixture, domain) = open("cancelled-close-retry");
        cancelled_with_its_pr_reopened(&domain);
        close_cancelled(&domain);
        let observer = FakeObserver::new();
        observer.set(7, Ok(facts(HEAD, PrState::Open)));
        *observer.2.lock().unwrap() = Some("HTTP 502".into());
        let mut failures = ObservationFailures::default();
        reconcile(&domain, &observer, &mut failures).await.unwrap();
        reconcile(&domain, &observer, &mut failures).await.unwrap();
        reconcile(&domain, &observer, &mut failures).await.unwrap();
        assert_eq!(observer.3.load(std::sync::atomic::Ordering::Relaxed), 2, "the second refusal waits a round before trying again");
        assert!(observer.1.lock().unwrap().is_empty());
        drop(domain);
        let domain = crate::unwired(&fixture.0).unwrap();
        assert!(matches!(domain.state().unwrap().tasks[&"t".into()].open_pr, Some(OpenPrPending::Close { .. })), "the refusals did not lose the close");
        *observer.2.lock().unwrap() = None;
        reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
        assert_eq!(*observer.1.lock().unwrap(), vec![(7, "Cancelled in Alondra: Task".to_owned())]);
        let state = domain.state().unwrap();
        assert_eq!((state.pull_requests[&"pr".into()].state, state.tasks[&"t".into()].open_pr.clone()), (PrState::Closed, None));
        assert_eq!(state.tasks[&"t".into()].state, TaskState::Cancelled);
        assert!(state.tasks[&"t".into()].final_presentation.is_none());
    }

}
