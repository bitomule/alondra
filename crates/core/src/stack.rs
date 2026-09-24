//! PR trains: a card is stacked on another open card of the same repo when its base branch is
//! that card's head branch. The bottom of a train has to be merged before the ones above it.

use crate::pr::{PrState, PullRequest};
use crate::view::View;

/// Where a card sits in its train: the card right below it, the bottom card and how many cards
/// lie between them. A card on its own is the bottom of a train of one.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct StackPlace<'a> {
    pub below: Option<&'a PullRequest>,
    pub bottom: &'a PullRequest,
    pub position: usize,
}

/// A PR to move onto a new base once the one under it is merged.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Retarget {
    pub id: u64,
    pub url: String,
    pub base: String,
}

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

    /// The train from its bottom card up to `pr`.
    pub fn stack_chain<'a>(&'a self, pr: &'a PullRequest) -> Vec<&'a PullRequest> {
        let mut chain = vec![pr];
        while let Some(below) = chain.last().and_then(|p| self.stack_below(p)) {
            if chain.iter().any(|p| p.id == below.id) {
                break;
            }
            chain.push(below);
        }
        chain.reverse();
        chain
    }

    pub fn stack_place<'a>(&'a self, pr: &'a PullRequest) -> StackPlace<'a> {
        let chain = self.stack_chain(pr);
        StackPlace { below: self.stack_below(pr), bottom: chain[0], position: chain.len() - 1 }
    }

    /// Cards in train order: trains by when their bottom card was opened, each bottom → top.
    pub fn in_train_order<'a>(&'a self, prs: impl Iterator<Item = &'a PullRequest>) -> Vec<&'a PullRequest> {
        let mut prs: Vec<&PullRequest> = prs.collect();
        prs.sort_by_cached_key(|p| {
            let place = self.stack_place(p);
            (place.bottom.opened_at, place.bottom.id, place.position, p.id)
        });
        prs
    }

    /// Why a card cannot be merged yet: the open PR it is stacked on.
    pub fn merge_blocked_by(&self, id: u64) -> Option<&PullRequest> {
        self.prs.get(&id).and_then(|p| self.stack_below(p))
    }

    /// The open cards stacked right on `merged`, each moved onto the branch `merged` went into.
    /// Read it before the merge is recorded: a merged card holds nothing up any more.
    pub fn retarget_plan(&self, merged: u64) -> Vec<Retarget> {
        let Some(pr) = self.prs.get(&merged) else { return vec![] };
        let Some(base) = pr.base.clone() else { return vec![] };
        self.prs
            .values()
            .filter(|p| self.stack_below(p).is_some_and(|below| below.id == merged))
            .map(|p| Retarget { id: p.id, url: p.url.clone(), base: base.clone() })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(cards: &[(u64, &str, &str, &str, PrState)]) -> View {
        let mut v = View::default();
        for &(id, repo, base, head, state) in cards {
            let mut pr = PullRequest::opened(id, &format!("https://github.com/{repo}/pull/{}", id + 100), "s", None, id as i64 * 10).unwrap();
            pr.base = Some(base.into());
            pr.head = Some(head.into());
            pr.state = state;
            v.prs.insert(id, pr);
        }
        v
    }

    use PrState::{Closed, Dismissed, Merged, Open};

    #[test]
    fn a_card_stacks_on_the_open_card_whose_head_is_its_base() {
        let cases: &[(&str, Vec<(u64, &str, &str, &str, PrState)>, u64, Option<u64>, Vec<u64>)] = &[
            ("no stack", vec![(1, "a/b", "main", "x", Open), (2, "a/b", "main", "y", Open)], 2, None, vec![2]),
            ("two long", vec![(1, "a/b", "main", "x", Open), (2, "a/b", "x", "y", Open)], 2, Some(1), vec![1, 2]),
            ("three long", vec![(1, "a/b", "main", "x", Open), (2, "a/b", "x", "y", Open), (3, "a/b", "y", "z", Open)], 3, Some(2), vec![1, 2, 3]),
            ("three long, bottom seen from the top card's id order", vec![(3, "a/b", "main", "x", Open), (1, "a/b", "x", "y", Open), (2, "a/b", "y", "z", Open)], 2, Some(1), vec![3, 1, 2]),
            ("another repo", vec![(1, "a/c", "main", "x", Open), (2, "a/b", "x", "y", Open)], 2, None, vec![2]),
            ("base merged", vec![(1, "a/b", "main", "x", Merged), (2, "a/b", "x", "y", Open)], 2, None, vec![2]),
            ("base closed", vec![(1, "a/b", "main", "x", Closed), (2, "a/b", "x", "y", Open)], 2, None, vec![2]),
            ("base set aside, still open on GitHub", vec![(1, "a/b", "main", "x", Dismissed), (2, "a/b", "x", "y", Open)], 2, Some(1), vec![1, 2]),
            ("a loop of two", vec![(1, "a/b", "y", "x", Open), (2, "a/b", "x", "y", Open)], 2, None, vec![2]),
            ("a card on top of a loop", vec![(1, "a/b", "y", "x", Open), (2, "a/b", "x", "y", Open), (3, "a/b", "x", "z", Open)], 3, None, vec![3]),
            ("its own base", vec![(1, "a/b", "x", "x", Open)], 1, None, vec![1]),
        ];
        for (what, cards, id, below, chain) in cases {
            let v = view(cards);
            let pr = &v.prs[id];
            assert_eq!(v.stack_below(pr).map(|p| p.id), *below, "{what}: below");
            assert_eq!(v.stack_chain(pr).iter().map(|p| p.id).collect::<Vec<_>>(), *chain, "{what}: chain");
            assert_eq!(v.stack_place(pr).position, chain.len() - 1, "{what}: position");
            assert_eq!(v.merge_blocked_by(*id).map(|p| p.id), *below, "{what}: merge");
        }
    }

    #[test]
    fn trains_are_listed_together_bottom_first() {
        let v = view(&[(1, "a/b", "y", "z", Open), (2, "a/b", "main", "solo", Open), (3, "a/b", "main", "x", Open), (4, "a/b", "x", "y", Open)]);
        let order: Vec<u64> = v.in_train_order(v.prs.values()).iter().map(|p| p.id).collect();
        assert_eq!(order, [2, 3, 4, 1]);

        let mut v = v;
        v.prs.get_mut(&3).unwrap().state = Dismissed;
        let shown: Vec<u64> = v.in_train_order(v.prs.values().filter(|p| p.state == Open)).iter().map(|p| p.id).collect();
        assert_eq!(shown, [2, 4, 1], "a train whose bottom was set aside stays in its place, in order");
        assert_eq!(v.stack_place(&v.prs[&1]).position, 2);
    }

    #[test]
    fn merging_a_card_moves_the_ones_right_above_onto_its_base() {
        let cases: &[(&str, Vec<(u64, &str, &str, &str, PrState)>, u64, Vec<(u64, &str)>)] = &[
            ("nothing above", vec![(1, "a/b", "main", "x", Open)], 1, vec![]),
            ("one above", vec![(1, "a/b", "main", "x", Open), (2, "a/b", "x", "y", Open)], 1, vec![(2, "main")]),
            ("only the next one of three", vec![(1, "a/b", "main", "x", Open), (2, "a/b", "x", "y", Open), (3, "a/b", "y", "z", Open)], 1, vec![(2, "main")]),
            ("the middle of three", vec![(1, "a/b", "main", "x", Open), (2, "a/b", "x", "y", Open), (3, "a/b", "y", "z", Open)], 2, vec![(3, "x")]),
            ("two on the same card", vec![(1, "a/b", "main", "x", Open), (2, "a/b", "x", "y", Open), (3, "a/b", "x", "z", Open)], 1, vec![(2, "main"), (3, "main")]),
            ("same branch in another repo", vec![(1, "a/b", "main", "x", Open), (2, "a/c", "x", "y", Open)], 1, vec![]),
            ("the one above was closed", vec![(1, "a/b", "main", "x", Open), (2, "a/b", "x", "y", Closed)], 1, vec![]),
            ("the one above was set aside", vec![(1, "a/b", "main", "x", Open), (2, "a/b", "x", "y", Dismissed)], 1, vec![(2, "main")]),
        ];
        for (what, cards, merged, expected) in cases {
            let v = view(cards);
            let plan = v.retarget_plan(*merged);
            assert_eq!(plan.iter().map(|r| (r.id, r.base.as_str())).collect::<Vec<_>>(), *expected, "{what}");
            assert!(plan.iter().all(|r| r.url == v.prs[&r.id].url), "{what}: url");
        }
    }
}
