use std::io::{BufRead, Write};
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};

use crate::client::Client;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Node,
    Orchestrator,
}

fn instructions(role: Role, user: &str) -> String {
    let language = match crate::i18n::lang() {
        crate::i18n::Lang::Es => "Spanish",
        crate::i18n::Lang::En => "English",
    };
    let who = format!(
        "The person who supervises the fleet is called {user}: wherever Alondra's tools and skill say \"the user\", that is {user}. \
         Write every question, report and message for {user} in {language}, whatever language this conversation is in."
    );
    match role {
        Role::Node => format!("Alondra connects this node with {user} and with the orchestrator. Your terminal does not reach {user}: only these tools do. If you need {user} to decide something, use `ask` (one question per decision, a one-line title, all the context in `detail`, options, links); then end your turn, and Alondra will type the answer into this session. When you finish, use `report` with kind final. Use `report` otherwise only for a result or finding {user} must read now; never to say you are still working or do not need {user} (that is noise and Alondra rejects it). While you work on your own, including waiting for a build or an upload, send nothing. If you are stopped for lack of machine resources, use `wait_for_machine`. {who}"),
        Role::Orchestrator => format!("Alondra is your nervous system: reliable fleet state (`fleet`, `node`), machine capacity (`machine`), and hands (`launch`, `send`). The nodes' questions to {user} reach {user} directly; what is meant for you is typed into your session. Use `ask` to ask {user} something and `report` to inform {user}. {who}"),
    }
}

fn ask_schema(user: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string", "description": format!("The question in one line: it is what {user} sees in the notification")},
            "detail": {"type": "string", "description": format!("All the context {user} needs to answer without asking you anything (markdown)")},
            "options": {"type": "array", "items": {"type": "object", "properties": {"label": {"type": "string"}, "description": {"type": "string"}}, "required": ["label"]}, "description": format!("Possible options; {user} can always answer with free text")},
            "links": {"type": "array", "items": {"type": "object", "properties": {"url": {"type": "string"}, "title": {"type": "string"}}, "required": ["url"]}, "description": "Relevant PRs, commits or URLs"},
            "attachments": {"type": "array", "items": {"type": "string"}, "description": format!("Absolute paths of files {user} must see (e.g. the plan to approve)")},
            "blocking": {"type": "boolean", "description": "true if you cannot continue without the answer (default true)"},
            "to_orchestrator": {"type": "boolean", "description": format!("true if the question is for the orchestrator and not for {user}")}
        },
        "required": ["title", "detail"]
    })
}

fn report_schema(user: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string", "description": "The result or finding itself, in one line (not 'update' or 'status')"},
            "body": {"type": "string", "description": "The report (markdown), short: what was measured, what changes and what is left"},
            "kind": {"type": "string", "enum": ["notice", "document", "final"], "description": format!("final = you are done and nothing is pending (the usual kind). notice = a result or finding {user} must know now, short. document = a document for {user} to read (plan, long report): attach its path in attachments. Never report progress or that you do not need {user}. If you need {user} to decide, approve or do something, do not use report: use ask.")},
            "links": {"type": "array", "items": {"type": "object", "properties": {"url": {"type": "string"}, "title": {"type": "string"}}, "required": ["url"]}},
            "attachments": {"type": "array", "items": {"type": "string"}, "description": "Absolute paths of the documents you deliver"}
        },
        "required": ["title", "body", "kind"]
    })
}

fn tools(role: Role, user: &str) -> Value {
    let mut list = vec![
        json!({"name": "ask", "description": format!("Structured question to {user} (or to the orchestrator). Returns right away with the number; end your turn and Alondra types the answer into this session."), "inputSchema": ask_schema(user)}),
        json!({"name": "report", "description": format!("Tell {user} you are done (kind final) or give {user} a result, finding or document to read. Not for progress, not for asking: those are noise here, and questions go through ask. Any comment from {user} is typed into this session."), "inputSchema": report_schema(user)}),
        json!({"name": "wait_for_machine", "description": "Declare that you are stopped waiting for machine resources (RAM, build queue, usage limit).", "inputSchema": {"type": "object", "properties": {"reason": {"type": "string"}}, "required": ["reason"]}}),
    ];
    if role == Role::Orchestrator {
        list.extend([
            json!({"name": "fleet", "description": "Fleet summary: unfinished nodes with their state, open questions, unread reports and the machine. For one node's detail, `node`.", "inputSchema": {"type": "object", "properties": {}}}),
            json!({"name": "node", "description": "A node's detail: state, tool in progress, last message, questions, reports and timeline.", "inputSchema": {"type": "object", "properties": {"node": {"type": "string", "description": "Name, short id or session_id"}}, "required": ["node"]}}),
            json!({"name": "machine", "description": "Machine capacity: memory, load, CPU and disk.", "inputSchema": {"type": "object", "properties": {}}}),
            json!({"name": "send", "description": "Send a message to a node; Alondra types it into the node's session (waking it if it is dormant).", "inputSchema": {"type": "object", "properties": {"node": {"type": "string"}, "text": {"type": "string"}}, "required": ["node", "text"]}}),
            json!({"name": "launch", "description": "Launch a new node with the Alondra plugin.", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "cwd": {"type": "string"}, "prompt": {"type": "string"}, "model": {"type": "string"}}, "required": ["name", "cwd", "prompt"]}}),
        ]);
    }
    Value::Array(list)
}

pub fn caller() -> Value {
    let session = std::env::var("CLAUDE_CODE_SESSION_ID").ok();
    json!({"session_id": session, "pids": ancestors()})
}

fn ancestors() -> Vec<i32> {
    let mut out = vec![];
    let mut pid = unsafe { libc::getppid() };
    for _ in 0..4 {
        if pid <= 1 {
            break;
        }
        out.push(pid);
        let parent = std::process::Command::new("/bin/ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok());
        match parent {
            Some(p) => pid = p,
            None => break,
        }
    }
    out
}

fn merge(mut a: Value, b: &Value) -> Value {
    if let (Some(a), Some(b)) = (a.as_object_mut(), b.as_object()) {
        for (k, v) in b {
            a.insert(k.clone(), v.clone());
        }
    }
    a
}

fn call(client: &Client, role: Role, user: &str, name: &str, args: &Value) -> Result<String> {
    let who = caller();
    Ok(match name {
        "ask" => {
            let r = client.post("/v1/questions", &merge(args.clone(), &who))?;
            let id = r.get("id").and_then(Value::as_u64).unwrap_or(0);
            format!("Question #{id} recorded and sent to {user}. End your turn when you have nothing else to do: Alondra will type the answer into this session.")
        }
        "report" => {
            let r = client.post("/v1/reports", &merge(args.clone(), &who))?;
            format!("Report #{} delivered to {user}.", r.get("id").and_then(Value::as_u64).unwrap_or(0))
        }
        "wait_for_machine" => {
            let reason = args.get("reason").and_then(Value::as_str).unwrap_or("");
            client.post("/v1/waiting-machine", &merge(json!({"reason": reason}), &who))?;
            "Noted: you are waiting for the machine.".to_string()
        }
        "fleet" if role == Role::Orchestrator => serde_json::to_string_pretty(&fleet_summary(&client.get::<Value>("/v1/overview")?))?,
        "machine" if role == Role::Orchestrator => {
            let v: Value = client.get("/v1/overview")?;
            serde_json::to_string_pretty(v.get("machine").unwrap_or(&Value::Null))?
        }
        "node" if role == Role::Orchestrator => {
            let key = args.get("node").and_then(Value::as_str).unwrap_or("");
            serde_json::to_string_pretty(&client.get::<Value>(&format!("/v1/nodes/{}", urlencode(key)))?)?
        }
        "send" if role == Role::Orchestrator => {
            let key = args.get("node").and_then(Value::as_str).unwrap_or("");
            let text = args.get("text").and_then(Value::as_str).unwrap_or("");
            client.post(&format!("/v1/nodes/{}/messages", urlencode(key)), &json!({"text": text, "from": "orchestrator"}))?;
            format!("Message queued for {key}; Alondra will type it into its session.")
        }
        "launch" if role == Role::Orchestrator => serde_json::to_string_pretty(&client.post("/v1/launch", args)?)?,
        other => anyhow::bail!("unknown tool: {other}"),
    })
}

fn fleet_summary(o: &Value) -> Value {
    let list = |k: &str| o.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
    let pick = |v: &Value, keys: &[&str]| {
        Value::Object(keys.iter().filter_map(|k| v.get(*k).filter(|x| !x.is_null()).map(|x| (k.to_string(), x.clone()))).collect())
    };
    let nodes = list("nodes");
    let done = nodes.iter().filter(|n| n.get("state").and_then(Value::as_str) == Some("done")).count();
    let active: Vec<Value> = nodes
        .iter()
        .filter(|n| n.get("state").and_then(Value::as_str) != Some("done"))
        .map(|n| pick(n, &["name", "short_id", "project", "state", "state_since", "daemon_needs", "waiting_permission", "alive"]))
        .collect();
    let questions: Vec<Value> = list("open_questions").iter().map(|q| pick(q, &["id", "title", "node_name", "origin", "to_orchestrator", "asked_at"])).collect();
    let unread: Vec<Value> = list("reports")
        .iter()
        .filter(|r| r.get("read") == Some(&Value::Bool(false)))
        .map(|r| pick(r, &["id", "title", "kind", "node_name"]))
        .collect();
    json!({"nodes": active, "done_nodes": done, "open_questions": questions, "unread_reports": unread,
           "orchestrator_messages": o.get("orchestrator_messages"), "machine": o.get("machine")})
}

pub fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn serve(role: Role) -> Result<()> {
    let user = crate::client::config().name.clone();
    let client = Client::new(Duration::from_secs(30));
    let mut registered = role != Role::Orchestrator;
    let mut register = |client: &Client| {
        if !registered {
            registered = client.post("/v1/orchestrator", &caller()).is_ok();
        }
    };
    register(&client);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = req.get("id").cloned() else { continue };
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "alondra", "version": env!("CARGO_PKG_VERSION")},
                "instructions": instructions(role, &user),
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools(role, &user)})),
            "tools/call" => {
                register(&client);
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(match call(&client, role, &user, name, &args) {
                    Ok(text) => json!({"content": [{"type": "text", "text": text}]}),
                    Err(e) => json!({"content": [{"type": "text", "text": format!("Alondra error: {e}")}], "isError": true}),
                })
            }
            _ => Err(json!({"code": -32601, "message": format!("unsupported method: {method}")})),
        };
        let resp = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        };
        writeln!(stdout, "{resp}")?;
        stdout.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configured_name_reaches_every_text_a_node_reads() {
        for role in [Role::Node, Role::Orchestrator] {
            let text = format!("{} {}", instructions(role, "Mostafa"), tools(role, "Mostafa"));
            assert!(text.contains("Mostafa") && !text.contains("David"), "{text}");
        }
    }
}
