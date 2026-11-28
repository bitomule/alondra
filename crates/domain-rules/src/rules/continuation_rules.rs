//! A task the orchestrator creates to continue another takes over its work in the same step: its
//! open worktrees, its checklist and its open pull requests.
use super::*;

/// Why `original` cannot be continued by a task of `request_id` now, if it cannot.
pub(crate) fn continuation_refusal(domain: &DomainState, original: &TaskId, request_id: &RequestId, plan_items: bool) -> Result<()> {
    let carried = domain.task(original)?;
    if carried.request_id != *request_id {
        return refuse("continuation_request", "request_id", &format!(
            "a task that carries on another goes in that task's request, so the request still counts its work: create it under request {}",
            carried.request_id.0));
    }
    if plan_items {
        return refuse("invalid_input", "plan_items", "a task that carries on another takes that task's whole plan: leave plan_items out");
    }
    if domain.lifecycle_of(original).is_ok_and(|lifecycle| lifecycle.phase() == alondra_domain::TaskPhase::Delivered) {
        return refuse("delivery_open", "continues", &format!(
            "task {original} delivered its work and it waits for the user's review: the user settles that delivery first, then a task may continue it"));
    }
    Ok(())
}

/// `task_id` continues `original`: it takes the original's open worktrees, its checklist and its
/// open pull requests, with their history.
pub(crate) fn take_over(domain: &mut DomainState, task_id: &TaskId, original: &TaskId) {
    let inherited: BTreeMap<String, TaskWorktree> = domain.tasks[original].open_worktrees()
        .map(|(path, worktree)| (path.clone(), TaskWorktree { origin: WorktreeOrigin::Inherited, branch: worktree.branch.clone(), discard: false, end: None }))
        .collect();
    domain.tasks.get_mut(task_id).expect("created above").worktrees = inherited;
    if let Some(checklist) = domain.checklists.remove(original) {
        domain.checklists.insert(task_id.clone(), checklist);
    }
    domain.hand_open_prs(original, task_id);
}
