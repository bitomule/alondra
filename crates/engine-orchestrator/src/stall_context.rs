//! What the orchestrator reads with the case of a stopped node: the task's mission, the node's
//! last words and its worktree, read when the case reaches it.
use crate::engine::OrchestratorOf as _;
use crate::domain_runtime::DomainRuntime;
use alondra_domain_model::{DomainState, Interaction, Scope, Task, WorktreeOrigin};
use std::path::Path;

const MISSION_SHOWN: usize = 4_000;
const WORDS_SHOWN: usize = 2_000;

/// The case's context, or nothing for a message that is not a case.
pub(crate) fn context(domain: &DomainRuntime, state: &DomainState, case: &Interaction) -> String {
    if !state.unsettled_stall(case) {
        return String::new();
    }
    let Scope::Task { task_id } = &case.scope else { return String::new() };
    let Some(task) = state.tasks.get(task_id) else { return String::new() };
    let plan = domain.launch_plan(task_id).ok().flatten();
    let mission = plan.as_ref().and_then(|plan| plan.mission.clone())
        .or_else(|| state.requests.get(&task.request_id).map(|request| request.body.clone()))
        .unwrap_or_default();
    let words = task.agent_id.as_ref()
        .and_then(|agent| domain.engine().last_words(agent))
        .filter(|text| !text.trim().is_empty());
    let worktree = own_worktree(task).or_else(|| plan.as_ref().map(|plan| plan.cwd.clone()));
    format!(
        "\nIts mission:\n{}\n\nIts last words:\n{}\n\nIts worktree:\n{}",
        shown(&mission, MISSION_SHOWN),
        words.map_or_else(|| "(none recorded)".to_owned(), |words| shown(&words, WORDS_SHOWN)),
        worktree.map_or_else(|| "(unknown)".to_owned(), |path| domain.ports().worktrees().summary(Path::new(&path))))
}

fn own_worktree(task: &Task) -> Option<String> {
    task.worktrees.iter()
        .find(|(_, worktree)| worktree.end.is_none() && matches!(worktree.origin, WorktreeOrigin::Task | WorktreeOrigin::Inherited))
        .map(|(path, _)| path.clone())
}

fn shown(text: &str, max: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}
