//! Exercise the two MCP entrypoints over JSON-RPC and their shared domain HTTP adapter.
//! The local HTTP peer is synthetic; domain authorization is tested by alondrad.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const BEARER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn request_body(mut stream: &std::net::TcpStream) -> Value {
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0; 4096];
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "HTTP request ended before its headers");
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]).to_ascii_lowercase();
    assert!(
        headers.starts_with("post /v2/domain/commands http/1.1\r\n"),
        "{headers}"
    );
    assert!(headers.contains(&format!("authorization: bearer {BEARER}\r\n")));
    let length: usize = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    while bytes.len() - header_end < length {
        let mut chunk = [0; 4096];
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "HTTP request ended before its body");
        bytes.extend_from_slice(&chunk[..n]);
    }
    serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
}

fn mcp_exchange(entrypoint: &str, tool: &str, arguments: Value) -> (Vec<Value>, Value) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // No wall-clock deadline: under Rosetta the MCP process can take seconds to start. If it
    // exits without calling, the assertions below fail before this thread is joined.
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let body = request_body(&stream);
        let response = br#"{"revision":2,"changed":true,"effects":[],"created_ids":[]}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).unwrap();
        stream.write_all(response).unwrap();
        body
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_alondra"))
        .arg(entrypoint)
        .env(
            "ALONDRA_HOME",
            std::env::temp_dir().join(format!("alondra-domain-mcp-{}-{port}", std::process::id())),
        )
        .env("ALONDRA_PORT", port.to_string())
        .env("ALONDRA_DOMAIN_CAPABILITY", BEARER)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.as_mut().unwrap();
    for request in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":tool,"arguments":arguments}}),
    ] {
        writeln!(stdin, "{request}").unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let replies: Vec<Value> = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 3);
    assert_ne!(replies[2]["result"]["isError"], true, "{}", replies[2]);
    (replies, server.join().unwrap())
}

#[test]
fn distinct_mcp_servers_forward_named_commands_with_the_existing_capability() {
    let (node, node_command) = mcp_exchange(
        "mcp-node",
        "register_pr",
        json!({
            "data_epoch":"epoch", "command_id":"register-one", "expected_revision":1,
            "task_id":"task-one", "pr_id":"pr-one", "host":"github.com",
            "repository":"example/project", "number":7, "head":"abc123", "base":"main",
            "allowed_methods":["squash"]
        }),
    );
    assert_eq!(node[0]["result"]["serverInfo"]["name"], "alondra-node");
    assert!(
        node[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "register_pr")
    );
    assert_eq!(node_command["command"]["kind"], "register_pr");
    assert_eq!(node_command["command"]["task_id"], "task-one");
    assert_eq!(node_command["protocol_version"], 2);

    let (orchestrator, manager_command) = mcp_exchange(
        "mcp-orchestrator",
        "create_request",
        json!({
            "data_epoch":"epoch", "command_id":"request-one", "expected_revision":1,
            "request_id":"request-one", "body":"Investigate"
        }),
    );
    assert_eq!(
        orchestrator[0]["result"]["serverInfo"]["name"],
        "alondra-orchestrator"
    );
    assert!(
        !orchestrator[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "register_pr")
    );
    assert_eq!(manager_command["command"]["kind"], "create_request");
    assert_eq!(manager_command["command_id"], "request-one");
}

#[test]
fn named_tools_construct_commands_and_publish_separate_catalogs() {
    let task = json!({
        "protocol_version":2, "data_epoch":"epoch", "command_id":"task-one",
        "expected_revision":2, "command":{"kind":"create_task", "task_id":"task-one",
            "request_id":"request-one", "title":"Prepare result", "agent_id":"agent-one",
            "review_prs":false, "dependencies":[], "stack":null}
    });
    let (_, forwarded) = mcp_exchange(
        "mcp-orchestrator",
        "create_task",
        json!({
            "data_epoch":"epoch", "command_id":"task-one", "expected_revision":2,
            "task_id":"task-one", "request_id":"request-one", "title":"Prepare result",
            "agent_id":"agent-one", "review_prs":false, "dependencies":[], "stack":null
        }),
    );
    assert_eq!(forwarded, task);

    let document = json!({
        "protocol_version":2, "data_epoch":"epoch", "command_id":"document-one",
        "expected_revision":4, "command":{"kind":"create_document", "task_id":"task-one",
            "document_id":"document-one", "title":"Design", "revision_id":"revision-one",
            "content_digest":"a".repeat(64), "content_reference":"files/source", "role":"result"}
    });
    let (node, forwarded) = mcp_exchange(
        "mcp-node",
        "create_document",
        json!({
            "data_epoch":"epoch", "command_id":"document-one", "expected_revision":4,
            "task_id":"task-one", "document_id":"document-one", "title":"Design",
            "revision_id":"revision-one", "content_digest":"a".repeat(64),
            "content_reference":"files/source", "role":"result"
        }),
    );
    assert_eq!(forwarded, document);
    let names: Vec<_> = node[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"present_result") && names.contains(&"complete_task"));
    assert!(!names.contains(&"domain_command") && !names.contains(&"create_task"));

    let question = json!({
        "protocol_version":2, "data_epoch":"epoch", "command_id":"question-one",
        "expected_revision":5, "command":{
            "kind":"ask_question", "interaction_id":"question-one",
            "scope":{"kind":"task", "task_id":"task-one"},
            "recipient":{"kind":"user"}, "body":"Which answer?", "blocking":true,
            "options":[{"id":"yes", "label":"Yes", "inputs":[]}]
        }
    });
    let (_, forwarded) = mcp_exchange(
        "mcp-node",
        "ask",
        json!({
            "data_epoch":"epoch", "command_id":"question-one", "expected_revision":5,
            "interaction_id":"question-one", "scope":{"kind":"task", "task_id":"task-one"},
            "recipient":{"kind":"user"}, "body":"Which answer?", "blocking":true,
            "options":[{"id":"yes", "label":"Yes", "inputs":[]}]
        }),
    );
    assert_eq!(forwarded, question);

    let answer = json!({
        "protocol_version":2, "data_epoch":"epoch", "command_id":"answer-one",
        "expected_revision":7, "command":{
            "kind":"present_result", "task_id":"task-one",
            "interaction_id":"answer-delivery", "body":"The requested answer",
            "targets":[{"kind":"text", "text":"The requested answer"}], "is_final":true
        }
    });
    let (_, forwarded) = mcp_exchange(
        "mcp-node",
        "present_result",
        json!({
            "data_epoch":"epoch", "command_id":"answer-one", "expected_revision":7,
            "task_id":"task-one", "interaction_id":"answer-delivery", "body":"The requested answer",
            "targets":[{"kind":"text", "text":"The requested answer"}], "is_final":true
        }),
    );
    assert_eq!(forwarded, answer);
}

#[test]
fn v2_mcp_without_a_producer_capability_fails_closed() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_alondra"))
        .arg("mcp-node")
        .env(
            "ALONDRA_HOME",
            std::env::temp_dir().join(format!("alondra-domain-mcp-missing-{}", std::process::id())),
        )
        .env_remove("ALONDRA_DOMAIN_CAPABILITY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        child.stdin.as_mut().unwrap(),
        "{}",
        json!({
            "jsonrpc":"2.0", "id":1, "method":"tools/call",
            "params":{"name":"register_pr","arguments":{
                "data_epoch":"epoch", "command_id":"one", "expected_revision":0,
                "task_id":"task-one", "pr_id":"pr-one", "host":"github.com",
                "repository":"example/project", "number":7, "head":"abc123", "base":"main",
                "allowed_methods":["squash"]
            }}
        })
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let reply: Value = serde_json::from_slice(output.stdout.trim_ascii()).unwrap();
    assert_eq!(reply["result"]["isError"], true);
    assert!(
        reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("domain producer capability unavailable")
    );
}
