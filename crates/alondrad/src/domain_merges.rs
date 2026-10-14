//! Alondra merges the PR of a final delivery, with the same `gh` the watcher reads it with: once
//! the user accepts the delivery (or presses Merge), or, for an auto-merge Request, once its
//! checks pass. Every merge first takes a durable system claim, which checks the head, the base
//! and the stack (dependencies only hold back a start), and the delivery is read again right
//! before `gh pr merge`. A
//! merge counts only when GitHub reports the PR merged. A merge GitHub refuses goes back to the
//! node with GitHub's reason, as feedback on that delivery.
use crate::domain_github::{GhCli, execute_system, pr_url};
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use alondra_core::domain::{
    AuthorizationId, CardSpec, Command, DeliveryTarget, DomainError, DomainState, Instruction, InteractionId, InteractionPayload,
    MergeAttemptState, MergeMethod, PrId, PrState, PullRequest, Scope, clip,
};
use anyhow::Result;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Checks {
    /// GitHub reports no check at all, which may only mean none has started yet.
    None,
    Pending,
    Passed,
    Failed(Vec<String>),
    /// Checks that wait for someone to act on GitHub.
    ActionRequired(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub state: PrState,
    pub head: String,
    pub base: String,
    pub checks: Checks,
    pub merge_state: MergeState,
}

/// What GitHub's `mergeable` and `mergeStateStatus` say about merging the PR into its base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MergeState {
    /// CLEAN: nothing the base requires is missing, required checks included.
    Clean,
    /// `mergeable` CONFLICTING or `mergeStateStatus` DIRTY: it conflicts with its base.
    Conflicting,
    /// UNKNOWN: GitHub has not worked it out yet.
    Computing,
    /// BLOCKED, BEHIND, UNSTABLE or HAS_HOOKS: only GitHub's answer to the merge tells.
    Other,
}

impl MergeState {
    fn of(value: &Value) -> Self {
        match (value["mergeable"].as_str(), value["mergeStateStatus"].as_str()) {
            (Some("CONFLICTING"), _) | (_, Some("DIRTY")) => Self::Conflicting,
            (_, Some("CLEAN")) => Self::Clean,
            (_, Some("UNKNOWN") | None) => Self::Computing,
            _ => Self::Other,
        }
    }
}

type Pending<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

pub(crate) trait PrMerger: Send + Sync + 'static {
    fn status<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, Status>;
    /// Merges the PR only while GitHub still has it at `head`. Success may only mean GitHub put
    /// it in its merge queue.
    fn merge<'a>(&'a self, pr: &'a PullRequest, head: &'a str, method: MergeMethod) -> Pending<'a, ()>;
    fn in_queue<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool>;
    fn is_draft<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool>;
    fn ready<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, ()>;
    /// The checks the PR's base branch requires, from its rulesets and its classic branch protection.
    fn required_checks<'a>(&'a self, pr: &'a PullRequest, base: &'a str) -> Pending<'a, Vec<String>>;
}

impl<T: PrMerger> PrMerger for Arc<T> {
    fn status<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, Status> {
        (**self).status(pr)
    }
    fn merge<'a>(&'a self, pr: &'a PullRequest, head: &'a str, method: MergeMethod) -> Pending<'a, ()> {
        (**self).merge(pr, head, method)
    }
    fn in_queue<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> {
        (**self).in_queue(pr)
    }
    fn is_draft<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> {
        (**self).is_draft(pr)
    }
    fn ready<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, ()> {
        (**self).ready(pr)
    }
    fn required_checks<'a>(&'a self, pr: &'a PullRequest, base: &'a str) -> Pending<'a, Vec<String>> {
        (**self).required_checks(pr, base)
    }
}

impl PrMerger for GhCli {
    fn status<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, Status> {
        Box::pin(async move {
            let url = pr_url(&pr.host, &pr.repository, pr.number);
            let out = self.run(&["pr", "view", &url, "--json", "state,headRefOid,baseRefName,statusCheckRollup,mergeStateStatus,mergeable"], None,
                Duration::from_secs(30)).await?;
            let value: Value = serde_json::from_slice(&out).map_err(|e| e.to_string())?;
            let facts = crate::domain_github::parse(&value)?;
            Ok(Status { state: facts.state, head: facts.head, base: facts.base, checks: rollup(&value), merge_state: MergeState::of(&value) })
        })
    }
    fn merge<'a>(&'a self, pr: &'a PullRequest, head: &'a str, method: MergeMethod) -> Pending<'a, ()> {
        Box::pin(async move {
            let url = pr_url(&pr.host, &pr.repository, pr.number);
            let flag = match method {
                MergeMethod::Merge => "--merge",
                MergeMethod::Squash => "--squash",
                MergeMethod::Rebase => "--rebase",
            };
            self.run(&["pr", "merge", &url, flag, "--match-head-commit", head], None, Duration::from_secs(120)).await.map(|_| ())
        })
    }
    fn is_draft<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> {
        Box::pin(async move {
            let url = pr_url(&pr.host, &pr.repository, pr.number);
            let out = self.run(&["pr", "view", &url, "--json", "isDraft", "--jq", ".isDraft"], None, Duration::from_secs(30)).await?;
            match String::from_utf8_lossy(&out).trim() {
                "true" => Ok(true),
                "false" => Ok(false),
                other => Err(format!("GitHub did not say whether the PR is a draft: {other}")),
            }
        })
    }
    fn ready<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, ()> {
        Box::pin(async move {
            let url = pr_url(&pr.host, &pr.repository, pr.number);
            self.run(&["pr", "ready", &url], None, Duration::from_secs(60)).await.map(|_| ())
        })
    }
    fn required_checks<'a>(&'a self, pr: &'a PullRequest, base: &'a str) -> Pending<'a, Vec<String>> {
        Box::pin(async move {
            let host = alondra_core::domain::normalize_pr_host(&pr.host);
            let repo = format!("repos/{}", pr.repository);
            let mut required = match self.run(&["api", "--hostname", &host, &format!("{repo}/rules/branches/{base}?per_page=100")], None,
                Duration::from_secs(30)).await
            {
                Ok(rules) => ruleset_checks(&serde_json::from_slice(&rules).map_err(|e| e.to_string())?),
                Err(reason) if no_rules(&reason) => vec![],
                Err(reason) => return Err(reason),
            };
            match self.run(&["api", "--hostname", &host, &format!("{repo}/branches/{base}/protection/required_status_checks")], None,
                Duration::from_secs(30)).await
            {
                Ok(classic) => required.extend(classic_checks(&serde_json::from_slice(&classic).map_err(|e| e.to_string())?)),
                Err(reason) if no_rules(&reason) => {}
                Err(reason) => return Err(reason),
            }
            required.sort();
            required.dedup();
            Ok(required)
        })
    }
    fn in_queue<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> {
        Box::pin(async move {
            let (owner, name) = pr.repository.split_once('/').ok_or("the PR's repository is not owner/name")?;
            let host = alondra_core::domain::normalize_pr_host(&pr.host);
            let query = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){pullRequest(number:$number){isInMergeQueue}}}";
            let (owner, name, number) = (format!("owner={owner}"), format!("name={name}"), format!("number={}", pr.number));
            let query = format!("query={query}");
            let out = self.run(&["api", "graphql", "--hostname", &host, "-f", &query, "-F", &owner, "-F", &name, "-F", &number,
                "--jq", ".data.repository.pullRequest.isInMergeQueue"], None, Duration::from_secs(30)).await?;
            match String::from_utf8_lossy(&out).trim() {
                "true" => Ok(true),
                "false" => Ok(false),
                other => Err(format!("GitHub did not say whether the PR is in its merge queue: {other}")),
            }
        })
    }
}

/// The checks the active rulesets of a branch require (`GET /repos/{owner}/{repo}/rules/branches/{branch}`).
pub(crate) fn ruleset_checks(rules: &Value) -> Vec<String> {
    rules.as_array().into_iter().flatten().filter(|rule| rule["type"].as_str() == Some("required_status_checks"))
        .flat_map(|rule| rule["parameters"]["required_status_checks"].as_array().into_iter().flatten())
        .filter_map(|check| check["context"].as_str().map(str::to_owned)).collect()
}

/// GitHub has no rules to show: none set (404), or a private repository whose plan has no branch
/// protection (403). Either way no required check is known, and `mergeStateStatus` decides.
fn no_rules(reason: &str) -> bool {
    has(reason, &["http 404", "http 403"])
}

/// The checks a branch's classic protection requires (`.../protection/required_status_checks`).
pub(crate) fn classic_checks(protection: &Value) -> Vec<String> {
    let contexts = protection["contexts"].as_array().into_iter().flatten().filter_map(|c| c.as_str().map(str::to_owned));
    let checks = protection["checks"].as_array().into_iter().flatten().filter_map(|c| c["context"].as_str().map(str::to_owned));
    contexts.chain(checks).collect()
}

/// The checks GitHub reports for a PR's head. Only a check that failed sends the PR back;
/// cancelled, stale, skipped and neutral checks neither fail nor hold the merge.
pub(crate) fn rollup(value: &Value) -> Checks {
    let Some(list) = value["statusCheckRollup"].as_array().filter(|list| !list.is_empty()) else { return Checks::None };
    let (mut failed, mut action, mut pending) = (vec![], vec![], false);
    for check in list {
        let s = |key: &str| check.get(key).and_then(Value::as_str).unwrap_or_default();
        let name = [s("name"), s("context")].into_iter().find(|name| !name.is_empty()).unwrap_or("check").to_owned();
        match (s("status"), s("conclusion"), s("state")) {
            (_, "FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE", _) | (_, _, "FAILURE" | "ERROR") => failed.push(name),
            (_, "ACTION_REQUIRED", _) => action.push(name),
            (_, "SUCCESS" | "NEUTRAL" | "SKIPPED" | "CANCELLED" | "STALE", _) | (_, _, "SUCCESS") => {}
            _ => pending = true,
        }
    }
    if !failed.is_empty() {
        Checks::Failed(failed)
    } else if !action.is_empty() {
        Checks::ActionRequired(action)
    } else if pending {
        Checks::Pending
    } else {
        Checks::Passed
    }
}

/// One PR of a final delivery Alondra should merge now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Due {
    pub presentation: InteractionId,
    pub targets: Vec<DeliveryTarget>,
    pub pr: PullRequest,
    pub head: String,
    pub method: MergeMethod,
    /// The authorization to merge, absent until Alondra authorizes an auto-merge itself.
    pub authorization: Option<AuthorizationId>,
}

/// The open PRs of current final deliveries the user authorized (accepting or pressing Merge),
/// or that an auto-merge Request lets Alondra merge, and that nobody handed back since.
pub(crate) fn due(state: &DomainState) -> Vec<Due> {
    let mut due = vec![];
    for task in state.tasks.values().filter(|task| !task.state.terminal()) {
        let Some(p) = task.final_presentation.as_ref().and_then(|p| state.interactions.get(p)) else { continue };
        let InteractionPayload::Delivery { targets, acceptance, feedback, .. } = &p.payload else { continue };
        if state.handed_back(p).is_some() || !state.open_blockers(&task.id).is_empty() {
            continue;
        }
        let auto = acceptance.is_none() && feedback.is_empty()
            && state.requests.get(&task.request_id).is_some_and(|r| r.auto_merge);
        for target in targets {
            let DeliveryTarget::PullRequest { pr_id, head } = target else { continue };
            let Some(pr) = state.pull_requests.get(pr_id).filter(|pr| pr.state == PrState::Open && &pr.head == head) else { continue };
            let authorization = state.merge_authorizations.values().find(|a| {
                a.valid && a.presentation_id == p.id && &a.pr_id == pr_id && &a.head == head
                    && matches!(a.attempt, MergeAttemptState::Unclaimed | MergeAttemptState::Claimed | MergeAttemptState::Uncertain)
            });
            let (method, authorization) = match (authorization, pr.allowed_methods.first()) {
                (Some(a), _) => (a.method, Some(a.id.clone())),
                (None, Some(method)) if auto => (*method, None),
                _ => continue,
            };
            due.push(Due { presentation: p.id.clone(), targets: targets.clone(), pr: pr.clone(), head: head.clone(), method, authorization });
        }
    }
    due
}

/// The delivery is still the one to merge, and Alondra's claim on it still stands.
fn still_due(state: &DomainState, item: &Due, authorization: &AuthorizationId) -> bool {
    let Some(task) = state.tasks.get(&item.pr.task_id).filter(|task| !task.state.terminal()) else { return false };
    let Some(p) = task.final_presentation.as_ref().filter(|p| **p == item.presentation).and_then(|p| state.interactions.get(p)) else {
        return false;
    };
    state.handed_back(p).is_none()
        && state.pull_requests.get(&item.pr.id).is_some_and(|pr| pr.state == PrState::Open && pr.head == item.head)
        && state.merge_authorizations.get(authorization).is_some_and(|a| a.valid && a.claimed_by.is_none()
            && matches!(a.attempt, MergeAttemptState::Claimed | MergeAttemptState::Uncertain))
}

fn has(reason: &str, signs: &[&str]) -> bool {
    let reason = reason.to_ascii_lowercase();
    signs.iter().any(|sign| reason.contains(sign))
}

/// GitHub or the network did not answer, or GitHub failed on its side: "GraphQL: Something went
/// wrong while executing your query. This may be the result of a timeout, or it could be a GitHub bug."
fn transient(reason: &str) -> bool {
    has(reason, &["error connecting", "timed out", "timeout", "did not finish", "could not resolve", "connection", "http 5", "502", "503",
        "504", "rate limit", "something went wrong", "internal server error", "bad gateway", "service unavailable"])
}

/// A final delivery whose PR GitHub confirms is still a draft cannot be merged, so it is refused;
/// when GitHub does not answer, the delivery is accepted and the merger marks the PR ready itself.
pub(crate) async fn draft_refusal(state: &DomainState, targets: &[DeliveryTarget], github: &impl PrMerger) -> Option<DomainError> {
    for target in targets {
        let DeliveryTarget::PullRequest { pr_id, .. } = target else { continue };
        let Some(pr) = state.pull_requests.get(pr_id) else { continue };
        if github.is_draft(pr).await == Ok(true) {
            return Some(DomainError::new("pull_request_is_draft", format!(
                "PR #{} is a draft: mark it ready with gh pr ready before delivering", pr.number)));
        }
    }
    None
}

/// `gh` itself cannot work here: missing, logged out, or without access.
fn environment(reason: &str) -> bool {
    has(reason, &["gh auth login", "not logged", "authentication", "no such file", "command not found", "http 401", "http 403",
        "bad credentials", "admin rights", "permission", "not authorized", "resource not accessible"])
}

/// GitHub refuses only because required checks have not reported yet.
fn checks_waiting(reason: &str) -> bool {
    has(reason, &["required status check", "status checks are pending", "is expected", "waiting for status"])
}

fn already_merged(reason: &str) -> bool {
    has(reason, &["already merged", "was merged"])
}

enum Step {
    Wait,
    /// A merge GitHub failed transiently, tried again once its backoff is over rather than after the poll.
    Retry,
    Settled,
}

struct Trouble {
    count: u32,
    since: Instant,
    card: Option<InteractionId>,
}

const NON_TRANSIENT_FAILURES_BEFORE_CARD: u32 = 3;
/// Tries of `gh pr merge` that fail transiently before the delivery goes back to its node.
const TRANSIENT_MERGE_TRIES: u32 = 6;
const FIRST_MERGE_RETRY: Duration = Duration::from_secs(30);

/// The wait before retrying a merge that failed transiently `failures` times: 30 s doubling,
/// about 15 minutes across all tries.
fn merge_retry_wait(first: Duration, failures: u32) -> Duration {
    first * 2u32.pow(failures.saturating_sub(1))
}

struct Retry {
    failures: u32,
    at: Instant,
}

pub(crate) struct Merger {
    /// At most one GitHub read per PR this often while it waits.
    pub poll: Duration,
    /// How long GitHub may keep failing before the user is told.
    pub give_up: Duration,
    /// The wait before the first retry of a merge GitHub failed transiently; doubled after each.
    pub first_retry: Duration,
    waited: BTreeMap<PrId, Instant>,
    transient: BTreeMap<PrId, Retry>,
    trouble: BTreeMap<PrId, Trouble>,
    /// `gh pr merge` refused for lack of access while reads work: cleared only by a merge.
    merge_trouble: BTreeMap<PrId, Trouble>,
    queued: BTreeSet<PrId>,
    action: BTreeMap<PrId, InteractionId>,
    /// The authorization each PR was last looked at with: a new one is the user pressing again.
    pressed: BTreeMap<PrId, AuthorizationId>,
}

impl Default for Merger {
    fn default() -> Self {
        Self::with(Duration::from_secs(60), Duration::from_secs(15 * 60))
    }
}

impl Merger {
    pub(crate) fn with(poll: Duration, give_up: Duration) -> Self {
        Self { poll, give_up, first_retry: FIRST_MERGE_RETRY, waited: BTreeMap::new(), transient: BTreeMap::new(),
            trouble: BTreeMap::new(), merge_trouble: BTreeMap::new(), queued: BTreeSet::new(), action: BTreeMap::new(),
            pressed: BTreeMap::new() }
    }

    /// Merges what is due. `true` while something waits, so the caller looks again.
    pub(crate) async fn merge_due(&mut self, domain: &DomainRuntime, github: &impl PrMerger) -> Result<bool> {
        let mut waiting = false;
        let due = due(&*domain.state()?);
        let open: BTreeSet<_> = due.iter().map(|item| item.pr.id.clone()).collect();
        self.forget_all_but(domain, &open);
        for item in due {
            let now = Instant::now();
            // A new authorization is the user pressing Merge again: it is looked at now, not after
            // the wait the one before left behind.
            if let Some(authorization) = &item.authorization
                && self.pressed.insert(item.pr.id.clone(), authorization.clone()).is_some_and(|before| before != *authorization)
            {
                self.waited.remove(&item.pr.id);
            }
            if self.transient.get(&item.pr.id).is_some_and(|retry| now < retry.at)
                || self.waited.get(&item.pr.id).is_some_and(|at| now.duration_since(*at) < self.poll)
            {
                waiting = true;
                continue;
            }
            match self.step(domain, github, &item).await? {
                Step::Wait => {
                    waiting = true;
                    self.waited.insert(item.pr.id.clone(), now);
                }
                Step::Retry => {
                    waiting = true;
                    self.waited.remove(&item.pr.id);
                }
                Step::Settled => {
                    self.waited.remove(&item.pr.id);
                }
            }
        }
        Ok(waiting)
    }

    /// How long the loop sleeps while something waits: the poll, or less when a merge retry is due sooner.
    pub(crate) fn pause(&self) -> Duration {
        let now = Instant::now();
        self.transient.values().filter(|retry| retry.at > now).map(|retry| retry.at - now).fold(self.poll, Duration::min)
    }

    async fn step(&mut self, domain: &DomainRuntime, github: &impl PrMerger, item: &Due) -> Result<Step> {
        let pr = &item.pr;
        let status = match github.status(pr).await {
            Ok(status) => {
                self.recovered(domain, pr);
                status
            }
            Err(reason) => {
                self.troubled(domain, pr, &reason);
                return Ok(Step::Wait);
            }
        };
        match status.state {
            PrState::Merged => {
                record_merged(domain, pr, &status)?;
                self.forget(domain, &pr.id);
                return Ok(Step::Settled);
            }
            PrState::Closed => return Ok(Step::Settled),
            PrState::Open if status.head != item.head => return Ok(Step::Settled),
            PrState::Open => {}
        }
        if self.queued.contains(&pr.id) {
            return match github.in_queue(pr).await {
                Ok(true) => Ok(Step::Wait),
                Ok(false) => match github.status(pr).await {
                    Ok(now) if now.state == PrState::Merged => {
                        record_merged(domain, pr, &now)?;
                        self.forget(domain, &pr.id);
                        Ok(Step::Settled)
                    }
                    Ok(now) if now.state == PrState::Open && now.head == item.head => {
                        self.queued.remove(&pr.id);
                        self.failed(domain, item, "GitHub's merge queue removed it without merging it")?;
                        Ok(Step::Settled)
                    }
                    Ok(_) => Ok(Step::Settled),
                    Err(reason) => {
                        self.troubled(domain, pr, &reason);
                        Ok(Step::Wait)
                    }
                },
                Err(reason) => {
                    self.troubled(domain, pr, &reason);
                    Ok(Step::Wait)
                }
            };
        }
        if status.merge_state == MergeState::Conflicting {
            self.clear_action(domain, pr);
            hand_back(domain, item, &format!("it has conflicts with {base}: merge {base} into its branch and resolve them",
                base = status.base))?;
            return Ok(Step::Settled);
        }
        match &status.checks {
            Checks::Failed(names) => {
                self.clear_action(domain, pr);
                hand_back(domain, item, &format!("its checks failed ({})", names.join(", ")))?;
                return Ok(Step::Settled);
            }
            Checks::ActionRequired(names) => {
                self.ask_action(domain, pr, names);
                return Ok(Step::Wait);
            }
            Checks::Pending => return Ok(Step::Wait),
            Checks::None if status.merge_state == MergeState::Computing => return Ok(Step::Wait),
            Checks::None if status.merge_state != MergeState::Clean => match github.required_checks(pr, &status.base).await {
                Ok(required) if !required.is_empty() => return Ok(Step::Wait),
                Ok(_) => self.clear_action(domain, pr),
                Err(reason) => {
                    log!("cannot read the checks {} requires of {}: {reason}", status.base, pr.id);
                    return Ok(Step::Wait);
                }
            },
            Checks::None | Checks::Passed => self.clear_action(domain, pr),
        }
        let authorization = match &item.authorization {
            Some(authorization) => authorization.clone(),
            None => {
                let revision = domain.state()?.revision;
                let authorization = AuthorizationId(format!("auto-{}-{}-{revision}", item.presentation, pr.id));
                let authorize = Command::AuthorizeMerge { authorization_id: authorization.clone(), presentation_id: item.presentation.clone(),
                    pr_id: pr.id.clone(), expected_head: item.head.clone(), method: item.method };
                if !recorded(execute_system(domain, authorize))? {
                    return Ok(Step::Wait);
                }
                authorization
            }
        };
        let state = domain.state()?;
        let Some(attempt) = state.merge_authorizations.get(&authorization) else { return Ok(Step::Wait) };
        if attempt.attempt == MergeAttemptState::Unclaimed || attempt.claimed_by.is_some() {
            let claim = Command::ClaimMergeAuthorization { authorization_id: authorization.clone(),
                remote_head: status.head.clone(), remote_base: status.base.clone(), base_verified: true };
            if !recorded(execute_system(domain, claim))? {
                return Ok(Step::Wait);
            }
        }
        if !still_due(&*domain.state()?, item, &authorization) {
            return Ok(Step::Settled);
        }
        let marked = match github.is_draft(pr).await {
            Ok(false) => Ok(()),
            Ok(true) | Err(_) => github.ready(pr).await,
        };
        let merged = match marked {
            Ok(()) => github.merge(pr, &item.head, item.method).await,
            Err(reason) => Err(reason),
        };
        match merged {
            Ok(()) => {
                self.transient.remove(&pr.id);
                if let Some(Trouble { card: Some(card), .. }) = self.merge_trouble.remove(&pr.id) {
                    let _ = domain.withdraw_problem(&card);
                }
                log!("asked GitHub to merge {} at {}", pr_url(&pr.host, &pr.repository, pr.number), item.head);
                self.confirm(domain, github, pr).await
            }
            Err(reason) if already_merged(&reason) => self.confirm(domain, github, pr).await,
            Err(reason) if has(&reason, &["merge queue"]) => {
                self.queued.insert(pr.id.clone());
                Ok(Step::Wait)
            }
            Err(reason) if checks_waiting(&reason) => Ok(Step::Wait),
            Err(reason) if environment(&reason) => {
                self.merge_troubled(domain, pr, &reason);
                Ok(Step::Wait)
            }
            Err(reason) if transient(&reason) => {
                let failures = self.transient.get(&pr.id).map_or(0, |retry| retry.failures) + 1;
                if failures < TRANSIENT_MERGE_TRIES {
                    let wait = merge_retry_wait(self.first_retry, failures);
                    log!("merging {} failed, trying again in {} s: {reason}", pr.id, wait.as_secs());
                    self.transient.insert(pr.id.clone(), Retry { failures, at: Instant::now() + wait });
                    return Ok(Step::Retry);
                }
                self.transient.remove(&pr.id);
                self.failed(domain, item, &format!("GitHub kept failing after {failures} tries: {reason}"))?;
                Ok(Step::Settled)
            }
            Err(reason) => {
                self.transient.remove(&pr.id);
                self.failed(domain, item, &reason)?;
                Ok(Step::Settled)
            }
        }
    }

    /// After `gh pr merge`, only GitHub saying MERGED records the merge; otherwise the PR waits
    /// in GitHub's merge queue.
    async fn confirm(&mut self, domain: &DomainRuntime, github: &impl PrMerger, pr: &PullRequest) -> Result<Step> {
        match github.status(pr).await {
            Ok(status) if status.state == PrState::Merged => {
                record_merged(domain, pr, &status)?;
                Ok(Step::Settled)
            }
            _ => {
                self.queued.insert(pr.id.clone());
                Ok(Step::Wait)
            }
        }
    }

    fn failed(&mut self, domain: &DomainRuntime, item: &Due, reason: &str) -> Result<()> {
        let state = domain.state()?;
        for authorization in state.merge_authorizations.values().filter(|a| a.pr_id == item.pr.id
            && matches!(a.attempt, MergeAttemptState::Claimed | MergeAttemptState::Uncertain))
        {
            recorded(execute_system(domain, Command::ObserveMergeAttempt {
                authorization_id: authorization.id.clone(), state: MergeAttemptState::Failed,
            }))?;
        }
        hand_back(domain, item, reason)
    }

    fn troubled(&mut self, domain: &DomainRuntime, pr: &PullRequest, reason: &str) {
        let trouble = self.trouble.entry(pr.id.clone()).or_insert_with(|| Trouble { count: 0, since: Instant::now(), card: None });
        raise_trouble(domain, pr, reason, trouble, self.give_up);
    }

    fn merge_troubled(&mut self, domain: &DomainRuntime, pr: &PullRequest, reason: &str) {
        let trouble = self.merge_trouble.entry(pr.id.clone()).or_insert_with(|| Trouble { count: 0, since: Instant::now(), card: None });
        raise_trouble(domain, pr, reason, trouble, self.give_up);
    }

    /// Everything kept about a PR that is no longer due, with the cards raised for it.
    fn forget(&mut self, domain: &DomainRuntime, pr: &PrId) {
        self.waited.remove(pr);
        self.pressed.remove(pr);
        self.transient.remove(pr);
        self.queued.remove(pr);
        let cards = [self.trouble.remove(pr).and_then(|t| t.card), self.merge_trouble.remove(pr).and_then(|t| t.card),
            self.action.remove(pr)];
        for card in cards.into_iter().flatten() {
            let _ = domain.withdraw_problem(&card);
        }
    }

    fn forget_all_but(&mut self, domain: &DomainRuntime, due: &BTreeSet<PrId>) {
        let known: BTreeSet<PrId> = self.waited.keys().chain(self.pressed.keys()).chain(self.transient.keys()).chain(self.queued.iter())
            .chain(self.trouble.keys()).chain(self.merge_trouble.keys())
            .chain(self.action.keys()).cloned().collect();
        for pr in known.difference(due) {
            self.forget(domain, pr);
        }
    }

    #[cfg(test)]
    pub(crate) fn tracked(&self) -> usize {
        self.waited.len() + self.pressed.len() + self.transient.len() + self.queued.len() + self.trouble.len()
            + self.merge_trouble.len() + self.action.len()
    }
}

fn raise_trouble(domain: &DomainRuntime, pr: &PullRequest, reason: &str, trouble: &mut Trouble, give_up: Duration) {
    log!("cannot merge {}: {reason}", pr.id);
    if !transient(reason) {
        trouble.count += 1;
    }
    if trouble.card.is_some() || (trouble.count < NON_TRANSIENT_FAILURES_BEFORE_CARD && trouble.since.elapsed() < give_up) {
        return;
    }
    let id = format!("problem-merge-{}-{}", pr.id, crate::auth::random_hex(6));
    let need = clip(&format!("Alondra can't merge PR #{}: {}", pr.number, reason.trim()), CardSpec::NEED_MAX);
    let why = clip(&format!("GitHub keeps failing when Alondra reads or merges PR #{}, so it stays unmerged until this is fixed.", pr.number),
        CardSpec::WHY_MAX);
    let diagnosis = format!(
        "Alondra uses the GitHub CLI (gh) on this Mac to merge {}. Last error: {}. Check that gh is installed and logged in \
         (gh auth status) with access to {}. Alondra keeps trying and withdraws this card once it works.",
        pr_url(&pr.host, &pr.repository, pr.number), reason.trim(), pr.repository
    );
    match domain.raise_problem(&id, Some(&format!("merge:{}", pr.id)), Scope::Task { task_id: pr.task_id.clone() }, need, why, diagnosis, vec![]) {
        Ok(_) => trouble.card = Some(id.into()),
        Err(error) => log!("could not tell the user Alondra cannot merge {}: {error:?}", pr.id),
    }
}

impl Merger {
    fn recovered(&mut self, domain: &DomainRuntime, pr: &PullRequest) {
        if let Some(Trouble { card: Some(card), .. }) = self.trouble.remove(&pr.id) {
            let _ = domain.withdraw_problem(&card);
        }
    }

    fn ask_action(&mut self, domain: &DomainRuntime, pr: &PullRequest, names: &[String]) {
        if self.action.contains_key(&pr.id) {
            return;
        }
        let id = format!("problem-checks-{}-{}", pr.id, crate::auth::random_hex(6));
        let need = clip(&format!("Act on the checks of PR #{} on GitHub", pr.number), CardSpec::NEED_MAX);
        let why = clip(&format!("GitHub says {} need someone to act before Alondra can merge PR #{}.", names.join(", "), pr.number),
            CardSpec::WHY_MAX);
        let diagnosis = format!("Open {} and act on {} (for example, approve the workflow run). Alondra merges the PR once its checks pass.",
            pr_url(&pr.host, &pr.repository, pr.number), names.join(", "));
        if domain.raise_problem(&id, Some(&format!("checks:{}", pr.id)), Scope::Task { task_id: pr.task_id.clone() }, need, why, diagnosis, vec![]).is_ok() {
            self.action.insert(pr.id.clone(), id.into());
        }
    }

    fn clear_action(&mut self, domain: &DomainRuntime, pr: &PullRequest) {
        if let Some(card) = self.action.remove(&pr.id) {
            let _ = domain.withdraw_problem(&card);
        }
    }
}

fn record_merged(domain: &DomainRuntime, pr: &PullRequest, status: &Status) -> Result<()> {
    log!("{} is merged at {}", pr_url(&pr.host, &pr.repository, pr.number), status.head);
    recorded(execute_system(domain, Command::ObservePrState {
        pr_id: pr.id.clone(), head: status.head.clone(), base: status.base.clone(),
        state: PrState::Merged, merged_head: Some(status.head.clone()),
    })).map(|_| ())
}

/// `false` when the domain refused the command, which is logged.
fn recorded(result: Result<(), StoreError>) -> Result<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(StoreError::Domain(error)) => {
            log!("merge step refused: {error}");
            Ok(false)
        }
        Err(StoreError::Storage(error)) => Err(error),
    }
}

fn hand_back(domain: &DomainRuntime, item: &Due, reason: &str) -> Result<()> {
    log!("handing {} back to its node: {reason}", item.pr.id);
    let pr = &item.pr;
    let body = alondra_core::domain::merge_failed_text(pr.number, &pr_url(&pr.host, &pr.repository, pr.number), &pr.base, reason);
    recorded(execute_system(domain, Command::SubmitFeedback {
        attachments: vec![], presentation_id: item.presentation.clone(), targets: item.targets.clone(),
        body, instruction: Instruction::Conflicts,
    })).map(|_| ())
}

const FIRST_BACKOFF: Duration = Duration::from_secs(5);
const LAST_BACKOFF: Duration = Duration::from_secs(5 * 60);

fn next_backoff(backoff: Duration) -> Duration {
    (backoff * 2).min(LAST_BACKOFF)
}

pub(crate) async fn run(domain: Arc<DomainRuntime>, github: Arc<impl PrMerger>, mut merger: Merger) {
    let mut changed = domain.subscribe();
    let mut backoff = FIRST_BACKOFF;
    loop {
        let pause = match merger.merge_due(&domain, &*github).await {
            Ok(waiting) => {
                backoff = FIRST_BACKOFF;
                waiting.then(|| merger.pause())
            }
            Err(error) => {
                log!("merging pull requests failed, trying again in {} s: {error:#}", backoff.as_secs());
                let pause = backoff;
                backoff = next_backoff(backoff);
                Some(pause)
            }
        };
        let woke = match pause {
            Some(pause) => tokio::select! {
                change = changed.changed() => change.is_ok(),
                () = tokio::time::sleep(pause) => true,
            },
            None => changed.changed().await.is_ok(),
        };
        if !woke {
            return;
        }
    }
}

#[cfg(test)]
pub(crate) struct FakeGithub {
    pub statuses: std::sync::Mutex<BTreeMap<u64, Result<Status, String>>>,
    pub merges: std::sync::Mutex<BTreeMap<u64, Result<(), String>>>,
    /// PRs a successful merge puts in GitHub's merge queue instead of merging.
    pub queue: std::sync::Mutex<BTreeMap<u64, bool>>,
    pub merged: std::sync::Mutex<Vec<(u64, String, MergeMethod)>>,
    pub reads: std::sync::atomic::AtomicUsize,
    pub on_read: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// PRs the queue merges between Alondra's status read and its queue check.
    pub merged_by_queue: std::sync::Mutex<BTreeSet<u64>>,
    /// What GitHub answers to "is it a draft", per PR; unlisted PRs are not drafts.
    pub drafts: std::sync::Mutex<BTreeMap<u64, Result<bool, String>>>,
    pub readies: std::sync::Mutex<BTreeMap<u64, Result<(), String>>>,
    /// `ready` and `merge` calls, in the order they happened.
    pub calls: std::sync::Mutex<Vec<String>>,
    /// The checks each PR's base requires; unlisted PRs require none.
    pub required: std::sync::Mutex<BTreeMap<u64, Result<Vec<String>, String>>>,
}

#[cfg(test)]
impl FakeGithub {
    pub(crate) fn new() -> Self {
        Self { statuses: Default::default(), merges: Default::default(), queue: Default::default(),
            merged: Default::default(), reads: Default::default(), on_read: Default::default(), merged_by_queue: Default::default(),
            drafts: Default::default(), readies: Default::default(), calls: Default::default(),
            required: Default::default() }
    }
    pub(crate) fn open(&self, number: u64, head: &str, checks: Checks) {
        self.statuses.lock().unwrap().insert(number, Ok(Status { state: PrState::Open, head: head.into(), base: "main".into(), checks,
            merge_state: MergeState::Clean }));
    }
    /// An open PR whose `mergeStateStatus` is BLOCKED.
    pub(crate) fn open_blocked(&self, number: u64, head: &str, checks: Checks) {
        self.open(number, head, checks);
        if let Some(Ok(status)) = self.statuses.lock().unwrap().get_mut(&number) {
            status.merge_state = MergeState::Other;
        }
    }
}

#[cfg(test)]
impl PrMerger for FakeGithub {
    fn status<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, Status> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(hook) = self.on_read.lock().unwrap().take() {
            hook();
        }
        let status = self.statuses.lock().unwrap().get(&pr.number).cloned().unwrap_or_else(|| Err("no such PR".into()));
        Box::pin(async move { status })
    }
    fn merge<'a>(&'a self, pr: &'a PullRequest, head: &'a str, method: MergeMethod) -> Pending<'a, ()> {
        let result = self.merges.lock().unwrap().get(&pr.number).cloned().unwrap_or(Ok(()));
        self.calls.lock().unwrap().push(format!("merge {}", pr.number));
        if result.is_ok() {
            self.merged.lock().unwrap().push((pr.number, head.to_owned(), method));
            if !self.queue.lock().unwrap().contains_key(&pr.number)
                && let Some(Ok(status)) = self.statuses.lock().unwrap().get_mut(&pr.number)
            {
                status.state = PrState::Merged;
            }
        }
        Box::pin(async move { result })
    }
    fn is_draft<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> {
        let draft = self.drafts.lock().unwrap().get(&pr.number).cloned().unwrap_or(Ok(false));
        Box::pin(async move { draft })
    }
    fn ready<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, ()> {
        self.calls.lock().unwrap().push(format!("ready {}", pr.number));
        let result = self.readies.lock().unwrap().get(&pr.number).cloned().unwrap_or(Ok(()));
        Box::pin(async move { result })
    }
    fn required_checks<'a>(&'a self, pr: &'a PullRequest, _base: &'a str) -> Pending<'a, Vec<String>> {
        self.calls.lock().unwrap().push(format!("required {}", pr.number));
        let required = self.required.lock().unwrap().get(&pr.number).cloned().unwrap_or(Ok(vec![]));
        Box::pin(async move { required })
    }
    fn in_queue<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> {
        if self.merged_by_queue.lock().unwrap().contains(&pr.number)
            && let Some(Ok(status)) = self.statuses.lock().unwrap().get_mut(&pr.number)
        {
            status.state = PrState::Merged;
            return Box::pin(async { Ok(false) });
        }
        let queued = self.queue.lock().unwrap().get(&pr.number).copied().unwrap_or(false);
        Box::pin(async move { Ok(queued) })
    }
}

#[cfg(test)]
mod tests;
