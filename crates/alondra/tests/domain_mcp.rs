//! Exercise the two MCP entrypoints over JSON-RPC and their shared domain HTTP adapter.
//! The local HTTP peer is synthetic; domain authorization is tested by alondrad.
#[path = "../../test_support.rs"]
mod test_support;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const BEARER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn request_body(mut stream: &std::net::TcpStream) -> (String, Value) {
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
    let request_line = headers.lines().next().unwrap().to_owned();
    assert!(headers.contains(&format!("authorization: bearer {BEARER}\r\n")));
    let length: usize = headers.lines().find_map(|line| line.strip_prefix("content-length: "))
        .map(|value| value.trim().parse().unwrap()).unwrap_or(0);
    while bytes.len() - header_end < length {
        let mut chunk = [0; 4096];
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "HTTP request ended before its body");
        bytes.extend_from_slice(&chunk[..n]);
    }
    let body = if length == 0 { Value::Null } else {
        serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
    };
    (request_line, body)
}

fn respond(mut stream: std::net::TcpStream, body: &[u8]) {
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
    stream.write_all(body).unwrap();
}

fn mcp_exchange(entrypoint: &str, tool: &str, arguments: Value) -> (Vec<Value>, (String, Value)) {
    let scratch_directory = crate::test_support::Directory::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let capabilities = request_body(&stream);
        assert_eq!(capabilities.0, "get /capabilities http/1.1");
        respond(stream, br#"{"data_epoch":"epoch"}"#);
        let (stream, _) = listener.accept().unwrap();
        let request = request_body(&stream);
        let response = br#"{"revision":2,"changed":true,"effects":[],"created_ids":[]}"#;
        respond(stream, response);
        request
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_alondra"))
        .arg(entrypoint)
        .env("ALONDRA_HOME", scratch_directory.parent().unwrap().join(format!("alondra-domain-mcp-{}-{port}", std::process::id())))
        .env("ALONDRA_PORT", port.to_string())
        .env("ALONDRA_DOMAIN_CAPABILITY", BEARER)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let stdin = child.stdin.as_mut().unwrap();
    for request in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":tool,"arguments":arguments}}),
    ] { writeln!(stdin, "{request}").unwrap(); }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let replies: Vec<Value> = output.stdout.split(|byte| *byte == b'\n').filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap()).collect();
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
            "task_id":"task-one", "host":"github.com",
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
    assert_eq!(node_command.1["task_id"], "task-one");
    assert_eq!(node_command.0, "post /tasks/task-one/pull-requests http/1.1");
    assert_eq!(node_command.1["data_epoch"], "epoch");
    assert_eq!(node_command.1["expected_revision"], 0);
    assert!(node_command.1["command_id"].as_str().unwrap().starts_with("mcp-"));
    assert_eq!(node[2]["result"]["content"][0]["text"], "Recorded.");
    assert!(!node[2].to_string().contains("revision"));

    let (orchestrator, manager_command) = mcp_exchange(
        "mcp-orchestrator",
        "create_request",
        json!({
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
    assert_eq!(manager_command.0, "post /requests http/1.1");
    assert_eq!(manager_command.1["body"], "Investigate");
    assert!(manager_command.1["command_id"].as_str().unwrap().starts_with("mcp-"));
}

#[test]
fn named_tools_construct_commands_and_publish_separate_catalogs() {
        let (_, forwarded) = mcp_exchange(
        "mcp-orchestrator",
        "create_task",
        json!({
            "task_id":"task-one", "request_id":"request-one", "title":"Prepare result",
            "agent_id":"agent-one", "review_prs":false, "dependencies":[], "stack":null,
            "launch":{"name":"project/result", "cwd":"/Users/example/Projects/project"}
        }),
    );
    assert_eq!(forwarded.0, "post /requests/request-one/tasks http/1.1");
    assert!(forwarded.1["command_id"].as_str().unwrap().starts_with("mcp-"));
    assert_eq!(forwarded.1["title"], "Prepare result");

    let (node, forwarded) = mcp_exchange(
        "mcp-node",
        "create_document",
        json!({
            "task_id":"task-one", "document_id":"document-one", "title":"Design",
            "revision_id":"revision-one", "content_digest":"a".repeat(64),
            "content_reference":"files/source", "role":"result"
        }),
    );
    assert_eq!(forwarded.0, "post /tasks/task-one/documents http/1.1");
    assert_eq!(forwarded.1["document_id"], "document-one");
    let names: Vec<_> = node[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"present_result") && !names.contains(&"complete_task") && !names.contains(&"claim_merge_authorization"));
    assert!(!names.contains(&"domain_command") && !names.contains(&"create_task"));

    let (_, forwarded) = mcp_exchange(
        "mcp-node",
        "ask",
        json!({
            "interaction_id":"question-one", "scope":{"kind":"task", "task_id":"task-one"},
            "recipient":{"kind":"user"}, "need":"Pick the answer", "why":"The task cannot go on without it",
            "blocking":true, "recommended":"yes",
            "options":[{"id":"yes", "label":"Yes"}, {"id":"no", "label":"No"}]
        }),
    );
    assert_eq!(forwarded.0, "post /questions http/1.1");
    assert_eq!(forwarded.1["card"]["kind"], "decide");
    assert_eq!(forwarded.1["card"]["need"], "Pick the answer");
    assert_eq!(forwarded.1["card"]["recommended"], "yes");
    assert!(forwarded.1.get("need").is_none() && forwarded.1.get("why").is_none());
    assert_eq!(forwarded.1["command_id"], "mcp-ask-question-one", "a card is retried by its own id");
    assert!(forwarded.1["command_id"].as_str().unwrap().starts_with("mcp-"));
    assert_eq!(forwarded.1["interaction_id"], "question-one");
    assert!(forwarded.1.get("kind").is_none() && forwarded.1.get("protocol_version").is_none());

    let (_, forwarded) = mcp_exchange(
        "mcp-node",
        "present_result",
        json!({
            "task_id":"task-one", "interaction_id":"answer-delivery", "need":"Review the answer",
            "content":{"kind":"report", "body":"The requested answer"},
            "targets":[{"kind":"text", "text":"The requested answer"}], "is_final":true
        }),
    );
    assert_eq!(forwarded.0, "post /tasks/task-one/results http/1.1");
    assert!(forwarded.1["command_id"].as_str().unwrap().starts_with("mcp-"));
    assert_eq!(forwarded.1["body"], "The requested answer");
    assert_eq!(forwarded.1["card"]["kind"], "review");
    assert!(forwarded.1.get("kind").is_none() && forwarded.1.get("protocol_version").is_none());
}

#[test]
fn mcp_without_a_producer_capability_fails_closed() {
    let scratch_directory = crate::test_support::Directory::new();
    let mut child = Command::new(env!("CARGO_BIN_EXE_alondra"))
        .arg("mcp-node")
        .env(
            "ALONDRA_HOME",
            scratch_directory.parent().unwrap().join(format!("alondra-domain-mcp-missing-{}", std::process::id())),
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
                "task_id":"task-one", "host":"github.com",
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

/// One MCP session whose tool calls reach a daemon that answers each request with `reply`;
/// returns every reply to a tool call and the commands the daemon received.
fn mcp_session(calls: &[(&str, Value)], reply: impl Fn(&str) -> (u16, Value) + Send + 'static) -> (Vec<Value>, Vec<(String, Value)>) {
    let scratch_directory = crate::test_support::Directory::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = stop.clone();
    let server = std::thread::spawn(move || {
        let mut received = vec![];
        while !stopped.load(std::sync::atomic::Ordering::SeqCst) {
            let Ok((stream, _)) = listener.accept() else {
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            };
            stream.set_nonblocking(false).unwrap();
            let (line, body) = request_body(&stream);
            let path = line.split_whitespace().nth(1).unwrap().to_owned();
            let (status, answer) = reply(&path);
            let bytes = answer.to_string();
            let mut stream = stream;
            write!(stream, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{bytes}", bytes.len()).unwrap();
            if path != "/capabilities" {
                received.push((path, body));
            }
        }
        received
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_alondra"))
        .arg("mcp-node")
        .env("ALONDRA_HOME", scratch_directory.parent().unwrap().join(format!("alondra-domain-mcp-session-{}-{port}", std::process::id())))
        .env("ALONDRA_PORT", port.to_string())
        .env("ALONDRA_DOMAIN_CAPABILITY", BEARER)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let stdin = child.stdin.as_mut().unwrap();
    for (index, (tool, arguments)) in calls.iter().enumerate() {
        writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":index + 1,"method":"tools/call","params":{"name":tool,"arguments":arguments}})).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let replies = output.stdout.split(|byte| *byte == b'\n').filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap()["result"].clone()).collect();
    (replies, server.join().unwrap())
}

fn needy_ask(id: &str) -> Value {
    json!({
        "interaction_id":id, "scope":{"kind":"task", "task_id":"task-one"}, "recipient":{"kind":"user"},
        "why":"It holds the release back", "blocking":true, "options":[{"id":"a", "label":"A"}, {"id":"b", "label":"B"}]
    })
}

/// What the node of the incident sent on 23 October: its card, as text, under a field
/// present_result does not have, and no need.
fn incident_result() -> Value {
    json!({"task_id":"task-one", "interaction_id":"task-one-result", "is_final":true,
        "body":"PR #38 adds AGENTS.md", "targets":[{"kind":"pull_request"}],
        "card":"{\"need\":\"Review simpool PR 38\",\"why\":\"Agents finish the rollout themselves\"}"})
}

#[test]
fn arguments_that_do_not_fit_the_schema_are_refused_by_name_and_never_reach_the_daemon() {
    let mut content_as_text = needy_ask("card-2");
    content_as_text["content"] = json!("{\"kind\":\"report\",\"body\":\"The log\"}");
    let mut options_as_text = needy_ask("card-3");
    options_as_text["options"] = json!("[{\"id\":\"a\",\"label\":\"A\"}]");
    let mut scope_as_text = needy_ask("card-4");
    scope_as_text["scope"] = json!("task");
    let mut recipient_as_text = needy_ask("card-5");
    recipient_as_text["recipient"] = json!("{\"kind\":\"user\"}");
    let mut targets_as_text = incident_result();
    targets_as_text.as_object_mut().unwrap().remove("card");
    targets_as_text["need"] = json!("Review simpool PR 38");
    targets_as_text["targets"] = json!("[{\"kind\":\"pull_request\"}]");
    let calls = [
        ("present_result", incident_result()),
        ("ask", content_as_text),
        ("ask", options_as_text),
        ("ask", scope_as_text),
        ("ask", recipient_as_text),
        ("present_result", targets_as_text),
        ("register_pr", json!({})),
    ];
    let (replies, received) = mcp_session(&calls, |_| (200, json!({"data_epoch":"epoch"})));
    let texts: Vec<&str> = replies.iter().map(|reply| {
        assert_eq!(reply["isError"], true, "{reply}");
        reply["content"][0]["text"].as_str().unwrap()
    }).collect();
    assert!(texts[0].contains("it has no field `card`: its fields are") && ["need", "why", "content"].iter().all(|field| texts[0].contains(field)), "{}", texts[0]);
    assert!(!texts[0].contains("card.need"), "the refusal names the field that is wrong, not one the node never sent: {}", texts[0]);
    for (text, field, shape) in [(texts[1], "content", "an object"), (texts[2], "options", "a list"), (texts[3], "scope", "an object"),
        (texts[4], "recipient", "an object"), (texts[5], "targets", "a list")] {
        assert!(text.contains(&format!("`{field}` must be {shape}, got a string: send it as JSON itself, not as text that holds JSON")), "{text}");
    }
    assert!(texts[6].contains("missing `task_id`, `host`, `repository`, `number`, `head`, `base`, `allowed_methods`"), "{}", texts[6]);
    for text in &texts {
        assert!(!text.contains("pr_id") && !text.contains("filled-by-alondra") && !text.contains("expected one of"), "no internal field leaks: {text}");
    }
    assert!(received.is_empty(), "nothing reached alondrad, so no refusal counts against a card: {received:?}");
}

#[test]
fn a_refused_card_tells_the_node_the_field_and_the_fix_and_grants_nothing() {
    let refusal = json!({"code":"invalid_input", "message":"card.need: Say in one line, in the imperative, what you need from the user",
        "current_revision":null, "subject":"card.need"});
    let (replies, received) = mcp_session(&[("ask", needy_ask("card-1"))], move |path| {
        if path == "/capabilities" { (200, json!({"data_epoch":"epoch"})) } else { (422, refusal.clone()) }
    });
    assert_eq!(replies[0]["isError"], true);
    let text = replies[0]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("invalid_input") && text.contains("card.need") && text.contains("what you need from the user"), "{text}");
    assert!(text.contains("never an answer or a permission"), "{text}");
    assert_eq!(received[0].1["card"]["need"], "", "the missing need is refused by the daemon, not invented by the producer");
}

#[test]
fn a_retry_after_a_timeout_is_the_same_command() {
    let (replies, received) = mcp_session(&[("ask", needy_ask("card-1")), ("ask", needy_ask("card-1"))], |path| {
        if path == "/capabilities" { (200, json!({"data_epoch":"epoch"})) } else { (200, json!({"revision":2,"changed":true,"effects":[],"created_ids":["card-1"]})) }
    });
    assert!(replies.iter().all(|reply| reply["isError"] != true), "{replies:?}");
    assert_eq!(received.len(), 2);
    assert_eq!(received[0].1["command_id"], received[1].1["command_id"]);
    assert_eq!(received[0].1, received[1].1);
}

#[test]
fn a_daemon_that_is_down_says_not_to_proceed() {
    let scratch_directory = crate::test_support::Directory::new();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_alondra"))
        .arg("mcp-node")
        .env("ALONDRA_HOME", scratch_directory.parent().unwrap().join(format!("alondra-domain-mcp-down-{}-{port}", std::process::id())))
        .env("ALONDRA_PORT", port.to_string())
        .env("ALONDRA_DOMAIN_CAPABILITY", BEARER)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    writeln!(child.stdin.as_mut().unwrap(), "{}",
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ask","arguments":needy_ask("card-1")}})).unwrap();
    let output = child.wait_with_output().unwrap();
    let reply: Value = serde_json::from_slice(output.stdout.trim_ascii()).unwrap();
    assert_eq!(reply["result"]["isError"], true);
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("don't proceed: could not reach Alondra;") && text.contains("unconfirmed") && text.contains("durable outcome") && !text.contains("Nothing was sent"), "{text}");
    assert!(!text.contains("Recorded"), "{text}");
}
