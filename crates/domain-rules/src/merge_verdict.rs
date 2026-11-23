//! Whether the provider would let a pull request merge, as the provider's adapter reports it.
//!
//! The domain never learns the provider's words: the adapter reduces what the provider says to
//! this verdict, and the merge button follows it.
use serde::{Deserialize, Serialize};

/// What stops a pull request from merging.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MergeBlocker {
    /// A check the base requires failed.
    RequiredCheckFailing { name: String },
    /// A check the base requires is still running.
    RequiredCheckRunning { name: String },
    /// A check the base requires has not reported.
    RequiredCheckMissing { name: String },
    /// A check the base requires waits for someone to act on it.
    CheckNeedsAction { name: String },
    /// The base requires a review the pull request has not got.
    ReviewRequired,
    /// A reviewer asked for changes.
    ChangesRequested,
    /// It conflicts with its base.
    Conflicts,
    /// Its branch must include the base's latest commits first.
    BehindBase,
    /// The provider blocks the merge and says no more.
    Other,
}

/// Something the user should know about a merge that is allowed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MergeNote {
    /// The merge goes past rules the signed-in account may bypass.
    BypassingRules { names: Vec<String> },
    /// A check failed that the base does not require.
    CheckFailingNotRequired { name: String },
    /// A check is still running that the base does not require.
    CheckRunningNotRequired { name: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MergeVerdict {
    Mergeable {
        #[serde(default)]
        notes: Vec<MergeNote>,
    },
    Blocked { blockers: Vec<MergeBlocker> },
    /// The provider has not answered, or answered something not yet readable.
    Unknown,
}

impl crate::DomainState {
    /// For tests of what comes after the provider allows a merge: the observations a poll of the
    /// provider would have made so that every open PR `command` accepts or authorizes
    /// may merge at its head.
    #[doc(hidden)]
    pub fn mergeable_observations(&self, command: &crate::Command) -> Vec<crate::Command> {
        self.observations_for(command, false)
    }

    fn observations_for(&self, command: &crate::Command, presenting: bool) -> Vec<crate::Command> {
        use crate::{Command, DeliveryTarget, PrState};
        let wanted: Vec<(crate::PrId, String)> = match command {
            Command::AuthorizeMerge { pr_id, expected_head, .. } => vec![(pr_id.clone(), expected_head.clone())],
            Command::AcceptDelivery { targets, .. } => targets.iter().filter_map(|target| match target {
                DeliveryTarget::PullRequest { pr_id, head } => Some((pr_id.clone(), head.clone())),
                _ => None,
            }).collect(),
            Command::PresentResult { task_id, .. } if presenting => self.pull_requests.values()
                .filter(|pr| &pr.task_id == task_id).map(|pr| (pr.id.clone(), pr.head.clone())).collect(),
            _ => vec![],
        };
        let wanted = wanted.into_iter().map(|(pr_id, head)| {
            let head = if head.is_empty() { self.pull_requests.get(&pr_id).map(|pr| pr.head.clone()).unwrap_or_default() } else { head };
            (pr_id, head)
        });
        wanted.filter(|(pr_id, head)| {
            let open = self.pull_requests.get(pr_id).is_some_and(|pr| pr.state == PrState::Open && &pr.head == head);
            open && !self.merge_verdicts.get(pr_id).is_some_and(|observed| &observed.head == head)
        }).map(|(pr_id, head)| Command::ObserveMergeVerdict { pr_id, head, verdict: MergeVerdict::mergeable() }).collect()
    }

    /// `mergeable_observations`, recorded straight into the state without moving its revision.
    #[doc(hidden)]
    pub fn seed_mergeable_for(&mut self, command: &crate::Command) {
        self.seed(command, false);
    }

    /// `seed_mergeable_for`, also for the PRs a result is presenting: screens that show the merge
    /// button of a delivery need its verdict as soon as it is presented.
    #[doc(hidden)]
    pub fn seed_mergeable_for_presented(&mut self, command: &crate::Command) {
        self.seed(command, true);
    }

    /// For tests of screens: the provider lets every open PR merge at the head it has now.
    #[doc(hidden)]
    pub fn seed_every_open_pr_mergeable(&mut self) {
        let open: Vec<(crate::PrId, String)> = self.pull_requests.values()
            .filter(|pr| pr.state == crate::PrState::Open).map(|pr| (pr.id.clone(), pr.head.clone())).collect();
        for (pr_id, head) in open {
            self.merge_verdicts.insert(pr_id, ObservedVerdict { head, verdict: MergeVerdict::mergeable() });
        }
    }

    fn seed(&mut self, command: &crate::Command, presenting: bool) {
        for observation in self.observations_for(command, presenting) {
            if let crate::Command::ObserveMergeVerdict { pr_id, head, verdict } = observation {
                self.merge_verdicts.insert(pr_id, ObservedVerdict { head, verdict });
            }
        }
    }
}

impl MergeVerdict {
    pub fn mergeable() -> Self {
        Self::Mergeable { notes: vec![] }
    }
}

/// The verdict read for one head of a pull request: another head voids it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedVerdict {
    pub head: String,
    pub verdict: MergeVerdict,
}
