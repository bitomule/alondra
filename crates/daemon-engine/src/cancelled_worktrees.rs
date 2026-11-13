//! The worktrees of cancelled tasks. One with no unsaved file goes with its local branch, unasked.
//! One with unsaved files is never touched without the user's word: a card asks whether to delete
//! them, and Alondra keeps no copy. Cancelled tasks already closed are settled once, when the
//! daemon starts, with a single card for all of them.
use std::collections::HashSet;
use std::path::Path;

use alondra_core::discard_worktrees::{CancelledWorktree, DISCARD_WORKTREES_PREFIX, DiscardWorktrees};
use alondra_core::domain::{
    Author, Command, DomainState, InteractionId, InteractionPayload, NodeEnd, PrState, Scope, SystemCard, Task, TaskId, TaskState,
    WorktreeEnd,
};
use alondra_core::views::Lang;
use anyhow::{Result, anyhow};

use crate::domain_github::execute_system;
use crate::locks::Poisonable as _;
use crate::domain_runtime::DomainRuntime;
use crate::worktree::TaskWorktree;

enum Settled {
    Gone(WorktreeEnd),
    Unsaved(usize),
}

/// A task with an open pull request keeps its local branch.
fn keeps_branch(state: &DomainState, task: &Task) -> bool {
    state.open_prs(task).next().is_some()
}

/// Deletes the worktree at `path` unless it holds unsaved files, which it only counts.
async fn settle_path(domain: &DomainRuntime, task: &Task, path: &str) -> Result<Settled> {
    crate::task_worktrees::ready(domain, path).await?;
    if let Some(why) = crate::worktree::unreachable_link(Path::new(path)) {
        return Err(crate::task_worktrees::Unreachable(why).into());
    }
    let unsaved = crate::worktree::uncommitted_files_of_alondras_worktree(Path::new(path)).await.map_err(|why| anyhow!(why))?.unwrap_or(0);
    if unsaved > 0 {
        return Ok(Settled::Unsaved(unsaved));
    }
    let keeps_branch = keeps_branch(&*domain.state()?, task);
    match crate::worktree::discard_worktree(Path::new(path), keeps_branch).await.map_err(|why| anyhow!(why))? {
        TaskWorktree::Removed { freed } => {
            log!("deleting the worktree {path} of the cancelled task {} freed {}", task.id, alondra_core::machine_incident::bytes_text(freed, Lang::En));
            Ok(Settled::Gone(WorktreeEnd::Removed))
        }
        TaskWorktree::Absent => Ok(Settled::Gone(WorktreeEnd::Removed)),
        TaskWorktree::NotAlondras => Ok(Settled::Gone(WorktreeEnd::UsersOwn)),
        TaskWorktree::Unsaved(left) => Err(anyhow!("the worktree {path} still holds work that is not saved: {left}")),
        TaskWorktree::Unavailable(why) => Err(crate::task_worktrees::Unreachable(why).into()),
    }
}

fn raise_card(domain: &DomainRuntime, id: &str, scope: Scope, tasks: Vec<CancelledWorktree>) -> Result<()> {
    if domain.state()?.interactions.contains_key(&InteractionId(id.to_owned())) {
        return Ok(());
    }
    let card = DiscardWorktrees { tasks };
    let (need, why) = card.text(Lang::En);
    let (options, recommended) = card.options();
    domain.raise_worded_problem_with_form(scope, id, Some(SystemCard::DiscardWorktrees), &format!("discard-worktrees:{id}"), need, why, card.to_body(), options, Some(recommended), card.form(Lang::En))
        .map_err(|error| anyhow!("could not ask whether to delete the unsaved changes of cancelled worktrees: {error:?}"))?;
    Ok(())
}

/// Settles every worktree of a task that is being cancelled: the ones without unsaved files are
/// deleted, the ones with them are left as they are and one card asks the user about them.
pub(crate) async fn release_cancelled(domain: &DomainRuntime, task: &Task) -> Result<()> {
    let mut paths: Vec<(String, bool)> = task.open_worktrees().map(|(path, _)| (path.clone(), true)).collect();
    paths.extend(domain.task_worktree(&task.id)?.filter(|dir| !task.worktrees.contains_key(dir)).map(|dir| (dir, false)));
    let mut held = CancelledWorktree { task_id: task.id.0.clone(), title: task.title.clone(), files: 0, paths: vec![] };
    let mut ends = vec![];
    for (path, recorded) in paths {
        crate::task_worktrees::ready(domain, &path).await?;
        let end = if let Err(why) = crate::task_worktrees::may_handle(domain, &path) {
            log!("leaving {path} of the cancelled task {} as it is: {why}", task.id);
            WorktreeEnd::UsersOwn
        } else if crate::task_worktrees::heir(domain, task, &path)?.is_some() {
            WorktreeEnd::HandedOver
        } else {
            match settle_path(domain, task, &path).await? {
                Settled::Gone(end) => end,
                Settled::Unsaved(files) => {
                    held.files += files;
                    held.paths.push(path.clone());
                    WorktreeEnd::KeptByUser
                }
            }
        };
        if recorded {
            ends.push((path, end));
        }
    }
    let asked = asked_paths(&*domain.state()?);
    held.paths.retain(|path| !asked.contains(path));
    if !held.paths.is_empty() {
        let id = format!("{DISCARD_WORKTREES_PREFIX}{}", task.id);
        raise_card(domain, &id, Scope::Orchestrator { request_id: Some(task.request_id.clone()) }, vec![held])?;
    }
    for (path, end) in ends {
        execute_system(domain, Command::ReleaseTaskWorktree { task_id: task.id.clone(), path, end }).map_err(|error| anyhow!("{error}"))?;
    }
    Ok(())
}

fn covering_cards(state: &DomainState) -> impl Iterator<Item = (&alondra_core::domain::Interaction, DiscardWorktrees)> {
    state.interactions.values()
        .filter(|card| card.author == Author::System && card.system_card == Some(SystemCard::DiscardWorktrees) && !card.withdrawn)
        .filter_map(|card| Some((card, DiscardWorktrees::from_body(&card.body)?)))
}

/// The worktrees a card, open or answered, already asked the user about.
fn asked_paths(state: &DomainState) -> HashSet<String> {
    covering_cards(state).flat_map(|(_, listed)| listed.tasks.into_iter().flat_map(|entry| entry.paths)).collect()
}

/// Whether another task that has not ended works in `path`, or will start there.
fn in_use(domain: &DomainRuntime, state: &DomainState, task: &TaskId, path: &str) -> bool {
    state.worktree_held_by_other(path, task).is_some()
        || state.tasks.values().filter(|other| &other.id != task && !other.over())
            .any(|other| domain.launch_plan(&other.id).ok().flatten().is_some_and(|plan| Path::new(&plan.cwd) == Path::new(path)))
}

/// Deletes the worktrees the user chose to delete on each answered card: all of them, or only
/// the ones ticked. Keep needs nothing: the answered card is what stops anyone from asking again.
/// A worktree already gone is nothing to do.
pub(crate) async fn apply_answers(domain: &DomainRuntime) -> Result<()> {
    let state = domain.state()?;
    for (card, listed) in covering_cards(&state) {
        let InteractionPayload::Question { answer: Some(answer), .. } = &card.payload else { continue };
        for entry in listed.deleted_by(&answer.option_id, answer.form_values()) {
            let task_id = TaskId(entry.task_id.clone());
            let keeps_branch = state.tasks.get(&task_id).is_some_and(|task| keeps_branch(&state, task));
            for path in entry.paths.iter().filter(|path| !in_use(domain, &state, &task_id, path)) {
                if crate::task_worktrees::ready(domain, path).await.is_err() || crate::task_worktrees::may_handle(domain, path).is_err() { continue; }
                match crate::worktree::discard_worktree(Path::new(path), keeps_branch).await {
                    Ok(TaskWorktree::Removed { freed }) => log!("deleting the worktree {path} of {task_id}, as the user asked, freed {}",
                        alondra_core::machine_incident::bytes_text(freed, Lang::En)),
                    Ok(_) => {}
                    Err(why) => log!("could not delete the worktree {path} of {task_id}, as the user asked: {why}"),
                }
            }
        }
    }
    Ok(())
}

/// The worktrees cancelled tasks left behind before this rule: the clean ones go, and one card
/// lists the tasks whose worktrees hold unsaved files that no earlier card asked about.
pub(crate) async fn sweep(domain: &DomainRuntime) -> Result<()> {
    let state = domain.state()?;
    let asked = asked_paths(&state);
    let stored: std::collections::BTreeMap<TaskId, String> = domain.store.lock_or_exit("the domain store").task_worktrees()?.into_iter().collect();
    let mut unsaved = vec![];
    for task in state.tasks.values().filter(|task| task.state == TaskState::Cancelled) {
        if matches!(task.wrap_up.node, Some(NodeEnd::KeptByUser | NodeEnd::UsersOwn)) {
            continue;
        }
        let mut paths: Vec<&String> = stored.get(&task.id).into_iter()
            .chain(task.worktrees.iter().filter(|(_, worktree)| !matches!(worktree.end, Some(WorktreeEnd::KeptByUser | WorktreeEnd::UsersOwn | WorktreeEnd::HandedOver))).map(|(path, _)| path))
            .collect();
        paths.sort();
        paths.dedup();
        let mut held = CancelledWorktree { task_id: task.id.0.clone(), title: task.title.clone(), files: 0, paths: vec![] };
        for path in paths.into_iter().filter(|path| Path::new(path.as_str()).exists() && !in_use(domain, &state, &task.id, path)) {
            if asked.contains(path.as_str()) {
                continue;
            }
            match settle_path(domain, task, path).await {
                Ok(Settled::Unsaved(files)) => {
                    held.files += files;
                    held.paths.push(path.clone());
                }
                Ok(Settled::Gone(_)) => {}
                Err(error) => log!("could not settle the worktree {path} of the cancelled task {}: {error:#}", task.id),
            }
        }
        if held.files > 0 {
            unsaved.push(held);
        }
    }
    if unsaved.is_empty() {
        return Ok(());
    }
    let id = format!("{DISCARD_WORKTREES_PREFIX}startup-{}", crate::auth::random_hex(6));
    raise_card(domain, &id, Scope::Orchestrator { request_id: None }, unsaved)
}
