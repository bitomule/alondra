//! The worktrees of cancelled tasks. One with no unsaved file goes with its local branch, unasked.
//! One with unsaved files is never touched without the user's word: a card asks whether to delete
//! them, and Alondra keeps no copy. Cancelled tasks already closed are settled once, when the
//! daemon starts, with a single card for all of them.
use std::collections::{HashMap, HashSet};
use std::path::Path;

use alondra_domain_rules::discard_worktrees::{CancelledWorktree, DISCARD_WORKTREES_PREFIX, DiscardWorktrees};
use alondra_domain_rules::{
    Author, Command, DomainState, InteractionId, InteractionPayload, NodeEnd, PrState, Scope, SystemCard, Task, TaskId, TaskState,
    WorktreeEnd,
};
use alondra_view_model::Lang;
use anyhow::{Result, anyhow};

use crate::locks::Poisonable as _;
use alondra_engine_ports::Runtime;
use alondra_engine_kernel::domain_runtime::RuntimeRecords as _;
use alondra_engine_ports::worktrees::TaskWorktree;

enum Settled {
    Gone(WorktreeEnd),
    Unsaved(usize),
    Unpushed(String),
}

/// A task with an open pull request keeps its local branch.
fn keeps_branch(state: &DomainState, task: &Task) -> bool {
    state.open_prs(task).next().is_some()
}

/// Deletes the worktree at `path` unless it holds unsaved files, which it only counts, or, with
/// `keep_unpushed`, commits no remote has.
async fn settle_path(domain: &dyn Runtime, task: &Task, path: &str, keep_unpushed: bool) -> Result<Settled> {
    crate::task_worktrees::ready(domain, path).await?;
    if let Some(why) = domain.ports().worktrees().unreachable_link(Path::new(path)) {
        return Err(crate::task_worktrees::Unreachable(why).into());
    }
    let unsaved = domain.ports().worktrees().uncommitted_files_of_alondras_worktree(Path::new(path)).await.map_err(|why| anyhow!(why))?.unwrap_or(0);
    if unsaved > 0 {
        return Ok(Settled::Unsaved(unsaved));
    }
    if keep_unpushed && let Some(what) = domain.ports().worktrees().unsaved(Path::new(path)).await.map_err(|why| anyhow!(why))? {
        return Ok(Settled::Unpushed(what));
    }
    let keeps_branch = keeps_branch(&*domain.state(), task);
    match domain.ports().worktrees().discard_worktree(Path::new(path), keeps_branch).await.map_err(|why| anyhow!(why))? {
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

fn raise_card(domain: &dyn Runtime, id: &str, scope: Scope, tasks: Vec<CancelledWorktree>) -> Result<()> {
    if domain.state().interactions.contains_key(&InteractionId(id.to_owned())) {
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
pub async fn release_cancelled(domain: &dyn Runtime, task: &Task) -> Result<()> {
    let task = &crate::task_worktrees::recover_noted(domain, task)?;
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
            match settle_path(domain, task, &path, false).await? {
                Settled::Gone(end) => end,
                Settled::Unsaved(files) => {
                    held.files += files;
                    held.paths.push(path.clone());
                    WorktreeEnd::KeptByUser
                }
                Settled::Unpushed(what) => {
                    log!("leaving the worktree {path} of the cancelled task {} as it is: {what}", task.id);
                    WorktreeEnd::KeptByUser
                }
            }
        };
        if recorded {
            ends.push((path, end));
        }
    }
    let state = domain.state();
    let asked = still_asked(&state);
    held.paths.retain(|path| !asked.contains(path));
    if !held.paths.is_empty() {
        let first = format!("{DISCARD_WORKTREES_PREFIX}{}", task.id);
        let id = match state.interactions.contains_key(&InteractionId(first.clone())) {
            true => format!("{first}-{}", state.revision),
            false => first,
        };
        drop(state);
        raise_card(domain, &id, Scope::Orchestrator { request_id: Some(task.request_id.clone()) }, vec![held])?;
    }
    for (path, end) in ends {
        domain.execute_system_command(Command::ReleaseTaskWorktree { task_id: task.id.clone(), path, end }).map_err(|error| anyhow!("{error}"))?;
    }
    Ok(())
}

fn covering_cards(state: &DomainState) -> impl Iterator<Item = (&alondra_domain_rules::Interaction, DiscardWorktrees)> {
    state.interactions.values()
        .filter(|card| card.author == Author::System && card.system_card == Some(SystemCard::DiscardWorktrees) && !card.withdrawn)
        .filter_map(|card| Some((card, DiscardWorktrees::from_body(&card.body)?)))
}

/// The worktrees a card, open or answered, already asked the user about.
fn asked_paths(state: &DomainState) -> HashSet<String> {
    covering_cards(state).flat_map(|(_, listed)| listed.tasks.into_iter().flat_map(|entry| entry.paths)).collect()
}

/// For each worktree of each task, the newest card that asked about it, and whether its answer
/// deletes it: `None` while it waits for the user. An older card is a decision about an earlier
/// cancellation of the task, never about the work of a later one.
fn decisions(state: &DomainState) -> HashMap<(TaskId, String), Option<bool>> {
    newest_decisions(state).into_iter().map(|(key, (_, deletes))| (key, deletes)).collect()
}

fn newest_decisions(state: &DomainState) -> HashMap<(TaskId, String), (u64, Option<bool>)> {
    let mut newest: HashMap<(TaskId, String), (u64, Option<bool>)> = HashMap::new();
    for (card, listed) in covering_cards(state) {
        let answer = match &card.payload {
            InteractionPayload::Question { answer: Some(answer), .. } => Some(listed.deleted_by(&answer.option_id, answer.form_values())),
            _ => None,
        };
        for entry in &listed.tasks {
            let deletes = answer.as_ref().map(|deleted| deleted.iter().any(|chosen| chosen.task_id == entry.task_id));
            for path in &entry.paths {
                let key = (TaskId(entry.task_id.clone()), path.clone());
                if newest.get(&key).is_none_or(|(order, _)| *order < card.order) {
                    newest.insert(key, (card.order, deletes));
                }
            }
        }
    }
    newest
}

/// The worktrees a card still decides, whichever task it listed them under: the newest card about
/// each one waits for the user, or kept it. One the newest card deleted was settled, so cancelling
/// a task that works there again asks anew.
fn still_asked(state: &DomainState) -> HashSet<String> {
    let mut newest: HashMap<String, (u64, Option<bool>)> = HashMap::new();
    for ((_, path), (order, deletes)) in newest_decisions(state) {
        if newest.get(&path).is_none_or(|(seen, _)| *seen < order) {
            newest.insert(path, (order, deletes));
        }
    }
    newest.into_iter().filter(|(_, (_, deletes))| *deletes != Some(true)).map(|(path, _)| path).collect()
}

/// Deletes the worktrees the user chose to delete, each once: only while its task is still
/// cancelled, the newest card about it is the one that said delete, and its record still says
/// the user kept it, or there is none. Deleting records it as removed, also a launch folder the
/// task never recorded, so no later pass applies the answer again.
/// Keep needs nothing: the answered card is what stops anyone from asking again.
/// A worktree already gone is nothing to do.
pub async fn apply_answers(domain: &dyn Runtime) -> Result<()> {
    let state = domain.state();
    let mut chosen: Vec<(TaskId, String)> = decisions(&state).into_iter().filter(|(_, deletes)| *deletes == Some(true)).map(|(key, _)| key).collect();
    chosen.sort();
    for (task_id, path) in chosen {
        let Some(task) = state.tasks.get(&task_id).filter(|task| task.state == TaskState::Cancelled) else { continue };
        let recorded = task.worktrees.get(&path).map(|worktree| worktree.end);
        if !matches!(recorded, None | Some(Some(WorktreeEnd::KeptByUser))) || crate::task_worktrees::in_use(domain, &state, &task_id, &path) {
            continue;
        }
        if crate::task_worktrees::ready(domain, &path).await.is_err() || crate::task_worktrees::may_handle(domain, &path).is_err() { continue; }
        match domain.ports().worktrees().discard_worktree(Path::new(&path), keeps_branch(&state, task)).await {
            Ok(TaskWorktree::Removed { freed }) => log!("deleting the worktree {path} of {task_id}, as the user asked, freed {}",
                alondra_core::machine_incident::bytes_text(freed, Lang::En)),
            Ok(TaskWorktree::Absent) => {}
            Ok(_) => continue,
            Err(why) => {
                log!("could not delete the worktree {path} of {task_id}, as the user asked: {why}");
                continue;
            }
        }
        domain.execute_system_command(Command::ReleaseTaskWorktree { task_id: task_id.clone(), path, end: WorktreeEnd::Removed }).map_err(|error| anyhow!("{error}"))?;
    }
    Ok(())
}

/// The folders some task's record says the user keeps: never swept, whichever task left them.
fn kept_by_user(state: &DomainState) -> HashSet<&str> {
    state.tasks.values()
        .flat_map(|task| task.worktrees.iter())
        .filter(|(_, worktree)| matches!(worktree.end, Some(WorktreeEnd::KeptByUser | WorktreeEnd::UsersOwn)))
        .map(|(path, _)| path.as_str())
        .collect()
}

/// The worktrees cancelled tasks left behind before this rule: the clean ones go, and one card
/// lists the tasks whose worktrees hold unsaved files that no earlier card asked about. A folder
/// whose record ended, that any task's record says the user keeps, or with commits no remote has
/// is left as it is.
pub async fn sweep(domain: &dyn Runtime) -> Result<()> {
    let state = domain.state();
    let asked = asked_paths(&state);
    let kept = kept_by_user(&state);
    let stored: std::collections::BTreeMap<TaskId, String> = domain.store().lock_or_exit("the domain store").task_worktrees()?.into_iter().collect();
    let mut unsaved = vec![];
    for task in state.tasks.values().filter(|task| task.state == TaskState::Cancelled) {
        if matches!(task.wrap_up.node, Some(NodeEnd::KeptByUser | NodeEnd::UsersOwn)) {
            continue;
        }
        let mut paths: Vec<&String> = stored.get(&task.id).into_iter().chain(task.worktrees.keys())
            .filter(|path| task.worktrees.get(*path).is_none_or(|worktree| worktree.end.is_none()) && !kept.contains(path.as_str()))
            .collect();
        paths.sort();
        paths.dedup();
        let mut held = CancelledWorktree { task_id: task.id.0.clone(), title: task.title.clone(), files: 0, paths: vec![] };
        for path in paths.into_iter().filter(|path| domain.ports().worktrees().exists(Path::new(path.as_str())) && !crate::task_worktrees::in_use(domain, &state, &task.id, path)) {
            if asked.contains(path.as_str()) {
                continue;
            }
            match settle_path(domain, task, path, true).await {
                Ok(Settled::Unsaved(files)) => {
                    held.files += files;
                    held.paths.push(path.clone());
                }
                Ok(Settled::Unpushed(what)) => log!("leaving the worktree {path} of the cancelled task {} as it is: {what}", task.id),
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
