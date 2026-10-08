use std::io::{Read, Write};
#[path = "../../test_support.rs"]
mod test_support;

use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};

fn cron_create_refusal(orchestrator: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_alondra"));
    command.arg("hook").env("ALONDRA_LAUNCHED", "1").env_remove("ALONDRA_ORCHESTRATOR_EXECUTION");
    if orchestrator { command.env("ALONDRA_ORCHESTRATOR_EXECUTION", "run-1"); }
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(br#"{"hook_event_name":"PreToolUse","tool_name":"CronCreate","tool_input":{"cron":"0 9 * * 1"}}"#).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["hookSpecificOutput"]["permissionDecision"], "deny");
    reply["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().to_owned()
}

#[test]
fn the_orchestrator_is_sent_to_schedule_create_and_a_node_to_the_orchestrator() {
    assert!(cron_create_refusal(true).contains("schedule_create"));
    let node = cron_create_refusal(false);
    assert!(!node.contains("schedule_create"), "{node}");
    assert!(node.contains("ask") && node.contains("orchestrator"), "{node}");
}

#[test]
fn hook_cli_sends_json_to_local_daemon_and_returns_its_reply() {
    let scratch_directory = crate::test_support::Directory::new();
    let home = scratch_directory.parent().unwrap().join(format!("alondra-cli-hook-{}", std::process::id()));
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
    let scratch_directory = crate::test_support::Directory::new();
    let home = scratch_directory.parent().unwrap().join(format!("alondra-no-socket-{}", std::process::id()));
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
    let scratch_directory = crate::test_support::Directory::new();
    let home = scratch_directory.parent().unwrap().join(format!("alondra-hook-{name}-{}", std::process::id()));
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
fn a_subagent_runs_without_alondra_whether_or_not_alondra_launched_its_session() {
    for (name, launched) in [("agent", true), ("agent-own", false)] {
        let output = run_hook(name, Daemon::Missing, launched, AGENT);
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stdout.is_empty(), "it works on its node's one task and needs no task token");
    }
}

#[test]
fn post_tool_hook_sends_only_the_last_stdout_line() {
    let scratch_directory = crate::test_support::Directory::new();
    let home = scratch_directory.parent().unwrap().join(format!("alondra-compact-hook-{}", std::process::id()));
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

/// The node mod starts its helper again unless it ended by SIGTERM: a daemon that goes away, or
/// one that is not there yet, must end the helper with an exit code, never a signal.
#[test]
fn node_connect_ends_with_an_exit_code_when_the_daemon_goes_away_or_is_not_there() {
    let scratch_directory = crate::test_support::Directory::new();
    use std::os::unix::process::ExitStatusExt;
    let home = scratch_directory.parent().unwrap().join(format!("alondra-cli-connect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let connect = || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_alondra"))
            .args(["node", "connect"])
            .env("ALONDRA_HOME", &home)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().unwrap();
        child.stdin.take().unwrap().write_all(br#"{"session_id":"s","version":"2.1.287"}"#).unwrap();
        child
    };

    let absent = connect().wait_with_output().unwrap();
    assert_eq!((absent.status.code(), absent.status.signal()), (Some(1), None), "no daemon yet");

    let listener = UnixListener::bind(home.join("hooks.sock")).unwrap();
    let daemon = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut size = [0; 4];
        stream.read_exact(&mut size).unwrap();
        let mut bytes = vec![0; u32::from_be_bytes(size) as usize];
        stream.read_exact(&mut bytes).unwrap();
        let sent: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(sent["node_connect"]["session_id"], "s");
        stream.write_all(&200u16.to_be_bytes()).unwrap();
    });
    let held = connect().wait_with_output().unwrap();
    daemon.join().unwrap();
    assert_eq!(String::from_utf8_lossy(&held.stdout), "connected\n");
    assert_eq!((held.status.code(), held.status.signal()), (Some(3), None), "the daemon went away");
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn a_permission_prompt_is_held_as_a_card_and_answered_with_claude_codes_decision_or_left_to_claude_code() {
    let scratch_directory = crate::test_support::Directory::new();
    let home = scratch_directory.parent().unwrap().join(format!("alondra-cli-permission-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let prompt = br#"{"session_id":"s","hook_event_name":"PermissionRequest","permission_mode":"auto","tool_name":"Bash","tool_input":{"command":"git push"}}"#;
    let hook = || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_alondra"))
            .arg("hook")
            .env("ALONDRA_HOME", &home)
            .env("ALONDRA_LAUNCHED", "1")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().unwrap();
        child.stdin.take().unwrap().write_all(prompt).unwrap();
        child.wait_with_output().unwrap()
    };

    let absent = hook();
    assert!(absent.status.success());
    assert!(absent.stdout.is_empty(), "no daemon decides nothing: {}", String::from_utf8_lossy(&absent.stdout));

    let listener = UnixListener::bind(home.join("hooks.sock")).unwrap();
    let daemon = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut size = [0; 4];
        stream.read_exact(&mut size).unwrap();
        let mut bytes = vec![0; u32::from_be_bytes(size) as usize];
        stream.read_exact(&mut bytes).unwrap();
        let sent: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        stream.write_all(&200u16.to_be_bytes()).unwrap();
        stream.write_all(br#"{"held":true,"allowed":false,"reason":"David denies it"}"#).unwrap();
        sent
    });
    let answered = hook();
    let card = daemon.join().unwrap()["node_hold"].take();
    assert!(answered.status.success());
    assert_eq!((card["session_id"].as_str(), card["kind"].as_str(), card["tool"].as_str()), (Some("s"), Some("permission"), Some("Bash")));
    assert_eq!(card["input"], serde_json::json!({"command": "git push"}));
    assert_eq!(card["withdraw_when_gone"], true);
    let decision: serde_json::Value = serde_json::from_slice(&answered.stdout).unwrap();
    assert_eq!(decision, serde_json::json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest",
        "decision": {"behavior": "deny", "message": "David denies it"}}}));
    std::fs::remove_dir_all(home).unwrap();
}
