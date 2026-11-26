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

pub(crate) fn compact_ended_tasks(domain: &mut DomainState, task_ids: &[TaskId]) -> Result<()> {
    if task_ids.is_empty() || task_ids.len() > 64 {
        return fail("invalid_input", "Compact between 1 and 64 tasks at a time");
    }
    for task_id in task_ids {
        if !domain.tasks.get(task_id).is_some_and(|task| task.state.terminal()) {
            return fail("conflict", "Only the cards of a done or cancelled task are compacted");
        }
    }
    let kept = domain.kept_whole();
    let compacted: Vec<Interaction> = domain
        .interactions
        .values()
        .filter_map(|i| {
            let task = domain.ended_task_of(i).filter(|task| task_ids.contains(&task.id))?;
            domain.compacted(i, task, &kept)
        })
        .collect();
    for i in compacted {
        domain.interactions.insert(i.id.clone(), i);
    }
    Ok(())
}
