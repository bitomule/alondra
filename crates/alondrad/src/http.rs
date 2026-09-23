use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use alondra_core::{
    Answer, Attachment, Event, HookEvent, Link, Message, MessageFrom, Question, QuestionOption,
    QuestionOrigin, QuestionStatus, Report, ReportKind, View, answer_option, now_millis,
};
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::Extension;
use std::net::SocketAddr;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::{Auth, is_local};
use crate::push::{Push, Registration};
use crate::hub::Hub;

const MAX_WAIT: Duration = Duration::from_secs(55);
const MAX_ATTACHMENT_BYTES: u64 = 5 * 1024 * 1024;

type AppState = Arc<Hub>;

pub fn router(hub: Arc<Hub>, auth: Arc<Auth>, push: Arc<Push>) -> Router {
    Router::new()
        .route("/v1/push/register", post(push_register))
        .route("/v1/push/check", post(push_check))
        .route("/v1/pairing", post(new_pairing))
        .route("/v1/pair", post(pair))
        .route("/v1/devices", get(devices))
        .route("/v1/devices/{id}", axum::routing::delete(revoke_device))
        .route("/hook", post(hook))
        .route("/v1/overview", get(overview))
        .route("/v1/changes", get(changes))
        .route("/v1/nodes/{key}", get(node_detail).delete(forget_node))
        .route("/v1/nodes/{key}/close", post(close_node))
        .route("/v1/nodes/{key}/messages", post(send_message))
        .route("/v1/nodes/{key}/ack", post(acknowledge))
        .route("/v1/questions", post(ask))
        .route("/v1/questions/{id}", get(question_detail))
        .route("/v1/questions/{id}/answer", post(answer))
        .route("/v1/questions/{id}/withdraw", post(withdraw))
        .route("/v1/questions/{id}/more-context", post(more_context))
        .route("/v1/reports", post(report))
        .route("/v1/reports/{id}/read", post(report_read))
        .route("/v1/waiting-machine", post(waiting_machine))
        .route("/v1/orchestrator", post(mark_orchestrator))
        .route("/v1/launch", post(launch))
        .route("/v1/requests", post(new_request))
        .route("/v1/search", get(search))
        .route("/v1/health", get(health))
        .route("/v1/file", get(file))
        .layer(Extension(auth))
        .layer(Extension(push))
        .with_state(hub)
}

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

fn internal(e: anyhow::Error) -> Response {
    err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn str_field(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

fn keep_rejected(home: &std::path::Path, title: &str, body: Option<&str>) {
    use std::io::Write;
    let line = json!({"at": now_millis(), "title": title, "body": body}).to_string();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(home.join("rejected-reports.jsonl")) {
        let _ = writeln!(f, "{line}");
    }
}

pub const USER_KEY_FILE: &str = "tui-key.pem";
const USER_ONLY_COMMANDS: &[&str] = &["answer", "send", "pair", "devices"];

/// Nodes share the user's account, so they could run the CLI that signs as the user. The
/// plugin's hook refuses it; a determined node could still hide the command, which is why
/// the phone and the Mac app keep their keys in the Secure Enclave.
fn forbidden_for_nodes(tool: Option<&str>, input: Option<&Value>) -> Option<String> {
    let field = |k: &str| input.and_then(|i| i.get(k)).and_then(Value::as_str).unwrap_or("");
    let touches_key = |s: &str| s.contains(USER_KEY_FILE) || (s.contains("Application Support") && s.to_lowercase().contains("alondra"));
    match tool? {
        "Bash" => {
            let command = field("command");
            let words: Vec<&str> = command.split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '`')).filter(|w| !w.is_empty()).collect();
            let runs_user_command = words
                .windows(2)
                .any(|w| (w[0] == "alondra" || w[0].ends_with("/alondra")) && USER_ONLY_COMMANDS.contains(&w[1]));
            if runs_user_command {
                return Some("Nodes cannot run `alondra answer`, `send`, `pair` or `devices`: they act as the user. Use your Alondra MCP tools (ask, report) instead.".into());
            }
            touches_key(command).then(|| "Nodes cannot touch the user's signing key.".into())
        }
        _ => ["file_path", "path", "pattern", "notebook_path"]
            .iter()
            .any(|k| touches_key(field(k)))
            .then(|| "Nodes cannot touch the user's signing key.".into()),
    }
}

fn tool_summary(tool: Option<&str>, input: Option<&Value>) -> Option<String> {
    let input = input?;
    let pick = |k: &str| input.get(k).and_then(Value::as_str).map(str::to_string);
    let s = match tool {
        Some("AskUserQuestion") => input
            .get("questions")
            .and_then(|q| q.get(0))
            .and_then(|q| q.get("question"))
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => pick("command").or_else(|| pick("file_path")).or_else(|| pick("description")).or_else(|| pick("pattern")).or_else(|| pick("url")),
    }?;
    Some(s.chars().take(300).collect())
}

async fn hook(State(hub): State<AppState>, Json(p): Json<Value>) -> Response {
    let Some(session_id) = str_field(&p, "session_id") else {
        return Json(json!({})).into_response();
    };
    let event = str_field(&p, "hook_event_name").unwrap_or_default();
    let tool = str_field(&p, "tool_name");
    let h = HookEvent {
        session_id: session_id.clone(),
        event: event.clone(),
        tool_name: tool.clone(),
        tool_use_id: str_field(&p, "tool_use_id"),
        tool_summary: tool_summary(tool.as_deref(), p.get("tool_input")),
        last_message: str_field(&p, "last_assistant_message"),
        notification_type: str_field(&p, "notification_type"),
        error_type: str_field(&p, "error_type").or_else(|| str_field(&p, "error")),
        cwd: str_field(&p, "cwd"),
        agent_id: str_field(&p, "agent_id"),
    };
    if let Err(e) = hub.append(Event::Hook(h.clone())) {
        eprintln!("alondrad: hook lost: {e}");
        return Json(json!({})).into_response();
    }
    if event == "PreToolUse"
        && let Some(reason) = forbidden_for_nodes(tool.as_deref(), p.get("tool_input"))
    {
        eprintln!("alondrad: blocked a node from acting as the user: {reason}");
        return Json(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny",
            "permissionDecisionReason": reason}})).into_response();
    }
    if event == "PreToolUse" && tool.as_deref().is_some_and(|t| t.starts_with("mcp__plugin_alondra")) {
        return Json(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow",
            "permissionDecisionReason": "Alondra tool"}})).into_response();
    }
    let only_parent_may_ask = h.agent_id.is_some() && event != "PermissionRequest";
    if !hub.hold_enabled || only_parent_may_ask || !crate::watcher::is_background_session(&crate::watcher::claude_home(), &session_id) {
        return Json(json!({})).into_response();
    }
    match (event.as_str(), tool.as_deref()) {
        ("PreToolUse", Some("AskUserQuestion")) => hold(hub, p, h, QuestionOrigin::AskUserQuestion).await,
        ("PreToolUse", Some("ExitPlanMode")) => hold(hub, p, h, QuestionOrigin::Plan).await,
        ("PermissionRequest", _) => hold(hub, p, h, QuestionOrigin::Permission).await,
        _ => Json(json!({})).into_response(),
    }
}

struct HoldGuard {
    hub: Arc<Hub>,
    id: u64,
}

impl Drop for HoldGuard {
    fn drop(&mut self) {
        self.hub.drop_hold(self.id);
    }
}

fn held_question(view: &View, p: &Value, h: &HookEvent, origin: QuestionOrigin) -> Question {
    let input = p.get("tool_input").cloned().unwrap_or(Value::Null);
    let node = view.nodes.get(&h.session_id);
    let (title, detail, options) = match origin {
        QuestionOrigin::AskUserQuestion => {
            let qs = input.get("questions").and_then(Value::as_array).cloned().unwrap_or_default();
            let first = qs.first().cloned().unwrap_or(Value::Null);
            let title = str_field(&first, "question").unwrap_or_else(|| "?".into());
            let options = first
                .get("options")
                .and_then(Value::as_array)
                .map(|o| {
                    o.iter()
                        .map(|x| QuestionOption { label: str_field(x, "label").unwrap_or_default(), description: str_field(x, "description") })
                        .collect()
                })
                .unwrap_or_default();
            let extra: Vec<String> = qs.iter().skip(1).filter_map(|q| str_field(q, "question")).collect();
            let detail = (!extra.is_empty()).then(|| format!("- {}", extra.join("\n- ")));
            (title, detail, options)
        }
        QuestionOrigin::Plan => (
            str_field(&input, "plan")
                .and_then(|p| p.lines().map(|l| l.trim_start_matches('#').trim().to_string()).find(|l| !l.is_empty()))
                .map(|t| t.chars().take(120).collect::<String>())
                .unwrap_or_else(|| h.session_id.chars().take(8).collect()),
            str_field(&input, "plan"),
            vec![
                QuestionOption { label: answer_option::APPROVE.into(), description: None },
                QuestionOption { label: answer_option::REQUEST_CHANGES.into(), description: Some("Write what to change".into()) },
            ],
        ),
        _ => (
            h.tool_summary.clone().map(|t| t.chars().take(120).collect()).unwrap_or_else(|| h.tool_name.clone().unwrap_or_default()),
            Some(h.tool_summary.clone().unwrap_or_else(|| input.to_string())),
            vec![
                QuestionOption { label: answer_option::ALLOW.into(), description: None },
                QuestionOption { label: answer_option::DENY.into(), description: Some("Write the reason".into()) },
            ],
        ),
    };
    Question {
        id: view.questions.keys().next_back().copied().unwrap_or(0) + 1,
        session_id: h.session_id.clone(),
        node_name: node.and_then(|n| n.name.clone()),
        title,
        detail,
        options,
        links: vec![],
        attachments: vec![],
        blocking: true,
        origin,
        tool_use_id: h.tool_use_id.clone(),
        to_orchestrator: false,
        node_context: node.and_then(|n| n.last_message.clone()),
        status: QuestionStatus::Open,
        answer: None,
        asked_at: now_millis(),
    }
}

async fn hold(hub: Arc<Hub>, p: Value, h: HookEvent, origin: QuestionOrigin) -> Response {
    let appended = hub.append_with(|v| Some(Event::QuestionAsked(held_question(v, &p, &h, origin))));
    let id = match appended {
        Ok(Some((_, Event::QuestionAsked(q)))) => q.id,
        Ok(_) => return Json(json!({})).into_response(),
        Err(e) => return internal(e),
    };
    let rx = hub.register_hold(id);
    let _guard = HoldGuard { hub: hub.clone(), id };
    let Ok(answer) = rx.await else {
        return Json(json!({})).into_response();
    };
    let _ = hub.append(Event::QuestionDelivered { id });
    let input = p.get("tool_input").cloned().unwrap_or(Value::Null);
    Json(hold_decision(origin, &answer, input, &hub.config.name)).into_response()
}

fn hold_decision(origin: QuestionOrigin, answer: &Answer, input: Value, user: &str) -> Value {
    let text = match answer.option.as_deref() {
        Some(answer_option::MORE_CONTEXT) | None => answer.text.clone(),
        Some(o) => o.to_string(),
    };
    let wants_context = answer.option.as_deref() == Some(answer_option::MORE_CONTEXT);
    let approves = |yes: &str| answer.option.as_deref() == Some(yes) || answer.text.trim().eq_ignore_ascii_case(yes);
    match origin {
        QuestionOrigin::AskUserQuestion => {
            let qs = input.get("questions").cloned().unwrap_or(json!([]));
            let mut answers = serde_json::Map::new();
            for q in qs.as_array().into_iter().flatten() {
                if let Some(k) = str_field(q, "question") {
                    answers.insert(k, Value::String(text.clone()));
                }
            }
            json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow",
                   "updatedInput": {"questions": qs, "answers": answers}}})
        }
        QuestionOrigin::Plan if approves(answer_option::APPROVE) => json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": input}}),
        QuestionOrigin::Plan => json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
            "permissionDecision": "deny", "permissionDecisionReason": if wants_context { answer.text.clone() } else { format!("{user} requests changes to the plan: {}", answer.text) }}}),
        _ if approves(answer_option::ALLOW) => json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest",
            "decision": {"behavior": "allow"}}}),
        _ => json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest",
            "decision": {"behavior": "deny", "message": if wants_context { answer.text.clone() } else { format!("{user} denies it: {}", answer.text) }}}}),
    }
}

#[derive(Deserialize)]
struct FileQuery {
    path: String,
}

async fn file(State(hub): State<AppState>, Query(q): Query<FileQuery>) -> Response {
    let root = hub.home.join("files");
    let Ok(path) = std::fs::canonicalize(&q.path) else {
        return err(StatusCode::NOT_FOUND, "not found");
    };
    let Ok(root) = std::fs::canonicalize(&root) else {
        return err(StatusCode::NOT_FOUND, "not found");
    };
    if !path.starts_with(&root) {
        return err(StatusCode::FORBIDDEN, "only copies stored by Alondra are served");
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], bytes).into_response(),
        Err(e) => err(StatusCode::NOT_FOUND, e.to_string()),
    }
}

fn local_only(peer: &SocketAddr, headers: &axum::http::HeaderMap) -> Option<Response> {
    (!is_local(peer, headers)).then(|| err(StatusCode::FORBIDDEN, "only from this Mac"))
}

async fn new_pairing(State(hub): State<AppState>, Extension(auth): Extension<Arc<Auth>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: axum::http::HeaderMap) -> Response {
    if let Some(r) = local_only(&peer, &headers) {
        return r;
    }
    let url = hub.config.public_url.clone();
    Json(json!({"code": auth.new_pairing_code(), "expires_in": 600, "url": url})).into_response()
}

#[derive(Deserialize)]
struct PairBody {
    code: String,
    device_name: String,
    public_key: String,
}

async fn pair(Extension(auth): Extension<Arc<Auth>>, Json(b): Json<PairBody>) -> Response {
    match auth.pair(&b.code, &b.device_name, &b.public_key) {
        Ok(d) => Json(json!({"device_id": d.id})).into_response(),
        Err(e) => err(StatusCode::UNAUTHORIZED, e),
    }
}

async fn devices(Extension(auth): Extension<Arc<Auth>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: axum::http::HeaderMap) -> Response {
    if let Some(r) = local_only(&peer, &headers) {
        return r;
    }
    let list: Vec<Value> = auth
        .list()
        .into_iter()
        .map(|d| json!({"id": d.id, "name": d.name, "created_at": d.created_at, "last_seen": d.last_seen}))
        .collect();
    Json(json!({"devices": list})).into_response()
}

async fn revoke_device(Extension(auth): Extension<Arc<Auth>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: axum::http::HeaderMap, Path(id): Path<String>) -> Response {
    if let Some(r) = local_only(&peer, &headers) {
        return r;
    }
    if auth.revoke(&id) { Json(json!({"ok": true})).into_response() } else { err(StatusCode::NOT_FOUND, "not found") }
}

#[derive(Deserialize)]
struct PushRegisterBody {
    token: String,
    environment: Option<String>,
    quiet_start: Option<String>,
    quiet_end: Option<String>,
}

async fn push_register(
    Extension(push): Extension<Arc<Push>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(b): Json<PushRegisterBody>,
) -> Response {
    let device = if is_local(&peer, &headers) {
        headers.get("x-alondra-device").and_then(|v| v.to_str().ok()).unwrap_or("local").to_string()
    } else {
        headers.get("x-alondra-device").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string()
    };
    if b.token.is_empty() || !b.token.chars().all(|c| c.is_ascii_hexdigit()) {
        return err(StatusCode::BAD_REQUEST, "invalid token");
    }
    let reg = Registration {
        token: b.token.to_lowercase(),
        environment: b.environment.unwrap_or_else(|| "sandbox".into()),
        quiet_start: b.quiet_start,
        quiet_end: b.quiet_end,
        updated_at: now_millis(),
    };
    match push.register(&device, reg) {
        Ok(()) => Json(json!({"ok": true, "push_enabled": push.enabled()})).into_response(),
        Err(e) => internal(e),
    }
}

async fn push_check(Extension(push): Extension<Arc<Push>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: axum::http::HeaderMap) -> Response {
    if let Some(r) = local_only(&peer, &headers) {
        return r;
    }
    match push.check().await {
        Ok((status, body)) => Json(json!({"status": status, "body": body})).into_response(),
        Err(e) => err(StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
    }
}

async fn health(State(hub): State<AppState>) -> Json<Value> {
    let c = &hub.config;
    Json(json!({"ok": true, "tick": hub.tick(), "holds": hub.holds_count(), "version": env!("CARGO_PKG_VERSION"),
                "config": {"name": c.name, "port": c.port, "listen": c.listen.to_string(), "public_url": c.public_url,
                           "legacy_bridge": c.legacy_bridge, "wake": c.wake, "retention_hours": c.retention_hours,
                           "home": c.home.display().to_string()}}))
}

async fn overview(State(hub): State<AppState>) -> Json<Value> {
    let machine = hub.machine();
    let body = hub.read(|v| {
        let mut nodes: Vec<&alondra_core::Node> = v.nodes.values().collect();
        nodes.sort_by_key(|n| (!n.is_orchestrator, n.state, std::cmp::Reverse(n.last_seen)));
        let open: Vec<&Question> = v.questions.values().filter(|q| q.status.is_open()).collect();
        let recent_answered: Vec<&Question> = v.questions.values().rev().filter(|q| !q.status.is_open()).take(20).collect();
        let reports: Vec<&Report> = v.reports.values().rev().take(50).collect();
        let news: Vec<&Report> = v.reports.values().rev().filter(|r| !r.read && r.kind != ReportKind::Progress).collect();
        let orchestrator_messages: Vec<&Message> = v
            .nodes
            .values()
            .find(|n| n.is_orchestrator)
            .map(|o| v.messages.values().rev().filter(|m| m.session_id == o.session_id).take(20).collect())
            .unwrap_or_default();
        let nodes: Vec<Value> = nodes
            .into_iter()
            .map(|n| {
                let mut value = serde_json::to_value(n).unwrap_or(Value::Null);
                value["project"] = Value::String(n.project());
                value
            })
            .collect();
        json!({
            "tick": hub.tick(), "seq": v.seq, "nodes": nodes, "open_questions": open, "news": news,
            "orchestrator_messages": orchestrator_messages,
            "recent_questions": recent_answered, "reports": reports, "machine": machine,
            "user": hub.config.name,
        })
    });
    Json(body)
}

#[derive(Deserialize)]
struct ChangesQuery {
    after: Option<u64>,
    wait: Option<u64>,
}

async fn changes(State(hub): State<AppState>, Query(q): Query<ChangesQuery>) -> Json<Value> {
    let wait = Duration::from_secs(q.wait.unwrap_or(50)).min(MAX_WAIT);
    let tick = hub.wait_change(q.after.unwrap_or(0), wait).await;
    Json(json!({ "tick": tick }))
}

async fn node_detail(State(hub): State<AppState>, Path(key): Path<String>) -> Response {
    let found = hub.read(|v| {
        let n = v.resolve_node(&key)?.clone();
        let sid = n.session_id.clone();
        let questions: Vec<Question> = v.questions.values().filter(|q| q.session_id == sid).cloned().collect();
        let reports: Vec<Report> = v.reports.values().filter(|r| r.session_id == sid).cloned().collect();
        let messages: Vec<Message> = v.messages.values().filter(|m| m.session_id == sid).cloned().collect();
        Some((n, questions, reports, messages))
    });
    let Some((node, questions, reports, messages)) = found else {
        return err(StatusCode::NOT_FOUND, format!("unknown node {key}"));
    };
    let timeline: Vec<Value> = hub
        .timeline(&node.session_id, 40)
        .unwrap_or_default()
        .into_iter()
        .map(|(seq, at, body)| json!({"seq": seq, "at": at, "event": serde_json::from_str::<Value>(&body).unwrap_or(Value::Null)}))
        .collect();
    Json(json!({"node": node, "questions": questions, "reports": reports, "messages": messages, "timeline": timeline})).into_response()
}

#[derive(Deserialize)]
struct Caller {
    session_id: Option<String>,
    #[serde(default)]
    pids: Vec<i32>,
}

fn resolve_caller(view: &View, c: &Caller) -> Option<(String, Option<String>)> {
    if let Some(sid) = &c.session_id
        && !sid.is_empty()
    {
        let node = view.nodes.get(sid)?;
        return Some((sid.clone(), node.name.clone()));
    }
    view.nodes
        .values()
        .filter(|n| n.alive)
        .find(|n| n.pid.is_some_and(|p| c.pids.contains(&p)))
        .map(|n| (n.session_id.clone(), n.name.clone()))
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OptionIn {
    Label(String),
    Full(QuestionOption),
}

impl From<OptionIn> for QuestionOption {
    fn from(o: OptionIn) -> Self {
        match o {
            OptionIn::Label(label) => QuestionOption { label, description: None },
            OptionIn::Full(q) => q,
        }
    }
}

#[derive(Deserialize)]
struct AskBody {
    #[serde(flatten)]
    caller: Caller,
    title: String,
    detail: Option<String>,
    #[serde(default)]
    options: Vec<OptionIn>,
    #[serde(default)]
    links: Vec<Link>,
    #[serde(default)]
    attachments: Vec<String>,
    #[serde(default = "yes")]
    blocking: bool,
    #[serde(default)]
    to_orchestrator: bool,
}

fn yes() -> bool {
    true
}

fn store_attachments(home: &std::path::Path, prefix: &str, paths: &[String]) -> Vec<Attachment> {
    paths
        .iter()
        .map(|src| {
            let path = PathBuf::from(src);
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| src.clone());
            let bytes = std::fs::metadata(&path).ok().map(|m| m.len());
            let stored = bytes.filter(|b| *b <= MAX_ATTACHMENT_BYTES).and_then(|_| {
                let dir = home.join("files").join(prefix);
                std::fs::create_dir_all(&dir).ok()?;
                let dest = dir.join(&name);
                std::fs::copy(&path, &dest).ok()?;
                Some(dest.to_string_lossy().to_string())
            });
            Attachment { name, source_path: src.clone(), stored_path: stored, bytes }
        })
        .collect()
}

async fn ask(State(hub): State<AppState>, Json(mut b): Json<AskBody>) -> Response {
    let stamp = now_millis();
    let options: Vec<QuestionOption> = std::mem::take(&mut b.options).into_iter().map(Into::into).collect();
    let attachments = store_attachments(&hub.home, &format!("q-{stamp}"), &b.attachments);
    let res = hub.append_with(|v| {
        let (sid, name) = resolve_caller(v, &b.caller)?;
        let ctx = v.nodes.get(&sid).and_then(|n| n.tool.as_ref().map(|t| format!("{} {}", t.name, t.summary.clone().unwrap_or_default())).or(n.last_message.clone()));
        Some(Event::QuestionAsked(Question {
            id: v.questions.keys().next_back().copied().unwrap_or(0) + 1,
            session_id: sid,
            node_name: name,
            title: b.title.clone(),
            detail: b.detail.clone(),
            options: options.clone(),
            links: b.links.clone(),
            attachments: attachments.clone(),
            blocking: b.blocking,
            origin: QuestionOrigin::Ask,
            tool_use_id: None,
            to_orchestrator: b.to_orchestrator,
            node_context: ctx,
            status: QuestionStatus::Open,
            answer: None,
            asked_at: stamp,
        }))
    });
    match res {
        Ok(Some((_, Event::QuestionAsked(q)))) => Json(json!({"id": q.id})).into_response(),
        Ok(_) => err(StatusCode::BAD_REQUEST, "unknown asking node: session_id is missing and no pid matches"),
        Err(e) => internal(e),
    }
}

async fn question_detail(State(hub): State<AppState>, Path(id): Path<u64>) -> Response {
    match hub.read(|v| v.questions.get(&id).cloned().map(|q| (q.clone(), v.nodes.get(&q.session_id).cloned()))) {
        Some((q, node)) => Json(json!({"question": q, "node": node})).into_response(),
        None => err(StatusCode::NOT_FOUND, format!("question #{id} does not exist")),
    }
}

#[derive(Deserialize)]
struct AnswerBody {
    text: String,
    option: Option<String>,
    by: Option<String>,
}

impl AnswerBody {
    fn canonicalize(&mut self) {
        self.option = self.option.as_deref().map(|o| answer_option::canonical(o).to_string());
        self.text = answer_option::canonical(&self.text).to_string();
    }
}

async fn answer(State(hub): State<AppState>, signed: Option<Extension<crate::auth::Signed>>, Path(id): Path<u64>, Json(mut b): Json<AnswerBody>) -> Response {
    let signed = signed.map(|Extension(s)| s);
    b.canonicalize();
    let Some(q) = hub.read(|v| v.questions.get(&id).cloned()) else {
        return err(StatusCode::NOT_FOUND, format!("question #{id} does not exist"));
    };
    if !q.status.is_open() {
        return err(StatusCode::CONFLICT, format!("question #{id} is no longer open"));
    }
    let device = signed.as_ref().map_or_else(|| "local".to_string(), |s| s.name.clone());
    let provenance = match &signed {
        Some(s) => format!("via {} (signature verified)", s.name),
        None => "via the Mac (local, unsigned)".to_string(),
    };
    let user = hub.config.name.clone();
    let by = b.by.clone().or_else(|| Some(user.clone()));
    let answer = Answer { text: b.text.clone(), option: b.option.clone(), by, device: Some(device), at: now_millis() };
    if let Err(e) = hub.append(Event::QuestionAnswered { id, answer: answer.clone() }) {
        return internal(e);
    }
    if hub.release_hold(id, answer.clone()) {
        return Json(json!({"id": id, "delivered": "hook"})).into_response();
    }
    let chosen = b.option.clone().filter(|o| o.trim() != b.text.trim()).map(|o| format!("{o}. ")).unwrap_or_default();
    let other_open = hub.read(|v| v.questions.values().any(|o| o.id != id && o.session_id == q.session_id && o.status.is_open()));
    let text = if other_open { format!("About \"{}\": {chosen}{}", q.title, b.text) } else { format!("{chosen}{}", b.text) };
    eprintln!("alondrad: answer to #{id} from {user} {provenance}");
    let res = hub.append_with(|v| {
        Some(Event::MessageQueued(Message {
            id: v.messages.keys().next_back().copied().unwrap_or(0) + 1,
            session_id: q.session_id.clone(),
            from: MessageFrom::User,
            text: text.clone(),
            answers_question: Some(id),
            created_at: now_millis(),
            handed_at: None,
            delivered_at: None,
        }))
    });
    match res {
        Ok(_) => Json(json!({"id": id, "delivered": "inbox"})).into_response(),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct MoreContextBody {
    note: Option<String>,
}

async fn more_context(State(hub): State<AppState>, signed: Option<Extension<crate::auth::Signed>>, Path(id): Path<u64>, Json(b): Json<MoreContextBody>) -> Response {
    let note = b.note.filter(|n| !n.trim().is_empty()).map(|n| format!(" What is missing: {n}.")).unwrap_or_default();
    let user = hub.config.name.clone();
    let text = format!(
        "{user} needs more context before deciding.{note} Explain what is at stake, the options with their consequences and your recommendation, and ask again."
    );
    answer(State(hub), signed, Path(id), Json(AnswerBody { text, option: Some(answer_option::MORE_CONTEXT.into()), by: None })).await
}

async fn withdraw(State(hub): State<AppState>, Path(id): Path<u64>) -> Response {
    match hub.append(Event::QuestionClosed { id, status: QuestionStatus::Withdrawn }) {
        Ok(_) => Json(json!({"id": id})).into_response(),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct ReportBody {
    #[serde(flatten)]
    caller: Caller,
    title: String,
    body: Option<String>,
    kind: Option<ReportKind>,
    #[serde(default)]
    links: Vec<Link>,
    #[serde(default)]
    attachments: Vec<String>,
}

async fn report(State(hub): State<AppState>, Json(b): Json<ReportBody>) -> Response {
    if b.kind != Some(ReportKind::Progress) {
        let text = format!("{}\n\n{}", b.title, b.body.as_deref().unwrap_or(""));
        let user = &hub.config.name;
        match crate::classify::judge_report(&text, user).await {
            crate::classify::ReportVerdict::Asks => {
                eprintln!("alondrad: report gate: rejected a report that asks for something: {}", b.title);
                keep_rejected(&hub.home, &b.title, b.body.as_deref());
                return err(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!(
                        "This report asks {user} to decide or do something, and a report is only for reading. Send each \
                         decision as its own question with ask (a one-line title, context in detail, options) and keep \
                         only the informative part in report. If the question is already open in Alondra, do not repeat it."
                    ),
                );
            }
            crate::classify::ReportVerdict::Fine => {}
        }
    }
    let stamp = now_millis();
    let attachments = store_attachments(&hub.home, &format!("r-{stamp}"), &b.attachments);
    let res = hub.append_with(|v| {
        let (sid, name) = resolve_caller(v, &b.caller)?;
        Some(Event::ReportPosted(Report {
            id: v.reports.keys().next_back().copied().unwrap_or(0) + 1,
            session_id: sid,
            node_name: name,
            title: b.title.clone(),
            body: b.body.clone(),
            links: b.links.clone(),
            attachments: attachments.clone(),
            kind: b.kind.unwrap_or(ReportKind::Notice),
            created_at: stamp,
            read: false,
        }))
    });
    match res {
        Ok(Some((_, Event::ReportPosted(r)))) => Json(json!({"id": r.id})).into_response(),
        Ok(_) => err(StatusCode::BAD_REQUEST, "unknown reporting node"),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct WaitingMachineBody {
    #[serde(flatten)]
    caller: Caller,
    reason: String,
}

async fn waiting_machine(State(hub): State<AppState>, Json(b): Json<WaitingMachineBody>) -> Response {
    let res = hub.append_with(|v| {
        let (session_id, _) = resolve_caller(v, &b.caller)?;
        Some(Event::WaitingMachine { session_id, reason: b.reason.clone() })
    });
    match res {
        Ok(Some(_)) => Json(json!({"ok": true})).into_response(),
        Ok(None) => err(StatusCode::BAD_REQUEST, "unknown node waiting for the machine"),
        Err(e) => internal(e),
    }
}

async fn report_read(State(hub): State<AppState>, Path(id): Path<u64>) -> Response {
    match hub.append(Event::ReportRead { id }) {
        Ok(_) => Json(json!({"id": id})).into_response(),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct MessageBody {
    text: String,
    from: Option<MessageFrom>,
}

async fn send_message(State(hub): State<AppState>, Path(key): Path<String>, Json(b): Json<MessageBody>) -> Response {
    let res = hub.append_with(|v| {
        let node = v.resolve_node(&key)?;
        Some(Event::MessageQueued(Message {
            id: v.messages.keys().next_back().copied().unwrap_or(0) + 1,
            session_id: node.session_id.clone(),
            from: b.from.unwrap_or(MessageFrom::User),
            text: b.text.clone(),
            answers_question: None,
            created_at: now_millis(),
            handed_at: None,
            delivered_at: None,
        }))
    });
    match res {
        Ok(Some((_, Event::MessageQueued(m)))) => Json(json!({"id": m.id, "session_id": m.session_id})).into_response(),
        Ok(_) => err(StatusCode::NOT_FOUND, format!("unknown node {key}")),
        Err(e) => internal(e),
    }
}

async fn close_node(State(hub): State<AppState>, Path(key): Path<String>) -> Response {
    let Some((short, session)) = hub.read(|v| v.resolve_node(&key).map(|n| (n.short_id.clone(), n.session_id.clone()))) else {
        return err(StatusCode::NOT_FOUND, format!("unknown node {key}"));
    };
    let run = tokio::process::Command::new("claude").args(["rm", &short]).kill_on_drop(true).output();
    match tokio::time::timeout(Duration::from_secs(60), run).await {
        Ok(Ok(out)) => {
            let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)).trim().to_string();
            if text.contains(&format!("removed {short}")) {
                let _ = hub.append(Event::NodeRemoved { session_id: session });
                Json(json!({"ok": true, "output": text})).into_response()
            } else {
                err(StatusCode::CONFLICT, text)
            }
        }
        Ok(Err(e)) => err(StatusCode::BAD_GATEWAY, e.to_string()),
        Err(_) => err(StatusCode::GATEWAY_TIMEOUT, "claude rm did not answer in 60 s"),
    }
}

async fn forget_node(State(hub): State<AppState>, Path(key): Path<String>) -> Response {
    let res = hub.append_with(|v| {
        let n = v.resolve_node(&key)?;
        (!n.in_daemon || !n.alive).then(|| Event::NodeRemoved { session_id: n.session_id.clone() })
    });
    match res {
        Ok(Some(_)) => Json(json!({"ok": true})).into_response(),
        Ok(None) => err(StatusCode::CONFLICT, format!("{key} does not exist or is still alive in claude agents: close it with claude rm")),
        Err(e) => internal(e),
    }
}

async fn acknowledge(State(hub): State<AppState>, Path(key): Path<String>) -> Response {
    let res = hub.append_with(|v| v.resolve_node(&key).map(|n| Event::Acknowledged { session_id: n.session_id.clone() }));
    match res {
        Ok(Some(_)) => Json(json!({"ok": true})).into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, format!("unknown node {key}")),
        Err(e) => internal(e),
    }
}

async fn mark_orchestrator(State(hub): State<AppState>, Json(c): Json<Caller>) -> Response {
    let res = hub.append_with(|v| resolve_caller(v, &c).map(|(sid, _)| Event::MarkedOrchestrator { session_id: sid }));
    match res {
        Ok(Some(_)) => Json(json!({"ok": true})).into_response(),
        Ok(None) => err(StatusCode::BAD_REQUEST, "unknown orchestrator session"),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct RequestBody {
    text: String,
}

async fn new_request(State(hub): State<AppState>, Json(b): Json<RequestBody>) -> Response {
    let text = format!("[Alondra] New request from {}: {}\n\nPrepare the mission and launch the node with Alondra's launch tool, or use ask if something is missing.", hub.config.name, b.text);
    let res = hub.append_with(|v| {
        let o = v.nodes.values().find(|n| n.is_orchestrator)?;
        Some(Event::MessageQueued(Message {
            id: v.messages.keys().next_back().copied().unwrap_or(0) + 1,
            session_id: o.session_id.clone(),
            from: MessageFrom::User,
            text: text.clone(),
            answers_question: None,
            created_at: now_millis(),
            handed_at: None,
            delivered_at: None,
        }))
    });
    match res {
        Ok(Some(_)) => Json(json!({"ok": true})).into_response(),
        Ok(None) => err(StatusCode::CONFLICT, "the orchestrator is not connected to Alondra"),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

async fn search(State(hub): State<AppState>, Query(sq): Query<SearchQuery>) -> Json<Value> {
    let q = sq.q.trim().to_lowercase();
    if q.is_empty() {
        return Json(json!({"via": "none", "sessions": []}));
    }
    let candidates: Vec<(String, String)> = hub.read(|v| {
        v.nodes
            .values()
            .map(|n| {
                let text = format!(
                    "Node {} of project {}. State: {}. {}",
                    n.display_name(),
                    n.project(),
                    n.state.label(),
                    n.last_message.clone().or(n.daemon_detail.clone()).unwrap_or_default().chars().take(600).collect::<String>()
                );
                (n.session_id.clone(), text)
            })
            .collect()
    });
    let literal: Vec<&String> = candidates
        .iter()
        .filter(|(_, t)| q.split_whitespace().all(|w| t.to_lowercase().contains(w)))
        .map(|(s, _)| s)
        .collect();
    if !literal.is_empty() {
        return Json(json!({"via": "text", "sessions": literal}));
    }
    let matches = crate::classify::search(&sq.q, candidates).await;
    Json(json!({"via": "jev", "sessions": matches}))
}

#[derive(Deserialize)]
struct LaunchBody {
    name: String,
    cwd: String,
    prompt: String,
    model: Option<String>,
    permission_mode: Option<String>,
}

async fn launch(Json(b): Json<LaunchBody>) -> Response {
    if !std::path::Path::new(&b.cwd).is_dir() {
        return err(StatusCode::BAD_REQUEST, format!("directory {} does not exist", b.cwd));
    }
    let mut cmd = tokio::process::Command::new("claude");
    cmd.current_dir(&b.cwd)
        .args(["--bg", "--name", &b.name])
        .args(["--permission-mode", b.permission_mode.as_deref().unwrap_or("auto")]);
    if let Some(m) = &b.model {
        cmd.args(["--model", m]);
    }
    cmd.arg("--").arg(&b.prompt).kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(60), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            Json(json!({"ok": true, "output": stdout})).into_response()
        }
        Ok(Ok(out)) => err(StatusCode::BAD_GATEWAY, String::from_utf8_lossy(&out.stderr).to_string()),
        Ok(Err(e)) => err(StatusCode::BAD_GATEWAY, e.to_string()),
        Err(_) => err(StatusCode::GATEWAY_TIMEOUT, "claude --bg did not respond within 60 s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answered(text: &str, option: Option<&str>) -> Answer {
        let mut b = AnswerBody { text: text.into(), option: option.map(Into::into), by: None };
        b.canonicalize();
        Answer { text: b.text, option: b.option, by: None, device: None, at: 0 }
    }

    fn decision(origin: QuestionOrigin, a: &Answer) -> Value {
        hold_decision(origin, a, json!({"plan": "p"}), "Ana")
    }

    #[test]
    fn the_installed_app_still_approves_and_allows_with_the_spanish_labels() {
        for a in [answered("Aprobar", Some("Aprobar")), answered("Approve", Some("Approve"))] {
            assert_eq!(decision(QuestionOrigin::Plan, &a)["hookSpecificOutput"]["permissionDecision"], "allow");
        }
        for a in [answered("Permitir", Some("Permitir")), answered("Allow", Some("Allow"))] {
            assert_eq!(decision(QuestionOrigin::Permission, &a)["hookSpecificOutput"]["decision"]["behavior"], "allow");
        }
    }

    #[test]
    fn spanish_labels_are_stored_as_the_english_ones() {
        let a = answered("Denegar", Some("Denegar"));
        assert_eq!((a.text.as_str(), a.option.as_deref()), ("Deny", Some(answer_option::DENY)));
        let out = decision(QuestionOrigin::Permission, &a);
        assert_eq!(out["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert_eq!(out["hookSpecificOutput"]["decision"]["message"], "Ana denies it: Deny");
    }

    #[test]
    fn the_legacy_more_context_option_passes_the_request_through_untouched() {
        let a = answered("Explain the risk", Some("Necesito más contexto"));
        let out = decision(QuestionOrigin::Plan, &a);
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(out["hookSpecificOutput"]["permissionDecisionReason"], "Explain the risk");
    }

    #[test]
    fn free_text_is_not_mistaken_for_an_option() {
        let a = answered("Pedir cambios en el paso 2", None);
        assert_eq!(a.text, "Pedir cambios en el paso 2");
        assert_eq!(decision(QuestionOrigin::Plan, &a)["hookSpecificOutput"]["permissionDecision"], "deny");
    }
}

#[cfg(test)]
mod node_guard_tests {
    use super::*;

    fn bash(c: &str) -> Option<String> {
        forbidden_for_nodes(Some("Bash"), Some(&json!({"command": c})))
    }

    #[test]
    fn nodes_cannot_answer_as_the_user() {
        assert!(bash("alondra answer 55 Mergea").is_some());
        assert!(bash("cd /tmp && ~/.local/bin/alondra send orquestador hola").is_some());
        assert!(bash("cat ~/.alondra/tui-key.pem").is_some());
        assert!(bash("ls ~/Library/'Application Support'/com.bitomule.alondra.mac").is_some());
        assert!(forbidden_for_nodes(Some("Read"), Some(&json!({"file_path": "/Users/x/.alondra/tui-key.pem"}))).is_some());
    }

    #[test]
    fn ordinary_commands_pass() {
        assert!(bash("alondra status").is_none());
        assert!(bash("alondra doctor --json").is_none());
        assert!(bash("git commit -m 'answer the question'").is_none());
        assert!(forbidden_for_nodes(Some("Read"), Some(&json!({"file_path": "/tmp/x.md"}))).is_none());
    }
}
