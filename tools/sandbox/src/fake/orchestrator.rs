//! A fake orchestrator batch. Through the Agent SDK host it is a native Claude started with
//! `--restricted` that takes the batch as its one input on stdin; `claude -p --output-format
//! stream-json …` is the same batch for a daemon without the SDK backend. Either creates the
//! scripted tasks of every request the batch names, or closes the request with its scripted
//! answer, through the MCP server Alondra configured, and ends with the stream Alondra expects
//! from a successful run. A run that wrote `fakes/orchestrator/ask.json` has the next batch ask
//! the user that card once, then moves it to `asked.json`. A run whose batch holds the case of a
//! stopped node settles it with the outcome `fakes/orchestrator/stall.json` holds, then moves that
//! file to `stall-settled-<case>.json`; without the file it leaves the case unsettled.
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
const TOOLS: [&str; 8] = ["tasks_in_progress", "node", "ask", "create_task", "launch", "close_request", "settle_suggestion", "settle_stall"];

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
    let card = dir.join("ask.json");
    if card.exists() {
        let call: Value = serde_json::from_slice(&std::fs::read(&card)?)?;
        let reply = mcp::call(&program, &server_args, &[], &[("ask", call.clone())])?.remove(0);
        append(&dir.join("actions.jsonl"), &json!({"at": now_ms(), "tool": "ask", "arguments": call, "refused": reply.refused, "text": reply.text}))?;
        std::fs::rename(&card, dir.join("asked.json"))?;
        made.push(("ask", call, reply));
    }
    let read = dir.join("call.json");
    if read.exists() {
        let call: Value = serde_json::from_slice(&std::fs::read(&read)?)?;
        let tool = match call["tool"].as_str() {
            Some("node") => "node",
            Some("tasks_in_progress") => "tasks_in_progress",
            other => anyhow::bail!("call.json names {other:?}: the sandbox's orchestrator only reads with node and tasks_in_progress"),
        };
        let arguments = call["arguments"].clone();
        let reply = mcp::call(&program, &server_args, &[], &[(tool, arguments.clone())])?.remove(0);
        append(&dir.join("actions.jsonl"), &json!({"at": now_ms(), "tool": tool, "arguments": arguments, "refused": reply.refused, "text": reply.text}))?;
        std::fs::rename(&read, dir.join("called.json"))?;
        made.push((tool, arguments, reply));
    }
    let outcome = dir.join("stall.json");
    if let Some((case, task)) = stall_case(prompt).filter(|_| outcome.exists()) {
        let call = json!({"case_id": case, "task_id": task, "outcome": serde_json::from_slice::<Value>(&std::fs::read(&outcome)?)?});
        let reply = mcp::call(&program, &server_args, &[], &[("settle_stall", call.clone())])?.remove(0);
        append(&dir.join("actions.jsonl"), &json!({"at": now_ms(), "tool": "settle_stall", "arguments": call, "refused": reply.refused, "text": reply.text}))?;
        std::fs::rename(&outcome, dir.join(format!("stall-settled-{case}.json")))?;
        made.push(("settle_stall", call, reply));
    }
    for script in fakes.request_scripts()? {
        let handled = dir.join(format!("{}.handled", script.request_id));
        let summarized = dir.join(format!("{}.summarized", script.request_id));
        let mut calls: Vec<(&'static str, Value)> = vec![];
        if let Some(summary) = script.summary.as_ref().filter(|_| {
            handled.exists() && !summarized.exists() && prompt.contains(&format!("Every Task of Request {}", script.request_id))
        }) {
            calls.push(("close_request", json!({"request_id": script.request_id, "answer": summary})));
            std::fs::write(&summarized, "")?;
        }
        if !prompt.contains(&script.request_id) || handled.exists() {
            made.extend(call_all(&dir, &program, &server_args, calls)?);
            continue;
        }
        if let Some(answer) = &script.answer {
            let mut close = json!({"request_id": script.request_id, "answer": answer});
            if let Some(headline) = &script.headline {
                close["headline"] = json!(headline);
            }
            calls.push(("close_request", close));
        }
        if let Some(title) = &script.title {
            calls.push(("title_request", json!({"request_id": script.request_id, "title": title})));
        }
        for task in &script.tasks {
            let launch = json!({"name": format!("sandbox/{}", task.task_id), "cwd": fakes.repo(), "model": task.model.as_deref().unwrap_or("sonnet"), "reason": "A scripted sandbox node"});
            calls.push(("create_task", json!({
                "task_id": task.task_id, "request_id": script.request_id, "title": task.title, "agent_id": null,
                "review_prs": false, "dependencies": task.dependencies, "launch": launch,
            })));
        }
        for task in &script.tasks {
            if let Some(priority) = &task.priority {
                calls.push(("set_priority", json!({"task_id": task.task_id, "priority": priority})));
            }
        }
        made.extend(call_all(&dir, &program, &server_args, calls)?);
        std::fs::write(&handled, "")?;
    }
    Ok(made)
}

/// The case and task of the first stopped node the batch asks to settle.
fn stall_case(prompt: &str) -> Option<(String, String)> {
    let rest = prompt.split("settle_stall(case_id: \"").nth(1)?;
    let (case, rest) = rest.split_once('"')?;
    let task = rest.split("task_id: \"").nth(1)?.split('"').next()?;
    Some((case.to_owned(), task.to_owned()))
}

fn call_all(dir: &std::path::Path, program: &std::path::Path, server_args: &[String], calls: Vec<(&'static str, Value)>) -> Result<Vec<(&'static str, Value, mcp::Reply)>> {
    if calls.is_empty() {
        return Ok(vec![]);
    }
    let replies = mcp::call(program, server_args, &[], &calls)?;
    let mut made = vec![];
    for ((tool, call), reply) in calls.into_iter().zip(replies) {
        append(&dir.join("actions.jsonl"), &json!({"at": now_ms(), "tool": tool, "arguments": call, "refused": reply.refused, "text": reply.text}))?;
        made.push((tool, call, reply));
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
        append(&dir.join("batches.jsonl"), &json!({"at": now_ms(), "session": session, "account": super::native::account(), "prompt": prompt}))?;
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
