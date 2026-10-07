//! What a task asks of the user right now. The one place that decides whether a delivery is a card
//! in Waiting for You: the list, the badge and the push all read it.

use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A card and the revision it had when the user got something to do with it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CardRef {
    pub interaction: InteractionId,
    pub revision: Revision,
}

/// Why a task has no card yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitReason {
    /// An open task blocks it.
    BlockedBy { task: TaskId },
    /// It is stacked on a PR that is not merged.
    UpstreamPr { number: u64 },
    /// An open card of the task holds it back until the user answers.
    Answer { need: String },
    /// Its node is resolving the conflicts Alondra handed back.
    NodeFixing { node: String, pr: u64 },
    Merging { pr: u64 },
    Closing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attention {
    /// The user can do something about it now.
    Act(CardRef),
    /// The system or another task has it: shown as the task's state, never as a card.
    Wait(WaitReason),
    /// Working, nothing to show.
    Idle,
}

impl DomainState {
    /// The tasks that hold `task` back, while they are open.
    pub fn open_blockers(&self, task: &TaskId) -> Vec<TaskId> {
        self.tasks
            .values()
            .filter(|b| b.blocks.contains(task) && self.holds_back(b))
            .map(|b| b.id.clone())
            .collect()
    }
    /// The task blocks one that is still open: its delivery is a verdict for that task, never a
    /// card for the user.
    pub fn reviewing(&self, t: &Task) -> bool {
        t.blocks.iter().any(|b| self.tasks.get(b).is_some_and(|b| !b.state.terminal()))
    }
    /// The task still has a blocked task to review that is not over.
    pub fn reviewing_open(&self, t: &Task) -> bool {
        t.blocks.iter().any(|a| self.tasks.get(a).is_some_and(|a| !a.state.terminal() && !self.finished(a)))
    }
    /// A blocking task keeps holding back until its own delivery is settled.
    fn holds_back(&self, blocker: &Task) -> bool {
        !blocker.state.terminal() && !self.finished(blocker)
    }
    pub fn attention(&self, t: &Task) -> Attention {
        if t.state.terminal() {
            return Attention::Idle;
        }
        let Some(p) = t.final_presentation.as_ref().and_then(|p| self.interactions.get(p)) else {
            return Attention::Idle;
        };
        if self.settled(t, p) {
            return if self.wraps_up(t) { Attention::Wait(WaitReason::Closing) } else { Attention::Idle };
        }
        if self.accepted(p) {
            let open_pr = match &p.payload {
                InteractionPayload::Delivery { targets, .. } => targets.iter().find_map(|x| match x {
                    DeliveryTarget::PullRequest { pr_id, .. } => self.pull_requests.get(pr_id).filter(|pr| pr.state == PrState::Open),
                    _ => None,
                }),
                _ => None,
            };
            return Attention::Wait(match open_pr {
                Some(pr) => WaitReason::Merging { pr: pr.number },
                None => WaitReason::Closing,
            });
        }
        if let Some(instruction) = self.handed_back(p) {
            let pr = t.pr_id.as_ref().and_then(|id| self.pull_requests.get(id)).map_or(0, |pr| pr.number);
            let node = t.agent_id.as_ref().and_then(|id| self.agents.get(id)).map_or_else(|| t.title.clone(), |agent| agent.name.clone());
            return match instruction {
                Instruction::Conflicts => Attention::Wait(WaitReason::NodeFixing { node, pr }),
                _ => Attention::Idle,
            };
        }
        if !t.blocks.is_empty() || !self.open(p) {
            return Attention::Idle;
        }
        if let Some(blocker) = self.open_blockers(&t.id).into_iter().next() {
            return Attention::Wait(WaitReason::BlockedBy { task: blocker });
        }
        if let Some(card) = self.blocked_by(&p.id).first().and_then(|id| self.interactions.get(id)) {
            return Attention::Wait(WaitReason::Answer { need: card.card.need.clone() });
        }
        if let Some(stack) = &t.stack
            && let Some(upstream) = self.interactions.get(&stack.presentation_id)
            && self.open(upstream)
            && upstream.id != p.id
            && self.pull_requests.get(&stack.pr_id).is_none_or(|pr| pr.state == PrState::Open)
        {
            let number = self.pull_requests.get(&stack.pr_id).map_or(0, |pr| pr.number);
            return Attention::Wait(WaitReason::UpstreamPr { number });
        }
        Attention::Act(CardRef { interaction: p.id.clone(), revision: p.revision })
    }
    /// The cards the user can act on, by task.
    pub fn acting(&self) -> BTreeMap<TaskId, CardRef> {
        self.tasks
            .values()
            .filter_map(|t| match self.attention(t) {
                Attention::Act(card) => Some((t.id.clone(), card)),
                _ => None,
            })
            .collect()
    }
}
