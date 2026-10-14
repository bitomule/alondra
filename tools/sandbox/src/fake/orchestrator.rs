//! A fake orchestrator run: `claude -p --output-format stream-json …` with the batch on stdin. It
//! creates the scripted tasks of every request the batch names, through the MCP server Alondra
//! configured, and answers with the stream Alondra expects from a successful run.
use std::{io::Read, path::PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::{Fakes, append, flag_value, mcp};
use crate::sandbox::now_ms;

pub fn run(fakes: &Fakes, args: &[String]) -> Result<i32> {
    let session = flag_value(args, "--session-id").or_else(|| flag_value(args, "--resume")).context("no session id")?.to_owned();
    let mut prompt = String::new();
    std::io::stdin().read_to_string(&mut prompt)?;
    let dir = fakes.dir.join("orchestrator");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("prompt-{}.txt", now_ms())), &prompt)?;
    println!("{}", json!({
        "type": "system", "subtype": "init", "session_id": session, "permissionMode": "auto", "model": "opus",
        "mcp_servers": [{"name": "alondra-orchestrator", "status": "connected"}],
        "tools": ["Skill", "Read", "Glob", "Grep", "mcp__alondra-orchestrator__create_task", "mcp__alondra-orchestrator__ask"],
    }));
    let (program, server_args) = server(args)?;
    for script in fakes.request_scripts()? {
        let handled = dir.join(format!("{}.handled", script.request_id));
        if !prompt.contains(&script.request_id) || handled.exists() {
            continue;
        }
        let mut calls: Vec<(&str, Value)> = vec![];
        for task in &script.tasks {
            let launch = json!({"name": format!("sandbox/{}", task.task_id), "cwd": fakes.repo(), "model": "sonnet", "reason": "A scripted sandbox node"});
            calls.push(("create_task", json!({
                "task_id": task.task_id, "request_id": script.request_id, "title": task.title, "agent_id": null,
                "review_prs": false, "dependencies": [], "launch": launch,
            })));
            calls.push(("launch", json!({"task": task.task_id, "title": task.title, "model": "sonnet", "reason": "A scripted sandbox node"})));
        }
        let replies = mcp::call(&program, &server_args, &[], &calls)?;
        for ((tool, call), reply) in calls.iter().zip(&replies) {
            append(&dir.join("actions.jsonl"), &json!({"at": now_ms(), "tool": tool, "arguments": call, "refused": reply.refused, "text": reply.text}))?;
        }
        std::fs::write(&handled, "")?;
    }
    println!("{}", json!({"type": "assistant", "session_id": session, "message": {"model": "opus", "usage": {"input_tokens": 1}}}));
    println!("{}", json!({"type": "result", "session_id": session, "subtype": "success", "is_error": false, "result": "Done.", "modelUsage": {"opus": {"contextWindow": 1_000_000}}}));
    Ok(0)
}

/// The Alondra MCP server `--mcp-config` names, as Claude Code would start it.
fn server(args: &[String]) -> Result<(PathBuf, Vec<String>)> {
    let config: Value = serde_json::from_str(flag_value(args, "--mcp-config").context("no --mcp-config")?)?;
    let server = &config["mcpServers"]["alondra-orchestrator"];
    let program = PathBuf::from(server["command"].as_str().context("the MCP config names no command")?);
    let args = server["args"].as_array().map(|args| args.iter().filter_map(|arg| arg.as_str().map(str::to_owned)).collect()).unwrap_or_default();
    Ok((program, args))
}
