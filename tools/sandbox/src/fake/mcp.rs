//! A Claude-side MCP client: starts an Alondra MCP server as Claude Code would and calls its tools.
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// What one tool call answered: refused or not, and its text.
#[derive(Debug, Clone)]
pub struct Reply {
    pub refused: bool,
    pub text: String,
}

/// Starts `program args` with `env`, initializes it and runs `calls` in order.
pub fn call(program: &Path, args: &[String], env: &[(String, String)], calls: &[(&str, Value)]) -> Result<Vec<Reply>> {
    let mut child = Command::new(program)
        .args(args)
        .envs(env.iter().map(|(key, value)| (key.as_str(), value.as_str())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start {}", program.display()))?;
    {
        let stdin = child.stdin.as_mut().context("MCP stdin")?;
        writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}))?;
        for (index, (tool, arguments)) in calls.iter().enumerate() {
            writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":index + 1,"method":"tools/call","params":{"name":tool,"arguments":arguments}}))?;
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!("{} exited {}: {}", program.display(), output.status, String::from_utf8_lossy(&output.stderr).trim());
    }
    let replies: Vec<Value> = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice)
        .collect::<Result<_, _>>()?;
    if replies.len() != calls.len() + 1 {
        bail!("{} answered {} of {} calls: {}", program.display(), replies.len().saturating_sub(1), calls.len(), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(replies[1..]
        .iter()
        .map(|reply| {
            let result = &reply["result"];
            let text = result["content"][0]["text"].as_str().map(str::to_owned).unwrap_or_else(|| reply["error"].to_string());
            Reply { refused: result["isError"] == true || reply.get("error").is_some(), text }
        })
        .collect())
}
