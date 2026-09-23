use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alondra_core::{
    Answer, Attachment, Event, HookEvent, Link, Message, MessageFrom, Question, QuestionOption,
    QuestionOrigin, QuestionStatus, Report, ReportKind, View, now_millis,
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
use crate::classify;
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
        .route("/v1/inbox/{session}", get(inbox))
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
        eprintln!("alondrad: hook perdido: {e}");
        return Json(json!({})).into_response();
    }
    if event == "Stop" {
        classify::spawn_if_unclear(&hub, &session_id);
    }
    if event == "PreToolUse" && tool.as_deref().is_some_and(|t| t.starts_with("mcp__plugin_alondra")) {
        return Json(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow",
            "permissionDecisionReason": "Herramienta de Alondra"}})).into_response();
    }
    let in_subagent = h.agent_id.is_some();
    if !hub.hold_enabled || in_subagent || !crate::watcher::is_background_session(&crate::watcher::claude_home(), &session_id) {
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
            let title = str_field(&first, "question").unwrap_or_else(|| "Pregunta".into());
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
            let detail = (!extra.is_empty()).then(|| format!("También pregunta:\n- {}", extra.join("\n- ")));
            (title, detail, options)
        }
        QuestionOrigin::Plan => (
            str_field(&input, "plan")
                .and_then(|p| p.lines().map(|l| l.trim_start_matches('#').trim().to_string()).find(|l| !l.is_empty()))
                .map(|t| format!("Plan: {}", t.chars().take(120).collect::<String>()))
                .unwrap_or_else(|| "Aprobar el plan".to_string()),
            str_field(&input, "plan"),
            vec![
                QuestionOption { label: "Aprobar".into(), description: None },
                QuestionOption { label: "Pedir cambios".into(), description: Some("Escribe qué cambiar".into()) },
            ],
        ),
        _ => (
            format!("Permiso: {}", h.tool_name.clone().unwrap_or_default()),
            Some(h.tool_summary.clone().unwrap_or_else(|| input.to_string())),
            vec![
                QuestionOption { label: "Permitir".into(), description: None },
                QuestionOption { label: "Denegar".into(), description: Some("Escribe el motivo".into()) },
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
    let text = match answer.option.as_deref() {
        Some("Necesito más contexto") | None => answer.text.clone(),
        Some(o) => o.to_string(),
    };
    let wants_context = answer.option.as_deref() == Some("Necesito más contexto");
    let user = &hub.config.name;
    let approves = |yes: &str| answer.option.as_deref() == Some(yes) || answer.text.trim().eq_ignore_ascii_case(yes);
    let body = match origin {
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
        QuestionOrigin::Plan if approves("Aprobar") => json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": input}}),
        QuestionOrigin::Plan => json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
            "permissionDecision": "deny", "permissionDecisionReason": if wants_context { answer.text.clone() } else { format!("{user} pide cambios en el plan: {}", answer.text) }}}),
        _ if approves("Permitir") => json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest",
            "decision": {"behavior": "allow"}}}),
        _ => json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest",
            "decision": {"behavior": "deny", "message": if wants_context { answer.text.clone() } else { format!("{user} lo deniega: {}", answer.text) }}}}),
    };
    Json(body).into_response()
}

#[derive(Deserialize)]
struct FileQuery {
    path: String,
}

async fn file(State(hub): State<AppState>, Query(q): Query<FileQuery>) -> Response {
    let root = hub.home.join("files");
    let Ok(path) = std::fs::canonicalize(&q.path) else {
        return err(StatusCode::NOT_FOUND, "no existe");
    };
    let Ok(root) = std::fs::canonicalize(&root) else {
        return err(StatusCode::NOT_FOUND, "no existe");
    };
    if !path.starts_with(&root) {
        return err(StatusCode::FORBIDDEN, "solo sirvo copias guardadas por Alondra");
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], bytes).into_response(),
        Err(e) => err(StatusCode::NOT_FOUND, e.to_string()),
    }
}

fn local_only(peer: &SocketAddr, headers: &axum::http::HeaderMap) -> Option<Response> {
    (!is_local(peer, headers)).then(|| err(StatusCode::FORBIDDEN, "solo desde el propio Mac"))
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
    if auth.revoke(&id) { Json(json!({"ok": true})).into_response() } else { err(StatusCode::NOT_FOUND, "no existe") }
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
        return err(StatusCode::BAD_REQUEST, "token no válido");
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
    let listening = hub.inbox_listening();
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
            "inbox_listening": listening, "user": hub.config.name,
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
        return err(StatusCode::NOT_FOUND, format!("no conozco el nodo {key}"));
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
        Ok(_) => err(StatusCode::BAD_REQUEST, "no sé qué nodo pregunta: falta session_id y ningún pid coincide"),
        Err(e) => internal(e),
    }
}

async fn question_detail(State(hub): State<AppState>, Path(id): Path<u64>) -> Response {
    match hub.read(|v| v.questions.get(&id).cloned().map(|q| (q.clone(), v.nodes.get(&q.session_id).cloned()))) {
        Some((q, node)) => Json(json!({"question": q, "node": node})).into_response(),
        None => err(StatusCode::NOT_FOUND, format!("no existe la pregunta #{id}")),
    }
}

#[derive(Deserialize)]
struct AnswerBody {
    text: String,
    option: Option<String>,
    by: Option<String>,
}

async fn answer(State(hub): State<AppState>, signed: Option<Extension<crate::auth::Signed>>, Path(id): Path<u64>, Json(b): Json<AnswerBody>) -> Response {
    let signed = signed.map(|Extension(s)| s);
    let Some(q) = hub.read(|v| v.questions.get(&id).cloned()) else {
        return err(StatusCode::NOT_FOUND, format!("no existe la pregunta #{id}"));
    };
    if !q.status.is_open() {
        return err(StatusCode::CONFLICT, format!("la pregunta #{id} ya no está abierta"));
    }
    let device = signed.as_ref().map_or_else(|| "local".to_string(), |s| s.name.clone());
    let provenance = match &signed {
        Some(s) => format!("desde {} (firmada)", s.name),
        None => "desde el Mac (local, sin firma)".to_string(),
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
    let text = format!("[Alondra] Respuesta de {user} {provenance} a tu pregunta #{id} «{}»: {chosen}{}", q.title, b.text);
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
    let note = b.note.filter(|n| !n.trim().is_empty()).map(|n| format!(" Lo que le falta: {n}.")).unwrap_or_default();
    let user = hub.config.name.clone();
    let text = format!(
        "{user} necesita más contexto antes de decidir.{note} Explica lo que está en juego, las opciones con sus consecuencias y tu recomendación, y vuelve a preguntar."
    );
    answer(State(hub), signed, Path(id), Json(AnswerBody { text, option: Some("Necesito más contexto".into()), by: None })).await
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
                return err(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!(
                        "This report asks {user} to decide or do something, and a report is only for reading. Send each \
                         decision as its own question with ask (a one-line title, context in detail, options) and keep \
                         only the informative part in report. If the question is already open in Alondra, do not repeat it."
                    ),
                );
            }
            crate::classify::ReportVerdict::Progress => {
                eprintln!("alondrad: report gate: rejected a progress-only report: {}", b.title);
                return err(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!(
                        "This report only says the work is still under way and you do not need {user} yet. That is noise \
                         for {user}: do not send it, or send it with kind progress, which stays out of the inbox. If it \
                         does carry something {user} must know now (a result, a finding, a failure), rewrite the title \
                         to say that thing and send it again."
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
        Ok(_) => err(StatusCode::BAD_REQUEST, "no sé qué nodo informa"),
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
        Ok(None) => err(StatusCode::BAD_REQUEST, "no sé qué nodo espera máquina"),
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
        Ok(_) => err(StatusCode::NOT_FOUND, format!("no conozco el nodo {key}")),
        Err(e) => internal(e),
    }
}

async fn forget_node(State(hub): State<AppState>, Path(key): Path<String>) -> Response {
    let res = hub.append_with(|v| {
        let n = v.resolve_node(&key)?;
        (!n.in_daemon || !n.alive).then(|| Event::NodeRemoved { session_id: n.session_id.clone() })
    });
    match res {
        Ok(Some(_)) => Json(json!({"ok": true})).into_response(),
        Ok(None) => err(StatusCode::CONFLICT, format!("{key} no existe o sigue vivo en claude agents: ciérralo con claude rm")),
        Err(e) => internal(e),
    }
}

async fn acknowledge(State(hub): State<AppState>, Path(key): Path<String>) -> Response {
    let res = hub.append_with(|v| v.resolve_node(&key).map(|n| Event::Acknowledged { session_id: n.session_id.clone() }));
    match res {
        Ok(Some(_)) => Json(json!({"ok": true})).into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, format!("no conozco el nodo {key}")),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct InboxQuery {
    wait: Option<u64>,
}

async fn inbox(State(hub): State<AppState>, Path(session): Path<String>, Query(q): Query<InboxQuery>) -> Response {
    let generation = hub.claim_inbox(&session);
    let deadline = Instant::now() + Duration::from_secs(q.wait.unwrap_or(3600).min(24 * 3600));
    loop {
        if !hub.inbox_current(&session, generation) {
            return (StatusCode::NO_CONTENT, "superseded").into_response();
        }
        let pending: Vec<Message> = hub.read(|v| v.pending_messages(&session).cloned().collect());
        if !pending.is_empty() {
            for m in &pending {
                let _ = hub.append(Event::MessageHanded { id: m.id });
                if let Some(qid) = m.answers_question {
                    let _ = hub.append(Event::QuestionDelivered { id: qid });
                }
            }
            hub.release_inbox(&session, generation);
            let text = pending.iter().map(|m| m.text.as_str()).collect::<Vec<_>>().join("\n\n");
            return (StatusCode::OK, text).into_response();
        }
        let now = Instant::now();
        if now >= deadline {
            hub.release_inbox(&session, generation);
            return StatusCode::NO_CONTENT.into_response();
        }
        hub.wait_event((deadline - now).min(Duration::from_secs(30))).await;
    }
}

async fn mark_orchestrator(State(hub): State<AppState>, Json(c): Json<Caller>) -> Response {
    let res = hub.append_with(|v| resolve_caller(v, &c).map(|(sid, _)| Event::MarkedOrchestrator { session_id: sid }));
    match res {
        Ok(Some(_)) => Json(json!({"ok": true})).into_response(),
        Ok(None) => err(StatusCode::BAD_REQUEST, "no sé qué sesión es el orquestador"),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct RequestBody {
    text: String,
}

async fn new_request(State(hub): State<AppState>, Json(b): Json<RequestBody>) -> Response {
    let text = format!("[Alondra] Petición nueva de {}: {}\n\nPrepara la misión y lanza el nodo con la herramienta launch de Alondra, o pregúntale con ask si falta algo.", hub.config.name, b.text);
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
        Ok(None) => err(StatusCode::CONFLICT, "el orquestador no está conectado a Alondra"),
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
                    "Nodo {} del proyecto {}. Estado: {}. {}",
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
        return Json(json!({"via": "texto", "sessions": literal}));
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
        return err(StatusCode::BAD_REQUEST, format!("no existe el directorio {}", b.cwd));
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
        Err(_) => err(StatusCode::GATEWAY_TIMEOUT, "claude --bg no respondió en 60 s"),
    }
}
