//! PR trains: a card is stacked on another open card of the same repo when its base branch is
//! that card's head branch. The bottom of a train has to be merged before the ones above it.

use crate::pr::{PrState, PullRequest};
use crate::view::View;

/// Still open on GitHub: a card set aside in Alondra was never merged or closed there.
pub fn open_on_github(pr: &PullRequest) -> bool {
    matches!(pr.state, PrState::Open | PrState::Dismissed)
}

impl View {
    fn directly_below(&self, pr: &PullRequest) -> Option<&PullRequest> {
        let base = pr.base.as_deref()?;
        self.prs.values().find(|p| p.id != pr.id && open_on_github(p) && p.repo == pr.repo && p.head.as_deref() == Some(base))
    }

    fn in_cycle(&self, pr: &PullRequest) -> bool {
        let mut at = pr;
        for _ in 0..self.prs.len() {
            match self.directly_below(at) {
                Some(p) if p.id == pr.id => return true,
                Some(p) => at = p,
                None => return false,
            }
        }
        false
    }

    /// The open card this one is stacked on; none for cards that stack on each other in a loop.
    pub fn stack_below(&self, pr: &PullRequest) -> Option<&PullRequest> {
        if !open_on_github(pr) || self.in_cycle(pr) {
            return None;
        }
        self.directly_below(pr).filter(|p| !self.in_cycle(p))
    }

    /// Why a card cannot be merged yet: the open PR it is stacked on.
    pub fn merge_blocked_by(&self, id: u64) -> Option<&PullRequest> {
        self.prs.get(&id).and_then(|p| self.stack_below(p))
    }
}
