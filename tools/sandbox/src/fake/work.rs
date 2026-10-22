//! What a scripted session does to the repository, the fake GitHub and Alondra for one step,
//! whichever session runs it: a `claude --bg` one or a native Claude of the Agent SDK host.
use std::{fs, path::Path, time::Duration};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::{Fakes, gh, git, mcp, script::Step};
use crate::sandbox::{hex, random};

/// The session a step runs in: its task, the folder it works in, the short id its cards carry and
/// the environment Alondra's MCP server gets.
pub struct Worker<'a> {
    pub task: Option<&'a str>,
    pub dir: &'a Path,
    pub short: &'a str,
    pub env: &'a dyn Fn() -> Result<Vec<(String, String)>>,
}

impl Worker<'_> {
    fn tool(&self, fakes: &Fakes, name: &str, arguments: Value) -> Result<mcp::Reply> {
        let mut replies = mcp::call(&fakes.alondra(), &["mcp-node".into()], &(self.env)()?, &[(name, arguments)])?;
        replies.pop().context("no reply")
    }
}

/// The Claude Code tool a step stands for, as its hooks and stream name it.
pub fn tool_of(step: &Step) -> Option<(String, Value)> {
    let mcp = |name: &str| Some((format!("mcp__plugin_alondra_alondra__{name}"), json!({})));
    let bash = |command: &str| Some(("Bash".to_owned(), json!({"command": command})));
    match step {
        Step::Commit { file, .. } => bash(&format!("git add -A && git commit -m 'Add {file}'")),
        Step::Dirty { file } => Some(("Write".into(), json!({"file_path": file}))),
        Step::Push => bash("git push -u origin HEAD"),
        Step::OpenPr { .. } => bash("gh pr create --fill"),
        Step::RegisterPr { .. } => mcp("register_pr"),
        Step::Deliver { .. } => mcp("present_result"),
        Step::Ask { .. } => mcp("ask"),
        Step::Cleanup => mcp("cleanup_done"),
        Step::PostMergeWork { .. } => mcp("post_merge_work"),
        Step::Call { tool, arguments } => Some((format!("mcp__plugin_alondra_alondra__{tool}"), arguments.clone())),
        Step::Bash { command } => bash(command),
        Step::Speak { .. } | Step::End | Step::Die | Step::OwnTurn { .. } | Step::LeavePlanMode { .. } | Step::EndOwnTurn | Step::RateLimit { .. } => None,
    }
}

/// Runs `command` with `/bin/sh` in `dir`: its output, refused when it fails.
pub fn shell(dir: &Path, command: &str) -> Result<mcp::Reply> {
    let out = std::process::Command::new("/bin/sh").arg("-c").arg(command).current_dir(dir).output().context("sh")?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)).trim().to_owned();
    Ok(mcp::Reply { refused: !out.status.success(), text })
}

pub fn done(text: impl Into<String>) -> mcp::Reply {
    mcp::Reply { refused: false, text: text.into() }
}

/// Does a step that acts on the repository, GitHub or Alondra; `None` for the steps that end a
/// turn or the session, which only the session itself can take.
pub fn perform(fakes: &Fakes, worker: &Worker, step: &Step) -> Option<Result<mcp::Reply>> {
    let task = || worker.task.map(str::to_owned).context("this session has no scripted task");
    let dir = worker.dir;
    let result = match step {
        Step::Commit { file, text } => (|| -> Result<mcp::Reply> {
            fs::write(dir.join(file), text)?;
            git(dir, &["add", "-A"])?;
            git(dir, &["commit", "-m", &format!("Add {file}")])?;
            Ok(done(git(dir, &["rev-parse", "HEAD"])?))
        })(),
        Step::Dirty { file } => fs::write(dir.join(file), "left behind\n").map(|()| done(file.clone())).map_err(Into::into),
        Step::Push => git(dir, &["push", "-u", "origin", "HEAD"]).map(done),
        Step::OpenPr { number, draft, checks } => gh::open(fakes, dir, *number, *draft, *checks).map(|pr| done(format!("PR {} on {}", pr.number, pr.branch))),
        Step::RegisterPr { number } => (|| -> Result<mcp::Reply> {
            let head = git(dir, &["rev-parse", "HEAD"])?;
            worker.tool(fakes, "register_pr", json!({
                "task_id": task()?, "pr_id": format!("pr-{number}"), "host": "github.com", "repository": gh::REPOSITORY,
                "number": number, "head": head, "base": "main", "allowed_methods": ["squash"],
            }))
        })(),
        Step::Deliver { need, pr, is_final } => (|| -> Result<mcp::Reply> {
            let targets = match pr {
                Some(number) => json!([{"kind": "pull_request", "pr_id": format!("pr-{number}"), "head": git(dir, &["rev-parse", "HEAD"])?}]),
                None => json!([{"kind": "text", "text": need}]),
            };
            let arguments = json!({
                "task_id": task()?, "interaction_id": format!("result-{}-{}", worker.short, hex(&random::<3>()?)),
                "need": need, "body": format!("{need}: done in the sandbox."), "targets": targets, "is_final": is_final,
                "content": {"kind": "report", "body": format!("{need}: the sandbox node did the scripted work.")},
            });
            // Alondra learns a pushed head from GitHub a moment later; a node told so sends the
            // same card again, as the refusal says.
            let start = std::time::Instant::now();
            loop {
                let reply = worker.tool(fakes, "present_result", arguments.clone())?;
                if !reply.text.contains("[pr_mismatch]") || start.elapsed() > Duration::from_secs(15) {
                    return Ok(reply);
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        })(),
        Step::Ask { id, need, options } => (|| -> Result<mcp::Reply> {
            let options: Vec<Value> = options
                .iter()
                .map(|label| json!({"id": label.to_lowercase().replace(' ', "-"), "label": label, "description": format!("Choose {label}"), "inputs": []}))
                .collect();
            worker.tool(fakes, "ask", json!({
                "interaction_id": id, "scope": {"kind": "task", "task_id": task()?}, "recipient": {"kind": "user"},
                "need": need, "why": "The sandbox node needs this decision to go on.", "blocking": true, "options": options,
            }))
        })(),
        Step::Cleanup => task().and_then(|task| worker.tool(fakes, "cleanup_done", json!({"task_id": task}))),
        Step::PostMergeWork { reason } => task().and_then(|task| worker.tool(fakes, "post_merge_work", json!({"task_id": task, "reason": reason}))),
        Step::Call { tool, arguments } => worker.tool(fakes, tool, arguments.clone()),
        Step::Bash { command } => shell(dir, command),
        Step::Speak { .. } | Step::End | Step::Die | Step::OwnTurn { .. } | Step::LeavePlanMode { .. } | Step::EndOwnTurn | Step::RateLimit { .. } => return None,
    };
    Some(result)
}
