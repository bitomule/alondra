//! Every git worktree a task made or took, its node's and those of the subagents its node
//! launched: recorded from what Claude Code says it made, and settled when the task closes.
use std::path::Path;

use alondra_core::domain::{AgentId, Command, Task, TaskId, WorktreeEnd, WorktreeOrigin};
use crate::worktree::TaskWorktree;
use anyhow::{Context, Result};
use serde_json::Value;

use crate::domain_runtime::DomainRuntime;

/// A worktree a tool result says Claude Code made.
#[derive(Debug, PartialEq)]
pub(crate) enum Made {
    At { path: String, branch: Option<String>, subagent: bool },
    /// A subagent with an isolated worktree that went on in the background: Claude Code names
    /// its worktree after the subagent's id.
    Subagent { agent: String },
}

/// What a `PostToolUse` says about a worktree: EnterWorktree's and a kept isolated subagent's
/// `worktreePath`, or the id of an isolated subagent that went on in the background. ExitWorktree
/// settles nothing: its `remove` can keep the worktree, so only the disk tells whether it is gone.
pub(crate) fn made_by_tool(payload: &Value) -> Option<Made> {
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
                let agent = text("agentId").filter(|agent| subagent_id(agent))?;
                (text("status") == Some("async_launched")).then(|| Made::Subagent { agent: agent.to_owned() })
            })
        }
        _ => None,
    }
}

/// The ids Claude Code gives subagents, which name their worktrees `agent-<id>`.
fn subagent_id(agent: &str) -> bool {
    matches!(agent.len(), 8 | 17) && agent.starts_with('a') && agent[1..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Records the worktree a `claude --bg` node's tool made as its task's.
pub(crate) fn record_from_hook(domain: &DomainRuntime, agent: &AgentId, payload: &Value) -> Result<()> {
    let Some(made) = made_by_tool(payload) else { return Ok(()) };
    let state = domain.state()?;
    let Some(task) = state.tasks.values().find(|t| t.agent_id.as_ref() == Some(agent) && !t.state.terminal()).map(|t| t.id.clone()) else {
        return Ok(());
    };
    record(domain, &task, made)
}

fn record(domain: &DomainRuntime, task: &TaskId, made: Made) -> Result<()> {
    let (path, branch, origin) = match made {
        Made::At { path, branch, subagent } => (path, branch, if subagent { WorktreeOrigin::Subagent } else { WorktreeOrigin::Entered }),
        Made::Subagent { agent } => {
            let own = domain.task_worktree(task)?.with_context(|| format!("{task} has no worktree of its own to place subagent {agent}'s"))?;
            let path = crate::worktree::subagent_worktree(Path::new(&own), &agent)
                .with_context(|| format!("{own} is not a worktree Alondra made"))?;
            (path.display().to_string(), None, WorktreeOrigin::Subagent)
        }
    };
    let command = Command::RecordTaskWorktree { task_id: task.clone(), path, origin, branch };
    crate::domain_github::execute_system(domain, command).map_err(|error| anyhow::anyhow!("{error}"))
}

/// What each worktree of the task that the node did not give up still holds that no remote has:
/// the work that keeps the task from closing, by path.
pub(crate) async fn leftovers(task: &Task) -> Vec<(String, String)> {
    let mut left = vec![];
    for (path, _) in task.open_worktrees().filter(|(path, worktree)| !worktree.discard && Path::new(path).exists()) {
        match crate::worktree::unsaved(Path::new(path)).await {
            Ok(None) => {}
            Ok(Some(what)) => left.push((path.clone(), what)),
            Err(why) => left.push((path.clone(), why)),
        }
    }
    left
}

/// Deletes every worktree of the task that can go, the ones the node gave up as they are, and
/// records how each ended. What could not go is returned, by path.
pub(crate) async fn release_all(domain: &DomainRuntime, task: &Task) -> Result<Vec<(String, String)>> {
    let mut left = vec![];
    for (path, worktree) in task.open_worktrees() {
        let end = match crate::worktree::release_recorded_worktree(Path::new(path), worktree.discard).await {
            Ok(TaskWorktree::Removed | TaskWorktree::Absent) => WorktreeEnd::Removed,
            Ok(TaskWorktree::NotAlondras) => WorktreeEnd::UsersOwn,
            Ok(TaskWorktree::Unsaved(what)) => {
                left.push((path.clone(), what));
                continue;
            }
            Err(why) => {
                left.push((path.clone(), why));
                continue;
            }
        };
        let command = Command::ReleaseTaskWorktree { task_id: task.id.clone(), path: path.clone(), end };
        crate::domain_github::execute_system(domain, command).map_err(|error| anyhow::anyhow!("{error}"))?;
    }
    Ok(left)
}

/// What the node of a task that cannot close yet is told: each worktree with work left, and how
/// to settle it.
pub(crate) fn settle_text(task: &TaskId, left: &[(String, String)]) -> String {
    let list: String = left.iter().map(|(path, what)| format!("\n- {path}: {what}")).collect();
    format!("Alondra cannot close task {task} yet: these worktrees of the task, yours or those of the subagents you \
launched, hold work no remote has:{list}\nFor each one, commit the work and push it to its branch, or give it up. \
Then call cleanup_done with task_id {task}, listing in discard the paths you give up: Alondra deletes those as they \
are and closes the task once no worktree of it holds work no remote has.")
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
}
