//! Exercise the two MCP entrypoints over JSON-RPC and their shared domain HTTP adapter.
//! The local HTTP peer is synthetic; domain authorization is tested by alondrad.
#[path = "../../test_support.rs"]
mod test_support;

use std::io::{BufRead, Read, Write};
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
    mcp_exchange_with_response(entrypoint, tool, arguments, json!({"revision":2,"changed":true,"effects":[],"created_ids":[],"effect":["PR github.com/example/project#7 registered."]}))
}

fn mcp_exchange_with_response(entrypoint: &str, tool: &str, arguments: Value, response: Value) -> (Vec<Value>, (String, Value)) {
    let scratch_directory = crate::test_support::Directory::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let first = request_body(&stream);
        let (stream, request) = if first.0 == "get /capabilities http/1.1" {
            respond(stream, br#"{"data_epoch":"epoch"}"#);
            let (stream, _) = listener.accept().unwrap();
            let request = request_body(&stream);
            (stream, request)
        } else { (stream, first) };
        respond(stream, response.to_string().as_bytes());
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

fn tool_json(replies: &[Value]) -> Value {
    assert!(serde_json::to_vec(&replies[2]).unwrap().len() <= 24 * 1024, "complete JSON-RPC reply exceeds budget");
    serde_json::from_str(replies[2]["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[test]
fn orchestrator_overview_is_bounded_over_the_actual_mcp_transport() {
    let tasks: Vec<_> = (0..500).map(|n| json!({"id":format!("t-{n:04}"),"title":"猫🙂\\\"".repeat(200),
        "state":"working","repo":"example/project","request":"r-work"})).collect();
    let raw = json!({"tasks":tasks,"requests":[{"id":"r-open","state":"defining","text":"Unassigned request"},
        {"id":"r-work","state":"in_progress","text":"Work"}]});
    assert!(serde_json::to_vec(&raw).unwrap().len() > 24 * 1024);
    for all in [false, true] {
        let (replies, request) = mcp_exchange_with_response("mcp-orchestrator", "tasks_in_progress", json!({"all":all}), raw.clone());
        assert_eq!(request.0, "get /orchestrator/tasks_in_progress http/1.1");
        let page = tool_json(&replies);
        assert_eq!(page["tasks"]["total"], 500);
        assert!(page["tasks"]["items"].as_array().unwrap().len() < 500);
        assert_eq!(page["requests"]["items"][0]["id"], "r-open");
        assert!(page["requests"]["items"][1].get("tasks").is_none());
        let mut args = json!({"section":"tasks","all":all});
        let mut seen = std::collections::BTreeSet::new();
        loop {
            let (replies, _) = mcp_exchange_with_response("mcp-orchestrator", "tasks_in_progress", args, raw.clone());
            let page = tool_json(&replies);
            for row in page["items"].as_array().unwrap() {
                assert!(seen.insert(row["id"].as_str().unwrap().to_owned()), "duplicate task");
            }
            if page["next"].is_null() { break; }
            args = page["next"].clone();
        }
        assert_eq!(seen.len(), 500);
    }
}

#[test]
fn oversized_machine_reads_reassemble_and_mutations_return_receipts() {
    let model_table = "猫🙂\\\"\n".repeat(6000);
    let raw = json!({"available_models":model_table,"status":"ready"});
    let expected = format!("{}\n{}", raw["available_models"].as_str().unwrap(), serde_json::to_string_pretty(&json!({"status":"ready"})).unwrap());
    let mut args = json!({"response_bytes":3000});
    let mut restored = String::new();
    loop {
        let (replies, request) = mcp_exchange_with_response("mcp-orchestrator", "machine", args, raw.clone());
        assert_eq!(request.0, "get /orchestrator/machine http/1.1");
        let page = tool_json(&replies);
        restored.push_str(page["fragment"].as_str().unwrap());
        if page["next"].is_null() { break; }
        args = page["next"].clone();
    }
    assert_eq!(restored, expected);
    let (replies, request) = mcp_exchange_with_response("mcp-orchestrator", "send", json!({"node":"worker","text":"Continue"}),
        json!({"accepted":true,"detail":"\\\"猫🙂".repeat(10000)}));
    assert_eq!(request.0, "post /orchestrator/send http/1.1");
    let receipt = tool_json(&replies);
    assert_eq!(receipt["completed"], true);
    assert_eq!(receipt["detail_omitted"], true);
    assert!(receipt["see"].as_str().unwrap().contains("Do not repeat"));
}

#[test]
fn live_read_fragments_use_one_immutable_snapshot_without_refetching() {
    let home = crate::test_support::Directory::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let table = "Initial model catalog\n".repeat(3000);
    let expected = format!("{table}\n{{}}");
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        assert_eq!(request_body(&stream).0, "get /orchestrator/machine http/1.1");
        respond(stream, json!({"available_models":table}).to_string().as_bytes());
        // The listener closes here. A continuation that refetches cannot succeed.
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_alondra"))
        .arg("mcp-orchestrator")
        .env("ALONDRA_HOME", &*home)
        .env("ALONDRA_PORT", port.to_string())
        .env("ALONDRA_DOMAIN_CAPABILITY", BEARER)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut args = json!({});
    let mut restored = String::new();
    loop {
        writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"machine","arguments":args}})).unwrap();
        let mut line = String::new();
        assert!(stdout.read_line(&mut line).unwrap() > 0);
        assert!(line.len() <= 24 * 1024);
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_ne!(reply["result"]["isError"], true, "{reply}");
        let page: Value = serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        restored.push_str(page["fragment"].as_str().unwrap());
        if page["next"].is_null() { break; }
        args = page["next"].clone();
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());
    server.join().unwrap();
    assert_eq!(restored, expected);
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
    assert_eq!(node[2]["result"]["content"][0]["text"], "PR github.com/example/project#7 registered.");
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
            "recipient":{"kind":"user"}, "title":"Release answer", "need":"Pick the answer", "why":"The task cannot go on without it",
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
            "task_id":"task-one", "interaction_id":"answer-delivery", "title":"Release answer", "need":"Review the answer",
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
    mcp_session_with(calls, &[], move |path| { let (status, body) = reply(path); (status, body, None) })
}

/// The session of `mcp_session` against a daemon that may say its version, with `environment` on
/// the MCP server. Standard input stays open until every call has its reply, as in a live session.
fn mcp_session_with(
    calls: &[(&str, Value)],
    environment: &[(&str, &str)],
    reply: impl Fn(&str) -> (u16, Value, Option<String>) + Send + 'static,
) -> (Vec<Value>, Vec<(String, Value)>) {
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
            let (status, answer, version) = reply(&path);
            let bytes = answer.to_string();
            let version = version.map(|version| format!("x-alondra-version: {version}\r\n")).unwrap_or_default();
            let mut stream = stream;
            write!(stream, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{version}Content-Length: {}\r\nConnection: close\r\n\r\n{bytes}", bytes.len()).unwrap();
            if path != "/capabilities" && path != "/daemon" {
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
        .envs(environment.iter().copied())
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for (index, (tool, arguments)) in calls.iter().enumerate() {
        writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":index + 1,"method":"tools/call","params":{"name":tool,"arguments":arguments}})).unwrap();
    }
    let stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    let replies: Vec<Value> = stdout.lines().take(calls.len())
        .map(|line| serde_json::from_str::<Value>(&line.unwrap()).unwrap()["result"].clone()).collect();
    drop(stdin);
    child.wait().unwrap();
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    (replies, server.join().unwrap())
}

fn needy_ask(id: &str) -> Value {
    json!({
        "interaction_id":id, "scope":{"kind":"task", "task_id":"task-one"}, "recipient":{"kind":"user"},
        "title":"Release answer", "need":"", "why":"It holds the release back", "blocking":true, "options":[{"id":"a", "label":"A"}, {"id":"b", "label":"B"}]
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

/// A daemon of a release before this client: what it refuses it only does not know.
const OLD_DAEMON: &str = "0.0.1";
const UPDATED_DAEMON: &str = "99.0.0";

fn unknown_operation() -> Value {
    json!({"code":"invalid_input", "message":"unsupported resource operation", "current_revision":null, "subject":null})
}

fn accepted() -> Value {
    json!({"revision":2,"changed":true,"effects":[],"created_ids":[]})
}

#[test]
fn a_card_an_older_daemon_refuses_waits_and_goes_again_once_the_daemon_matches() {
    let updated = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (replies, received) = mcp_session_with(&[("ask", needy_ask("card-1"))], &[], move |path| {
        let running = if updated.load(std::sync::atomic::Ordering::SeqCst) { UPDATED_DAEMON } else { OLD_DAEMON };
        match path {
            "/capabilities" => (200, json!({"data_epoch":"epoch"}), Some(running.into())),
            "/daemon" => {
                updated.store(true, std::sync::atomic::Ordering::SeqCst);
                (200, json!({"version":running}), Some(running.into()))
            }
            _ if running == OLD_DAEMON => (422, unknown_operation(), Some(running.into())),
            _ => (200, accepted(), Some(running.into())),
        }
    });
    assert_ne!(replies[0]["isError"], true, "{}", replies[0]);
    assert_eq!(received.len(), 2, "refused once, sent once more");
    assert_eq!(received[0].1["command_id"], received[1].1["command_id"], "the same card, never a second one");
}

#[test]
fn a_refusal_of_a_daemon_that_matches_is_not_held() {
    let (replies, received) = mcp_session_with(&[("ask", needy_ask("card-1"))], &[], |path| match path {
        "/capabilities" => (200, json!({"data_epoch":"epoch"}), Some(UPDATED_DAEMON.into())),
        _ => (422, unknown_operation(), Some(UPDATED_DAEMON.into())),
    });
    assert_eq!(replies[0]["isError"], true);
    assert!(replies[0]["content"][0]["text"].as_str().unwrap().contains("invalid_input"), "{}", replies[0]);
    assert_eq!(received.len(), 1);
}

#[test]
fn a_real_refusal_still_reaches_the_node_after_the_daemon_matches() {
    let updated = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (replies, received) = mcp_session_with(&[("ask", needy_ask("card-1"))], &[], move |path| {
        let running = if updated.load(std::sync::atomic::Ordering::SeqCst) { UPDATED_DAEMON } else { OLD_DAEMON };
        if path == "/daemon" { updated.store(true, std::sync::atomic::Ordering::SeqCst); }
        match path {
            "/capabilities" => (200, json!({"data_epoch":"epoch"}), Some(running.into())),
            "/daemon" => (200, json!({"version":running}), Some(running.into())),
            _ => (422, json!({"code":"invalid_input", "message":"card.need: say what you need", "current_revision":null, "subject":"card.need"}), Some(running.into())),
        }
    });
    assert_eq!(replies[0]["isError"], true);
    assert!(replies[0]["content"][0]["text"].as_str().unwrap().contains("card.need"), "{}", replies[0]);
    assert_eq!(received.len(), 2);
}

#[test]
fn a_hold_that_runs_out_says_both_versions_and_that_nothing_was_sent() {
    let (replies, received) = mcp_session_with(&[("ask", needy_ask("card-1"))], &[("ALONDRA_VERSION_HOLD_SECS", "0")], |path| match path {
        "/capabilities" => (200, json!({"data_epoch":"epoch"}), Some(OLD_DAEMON.into())),
        "/daemon" => (200, json!({"version":OLD_DAEMON}), Some(OLD_DAEMON.into())),
        _ => (422, unknown_operation(), Some(OLD_DAEMON.into())),
    });
    assert_eq!(replies[0]["isError"], true);
    assert_eq!(replies[0]["structuredContent"]["kind"], "version_mismatch");
    assert_eq!(replies[0]["structuredContent"]["daemon"], OLD_DAEMON);
    assert!(replies[0]["structuredContent"]["client"].is_string());
    let text = replies[0]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains(OLD_DAEMON) && text.contains("Nothing was sent") && text.contains("same interaction_id"), "{text}");
    assert_eq!(received.len(), 1);
}

#[test]
fn a_denied_call_is_never_held() {
    let (replies, received) = mcp_session_with(&[("ask", needy_ask("card-1"))], &[], |path| match path {
        "/capabilities" => (200, json!({"data_epoch":"epoch"}), Some(OLD_DAEMON.into())),
        _ => (403, json!({"code":"forbidden", "message":"not your task", "current_revision":null, "subject":null}), Some(OLD_DAEMON.into())),
    });
    assert_eq!(replies[0]["isError"], true);
    assert_eq!(received.len(), 1, "no wait, and nothing goes again");
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
    assert!(text.contains("don't proceed: Alondra did not confirm this ask after 3 sends") && text.contains("same interaction_id") && !text.contains("Nothing was sent"), "{text}");
    assert!(!text.contains("Recorded"), "{text}");
}
