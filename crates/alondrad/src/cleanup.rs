//! `alondra cleanup`: closes the sessions Alondra launched whose tasks ended before tasks closed
//! their own node, and the ones stopped without a task for more than a day; a session stopped in
//! Claude agents while its task was open frees that task for a relaunch. It looks only at sessions
//! Alondra launched, and closes only what `claude agents` lists. One that still works after its task
//! ended has its turn cut first; a task whose node declared work after the merge has not ended, so
//! that node is never touched. It never closes one that waits for a permission, has work that is
//! not saved or that the user chose to leave open. It also deletes the worktrees Alondra made for
//! tasks that ended and whose session is gone, unless they hold work not saved.
use crate::domain_runtime::DomainRuntime;
use crate::domain_github::execute_system;
use crate::node_close::{CloseOutcome, Sessions, close_node, listed};
use crate::worktree::TaskWorktree;
use alondra_core::domain::{AgentId, Command, DomainState, NodeEnd, Task, TaskState};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

const STOPPED_FOR_MS: i64 = 24 * 60 * 60 * 1_000;

/// Why a session is closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Why {
    TaskDone,
    TaskCancelled,
    StoppedWithoutTask,
}

impl Why {
    fn as_str(self) -> &'static str {
        match self {
            Self::TaskDone => "task_done",
            Self::TaskCancelled => "task_cancelled",
            Self::StoppedWithoutTask => "stopped_without_task",
        }
    }
}

struct Candidate {
    agent: AgentId,
    short: String,
    name: String,
    why: Why,
    title: Option<String>,
}

impl Candidate {
    fn row(&self) -> Value {
        json!({"session": self.agent.0, "short_id": self.short, "name": self.name, "why": self.why.as_str(), "task": self.title})
    }
}

fn kept(candidate: &Candidate, reason: &str, detail: Option<&str>) -> Value {
    json!({"session": candidate.agent.0, "short_id": candidate.short, "name": candidate.name, "reason": reason, "detail": detail})
}

fn why(state: &DomainState, domain: &DomainRuntime, claude_home: &Path, agent: &AgentId, now: i64) -> Option<(Why, Option<String>)> {
    let tasks: Vec<&Task> = state.tasks.values().filter(|task| task.agent_id.as_ref() == Some(agent)).collect();
    let ended = |task: &&Task| task.over();
    if !tasks.is_empty() {
        if !tasks.iter().all(ended) || tasks.iter().any(|task| task.wrap_up.node == Some(NodeEnd::KeptByUser)) {
            return None;
        }
        let title = tasks.first().map(|task| task.title.clone());
        let any_done = tasks.iter().any(|task| task.state == TaskState::Done);
        return Some((if any_done { Why::TaskDone } else { Why::TaskCancelled }, title));
    }
    let stopped_since = match crate::job_state::locate(claude_home, &agent.0).and_then(|dir| crate::job_state::read_in(&dir, &agent.0)) {
        Some(job) if matches!(job.phase, crate::job_state::Phase::Working | crate::job_state::Phase::Blocked) => return None,
        Some(job) if job.worktree.as_deref().is_some_and(|dir| worked_in(state, domain, agent, dir)) => return None,
        Some(job) => job.updated_at,
        None => domain.live.node(&agent.0)?.state_since,
    };
    (now - stopped_since > STOPPED_FOR_MS).then_some((Why::StoppedWithoutTask, None))
}

/// Whether another node with an open task works in `dir`: the node that replaced this one keeps
/// its worktree, which `claude rm` of this one would delete.
fn worked_in(state: &DomainState, domain: &DomainRuntime, agent: &AgentId, dir: &Path) -> bool {
    state.tasks.values().filter(|task| !task.over()).filter_map(|task| task.agent_id.as_ref()).filter(|other| *other != agent)
        .filter_map(|other| domain.live.mods.session(&other.0)?.cwd)
        .any(|cwd| Path::new(&cwd).starts_with(dir))
}

/// How long a session whose turn was cut gets to stop before it is left as working.
pub(crate) const STOP_WAIT: Duration = Duration::from_secs(20);

/// Cuts the session's turn and waits up to `wait` for it to stop; true once it no longer works.
async fn stop_turn(domain: &DomainRuntime, agent: &AgentId, wait: Duration) -> bool {
    let _ = domain.live.pushes.interrupt(&agent.0);
    let until = tokio::time::Instant::now() + wait;
    loop {
        if !domain.live.node(&agent.0).is_some_and(|node| node.busy) {
            return true;
        }
        if tokio::time::Instant::now() >= until {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100).min(wait)).await;
    }
}

/// Sessions Alondra launched that were stopped or deleted in Claude agents while their task was
/// open: their tasks go back to not started, waiting for a relaunch.
fn free_stopped(domain: &DomainRuntime, state: &DomainState, claude_home: &Path, dry_run: bool) -> Vec<Value> {
    let mut rows = vec![];
    for known in state.agents.values() {
        let session = known.id.0.as_str();
        let node = domain.live.node(session);
        if node.as_ref().is_some_and(|node| node.is_orchestrator) || !domain.launched_by_alondra(&known.id).unwrap_or(false)
            || !crate::node_revival::has_open_task(domain, session) || domain.live.mods.connected(session)
            || !crate::node_revival::session_ended_in(domain, claude_home, session)
        {
            continue;
        }
        let short: String = node.as_ref().map_or_else(|| session.chars().take(8).collect(), |node| node.short_id.clone());
        let name = node.and_then(|node| node.name).unwrap_or_else(|| known.name.clone());
        let titles: Vec<String> = state.tasks.values()
            .filter(|task| task.agent_id.as_ref() == Some(&known.id) && !task.over() && !task.archived)
            .map(|task| task.title.clone()).collect();
        if !dry_run && let Err(error) = crate::node_revival::user_closed(domain, session) {
            log!("could not free the tasks of {session}, stopped in Claude agents: {error:#}");
            continue;
        }
        rows.push(json!({"session": session, "short_id": short, "name": name, "tasks": titles}));
    }
    rows.sort_by(|a, b| a["session"].as_str().cmp(&b["session"].as_str()));
    rows
}

/// What would be closed (`dry_run`) or what was closed, kept and failed.
pub(crate) async fn run(
    domain: &DomainRuntime, sessions: &dyn Sessions, claude_home: &Path, dry_run: bool, now: i64, stop_wait: Duration,
) -> Result<Value, (StatusCode, String)> {
    let listing = sessions.claude(vec!["agents".into(), "--json".into(), "--all".into()]).await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
    let state = domain.state().map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "the domain could not be read".to_owned()))?;
    let freed = free_stopped(domain, &state, claude_home, dry_run);
    let mut candidates = vec![];
    for known in state.agents.values() {
        let node = domain.live.node(&known.id.0);
        let short: String = node.as_ref().map_or_else(|| known.id.0.chars().take(8).collect(), |node| node.short_id.clone());
        if node.as_ref().is_some_and(|node| node.is_orchestrator) || !listed(&listing, &short) || !domain.launched_by_alondra(&known.id).unwrap_or(false) {
            continue;
        }
        let Some((why, title)) = why(&state, domain, claude_home, &known.id, now) else { continue };
        let name = node.and_then(|node| node.name).unwrap_or_else(|| known.name.clone());
        candidates.push(Candidate { agent: known.id.clone(), short, name, why, title });
    }
    candidates.sort_by(|a, b| a.agent.0.cmp(&b.agent.0));
    let (mut close, mut closed, mut left, mut failed) = (vec![], vec![], vec![], vec![]);
    for candidate in candidates {
        let mut node = domain.live.node(&candidate.agent.0);
        let mut stuck = false;
        if node.as_ref().is_some_and(|node| node.busy) {
            if !dry_run && !stop_turn(domain, &candidate.agent, stop_wait).await {
                left.push(kept(&candidate, "busy", None));
                continue;
            }
            stuck = true;
            node = domain.live.node(&candidate.agent.0);
        }
        if node.as_ref().is_some_and(|node| node.waiting_permission) || crate::domain_holds::waits_on_user(&state, &candidate.agent.0) {
            left.push(kept(&candidate, "permission", None));
            continue;
        }
        if let Some(detail) = sessions.leftovers(&candidate.agent.0).await {
            left.push(kept(&candidate, "worktree", Some(&detail)));
            continue;
        }
        let mut row = candidate.row();
        row["stuck"] = json!(stuck);
        if dry_run {
            close.push(row);
            continue;
        }
        if !domain.live.begin_watch(&candidate.agent.0) {
            left.push(kept(&candidate, "busy", None));
            continue;
        }
        let outcome = close_node(domain, sessions, &candidate.agent, false).await;
        domain.live.end_watch(&candidate.agent.0);
        match outcome {
            Ok(CloseOutcome::Closed { .. }) => {
                for task in state.tasks.values().filter(|task| task.agent_id.as_ref() == Some(&candidate.agent) && task.over() && task.wrap_up.node.is_none()) {
                    if let Err(error) = execute_system(domain, Command::RecordNodeEnd { task_id: task.id.clone(), end: NodeEnd::Closed, agent_id: Some(candidate.agent.clone()) }) {
                        log!("could not record that the node of {} was closed: {error:?}", task.id);
                    }
                }
                closed.push(row);
            }
            Ok(CloseOutcome::NotListed) => {}
            Ok(CloseOutcome::Refused { message, .. }) => failed.push(failure(&candidate, &message)),
            Ok(CloseOutcome::Failed { message, .. }) => failed.push(failure(&candidate, &message)),
            Err(error) => failed.push(failure(&candidate, &format!("{error:#}"))),
        }
    }
    let ended: Vec<&str> = if dry_run { &close } else { &closed }.iter().filter_map(|row| row["short_id"].as_str()).collect();
    let (worktrees, kept_worktrees) = closed_task_worktrees(domain, &state, &listing, &ended, dry_run).await;
    let (free, freed) = if dry_run { (freed, vec![]) } else { (vec![], freed) };
    let (worktrees, removed_worktrees) = if dry_run { (worktrees, vec![]) } else { (vec![], worktrees) };
    Ok(json!({"dry_run": dry_run, "free": free, "freed": freed, "close": close, "closed": closed, "kept": left, "failed": failed,
        "worktrees": worktrees, "removed_worktrees": removed_worktrees, "kept_worktrees": kept_worktrees}))
}

/// The worktrees Alondra made for tasks that ended, which `claude rm` leaves because Claude Code
/// did not make them: deleted (or listed, on a dry run) once the task's session is gone. Never one
/// whose node the user kept or is the user's own, nor one a node of an open task works in; one with
/// work not saved is kept and said why.
async fn closed_task_worktrees(
    domain: &DomainRuntime, state: &DomainState, listing: &str, ended: &[&str], dry_run: bool,
) -> (Vec<Value>, Vec<Value>) {
    let recorded = match domain.store.lock().expect("domain store poisoned").task_worktrees() {
        Ok(recorded) => recorded,
        Err(error) => {
            log!("could not read the worktrees of tasks: {error:#}");
            return (vec![], vec![]);
        }
    };
    let (mut released, mut kept) = (vec![], vec![]);
    for (task_id, dir) in recorded {
        let Some(task) = state.tasks.get(&task_id) else { continue };
        if !task.over() || matches!(task.wrap_up.node, Some(NodeEnd::KeptByUser | NodeEnd::UsersOwn)) {
            continue;
        }
        if let Some(agent) = &task.agent_id {
            let short: String = domain.live.node(&agent.0).map_or_else(|| agent.0.chars().take(8).collect(), |node| node.short_id);
            if !domain.launched_by_alondra(agent).unwrap_or(false) || (listed(listing, &short) && !ended.contains(&short.as_str())) {
                continue;
            }
        }
        if open_task_works_in(state, domain, Path::new(&dir)) {
            continue;
        }
        let row = json!({"task_id": task_id.0, "task": task.title, "dir": dir});
        match crate::worktree::release_task_worktree(Path::new(&dir), &crate::launch_worktree::slug(&task_id.0), !dry_run).await {
            Ok(TaskWorktree::Removed) => released.push(row),
            Ok(TaskWorktree::Absent | TaskWorktree::NotAlondras) => {}
            Ok(TaskWorktree::Unsaved(left)) => kept.push(json!({"task_id": task_id.0, "task": task.title, "dir": dir, "detail": left})),
            Err(why) => kept.push(json!({"task_id": task_id.0, "task": task.title, "dir": dir, "detail": why})),
        }
    }
    (released, kept)
}

fn open_task_works_in(state: &DomainState, domain: &DomainRuntime, dir: &Path) -> bool {
    state.tasks.values().filter(|task| !task.over()).filter_map(|task| task.agent_id.as_ref())
        .filter_map(|agent| domain.live.mods.session(&agent.0)?.cwd)
        .any(|cwd| Path::new(&cwd).starts_with(dir))
}

fn failure(candidate: &Candidate, error: &str) -> Value {
    json!({"session": candidate.agent.0, "short_id": candidate.short, "name": candidate.name, "error": error})
}

#[cfg(test)]
mod tests;
