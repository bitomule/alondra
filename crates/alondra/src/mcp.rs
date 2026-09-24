use std::collections::{HashMap, HashSet};
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
        Role::Node => format!("Alondra connects this node with {user} and with the orchestrator. Your terminal does not reach {user}; these tools do, and Alondra types {user}'s answers into this session. `ask` puts one decision in front of {user}; end your turn after it and the answer arrives here. `request_action` hands {user} something to do outside Alondra (test on a device, get a key, check a console). `report` with kind final says you are done; other kinds deliver a result {user} should read now. Alondra rejects reports that only say you are still working, so while you work on your own there is nothing to send. `withdraw` removes a question that no longer needs {user}. A PR you open gets its own card where {user} decides the merge, and you count as waiting for that merge until then, even after a final report; when the merge notice arrives, do what it unblocks and report final again. An `ask` about merging would be a second card for the same decision. A report that links the PR is shown inside that card, so the PR being ready needs no report of its own. `wait_for_machine` says you are stopped for machine resources. Run each git command in its own Bash call, without `&&`, `;` or pipes: chained commands reach {user} as a permission card even when each part is allowed. {who}"),
        Role::Orchestrator => format!("Alondra gives you the fleet's state (`fleet`, `node`), the machine's capacity (`machine`), and hands (`launch`, `send`, `interrupt`). The nodes' questions reach {user} directly; what is meant for you is typed into your session. `ask` asks {user} something and `report` informs {user}. {who}"),
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
            "to_orchestrator": {"type": "boolean", "description": format!("true if the question is for the orchestrator and not for {user}")},
            "related": {"type": "array", "items": {"type": "integer"}, "description": format!("Required when you already have open cards for {user} (open questions or unread reports): the numbers of those this one belongs with, shown together as one card, or [] if none. Without it Alondra records nothing and lists your open cards.")},
            "replaces": {"type": "integer", "description": "The number of an open question of yours that this one replaces (you rephrased it or the situation changed); it is withdrawn when this one is recorded"}
        },
        "required": ["title", "detail"]
    })
}

fn action_schema(user: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string", "description": format!("What {user} has to do, in one line: it is what {user} sees in the notification")},
            "steps": {"type": "array", "items": {"type": "string"}, "description": format!("The steps, in order, each one something {user} can do without asking you anything")},
            "links": {"type": "array", "items": {"type": "object", "properties": {"url": {"type": "string"}, "title": {"type": "string"}}, "required": ["url"]}, "description": "Where to do it: consoles, PRs, builds"},
            "detail": {"type": "string", "description": "Why it is needed and what you do once it is done (markdown, optional)"}
        },
        "required": ["title", "steps"]
    })
}

fn report_schema(user: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string", "description": "The result or finding itself, in one line (not 'update' or 'status')"},
            "body": {"type": "string", "description": "The report (markdown), short: what was measured, what changes and what is left"},
            "kind": {"type": "string", "enum": ["notice", "document", "final"], "description": format!("final: you are done and nothing is pending (the usual kind). notice: a short result or finding {user} should know now. document: something for {user} to read (plan, long report), with its path in attachments. A report is for reading: a decision, approval or task for {user} goes through ask or request_action.")},
            "links": {"type": "array", "items": {"type": "object", "properties": {"url": {"type": "string"}, "title": {"type": "string"}}, "required": ["url"]}},
            "attachments": {"type": "array", "items": {"type": "string"}, "description": "Absolute paths of the documents you deliver"},
            "related": {"type": "array", "items": {"type": "integer"}, "description": format!("Required when you already have open cards for {user} (open questions or unread reports): the numbers of those this one belongs with, shown together as one card, or [] if none. Without it Alondra records nothing and lists your open cards.")}
        },
        "required": ["title", "body", "kind"]
    })
}

fn tools(role: Role, user: &str) -> Value {
    let mut list = vec![
        json!({"name": "ask", "description": format!("Structured question to {user} (or to the orchestrator). Returns its number right away; Alondra types the answer into this session when it comes."), "inputSchema": ask_schema(user)}),
        json!({"name": "request_action", "description": format!("Hand {user} something to do outside Alondra (test on the iPhone, get a key, check App Store Connect). {user} sees a \"Your turn\" card with the steps and answers \"Done\" or explains why they cannot; Alondra types that into this session."), "inputSchema": action_schema(user)}),
        json!({"name": "report", "description": format!("Tell {user} you are done (kind final) or give {user} a result, finding or document to read. Alondra rejects reports that only give progress. Any comment from {user} is typed into this session."), "inputSchema": report_schema(user)}),
        json!({"name": "withdraw", "description": format!("Withdraw a question you asked that no longer needs {user}: it was settled another way, or the work moved on. Its card leaves every client."), "inputSchema": {"type": "object", "properties": {"id": {"type": "integer", "description": "The question number `ask` returned"}}, "required": ["id"]}}),
        json!({"name": "wait_for_machine", "description": "Declare that you are stopped waiting for machine resources (RAM, build queue, usage limit).", "inputSchema": {"type": "object", "properties": {"reason": {"type": "string"}}, "required": ["reason"]}}),
    ];
    if role == Role::Orchestrator {
        list.extend([
            json!({"name": "fleet", "description": format!("Fleet summary: unfinished nodes that are running, dormant or owe {user} something, with what they run in the background (subagents and shells; `suspect` when possibly hung); open questions, unread reports, undelivered messages for you, and the machine."), "inputSchema": {"type": "object", "properties": {"all": {"type": "boolean", "description": "Add finished and gone nodes"}}}}),
            json!({"name": "node", "description": "A node's state, tool in progress, last message, open questions and actions, unread reports, pending messages and recent events.", "inputSchema": {"type": "object", "properties": {"node": {"type": "string", "description": "Name, short id or session_id"}, "history": {"type": "boolean", "description": "Add every question, report and message, and 100 events"}, "before": {"type": "integer", "description": "The 100 events before this seq"}}, "required": ["node"]}}),
            json!({"name": "machine", "description": "Machine capacity (memory, load, CPU, disk) and the subscription's usage: the week, today against its slice, and whether today is over pace.", "inputSchema": {"type": "object", "properties": {}}}),
            json!({"name": "send", "description": "Send a message to a node; Alondra types it into the node's session (waking it if it is dormant).", "inputSchema": {"type": "object", "properties": {"node": {"type": "string"}, "text": {"type": "string"}}, "required": ["node", "text"]}}),
            json!({"name": "interrupt", "description": "Press Esc in a node's session to stop the call it is stuck in, for when Alondra reports a node inside one call that looks hung; `send` then moves it on.", "inputSchema": {"type": "object", "properties": {"node": {"type": "string", "description": "Name, short id or session_id"}}, "required": ["node"]}}),
            json!({"name": "launch", "description": format!("Launch a new node with the Alondra plugin. Refused while today is over its slice of the weekly subscription usage (see `machine`); over_pace_ok launches anyway once {user} has said so."), "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "cwd": {"type": "string"}, "prompt": {"type": "string"}, "model": {"type": "string"}, "over_pace_ok": {"type": "boolean", "description": format!("true only when {user} approved launching over today's usage slice")}}, "required": ["name", "cwd", "prompt"]}}),
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
        "request_action" => {
            let r = client.post("/v1/actions", &merge(args.clone(), &who))?;
            let id = r.get("id").and_then(Value::as_u64).unwrap_or(0);
            format!("Action #{id} handed to {user}. End your turn when you have nothing else to do: Alondra will type into this session whether it is done.")
        }
        "report" => {
            let r = client.post("/v1/reports", &merge(args.clone(), &who))?;
            let delivered = format!("Report #{} delivered to {user}.", r.get("id").and_then(Value::as_u64).unwrap_or(0));
            match r.get("note").and_then(Value::as_str) {
                Some(note) => format!("{delivered} {note}"),
                None => delivered,
            }
        }
        "withdraw" => {
            let id = args.get("id").and_then(Value::as_u64).ok_or_else(|| anyhow::anyhow!("id is required"))?;
            client.post(&format!("/v1/questions/{id}/withdraw"), &who)?;
            format!("Question #{id} withdrawn; its card is gone.")
        }
        "wait_for_machine" => {
            let reason = args.get("reason").and_then(Value::as_str).unwrap_or("");
            client.post("/v1/waiting-machine", &merge(json!({"reason": reason}), &who))?;
            "Noted: you are waiting for the machine.".to_string()
        }
        "fleet" if role == Role::Orchestrator => {
            let all = flag(args, "all");
            serde_json::to_string_pretty(&fleet_summary(&client.get::<Value>("/v1/overview")?, all))?
        }
        "machine" if role == Role::Orchestrator => {
            let v: Value = client.get("/v1/overview")?;
            serde_json::to_string_pretty(&json!({"machine": v.get("machine"), "usage": v.get("usage")}))?
        }
        "node" if role == Role::Orchestrator => {
            let key = args.get("node").and_then(Value::as_str).unwrap_or("");
            let before = args.get("before").and_then(|b| b.as_u64().or_else(|| b.as_str()?.parse().ok()));
            let page = if flag(args, "history") || before.is_some() { Page::History { before } } else { Page::Recent };
            let mut path = format!("/v1/nodes/{}?limit={}", urlencode(key), page.window());
            if let Some(b) = before {
                path.push_str(&format!("&before={b}"));
            }
            serde_json::to_string_pretty(&node_view(&client.get::<Value>(&path)?, page))?
        }
        "send" if role == Role::Orchestrator => {
            let key = args.get("node").and_then(Value::as_str).unwrap_or("");
            let text = args.get("text").and_then(Value::as_str).unwrap_or("");
            client.post(&format!("/v1/nodes/{}/messages", urlencode(key)), &json!({"text": text, "from": "orchestrator"}))?;
            format!("Message queued for {key}; Alondra will type it into its session.")
        }
        "interrupt" if role == Role::Orchestrator => {
            let key = args.get("node").and_then(Value::as_str).unwrap_or("");
            client.post(&format!("/v1/nodes/{}/interrupt", urlencode(key)), &json!({}))?;
            format!("Pressed Esc in {key}'s session. `send` moves it on if it should carry on.")
        }
        "launch" if role == Role::Orchestrator => serde_json::to_string_pretty(&client.post("/v1/launch", args)?)?,
        other => anyhow::bail!("unknown tool: {other}"),
    })
}

fn flag(args: &Value, key: &str) -> bool {
    match args.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    }
}

fn pick(v: &Value, keys: &[&str]) -> Value {
    Value::Object(keys.iter().filter_map(|k| v.get(*k).filter(|x| !x.is_null()).map(|x| (k.to_string(), x.clone()))).collect())
}

/// The subagents and shells a node runs, when it runs any.
fn running(n: &Value) -> Option<Value> {
    n.get("background").filter(|b| b.as_array().is_some_and(|a| !a.is_empty())).cloned()
}

fn list(v: &Value, key: &str) -> Vec<Value> {
    v.get(key).and_then(Value::as_array).cloned().unwrap_or_default()
}

/// The fleet the orchestrator acts on: unfinished nodes that are running, dormant, or still owe
/// the user something. `all` adds finished nodes and the ones whose session is gone.
fn fleet_summary(o: &Value, all: bool) -> Value {
    let open = list(o, "open_questions");
    let unread: Vec<Value> = list(o, "reports").into_iter().filter(|r| r.get("read") == Some(&Value::Bool(false))).collect();
    let owes = |session: Option<&str>| open.iter().chain(&unread).any(|x| x.get("session_id").and_then(Value::as_str) == session);
    let (mut done, mut gone) = (0, 0);
    let mut nodes = vec![];
    for n in list(o, "nodes") {
        let is = |k: &str| n.get(k) == Some(&Value::Bool(true));
        let finished = n.get("state").and_then(Value::as_str) == Some("done");
        let present = is("alive") || is("in_daemon") || owes(n.get("session_id").and_then(Value::as_str));
        if !all && finished {
            done += 1;
        } else if !all && !present {
            gone += 1;
        } else {
            let mut row = pick(&n, &["name", "short_id", "project", "state", "state_since", "daemon_needs", "waiting_permission", "alive", "in_daemon", "undeliverable_messages"]);
            if let Some(b) = running(&n) {
                row["background"] = b;
            }
            nodes.push(row);
        }
    }
    let questions: Vec<Value> = open.iter().map(|q| pick(q, &["id", "title", "node_name", "origin", "to_orchestrator", "action", "asked_at"])).collect();
    let reports: Vec<Value> = unread.iter().map(|r| pick(r, &["id", "title", "kind", "node_name"])).collect();
    let messages: Vec<Value> =
        list(o, "orchestrator_messages").into_iter().filter(|m| all || m.get("handed_at").is_none_or(Value::is_null)).collect();
    let mut out = json!({"nodes": nodes, "open_questions": questions, "unread_reports": reports,
                         "orchestrator_messages": messages, "machine": o.get("machine"), "usage": o.get("usage")});
    if done + gone > 0 {
        out["hidden"] = json!({"done_nodes": done, "gone_nodes": gone, "see_them_with": "fleet with all: true"});
    }
    out
}

/// Which part of a node's record `node` returns.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Page {
    /// What the orchestrator acts on, with the last few distinct events.
    Recent,
    /// Every question, report and message, and a page of distinct events: the newest ones, or
    /// the ones before a sequence number.
    History { before: Option<u64> },
}

impl Page {
    /// Raw events fetched, enough for the watcher's heartbeats to collapse into `shown` distinct ones.
    fn window(self) -> usize {
        match self {
            Page::Recent => 200,
            Page::History { .. } => 400,
        }
    }

    fn shown(self) -> usize {
        match self {
            Page::Recent => 12,
            Page::History { .. } => 100,
        }
    }
}

/// Strings this long are texts (messages, prompts, details); shorter ones are values like
/// states, which repeat by nature.
const TEXT_MIN: usize = 40;
const EVENT_NOISE: &[&str] = &["session_id", "short_id", "updated_at", "last_text_at"];

fn is_text(v: &Value) -> Option<&String> {
    match v {
        Value::String(s) if s.chars().count() >= TEXT_MIN => Some(s),
        _ => None,
    }
}

fn texts(v: &Value, out: &mut HashSet<String>) {
    match v {
        Value::Array(a) => a.iter().for_each(|x| texts(x, out)),
        Value::Object(o) => o.values().for_each(|x| texts(x, out)),
        _ => {
            if let Some(s) = is_text(v) {
                out.insert(s.clone());
            }
        }
    }
}

/// Drops every text already shown, and remembers the ones it keeps.
fn drop_seen(v: &mut Value, seen: &mut HashSet<String>) {
    let repeated = |x: &Value, seen: &HashSet<String>| is_text(x).is_some_and(|s| seen.contains(s));
    match v {
        Value::Object(o) => {
            o.retain(|_, x| !repeated(x, seen));
            o.values_mut().for_each(|x| drop_seen(x, seen));
        }
        Value::Array(a) => {
            a.retain(|x| !repeated(x, seen));
            a.iter_mut().for_each(|x| drop_seen(x, seen));
        }
        _ => {
            if let Some(s) = is_text(v) {
                seen.insert(s.clone());
            }
        }
    }
}

/// The node's timeline, oldest first, each event flattened, leaving out any event equal to the
/// previous one of its type (the watcher's heartbeats).
fn distinct_events(timeline: &[Value]) -> Vec<Value> {
    let mut last: HashMap<String, Value> = HashMap::new();
    let mut out = vec![];
    for e in timeline.iter().rev() {
        let mut event = e.get("event").cloned().unwrap_or(Value::Null);
        if let Some(o) = event.as_object_mut() {
            o.retain(|k, x| !x.is_null() && !EVENT_NOISE.contains(&k.as_str()));
        }
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("").to_string();
        if last.get(&kind) == Some(&event) {
            continue;
        }
        let mut row = json!({"seq": e.get("seq"), "at": e.get("at")});
        if let (Some(row), Some(fields)) = (row.as_object_mut(), event.as_object()) {
            row.extend(fields.clone());
        }
        last.insert(kind, event);
        out.push(row);
    }
    out
}

/// What the orchestrator acts on about one node: its state, its last message once, what is open
/// with the user, and its recent distinct events. A history page adds every question, report and
/// message, and pages through the events. No text appears twice in one response.
fn node_view(d: &Value, page: Page) -> Value {
    let history = page != Page::Recent;
    let mut node = pick(
        d.get("node").unwrap_or(&Value::Null),
        &["name", "short_id", "cwd", "state", "state_since", "alive", "in_daemon", "is_orchestrator", "busy", "tool", "waiting_permission",
          "menu_waiting", "mission", "daemon_needs", "usage_limited", "evidence", "last_prompt", "last_prompt_at", "last_message", "last_message_at"],
    );
    if let Some(b) = d.get("node").and_then(running) {
        node["background"] = b;
    }
    let questions = list(d, "questions");
    let is_open = |q: &Value| q.get("status").and_then(Value::as_str) == Some("open");
    let open: Vec<Value> = questions
        .iter()
        .filter(|q| is_open(q))
        .map(|q| pick(q, &["id", "title", "detail", "options", "links", "attachments", "blocking", "to_orchestrator", "action", "steps", "asked_at"]))
        .collect();
    let reports = list(d, "reports");
    let unread = |r: &Value| r.get("read") == Some(&Value::Bool(false));
    let shown_reports: Vec<Value> = reports
        .iter()
        .filter(|r| history || unread(r))
        .map(|r| pick(r, &["id", "title", "kind", "body", "links", "attachments", "read", "created_at"]))
        .collect();
    let messages = list(d, "messages");
    let pending = |m: &Value| m.get("handed_at").is_none_or(Value::is_null);
    let shown_messages: Vec<Value> = messages
        .iter()
        .filter(|m| history || pending(m))
        .map(|m| pick(m, &["id", "from", "text", "answers_question", "created_at", "handed_at"]))
        .collect();

    let mut out = json!({"node": node, "open_questions": open, "reports": shown_reports, "messages": shown_messages});
    if let Some(held) = d.get("messages_held_for_turn_end").filter(|h| h.as_u64().unwrap_or(0) > 0) {
        out["messages_held_for_turn_end"] = held.clone();
    }
    if history {
        out["closed_questions"] =
            Value::Array(questions.iter().filter(|q| !is_open(q)).map(|q| pick(q, &["id", "title", "status", "answer", "asked_at"])).collect());
    }

    let mut seen = HashSet::new();
    texts(&out, &mut seen);
    if let Page::History { before: Some(_) } = page {
        out = json!({"node": pick(&out["node"], &["name", "short_id"])});
    }

    let timeline = list(d, "timeline");
    let all = distinct_events(&timeline);
    let keep = page.shown().min(all.len());
    let mut events = all[all.len() - keep..].to_vec();
    events.iter_mut().for_each(|e| drop_seen(e, &mut seen));
    let earlier = (keep < all.len() || timeline.len() >= page.window()).then(|| events.first().and_then(|e| e.get("seq")).cloned()).flatten();
    out[if history { "events" } else { "recent_events" }] = Value::Array(events);

    if history {
        if let Some(seq) = earlier {
            out["earlier_events"] = json!({"see_them_with": format!("node with before: {seq}")});
        }
        return out;
    }
    let mut hidden = serde_json::Map::new();
    for (k, n) in [
        ("closed_questions", questions.len() - open.len()),
        ("read_reports", reports.iter().filter(|r| !unread(r)).count()),
        ("delivered_messages", messages.iter().filter(|m| !pending(m)).count()),
    ] {
        if n > 0 {
            hidden.insert(k.into(), json!(n));
        }
    }
    if earlier.is_some() {
        hidden.insert("earlier_events".into(), json!(true));
    }
    if !hidden.is_empty() {
        hidden.insert("see_them_with".into(), json!("node with history: true"));
        out["hidden"] = Value::Object(hidden);
    }
    out
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

    const SAID: &str = "Phase 1 is merged and deployed; the TUI, the Mac and the iPhone show notices inside their PR card.";
    const ASKED: &str = "Approved, start with phase 1 and open the PR when it builds and its checks pass.";

    fn every_text(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Array(a) => a.iter().for_each(|x| every_text(x, out)),
            Value::Object(o) => o.values().for_each(|x| every_text(x, out)),
            _ => out.extend(is_text(v).cloned()),
        }
    }

    fn assert_no_text_repeats(v: &Value) {
        let mut all = vec![];
        every_text(v, &mut all);
        let mut seen = HashSet::new();
        for t in &all {
            assert!(seen.insert(t), "repeated: {t}\n{v:#}");
        }
    }

    fn event(seq: u64, body: Value) -> Value {
        json!({"seq": seq, "at": seq * 1000, "event": body})
    }

    fn heartbeat(seq: u64) -> Value {
        event(seq, json!({"type": "daemon_seen", "session_id": "s", "short_id": "s", "state": "idle", "last_text": SAID, "last_prompt": ASKED, "updated_at": seq}))
    }

    /// A long-lived node as the daemon returns it: newest first, the watcher's heartbeats each
    /// repeating the last message, and one question, report and message of each kind.
    fn detail(timeline: Vec<Value>) -> Value {
        let mut timeline = timeline;
        timeline.reverse();
        json!({
            "node": {"name": "alondra/plan", "short_id": "s", "session_id": "s", "state": "working", "alive": true,
                     "last_message": SAID, "last_prompt": ASKED, "turn": 7},
            "questions": [
                {"id": 1, "title": "Which phase goes first, the card rules or stacked PRs?", "status": "answered", "asked_at": 1},
                {"id": 2, "title": "Test the new card on the iPhone and say whether it reads well", "status": "open", "action": true, "steps": ["Open the inbox"], "asked_at": 2}
            ],
            "reports": [
                {"id": 3, "title": "Phase 0 is done and every card now carries its node's mission", "kind": "final", "read": true, "created_at": 3},
                {"id": 4, "title": "The stacked PR support still waits on its last fix from review", "kind": "notice", "read": false, "created_at": 4}
            ],
            "messages": [
                {"id": 5, "from": "user", "text": "Start with phase 1 once the checks of the previous PR are green", "handed_at": 5},
                {"id": 6, "from": "orchestrator", "text": "Report kind final when the PR is open, with its link in links", "handed_at": null}
            ],
            "messages_held_for_turn_end": 0,
            "timeline": timeline
        })
    }

    fn long_lived() -> Vec<Value> {
        let mut t = vec![event(1, json!({"type": "hook", "event": "UserPromptSubmit", "prompt": ASKED}))];
        t.extend((2..32).map(heartbeat));
        t.push(event(32, json!({"type": "hook", "event": "Stop", "last_message": SAID})));
        t.push(event(33, json!({"type": "question_asked", "id": 2, "title": "Test the new card on the iPhone and say whether it reads well"})));
        t
    }

    #[test]
    fn node_gives_the_last_message_once_and_collapses_heartbeats() {
        let v = node_view(&detail(long_lived()), Page::Recent);
        let text = v.to_string();
        assert_eq!(text.matches(SAID).count(), 1, "{v:#}");
        assert_no_text_repeats(&v);
        let events = v["recent_events"].as_array().unwrap();
        assert_eq!(events.iter().filter(|e| e["type"] == "daemon_seen").count(), 1, "30 identical heartbeats are one event: {v:#}");
        assert_eq!(events.iter().map(|e| e["seq"].as_u64().unwrap()).collect::<Vec<_>>(), vec![1, 2, 32, 33], "oldest first");
        assert_eq!(v["open_questions"].as_array().unwrap().iter().map(|q| q["id"].as_u64().unwrap()).collect::<Vec<_>>(), vec![2]);
        assert_eq!(v["open_questions"][0]["action"], true, "actions are open questions too");
        assert_eq!(v["reports"].as_array().unwrap().len(), 1, "only the unread report");
        assert_eq!(v["messages"].as_array().unwrap().len(), 1, "only the message not yet typed in");
        assert_eq!(v["hidden"], json!({"closed_questions": 1, "read_reports": 1, "delivered_messages": 1, "see_them_with": "node with history: true"}));
    }

    #[test]
    fn node_shows_a_heartbeat_again_when_something_in_it_changes() {
        let mut t = long_lived();
        t.push(event(34, json!({"type": "daemon_seen", "state": "working", "last_text": SAID, "updated_at": 34})));
        let v = node_view(&detail(t), Page::Recent);
        let seen: Vec<&Value> = v["recent_events"].as_array().unwrap().iter().filter(|e| e["type"] == "daemon_seen").collect();
        assert_eq!(seen.len(), 2, "{v:#}");
        assert_eq!(seen[1]["state"], "working");
        assert_no_text_repeats(&v);
    }

    #[test]
    fn node_keeps_only_the_newest_distinct_events() {
        let t: Vec<Value> = (1..=40).map(|i| event(i, json!({"type": "hook", "event": "PreToolUse", "tool_summary": format!("Bash cargo test --package alondra --lib mcp::tests number {i}")}))).collect();
        let v = node_view(&detail(t), Page::Recent);
        let seqs: Vec<u64> = v["recent_events"].as_array().unwrap().iter().map(|e| e["seq"].as_u64().unwrap()).collect();
        assert_eq!(seqs, (29..=40).collect::<Vec<_>>());
        assert_eq!(v["hidden"]["earlier_events"], true);
    }

    #[test]
    fn history_reaches_every_question_report_message_and_event() {
        let v = node_view(&detail(long_lived()), Page::History { before: None });
        assert_no_text_repeats(&v);
        let ids = |k: &str| v[k].as_array().unwrap().iter().map(|x| x["id"].as_u64().unwrap()).collect::<Vec<_>>();
        assert_eq!(ids("closed_questions"), vec![1]);
        assert_eq!(ids("open_questions"), vec![2]);
        assert_eq!(ids("reports"), vec![3, 4]);
        assert_eq!(ids("messages"), vec![5, 6]);
        assert_eq!(v["events"].as_array().unwrap().len(), 4);
        assert!(v.get("hidden").is_none() && v.get("earlier_events").is_none(), "{v:#}");
    }

    #[test]
    fn history_pages_back_through_events_the_daemon_has() {
        let page = Page::History { before: None };
        let mut t: Vec<Value> = (1..=page.window() as u64)
            .map(|i| event(i, json!({"type": "hook", "event": "PreToolUse", "tool_summary": format!("Read crates/alondra/src/mcp.rs lines {i} to {}", i + 40)})))
            .collect();
        t.push(event(1000, json!({"type": "hook", "event": "Stop", "last_message": SAID})));
        let first = node_view(&detail(t.split_off(t.len() - page.window())), page);
        let cursor = first["events"][0]["seq"].as_u64().unwrap();
        assert_eq!(first["events"].as_array().unwrap().len(), page.shown());
        assert_eq!(first["earlier_events"]["see_them_with"], format!("node with before: {cursor}"));

        let older: Vec<Value> = t.into_iter().chain((2..cursor).map(|i| event(i, json!({"type": "hook", "event": "PreToolUse", "tool_summary": format!("Read crates/alondra/src/mcp.rs lines {i} to {}", i + 40)})))).filter(|e| e["seq"].as_u64().unwrap() < cursor).collect();
        let next = node_view(&detail(older), Page::History { before: Some(cursor) });
        assert_eq!(next["node"], json!({"name": "alondra/plan", "short_id": "s"}), "a later page does not repeat the node");
        assert!(next.get("reports").is_none() && next.get("closed_questions").is_none());
        assert_eq!(next["events"].as_array().unwrap().last().unwrap()["seq"], cursor - 1, "it continues right where the first page stopped");
        assert_no_text_repeats(&next);
    }

    fn overview() -> Value {
        let node = |sid: &str, state: &str, alive: bool, in_daemon: bool| json!({"session_id": sid, "name": sid, "short_id": sid, "state": state, "alive": alive, "in_daemon": in_daemon, "last_message": SAID});
        json!({
            "nodes": [
                node("running", "working", true, true),
                node("dormant", "waiting_answer", false, true),
                node("gone-owing", "unclear", false, false),
                node("gone", "unclear", false, false),
                node("gone-too", "working", false, false),
                node("finished", "done", false, true),
            ],
            "open_questions": [{"id": 9, "session_id": "gone-owing", "title": "Keep the old branch?", "detail": SAID, "status": "open"}],
            "reports": [{"id": 8, "session_id": "running", "title": "Done", "kind": "final", "read": false, "body": SAID}],
            "orchestrator_messages": [
                {"id": 1, "text": "[Alondra] Stalled node: gone (gone) ended its turn twice", "handed_at": 5},
                {"id": 2, "text": "[Alondra] Stalled node: running (running) ended its turn twice", "handed_at": null}
            ],
            "machine": {"load": [1.0, 1.0, 1.0]}
        })
    }

    #[test]
    fn fleet_lists_the_nodes_that_matter_and_counts_the_rest() {
        let v = fleet_summary(&overview(), false);
        let names: Vec<&str> = v["nodes"].as_array().unwrap().iter().map(|n| n["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["running", "dormant", "gone-owing"], "a gone node that still owes the user an answer stays");
        assert_eq!(v["hidden"], json!({"done_nodes": 1, "gone_nodes": 2, "see_them_with": "fleet with all: true"}));
        assert_eq!(v["orchestrator_messages"].as_array().unwrap().iter().map(|m| m["id"].as_u64().unwrap()).collect::<Vec<_>>(), vec![2], "already typed in: not again");
        assert!(!v.to_string().contains(SAID), "fleet carries titles, not bodies: {v:#}");
    }

    #[test]
    fn fleet_with_all_lists_every_node_and_message() {
        let v = fleet_summary(&overview(), true);
        assert_eq!(v["nodes"].as_array().unwrap().len(), 6);
        assert_eq!(v["orchestrator_messages"].as_array().unwrap().len(), 2);
        assert!(v.get("hidden").is_none());
    }

    #[test]
    fn the_fleet_lists_what_a_node_runs_only_when_it_runs_something() {
        let o = json!({"nodes": [
            {"name": "a", "state": "working", "alive": true, "background": [{"kind": "shell", "label": "until pgrep", "started_at": 1, "suspect": true}]},
            {"name": "b", "state": "working", "alive": true, "background": []}
        ]});
        let f = fleet_summary(&o, false);
        assert_eq!(f["nodes"][0]["background"][0]["suspect"], true);
        assert!(f["nodes"][1].get("background").is_none());
    }
}
