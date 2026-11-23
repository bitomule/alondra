//! Alondra merges the PR of a final delivery, with the same `gh` the watcher reads it with: once
//! the user accepts the delivery (or presses Merge), or, for an auto-merge Request, once its
//! checks pass. Every merge first takes a durable system claim, which checks the head, the base
//! and the stack (dependencies only hold back a start), and the delivery is read again right
//! before `gh pr merge`. A
//! merge counts only when GitHub reports the PR merged. A merge GitHub refuses goes back to the
//! node with GitHub's reason, as feedback on that delivery. A merge the user authorized does not
//! wait for required checks the rulesets let the signed-in account bypass: it merges past them.
use alondra_engine_ports::Runtime as _;
use alondra_daemon_support::locks::Poisonable as _;
use alondra_core::clock;
use alondra_core::clock::Instant;
use crate::domain_github::{GhFailure, execute_system, pr_url};
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use alondra_domain_rules::{
    AuthorizationId, CardSpec, Command, DeliveryTarget, DomainError, DomainState, Feedback, FeedbackTarget, Instruction, InteractionId, InteractionPayload,
    MergeAttemptState, MergeMethod, PrId, PrState, PullRequest, Scope, clip,
};
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

pub use alondra_engine_ports::github::{Checks, MergeState, Pending, PrMerger, Status};

/// One PR of a final delivery Alondra should merge now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Due {
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
pub fn due(state: &DomainState) -> Vec<Due> {
    let mut due = vec![];
    for task in state.tasks.values().filter(|task| !task.over()) {
        let Some(p) = task.final_presentation.as_ref().and_then(|p| state.interactions.get(p)) else { continue };
        let InteractionPayload::Delivery { targets, acceptance, feedback, .. } = &p.payload else { continue };
        if !state.open_blockers(&task.id).is_empty() || state.stack_ready(task).is_err() {
            continue;
        }
        for target in targets {
            let DeliveryTarget::PullRequest { pr_id, head } = target else { continue };
            let feedback_target = FeedbackTarget::PullRequest { pr_id: pr_id.clone(), head: head.clone() };
            let auto = acceptance.is_none() && feedback.iter().all(|feedback| !feedback.target.affects(&feedback_target))
                && state.requests.get(&task.request_id).is_some_and(|r| r.auto_merge);
            let Some(pr) = state.pull_requests.get(pr_id).filter(|pr| pr.is_tracked() && pr.state == PrState::Open && &pr.head == head) else { continue };
            let authorization = state.merge_authorizations.values().find(|a| {
                a.valid && a.presentation_id == p.id && &a.pr_id == pr_id && &a.head == head
                    && matches!(a.attempt, MergeAttemptState::Unclaimed | MergeAttemptState::Claimed | MergeAttemptState::Uncertain)
            });
            let authorized_by = authorization.map_or(alondra_domain_rules::Feedback::ALONDRA, |a| a.user_id.as_str());
            if state.authorized_merge_held(p, authorized_by, &feedback_target) {
                continue;
            }
            let (method, authorization) = match (authorization, MergeMethod::for_pr(state.merge_method, &pr.allowed_methods)) {
                (Some(a), _) => (a.method, Some(a.id.clone())),
                (None, Some(method)) if auto => (method, None),
                _ => continue,
            };
            due.push(Due { presentation: p.id.clone(), targets: targets.clone(), pr: pr.clone(), head: head.clone(), method, authorization });
        }
    }
    due
}

/// The delivery is still the one to merge, and Alondra's claim on it still stands.
fn still_due(state: &DomainState, item: &Due, authorization: &AuthorizationId) -> bool {
    let Some(task) = state.tasks.get(&item.pr.task_id).filter(|task| !task.over()) else { return false };
    let Some(p) = task.final_presentation.as_ref().filter(|p| **p == item.presentation).and_then(|p| state.interactions.get(p)) else {
        return false;
    };
    let authorized_by = state.merge_authorizations.get(authorization).map_or(alondra_domain_rules::Feedback::ALONDRA, |a| a.user_id.as_str());
    state.stack_ready_for_merge(&item.pr.id).is_ok()
        && !state.authorized_merge_held(p, authorized_by, &FeedbackTarget::PullRequest { pr_id: item.pr.id.clone(), head: item.head.clone() })
        && state.pull_requests.get(&item.pr.id).is_some_and(|pr| pr.state == PrState::Open && pr.head == item.head)
        && state.merge_authorizations.get(authorization).is_some_and(|a| a.valid && a.claimed_by.is_none()
            && matches!(a.attempt, MergeAttemptState::Claimed | MergeAttemptState::Uncertain))
}

/// A final delivery whose PR GitHub confirms is still a draft cannot be merged, so it is refused;
/// when GitHub does not answer, the delivery is accepted and the merger marks the PR ready itself.
pub async fn draft_refusal(state: &DomainState, targets: &[DeliveryTarget], github: &impl PrMerger) -> Option<DomainError> {
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

fn observe_status(domain: &DomainRuntime, pr_id: &PrId, status: &Status) -> Result<bool> {
    recorded(execute_system(domain, Command::ObservePrState { pr_id: pr_id.clone(),
        head: status.head.clone(), base: status.base.clone(), state: status.state,
        merged_head: (status.state == PrState::Merged).then(|| status.head.clone()) }))
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

pub struct Merger {
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
    /// PRs whose merge past their required checks GitHub refused: they wait for the checks.
    bypass_refused: BTreeSet<PrId>,
}

impl Default for Merger {
    fn default() -> Self {
        Self::with(Duration::from_secs(60), Duration::from_secs(15 * 60))
    }
}

impl Merger {
    pub fn with(poll: Duration, give_up: Duration) -> Self {
        Self { poll, give_up, first_retry: FIRST_MERGE_RETRY, waited: BTreeMap::new(), transient: BTreeMap::new(),
            trouble: BTreeMap::new(), merge_trouble: BTreeMap::new(), queued: BTreeSet::new(), action: BTreeMap::new(),
            pressed: BTreeMap::new(), bypass_refused: BTreeSet::new() }
    }

    /// Merges what is due. `true` while something waits, so the caller looks again.
    pub async fn merge_due(&mut self, domain: &DomainRuntime, github: &impl PrMerger) -> Result<bool> {
        let mut waiting = false;
        let due = due(&*domain.state()?);
        let open: BTreeSet<_> = due.iter().map(|item| item.pr.id.clone()).collect();
        self.forget_all_but(domain, &open);
        for item in due {
            let now = clock::instant();
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
    pub fn pause(&self) -> Duration {
        let now = clock::instant();
        self.transient.values().filter(|retry| retry.at > now).map(|retry| retry.at - now).fold(self.poll, Duration::min)
    }

    async fn retarget_stack(&mut self, domain: &DomainRuntime, github: &impl PrMerger, item: &Due, status: Status) -> Result<Option<Status>> {
        let state = domain.state()?;
        let Some(stack) = state.tasks.get(&item.pr.task_id).and_then(|task| task.stack.as_ref()).cloned() else {
            return self.retarget_sibling(domain, github, item, status).await;
        };
        if state.stack_ready(&state.tasks[&item.pr.task_id]).is_err() { return Ok(None); }
        let Some(upstream) = state.pull_requests.get(&stack.pr_id) else { return Ok(None) };
        if upstream.host != item.pr.host || upstream.repository != item.pr.repository { return Ok(None); }
        let remote = match github.status(upstream).await {
            Ok(remote) => remote,
            Err(error) => { self.troubled(domain, &item.pr, &error); return Ok(None); }
        };
        if remote.state != PrState::Merged || remote.head != stack.head { return Ok(None); }
        let default = match github.default_branch(&item.pr).await {
            Ok(base) => base,
            Err(error) => { self.troubled(domain, &item.pr, &error); return Ok(None); }
        };
        if remote.base != default { return Ok(None); }
        record_merged(domain, upstream, &remote)?;
        let mut current = match github.status(&item.pr).await {
            Ok(current) => current,
            Err(error) => { self.troubled(domain, &item.pr, &error); return Ok(None); }
        };
        if !observe_status(domain, &item.pr.id, &current)? { return Ok(None); }
        if current.state != PrState::Open || current.head != item.head || current.base != status.base { return Ok(None); }
        let state = domain.state()?;
        if state.tasks.get(&item.pr.task_id).and_then(|task| task.stack.as_ref()) != Some(&stack)
            || !due(&state).iter().any(|now| now.pr.id == item.pr.id && now.head == item.head
                && now.presentation == item.presentation && now.authorization == item.authorization)
        { return Ok(None); }
        if current.base != default {
            return self.move_onto(domain, github, item, &default).await;
        }
        Ok((current.state == PrState::Open && current.head == item.head).then_some(current))
    }

    /// A pull request opened on the branch of another pull request of its own task, with no stack declared:
    /// it waits until that one is merged, then moves onto the default branch before it merges.
    async fn retarget_sibling(&mut self, domain: &DomainRuntime, github: &impl PrMerger, item: &Due, status: Status) -> Result<Option<Status>> {
        let state = domain.state()?;
        let siblings: Vec<_> = state.pull_requests.values().filter(|other| other.id != item.pr.id && other.task_id == item.pr.task_id
            && other.host == item.pr.host && other.repository == item.pr.repository).cloned().collect();
        if siblings.is_empty() { return Ok(Some(status)); }
        let default = match github.default_branch(&item.pr).await {
            Ok(base) => base,
            Err(error) => { self.troubled(domain, &item.pr, &error); return Ok(None); }
        };
        if status.base == default { return Ok(Some(status)); }
        for sibling in &siblings {
            let remote = match github.status(sibling).await {
                Ok(remote) => remote,
                Err(error) => { self.troubled(domain, &item.pr, &error); return Ok(None); }
            };
            if remote.head_ref != status.base { continue; }
            match remote.state {
                PrState::Open => return Ok(None),
                PrState::Closed => return Ok(Some(status)),
                PrState::Merged if remote.base != default => return Ok(None),
                PrState::Merged => record_merged(domain, sibling, &remote)?,
            }
            return self.move_onto(domain, github, item, &default).await;
        }
        Ok(Some(status))
    }

    async fn move_onto(&mut self, domain: &DomainRuntime, github: &impl PrMerger, item: &Due, default: &str) -> Result<Option<Status>> {
        if let Err(error) = github.retarget(&item.pr, default).await {
            self.troubled(domain, &item.pr, &error);
            return Ok(None);
        }
        let current = match github.status(&item.pr).await {
            Ok(current) => current,
            Err(error) => { self.troubled(domain, &item.pr, &error); return Ok(None); }
        };
        if !observe_status(domain, &item.pr.id, &current)? { return Ok(None); }
        Ok((current.state == PrState::Open && current.head == item.head && current.base == default).then_some(current))
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
        let Some(status) = self.retarget_stack(domain, github, item, status).await? else { return Ok(Step::Wait) };
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
            let update = domain.state()?.branch_update;
            hand_back(domain, item, &format!("it has conflicts with {base}: {how}", base = status.base, how = update.summary(&status.base)))?;
            return Ok(Step::Settled);
        }
        let mut bypass = false;
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
            Checks::None if status.merge_state == MergeState::Computing => return Ok(Step::Wait),
            Checks::None if status.merge_state != MergeState::Clean => match github.required_checks(pr, &status.base).await {
                Ok(required) if !required.is_empty() => {
                    if !self.may_bypass(domain, github, item, &status.base, &required).await? { return Ok(Step::Wait) }
                    bypass = true;
                    self.clear_action(domain, pr);
                }
                Ok(_) => self.clear_action(domain, pr),
                Err(reason) => {
                    log!("cannot read the checks {} requires of {}: {reason}", status.base, pr.id);
                    return Ok(Step::Wait);
                }
            },
            Checks::Pending => match github.required_checks(pr, &status.base).await {
                Ok(required) if required.iter().any(|check| !status.passed.contains(check)) => {
                    let missing: Vec<String> = required.into_iter().filter(|check| !status.passed.contains(check)).collect();
                    if !self.may_bypass(domain, github, item, &status.base, &missing).await? { return Ok(Step::Wait) }
                    bypass = true;
                    self.clear_action(domain, pr);
                }
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
                    pr_id: pr.id.clone(), expected_head: item.head.clone(), method: item.method, despite_pending_message: false };
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
        if let Err(failure) = marked {
            return self.not_ready(domain, item, &failure);
        }
        if !still_due(&*domain.state()?, item, &authorization) {
            return Ok(Step::Settled);
        }
        if domain.state()?.tasks.get(&pr.task_id).is_some_and(|task| task.stack.is_some()) {
            match github.status(pr).await {
                Ok(current) if current == status => {}
                Ok(current) if current.state == PrState::Merged => {
                    record_merged(domain, pr, &current)?;
                    return Ok(Step::Settled);
                }
                Ok(_) => return Ok(Step::Wait),
                Err(error) => { self.troubled(domain, pr, &error); return Ok(Step::Wait); }
            }
        }
        if !still_due(&*domain.state()?, item, &authorization) { return Ok(Step::Settled); }
        let merged = if bypass {
            github.merge_bypassing(pr, &item.head, item.method).await
        } else {
            github.merge(pr, &item.head, item.method).await
        };
        match merged {
            Ok(()) => {
                self.transient.remove(&pr.id);
                if let Some(Trouble { card: Some(card), .. }) = self.merge_trouble.remove(&pr.id) {
                    let _ = domain.withdraw_problem(&card);
                }
                let past = if bypass { ", past its required checks" } else { "" };
                log!("asked GitHub to merge {} at {}{past}", pr_url(&pr.host, &pr.repository, pr.number), item.head);
                self.confirm(domain, github, pr).await
            }
            Err(failure) => {
                if bypass {
                    log!("GitHub refused to merge {} past its required checks, waiting for them: {failure}", pr.id);
                    self.bypass_refused.insert(pr.id.clone());
                }
                self.merge_failed(domain, github, item, &failure).await
            }
        }
    }

    /// The user authorized the merge and every required check still missing is one GitHub lets
    /// the signed-in account bypass. An auto-merge always waits for its checks.
    async fn may_bypass(&self, domain: &DomainRuntime, github: &impl PrMerger, item: &Due, base: &str, missing: &[String]) -> Result<bool> {
        let by_user = match &item.authorization {
            Some(authorization) => domain.state()?.merge_authorizations.get(authorization).is_some_and(|a| a.user_id != Feedback::ALONDRA),
            None => false,
        };
        if !by_user || self.bypass_refused.contains(&item.pr.id) {
            return Ok(false);
        }
        match github.bypassable_checks(&item.pr, base).await {
            Ok(bypassable) => Ok(missing.iter().all(|check| bypassable.contains(check))),
            Err(reason) => {
                log!("cannot read which checks {base} lets {} merge past: {reason}", item.pr.id);
                Ok(false)
            }
        }
    }

    /// `gh pr merge` failed: what GitHub says of the PR now decides, never what gh wrote.
    async fn merge_failed(&mut self, domain: &DomainRuntime, github: &impl PrMerger, item: &Due, failure: &GhFailure) -> Result<Step> {
        let pr = &item.pr;
        if failure.needs_setup() {
            self.merge_troubled(domain, pr, failure);
            return Ok(Step::Wait);
        }
        let status = match github.status(pr).await {
            Ok(status) => status,
            Err(_) => return self.retry(domain, item, failure),
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
        if github.in_queue(pr).await == Ok(true) {
            self.queued.insert(pr.id.clone());
            return Ok(Step::Wait);
        }
        if status.merge_state == MergeState::Other && !matches!(status.checks, Checks::Failed(_) | Checks::ActionRequired(_)) {
            match github.required_checks(pr, &status.base).await {
                Ok(required) if required.iter().any(|check| !status.passed.contains(check)) => return Ok(Step::Wait),
                Ok(_) => {}
                Err(_) => return self.retry(domain, item, failure),
            }
        }
        if github.can_merge(pr).await == Ok(false) {
            self.merge_troubled(domain, pr, failure);
            return Ok(Step::Wait);
        }
        if failure.timed_out() || status.merge_state == MergeState::Clean {
            return self.retry(domain, item, failure);
        }
        self.transient.remove(&pr.id);
        self.failed(domain, item, &failure.to_string())?;
        Ok(Step::Settled)
    }

    fn not_ready(&mut self, domain: &DomainRuntime, item: &Due, failure: &GhFailure) -> Result<Step> {
        if failure.needs_setup() {
            self.merge_troubled(domain, &item.pr, failure);
            return Ok(Step::Wait);
        }
        if failure.timed_out() {
            return self.retry(domain, item, failure);
        }
        self.transient.remove(&item.pr.id);
        self.failed(domain, item, &failure.to_string())?;
        Ok(Step::Settled)
    }

    /// A failure GitHub may not repeat: tried again after a doubling wait, then handed back.
    fn retry(&mut self, domain: &DomainRuntime, item: &Due, failure: &GhFailure) -> Result<Step> {
        let pr = &item.pr;
        let failures = self.transient.get(&pr.id).map_or(0, |retry| retry.failures) + 1;
        if failures < TRANSIENT_MERGE_TRIES {
            let wait = merge_retry_wait(self.first_retry, failures);
            log!("merging {} failed, trying again in {} s: {failure}", pr.id, wait.as_secs());
            self.transient.insert(pr.id.clone(), Retry { failures, at: clock::instant() + wait });
            return Ok(Step::Retry);
        }
        self.transient.remove(&pr.id);
        self.failed(domain, item, &format!("GitHub kept failing after {failures} tries: {failure}"))?;
        Ok(Step::Settled)
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

    /// Reading the PR failed. Only `gh` that needs installing or a sign-in counts toward the card
    /// at once; anything else raises it once it has gone on for `give_up`.
    fn troubled(&mut self, domain: &DomainRuntime, pr: &PullRequest, failure: &GhFailure) {
        let trouble = self.trouble.entry(pr.id.clone()).or_insert_with(|| Trouble { count: 0, since: clock::instant(), card: None });
        raise_trouble(domain, pr, failure, failure.needs_setup(), trouble, self.give_up);
    }

    /// Merging needs `gh` set up or access to the repository, which only the user can give.
    fn merge_troubled(&mut self, domain: &DomainRuntime, pr: &PullRequest, failure: &GhFailure) {
        let trouble = self.merge_trouble.entry(pr.id.clone()).or_insert_with(|| Trouble { count: 0, since: clock::instant(), card: None });
        raise_trouble(domain, pr, failure, true, trouble, self.give_up);
    }

    /// Everything kept about a PR that is no longer due, with the cards raised for it.
    fn forget(&mut self, domain: &DomainRuntime, pr: &PrId) {
        self.waited.remove(pr);
        self.pressed.remove(pr);
        self.transient.remove(pr);
        self.queued.remove(pr);
        self.bypass_refused.remove(pr);
        let cards = [self.trouble.remove(pr).and_then(|t| t.card), self.merge_trouble.remove(pr).and_then(|t| t.card),
            self.action.remove(pr)];
        for card in cards.into_iter().flatten() {
            let _ = domain.withdraw_problem(&card);
        }
    }

    fn forget_all_but(&mut self, domain: &DomainRuntime, due: &BTreeSet<PrId>) {
        let known: BTreeSet<PrId> = self.waited.keys().chain(self.pressed.keys()).chain(self.transient.keys()).chain(self.queued.iter())
            .chain(self.bypass_refused.iter()).chain(self.trouble.keys()).chain(self.merge_trouble.keys())
            .chain(self.action.keys()).cloned().collect();
        for pr in known.difference(due) {
            self.forget(domain, pr);
        }
    }

    pub fn tracked(&self) -> usize {
        self.waited.len() + self.pressed.len() + self.transient.len() + self.queued.len() + self.bypass_refused.len() + self.trouble.len()
            + self.merge_trouble.len() + self.action.len()
    }
}

fn raise_trouble(domain: &DomainRuntime, pr: &PullRequest, failure: &GhFailure, counts: bool, trouble: &mut Trouble, give_up: Duration) {
    let reason = failure.to_string();
    log!("cannot merge {}: {reason}", pr.id);
    if counts {
        trouble.count += 1;
    }
    if trouble.card.is_some() || (trouble.count < NON_TRANSIENT_FAILURES_BEFORE_CARD && alondra_core::clock::since(trouble.since) < give_up) {
        return;
    }
    let id = format!("problem-merge-{}-{}", pr.slug(), crate::auth::random_hex(6));
    let need = clip(&format!("Alondra can't merge PR #{}: {}", pr.number, reason.trim()), CardSpec::NEED_MAX);
    let why = clip(&format!("GitHub keeps failing when Alondra reads or merges PR #{}, so it stays unmerged until this is fixed.", pr.number),
        CardSpec::WHY_MAX);
    let diagnosis = format!(
        "Alondra uses the GitHub CLI (gh) on this Mac to merge {}. Last error: {}. Check that gh is installed and logged in \
         (gh auth status) with access to {}. Alondra keeps trying and withdraws this card once it works.",
        pr_url(&pr.host, &pr.repository, pr.number), reason.trim(), pr.repository
    );
    match domain.raise_problem(&id, None, Some(&format!("merge:{}", pr.slug())), Scope::Task { task_id: pr.task_id.clone() }, need, why, diagnosis, vec![]) {
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
        let id = format!("problem-checks-{}-{}", pr.slug(), crate::auth::random_hex(6));
        let need = clip(&format!("Act on the checks of PR #{} on GitHub", pr.number), CardSpec::NEED_MAX);
        let why = clip(&format!("GitHub says {} need someone to act before Alondra can merge PR #{}.", names.join(", "), pr.number),
            CardSpec::WHY_MAX);
        let diagnosis = format!("Open {} and act on {} (for example, approve the workflow run). Alondra merges the PR once its checks pass.",
            pr_url(&pr.host, &pr.repository, pr.number), names.join(", "));
        if domain.raise_problem(&id, None, Some(&format!("checks:{}", pr.slug())), Scope::Task { task_id: pr.task_id.clone() }, need, why, diagnosis, vec![]).is_ok() {
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
    let body = alondra_domain_rules::merge_failed_text(pr.number, &pr_url(&pr.host, &pr.repository, pr.number), &pr.base, reason);
    recorded(execute_system(domain, Command::SubmitFeedback {
        attachments: vec![], presentation_id: item.presentation.clone(),
        targets: vec![DeliveryTarget::PullRequest { pr_id: pr.id.clone(), head: item.head.clone() }],
        body, instruction: Instruction::Conflicts,
        merge_failure: Some(alondra_domain_rules::MergeFailure { number: pr.number, reason: reason.trim().trim_end_matches('.').to_owned() }),
    })).map(|_| ())
}

const FIRST_BACKOFF: Duration = Duration::from_secs(5);
const LAST_BACKOFF: Duration = Duration::from_secs(5 * 60);

fn next_backoff(backoff: Duration) -> Duration {
    (backoff * 2).min(LAST_BACKOFF)
}

pub async fn run(domain: Arc<DomainRuntime>, github: Arc<impl PrMerger>, mut merger: Merger) {
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
                () = clock::sleep(pause) => true,
            },
            None => changed.changed().await.is_ok(),
        };
        if !woke {
            return;
        }
    }
}

pub struct FakeGithub {
    pub statuses: std::sync::Mutex<BTreeMap<u64, Result<Status, GhFailure>>>,
    pub merges: std::sync::Mutex<BTreeMap<u64, Result<(), GhFailure>>>,
    /// PRs a successful merge puts in GitHub's merge queue instead of merging.
    pub queue: std::sync::Mutex<BTreeMap<u64, bool>>,
    pub merged: std::sync::Mutex<Vec<(u64, String, MergeMethod)>>,
    pub reads: std::sync::atomic::AtomicUsize,
    pub on_read: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// PRs the queue merges between Alondra's status read and its queue check.
    pub merged_by_queue: std::sync::Mutex<BTreeSet<u64>>,
    /// What GitHub answers to "is it a draft", per PR; unlisted PRs are not drafts.
    pub drafts: std::sync::Mutex<BTreeMap<u64, Result<bool, GhFailure>>>,
    pub readies: std::sync::Mutex<BTreeMap<u64, Result<(), GhFailure>>>,
    /// `ready` and `merge` calls, in the order they happened.
    pub calls: std::sync::Mutex<Vec<String>>,
    /// The checks each PR's base requires; unlisted PRs require none.
    pub required: std::sync::Mutex<BTreeMap<u64, Result<Vec<String>, GhFailure>>>,
    /// The required checks the signed-in account may bypass on each PR's base; unlisted PRs none.
    pub bypassable: std::sync::Mutex<BTreeMap<u64, Result<Vec<String>, GhFailure>>>,
    /// What a failed merge leaves GitHub reporting of the PR's merge state.
    pub refused_as: std::sync::Mutex<BTreeMap<u64, MergeState>>,
    /// PRs GitHub merges although `gh pr merge` reports a failure.
    pub merged_anyway: std::sync::Mutex<BTreeSet<u64>>,
    /// PRs the signed-in account may not merge into.
    pub no_access: std::sync::Mutex<BTreeSet<u64>>,
    pub default_base: std::sync::Mutex<String>,
    pub retargets: std::sync::Mutex<Vec<(u64, String)>>,
    pub after_retarget: std::sync::Mutex<BTreeMap<u64, Status>>,
    pub on_draft: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
    pub on_default: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl FakeGithub {
    pub fn new() -> Self {
        Self { statuses: Default::default(), merges: Default::default(), queue: Default::default(),
            merged: Default::default(), reads: Default::default(), on_read: Default::default(), merged_by_queue: Default::default(),
            drafts: Default::default(), readies: Default::default(), calls: Default::default(),
            required: Default::default(), bypassable: Default::default(), refused_as: Default::default(), merged_anyway: Default::default(),
            no_access: Default::default(), default_base: std::sync::Mutex::new("main".into()),
            retargets: Default::default(), after_retarget: Default::default(), on_draft: Default::default(), on_default: Default::default() }
    }
    pub fn open(&self, number: u64, head: &str, checks: Checks) {
        self.statuses.lock_or_recover("a test's fixture").insert(number, Ok(Status { state: PrState::Open, head: head.into(), head_ref: String::new(), base: "main".into(), checks,
            passed: BTreeSet::new(), merge_state: MergeState::Clean }));
    }
    /// An open PR whose `mergeStateStatus` is BLOCKED.
    pub fn open_blocked(&self, number: u64, head: &str, checks: Checks) {
        self.open(number, head, checks);
        if let Some(Ok(status)) = self.statuses.lock_or_recover("a test's fixture").get_mut(&number) {
            status.merge_state = MergeState::Other;
        }
    }
}


impl FakeGithub {
    fn fake_merge<'a>(&'a self, pr: &'a PullRequest, head: &'a str, method: MergeMethod, call: String) -> Pending<'a, ()> {
        let result = self.merges.lock_or_recover("a test's fixture").get(&pr.number).cloned().unwrap_or(Ok(()));
        self.calls.lock_or_recover("a test's fixture").push(call);
        if result.is_err()
            && let Some(merge_state) = self.refused_as.lock_or_recover("a test's fixture").get(&pr.number)
            && let Some(Ok(status)) = self.statuses.lock_or_recover("a test's fixture").get_mut(&pr.number)
        {
            status.merge_state = *merge_state;
        }
        if result.is_err()
            && self.merged_anyway.lock_or_recover("a test's fixture").contains(&pr.number)
            && let Some(Ok(status)) = self.statuses.lock_or_recover("a test's fixture").get_mut(&pr.number)
        {
            status.state = PrState::Merged;
        }
        if result.is_ok() {
            self.merged.lock_or_recover("a test's fixture").push((pr.number, head.to_owned(), method));
            if !self.queue.lock_or_recover("a test's fixture").contains_key(&pr.number)
                && let Some(Ok(status)) = self.statuses.lock_or_recover("a test's fixture").get_mut(&pr.number)
            {
                status.state = PrState::Merged;
            }
        }
        Box::pin(async move { result })
    }
}

impl PrMerger for FakeGithub {
    fn default_branch<'a>(&'a self, _pr: &'a PullRequest) -> Pending<'a, String> {
        let hook = self.on_default.lock_or_recover("a test's fixture").take();
        if let Some(hook) = hook { hook(); }
        let base = self.default_base.lock_or_recover("a test's fixture").clone();
        Box::pin(async move { Ok(base) })
    }
    fn retarget<'a>(&'a self, pr: &'a PullRequest, base: &'a str) -> Pending<'a, ()> {
        self.retargets.lock_or_recover("a test's fixture").push((pr.number, base.to_owned()));
        self.calls.lock_or_recover("a test's fixture").push(format!("retarget {} {base}", pr.number));
        if let Some(Ok(status)) = self.statuses.lock_or_recover("a test's fixture").get_mut(&pr.number) {
            status.base = base.to_owned();
            if let Some(next) = self.after_retarget.lock_or_recover("a test's fixture").remove(&pr.number) { *status = next; }
        }
        Box::pin(async { Ok(()) })
    }
    fn status<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, Status> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(hook) = self.on_read.lock_or_recover("a test's fixture").take() {
            hook();
        }
        let status = self.statuses.lock_or_recover("a test's fixture").get(&pr.number).cloned().unwrap_or_else(|| Err(GhFailure::Exit(Some(1), "no such PR".into())));
        Box::pin(async move { status })
    }
    fn merge<'a>(&'a self, pr: &'a PullRequest, head: &'a str, method: MergeMethod) -> Pending<'a, ()> {
        self.fake_merge(pr, head, method, format!("merge {}", pr.number))
    }
    fn merge_bypassing<'a>(&'a self, pr: &'a PullRequest, head: &'a str, method: MergeMethod) -> Pending<'a, ()> {
        self.fake_merge(pr, head, method, format!("merge {} --admin", pr.number))
    }
    fn bypassable_checks<'a>(&'a self, pr: &'a PullRequest, _base: &'a str) -> Pending<'a, Vec<String>> {
        self.calls.lock_or_recover("a test's fixture").push(format!("bypassable {}", pr.number));
        let bypassable = self.bypassable.lock_or_recover("a test's fixture").get(&pr.number).cloned().unwrap_or(Ok(vec![]));
        Box::pin(async move { bypassable })
    }
    fn is_draft<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> {
        let hook = self.on_draft.lock_or_recover("a test's fixture").take();
        if let Some(hook) = hook { hook(); }
        let draft = self.drafts.lock_or_recover("a test's fixture").get(&pr.number).cloned().unwrap_or(Ok(false));
        Box::pin(async move { draft })
    }
    fn ready<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, ()> {
        self.calls.lock_or_recover("a test's fixture").push(format!("ready {}", pr.number));
        let result = self.readies.lock_or_recover("a test's fixture").get(&pr.number).cloned().unwrap_or(Ok(()));
        Box::pin(async move { result })
    }
    fn required_checks<'a>(&'a self, pr: &'a PullRequest, _base: &'a str) -> Pending<'a, Vec<String>> {
        self.calls.lock_or_recover("a test's fixture").push(format!("required {}", pr.number));
        let required = self.required.lock_or_recover("a test's fixture").get(&pr.number).cloned().unwrap_or(Ok(vec![]));
        Box::pin(async move { required })
    }
    fn can_merge<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> {
        let allowed = !self.no_access.lock_or_recover("a test's fixture").contains(&pr.number);
        Box::pin(async move { Ok(allowed) })
    }
    fn in_queue<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> {
        if self.merged_by_queue.lock_or_recover("a test's fixture").contains(&pr.number)
            && let Some(Ok(status)) = self.statuses.lock_or_recover("a test's fixture").get_mut(&pr.number)
        {
            status.state = PrState::Merged;
            return Box::pin(async { Ok(false) });
        }
        let queued = self.queue.lock_or_recover("a test's fixture").get(&pr.number).copied().unwrap_or(false);
        Box::pin(async move { Ok(queued) })
    }
}

#[cfg(test)]
mod tests;
