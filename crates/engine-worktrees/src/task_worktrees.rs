//! Every git worktree a task made or took, its node's and those of the subagents its node
//! launched: recorded from what Claude Code says it made, and settled when the task closes.
use std::path::Path;

use alondra_domain_rules::{AgentId, Command, DomainState, Task, TaskId, WorktreeEnd, WorktreeOrigin};
use alondra_engine_ports::worktrees::{Conversations, TaskWorktree};
use anyhow::{Context, Result};
use serde_json::Value;

use alondra_engine_ports::Runtime;
use alondra_engine_kernel::domain_runtime::RuntimeRecords as _;
use crate::locks::Poisonable as _;

/// Mounts a recorded worktree's volume from metadata kept on the daemon's own disk.
pub async fn ready(domain: &dyn Runtime, path: &str) -> Result<()> {
    let storage = domain.store().lock_or_exit("the domain store").worktree_storage_at(path)?;
    if let Some((path, storage)) = storage {
        let worktrees = domain.ports().worktrees();
        alondra_core::clock::spawn_blocking(move || worktrees.restore(&path, &storage))
            .await?.map_err(Unreachable)?;
    }
    Ok(())
}

/// A worktree a tool result says Claude Code made.
#[derive(Debug, PartialEq)]
pub enum Made {
    At { path: String, branch: Option<String>, subagent: bool },
    /// A subagent with an isolated worktree that went on in the background: Claude Code names
    /// its worktree after the subagent's id.
    Subagent { agent: String },
}

/// What a `PostToolUse` says about a worktree: EnterWorktree's and a kept isolated subagent's
/// `worktreePath`, or the id of an isolated subagent that went on in the background. ExitWorktree
/// settles nothing: its `remove` can keep the worktree, so only the disk tells whether it is gone.
pub fn made_by_tool(payload: &Value) -> Option<Made> {
    let response = payload.get("tool_response").filter(|response| response.get("action").is_none())?;
    let text = |key: &str| response.get(key).and_then(Value::as_str);
    let at = |subagent: bool| {
        text("worktreePath").filter(|path| path.starts_with('/')).map(|path| Made::At {
            path: path.to_owned(),
            branch: text("worktreeBranch").map(str::to_owned),
            subagent,
        })
    };
    match payload.get("tool_name").and_then(Value::as_str)? {
        "EnterWorktree" => at(false),
        "Agent" | "Task" if payload.pointer("/tool_input/isolation").and_then(Value::as_str) == Some("worktree") => {
            at(true).or_else(|| {
                let agent = text("agentId").filter(|agent| alondra_engine_ports::worktrees::subagent_id(agent))?;
                (text("status") == Some("async_launched")).then(|| Made::Subagent { agent: agent.to_owned() })
            })
        }
        _ => None,
    }
}

/// Records the worktree a `claude --bg` node's tool made as its task's.
pub fn record_from_hook(domain: &dyn Runtime, agent: &AgentId, payload: &Value) -> Result<()> {
    let Some(made) = made_by_tool(payload) else { return Ok(()) };
    let state = domain.state();
    let Some(task) = state.tasks.values().find(|t| t.agent_id.as_ref() == Some(agent) && !t.state.terminal()).map(|t| t.id.clone()) else {
        return Ok(());
    };
    record(domain, &task, made)
}

fn record(domain: &dyn Runtime, task: &TaskId, made: Made) -> Result<()> {
    let (path, branch, origin) = match made {
        Made::At { path, branch, subagent } => (path, branch, if subagent { WorktreeOrigin::Subagent } else { WorktreeOrigin::Entered }),
        Made::Subagent { agent } => {
            let launched = domain.launch_plan(task).map(|plan| plan.cwd);
            let Some(own) = domain.task_worktree(task)?.or(launched) else {
                log!("{task} has neither a worktree of its own nor a launch folder to place subagent {agent}'s: its close finds it in its provider's notes");
                return Ok(());
            };
            let path = domain.ports().worktrees().subagent_worktree(Path::new(&own), &agent)
                .with_context(|| format!("{own} is in no repository"))?;
            (path.display().to_string(), None, WorktreeOrigin::Subagent)
        }
    };
    let command = Command::RecordTaskWorktree { task_id: task.clone(), path, origin, branch };
    domain.execute_system_command(command).map_err(|error| anyhow::anyhow!("{error}"))
}

/// Records as the task's each worktree a provider noted the sessions of the task's conversations
/// made that is still there and no task has: one whose making Alondra did not see, as when the
/// daemon was down. Closing then settles it as any other. The task as the domain has it after.
pub fn recover_noted(domain: &dyn Runtime, task: &Task) -> Result<Task> {
    let state = domain.state();
    let worktrees = domain.ports().worktrees();
    let records = domain.store().lock_or_exit("the domain store").conversations_metadata(&task.id).unwrap_or_else(|why| {
        log!("could not read the conversations of {}: {why}", task.id);
        vec![]
    });
    let conversations = Conversations { agent: task.agent_id.as_ref().map(|agent| agent.0.as_str()), records: &records };
    for noted in domain.ports().session_worktrees().made_by(&conversations) {
        let path = noted.path.display().to_string();
        if state.tasks.values().any(|other| other.worktrees.contains_key(&path)) || !worktrees.exists(&noted.path) {
            continue;
        }
        if let Err(why) = domain.registrable_worktree(&noted.path) {
            log!("{} does not take {path}, noted for one of its sessions: {why}", task.id);
            continue;
        }
        let command = Command::RecordTaskWorktree { task_id: task.id.clone(), path: path.clone(), origin: WorktreeOrigin::Subagent, branch: noted.branch };
        domain.execute_system_command(command).map_err(|error| anyhow::anyhow!("{error}"))?;
        log!("{path}, which its provider noted for a session of {}, is recorded as its worktree", task.id);
    }
    Ok(domain.state().tasks.get(&task.id).cloned().unwrap_or_else(|| task.clone()))
}

/// The open task that goes on working in the worktree at `path` once `task` lets it go: one that
/// holds it, or one whose launch plan starts it there, which then takes it. A folder that cannot
/// be a worktree, as a projects root other tasks were launched in, is nobody's to take.
pub fn heir(domain: &dyn Runtime, task: &Task, path: &str) -> Result<Option<TaskId>> {
    let state = domain.state();
    if let Some(heir) = state.worktree_held_by_other(path, &task.id) {
        return Ok(Some(heir.clone()));
    }
    if let Err(why) = domain.registrable_worktree(Path::new(path)) {
        log!("no task takes {path} from {}: {why}", task.id);
        return Ok(None);
    }
    let mut launched_there = None;
    for other in state.tasks.values().filter(|other| other.id != task.id && !other.over()) {
        if domain.launch_plan(&other.id).is_some_and(|plan| Path::new(&plan.cwd) == Path::new(path)) {
            launched_there = Some(other.id.clone());
            break;
        }
    }
    let Some(heir) = launched_there else {
        return Ok(None);
    };
    let branch = task.worktrees.get(path).and_then(|worktree| worktree.branch.clone());
    let command = Command::RecordTaskWorktree { task_id: heir.clone(), path: path.to_owned(), origin: WorktreeOrigin::Inherited, branch };
    domain.execute_system_command(command).map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok(Some(heir))
}

pub use alondra_engine_ports::worktrees::{Unreachable, settle_text};

/// What each worktree of the task that the node did not give up, and no other open task goes on
/// in, still holds that no remote has: the work that keeps the task from closing, by path.
pub async fn leftovers(domain: &dyn Runtime, task: &Task) -> Vec<(String, String)> {
    let recovered = recover_noted(domain, task).unwrap_or_else(|why| {
        log!("could not recover the subagent worktrees of {}: {why}", task.id);
        task.clone()
    });
    let task = &recovered;
    let mut left = vec![];
    for (path, _) in task.open_worktrees().filter(|(path, worktree)| !worktree.discard && domain.ports().worktrees().exists(Path::new(path))) {
        if domain.registrable_worktree(Path::new(path)).is_err() {
            continue;
        }
        match heir(domain, task, path) {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(why) => {
                left.push((path.clone(), why.to_string()));
                continue;
            }
        }
        match domain.ports().worktrees().unsaved(Path::new(path)).await {
            Ok(None) => {}
            Ok(Some(what)) => left.push((path.clone(), what)),
            Err(why) => left.push((path.clone(), why)),
        }
    }
    left
}

/// Deletes every worktree of the task that can go, the ones the node gave up as they are, and
/// records how each ended. What could not go is returned, by path.
pub async fn release_all(domain: &dyn Runtime, task: &Task) -> Result<Vec<(String, String)>> {
    let task = &recover_noted(domain, task)?;
    let (mut left, mut unreachable) = (vec![], vec![]);
    for (path, worktree) in task.open_worktrees() {
        if let Err(why) = ready(domain, path).await {
            unreachable.push(why.to_string());
            continue;
        }
        let end = if let Err(why) = may_handle(domain, path) {
            log!("leaving {path} of {} as it is: {why}", task.id);
            WorktreeEnd::UsersOwn
        } else if let Some(heir) = heir(domain, task, path)? {
            log!("leaving the worktree {path} of {} to {heir}, which goes on working in it", task.id);
            WorktreeEnd::HandedOver
        } else {
            match domain.ports().worktrees().release_finished_worktree(Path::new(path), None, worktree.discard, domain.state().merged_pr(task).is_some()).await {
                Ok(TaskWorktree::Removed { freed }) => {
                    log!("deleting the worktree {path} freed {}", alondra_core::machine_incident::bytes_text(freed, alondra_view_model::Lang::En));
                    WorktreeEnd::Removed
                }
                Ok(TaskWorktree::Absent) => WorktreeEnd::Removed,
                Ok(TaskWorktree::NotAlondras) => WorktreeEnd::UsersOwn,
                Ok(TaskWorktree::Unsaved(what)) => {
                    left.push((path.clone(), what));
                    continue;
                }
                Ok(TaskWorktree::Unavailable(why)) => {
                    unreachable.push(why);
                    continue;
                }
                Err(why) => {
                    left.push((path.clone(), why));
                    continue;
                }
            }
        };
        let command = Command::ReleaseTaskWorktree { task_id: task.id.clone(), path: path.clone(), end };
        domain.execute_system_command(command).map_err(|error| anyhow::anyhow!("{error}"))?;
    }
    if !unreachable.is_empty() {
        return Err(Unreachable(unreachable.join("; ")).into());
    }
    Ok(left)
}

/// What ended tasks left of their worktrees once git removed the contents: a link to nothing, or a
/// folder with nothing in it. Whoever ended the task (`kept_by_user`, `users_own`, or Alondra
/// itself) left it recorded as ended, so no later close would ever look at it again. It goes, and
/// the record says so. A worktree with anything in it, one an open task still holds, and one on a
/// volume that is not mounted are never touched. Returns how many went.
pub async fn sweep_shells(domain: &dyn Runtime) -> Result<usize> {
    let state = domain.state();
    let worktrees = domain.ports().worktrees();
    let mut gone = 0;
    for task in state.tasks.values().filter(|task| task.over()) {
        for (path, worktree) in &task.worktrees {
            if !matches!(worktree.end, Some(WorktreeEnd::KeptByUser | WorktreeEnd::UsersOwn | WorktreeEnd::Removed)) {
                continue;
            }
            let dir = Path::new(path);
            let dangling = std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.file_type().is_symlink()) && !dir.exists();
            if !(dangling || worktrees.is_empty_shell(dir)) || in_use(domain, &state, &task.id, path) {
                continue;
            }
            if ready(domain, path).await.is_err() || worktrees.unreachable_link(dir).is_some() {
                continue;
            }
            match worktrees.release_recorded_worktree(dir, false).await {
                Ok(TaskWorktree::Removed { .. } | TaskWorktree::Absent) => {
                    gone += 1;
                    log!("deleted what was left of the worktree {path} of {}", task.id);
                    if worktree.end != Some(WorktreeEnd::Removed) {
                        let command = Command::ReleaseTaskWorktree { task_id: task.id.clone(), path: path.clone(), end: WorktreeEnd::Removed };
                        domain.execute_system_command(command).map_err(|error| anyhow::anyhow!("{error}"))?;
                    }
                }
                Ok(_) => {}
                Err(why) => log!("could not delete what was left of the worktree {path} of {}: {why}", task.id),
            }
        }
    }
    Ok(gone)
}

/// Whether a task still owns `path`: `task` itself once reopened, another one that holds it open,
/// even while it closes, or one that has not ended and will start there.
pub(crate) fn in_use(domain: &dyn Runtime, state: &DomainState, task: &TaskId, path: &str) -> bool {
    state.tasks.get(task).is_some_and(|own| !own.over())
        || state.tasks.values().any(|other| &other.id != task && other.open_worktrees().any(|(open, _)| open == path))
        || state.tasks.values().filter(|other| &other.id != task && !other.over())
            .any(|other| domain.launch_plan(&other.id).is_some_and(|plan| Path::new(&plan.cwd) == Path::new(path)))
}

/// Whether Alondra may look into or delete the recorded `path`: it is gone, an empty folder git
/// left behind, or still a worktree.
/// Why not, otherwise.
pub fn may_handle(domain: &dyn Runtime, path: &str) -> std::result::Result<(), String> {
    let worktrees = domain.ports().worktrees();
    match worktrees.exists(Path::new(path)) && !worktrees.is_empty_shell(Path::new(path)) {
        true => domain.registrable_worktree(Path::new(path)),
        false => Ok(()),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn post(tool: &str, input: Value, response: Value) -> Value {
        json!({"hook_event_name": "PostToolUse", "tool_name": tool, "tool_input": input, "tool_response": response})
    }

    #[test]
    fn the_worktrees_a_node_and_its_subagents_make_are_read_from_their_tools() {
        let isolated = json!({"isolation": "worktree", "prompt": "Fix it"});
        assert_eq!(made_by_tool(&post("Agent", isolated.clone(), json!({"status": "completed", "agentId": "a9e1782746c156d58",
            "worktreePath": "/repo/.claude/worktrees/agent-a9e1782746c156d58", "worktreeBranch": "worktree-agent-a9e1782746c156d58"}))),
            Some(Made::At { path: "/repo/.claude/worktrees/agent-a9e1782746c156d58".into(), branch: Some("worktree-agent-a9e1782746c156d58".into()), subagent: true }));
        assert_eq!(made_by_tool(&post("Agent", isolated.clone(), json!({"status": "async_launched", "agentId": "abd91aef2f770f04e"}))),
            Some(Made::Subagent { agent: "abd91aef2f770f04e".into() }));
        assert_eq!(made_by_tool(&post("Agent", json!({"prompt": "Look"}), json!({"status": "async_launched", "agentId": "abd91aef2f770f04e"}))), None,
            "a subagent without an isolated worktree makes none");
        assert_eq!(made_by_tool(&post("Agent", isolated, json!({"status": "completed", "agentId": "a0000000000000002"}))), None,
            "Claude Code removed the worktree of a subagent that changed nothing");
        assert_eq!(made_by_tool(&post("EnterWorktree", json!({"name": "spike"}), json!({"worktreePath": "/repo/.claude/worktrees/spike", "worktreeBranch": "worktree-spike"}))),
            Some(Made::At { path: "/repo/.claude/worktrees/spike".into(), branch: Some("worktree-spike".into()), subagent: false }));
        assert_eq!(made_by_tool(&post("ExitWorktree", json!({"action": "remove"}), json!({"action": "remove", "worktreePath": "/repo/.claude/worktrees/spike"}))), None);
        assert_eq!(made_by_tool(&post("Agent", json!({"isolation": "worktree"}), json!({"status": "async_launched", "agentId": "../../etc"}))), None);
    }

    #[test]
    fn a_background_subagent_is_placed_whatever_the_length_of_the_id_claude_code_gives_it() {
        for agent in ["a1b2c3d4e5f6", "agent_7-B"] {
            assert_eq!(made_by_tool(&post("Agent", json!({"isolation": "worktree"}), json!({"status": "async_launched", "agentId": agent}))),
                Some(Made::Subagent { agent: agent.into() }), "{agent}");
        }
    }
}
