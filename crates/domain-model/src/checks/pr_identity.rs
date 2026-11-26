//! A pull request is its provider key, not its address. A read the adapter confirmed moves the
//! address to where the pull request lives now and keeps the old one as an alias; two records
//! of one key join into one, with every reference and decision of both and no remote effect.
use super::*;

/// What a user's action on a copy that joined another, or on a removed pull request, is told.
pub const PR_SUPERSEDED: &str = "This card was another copy of the same pull request; use the one that remains";
pub const PR_UNCHECKABLE: &str = "Alondra cannot check this pull request, so it cannot merge it";
pub const PR_DETACHED: &str = "This pull request is no longer in Alondra";

/// What the node of a task is told when the pull request it registered turns out to be the
/// one another live task registered first.
pub fn joined_elsewhere_notice(number: u64, title: &str) -> String {
    format!("Pull request #{number} is the same one task “{title}” registered first; go on without it.")
}

/// What the node is told after the user removed a pull request Alondra could not check.
pub fn detached_notice(number: u64) -> String {
    format!("Pull request #{number} is no longer in Alondra because its repository could not be checked; open another one if needed.")
}

impl DomainState {
    /// The record that stands for `pr_id` now: itself, or the one it joined.
    pub fn surviving_pr<'a>(&'a self, pr_id: &'a PrId) -> &'a PrId {
        let mut current = pr_id;
        for _ in 0..=self.pull_requests.len() {
            match self.pull_requests.get(current).map(|pr| &pr.tracking) {
                Some(PrTracking::Superseded { by }) => current = by,
                _ => return current,
            }
        }
        current
    }

    /// A command that acts on `pr_id` refuses a record that joined another or that the user removed.
    pub fn tracked_pr(&self, pr_id: &PrId) -> Result<&PullRequest> {
        let pr = self.pull_requests.get(pr_id).ok_or_else(|| DomainError::new("not_found", "PR missing"))?;
        match pr.tracking {
            PrTracking::Tracked => Ok(pr),
            PrTracking::Superseded { .. } => fail("pr_superseded", PR_SUPERSEDED),
            PrTracking::Detached { .. } => fail("pr_detached", PR_DETACHED),
        }
    }

    /// A pull request is acted on remotely only once the provider confirmed which one it is.
    pub fn identity_confirmed(&self, pr_id: &PrId) -> Result<()> {
        let Some(pr) = self.pull_requests.get(pr_id) else { return Ok(()) };
        if pr.identity.is_unverified() {
            return fail("pr_not_checked_yet", "The provider has not confirmed which pull request this is").map_err(|e| e.because(Refusal::PrNotCheckedYet));
        }
        Ok(())
    }

    /// Every known pull request a command names, by its id or as a target, is still tracked.
    pub fn refuse_untracked_prs(&self, command: &Command) -> Result<()> {
        let known = |pr_id: &PrId| -> Result<()> {
            if self.pull_requests.contains_key(pr_id) {
                self.tracked_pr(pr_id)?;
            }
            Ok(())
        };
        let targets = |targets: &[DeliveryTarget]| -> Result<()> {
            for target in targets {
                if let DeliveryTarget::PullRequest { pr_id, .. } = target {
                    known(pr_id)?;
                }
            }
            Ok(())
        };
        match command {
            Command::AuthorizeMerge { pr_id, .. } => {
                known(pr_id)?;
                match self.pull_requests.get(pr_id) {
                    Some(pr) if pr.identity.is_unresolvable() => fail("pr_uncheckable", PR_UNCHECKABLE),
                    _ => Ok(()),
                }
            }
            Command::ObservePrState { pr_id, .. }
            | Command::ObservePrMergeMethods { pr_id, .. }
            | Command::ObserveMergeVerdict { pr_id, .. }
            | Command::PublishStackReadiness { pr_id, .. }
            | Command::AdoptOpenPr { pr_id, .. }
            | Command::ClosePrOfCancelledTask { pr_id, .. }
            | Command::DetachPr { pr_id, .. } => known(pr_id),
            Command::SubmitFeedback { targets: t, .. }
            | Command::AcceptDelivery { targets: t, .. }
            | Command::DecideProposedPlan { targets: t, .. } => targets(t),
            _ => Ok(()),
        }
    }










    /// The combinations of identity, tracking and address no command may leave.
    pub fn validate_pr_identity(&self, pr: &PullRequest) -> Result<()> {
        let bad = |why: &str| fail("invalid_input", &format!("PR {}: {why}", pr.id));
        if pr.aliases.contains(&pr.repository) {
            return bad("its current address is not an alias");
        }
        for alias in &pr.aliases {
            canonical_pr(&pr.host, alias, pr.number)?;
        }
        if let Some(key) = pr.identity.key() {
            nonempty(&key.0)?;
        }
        if let PrIdentity::Verified { locator, .. } = &pr.identity {
            nonempty(&locator.0)?;
        }
        match &pr.tracking {
            PrTracking::Tracked => {
                let twin = self.pull_requests.values().any(|other| other.id != pr.id && other.is_tracked() && other.host == pr.host
                    && pr.identity.key().is_some() && other.identity.key() == pr.identity.key());
                if twin {
                    return bad("another tracked record has its key");
                }
            }
            PrTracking::Superseded { by } => {
                let Some(survivor) = self.pull_requests.get(by) else { return bad("it joined a record that does not exist") };
                if !pr.identity.is_verified() || matches!(survivor.tracking, PrTracking::Superseded { .. }) || survivor.identity.key() != pr.identity.key() || survivor.host != pr.host {
                    return bad("it joined a record that is not the same pull request or that joined another");
                }
            }
            PrTracking::Detached { .. } if !pr.identity.is_unresolvable() => return bad("only an unresolvable record is removed"),
            PrTracking::Detached { .. } => {}
        }
        Ok(())
    }



}
