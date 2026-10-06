//! Alondra merges the PR of a final delivery, with the same `gh` the watcher reads it with: once
//! the user accepts the delivery, or, for an auto-merge Request, once its checks pass. A merge
//! GitHub refuses goes back to the node with GitHub's reason, as feedback on that delivery.
use crate::domain_github::{GhCli, PrObserver, execute_system, pr_url};
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use alondra_core::domain::{
    AuthorizationId, Command, DeliveryTarget, DomainState, Instruction, InteractionId, InteractionPayload,
    MergeAttemptState, MergeMethod, PrId, PrState, PullRequest,
};
use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Checks {
    Passed,
    Pending,
    Failed(Vec<String>),
}

type Pending<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

pub(crate) trait PrMerger: Send + Sync + 'static {
    fn checks<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, Checks>;
    /// Merges the PR only while GitHub still has it at `head`.
    fn merge<'a>(&'a self, pr: &'a PullRequest, head: &'a str, method: MergeMethod) -> Pending<'a, ()>;
}

impl<T: PrMerger> PrMerger for Arc<T> {
    fn checks<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, Checks> {
        (**self).checks(pr)
    }
    fn merge<'a>(&'a self, pr: &'a PullRequest, head: &'a str, method: MergeMethod) -> Pending<'a, ()> {
        (**self).merge(pr, head, method)
    }
}

impl PrMerger for GhCli {
    fn checks<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, Checks> {
        Box::pin(async move {
            let url = pr_url(&pr.host, &pr.repository, pr.number);
            let out = self.run(&["pr", "view", &url, "--json", "statusCheckRollup"], None, Duration::from_secs(30)).await?;
            let value: Value = serde_json::from_slice(&out).map_err(|e| e.to_string())?;
            Ok(rollup(&value))
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
}

fn rollup(value: &Value) -> Checks {
    use alondra_core::pr::CheckState;
    let checks: Vec<_> = value["statusCheckRollup"].as_array().into_iter().flatten()
        .filter_map(crate::prs::check).collect();
    let failed: Vec<String> = checks.iter().filter(|c| c.state == CheckState::Failed).map(|c| c.name.clone()).collect();
    if !failed.is_empty() {
        Checks::Failed(failed)
    } else if checks.iter().any(|c| c.state == CheckState::Running) {
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
    /// The user's authorization, absent for an auto-merge Request.
    pub authorization: Option<(AuthorizationId, MergeAttemptState, bool)>,
}

/// The open PRs of current final deliveries that the user accepted, or that an auto-merge
/// Request lets Alondra merge, and that nobody handed back since.
pub(crate) fn due(state: &DomainState) -> Vec<Due> {
    let mut due = vec![];
    for task in state.tasks.values().filter(|task| !task.state.terminal()) {
        let Some(p) = task.final_presentation.as_ref().and_then(|p| state.interactions.get(p)) else { continue };
        let InteractionPayload::Delivery { targets, acceptance, feedback, .. } = &p.payload else { continue };
        if state.handed_back(p).is_some() {
            continue;
        }
        let accepted = acceptance.as_ref().is_some_and(|a| a.valid);
        let auto = !accepted && acceptance.is_none() && feedback.is_empty()
            && state.requests.get(&task.request_id).is_some_and(|r| r.auto_merge);
        for target in targets {
            let DeliveryTarget::PullRequest { pr_id, head } = target else { continue };
            let Some(pr) = state.pull_requests.get(pr_id).filter(|pr| pr.state == PrState::Open && &pr.head == head) else { continue };
            let authorization = state.merge_authorizations.values().find(|a| {
                a.valid && a.presentation_id == p.id && &a.pr_id == pr_id && &a.head == head
                    && matches!(a.attempt, MergeAttemptState::Unclaimed | MergeAttemptState::Claimed | MergeAttemptState::Uncertain)
            });
            let (method, authorization) = match (accepted, authorization) {
                (true, Some(a)) => (a.method, Some((a.id.clone(), a.attempt, a.claimed_by.is_some()))),
                _ if auto => match pr.allowed_methods.first() {
                    Some(method) => (*method, None),
                    None => continue,
                },
                _ => continue,
            };
            due.push(Due { presentation: p.id.clone(), targets: targets.clone(), pr: pr.clone(), head: head.clone(), method, authorization });
        }
    }
    due
}

const TRANSIENT_FAILURES_BEFORE_HANDING_BACK: u32 = 3;

/// Failures that say nothing about the PR: GitHub or the network did not answer.
fn transient(reason: &str) -> bool {
    let reason = reason.to_ascii_lowercase();
    ["error connecting", "timed out", "did not finish", "could not resolve", "connection", "http 5", "502", "503", "504", "rate limit"]
        .iter().any(|sign| reason.contains(sign))
}

pub(crate) fn merge_failed_text(pr: &PullRequest, reason: &str) -> String {
    format!(
        "Alondra could not merge pull request #{number} ({url}): {reason}. Fix what stops the merge (conflicts with {base}, \
         failing checks, or a review GitHub requires), push, and present the result again with present_result and the new head. \
         If only the user can unblock it, ask them.",
        number = pr.number, url = pr_url(&pr.host, &pr.repository, pr.number), reason = reason.trim().trim_end_matches('.'), base = pr.base,
    )
}

/// Merges what is due. `true` while something waits for checks or for GitHub to answer, so the
/// caller looks again without a domain change.
pub(crate) async fn merge_due(
    domain: &DomainRuntime, github: &(impl PrObserver + PrMerger), failures: &mut BTreeMap<PrId, u32>,
) -> Result<bool> {
    let mut waiting = false;
    for item in due(&domain.state()?) {
        let pr = &item.pr;
        let facts = match github.observe(&pr.host, &pr.repository, pr.number).await {
            Ok(facts) if facts.state == PrState::Open && facts.head == item.head => facts,
            Ok(_) => continue,
            Err(reason) => {
                log!("cannot merge {}: GitHub did not report it: {reason}", pr.id);
                waiting = true;
                continue;
            }
        };
        match github.checks(pr).await {
            Ok(Checks::Passed) => {}
            Ok(Checks::Pending) => {
                waiting = true;
                continue;
            }
            Ok(Checks::Failed(names)) => {
                hand_back(domain, &item, &format!("its checks failed ({})", names.join(", ")))?;
                continue;
            }
            Err(reason) => {
                log!("cannot read the checks of {}: {reason}", pr.id);
                waiting = true;
                continue;
            }
        }
        if let Some((authorization, attempt, node_claim)) = &item.authorization
            && (*attempt == MergeAttemptState::Unclaimed || *node_claim)
        {
            let claim = Command::ClaimMergeAuthorization {
                authorization_id: authorization.clone(), remote_head: facts.head.clone(),
                remote_base: facts.base.clone(), base_verified: true,
            };
            match execute_system(domain, claim) {
                Ok(()) => {}
                Err(StoreError::Domain(error)) => {
                    log!("Alondra did not claim the merge of {}: {error}", pr.id);
                    continue;
                }
                Err(StoreError::Storage(error)) => return Err(error),
            }
        }
        match github.merge(pr, &item.head, item.method).await {
            Ok(()) => {
                failures.remove(&pr.id);
                log!("merged {} at {}", pr_url(&pr.host, &pr.repository, pr.number), item.head);
                record(execute_system(domain, Command::ObservePrState {
                    pr_id: pr.id.clone(), head: item.head.clone(), base: facts.base.clone(),
                    state: PrState::Merged, merged_head: Some(item.head.clone()),
                }))?;
            }
            Err(reason) => {
                let count = failures.entry(pr.id.clone()).or_default();
                *count += 1;
                if transient(&reason) && *count < TRANSIENT_FAILURES_BEFORE_HANDING_BACK {
                    log!("merging {} failed, trying again: {reason}", pr.id);
                    waiting = true;
                    continue;
                }
                failures.remove(&pr.id);
                if let Some((authorization, ..)) = &item.authorization {
                    record(execute_system(domain, Command::ObserveMergeAttempt {
                        authorization_id: authorization.clone(), state: MergeAttemptState::Failed,
                    }))?;
                }
                hand_back(domain, &item, &reason)?;
            }
        }
    }
    Ok(waiting)
}

fn record(result: Result<(), StoreError>) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(StoreError::Domain(error)) => {
            log!("merge record refused: {error}");
            Ok(())
        }
        Err(StoreError::Storage(error)) => Err(error),
    }
}

fn hand_back(domain: &DomainRuntime, item: &Due, reason: &str) -> Result<()> {
    log!("handing {} back to its node: {reason}", item.pr.id);
    record(execute_system(domain, Command::SubmitFeedback {
        attachments: vec![], presentation_id: item.presentation.clone(), targets: item.targets.clone(),
        body: merge_failed_text(&item.pr, reason), instruction: Instruction::Conflicts,
    }))
}

const CHECKS_POLL: Duration = Duration::from_secs(60);

pub(crate) async fn run(domain: Arc<DomainRuntime>, github: Arc<impl PrObserver + PrMerger>) -> Result<()> {
    let mut changed = domain.subscribe();
    let mut failures = BTreeMap::new();
    loop {
        let waiting = merge_due(&domain, &*github, &mut failures).await?;
        if waiting {
            tokio::select! {
                change = changed.changed() => change?,
                () = tokio::time::sleep(CHECKS_POLL) => {}
            }
        } else {
            changed.changed().await?;
        }
    }
}

#[cfg(test)]
pub(crate) struct FakeGithub {
    pub observer: crate::domain_github::FakeObserver,
    pub checks: std::sync::Mutex<BTreeMap<u64, Result<Checks, String>>>,
    pub merges: std::sync::Mutex<BTreeMap<u64, Result<(), String>>>,
    pub merged: std::sync::Mutex<Vec<(u64, String, MergeMethod)>>,
}

#[cfg(test)]
impl FakeGithub {
    pub(crate) fn new() -> Self {
        Self { observer: crate::domain_github::FakeObserver::new(), checks: Default::default(),
            merges: Default::default(), merged: Default::default() }
    }
}

#[cfg(test)]
impl PrObserver for FakeGithub {
    fn observe<'a>(&'a self, host: &'a str, repository: &'a str, number: u64)
        -> Pin<Box<dyn Future<Output = Result<crate::domain_github::PrFacts, String>> + Send + 'a>> {
        self.observer.observe(host, repository, number)
    }
}

#[cfg(test)]
impl PrMerger for FakeGithub {
    fn checks<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, Checks> {
        let checks = self.checks.lock().unwrap().get(&pr.number).cloned().unwrap_or(Ok(Checks::Passed));
        Box::pin(async move { checks })
    }
    fn merge<'a>(&'a self, pr: &'a PullRequest, head: &'a str, method: MergeMethod) -> Pending<'a, ()> {
        let result = self.merges.lock().unwrap().get(&pr.number).cloned().unwrap_or(Ok(()));
        if result.is_ok() {
            self.merged.lock().unwrap().push((pr.number, head.to_owned(), method));
        }
        Box::pin(async move { result })
    }
}

#[cfg(test)]
mod tests;
