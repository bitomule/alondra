//! What the cards of an ended Task keep. A card of a done or cancelled Task is shown as settled,
//! without its content, and the Task's detail lists only cards that wait, so its long parts are no
//! longer read. What is still read stays:
//! - the final result's excerpt, which a Task that builds on this one starts from;
//! - a card whose body is the plan another Task implements, and the cards of a plan review;
//! - a notice nobody read yet, which still waits for the user;
//! - Alondra's own cards, some of which it recognizes by their text;
//! - each card's need, why, plan, answer, options, attachments, targets and feedback.
//!
//! The rest of the body becomes its title line, the content's body goes, and options lose their
//! descriptions. A reopened Task keeps its compacted cards: none of them waits again.
use super::*;
use std::collections::BTreeSet;

/// How much of a Task's final result the Tasks that build on it read.
pub const RESULT_LIMIT: usize = 4_000;
const TITLE_LIMIT: usize = 120;

/// What a Task that builds on another reads of its final result.
pub fn result_excerpt(body: &str) -> String {
    let body = body.trim();
    if body.chars().count() <= RESULT_LIMIT {
        return body.to_owned();
    }
    format!("{}\n[shortened]", body.chars().take(RESULT_LIMIT).collect::<String>())
}

/// A card's title as the orchestrator reads it: its first line that is not blank.
pub fn body_title(body: &str) -> String {
    let line = body.lines().find(|line| !line.trim().is_empty()).unwrap_or_default().trim();
    let mut title: String = line.chars().take(TITLE_LIMIT).collect();
    if line.chars().count() > TITLE_LIMIT {
        title.push('…');
    }
    title
}

impl DomainState {
    /// Ended Tasks whose cards still hold something nobody reads, at most `limit`.
    pub fn compactable_tasks(&self, limit: usize) -> Vec<TaskId> {
        let kept = self.kept_whole();
        let mut tasks = BTreeSet::new();
        for i in self.interactions.values() {
            if tasks.len() == limit {
                break;
            }
            if let Some(task) = self.ended_task_of(i)
                && !tasks.contains(&task.id)
                && self.compacted(i, task, &kept).is_some()
            {
                tasks.insert(task.id.clone());
            }
        }
        tasks.into_iter().collect()
    }

    pub(super) fn compact_ended_tasks(&mut self, task_ids: &[TaskId]) -> Result<()> {
        if task_ids.is_empty() || task_ids.len() > 64 {
            return fail("invalid_input", "Compact between 1 and 64 tasks at a time");
        }
        for task_id in task_ids {
            if !self.tasks.get(task_id).is_some_and(|task| task.state.terminal()) {
                return fail("conflict", "Only the cards of a done or cancelled task are compacted");
            }
        }
        let kept = self.kept_whole();
        let compacted: Vec<Interaction> = self
            .interactions
            .values()
            .filter_map(|i| {
                let task = self.ended_task_of(i).filter(|task| task_ids.contains(&task.id))?;
                self.compacted(i, task, &kept)
            })
            .collect();
        for i in compacted {
            self.interactions.insert(i.id.clone(), i);
        }
        Ok(())
    }

    fn ended_task_of(&self, i: &Interaction) -> Option<&Task> {
        let Scope::Task { task_id } = &i.scope else { return None };
        self.tasks.get(task_id).filter(|task| task.state.terminal())
    }

    /// The cards read whole after their Task ended: a plan another Task implements, and the
    /// decisions a plan review reads.
    fn kept_whole(&self) -> BTreeSet<InteractionId> {
        let mut kept: BTreeSet<InteractionId> = self
            .checklists
            .iter()
            .filter(|(task_id, checklist)| {
                self.interactions.get(&checklist.plan).is_some_and(|plan| plan.scope != Scope::Task { task_id: (*task_id).clone() })
            })
            .map(|(_, checklist)| checklist.plan.clone())
            .collect();
        kept.extend(self.plan_reviews.values().flat_map(|review| review.decisions.iter().cloned()));
        kept
    }

    /// The card as its ended Task keeps it, or nothing when it already is.
    fn compacted(&self, i: &Interaction, task: &Task, kept: &BTreeSet<InteractionId>) -> Option<Interaction> {
        let unread = matches!(&i.payload, InteractionPayload::Information { read_by } if read_by.is_empty()) && !i.withdrawn;
        if matches!(i.author, Author::System) || unread || kept.contains(&i.id) {
            return None;
        }
        let mut compact = i.clone();
        let body = if task.final_presentation.as_ref() == Some(&i.id) { result_excerpt(&i.body) } else { body_title(&i.body) };
        if !body.is_empty() || !i.attachments.is_empty() {
            compact.body = body;
        }
        if let Some(content) = &mut compact.card.content {
            content.body.clear();
        }
        if let InteractionPayload::Question { options, .. } | InteractionPayload::ActionRequest { options, .. } = &mut compact.payload {
            for option in options {
                option.description = None;
            }
        }
        (compact != *i).then_some(compact)
    }
}
