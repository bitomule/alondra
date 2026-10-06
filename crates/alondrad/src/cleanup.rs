//! `alondra cleanup`: closes the sessions Alondra launched whose tasks ended before tasks closed
//! their own node, and the ones stopped without a task for more than a day. It looks only at
//! sessions Alondra launched and `claude agents` lists, and never closes one that works, waits for
//! a permission, has work that is not saved or that the user chose to leave open.
use crate::domain_runtime::DomainRuntime;
use crate::domain_github::execute_system;
use crate::node_close::{CloseOutcome, Sessions, close_node, listed};
use alondra_core::domain::{AgentId, Command, DomainState, NodeEnd, Task, TaskState};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::path::Path;

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
    let ended = |task: &&Task| task.state.terminal();
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
        Some(job) => job.updated_at,
        None => domain.live.node(&agent.0)?.state_since,
    };
    (now - stopped_since > STOPPED_FOR_MS).then_some((Why::StoppedWithoutTask, None))
}

/// What would be closed (`dry_run`) or what was closed, kept and failed.
pub(crate) async fn run(domain: &DomainRuntime, sessions: &dyn Sessions, claude_home: &Path, dry_run: bool, now: i64) -> Result<Value, (StatusCode, String)> {
    let listing = sessions.claude(vec!["agents".into(), "--json".into(), "--all".into()]).await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
    let state = domain.state().map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "the domain could not be read".to_owned()))?;
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
        let node = domain.live.node(&candidate.agent.0);
        if node.as_ref().is_some_and(|node| node.busy) {
            left.push(kept(&candidate, "busy", None));
            continue;
        }
        if node.as_ref().is_some_and(|node| node.waiting_permission) || crate::domain_holds::waits_on_user(&state, &candidate.agent.0) {
            left.push(kept(&candidate, "permission", None));
            continue;
        }
        if let Some(detail) = sessions.leftovers(&candidate.agent.0).await {
            left.push(kept(&candidate, "worktree", Some(&detail)));
            continue;
        }
        if dry_run {
            close.push(candidate.row());
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
                for task in state.tasks.values().filter(|task| task.agent_id.as_ref() == Some(&candidate.agent) && task.state.terminal() && task.wrap_up.node.is_none()) {
                    if let Err(error) = execute_system(domain, Command::RecordNodeEnd { task_id: task.id.clone(), end: NodeEnd::Closed }) {
                        log!("could not record that the node of {} was closed: {error:?}", task.id);
                    }
                }
                closed.push(candidate.row());
            }
            Ok(CloseOutcome::NotListed) => {}
            Ok(CloseOutcome::Refused { message, .. }) => failed.push(failure(&candidate, &message)),
            Ok(CloseOutcome::Failed { message, .. }) => failed.push(failure(&candidate, &message)),
            Err(error) => failed.push(failure(&candidate, &format!("{error:#}"))),
        }
    }
    Ok(json!({"dry_run": dry_run, "close": close, "closed": closed, "kept": left, "failed": failed}))
}

fn failure(candidate: &Candidate, error: &str) -> Value {
    json!({"session": candidate.agent.0, "short_id": candidate.short, "name": candidate.name, "error": error})
}

#[cfg(test)]
mod tests;
