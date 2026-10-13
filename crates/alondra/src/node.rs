//! The commands the node mod runs: `event` hands alondrad one report of its session, `connect`
//! holds the session's connection to alondrad open for as long as both live, `ack` tells alondrad
//! which of the messages it wrote down that connection the session has taken, and `hold` puts one
//! card to the user and prints the answer. The open connection is what tells alondrad the node is
//! reachable; `connect` sleeps in `read` and copies whatever alondrad writes down it, one JSON line
//! per message or interrupt, to its
//! standard output. The plugin's PermissionRequest hook holds its prompt through
//! `permission_request`, on the same card path as `hold`.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde_json::{Value, json};

const MAX_REPORT_BYTES: u64 = 1024 * 1024;
const CLOSED_BY_DAEMON: i32 = 3;
const REDIAL: Duration = Duration::from_secs(1);

#[derive(Debug)]
struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

#[derive(Subcommand)]
pub enum Cmd {
    /// Hands alondrad the report on standard input
    Event,
    /// Opens the session's connection to alondrad and holds it until either side ends
    Connect,
    /// Tells alondrad the session took these message ids; the session id is on standard input
    Ack { ids: Vec<String> },
    /// Puts the card on standard input to the user and prints the answer, waiting across daemon restarts
    Hold,
}

pub fn run(command: Cmd) -> Result<()> {
    let input = stdin_json()?;
    let socket = crate::client::config().home.join("hooks.sock");
    match command {
        Cmd::Event => event(&socket, input),
        Cmd::Ack { ids } => {
            let mut frame = input;
            frame["ids"] = json!(ids);
            event_frame(&socket, json!({"node_ack": frame}))
        }
        Cmd::Hold => hold(&socket, input),
        Cmd::Connect => {
            let mut stream = open(&socket, json!({"node_connect": input}))?;
            println!("connected");
            std::io::stdout().flush()?;
            std::io::copy(&mut stream, &mut std::io::stdout())?;
            std::process::exit(CLOSED_BY_DAEMON);
        }
    }
}

fn stdin_json() -> Result<Value> {
    let mut text = String::new();
    std::io::stdin().take(MAX_REPORT_BYTES).read_to_string(&mut text)?;
    let value: Value = serde_json::from_str(&text).context("the report is not JSON")?;
    if !value.is_object() {
        bail!("the report is not a JSON object");
    }
    Ok(value)
}

fn event(socket: &Path, report: Value) -> Result<()> {
    event_frame(socket, json!({"node_event": report}))
}

fn event_frame(socket: &Path, frame: Value) -> Result<()> {
    let mut stream = open(socket, frame)?;
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest)?;
    Ok(())
}

/// A daemon that is down or restarting is dialled again every second and sent the same card,
/// which it recognises by its id: a card posted before the restart is still waiting when it is back.
fn hold(socket: &Path, card: Value) -> Result<()> {
    loop {
        match open(socket, json!({"node_hold": card})) {
            Ok(mut stream) => {
                let mut answer = String::new();
                if stream.read_to_string(&mut answer).is_ok() && !answer.is_empty() {
                    println!("{answer}");
                    return Ok(());
                }
            }
            Err(error) if error.is::<Refused>() => return Err(error),
            Err(_) => {}
        }
        std::thread::sleep(REDIAL);
    }
}

/// What the plugin's PermissionRequest hook prints for one prompt: Claude Code's decision once the
/// user answered the card, or `None` to leave the prompt to Claude Code's own dialog, as when
/// alondrad is down, the session is not a held node or the card was withdrawn. Never an allow the
/// user did not give.
pub fn permission_request(payload: &Value) -> Option<Value> {
    let launched = std::env::var("ALONDRA_LAUNCHED").is_ok_and(|s| !s.is_empty());
    if let Some(decision) = provably_safe(payload, launched, &crate::client::config().home.join("files")) {
        return Some(decision);
    }
    let card = permission_card(payload, &format!("permission-{}", hex(&crate::service_client::random::<12>().ok()?)))?;
    let socket = crate::client::config().home.join("hooks.sock");
    let answer: Value = serde_json::from_str(&hold_from_hook(&socket, &card)?).ok()?;
    permission_decision(&answer)
}

/// A node's command that provably loses nothing is approved without a card.
pub(crate) fn provably_safe(payload: &Value, launched: bool, files: &Path) -> Option<Value> {
    (launched && crate::safe_permissions::approve(payload, files))
        .then(|| json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "allow"}}}))
}

/// The prompt has no tool call id, so the card carries the hook run's own id and is withdrawn when
/// the hook goes away: Claude Code answered it in its dialog or dropped it.
fn permission_card(payload: &Value, id: &str) -> Option<Value> {
    let session_id = payload.get("session_id")?.as_str()?;
    let tool = payload.get("tool_name")?.as_str()?;
    if tool == "AskUserQuestion" {
        return None;
    }
    Some(json!({"session_id": session_id, "id": id, "kind": "permission", "tool": tool,
        "input": payload.get("tool_input").cloned().unwrap_or(Value::Null),
        "suggestions": payload.get("permission_suggestions").cloned().unwrap_or(Value::Null),
        "cwd": payload.get("cwd").cloned().unwrap_or(Value::Null),
        "permission_mode": payload.get("permission_mode").cloned().unwrap_or(Value::Null), "withdraw_when_gone": true}))
}

fn permission_decision(answer: &Value) -> Option<Value> {
    if answer.get("held") != Some(&Value::Bool(true)) {
        return None;
    }
    let decision = if answer.get("allowed") == Some(&Value::Bool(true)) {
        match answer.get("permissions") {
            Some(permissions) => json!({"behavior": "allow", "updatedPermissions": permissions}),
            None => json!({"behavior": "allow"}),
        }
    } else {
        let reason = answer.get("reason").and_then(Value::as_str).unwrap_or("The user denied it");
        json!({"behavior": "deny", "message": reason})
    };
    Some(json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": decision}}))
}

/// A daemon that cannot be reached before the card is posted leaves the prompt to Claude Code; once
/// posted, a daemon restart is dialled again with the same card, which the daemon kept.
fn hold_from_hook(socket: &Path, card: &Value) -> Option<String> {
    let mut posted = false;
    loop {
        match connect(socket) {
            Ok(stream) => {
                posted = true;
                match send(stream, json!({"node_hold": card})) {
                    Ok(mut stream) => {
                        let mut answer = String::new();
                        if stream.read_to_string(&mut answer).is_ok() && !answer.is_empty() {
                            return Some(answer);
                        }
                    }
                    Err(error) if error.is::<Refused>() => return None,
                    Err(_) => {}
                }
            }
            Err(_) if !posted => return None,
            Err(_) => {}
        }
        std::thread::sleep(REDIAL);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn connect(socket: &Path) -> Result<UnixStream> {
    UnixStream::connect(socket).with_context(|| format!("cannot reach alondrad at {}", socket.display()))
}

/// Sends one frame and reads the daemon's status; what follows the status is left on the stream.
fn open(socket: &Path, frame: Value) -> Result<UnixStream> {
    send(connect(socket)?, frame)
}

fn send(mut stream: UnixStream, frame: Value) -> Result<UnixStream> {
    let body = serde_json::to_vec(&frame)?;
    stream.write_all(&(body.len() as u32).to_be_bytes())?;
    stream.write_all(&body)?;
    let mut status = [0u8; 2];
    stream.read_exact(&mut status).context("alondrad closed the connection without an answer")?;
    let status = u16::from_be_bytes(status);
    if status >= 400 {
        let mut reason = String::new();
        let _ = stream.read_to_string(&mut reason);
        return Err(Refused(format!("alondrad refused the report ({status}): {reason}")).into());
    }
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn prompt() -> Value {
        json!({"session_id": "session-1", "hook_event_name": "PermissionRequest", "permission_mode": "auto",
            "cwd": "/repo/.claude/worktrees/t-1", "tool_name": "Bash", "tool_input": {"command": "git push origin main", "description": "Push"},
            "permission_suggestions": [suggestion()]})
    }

    fn suggestion() -> Value {
        json!({"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "git push origin main"}],
            "behavior": "allow", "destination": "localSettings"})
    }

    fn scratch() -> crate::test_support::Directory {
        let dir = crate::test_support::Directory::new();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_permission_prompt_becomes_a_permission_card_withdrawn_with_its_hook() {
        assert_eq!(permission_card(&prompt(), "permission-1"), Some(json!({"session_id": "session-1", "id": "permission-1",
            "kind": "permission", "tool": "Bash", "input": {"command": "git push origin main", "description": "Push"},
            "suggestions": [suggestion()], "cwd": "/repo/.claude/worktrees/t-1", "permission_mode": "auto", "withdraw_when_gone": true})));
    }

    #[test]
    fn a_question_or_a_prompt_without_session_or_tool_is_left_to_claude_code() {
        let mut question = prompt();
        question["tool_name"] = json!("AskUserQuestion");
        let mut anonymous = prompt();
        anonymous.as_object_mut().unwrap().remove("session_id");
        let mut toolless = prompt();
        toolless.as_object_mut().unwrap().remove("tool_name");
        for payload in [question, anonymous, toolless] {
            assert_eq!(permission_card(&payload, "permission-1"), None, "{payload}");
        }
    }

    #[test]
    fn the_answer_becomes_claude_codes_permission_decision() {
        assert_eq!(permission_decision(&json!({"held": true, "allowed": true, "reason": "David allowed this call"})),
            Some(json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "allow"}}})));
        assert_eq!(permission_decision(&json!({"held": true, "allowed": false, "reason": "David denied it: not now"})),
            Some(json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest",
                "decision": {"behavior": "deny", "message": "David denied it: not now"}}})));
    }

    #[test]
    fn always_answers_allow_with_the_permission_update_to_apply() {
        assert_eq!(permission_decision(&json!({"held": true, "allowed": true, "permissions": [suggestion()]})),
            Some(json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest",
                "decision": {"behavior": "allow", "updatedPermissions": [suggestion()]}}})));
    }

    #[test]
    fn a_denial_never_carries_permission_updates() {
        let decision = permission_decision(&json!({"held": true, "allowed": false, "reason": "no", "permissions": [suggestion()]})).unwrap();
        assert_eq!(decision["hookSpecificOutput"]["decision"], json!({"behavior": "deny", "message": "no"}));
    }

    #[test]
    fn a_card_not_held_decides_nothing() {
        assert_eq!(permission_decision(&json!({"held": false})), None);
        assert_eq!(permission_decision(&json!({})), None);
    }

    #[test]
    fn no_daemon_before_the_card_is_posted_leaves_the_prompt_to_claude_code() {
        let dir = scratch();
        let card = permission_card(&prompt(), "permission-1").unwrap();
        assert_eq!(hold_from_hook(&dir.join("hooks.sock"), &card), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_refused_card_leaves_the_prompt_to_claude_code() {
        let dir = scratch();
        let socket = dir.join("hooks.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let daemon = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut len = [0u8; 4];
            stream.read_exact(&mut len).unwrap();
            stream.read_exact(&mut vec![0; u32::from_be_bytes(len) as usize]).unwrap();
            stream.write_all(&400u16.to_be_bytes()).unwrap();
            stream.write_all(b"invalid hold").unwrap();
        });
        let card = permission_card(&prompt(), "permission-1").unwrap();
        assert_eq!(hold_from_hook(&socket, &card), None);
        daemon.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_card_is_answered_on_the_hook_socket_and_posted_again_after_a_daemon_restart() {
        let dir = scratch();
        let socket = dir.join("hooks.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let daemon = std::thread::spawn(move || {
            let mut frames = vec![];
            for answer in [None, Some(br#"{"held":true,"allowed":true}"#)] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut len = [0u8; 4];
                stream.read_exact(&mut len).unwrap();
                let mut frame = vec![0; u32::from_be_bytes(len) as usize];
                stream.read_exact(&mut frame).unwrap();
                frames.push(serde_json::from_slice::<Value>(&frame).unwrap());
                if let Some(answer) = answer {
                    stream.write_all(&200u16.to_be_bytes()).unwrap();
                    stream.write_all(answer).unwrap();
                }
            }
            frames
        });
        let card = permission_card(&prompt(), "permission-1").unwrap();
        let answer = hold_from_hook(&socket, &card).unwrap();
        assert_eq!(daemon.join().unwrap(), vec![json!({"node_hold": card}), json!({"node_hold": card})]);
        assert_eq!(permission_decision(&serde_json::from_str(&answer).unwrap()),
            Some(json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "allow"}}})));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
