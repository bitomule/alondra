use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};

#[test]
fn managed_sessions_redirect_cron_create_to_alondra_schedules() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_alondra"))
        .arg("hook")
        .env("ALONDRA_LAUNCHED", "1")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(br#"{"hook_event_name":"PreToolUse","tool_name":"CronCreate","tool_input":{"cron":"0 9 * * 1"}}"#).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(reply["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().contains("schedule_create"));
}

#[test]
fn hook_cli_sends_json_to_local_daemon_and_returns_its_reply() {
    let home = std::env::temp_dir().join(format!("alondra-cli-hook-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let listener = UnixListener::bind(home.join("hooks.sock")).unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut size = [0; 4];
        stream.read_exact(&mut size).unwrap();
        let mut bytes = Vec::new();
        bytes.resize(u32::from_be_bytes(size) as usize, 0);
        stream.read_exact(&mut bytes).unwrap();
        let sent: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(sent["launched"], true);
        assert_eq!(sent["payload"]["hook_event_name"], "SessionStart");
        stream.write_all(&200u16.to_be_bytes()).unwrap();
        stream.write_all(br#"{"hookSpecificOutput":{"hookEventName":"SessionStart"}}"#).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut ack = [0];
        stream.read_exact(&mut ack).unwrap();
        assert_eq!(ack, [6]);
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_alondra"))
        .arg("hook")
        .env("ALONDRA_HOME", &home)
        .env("ALONDRA_LAUNCHED", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(br#"{"session_id":"s","hook_event_name":"SessionStart"}"#).unwrap();
    let output = child.wait_with_output().unwrap();
    handle.join().unwrap();
    assert!(output.status.success());
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["hookSpecificOutput"]["hookEventName"], "SessionStart");
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn intermediate_message_batch_does_not_contact_daemon() {
    let home = std::env::temp_dir().join(format!("alondra-no-socket-{}", std::process::id()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_alondra"))
        .arg("hook")
        .env("ALONDRA_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(br#"{"session_id":"s","hook_event_name":"MessageDisplay","final":false}"#).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"{}\n");
}

enum Daemon {
    Missing,
    ClosesWithoutReply,
    Replies(u16, &'static str),
    RepliesAndDropsAck(&'static str),
    RepliesAndAwaitsAck(&'static str),
}

fn run_hook(name: &str, daemon: Daemon, launched: bool, input: &[u8]) -> std::process::Output {
    let home = std::env::temp_dir().join(format!("alondra-hook-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let server = match daemon {
        Daemon::Missing => None,
        daemon => {
            let listener = UnixListener::bind(home.join("hooks.sock")).unwrap();
            Some(std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut size = [0; 4];
                stream.read_exact(&mut size).unwrap();
                let mut bytes = vec![0; u32::from_be_bytes(size) as usize];
                stream.read_exact(&mut bytes).unwrap();
                match daemon {
                    Daemon::Missing | Daemon::ClosesWithoutReply => (),
                    Daemon::Replies(status, body) => {
                        stream.write_all(&status.to_be_bytes()).unwrap();
                        stream.write_all(body.as_bytes()).unwrap();
                        stream.shutdown(std::net::Shutdown::Write).unwrap();
                        let _ = stream.read(&mut [0]);
                    }
                    Daemon::RepliesAndAwaitsAck(body) => {
                        stream.write_all(&200u16.to_be_bytes()).unwrap();
                        stream.write_all(body.as_bytes()).unwrap();
                        stream.set_read_timeout(Some(std::time::Duration::from_secs(30))).unwrap();
                        let mut ack = [0];
                        stream.read_exact(&mut ack).expect("the hook acknowledges a complete reply while the connection stays open");
                        assert_eq!(ack, [6]);
                    }
                    Daemon::RepliesAndDropsAck(body) => {
                        stream.write_all(&200u16.to_be_bytes()).unwrap();
                        stream.write_all(body.as_bytes()).unwrap();
                        stream.shutdown(std::net::Shutdown::Both).unwrap();
                    }
                }
            }))
        }
    };
    let mut command = Command::new(env!("CARGO_BIN_EXE_alondra"));
    command.arg("hook").env("ALONDRA_HOME", &home).env_remove("ALONDRA_LAUNCHED");
    if launched { command.env("ALONDRA_LAUNCHED", "1"); }
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    if let Some(server) = server { server.join().unwrap(); }
    std::fs::remove_dir_all(home).unwrap();
    output
}

const BASH: &[u8] = br#"{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Bash"}"#;
const AGENT: &[u8] = br#"{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Agent","tool_input":{"prompt":"x"}}"#;

#[test]
fn claude_keeps_working_when_the_daemon_is_not_running() {
    let output = run_hook("missing", Daemon::Missing, true, BASH);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("did not answer"));
}

#[test]
fn claude_keeps_working_when_the_daemon_fails() {
    let output = run_hook("503", Daemon::Replies(503, "store unavailable"), false, BASH);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
}

#[test]
fn claude_keeps_working_when_the_daemon_hangs_up() {
    let output = run_hook("hangup", Daemon::ClosesWithoutReply, false, BASH);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
}

#[test]
fn every_event_continues_quietly_without_the_daemon() {
    let output = run_hook("post", Daemon::Missing, false,
        br#"{"session_id":"s","hook_event_name":"PostToolUse","tool_name":"Bash"}"#);
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn a_deliberate_refusal_still_blocks_the_tool() {
    let output = run_hook("409", Daemon::Replies(409, "stale orchestrator run"), true, BASH);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("stale orchestrator run"));
}

#[test]
fn a_printed_reply_stands_when_its_acknowledgement_is_lost() {
    let reply = r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}"#;
    let output = run_hook("ack", Daemon::RepliesAndDropsAck(reply), true, BASH);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout), reply);
}

#[test]
fn a_complete_reply_is_printed_and_acknowledged_before_the_daemon_closes() {
    let reply = r#"{"hookSpecificOutput":{"hookEventName":"Stop","additionalContext":"Yes."}}"#;
    let output = run_hook("open", Daemon::RepliesAndAwaitsAck(reply), true,
        br#"{"session_id":"s","hook_event_name":"Stop"}"#);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout), reply);
}

#[test]
fn a_launched_subagent_waits_for_alondra_to_get_its_task_token() {
    let output = run_hook("agent", Daemon::Missing, true, AGENT);
    assert_eq!(output.status.code(), Some(0));
    let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(reply["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().contains("not answering"));
}

#[test]
fn a_subagent_outside_alondra_runs_without_it() {
    let output = run_hook("agent-own", Daemon::Missing, false, AGENT);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
}

#[test]
fn post_tool_hook_sends_only_the_last_stdout_line() {
    let home = std::env::temp_dir().join(format!("alondra-compact-hook-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let listener = UnixListener::bind(home.join("hooks.sock")).unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut size = [0; 4];
        stream.read_exact(&mut size).unwrap();
        let mut bytes = vec![0; u32::from_be_bytes(size) as usize];
        stream.read_exact(&mut bytes).unwrap();
        let sent: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(sent["payload"]["tool_response"]["stdout"], "https://github.com/example/repo/pull/1");
        stream.write_all(&200u16.to_be_bytes()).unwrap();
        stream.write_all(b"{}").unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut ack = [0];
        stream.read_exact(&mut ack).unwrap();
        assert_eq!(ack, [6]);
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_alondra"))
        .arg("hook").env("ALONDRA_HOME", &home)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(br#"{"session_id":"s","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"gh pr create"},"tool_response":{"stdout":"long log\nhttps://github.com/example/repo/pull/1\n"}}"#).unwrap();
    let output = child.wait_with_output().unwrap();
    handle.join().unwrap();
    assert!(output.status.success());
    std::fs::remove_dir_all(home).unwrap();
}
