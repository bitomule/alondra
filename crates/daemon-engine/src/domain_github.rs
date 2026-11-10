//! GitHub facts for domain v2 pull requests, read with `gh`. An observation records remote
//! reality; a merged PR settles the final delivery that presented it. Merging is
//! `domain_merges`'s, with the same `gh`.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use alondra_core::domain::{
    Actor, Command, CommandEnvelope, DomainState, MergeAttemptState, MergeMethod, OpenPrPending, OpenPrStatus, OpenedPr,
    clip, hosted_pr_id, CardSpec, PrChecks, PrId, PrMergeable, PrState, PullRequest, Scope, TaskId, PROTOCOL_VERSION,
};
use alondra_core::pr::CheckState;
use anyhow::Result;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// How long a delivery waits for GitHub to name one branch's open PR.
const BRANCH_PR_LIMIT: Duration = Duration::from_secs(3);

/// What GitHub reports for one PR. `merged_head` is the PR head at merge time,
/// never the merge commit: squash and rebase produce a different commit that
/// can never match the presented and authorized head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrFacts {
    pub head: String,
    pub base: String,
    pub state: PrState,
    pub merged_head: Option<String>,
    pub allowed_methods: Vec<MergeMethod>,
    pub mergeable: PrMergeable,
    pub checks: PrChecks,
}

impl PrFacts {
    pub(crate) fn status(&self) -> OpenPrStatus {
        OpenPrStatus { state: self.state, head: self.head.clone(), base: self.base.clone(),
            mergeable: self.mergeable, checks: self.checks.clone() }
    }
}

type Observed<'a> = Pin<Box<dyn Future<Output = Result<PrFacts, String>> + Send + 'a>>;
type Closed<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

pub trait PrObserver: Send + Sync + 'static {
    fn observe<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Observed<'a>;
    /// Closes the PR on GitHub with `comment`.
    fn close<'a>(&'a self, _host: &'a str, _repository: &'a str, _number: u64, _comment: &'a str) -> Closed<'a> {
        Box::pin(async { Err("this observer cannot close pull requests".to_owned()) })
    }
}

impl<T: PrObserver> PrObserver for Arc<T> {
    fn observe<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Observed<'a> {
        (**self).observe(host, repository, number)
    }
    fn close<'a>(&'a self, host: &'a str, repository: &'a str, number: u64, comment: &'a str) -> Closed<'a> {
        (**self).close(host, repository, number, comment)
    }
}

const OBSERVED_FIELDS: &str = "state,headRefOid,baseRefName,mergeable,mergeStateStatus,statusCheckRollup";

#[derive(Clone)]
pub struct GhCli {
    program: PathBuf,
    branch_pr_limit: Duration,
}

impl GhCli {
    pub fn new() -> Self {
        Self { program: "gh".into(), branch_pr_limit: BRANCH_PR_LIMIT }
    }
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn at(program: PathBuf) -> Self {
        Self { program, branch_pr_limit: BRANCH_PR_LIMIT }
    }
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn with_branch_pr_limit(mut self, limit: Duration) -> Self {
        self.branch_pr_limit = limit;
        self
    }
    pub fn program(&self) -> &std::path::Path {
        &self.program
    }
}

impl PrObserver for GhCli {
    fn observe<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Observed<'a> {
        Box::pin(async move {
            let url = format!("https://{}/{repository}/pull/{number}", alondra_core::domain::normalize_pr_host(host));
            let run = tokio::process::Command::new(&self.program)
                .args(["pr", "view", &url, "--json", OBSERVED_FIELDS])
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
    fn close<'a>(&'a self, host: &'a str, repository: &'a str, number: u64, comment: &'a str) -> Closed<'a> {
        Box::pin(async move {
            let url = pr_url(host, repository, number);
            self.run(&["pr", "close", &url, "--comment", comment], None, Duration::from_secs(30)).await
                .map(drop).map_err(|failure| failure.to_string())
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

    pub async fn run(&self, args: &[&str], dir: Option<&std::path::Path>, limit: Duration) -> Result<Vec<u8>, GhFailure> {
        let output = self.output(args, dir, limit).await?;
        if output.status.success() {
            return Ok(output.stdout);
        }
        Err(GhFailure::Exit(output.status.code(), String::from_utf8_lossy(&output.stderr).trim().to_owned()))
    }

    /// One `gh api` call with `--include`: GitHub's HTTP status, headers and body, whatever the status.
    /// A failure is only `gh` failing before GitHub answered.
    pub(crate) async fn api(&self, args: &[&str], limit: Duration) -> Result<HttpAnswer, GhFailure> {
        let args: Vec<&str> = ["api", "--include"].into_iter().chain(args.iter().copied()).collect();
        let output = self.output(&args, None, limit).await?;
        match HttpAnswer::parse(&output.stdout) {
            Some(answer) => Ok(answer),
            None if output.status.success() => Err(GhFailure::Unreadable("gh api answered without an HTTP status".into())),
            None => Err(GhFailure::Exit(output.status.code(), String::from_utf8_lossy(&output.stderr).trim().to_owned())),
        }
    }

    async fn output(&self, args: &[&str], dir: Option<&std::path::Path>, limit: Duration) -> Result<std::process::Output, GhFailure> {
        let mut command = tokio::process::Command::new(&self.program);
        command.args(args).kill_on_drop(true);
        if let Some(dir) = dir {
            command.current_dir(dir);
        }
        match tokio::time::timeout(limit, command.output()).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) => Err(GhFailure::Launch(error.kind(), error.to_string())),
            Err(_) => Err(GhFailure::TimedOut(format!("gh {} did not finish within {} s", args.first().copied().unwrap_or_default(), limit.as_secs()))),
        }
    }

    /// The open PR GitHub has for the branch checked out in `dir`, as host, `owner/repo` and number.
    pub(crate) async fn branch_pr(&self, dir: &std::path::Path, limit: Duration) -> Option<(String, String, u64)> {
        let out = self.run(&["pr", "view", "--json", "url,state"], Some(dir), limit).await.ok()?;
        let value: Value = serde_json::from_slice(&out).ok()?;
        if value["state"].as_str() != Some("OPEN") {
            return None;
        }
        let (host, path) = value["url"].as_str()?.split_once("://")?.1.split_once('/')?;
        let (repository, number) = path.split_once("/pull/")?;
        Some((alondra_core::domain::normalize_pr_host(host), repository.to_owned(), number.trim_end_matches('/').parse().ok()?))
    }
}

/// Why a `gh` run failed, as the system and `gh` report it. The text in each case is only shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GhFailure {
    /// `gh` could not be started here.
    Launch(std::io::ErrorKind, String),
    TimedOut(String),
    /// `gh` exited with this code, `None` when a signal ended it. 4 is `gh` needing a sign-in (`gh help exit-codes`).
    Exit(Option<i32>, String),
    /// GitHub answered with an HTTP status the call does not expect.
    Http(u16, String),
    /// `gh` answered something Alondra cannot read.
    Unreadable(String),
}

impl GhFailure {
    /// `gh` cannot work on this Mac until someone installs it or signs it in.
    pub(crate) fn needs_setup(&self) -> bool {
        matches!(self, Self::Launch(..) | Self::Exit(Some(4), _))
    }

    pub(crate) fn signed_out(&self) -> bool {
        matches!(self, Self::Exit(Some(4), _))
    }

    pub(crate) fn timed_out(&self) -> bool {
        matches!(self, Self::TimedOut(_))
    }

    pub(crate) fn unreadable(error: impl std::fmt::Display) -> Self {
        Self::Unreadable(error.to_string())
    }
}

impl std::fmt::Display for GhFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Launch(_, text) | Self::TimedOut(text) | Self::Exit(_, text) | Self::Unreadable(text) => f.write_str(text),
            Self::Http(status, text) => write!(f, "{text} (HTTP {status})"),
        }
    }
}

#[cfg(any(test, feature = "test-hooks"))]
impl From<&str> for GhFailure {
    fn from(stderr: &str) -> Self {
        Self::Exit(Some(1), stderr.to_owned())
    }
}

impl From<GhFailure> for String {
    fn from(failure: GhFailure) -> Self {
        failure.to_string()
    }
}

/// What GitHub answered to a `gh api --include` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HttpAnswer {
    pub status: u16,
    /// Header names in lower case.
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpAnswer {
    /// `HTTP/<version> <status> <reason>`, header lines, an empty line, then the body.
    pub(crate) fn parse(stdout: &[u8]) -> Option<Self> {
        let end = stdout.windows(4).position(|window| window == b"\r\n\r\n")?;
        let head = std::str::from_utf8(&stdout[..end]).ok()?;
        let mut lines = head.lines();
        let status = lines.next()?.strip_prefix("HTTP/")?.split_whitespace().nth(1)?.parse().ok()?;
        let headers = lines.filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned())).collect();
        Some(Self { status, headers, body: stdout[end + 4..].to_vec() })
    }

    pub(crate) fn success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// GitHub refused for its rate limit: a 429, or a 403 with no requests left or a wait to respect.
    pub(crate) fn rate_limited(&self) -> bool {
        self.status == 429 || (self.status == 403
            && (self.headers.get("x-ratelimit-remaining").is_some_and(|left| left == "0") || self.headers.contains_key("retry-after")))
    }

    /// The answer as a failure, with GitHub's `message` for showing.
    pub(crate) fn failure(&self) -> GhFailure {
        let message = serde_json::from_slice::<Value>(&self.body).ok()
            .and_then(|value| value["message"].as_str().map(str::to_owned)).unwrap_or_default();
        GhFailure::Http(self.status, message)
    }
}

pub fn pr_url(host: &str, repository: &str, number: u64) -> String {
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

pub fn parse(value: &Value) -> Result<PrFacts, String> {
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
        mergeable: mergeable(value),
        checks: checks(value),
    })
}

fn mergeable(value: &Value) -> PrMergeable {
    match (value["mergeable"].as_str(), value["mergeStateStatus"].as_str()) {
        (Some("CONFLICTING"), _) | (_, Some("DIRTY")) => PrMergeable::Conflicting,
        (Some("UNKNOWN") | None, _) | (_, Some("UNKNOWN")) => PrMergeable::Computing,
        (Some("MERGEABLE"), None | Some("CLEAN" | "HAS_HOOKS")) => PrMergeable::Clean,
        _ => PrMergeable::Other,
    }
}

/// The latest run of each check: any failure first, then any that needs someone's approval to run,
/// then any still running.
fn checks(value: &Value) -> PrChecks {
    let entries = value["statusCheckRollup"].as_array().map(|rollup| alondra_daemon_support::prs::latest_entries(rollup)).unwrap_or_default();
    let (mut failed, mut approval, mut running, mut any) = (vec![], vec![], vec![], false);
    for entry in entries {
        let Some(check) = alondra_daemon_support::prs::check(entry) else { continue };
        any = true;
        match check.state {
            _ if entry["conclusion"].as_str() == Some("ACTION_REQUIRED") => approval.push(check.name),
            CheckState::Failed => failed.push(check.name),
            CheckState::Running => running.push(check.name),
            CheckState::Passed | CheckState::Other => (),
        }
    }
    if !failed.is_empty() {
        PrChecks::Failed { names: failed }
    } else if !approval.is_empty() {
        PrChecks::ActionRequired { names: approval }
    } else if !running.is_empty() {
        PrChecks::Pending { names: running }
    } else if any {
        PrChecks::Passed
    } else {
        PrChecks::None
    }
}

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

/// Reads from GitHub each PR a node was seen opening, of `task` or of every task, and makes it its
/// task's. One GitHub cannot report stays noted: its delivery waits, and the next round tries again.
pub(crate) async fn read_opened(domain: &DomainRuntime, observer: &impl PrObserver, task: Option<&TaskId>) -> Result<()> {
    let state = domain.state()?;
    let opened: Vec<(TaskId, OpenedPr)> = state.tasks.values().filter(|t| task.is_none_or(|task| *task == t.id))
        .flat_map(|t| t.opened_prs.iter().map(|pr| (t.id.clone(), pr.clone()))).collect();
    for (task_id, pr) in opened {
        let facts = match observer.observe(&pr.host, &pr.repository, pr.number).await {
            Ok(facts) if !facts.allowed_methods.is_empty() => facts,
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
/// and where each open PR is when there are several to tell apart by their heads.
pub async fn catch_up_task(domain: &DomainRuntime, gh: &GhCli, task_id: &TaskId, is_final: bool) -> Result<()> {
    let state = domain.state()?;
    let Some(task) = state.tasks.get(task_id) else { return Ok(()) };
    if is_final {
        for (path, _) in task.open_worktrees() {
            let Some((host, repository, number)) = gh.branch_pr(std::path::Path::new(path), gh.branch_pr_limit).await else { continue };
            match execute_system(domain, Command::NotePrOpened { task_id: task_id.clone(), host, repository, number }) {
                Ok(()) => (),
                Err(StoreError::Domain(error)) => log!("the open PR of {path} was refused: {error}"),
                Err(StoreError::Storage(error)) => return Err(error),
            }
        }
    }
    read_opened(domain, gh, Some(task_id)).await?;
    let state = domain.state()?;
    let Some(task) = state.tasks.get(task_id) else { return Ok(()) };
    let open: Vec<&PullRequest> = state.open_prs(task).collect();
    if open.len() > 1 {
        for pr in open {
            if let Ok(facts) = gh.observe(&pr.host, &pr.repository, pr.number).await {
                apply(domain, &state, pr, &facts)?;
            }
        }
    }
    Ok(())
}

/// Bring the domain's view of each watched PR up to date with GitHub. A PR
/// GitHub cannot report is left as it was.
pub(crate) async fn reconcile(
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
pub(crate) struct ObservationFailures(BTreeMap<PrId, u32>, BTreeMap<PrId, CloseFailure>);

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
        open = awaited(&*domain.state()?);
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
            open = awaited(&*domain.state()?);
            if open.is_empty() {
                break;
            }
            let current_pending = pending_prs(&*domain.state()?);
            if !open.is_subset(&known) || !current_pending.is_subset(&known_pending) {
                // A node opened a PR, a new one was registered, or a task closed while its PR is open.
                reconcile(&domain, &*observer, &mut failures).await?;
            }
            known = open.clone();
            known_pending = pending_prs(&*domain.state()?);
        }
    }
}

#[cfg(any(test, feature = "test-hooks"))]
/// GitHub from memory: the facts of each PR, the closes it took, the ones it was asked for, and
/// the error it refuses closes with, if any. A close it takes leaves the PR closed.
pub(crate) struct FakeObserver(
    pub std::sync::Mutex<std::collections::BTreeMap<u64, Result<PrFacts, String>>>,
    pub std::sync::Mutex<Vec<(u64, String)>>,
    pub std::sync::Mutex<Option<String>>,
    pub std::sync::atomic::AtomicUsize,
);
#[cfg(any(test, feature = "test-hooks"))]
impl FakeObserver {
    pub(crate) fn new() -> Self {
        Self(Default::default(), Default::default(), Default::default(), Default::default())
    }
    pub(crate) fn set(&self, number: u64, facts: Result<PrFacts, String>) {
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
    use super::*;
    use alondra_core::domain::*;
    use std::collections::BTreeSet;
    use std::os::unix::fs::PermissionsExt;

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

    pub fn presented(domain: &DomainRuntime, auto_merge: bool) {
        command(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
        command(domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge, maintenance: false, message: false });
        command(domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, continues: None,
            task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()),
            review_prs: true, dependencies: BTreeSet::new(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
        let register = Command::RegisterPr {
            task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
            number: 7, head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
        };
        command(domain, Actor::Agent { agent_id: "a".into() }, "pr", register.clone());
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
        let domain = DomainRuntime::open(&fixture.0).unwrap();
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].opened_prs.len(), 1, "the note outlives the daemon");

        let observer = FakeObserver::new();
        let mut github = facts(HEAD, PrState::Open);
        github.allowed_methods = vec![MergeMethod::Squash];
        observer.set(6, Ok(github.clone()));
        observer.set(7, Ok(github));
        read_opened(&domain, &observer, None).await.unwrap();
        drop(domain);
        let domain = DomainRuntime::open(&fixture.0).unwrap();
        let state = domain.state().unwrap();
        let task = &state.tasks[&"t".into()];
        assert_eq!(task.later_prs, vec![hosted_pr_id("github.com", "owner/site", 6)]);
        assert!(task.opened_prs.is_empty());
    }

    #[tokio::test]
    async fn a_final_delivery_finds_the_open_pr_of_a_branch_no_command_showed() {
        let (fixture, domain) = open("branch-pr");
        authorized(&domain);
        let worktree = crate::worktree::testing::task_worktree(&fixture.0.join("checkout"), "worktree");
        command(&domain, Actor::System, "worktree", Command::RecordTaskWorktree {
            task_id: "t".into(), path: worktree.display().to_string(), origin: alondra_core::domain::WorktreeOrigin::Entered, branch: None,
        });
        let program = fixture.0.join("gh");
        std::fs::write(&program, format!("#!/bin/sh\ncase \"$*\" in\n\
            *\"--json url,state\"*) printf '%s' '{{\"url\":\"https://github.com/owner/site/pull/6\",\"state\":\"OPEN\"}}' ;;\n\
            *\"repo view\"*) printf '%s' '{{\"mergeCommitAllowed\":false,\"squashMergeAllowed\":true,\"rebaseMergeAllowed\":false}}' ;;\n\
            *) printf '%s' '{{\"state\":\"OPEN\",\"headRefOid\":\"{HEAD}\",\"baseRefName\":\"main\"}}' ;;\nesac\n")).unwrap();
        std::fs::set_permissions(&program, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let gh = GhCli::at(program).with_branch_pr_limit(Duration::from_secs(600));

        catch_up_task(&domain, &gh, &"t".into(), false).await.unwrap();
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].later_prs.len(), 0, "only a final delivery looks at the branches");
        catch_up_task(&domain, &gh, &"t".into(), true).await.unwrap();
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].later_prs, vec![hosted_pr_id("github.com", "owner/site", 6)]);
    }

    #[tokio::test]
    async fn a_branch_gh_does_not_name_within_the_limit_is_left_out() {
        let (fixture, _) = open("branch-pr-limit");
        let program = fixture.0.join("gh");
        std::fs::write(&program, "#!/bin/sh\nprintf '%s' '{\"url\":\"https://github.com/owner/site/pull/6\",\"state\":\"OPEN\"}'\n").unwrap();
        std::fs::set_permissions(&program, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let gh = GhCli::at(program);
        assert_eq!(gh.branch_pr(&fixture.0, Duration::from_secs(600)).await, Some(("github.com".into(), "owner/site".into(), 6)));
        assert_eq!(gh.branch_pr(&fixture.0, Duration::ZERO).await, None);
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


    pub fn open(_tag: &str) -> (Fixture, DomainRuntime) {
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
            Command::CreateTask { plan_items: None, continues: None,
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
        let read = domain.read();
        let item = read.index().item(read.state(), &read.state().interactions[&"d2".into()]);
        let merge = item.available_actions.iter().find(|action| action.kind == "authorize_merge").unwrap();
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
        assert_eq!(merged, PrFacts { head: HEAD.into(), base: "main".into(), state: PrState::Merged, merged_head: Some(HEAD.into()), allowed_methods: vec![],
            mergeable: PrMergeable::Computing, checks: PrChecks::None });
        assert_eq!(gh.observe("github.com", "owner/repo", 8).await.unwrap_err(), "gh reported no PR head");
        assert!(gh.observe("github.com", "owner/repo", 9).await.unwrap_err().contains("no pull requests found"));
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.contains(&format!("pr view https://github.com/owner/repo/pull/7 --json {OBSERVED_FIELDS}")), "{calls}");
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

    /// The task `t` of PR #7, registered and never presented.
    fn registered(domain: &DomainRuntime) {
        command(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
        command(domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        command(domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, continues: None,
            task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()),
            review_prs: true, dependencies: BTreeSet::new(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
        command(domain, Actor::Agent { agent_id: "a".into() }, "pr", Command::RegisterPr {
            task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
            number: 7, head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
        });
    }

    /// Task `t` with its PR open, closed by the orchestrator under a build that left the close
    /// waiting for GitHub to deliver the PR.
    fn held_by_an_older_build(fixture: &Fixture) -> DomainRuntime {
        let domain = DomainRuntime::open(&fixture.0).unwrap();
        registered(&domain);
        drop(domain);
        let held = serde_json::to_string(&OpenPrPending::Deliver { result: None }).unwrap();
        rusqlite::Connection::open(crate::domain_database_migration::active_path(&fixture.0)).unwrap()
            .execute("INSERT INTO task_open_prs(task_id,pending) VALUES('t',?1)", [held]).unwrap();
        let domain = DomainRuntime::open(&fixture.0).unwrap();
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].open_pr, Some(OpenPrPending::Deliver { result: None }));
        domain
    }

    #[tokio::test]
    async fn the_orchestrators_close_of_a_task_with_its_pr_open_delivers_it_without_github() {
        let (_fixture, domain) = open("open-pr-deliver-now");
        registered(&domain);
        command(&domain, Actor::Orchestrator { generation: 0 }, "cancel", Command::CancelTask { task_id: "t".into() });
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
        let card = alondra_core::open_pr_delivery::OpenPrDelivery::from_body(&state.interactions[t.final_presentation.as_ref().unwrap()].body).unwrap();
        assert_eq!(card.readiness, alondra_core::open_pr_delivery::PrReadiness::Unconfirmed);
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
        let card = alondra_core::open_pr_delivery::OpenPrDelivery::from_body(&state.interactions[t.final_presentation.as_ref().unwrap()].body).unwrap();
        assert_eq!(card.readiness, alondra_core::open_pr_delivery::PrReadiness::ChecksRunning { names: vec!["lint".into()] });
        assert!(card.text(alondra_core::views::Lang::En).1.contains("lint"));
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
        let card = alondra_core::open_pr_delivery::OpenPrDelivery::from_body(&state.interactions[t.final_presentation.as_ref().unwrap()].body).unwrap();
        assert_eq!(card.readiness, alondra_core::open_pr_delivery::PrReadiness::ChecksFailed { names: vec!["test".into()] });
    }

    #[tokio::test]
    async fn the_users_cancel_closes_the_pr_on_github_with_the_reason() {
        let (_fixture, domain) = open("open-pr-close");
        registered(&domain);
        command(&domain, Actor::User { user_id: "u".into() }, "cancel", Command::CancelTask { task_id: "t".into() });
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
        command(&domain, Actor::User { user_id: "u".into() }, "cancel", Command::CancelTask { task_id: "t".into() });
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
        command(&domain, Actor::User { user_id: "u".into() }, "cancel", Command::CancelTask { task_id: "t".into() });
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
        command(domain, Actor::User { user_id: "u".into() }, "cancel", Command::CancelTask { task_id: "t".into() });
        command(domain, Actor::System, "node-end", Command::RecordNodeEnd { task_id: "t".into(), end: alondra_core::domain::NodeEnd::Closed, agent_id: None });
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
        let domain = DomainRuntime::open(&fixture.0).unwrap();
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
        let domain = DomainRuntime::open(&fixture.0).unwrap();
        assert!(matches!(domain.state().unwrap().tasks[&"t".into()].open_pr, Some(OpenPrPending::Close { .. })), "the refusals did not lose the close");
        *observer.2.lock().unwrap() = None;
        reconcile(&domain, &observer, &mut ObservationFailures::default()).await.unwrap();
        assert_eq!(*observer.1.lock().unwrap(), vec![(7, "Cancelled in Alondra: Task".to_owned())]);
        let state = domain.state().unwrap();
        assert_eq!((state.pull_requests[&"pr".into()].state, state.tasks[&"t".into()].open_pr.clone()), (PrState::Closed, None));
        assert_eq!(state.tasks[&"t".into()].state, TaskState::Cancelled);
        assert!(state.tasks[&"t".into()].final_presentation.is_none());
    }

    #[tokio::test]
    async fn gh_closes_a_pr_with_its_comment() {
        let (fixture, _) = open("gh-close");
        let program = fixture.0.join("gh");
        let log = fixture.0.join("calls");
        std::fs::write(&program, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\ncase \"$*\" in *pull/7*) exit 0;; *) echo 'no pull requests found' >&2; exit 1;; esac\n", log.display())).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let gh = GhCli::at(program);
        gh.close("github.com", "owner/repo", 7, "Cancelled in Alondra: Task").await.unwrap();
        assert!(gh.close("github.com", "owner/repo", 9, "Cancelled in Alondra: Task").await.unwrap_err().contains("no pull requests found"));
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.contains("pr close https://github.com/owner/repo/pull/7 --comment Cancelled in Alondra: Task"), "{calls}");
    }

    #[test]
    fn readiness_is_read_from_githubs_typed_fields() {
        let read = |extra: Value| {
            let mut value = serde_json::json!({"state": "OPEN", "headRefOid": HEAD, "baseRefName": "main"});
            value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            let facts = parse(&value).unwrap();
            (facts.mergeable, facts.checks)
        };
        let run = |name: &str, status: &str, conclusion: &str| serde_json::json!({"__typename": "CheckRun", "name": name, "status": status, "conclusion": conclusion});
        assert_eq!(read(serde_json::json!({"mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN", "statusCheckRollup": []})),
            (PrMergeable::Clean, PrChecks::None));
        assert_eq!(read(serde_json::json!({"mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY",
            "statusCheckRollup": [run("test", "COMPLETED", "SUCCESS"), run("lint", "IN_PROGRESS", "")]})), (PrMergeable::Conflicting, PrChecks::Pending { names: vec!["lint".into()] }));
        assert_eq!(read(serde_json::json!({"mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
            "statusCheckRollup": [run("lint", "IN_PROGRESS", ""), serde_json::json!({"context": "ci/legacy", "state": "PENDING"}), run("test", "COMPLETED", "SUCCESS")]})),
            (PrMergeable::Clean, PrChecks::Pending { names: vec!["lint".into(), "ci/legacy".into()] }));
        assert_eq!(serde_json::from_str::<PrChecks>(r#"{"kind":"pending"}"#).unwrap(), PrChecks::Pending { names: vec![] });
        assert_eq!(read(serde_json::json!({"mergeable": "UNKNOWN", "statusCheckRollup": [run("test", "COMPLETED", "FAILURE"), run("lint", "IN_PROGRESS", "")]})),
            (PrMergeable::Computing, PrChecks::Failed { names: vec!["test".into()] }));
        assert_eq!(read(serde_json::json!({"mergeable": "MERGEABLE", "mergeStateStatus": "BLOCKED",
            "statusCheckRollup": [run("deploy", "COMPLETED", "ACTION_REQUIRED"), run("test", "COMPLETED", "SUCCESS")]})),
            (PrMergeable::Other, PrChecks::ActionRequired { names: vec!["deploy".into()] }));
    }

}
