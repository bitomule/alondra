//! A fake orchestrator batch. Through the Agent SDK host it is a native Claude started with
//! `--restricted` that takes the batch as its one input on stdin; `claude -p --output-format
//! stream-json …` is the same batch for a daemon without the SDK backend. Either creates the
//! scripted tasks of every request the batch names, or closes the request with its scripted
//! answer, through the MCP server Alondra configured, and ends with the stream Alondra expects
//! from a successful run.
use std::{
    io::{BufRead, Read, Write},
    path::PathBuf,
};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::{
    Fakes, append, flag_value, mcp,
    native::{flag, uuid},
};
use crate::sandbox::{hex, now_ms, random};

const SERVER: &str = "alondra-orchestrator";
const TOOLS: [&str; 6] = ["fleet", "ask", "create_task", "launch", "close_request", "settle_suggestion"];

pub fn run(fakes: &Fakes, args: &[String]) -> Result<i32> {
    let session = flag_value(args, "--session-id").or_else(|| flag_value(args, "--resume")).context("no session id")?.to_owned();
    let mut prompt = String::new();
    std::io::stdin().read_to_string(&mut prompt)?;
    let dir = fakes.dir.join("orchestrator");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("prompt-{}.txt", now_ms())), &prompt)?;
    println!("{}", json!({
        "type": "system", "subtype": "init", "session_id": session, "permissionMode": "auto", "model": "opus",
        "mcp_servers": [{"name": SERVER, "status": "connected"}],
        "tools": ["Skill", "Read", "Glob", "Grep", "mcp__alondra-orchestrator__create_task", "mcp__alondra-orchestrator__ask"],
    }));
    handle(fakes, args, &prompt)?;
    println!("{}", json!({"type": "assistant", "session_id": session, "message": {"model": "opus", "usage": {"input_tokens": 1}}}));
    println!("{}", json!({"type": "result", "session_id": session, "subtype": "success", "is_error": false, "result": "Done.", "modelUsage": {"opus": {"contextWindow": 1_000_000}}}));
    Ok(0)
}

/// The calls the batch in `prompt` asks for, made through the configured MCP server and written
/// down: each with its arguments and what the server answered.
fn handle(fakes: &Fakes, args: &[String], prompt: &str) -> Result<Vec<(&'static str, Value, mcp::Reply)>> {
    let dir = fakes.dir.join("orchestrator");
    let (program, server_args) = server(args)?;
    let mut made = vec![];
    for script in fakes.request_scripts()? {
        let handled = dir.join(format!("{}.handled", script.request_id));
        if !prompt.contains(&script.request_id) || handled.exists() {
            continue;
        }
        let mut calls: Vec<(&'static str, Value)> = vec![];
        if let Some(answer) = &script.answer {
            calls.push(("close_request", json!({"request_id": script.request_id, "answer": answer})));
        }
        for task in &script.tasks {
            let launch = json!({"name": format!("sandbox/{}", task.task_id), "cwd": fakes.repo(), "model": "sonnet", "reason": "A scripted sandbox node"});
            calls.push(("create_task", json!({
                "task_id": task.task_id, "request_id": script.request_id, "title": task.title, "agent_id": null,
                "review_prs": false, "dependencies": [], "launch": launch,
            })));
            calls.push(("launch", json!({"task": task.task_id, "title": task.title, "model": "sonnet", "reason": "A scripted sandbox node"})));
        }
        let replies = mcp::call(&program, &server_args, &[], &calls)?;
        for ((tool, call), reply) in calls.into_iter().zip(replies) {
            append(&dir.join("actions.jsonl"), &json!({"at": now_ms(), "tool": tool, "arguments": call, "refused": reply.refused, "text": reply.text}))?;
            made.push((tool, call, reply));
        }
        std::fs::write(&handled, "")?;
    }
    Ok(made)
}

/// The Alondra MCP server `--mcp-config` names, as Claude Code would start it.
fn server(args: &[String]) -> Result<(PathBuf, Vec<String>)> {
    let config: Value = serde_json::from_str(flag_value(args, "--mcp-config").context("no --mcp-config")?)?;
    let server = &config["mcpServers"][SERVER];
    let program = PathBuf::from(server["command"].as_str().context("the MCP config names no command")?);
    let args = server["args"].as_array().map(|args| args.iter().filter_map(|arg| arg.as_str().map(str::to_owned)).collect()).unwrap_or_default();
    Ok((program, args))
}

/// One batch as the native Claude of an SDK host runs it: its input, every call it makes as a tool
/// use with its result, and one result for the input. It ends when the host closes its input.
pub fn batch(fakes: &Fakes, args: &[String]) -> Result<i32> {
    let session = flag(args, "--session-id").context("the SDK named no session")?.to_lowercase();
    let model = flag(args, "--model").unwrap_or("opus").to_owned();
    let dir = fakes.dir.join("orchestrator");
    std::fs::create_dir_all(&dir)?;
    let mut out = std::io::stdout().lock();
    let mut send = |value: Value| -> Result<()> {
        writeln!(out, "{value}")?;
        out.flush()?;
        Ok(())
    };
    for line in std::io::stdin().lock().lines() {
        let message: Value = serde_json::from_str(&line?)?;
        if message["type"] == "control_request" {
            let response = match message["request"]["subtype"].as_str() {
                Some("initialize") => json!({"commands": [], "models": [], "account": {}, "output_style": "default"}),
                Some("interrupt") => json!({"still_queued": []}),
                _ => json!({}),
            };
            send(json!({"type": "control_response", "response": {"subtype": "success", "request_id": message["request_id"], "response": response}}))?;
            continue;
        }
        if message["type"] != "user" {
            continue;
        }
        let input = message["uuid"].as_str().context("an input without a uuid")?.to_lowercase();
        let prompt = message["message"]["content"].as_str().unwrap_or_default().to_owned();
        append(&dir.join("batches.jsonl"), &json!({"at": now_ms(), "session": session, "prompt": prompt}))?;
        let lifecycle = |state: &str| -> Result<Value> {
            Ok(json!({"type": "command_lifecycle", "command_uuid": input, "state": state, "session_id": session, "uuid": uuid()?}))
        };
        send(lifecycle("queued")?)?;
        send(lifecycle("started")?)?;
        send(json!({
            "type": "system", "subtype": "init", "session_id": session, "uuid": uuid()?, "claude_code_version": super::node::VERSION,
            "model": model, "cwd": std::env::current_dir()?, "permissionMode": "auto",
            "tools": (["Skill", "Read", "Glob", "Grep"].into_iter().map(str::to_owned).chain(TOOLS.map(|tool| format!("mcp__{SERVER}__{tool}"))).collect::<Vec<_>>()),
            "mcp_servers": [{"name": SERVER, "status": "connected"}], "slash_commands": [], "output_style": "default", "skills": [], "plugins": [],
            "apiKeySource": "none",
        }))?;
        let assistant = |content: Value| -> Result<Value> {
            Ok(json!({
                "type": "assistant", "session_id": session, "uuid": uuid()?, "parent_tool_use_id": null,
                "message": {"id": format!("msg_{}", hex(&random::<8>()?)), "type": "message", "role": "assistant", "model": model, "content": content,
                    "stop_reason": null, "stop_sequence": null, "usage": {"input_tokens": 25, "output_tokens": 1}},
                "user_message_uuid": input, "user_message_uuids": [input],
            }))
        };
        send(assistant(json!([{"type": "text", "text": "Handling the batch"}]))?)?;
        for (tool, call, reply) in handle(fakes, args, &prompt)? {
            let id = format!("toolu_{}", hex(&random::<6>()?));
            send(assistant(json!([{"type": "tool_use", "id": id, "name": format!("mcp__{SERVER}__{tool}"), "input": call}]))?)?;
            send(json!({
                "type": "user", "session_id": session, "uuid": uuid()?, "parent_tool_use_id": null,
                "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "is_error": reply.refused, "content": reply.text}]},
            }))?;
        }
        send(json!({
            "type": "result", "subtype": "success", "is_error": false, "result": "Batch handled", "session_id": session, "uuid": uuid()?,
            "duration_ms": 0, "duration_api_ms": 0, "num_turns": 1, "total_cost_usd": 0, "usage": {},
            "modelUsage": {model.clone(): {"contextWindow": 1_000_000}}, "permission_denials": [],
            "terminal_reason": "completed", "user_message_uuid": input, "user_message_uuids": [input],
        }))?;
        send(lifecycle("completed")?)?;
    }
    Ok(0)
}
