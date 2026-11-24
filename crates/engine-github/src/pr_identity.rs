//! Which pull request a read of GitHub reached, decided here, in GitHub's adapter: the domain
//! only gets the verdict. A change to a pull request (close, merge, retarget, ready) carries the
//! key a read confirmed at its current address in the same round, and nothing else acts.
use alondra_engine_kernel::domain_runtime::RuntimeRecords;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use alondra_domain_rules::{Command, DomainState, PrAddressVerdict, PrIdentity, PrRead, ProviderPrKey, PullRequest};
use alondra_engine_ports::github::{GhFailure, PrObserver, ReadIdentity};
use anyhow::Result;
use std::collections::BTreeSet;

/// What one read of a pull request answered: which pull request and at which head, or why not.
pub type Answer<'a> = Result<(Option<&'a ReadIdentity>, &'a str), &'a GhFailure>;

/// A start time the domain accepts for a read beginning now.
pub fn read_start() -> alondra_domain_rules::Millis {
    alondra_core::clock::now_millis().max(1)
}

/// The branches the task of `pr` works on: a pull request from one of them is the task's own.
pub fn task_branches(state: &DomainState, pr: &PullRequest) -> BTreeSet<String> {
    state.tasks.get(&pr.task_id).into_iter().flat_map(|task| task.worktrees.values()).filter_map(|w| w.branch.clone()).collect()
}

/// What a read of `pr` through `read_repository` says about the record:
/// - a record with a key is the pull request that answered only with that key;
/// - without one, an answer from another address is GitHub's redirect of a moved or renamed
///   repository, whose pull request of that number is the same one;
/// - at the same address, it is the record's pull request only from one of its task's branches
///   or at the head the record has; otherwise the name was reused by another pull request.
pub fn verdict(pr: &PullRequest, branches: &BTreeSet<String>, read_repository: &str, answer: Answer<'_>) -> PrAddressVerdict {
    let (read, head) = match answer {
        Err(failure) if failure.not_found() => return PrAddressVerdict::NotFound,
        Err(_) | Ok((None, _)) => return PrAddressVerdict::Unreachable,
        Ok((Some(read), head)) => (read, head),
    };
    let confirmed = PrAddressVerdict::Confirmed { key: read.key.clone(), locator: read.locator.clone(), current_repository: read.repository.clone() };
    let same = match pr.identity.key() {
        Some(key) => *key == read.key,
        None => read.repository != read_repository || branches.contains(&read.head_ref) || head == pr.head,
    };
    if same { confirmed } else { PrAddressVerdict::NamesAnotherPr }
}

/// A confirmation the record already holds exactly: sending it again would only move the time
/// of its last read.
fn settled(pr: &PullRequest, read_repository: &str, verdict: &PrAddressVerdict) -> bool {
    let PrAddressVerdict::Confirmed { key, locator, current_repository } = verdict else { return false };
    pr.identity == PrIdentity::Verified { key: key.clone(), locator: locator.clone() }
        && pr.repository == *current_repository && read_repository == pr.repository
        && pr.missing.is_none() && pr.relocation_asked_at.is_none()
}

/// A verdict that would change nothing the domain keeps: no answer, a confirmation it already
/// holds, another miss of a record nobody can check, or a miss within the hour of the last one
/// that counted.
fn pointless(pr: &PullRequest, read_repository: &str, started: alondra_domain_rules::Millis, verdict: &PrAddressVerdict) -> bool {
    match verdict {
        PrAddressVerdict::Unreachable => true,
        PrAddressVerdict::Confirmed { .. } => settled(pr, read_repository, verdict),
        _ if pr.identity.is_unresolvable() => true,
        PrAddressVerdict::NotFound => pr.identity.locator().is_none()
            && pr.missing.is_some_and(|missing| started < missing.last.saturating_add(alondra_domain_rules::STRIKE_GAP)),
        PrAddressVerdict::NamesAnotherPr => false,
    }
}

/// Tells the domain what a read of `pr` through `read_repository`, started at `started`, says
/// about which pull request it is, and gives back the record as it stands then with the key every
/// change to it must carry: only when the read confirmed that the record is that pull request at
/// the address it has now.
pub fn identify(domain: &DomainRuntime, pr: &PullRequest, read_repository: &str, started: alondra_domain_rules::Millis, answer: Answer<'_>)
    -> Result<Option<(PullRequest, ProviderPrKey)>> {
    let state = domain.state()?;
    let pr_id = state.surviving_pr(&pr.id).clone();
    let Some(known) = state.pull_requests.get(&pr_id).filter(|known| known.is_tracked()) else { return Ok(None) };
    let verdict = verdict(known, &task_branches(&state, known), read_repository, answer);
    if !pointless(known, read_repository, started, &verdict) {
        let read = PrRead::Address { repository: read_repository.to_owned() };
        match domain.execute_system_command(Command::ObservePrAddress { pr_id: pr_id.clone(), read, read_started_at: started, verdict: verdict.clone() }) {
            Ok(()) => (),
            Err(StoreError::Domain(error)) => {
                log!("which pull request {}#{} is was refused: {error}", read_repository, pr.number);
                return Ok(None);
            }
            Err(StoreError::Storage(error)) => return Err(error),
        }
    }
    let PrAddressVerdict::Confirmed { key, current_repository, .. } = verdict else { return Ok(None) };
    let state = domain.state()?;
    let now = &state.pull_requests[state.surviving_pr(&pr_id)];
    let checked = now.is_tracked() && now.identity.is_verified() && now.identity.key() == Some(&key) && now.repository == current_repository;
    Ok(checked.then(|| (now.clone(), key)))
}

/// The key an action on `pr` may carry: a record that is still the pull request a read in this
/// round confirmed, never one only known by its address.
pub fn expected(pr: &PullRequest) -> Option<&ProviderPrKey> {
    (pr.is_tracked() && pr.identity.is_verified()).then(|| pr.identity.key()).flatten()
}

/// Finds again, by their locator, the pull requests whose address no longer leads to them, and
/// tells the domain where each is now, or that it is gone for certain.
pub async fn relocate(domain: &DomainRuntime, observer: &impl PrObserver, asked: &mut std::collections::BTreeMap<alondra_domain_rules::PrId, alondra_domain_rules::Millis>) -> Result<()> {
    let state = domain.state()?;
    let lost: Vec<PullRequest> = state.pull_requests.values()
        .filter(|pr| pr.is_tracked() && pr.relocation_asked_at.is_some() && pr.identity.locator().is_some()).cloned().collect();
    asked.retain(|id, _| lost.iter().any(|pr| pr.id == *id));
    for pr in lost {
        let (Some(at), Some(locator), Some(key)) = (pr.relocation_asked_at, pr.identity.locator(), pr.identity.key()) else { continue };
        if asked.get(&pr.id) == Some(&at) {
            continue;
        }
        let started = read_start().max(at.saturating_add(1));
        let verdict = match observer.relocate(&pr.host, locator).await {
            Ok(found) if found.key == *key => PrAddressVerdict::Confirmed { key: found.key, locator: found.locator, current_repository: found.repository },
            Ok(_) => PrAddressVerdict::NamesAnotherPr,
            Err(failure) if failure.not_found() => PrAddressVerdict::NotFound,
            Err(failure) => {
                log!("could not find {} by its locator: {failure}", pr.id);
                continue;
            }
        };
        asked.insert(pr.id.clone(), at);
        match domain.execute_system_command(Command::ObservePrAddress { pr_id: pr.id.clone(), read: PrRead::Locator, read_started_at: started, verdict }) {
            Ok(()) => (),
            Err(StoreError::Domain(error)) => log!("where {} is now was refused: {error}", pr.id),
            Err(StoreError::Storage(error)) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "pr_identity_tests.rs"]
mod tests;
