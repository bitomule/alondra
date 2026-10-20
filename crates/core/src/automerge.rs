//! Alondra merges the PRs of an auto-merge task itself, the way David does from the card, once
//! GitHub says the PR can go in: its checks passed, it merges cleanly, it is not a draft and
//! nothing under it is left to merge. The node waits for the merge notice without spending a turn
//! on the checks.

use crate::model::Millis;
use crate::pr::{CheckState, MergeMethod, PrState, PullRequest};
use crate::tasks::Task;
use crate::view::View;

/// Right after a PR is opened or pushed to, GitHub may not list the checks of its new head yet:
/// one with none is not merged before this long after Alondra first saw that head.
pub const NO_CHECKS_GRACE: Millis = 2 * 60 * 1000;

#[derive(Debug, PartialEq)]
pub enum AutoMerge {
    Ready(MergeMethod),
    /// Alondra waits for GitHub; the node has nothing to do.
    Waiting(&'static str),
    /// Only the node can move it on.
    Blocked(Obstacle),
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum Obstacle {
    ChecksFailed(Vec<String>),
    Conflicts,
    Draft,
}

impl View {
    /// Where an open PR Alondra merges on its own stands; none for a PR David merges.
    pub fn auto_merge(&self, pr: &PullRequest, now: Millis) -> Option<AutoMerge> {
        if !pr.auto_merge || pr.state != PrState::Open {
            return None;
        }
        let failed: Vec<String> = pr.checks.iter().filter(|c| c.state == CheckState::Failed).map(|c| c.name.clone()).collect();
        let below = self.merge_blocked_by(pr.id).is_some() || self.pr_task_of(pr).is_some_and(|t| self.task_guards(t).unmerged_dependency.is_some());
        Some(if !pr.fetched {
            AutoMerge::Waiting("GitHub has not been asked about it yet")
        } else if pr.draft {
            AutoMerge::Blocked(Obstacle::Draft)
        } else if pr.conflicts() {
            AutoMerge::Blocked(Obstacle::Conflicts)
        } else if !failed.is_empty() {
            AutoMerge::Blocked(Obstacle::ChecksFailed(failed))
        } else if pr.checks.iter().any(|c| c.state == CheckState::Running) {
            AutoMerge::Waiting("its checks are running")
        } else if pr.checks.is_empty() && now - pr.head_seen_at.unwrap_or(pr.opened_at).max(pr.opened_at) < NO_CHECKS_GRACE {
            AutoMerge::Waiting("its checks may not have started")
        } else if pr.mergeable.as_deref() != Some("MERGEABLE") {
            AutoMerge::Waiting("GitHub is working out whether it merges")
        } else if below {
            AutoMerge::Waiting("the PR under it is not merged yet")
        } else {
            AutoMerge::Ready(self.merge_method_for(pr))
        })
    }

    /// The method last used in the repo when the repo allows it, else the first it allows.
    pub fn merge_method_for(&self, pr: &PullRequest) -> MergeMethod {
        let allowed = |m: &MergeMethod| pr.merge_methods.is_empty() || pr.merge_methods.contains(m);
        self.merge_methods.get(&pr.repo).copied().filter(allowed).or_else(|| pr.merge_methods.first().copied()).unwrap_or(MergeMethod::Merge)
    }

    /// Alondra is waiting on GitHub to merge a PR of the task: the node is idle on purpose.
    pub fn auto_merge_pending(&self, task: &Task, now: Millis) -> bool {
        task.prs.iter().filter_map(|id| self.prs.get(id)).any(|p| matches!(self.auto_merge(p, now), Some(AutoMerge::Waiting(_) | AutoMerge::Ready(_))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;

    const MIN: Millis = 60 * 1000;

    /// The wait for a PR's first checks counted from when it was opened, so a push to a PR open
    /// for hours was merged before GitHub listed the checks of the new head.
    #[test]
    fn a_push_waits_for_the_checks_of_its_new_head() {
        let mut v = View::default();
        let mut pr = PullRequest::opened(1, "https://github.com/a/b/pull/7", "s", None, 0).unwrap();
        pr.auto_merge = true;
        v.prs.insert(1, pr);
        let fetched = |v: &mut View, seq: u64, head: &str, at: Millis| {
            let mut p = v.prs[&1].clone();
            (p.fetched, p.head_sha, p.mergeable) = (true, Some(head.into()), Some("MERGEABLE".into()));
            v.apply(seq, at, &Event::PrUpdated(p));
        };
        fetched(&mut v, 1, "a1", 10 * MIN);
        assert!(matches!(v.auto_merge(&v.prs[&1], 13 * MIN), Some(AutoMerge::Ready(_))));
        fetched(&mut v, 2, "a1", 60 * MIN);
        assert!(matches!(v.auto_merge(&v.prs[&1], 60 * MIN), Some(AutoMerge::Ready(_))), "the same head fetched again waits for nothing");
        fetched(&mut v, 3, "a2", 90 * MIN);
        assert_eq!(v.auto_merge(&v.prs[&1], 90 * MIN + MIN / 2), Some(AutoMerge::Waiting("its checks may not have started")));
        assert!(matches!(v.auto_merge(&v.prs[&1], 93 * MIN), Some(AutoMerge::Ready(_))));
    }
}
