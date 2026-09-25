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

#[test]
fn a_guard_hook_blocks_when_the_daemon_is_unavailable() {
    let home = std::env::temp_dir().join(format!("alondra-missing-hook-socket-{}", std::process::id()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_alondra"))
        .arg("hook")
        .env("ALONDRA_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(br#"{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Bash"}"#).unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot validate this action"));
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
