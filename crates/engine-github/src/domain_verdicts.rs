//! Whether GitHub would let each PR the user can merge actually merge, kept in the domain so the
//! merge button says so before anyone presses it. GitHub answers per PR and head; Alondra records
//! the answer, and an answer it could not get stands as "unknown" until it can.
use crate::domain_github::execute_system;
use alondra_engine_ports::github::PrMerger;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use alondra_core::clock;
use alondra_domain_model::{Command, DeliveryTarget, DomainState, InteractionPayload, MergeVerdict, PrState, PullRequest};
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;

/// The open PRs of the current deliveries of tasks still running, at the head each presents.
pub fn watched(state: &DomainState) -> Vec<PullRequest> {
    let mut watched: Vec<PullRequest> = vec![];
    for task in state.tasks.values().filter(|task| !task.over()) {
        let Some(presentation) = task.final_presentation.as_ref().and_then(|id| state.interactions.get(id)) else { continue };
        let InteractionPayload::Delivery { targets, .. } = &presentation.payload else { continue };
        for target in targets {
            let DeliveryTarget::PullRequest { pr_id, head } = target else { continue };
            let Some(pr) = state.pull_requests.get(pr_id).filter(|pr| pr.state == PrState::Open && &pr.head == head) else { continue };
            if watched.iter().all(|seen| seen.id != pr.id) {
                watched.push(pr.clone());
            }
        }
    }
    watched
}

/// Records what GitHub says of merging `pr` at its head, unless the domain already holds it.
pub fn record(domain: &dyn alondra_engine_ports::Runtime, pr: &PullRequest, verdict: MergeVerdict) -> Result<()> {
    let domain = DomainRuntime::of(domain);
    let state = domain.state()?;
    let known = state.merge_verdicts.get(&pr.id).is_some_and(|observed| observed.head == pr.head && observed.verdict == verdict);
    if known {
        return Ok(());
    }
    match execute_system(domain, Command::ObserveMergeVerdict { pr_id: pr.id.clone(), head: pr.head.clone(), verdict }) {
        Ok(()) => Ok(()),
        Err(StoreError::Domain(error)) => {
            log!("merge verdict of {} refused: {error}", pr.id);
            Ok(())
        }
        Err(StoreError::Storage(error)) => Err(error),
    }
}

/// Asks GitHub about `pr` and records the answer; GitHub not answering records "unknown".
pub async fn read(domain: &DomainRuntime, github: &impl PrMerger, pr: &PullRequest) -> Result<MergeVerdict> {
    let verdict = match github.verdict(pr).await {
        Ok(verdict) => verdict,
        Err(reason) => {
            log!("cannot tell whether {} can merge: {reason}", pr.id);
            MergeVerdict::Unknown
        }
    };
    record(domain, pr, verdict.clone())?;
    Ok(verdict)
}

/// Reads the PRs of deliveries: every one when `all`, otherwise only those without an answer for
/// their head, so a change elsewhere in the domain never costs a GitHub read of every PR.
pub async fn refresh(domain: &DomainRuntime, github: &impl PrMerger, all: bool) -> Result<()> {
    let state = domain.state()?;
    for pr in watched(&state) {
        let answered = state.merge_verdicts.get(&pr.id).is_some_and(|observed| observed.head == pr.head && observed.verdict != MergeVerdict::Unknown);
        if all || !answered {
            read(domain, github, &pr).await?;
        }
    }
    Ok(())
}

pub async fn run(domain: Arc<DomainRuntime>, github: Arc<impl PrMerger>, every: Duration) {
    let mut changed = domain.subscribe();
    let mut all = true;
    loop {
        if let Err(error) = refresh(&domain, &*github, all).await {
            log!("reading what GitHub says of merging failed: {error:#}");
        }
        all = tokio::select! {
            change = changed.changed() => { if change.is_err() { return } false }
            () = clock::sleep(every) => true,
        };
    }
}
