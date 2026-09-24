use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use alondra_core::{
    Answer, Attachment, CardRef, Event, HookEvent, Link, Message, MessageAbout, MessageFrom, Question, QuestionOption,
    QuestionOrigin, QuestionStatus, Report, ReportKind, ReviewVerdict, View, answer_option, now_millis, states,
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
use crate::uploads;

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
        .route("/v1/nodes/{key}/interrupt", post(interrupt))
        .route("/v1/nodes/{key}/ack", post(acknowledge))
        .route("/v1/questions", post(ask))
        .route("/v1/actions", post(request_action))
        .route("/v1/questions/{id}", get(question_detail))
        .route("/v1/questions/{id}/answer", post(answer))
        .route("/v1/questions/{id}/withdraw", post(withdraw))
        .route("/v1/questions/{id}/more-context", post(more_context))
        .route("/v1/reports", post(report))
        .route("/v1/reports/{id}/read", post(report_read))
        .route("/v1/reports/{id}/review", post(review_artifact))
        .route("/v1/waiting-machine", post(waiting_machine))
        .route("/v1/orchestrator", post(mark_orchestrator))
        .route("/v1/launch", post(launch))
        .route("/v1/requests", post(new_request))
        .route("/v1/options", get(options_catalog))
        .route("/v1/projects", get(projects))
        .route("/v1/search", get(search))
        .route("/v1/health", get(health))
        .route("/v1/activity", post(activity))
        .route("/v1/prs/{id}/merge", post(merge_pr))
        .route("/v1/prs/{id}/changes", post(request_pr_changes))
        .route("/v1/prs/{id}/resolve-conflicts", post(resolve_pr_conflicts))
        .route("/v1/prs/{id}/dismiss", post(dismiss_pr))
        .route("/v1/prs/{id}/diff", get(pr_diff))
        .route("/v1/file", get(file))
        .route("/v1/uploads", post(upload).layer(axum::extract::DefaultBodyLimit::max(uploads::MAX_UPLOAD_BYTES + 64 * 1024)))
        .layer(Extension(auth))
        .layer(Extension(push))
        .with_state(hub)
}

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

/// A machine-readable reason a close was refused, so each client can show it in the user's
/// language instead of the English `error` string (kept for logs and older clients).
#[derive(serde::Serialize, Debug, PartialEq)]
struct CloseReason {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

fn err_reasons(msg: impl Into<String>, reasons: Vec<CloseReason>) -> Response {
    (StatusCode::CONFLICT, Json(json!({ "error": msg.into(), "reasons": reasons }))).into_response()
}

/// A refusal the clients word in the user's language from `kind`; `error` stays for logs and
/// older clients.
fn refused(status: StatusCode, kind: &'static str, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into(), "kind": kind }))).into_response()
}

fn internal(e: anyhow::Error) -> Response {
    err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn str_field(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

fn keep_flagged(home: &std::path::Path, title: &str, body: Option<&str>) {
    use std::io::Write;
    let line = json!({"at": now_millis(), "title": title, "body": body}).to_string();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(home.join("flagged-reports.jsonl")) {
        let _ = writeln!(f, "{line}");
    }
}

/// Final reports and the orchestrator's notices skip the report gate: they close a node or
/// relay what the orchestrator checked, and a false positive cost the user exactly those.
fn gated(kind: Option<ReportKind>, from_orchestrator: bool) -> bool {
    !from_orchestrator && !matches!(kind, Some(ReportKind::Progress | ReportKind::Final))
}

fn flagged_note(user: &str) -> String {
    format!(
        "Delivered, marked as possibly asking {user} for something. If it does, send each decision as its own question \
         with ask (a one-line title, context in detail, options); if that question is already open, do not repeat it."
    )
}

fn shown_in_pr_note(number: u64, user: &str) -> String {
    format!(
        "Shown inside the card of PR #{number}, not as a card of its own: {user} reads it there when they review the PR. \
         Do not report the PR again."
    )
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
        prompt: str_field(&p, "prompt").map(|t| t.chars().take(2000).collect()),
    };
    if let Err(e) = hub.append(Event::Hook(h.clone())) {
        log!("hook lost: {e}");
        return Json(json!({})).into_response();
    }
    if event == "PostToolUse"
        && p.pointer("/tool_input/command").and_then(Value::as_str).is_some_and(crate::prs::runs_pr_create)
        && let Some(url) = p.get("tool_response").and_then(crate::prs::created_pr)
    {
        let (hub, session_id) = (hub.clone(), session_id.clone());
        tokio::spawn(async move { crate::prs::open_confirmed(&hub, &session_id, vec![url]).await });
    }
    if event == "Stop"
        && let Some(r) = hand_over_at_turn_end(&hub, &session_id)
    {
        return r;
    }
    if event == "Stop" {
        let user = hub.config.name.clone();
        let moved = hub.append_with(|v| v.nodes.get(&session_id).and_then(|n| states::stall_message(v, n, &user, now_millis())).map(Event::MessageQueued));
        if let Err(e) = moved {
            log!("could not act on a stalled node: {e}");
        }
    }
    if event == "PreToolUse"
        && let Some(reason) = forbidden_for_nodes(tool.as_deref(), p.get("tool_input"))
    {
        log!("blocked a node from acting as the user: {reason}");
        return Json(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny",
            "permissionDecisionReason": reason}})).into_response();
    }
    if event == "PreToolUse" && tool.as_deref().is_some_and(|t| t.starts_with("mcp__plugin_alondra")) {
        return Json(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow",
            "permissionDecisionReason": "Alondra tool"}})).into_response();
    }
    let only_parent_may_ask = h.agent_id.is_some() && !matches!(event.as_str(), "PermissionRequest" | "Elicitation");
    if hub.read(|v| v.nodes.get(&session_id).is_some_and(|n| states::watching(v, n, now_millis()))) {
        return Json(json!({})).into_response();
    }
    if !hub.hold_enabled || only_parent_may_ask || !crate::watcher::is_background_session(&crate::watcher::claude_home(), &session_id) {
        return Json(json!({})).into_response();
    }
    match (event.as_str(), tool.as_deref()) {
        ("PreToolUse", Some("AskUserQuestion")) => hold(hub, p, h, QuestionOrigin::AskUserQuestion).await,
        ("PreToolUse", Some("ExitPlanMode")) => hold(hub, p, h, QuestionOrigin::Plan).await,
        ("PermissionRequest", _) => hold(hub, p, h, QuestionOrigin::Permission).await,
        ("Elicitation", _) => hold(hub, p, h, QuestionOrigin::Elicitation).await,
        _ => Json(json!({})).into_response(),
    }
}

/// An interactive session gets what was queued for it when its turn ends: the Stop hook's
/// `additionalContext` keeps the turn going with the messages (code.claude.com/docs/en/hooks,
/// "Stop decision control"). Background jobs get theirs typed in by `wake`.
fn hand_over_at_turn_end(hub: &Hub, session_id: &str) -> Option<Response> {
    let (ids, text) = hub.read(|v| v.turn_end_delivery(session_id, &hub.config.name))?;
    for (id, question) in ids {
        if let Err(e) = hub.append(Event::MessageHanded { id }) {
            log!("could not hand a message to {session_id}: {e}");
            return None;
        }
        if let Some(q) = question {
            let _ = hub.append(Event::QuestionDelivered { id: q });
        }
    }
    log!("message handed to interactive session {session_id} at the end of its turn");
    Some(Json(json!({"hookSpecificOutput": {"hookEventName": "Stop", "additionalContext": text}})).into_response())
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
        QuestionOrigin::Elicitation => elicitation_card(p),
        _ => (
            permission_title(h.tool_name.as_deref(), &input, h.tool_summary.as_deref()),
            permission_detail(&input, h.tool_summary.as_deref()),
            vec![
                QuestionOption { label: answer_option::ALLOW.into(), description: None },
                QuestionOption { label: answer_option::DENY.into(), description: Some("Write the reason".into()) },
            ],
        ),
    };
    Question {
        related: vec![],
        id: view.questions.keys().next_back().copied().unwrap_or(0) + 1,
        session_id: h.session_id.clone(),
        node_name: node.map(|n| n.name.clone().unwrap_or_else(|| n.short_id.clone())),
        title,
        detail,
        options,
        links: vec![],
        attachments: vec![],
        blocking: true,
        origin,
        tool_use_id: h.tool_use_id.clone(),
        to_orchestrator: false,
        node_context: if matches!(origin, QuestionOrigin::Permission | QuestionOrigin::Elicitation) { None } else { node.and_then(|n| n.last_message.clone()) },
        status: QuestionStatus::Open,
        answer: None,
        asked_at: now_millis(),
        action: false,
        steps: vec![],
        mission: None,
    }
}

const PERMISSION_TITLE_CHARS: usize = 100;
const DESTRUCTIVE: &str = "⚠️ destructivo";

fn destructive(command: &str) -> bool {
    !risks(command).is_empty()
}

/// What each risky part of a command does, in plain words, one line per risk, checked segment by
/// segment so `cd x && rm y` is read as two commands.
fn risks(command: &str) -> Vec<&'static str> {
    let mut found = vec![];
    for segment in command.split(|c: char| matches!(c, ';' | '&' | '|' | '(' | ')' | '`' | '\n')) {
        let words: Vec<&str> = segment.split_whitespace().collect();
        let has = |w: &str| words.contains(&w);
        let force = words.iter().any(|w| w.starts_with("--force"));
        let short_flag_with = |f: char| words.iter().any(|w| w.starts_with('-') && !w.starts_with("--") && w.contains(f));
        let risk = if has("push") && (force || short_flag_with('f')) {
            Some("`push --force`: reescribe la rama remota; puede borrar commits que no tienes en local")
        } else if has("worktree") && has("remove") && force {
            Some("`worktree remove --force`: borra el worktree aunque tenga cambios sin commit")
        } else if has("branch") && has("-D") {
            Some("`branch -D`: borra la rama aunque no esté mergeada; sus commits sin push se pierden")
        } else if has("reset") && has("--hard") {
            Some("`reset --hard`: descarta todos los cambios sin commit")
        } else if has("clean") && short_flag_with('f') {
            Some("`clean -f`: borra los ficheros que git no sigue")
        } else if has("rm") && !has("git") {
            Some("`rm`: borra ficheros sin pasar por la papelera")
        } else if force {
            Some("`--force`: se salta las comprobaciones de seguridad del comando")
        } else if has("-D") || has("--hard") {
            Some("`-D` / `--hard`: variante forzada del comando, sin comprobaciones")
        } else {
            None
        };
        if let Some(r) = risk.filter(|r| !found.contains(r)) {
            found.push(r);
        }
    }
    found
}

fn permission_title(tool: Option<&str>, input: &Value, summary: Option<&str>) -> String {
    let tool = tool.unwrap_or("?");
    let said = str_field(input, "description").map(|d| d.trim().to_string()).filter(|d| !d.is_empty()).or_else(|| summary.map(|s| s.split_whitespace().collect::<Vec<_>>().join(" ")));
    let title = match said {
        Some(s) if s.chars().count() > PERMISSION_TITLE_CHARS => format!("{tool}: {}…", s.chars().take(PERMISSION_TITLE_CHARS).collect::<String>().trim_end()),
        Some(s) => format!("{tool}: {s}"),
        None => tool.to_string(),
    };
    match str_field(input, "command") {
        Some(c) if destructive(&c) => format!("{DESTRUCTIVE} · {title}"),
        _ => title,
    }
}

fn permission_detail(input: &Value, summary: Option<&str>) -> Option<String> {
    if let Some(command) = str_field(input, "command") {
        let risks = risks(&command);
        if risks.is_empty() {
            return Some(format!("```\n{command}\n```"));
        }
        let lines = risks.iter().map(|r| format!("- {r}")).collect::<Vec<_>>().join("\n");
        return Some(format!("{lines}\n\n```\n{command}\n```"));
    }
    summary.map(str::to_string).or_else(|| (!input.is_null()).then(|| input.to_string()))
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
    if let Some(text) = held_follow_up(origin, &answer)
        && let Err(e) = hub.append_with(|v| Some(follow_up_message(v, &h.session_id, &text)))
    {
        log!("could not queue the user's words after #{id}: {e}");
    }
    let input = p.get(if origin == QuestionOrigin::Elicitation { "requested_schema" } else { "tool_input" }).cloned().unwrap_or(Value::Null);
    Json(hold_decision(origin, &answer, input, &hub.config.name)).into_response()
}

fn hold_decision(origin: QuestionOrigin, answer: &Answer, input: Value, user: &str) -> Value {
    let text = match answer.option.as_deref() {
        Some(answer_option::MORE_CONTEXT) | None => answer.text.clone(),
        Some(o) => o.to_string(),
    };
    let wants_context = answer.option.as_deref() == Some(answer_option::MORE_CONTEXT);
    let follows = held_follow_up(origin, answer).is_some();
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
        QuestionOrigin::Elicitation => {
            let action = if approves(answer_option::ACCEPT) {
                "accept"
            } else if wants_context {
                "cancel"
            } else {
                "decline"
            };
            let mut out = json!({"hookEventName": "Elicitation", "action": action});
            if action == "accept" && input.is_object() {
                let said = answer.text.trim();
                let said = if said.eq_ignore_ascii_case(answer_option::ACCEPT) { "" } else { said };
                out["content"] = elicitation_content(&input, said);
            }
            json!({"hookSpecificOutput": out})
        }
        QuestionOrigin::Plan if approves(answer_option::APPROVE) => json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": input}}),
        QuestionOrigin::Plan => {
            let reason = match (wants_context, follows) {
                (true, _) => answer.text.clone(),
                (false, true) => format!("{user} requested changes; the message follows"),
                (false, false) => format!("{user} requests changes to the plan"),
            };
            json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}})
        }
        _ if approves(answer_option::ALLOW) => json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest",
            "decision": {"behavior": "allow"}}}),
        _ => {
            let reason = match (wants_context, follows) {
                (true, _) => answer.text.clone(),
                (false, true) => format!("{user} denied it; the message follows"),
                (false, false) => format!("{user} denies it"),
            };
            json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "deny", "message": reason}}})
        }
    }
}

/// What the user wrote when turning down a plan or a permission, which reaches the node as a
/// message of its own (a user turn) rather than inside the hook's reason.
fn held_follow_up(origin: QuestionOrigin, answer: &Answer) -> Option<String> {
    let yes = match origin {
        QuestionOrigin::Plan => answer_option::APPROVE,
        QuestionOrigin::AskUserQuestion | QuestionOrigin::Elicitation => return None,
        _ => answer_option::ALLOW,
    };
    let option = answer.option.as_deref();
    if option == Some(answer_option::MORE_CONTEXT) || option == Some(yes) {
        return None;
    }
    let text = answer.text.trim();
    let is_label = |l: &str| text.eq_ignore_ascii_case(l);
    if text.is_empty() || is_label(yes) || option.is_some_and(is_label) || [answer_option::DENY, answer_option::REQUEST_CHANGES].into_iter().any(is_label) {
        return None;
    }
    Some(text.to_string())
}

fn follow_up_message(view: &View, session_id: &str, text: &str) -> Event {
    Event::MessageQueued(Message {
        id: view.messages.keys().next_back().copied().unwrap_or(0) + 1,
        session_id: session_id.to_string(),
        from: MessageFrom::User,
        text: text.to_string(),
        answers_question: None,
        created_at: now_millis(),
        handed_at: None,
        delivered_at: None,
        about: None,
        attachments: vec![],
    })
}

/// The fields an MCP server's form asks for: (name, title, JSON type).
fn elicitation_fields(schema: &Value) -> Vec<(String, String, String)> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| {
            props
                .iter()
                .map(|(name, f)| (name.clone(), str_field(f, "title").unwrap_or_else(|| name.clone()), str_field(f, "type").unwrap_or_else(|| "string".into())))
                .collect()
        })
        .unwrap_or_default()
}

fn elicitation_card(p: &Value) -> (String, Option<String>, Vec<QuestionOption>) {
    let server = str_field(p, "mcp_server_name").unwrap_or_else(|| "An MCP server".into());
    let message = str_field(p, "message").unwrap_or_default();
    let title = message.lines().map(str::trim).find(|l| !l.is_empty()).map_or_else(|| format!("{server} requests your input"), |l| l.chars().take(120).collect());
    let fields = elicitation_fields(p.get("requested_schema").unwrap_or(&Value::Null));
    let mut detail = vec![format!("MCP server “{server}” requests your input."), message];
    if let Some(url) = str_field(p, "url") {
        detail.push(url);
    }
    let accept_hint = match fields.as_slice() {
        [] => None,
        [(_, title, _)] => Some(format!("Write the {title}")),
        many => Some(format!("Write one per line: {}", many.iter().map(|(name, _, _)| format!("{name}: …")).collect::<Vec<_>>().join(", "))),
    };
    (
        title,
        Some(detail.into_iter().filter(|d| !d.is_empty()).collect::<Vec<_>>().join("\n\n")),
        vec![
            QuestionOption { label: answer_option::ACCEPT.into(), description: accept_hint },
            QuestionOption { label: answer_option::DECLINE.into(), description: None },
        ],
    )
}

/// The form values for an accepted elicitation: a JSON object as written, the whole answer for a
/// one-field form, or `name: value` lines, each typed as the form's schema says.
fn elicitation_content(schema: &Value, said: &str) -> Value {
    if let Ok(Value::Object(o)) = serde_json::from_str::<Value>(said) {
        return Value::Object(o);
    }
    let fields = elicitation_fields(schema);
    let typed = |kind: &str, v: &str| -> Value {
        let v = v.trim();
        match kind {
            "integer" => v.parse::<i64>().map(Value::from).unwrap_or_else(|_| v.into()),
            "number" => v.parse::<f64>().map(Value::from).unwrap_or_else(|_| v.into()),
            "boolean" => Value::Bool(matches!(v.to_lowercase().as_str(), "true" | "yes" | "y" | "sí" | "si" | "1")),
            _ => v.into(),
        }
    };
    let mut content = serde_json::Map::new();
    if said.is_empty() {
        return Value::Object(content);
    }
    if let [(name, _, kind)] = fields.as_slice() {
        content.insert(name.clone(), typed(kind, said));
        return Value::Object(content);
    }
    for line in said.lines() {
        let Some((key, value)) = line.split_once(':').or_else(|| line.split_once('=')) else { continue };
        let key = key.trim();
        if let Some((name, _, kind)) = fields.iter().find(|(name, title, _)| name.eq_ignore_ascii_case(key) || title.eq_ignore_ascii_case(key)) {
            content.insert(name.clone(), typed(kind, value));
        }
    }
    Value::Object(content)
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

/// Every pull request a node mentions and GitHub confirms gets its card, once. Awaited, so a
/// question or report about a new PR is filed inside its card instead of loose in the inbox.
async fn note_prs(hub: &Hub, caller: &Caller, texts: &[String]) {
    if let Some((sid, _)) = hub.read(|v| resolve_caller(v, caller)) {
        crate::prs::open_confirmed(hub, &sid, texts.iter().flat_map(|t| alondra_core::pr::urls_in(t)).collect()).await;
    }
}

#[derive(Deserialize)]
struct MergeBody {
    method: alondra_core::pr::MergeMethod,
}

async fn merge_pr(State(hub): State<AppState>, signed: Option<Extension<crate::auth::Signed>>, Path(id): Path<u64>, Json(b): Json<MergeBody>) -> Response {
    let Some(pr) = hub.read(|v| v.prs.get(&id).cloned()) else {
        return err(StatusCode::NOT_FOUND, format!("PR card #{id} does not exist"));
    };
    match pr.state {
        alondra_core::pr::PrState::Open => {}
        alondra_core::pr::PrState::Merged => return already_merged(id),
        _ => return refused(StatusCode::CONFLICT, "pr_not_open", format!("{} is no longer open", pr.url)),
    }
    if !pr.merge_methods.is_empty() && !pr.merge_methods.contains(&b.method) {
        return err(StatusCode::UNPROCESSABLE_ENTITY, format!("{} does not allow {}", pr.repo, b.method.label()));
    }
    if let Some(reason) = hub.read(|v| merge_blocked(v, id)) {
        return refused(StatusCode::CONFLICT, "stacked", reason);
    }
    if pr.conflicts() {
        return conflicts(&pr);
    }
    let merged = tokio::time::timeout(
        Duration::from_secs(90),
        tokio::process::Command::new("gh").args(["pr", "merge", &pr.url, b.method.flag()]).kill_on_drop(true).output(),
    )
    .await;
    match merged {
        Ok(Ok(out)) if out.status.success() => {}
        Ok(Ok(out)) => {
            let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if gh_says_already_merged(&said) {
                if hub.read(|v| v.prs.get(&id).is_some_and(|p| p.state == alondra_core::pr::PrState::Open))
                    && let Err(e) = hub.append(Event::PrMerged { id, method: b.method })
                {
                    return internal(e);
                }
                return already_merged(id);
            }
            let mut card = crate::prs::refresh(&hub, &pr).await;
            if said.contains(MERGE_COMMIT_FAILS) && !card.conflicts() {
                card.mergeable = Some("CONFLICTING".into());
                let _ = hub.append(Event::PrUpdated(card.clone()));
            }
            return if card.conflicts() { conflicts(&card) } else { refused(StatusCode::UNPROCESSABLE_ENTITY, "merge_refused", said) };
        }
        Ok(Err(e)) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Err(_) => return err(StatusCode::GATEWAY_TIMEOUT, "gh pr merge did not finish within 90 s"),
    }
    let who = signed.map_or_else(|| "the Mac".to_string(), |Extension(s)| s.name);
    log!("{} merged by {} from {who}", pr.url, hub.config.name);
    let plan = hub.read(|v| v.retarget_plan(id));
    let text = format!("[Alondra] {} merged {} ({}).", hub.config.name, pr.url, b.method.label());
    let _ = tell_about_pr(&hub, &pr, text, MessageFrom::Alondra, Some(MessageAbout::PrMerged { pr_id: id }), vec![]);
    if let Err(e) = hub.append(Event::PrMerged { id, method: b.method }) {
        return internal(e);
    }
    let moved = crate::prs::retarget(&hub, &plan, &crate::prs::GhCli).await;
    if !moved.is_empty() {
        let hub = hub.clone();
        tokio::spawn(async move { crate::prs::settle(&hub, &moved, &crate::prs::GhCli, Duration::from_secs(5), 12).await });
    }
    Json(json!({"id": id, "merged": true})).into_response()
}

/// Why a PR card cannot be merged yet: the PR under it in its train is still open on GitHub.
fn merge_blocked(v: &View, id: u64) -> Option<String> {
    v.merge_blocked_by(id).map(|below| match below.state {
        alondra_core::pr::PrState::Dismissed => format!("PR #{} is still open on GitHub: merge or close it first", below.number),
        _ => format!("merge PR #{} first", below.number),
    })
}

/// What `gh pr merge` says when the branch conflicts with its base.
const MERGE_COMMIT_FAILS: &str = "merge commit cannot be cleanly created";

/// The refusal a client shows on a PR in conflict, in place of GitHub's error.
fn conflicts(pr: &alondra_core::pr::PullRequest) -> Response {
    let base = pr.base.as_deref().unwrap_or("its base");
    let msg = format!("PR #{} conflicts with {base}: resolve the conflicts first.", pr.number);
    (StatusCode::CONFLICT, Json(json!({"error": msg, "conflicts": true}))).into_response()
}

fn resolve_conflicts_text(user: &str, pr: &alondra_core::pr::PullRequest) -> String {
    let base = pr.base.as_deref().unwrap_or("its base branch");
    format!(
        "{user} asks you to resolve the conflicts of {} with {base}: bring {base} into the branch, resolve them, \
         run the tests and push. The PR card goes back to {user} once GitHub can merge it.",
        pr.url
    )
}

async fn resolve_pr_conflicts(State(hub): State<AppState>, Path(id): Path<u64>) -> Response {
    let Some(pr) = hub.read(|v| v.prs.get(&id).cloned()) else {
        return err(StatusCode::NOT_FOUND, format!("PR card #{id} does not exist"));
    };
    if pr.state != alondra_core::pr::PrState::Open {
        return err(StatusCode::CONFLICT, format!("{} is no longer open", pr.url));
    }
    match tell_about_pr(&hub, &pr, resolve_conflicts_text(&hub.config.name, &pr), MessageFrom::User, None, vec![]) {
        Ok(to) => {
            if let Err(e) = hub.append(Event::PrChangesRequested { id }) {
                log!("conflicts sent on PR card #{id}, card not set aside: {e}");
            }
            Json(json!({"id": id, "sent_to": to})).into_response()
        }
        Err(e) => internal(e),
    }
}

/// A merge sent twice (a phone retrying after a timeout) finds the PR merged: that is the
/// outcome it asked for, not an error.
fn already_merged(id: u64) -> Response {
    Json(json!({"id": id, "merged": true, "already": true})).into_response()
}

fn gh_says_already_merged(stderr: &str) -> bool {
    stderr.to_lowercase().contains("already merged")
}

#[derive(Deserialize)]
struct ChangesBody {
    text: String,
    #[serde(default)]
    attachments: Vec<String>,
}

async fn request_pr_changes(State(hub): State<AppState>, Path(id): Path<u64>, Json(b): Json<ChangesBody>) -> Response {
    let Some(pr) = hub.read(|v| v.prs.get(&id).cloned()) else {
        return err(StatusCode::NOT_FOUND, format!("PR card #{id} does not exist"));
    };
    if b.text.trim().is_empty() {
        return err(StatusCode::UNPROCESSABLE_ENTITY, "say what to change");
    }
    let attachments = match attached(&hub, &b.attachments) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let text = format!("{} requests changes on {}:\n\n{}", hub.config.name, pr.url, b.text.trim());
    match tell_about_pr(&hub, &pr, text, MessageFrom::User, None, attachments) {
        Ok(to) => {
            if let Err(e) = hub.append(Event::PrChangesRequested { id }) {
                log!("changes sent on PR card #{id}, card not set aside: {e}");
            }
            Json(json!({"id": id, "sent_to": to})).into_response()
        }
        Err(e) => internal(e),
    }
}

/// A message about a PR goes to the node that opened it, or to the orchestrator once that node is
/// gone, or when the message is Alondra's and the node is done.
pub(crate) fn tell_about_pr(hub: &Hub, pr: &alondra_core::pr::PullRequest, text: String, from: MessageFrom, about: Option<MessageAbout>, attachments: Vec<Attachment>) -> anyhow::Result<String> {
    let text = format!("{text}{}", uploads::delivery_note(&hub.config.name, &attachments));
    let res = hub.append_with(|v| {
        let node = v.nodes.get(&pr.session_id).filter(|n| (n.alive || n.in_daemon) && (from != MessageFrom::Alondra || v.may_alondra_write(n)));
        let to = node.or_else(|| v.nodes.values().find(|n| n.is_orchestrator && n.alive))?;
        Some(Event::MessageQueued(Message {
            id: v.messages.keys().next_back().copied().unwrap_or(0) + 1,
            session_id: to.session_id.clone(),
            from,
            text: text.clone(),
            answers_question: None,
            created_at: now_millis(),
            handed_at: None,
            delivered_at: None,
            about,
            attachments: attachments.clone(),
        }))
    })?;
    match res {
        Some((_, Event::MessageQueued(m))) => Ok(m.session_id),
        _ => anyhow::bail!("neither the node nor an orchestrator is there to receive it"),
    }
}

#[derive(Deserialize)]
struct UploadQuery {
    name: String,
}

/// The file itself is the body; `name` is its original file name. The copy is Alondra's from
/// here on: the original can be deleted.
async fn upload(State(hub): State<AppState>, Query(q): Query<UploadQuery>, body: axum::body::Bytes) -> Response {
    let home = hub.home.clone();
    match tokio::task::spawn_blocking(move || uploads::store(&home, &q.name, &body)).await {
        Ok(Ok((id, a))) => {
            let kind = if uploads::is_image(&a.name) { "image" } else { "file" };
            Json(json!({"id": id, "name": a.name, "bytes": a.bytes, "kind": kind, "path": a.stored_path})).into_response()
        }
        Ok(Err(e)) => err(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()),
        Err(e) => internal(e.into()),
    }
}

/// The uploads a send refers to; a missing one refuses the send.
fn attached(hub: &Hub, ids: &[String]) -> Result<Vec<Attachment>, Response> {
    uploads::resolve(&hub.home, ids).map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, e))
}

/// Large enough for any PR a person reviews on a phone; beyond it the diff is cut, and says so.
const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;

async fn pr_diff(State(hub): State<AppState>, Path(id): Path<u64>) -> Response {
    let Some(url) = hub.read(|v| v.prs.get(&id).map(|p| p.url.clone())) else {
        return err(StatusCode::NOT_FOUND, format!("PR card #{id} does not exist"));
    };
    let out = tokio::time::timeout(Duration::from_secs(60), tokio::process::Command::new("gh").args(["pr", "diff", &url]).kill_on_drop(true).output()).await;
    match out {
        Ok(Ok(o)) if o.status.success() => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            if text.len() > MAX_DIFF_BYTES {
                let cut = (0..=MAX_DIFF_BYTES).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
                text.truncate(cut);
                text.push_str("\n\\ Alondra: the diff was cut here; open the PR on GitHub for the rest\n");
            }
            ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response()
        }
        Ok(Ok(o)) => err(StatusCode::BAD_GATEWAY, String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Ok(Err(e)) => err(StatusCode::BAD_GATEWAY, e.to_string()),
        Err(_) => err(StatusCode::GATEWAY_TIMEOUT, "gh pr diff did not finish within 60 s"),
    }
}

async fn dismiss_pr(State(hub): State<AppState>, Path(id): Path<u64>) -> Response {
    match hub.append(Event::PrDismissed { id }) {
        Ok(_) => Json(json!({"id": id})).into_response(),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct ActivityBody {
    client: String,
}

async fn activity(State(hub): State<AppState>, Json(b): Json<ActivityBody>) -> Json<Value> {
    hub.touch(&b.client);
    Json(json!({"ok": true}))
}

async fn health(State(hub): State<AppState>) -> Json<Value> {
    let c = &hub.config;
    Json(json!({"ok": true, "tick": hub.tick(), "holds": hub.holds_count(), "version": env!("CARGO_PKG_VERSION"),
                "config": {"name": c.name, "port": c.port, "listen": c.listen.to_string(), "public_url": c.public_url,
                           "legacy_bridge": c.legacy_bridge, "wake": c.wake, "retention_hours": c.retention_hours,
                           "home": c.home.display().to_string()}}))
}

/// Oldest node first, state does not drive the order: a state change must not move a node's
/// place in the list. The session id breaks ties deterministically. The orchestrator always
/// sorts first (clients keep it out of the stable-ordered list entirely).
fn fleet_order_key(n: &alondra_core::Node) -> (bool, alondra_core::Millis, String) {
    (!n.is_orchestrator, n.created_at, n.session_id.clone())
}

async fn overview(State(hub): State<AppState>) -> Json<Value> {
    let now = now_millis();
    let machine = hub.machine();
    let usage = crate::usage::current(&crate::watcher::claude_home());
    let body = hub.read(|v| {
        let mut nodes: Vec<&alondra_core::Node> = v.nodes.values().collect();
        nodes.sort_by_key(|n| fleet_order_key(n));
        let open: Vec<Value> = v
            .questions
            .values()
            .filter(|q| q.status.is_open())
            .map(|q| {
                let mut value = serde_json::to_value(q).unwrap_or(Value::Null);
                if let Some(pr) = v.question_pr(q) {
                    value["pr_id"] = json!(pr);
                }
                let reports: Vec<&Report> = v.question_reports(q).iter().filter_map(|id| v.reports.get(id)).collect();
                if let Some(newest) = reports.last() {
                    value["report"] = json!(newest);
                    value["reports"] = json!(reports);
                }
                value
            })
            .collect();
        let recent_answered: Vec<&Question> = v.questions.values().rev().filter(|q| !q.status.is_open()).take(20).collect();
        let reports: Vec<&Report> = v.reports.values().rev().take(50).collect();
        let news: Vec<Value> = v
            .inbox_reports()
            .map(|r| {
                let mut value = serde_json::to_value(r).unwrap_or(Value::Null);
                if let Some(pr) = v.report_pr(r) {
                    value["pr_id"] = json!(pr);
                }
                let earlier: Vec<&Report> = v.report_stack(r).iter().filter_map(|id| v.reports.get(id)).collect();
                if !earlier.is_empty() {
                    value["earlier"] = json!(earlier);
                }
                value
            })
            .collect();
        let orchestrator_messages: Vec<&Message> = v
            .nodes
            .values()
            .find(|n| n.is_orchestrator)
            .map(|o| v.messages.values().rev().filter(|m| m.session_id == o.session_id).take(20).collect())
            .unwrap_or_default();
        let nodes: Vec<Value> = nodes
            .into_iter()
            .map(|n| {
                let mut value = client_node(n);
                value["project"] = Value::String(n.project());
                value["messages_held_for_turn_end"] = json!(v.held_for_turn_end(&n.session_id));
                value["background"] = json!(states::background(n, now));
                value
            })
            .collect();
        json!({
            "tick": hub.tick(), "seq": v.seq, "nodes": nodes, "open_questions": open, "news": news,
            "orchestrator_messages": orchestrator_messages,
            "recent_questions": recent_answered, "reports": reports, "machine": machine, "usage": usage,
            "prs": v.in_train_order(crate::prs::in_inbox(v)).into_iter().map(|p| pr_card(v, p)).collect::<Vec<Value>>(),
            "user": hub.config.name,
        })
    });
    Json(body)
}

/// A PR card as clients get it: the PR, the method last used in its repo and where it sits in
/// its train, so every client lists trains alike.
fn pr_card(v: &View, p: &alondra_core::pr::PullRequest) -> Value {
    let place = v.stack_place(p);
    let mut value = serde_json::to_value(p).unwrap_or(Value::Null);
    value["preferred_method"] = serde_json::to_value(v.merge_methods.get(&p.repo)).unwrap_or(Value::Null);
    value["below"] = json!(place.below.map(|b| b.id));
    value["below_number"] = json!(place.below.map(|b| b.number));
    value["below_set_aside"] = json!(place.below.is_some_and(|b| b.state == alondra_core::pr::PrState::Dismissed));
    value["stack_bottom"] = json!(place.bottom.id);
    value["stack_opened_at"] = json!(place.bottom.opened_at);
    value["stack_position"] = json!(place.position);
    value
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

#[derive(Deserialize)]
struct TimelineQuery {
    limit: Option<usize>,
    before: Option<u64>,
}

const TIMELINE_DEFAULT: usize = 40;
const TIMELINE_MAX: usize = 2000;

async fn node_detail(State(hub): State<AppState>, Path(key): Path<String>, Query(q): Query<TimelineQuery>) -> Response {
    let found = hub.read(|v| {
        let n = v.resolve_node(&key)?.clone();
        let sid = n.session_id.clone();
        let questions: Vec<Question> = v.questions.values().filter(|q| q.session_id == sid).cloned().collect();
        let reports: Vec<Report> = v.reports.values().filter(|r| r.session_id == sid).cloned().collect();
        let messages: Vec<Message> = v.messages.values().filter(|m| m.session_id == sid).cloned().collect();
        let mut node = client_node(&n);
        node["background"] = json!(states::background(&n, now_millis()));
        Some((node, n.session_id, questions, reports, messages, v.held_for_turn_end(&sid)))
    });
    let Some((node, session_id, questions, reports, messages, held)) = found else {
        return err(StatusCode::NOT_FOUND, format!("unknown node {key}"));
    };
    let timeline: Vec<Value> = hub
        .timeline(&session_id, q.limit.unwrap_or(TIMELINE_DEFAULT).min(TIMELINE_MAX), q.before)
        .unwrap_or_default()
        .into_iter()
        .map(|(seq, at, body)| json!({"seq": seq, "at": at, "event": serde_json::from_str::<Value>(&body).unwrap_or(Value::Null)}))
        .collect();
    Json(json!({"node": node, "questions": questions, "reports": reports, "messages": messages, "messages_held_for_turn_end": held, "timeline": timeline})).into_response()
}

/// A node as clients see it: whether its session is attached is the daemon's business.
fn client_node(n: &alondra_core::Node) -> Value {
    let mut value = serde_json::to_value(n).unwrap_or(Value::Null);
    if let Some(o) = value.as_object_mut() {
        o.remove("alive");
    }
    value
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
    #[serde(default)]
    steps: Vec<String>,
    /// The node's other open cards this one belongs with; required when it has any.
    #[serde(default, deserialize_with = "card_ids")]
    related: Option<Vec<u64>>,
    /// A question of the same node this one replaces: it is withdrawn once this one is recorded.
    #[serde(default, deserialize_with = "card_id")]
    replaces: Option<u64>,
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

const PROBE_PREFIX: &str = "test/";

fn probe_refused(name: Option<&str>, sandbox: bool) -> Option<Response> {
    let name = name.filter(|n| n.starts_with(PROBE_PREFIX) && !sandbox)?;
    Some(err(
        StatusCode::UNPROCESSABLE_ENTITY,
        format!(
            "{name} is a test node and this is the user's real Alondra, so its reports and questions would land in their inbox. \
             Run probes against a sandbox instead: `make e2e-sandbox` in the Alondra repo starts one and prints how to point a node at it."
        ),
    ))
}

fn caller_probe_refused(hub: &Hub, caller: &Caller) -> Option<Response> {
    let name = hub.read(|v| resolve_caller(v, caller).and_then(|(_, name)| name));
    probe_refused(name.as_deref(), hub.config.sandbox)
}

async fn ask(State(hub): State<AppState>, Json(b): Json<AskBody>) -> Response {
    if let Some(r) = caller_probe_refused(&hub, &b.caller) {
        return r;
    }
    if b.title.trim().is_empty() || b.detail.as_deref().is_none_or(|d| d.trim().is_empty()) {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("A question needs a one-line title and a detail with all the context {} needs to answer without asking you anything.", hub.config.name),
        );
    }
    if let Some(old) = b.replaces {
        let (question, asker) = hub.read(|v| (v.questions.get(&old).cloned(), resolve_caller(v, &b.caller).map(|(sid, _)| sid)));
        if let Some((status, why)) = withdraw_refusal(old, question.as_ref(), Some(asker.as_deref())) {
            return err(status, format!("replaces: {why}. Nothing was recorded."));
        }
    }
    let related = match hub.read(|v| {
        let (sid, _) = resolve_caller(v, &b.caller)?;
        let urls: Vec<String> = b.links.iter().map(|l| l.url.clone()).collect();
        Some(related_cards(v, &sid, b.related.as_deref(), b.replaces, b.to_orchestrator || lands_in_a_pr_card(v, &urls), &hub.config.name))
    }) {
        Some(Err((status, why))) => return err(status, why),
        Some(Ok(cards)) => cards,
        None => vec![],
    };
    let replaces = b.replaces;
    note_prs(&hub, &b.caller, &question_texts(&b)).await;
    let recorded = record_question(hub.clone(), b, false, related);
    if let Some(old) = replaces
        && recorded.status().is_success()
        && let Err(e) = hub.append(Event::QuestionClosed { id: old, status: QuestionStatus::Withdrawn, note: None })
    {
        log!("could not withdraw question #{old} replaced by a new one: {e}");
    }
    recorded
}

/// A new card that links a PR with a card (or one about to get one) lives inside that card, so
/// it adds nothing loose to the inbox.
fn lands_in_a_pr_card(v: &View, texts: &[String]) -> bool {
    texts.iter().flat_map(|t| alondra_core::pr::urls_in(t)).filter_map(|u| alondra_core::pr::parse_url(&u)).any(|(repo, number)| {
        v.prs.values().find(|p| p.repo == repo && p.number == number).is_none_or(|p| p.state == alondra_core::pr::PrState::Open)
    })
}

/// What a new `ask` or `report` of the node declares it belongs with. When the node already has
/// open cards, it must say which of them this one goes with (`related`, possibly empty): Alondra
/// never guesses it. Every id must be one of the node's own open cards. A question that replaces
/// another is related to it: the one it replaces needs no declaration, and what that one carried
/// passes to the new one.
fn related_cards(
    v: &View,
    sid: &str,
    related: Option<&[u64]>,
    replaces: Option<u64>,
    stays_out_of_the_inbox: bool,
    user: &str,
) -> Result<Vec<CardRef>, (StatusCode, String)> {
    let replaced = replaces.map(CardRef::Question);
    let own: Vec<CardRef> = v.own_open_cards(sid).into_iter().filter(|c| Some(*c) != replaced).collect();
    let inherited: Vec<CardRef> = replaces.and_then(|id| v.questions.get(&id)).map(|q| q.related.clone()).unwrap_or_default();
    let number = |c: &CardRef| match c {
        CardRef::Question(id) | CardRef::Report(id) => *id,
    };
    let Some(ids) = related else {
        let undeclared: Vec<&CardRef> = own.iter().filter(|c| !inherited.contains(c)).collect();
        if undeclared.is_empty() || stays_out_of_the_inbox {
            return Ok(inherited.into_iter().filter(|c| own.contains(c)).collect());
        }
        let listed: Vec<String> = undeclared
            .into_iter()
            .map(|c| match c {
                CardRef::Question(id) => format!("- #{id} question: {}", v.questions[id].title),
                CardRef::Report(id) => format!("- #{id} report ({}): {}", v.reports[id].kind.label(), v.reports[id].title),
            })
            .collect();
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "Nothing was recorded: you already have open cards for {user}:\n{}\nSend it again with `related` set to the ids of the cards this one \
                 belongs with, or `[]` if none; they are shown together as one card.",
                listed.join("\n")
            ),
        ));
    };
    let mut cards: Vec<CardRef> = inherited.into_iter().filter(|c| own.contains(c)).collect();
    for id in ids {
        let matching: Vec<CardRef> = own.iter().filter(|c| number(c) == *id).copied().collect();
        if matching.is_empty() {
            let q = v.questions.get(id).map(|q| q.session_id.as_str());
            let r = v.reports.get(id).map(|r| r.session_id.as_str());
            let why = if q == Some(sid) || r == Some(sid) {
                (StatusCode::CONFLICT, format!("related: #{id} is no longer open. Nothing was recorded."))
            } else if q.is_some() || r.is_some() {
                (StatusCode::FORBIDDEN, format!("related: #{id} is not yours. Nothing was recorded."))
            } else {
                (StatusCode::NOT_FOUND, format!("related: #{id} does not exist. Nothing was recorded."))
            };
            return Err(why);
        }
        cards.extend(matching);
    }
    cards.sort_unstable();
    cards.dedup();
    Ok(cards)
}

async fn request_action(State(hub): State<AppState>, Json(mut b): Json<AskBody>) -> Response {
    if let Some(r) = caller_probe_refused(&hub, &b.caller) {
        return r;
    }
    b.steps.retain(|s| !s.trim().is_empty());
    if b.title.trim().is_empty() || b.steps.is_empty() {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("An action needs a one-line title saying what {} has to do and the steps to do it.", hub.config.name),
        );
    }
    b.options = vec![];
    b.blocking = true;
    b.to_orchestrator = false;
    note_prs(&hub, &b.caller, &question_texts(&b)).await;
    record_question(hub, b, true, vec![])
}

fn question_texts(b: &AskBody) -> Vec<String> {
    b.links.iter().map(|l| l.url.clone()).chain(b.detail.clone()).collect()
}

fn record_question(hub: AppState, mut b: AskBody, action: bool, related: Vec<CardRef>) -> Response {
    let stamp = now_millis();
    let options: Vec<QuestionOption> = if action { answer_option::for_action() } else { std::mem::take(&mut b.options).into_iter().map(Into::into).collect() };
    let attachments = store_attachments(&hub.home, &format!("q-{stamp}"), &b.attachments);
    let res = hub.append_with(|v| {
        let (sid, name) = resolve_caller(v, &b.caller)?;
        let ctx = v.nodes.get(&sid).and_then(|n| n.tool.as_ref().map(|t| format!("{} {}", t.name, t.summary.clone().unwrap_or_default())).or(n.last_message.clone()));
        Some(Event::QuestionAsked(Question {
            related: related.clone(),
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
            action,
            steps: b.steps.clone(),
            mission: None,
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
        Some((q, node)) => Json(json!({"question": q, "node": node.as_ref().map(client_node)})).into_response(),
        None => err(StatusCode::NOT_FOUND, format!("question #{id} does not exist")),
    }
}

#[derive(Deserialize)]
struct AnswerBody {
    text: String,
    option: Option<String>,
    by: Option<String>,
    #[serde(default)]
    attachments: Vec<String>,
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
        if q.answer.as_ref().is_some_and(|a| a.text == b.text && a.option == b.option) {
            return Json(json!({"id": id, "already": true})).into_response();
        }
        return refused(StatusCode::CONFLICT, "question_closed", format!("question #{id} is no longer open"));
    }
    let attachments = match attached(&hub, &b.attachments) {
        Ok(a) => a,
        Err(r) => return r,
    };
    if crate::menus::is_menu_card(&q) {
        if !attachments.is_empty() {
            return err(StatusCode::UNPROCESSABLE_ENTITY, "This card is a menu in the node's terminal: it takes an option, not attachments. Send the files in a message to the node.");
        }
        return answer_menu(hub, q, b.option.as_deref(), signed.map(|s| s.name)).await;
    }
    let device = signed.as_ref().map_or_else(|| "local".to_string(), |s| s.name.clone());
    let provenance = match &signed {
        Some(s) => format!("via {} (signature verified)", s.name),
        None => "via the Mac (local, unsigned)".to_string(),
    };
    let user = hub.config.name.clone();
    let by = b.by.clone().or_else(|| Some(user.clone()));
    let answer = Answer { text: b.text.clone(), option: b.option.clone(), by, device: Some(device), at: now_millis() };
    let absorbed = hub.read(|v| v.question_reports(&q));
    if let Err(e) = hub.append(Event::QuestionAnswered { id, answer: answer.clone() }) {
        return internal(e);
    }
    for report in absorbed {
        if let Err(e) = hub.append(Event::ReportRead { id: report }) {
            log!("could not mark report #{report} read with question #{id}: {e}");
        }
    }
    let note = uploads::delivery_note(&user, &attachments);
    let held = Answer { text: format!("{}{note}", answer.text), ..answer.clone() };
    if hub.release_hold(id, held) {
        return Json(json!({"id": id, "delivered": "hook"})).into_response();
    }
    let reply = if q.action {
        action_reply(&answer, &user)
    } else {
        let chosen = b.option.clone().filter(|o| o.trim() != b.text.trim()).map(|o| format!("{o}. ")).unwrap_or_default();
        format!("{chosen}{}", b.text)
    };
    let other_open = hub.read(|v| v.questions.values().any(|o| o.id != id && o.session_id == q.session_id && o.status.is_open()));
    let text = if other_open { format!("About \"{}\": {reply}{note}", q.title) } else { format!("{reply}{note}") };
    log!("answer to #{id} from {user} {provenance}");
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
            about: None,
            attachments: attachments.clone(),
        }))
    });
    match res {
        Ok(_) => Json(json!({"id": id, "delivered": "inbox"})).into_response(),
        Err(e) => internal(e),
    }
}

/// What the node reads when the user resolves a "your turn" card.
fn action_reply(answer: &Answer, user: &str) -> String {
    let said = answer.text.trim();
    let extra = |label: &str| (!said.is_empty() && !said.eq_ignore_ascii_case(label)).then(|| format!(" {user} adds: {said}")).unwrap_or_default();
    match answer.option.as_deref() {
        Some(answer_option::DONE) => format!("{user} has done it.{}", extra(answer_option::DONE)),
        Some(answer_option::CANNOT) if said.is_empty() || said.eq_ignore_ascii_case(answer_option::CANNOT) => {
            format!("{user} cannot do it as asked. Find out what is in the way and explain it again with simpler steps.")
        }
        Some(answer_option::CANNOT) => format!("{user} cannot do it as asked: {said}"),
        _ if said.eq_ignore_ascii_case(answer_option::DONE) => format!("{user} has done it."),
        _ => said.to_string(),
    }
}

/// Over today's slice of the week, new nodes wait unless the user said to launch anyway.
fn launch_blocked(usage: Option<&alondra_core::usage::Usage>, over_pace_ok: bool) -> Option<String> {
    let u = usage.filter(|u| u.over_pace && !over_pace_ok)?;
    Some(format!(
        "Today has already used {:.0}% of the week against a {:.0}% slice ({:.0}% of the week used, ~{:.0}% per day left), so the launch was refused. over_pace_ok launches anyway once the user says so.",
        u.today_spent.unwrap_or(0.0),
        u.today_share.unwrap_or(0.0),
        u.week_percent,
        u.per_day_left
    ))
}

/// A menu card is answered by picking the option in the node's terminal.
async fn answer_menu(hub: AppState, q: Question, option: Option<&str>, device: Option<String>) -> Response {
    let Some(label) = option.filter(|o| q.options.iter().any(|x| x.label == *o)).map(str::to_string) else {
        return err(StatusCode::UNPROCESSABLE_ENTITY, "This card is a menu in the node's terminal: pick one of its options, or open the session to answer it there.");
    };
    let Some(short) = hub.read(|v| v.nodes.get(&q.session_id).map(|n| n.short_id.clone())) else {
        return err(StatusCode::NOT_FOUND, "the node is gone");
    };
    let own = hub.own_attach(&short);
    let pressed = {
        let short = short.clone();
        tokio::task::spawn_blocking(move || crate::wake::press(&short, &label)).await
    };
    drop(own);
    match pressed {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return err(StatusCode::BAD_GATEWAY, format!("could not answer {short}'s menu: {e}")),
        Err(e) => return internal(e.into()),
    }
    let label = option.unwrap_or_default().to_string();
    let answer = Answer { text: label.clone(), option: Some(label), by: Some(hub.config.name.clone()), device: Some(device.unwrap_or_else(|| "local".into())), at: now_millis() };
    for event in [
        Event::QuestionAnswered { id: q.id, answer },
        Event::QuestionDelivered { id: q.id },
        Event::MenuSeen { session_id: q.session_id.clone(), waiting_for: None },
    ] {
        if let Err(e) = hub.append(event) {
            return internal(e);
        }
    }
    Json(json!({"id": q.id, "delivered": "terminal"})).into_response()
}

#[derive(Deserialize)]
struct MoreContextBody {
    note: Option<String>,
    #[serde(default)]
    attachments: Vec<String>,
}

async fn more_context(State(hub): State<AppState>, signed: Option<Extension<crate::auth::Signed>>, Path(id): Path<u64>, Json(b): Json<MoreContextBody>) -> Response {
    let note = b.note.filter(|n| !n.trim().is_empty()).map(|n| format!(" What is missing: {n}.")).unwrap_or_default();
    let user = hub.config.name.clone();
    let text = format!(
        "{user} needs more context before deciding.{note} Explain what is at stake, the options with their consequences and your recommendation, and ask again."
    );
    answer(State(hub), signed, Path(id), Json(AnswerBody { text, option: Some(answer_option::MORE_CONTEXT.into()), by: None, attachments: b.attachments })).await
}

/// With a caller (a node's `withdraw` tool), only that node's own open questions can be withdrawn.
async fn withdraw(State(hub): State<AppState>, Path(id): Path<u64>, body: Option<Json<Caller>>) -> Response {
    let (question, caller) = hub.read(|v| (v.questions.get(&id).cloned(), body.as_ref().and_then(|Json(c)| resolve_caller(v, c))));
    let asker = body.is_some().then(|| caller.map(|(sid, _)| sid));
    if let Some((status, why)) = withdraw_refusal(id, question.as_ref(), asker.as_ref().map(Option::as_deref)) {
        return err(status, why);
    }
    match hub.append(Event::QuestionClosed { id, status: QuestionStatus::Withdrawn, note: None }) {
        Ok(_) => Json(json!({"id": id})).into_response(),
        Err(e) => internal(e),
    }
}

/// `asker` is `None` for a client acting as the user, `Some(None)` for a node Alondra cannot place.
fn withdraw_refusal(id: u64, question: Option<&Question>, asker: Option<Option<&str>>) -> Option<(StatusCode, String)> {
    let Some(q) = question else { return Some((StatusCode::NOT_FOUND, format!("question #{id} does not exist"))) };
    if asker.is_some_and(|sid| sid != Some(q.session_id.as_str())) {
        return Some((StatusCode::FORBIDDEN, format!("question #{id} is not yours")));
    }
    (q.status != QuestionStatus::Open).then(|| (StatusCode::CONFLICT, format!("question #{id} is no longer open")))
}

/// Card ids as a node's tools send them. Nodes launched before `related` existed have no such
/// field in their tool schema, so it arrives as text: "[]", "[3, 4]", "3,4" or "3".
fn card_ids<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Vec<u64>>, D::Error> {
    let v = Option::<Value>::deserialize(d)?;
    let bad = || serde::de::Error::custom("related must be a list of card numbers, e.g. [] or [3, 4]");
    let id = |v: &Value| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().trim_start_matches('#').parse().ok()));
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items.iter().map(|i| id(i).ok_or_else(bad)).collect::<Result<_, _>>().map(Some),
        Some(n @ Value::Number(_)) => id(&n).map(|i| Some(vec![i])).ok_or_else(bad),
        Some(Value::String(text)) => {
            let inner = text.trim().trim_start_matches('[').trim_end_matches(']');
            inner.split(',').map(str::trim).filter(|t| !t.is_empty()).map(|t| t.trim_start_matches('#').parse().map_err(|_| bad())).collect::<Result<_, _>>().map(Some)
        }
        Some(_) => Err(bad()),
    }
}

fn card_id<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    match card_ids(d)?.as_deref() {
        None | Some([]) => Ok(None),
        Some([one]) => Ok(Some(*one)),
        Some(_) => Err(serde::de::Error::custom("replaces takes one question number")),
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
    /// The node's other open cards this report belongs with; required when it has any.
    #[serde(default, deserialize_with = "card_ids")]
    related: Option<Vec<u64>>,
}

async fn report(State(hub): State<AppState>, Json(b): Json<ReportBody>) -> Response {
    if let Some(r) = caller_probe_refused(&hub, &b.caller) {
        return r;
    }
    if b.kind != Some(ReportKind::Progress) && (b.title.trim().is_empty() || b.body.as_deref().is_none_or(|t| t.trim().is_empty())) {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("A report needs a title that states the result and a body with what {} should know: what changed, what was measured, what is left.", hub.config.name),
        );
    }
    let related = match hub.read(|v| {
        let (sid, _) = resolve_caller(v, &b.caller)?;
        let mut texts: Vec<String> = b.links.iter().map(|l| l.url.clone()).collect();
        texts.extend([b.title.clone(), b.body.clone().unwrap_or_default()]);
        Some(related_cards(v, &sid, b.related.as_deref(), None, lands_in_a_pr_card(v, &texts), &hub.config.name))
    }) {
        Some(Err((status, why))) => return err(status, why),
        Some(Ok(cards)) => cards,
        None => vec![],
    };
    if b.kind == Some(ReportKind::Final)
        && let Some(session) = hub.read(|v| resolve_caller(v, &b.caller).map(|(s, _)| s))
        && let Some(left) = crate::worktree::leftovers(&session).await
    {
        log!("final report refused, worktree not clean: {left}");
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "You are not done: your worktree has {left}. Commit and push your work (open a PR if that is the \
                 mission) and then report final. If some of it should be thrown away, ask {} with ask before discarding anything.",
                hub.config.name
            ),
        );
    }
    let from_orchestrator = hub.read(|v| resolve_caller(v, &b.caller).and_then(|(s, _)| v.nodes.get(&s)).is_some_and(|n| n.is_orchestrator));
    let looks_like_question = gated(b.kind, from_orchestrator)
        && crate::classify::judge_report(&format!("{}\n\n{}", b.title, b.body.as_deref().unwrap_or("")), &hub.config.name).await == crate::classify::ReportVerdict::Asks;
    if looks_like_question {
        log!("report gate: a report reads like it asks for something, posted flagged: {}", b.title);
        keep_flagged(&hub.home, &b.title, b.body.as_deref());
    }
    let stamp = now_millis();
    let attachments = store_attachments(&hub.home, &format!("r-{stamp}"), &b.attachments);
    let mut texts: Vec<String> = b.links.iter().map(|l| l.url.clone()).collect();
    texts.extend([b.title.clone(), b.body.clone().unwrap_or_default()]);
    note_prs(&hub, &b.caller, &texts).await;
    let res = hub.append_with(|v| {
        let (sid, name) = resolve_caller(v, &b.caller)?;
        Some(Event::ReportPosted(Report {
            related: related.clone(),
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
            mission: None,
            looks_like_question,
        }))
    });
    match res {
        Ok(Some((_, Event::ReportPosted(r)))) => {
            let card = hub.read(|v| v.report_pr(&r).and_then(|id| v.prs.get(&id)).map(|p| (p.id, p.number)));
            let mut body = json!({"id": r.id});
            if looks_like_question {
                body["looks_like_question"] = json!(true);
                body["note"] = json!(flagged_note(&hub.config.name));
            } else if let Some((id, number)) = card {
                body["shown_in_pr"] = json!(id);
                body["note"] = json!(shown_in_pr_note(number, &hub.config.name));
            }
            Json(body).into_response()
        }
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
struct ReviewBody {
    verdict: ReviewVerdict,
}

/// What the node reads when David approves an artifact it sent, or asks it to take in his comments.
/// Comments live on the artifact itself: Alondra cannot read them, the node can.
fn review_text(verdict: ReviewVerdict, artifacts: &[String], user: &str) -> String {
    let urls = artifacts.join(", ");
    match verdict {
        ReviewVerdict::Approved => format!("{user} approved {urls}. Nothing on it waits for review any more."),
        ReviewVerdict::Comments => format!(
            "{user} left comments on {urls}. They are the comment threads on the artifact: ArtifactComments with action read and \
             its url lists them. Threads {user} sent to Claude can be answered and resolved there; the others stay open for {user}. \
             The review goes on once the artifact is republished to the same url and a report with kind document links it again."
        ),
    }
}

/// David's verdict on the artifact a report sent for review. The report leaves the inbox and the
/// verdict is typed into the node as David's own words; an approved artifact asks nothing of him again.
async fn review_artifact(State(hub): State<AppState>, Path(id): Path<u64>, Json(b): Json<ReviewBody>) -> Response {
    let Some(report) = hub.read(|v| v.reports.get(&id).cloned()) else {
        return err(StatusCode::NOT_FOUND, format!("report #{id} does not exist"));
    };
    let reachable = hub.read(|v| v.nodes.get(&report.session_id).is_some_and(|n| n.alive || n.in_daemon));
    if report.read {
        return refused(StatusCode::CONFLICT, "report_closed", format!("report #{id} was already reviewed or read"));
    }
    let artifacts = alondra_core::artifact::in_report(&report);
    if artifacts.is_empty() {
        return err(StatusCode::UNPROCESSABLE_ENTITY, format!("report #{id} links no artifact to review"));
    }
    if !reachable {
        return refused(StatusCode::CONFLICT, "node_gone", format!("the node that sent report #{id} is gone: nobody would read the verdict"));
    }
    let text = review_text(b.verdict, &artifacts, &hub.config.name);
    let queued = hub.append_with(|v| {
        Some(Event::MessageQueued(Message {
            id: v.messages.keys().next_back().copied().unwrap_or(0) + 1,
            session_id: report.session_id.clone(),
            from: MessageFrom::User,
            text: text.clone(),
            answers_question: None,
            created_at: now_millis(),
            handed_at: None,
            delivered_at: None,
            about: Some(MessageAbout::ArtifactReview { report_id: id }),
            attachments: vec![],
        }))
    });
    if let Err(e) = queued {
        return internal(e);
    }
    match hub.append(Event::ArtifactReviewed { report: id, verdict: b.verdict }) {
        Ok(_) => Json(json!({"id": id, "verdict": b.verdict})).into_response(),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct MessageBody {
    text: String,
    from: Option<MessageFrom>,
    #[serde(default)]
    attachments: Vec<String>,
}

async fn interrupt(State(hub): State<AppState>, Path(key): Path<String>) -> Response {
    let Some(short) = hub.read(|v| v.resolve_node(&key).map(|n| n.short_id.clone())) else {
        return err(StatusCode::NOT_FOUND, format!("unknown node {key}"));
    };
    if !hub.config.wake {
        return err(StatusCode::CONFLICT, "this alondrad does not attach to node sessions");
    }
    let own = hub.own_attach(&short);
    let outcome = tokio::task::spawn_blocking(move || crate::wake::interrupt(&short)).await;
    drop(own);
    match outcome {
        Ok(Ok(())) => Json(json!({"interrupted": key})).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_GATEWAY, e),
        Err(e) => internal(e.into()),
    }
}

fn held_for_turn_end_note(name: &str) -> String {
    format!(
        "{name} is an interactive terminal session, not a background job: Alondra cannot type into it, so the message \
         reaches it when its current turn ends. If it is sitting idle at its prompt, it arrives after the next thing \
         typed in its terminal."
    )
}

async fn send_message(State(hub): State<AppState>, Path(key): Path<String>, Json(b): Json<MessageBody>) -> Response {
    if b.from == Some(MessageFrom::Alondra) && hub.read(|v| v.resolve_node(&key).is_some_and(|n| !v.may_alondra_write(n))) {
        return err(StatusCode::CONFLICT, format!("{key} is done: only {} or the orchestrator can reopen it", hub.config.name));
    }
    let attachments = match attached(&hub, &b.attachments) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let text = format!("{}{}", b.text, uploads::delivery_note(&hub.config.name, &attachments));
    let interactive = hub.read(|v| v.resolve_node(&key).filter(|n| !n.in_daemon).map(|n| n.display_name().to_string()));
    let res = hub.append_with(|v| {
        let node = v.resolve_node(&key)?;
        Some(Event::MessageQueued(Message {
            id: v.messages.keys().next_back().copied().unwrap_or(0) + 1,
            session_id: node.session_id.clone(),
            from: b.from.unwrap_or(MessageFrom::User),
            text: text.clone(),
            answers_question: None,
            created_at: now_millis(),
            handed_at: None,
            delivered_at: None,
            about: None,
            attachments: attachments.clone(),
        }))
    });
    match res {
        Ok(Some((_, Event::MessageQueued(m)))) => match interactive {
            Some(name) => Json(json!({"id": m.id, "session_id": m.session_id, "interactive": true, "note": held_for_turn_end_note(&name)})).into_response(),
            None => Json(json!({"id": m.id, "session_id": m.session_id})).into_response(),
        },
        Ok(_) => err(StatusCode::NOT_FOUND, format!("unknown node {key}")),
        Err(e) => internal(e),
    }
}

/// Real risks a close must not paper over: the node still doing something (busy or a live
/// permission hold), or work left uncommitted or unpushed in its worktree. The node's own open
/// questions and actions are not among them: the user's decision to close answers them.
fn close_reasons(busy: bool, waiting_permission: bool, worktree_leftovers: Option<String>) -> Vec<CloseReason> {
    let mut reasons = vec![];
    if busy {
        reasons.push(CloseReason { kind: "busy", detail: None });
        return reasons;
    }
    if waiting_permission {
        reasons.push(CloseReason { kind: "permission", detail: None });
    }
    if let Some(left) = worktree_leftovers {
        reasons.push(CloseReason { kind: "worktree", detail: Some(left) });
    }
    reasons
}

/// The orchestrator is kept alive on purpose, so only the fleet's explicit close, which sends
/// `{"force": true}`, may close it; a card's read-and-close never does.
async fn close_node(State(hub): State<AppState>, Path(key): Path<String>, body: axum::body::Bytes) -> Response {
    let force = serde_json::from_slice::<Value>(&body).ok().and_then(|v| v.get("force").and_then(Value::as_bool)).unwrap_or(false);
    let Some((short, session, busy, waiting_permission, in_daemon, name, orchestrator)) = hub.read(|v| {
        v.resolve_node(&key).map(|n| (n.short_id.clone(), n.session_id.clone(), n.busy, n.waiting_permission, n.in_daemon, n.display_name().to_string(), n.is_orchestrator))
    }) else {
        return err(StatusCode::NOT_FOUND, format!("unknown node {key}"));
    };
    if orchestrator && !force {
        return err_reasons(
            format!("{name} is the orchestrator, which Alondra keeps alive; close it from the fleet if you really mean to"),
            vec![CloseReason { kind: "orchestrator", detail: None }],
        );
    }
    if !in_daemon {
        return forget_interactive(&hub, &session, &name);
    }
    let leftovers = crate::worktree::leftovers(&session).await;
    let reasons = close_reasons(busy, waiting_permission, leftovers);
    if !reasons.is_empty() {
        let msg = reasons.iter().map(|r| r.detail.clone().unwrap_or_else(|| r.kind.to_string())).collect::<Vec<_>>().join("; ");
        return err_reasons(format!("{short} still has something pending, so it was not closed: {msg}"), reasons);
    }
    // The node's own open questions and actions are moot once the user has decided to close it.
    let own_open: Vec<u64> = hub.read(|v| v.questions.values().filter(|q| q.session_id == session && q.status.is_open()).map(|q| q.id).collect());
    for id in own_open {
        if let Err(e) = hub.append(Event::QuestionClosed { id, status: QuestionStatus::Withdrawn, note: Some("the user closed the node".into()) }) {
            return internal(e);
        }
    }
    let job = std::fs::read_to_string(crate::watcher::claude_home().join("jobs").join(&short).join("state.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let worktree = job.as_ref().and_then(|v| v.get("worktreePath").and_then(Value::as_str).map(PathBuf::from));
    let branch = job.as_ref().and_then(|v| v.get("worktreeBranch").and_then(Value::as_str).map(str::to_string));
    let mut cleared_broken = false;
    let mut said = String::new();
    for _ in 0..3 {
        match run_claude(&["rm", &short]).await {
            Ok(text) if text.contains(&format!("removed {short}")) => said = text,
            Ok(text) if !cleared_broken && text.contains("Git couldn't remove it") => {
                let (Some(dir), Some(branch)) = (worktree.as_ref(), branch.as_deref()) else { return err(StatusCode::CONFLICT, text) };
                if let Err(why) = discard_if_identical(dir, branch).await {
                    return err(StatusCode::CONFLICT, format!("{text}\n\nAlondra did not delete it: {why}"));
                }
                cleared_broken = true;
                continue;
            }
            Ok(text) => return err(StatusCode::CONFLICT, text),
            Err(e) => return err(StatusCode::BAD_GATEWAY, e),
        }
        match run_claude(&["agents", "--json", "--all"]).await {
            Ok(list) if !listed(&list, &short) => {
                if let Some(dir) = worktree.as_ref().filter(|d| d.exists()) {
                    return err(StatusCode::CONFLICT, format!("{short} is gone from claude agents but its worktree is still at {}", dir.display()));
                }
                let _ = hub.append(Event::NodeRemoved { session_id: session });
                return Json(json!({"ok": true, "output": said})).into_response();
            }
            Ok(_) => continue,
            Err(e) => return err(StatusCode::BAD_GATEWAY, e),
        }
    }
    err(StatusCode::CONFLICT, format!("claude rm said '{said}' but {short} is still listed by claude agents"))
}

/// An interactive session is not a job `claude rm` knows, and the user may be typing in it: Alondra
/// only stops following it and leaves the terminal alone.
fn forget_interactive(hub: &Hub, session: &str, name: &str) -> Response {
    let own_open: Vec<u64> = hub.read(|v| v.questions.values().filter(|q| q.session_id == session && q.status.is_open()).map(|q| q.id).collect());
    for id in own_open {
        if let Err(e) = hub.append(Event::QuestionClosed { id, status: QuestionStatus::Withdrawn, note: Some("the user closed the node".into()) }) {
            return internal(e);
        }
    }
    if let Err(e) = hub.append(Event::NodeRemoved { session_id: session.to_string() }) {
        return internal(e);
    }
    Json(json!({"ok": true, "interactive": true, "output": format!(
        "{name} is an interactive session, not a background job: Alondra removed it from the fleet and left the terminal session itself open. Close it there with /exit when you are done."
    )}))
    .into_response()
}

/// A private index per call: two closes in the same millisecond must not share one.
fn scratch_index() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("alondra-index-{}-{}-{n}", std::process::id(), now_millis()))
}

/// A worktree whose git link is broken can only be deleted by hand. It is deleted only when
/// its files are exactly its branch, it has no new files and the branch is fully pushed.
async fn discard_if_identical(dir: &std::path::Path, branch: &str) -> Result<(), String> {
    let link = std::fs::read_to_string(dir.join(".git")).map_err(|e| format!("no .git link: {e}"))?;
    let gitdir = link.trim().strip_prefix("gitdir: ").ok_or("unexpected .git link")?;
    let repo_git = PathBuf::from(gitdir).parent().and_then(|p| p.parent()).map(PathBuf::from).ok_or("unexpected gitdir")?;
    let index = scratch_index();
    let git = |args: Vec<String>| {
        let repo_git = repo_git.clone();
        let index = index.clone();
        async move {
            let out = tokio::process::Command::new("git")
                .env("GIT_INDEX_FILE", &index)
                .arg(format!("--git-dir={}", repo_git.display()))
                .args(&args)
                .output()
                .await
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
            }
            Ok::<String, String>(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
    };
    let tree = format!("--work-tree={}", dir.display());
    git(vec!["read-tree".into(), branch.into()]).await?;
    let differs = git(vec![tree.clone(), "diff".into(), "--stat".into()]).await;
    let _ = std::fs::remove_file(&index);
    if !differs?.is_empty() {
        return Err(format!("its files differ from the branch {branch}"));
    }
    git(vec!["read-tree".into(), branch.into()]).await?;
    let untracked = git(vec![tree, "ls-files".into(), "--others".into(), "--exclude-standard".into()]).await;
    let _ = std::fs::remove_file(&index);
    if !untracked?.is_empty() {
        return Err("it has files that are not in git".into());
    }
    if git(vec!["rev-list".into(), "--count".into(), format!("origin/{branch}..{branch}")]).await? != "0" {
        return Err(format!("the branch {branch} has commits that are not pushed"));
    }
    tokio::fs::remove_dir_all(dir).await.map_err(|e| e.to_string())
}

async fn run_claude(args: &[&str]) -> Result<String, String> {
    let run = tokio::process::Command::new("claude").args(args).kill_on_drop(true).output();
    match tokio::time::timeout(Duration::from_secs(60), run).await {
        Ok(Ok(out)) => Ok(format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)).trim().to_string()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(format!("claude {} did not answer in 60 s", args.join(" "))),
    }
}

fn listed(agents_json: &str, short: &str) -> bool {
    serde_json::from_str::<Value>(agents_json)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .is_some_and(|a| a.iter().any(|x| x.get("id").and_then(Value::as_str) == Some(short)))
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
struct OptionsQuery {
    project: Option<String>,
}

async fn options_catalog(State(hub): State<AppState>, Query(q): Query<OptionsQuery>) -> Json<Value> {
    let loaded = crate::options::load(&hub);
    let project = q.project.filter(|p| !p.trim().is_empty());
    Json(json!({"project": project, "options": loaded.file.catalog(project.as_deref()), "error": loaded.error}))
}

async fn projects(State(hub): State<AppState>) -> Json<Value> {
    let loaded = crate::options::load(&hub);
    let root = loaded.file.projects_root(&crate::options::user_home());
    let list: Vec<Value> = alondra_core::options::projects(&root).into_iter().map(|(name, path)| json!({"name": name, "path": path})).collect();
    Json(json!({"root": root, "projects": list, "error": loaded.error}))
}

#[derive(Deserialize)]
struct RequestBody {
    text: String,
    #[serde(default)]
    attachments: Vec<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    options: std::collections::BTreeMap<String, alondra_core::options::Choice>,
}

fn request_text(user: &str, text: &str, delivery: &str, attached: bool, request_id: &str, options: &alondra_core::options::Resolved) -> String {
    let pass_on = if attached { " Put the attached files' paths in the node's mission so it can open them." } else { "" };
    format!(
        "[Alondra] New request from {user}: {text}{delivery}\n\nOptions chosen by {user} (request {request_id}): {}.\nPass request_id \"{request_id}\" to launch: Alondra writes these options into the node's mission and sets its model and effort, so do not restate them.\n\nPrepare the mission and launch the node with Alondra's launch tool, or use ask if something is missing.{pass_on}",
        options.summary()
    )
}

async fn new_request(State(hub): State<AppState>, Json(b): Json<RequestBody>) -> Response {
    let attachments = match attached(&hub, &b.attachments) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let loaded = crate::options::load(&hub);
    let project = b.project.as_deref().filter(|p| !p.trim().is_empty());
    let resolved = match loaded.file.resolve(project, &b.options) {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    if !hub.read(|v| v.nodes.values().any(|n| n.is_orchestrator)) {
        return err(StatusCode::CONFLICT, "the orchestrator is not connected to Alondra");
    }
    let request_id = format!("r-{}", now_millis());
    if let Err(e) = hub.append(Event::RequestSent { request_id: request_id.clone(), options: resolved.clone() }) {
        return internal(e);
    }
    let text = request_text(&hub.config.name, &b.text, &uploads::delivery_note(&hub.config.name, &attachments), !attachments.is_empty(), &request_id, &resolved);
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
            about: None,
            attachments: attachments.clone(),
        }))
    });
    match res {
        Ok(Some(_)) => Json(json!({"ok": true, "request_id": request_id})).into_response(),
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
    #[serde(default, deserialize_with = "lenient_bool")]
    over_pace_ok: bool,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    effort: Option<String>,
}

fn lenient_bool<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    match Value::deserialize(d)? {
        Value::Bool(b) => Ok(b),
        Value::Null => Ok(false),
        Value::String(s) if s.trim().eq_ignore_ascii_case("true") => Ok(true),
        Value::String(s) if s.trim().eq_ignore_ascii_case("false") => Ok(false),
        other => Err(serde::de::Error::custom(format!("expected true or false, got {other}"))),
    }
}

/// The options a launch carries: its request's, or the defaults when no request stands behind
/// it. Only a request the user sent can let a node merge on its own.
fn launch_options(hub: &Hub, b: &LaunchBody) -> Result<alondra_core::options::Resolved, Response> {
    let request = b.request_id.as_deref().map(str::trim).filter(|r| !r.is_empty());
    match request {
        Some(id) => hub
            .read(|v| v.requests.get(id).cloned())
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("unknown request_id {id}: pass the one in the user's request, or none"))),
        None => {
            let file = crate::options::load(hub).file;
            let project = crate::options::project_of(&file, std::path::Path::new(&b.cwd));
            Ok(file.orchestrator_defaults(project.as_deref(), b.effort.as_deref()))
        }
    }
}

async fn launch(State(hub): State<AppState>, Json(b): Json<LaunchBody>) -> Response {
    if let Some(reason) = launch_blocked(crate::usage::current(&crate::watcher::claude_home()).as_ref(), b.over_pace_ok) {
        return err(StatusCode::CONFLICT, reason);
    }
    if !std::path::Path::new(&b.cwd).is_dir() {
        return err(StatusCode::BAD_REQUEST, format!("directory {} does not exist", b.cwd));
    }
    let resolved = match launch_options(&hub, &b) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let request_id = b.request_id.as_deref().map(str::trim).filter(|r| !r.is_empty());
    let args = crate::options::launch_args(&b.name, b.permission_mode.as_deref(), b.model.as_deref(), &b.prompt, &resolved, request_id);
    let mut cmd = tokio::process::Command::new("claude");
    cmd.current_dir(&b.cwd).args(&args).kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(60), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let launched = Event::Launched { name: b.name.clone(), request_id: request_id.map(str::to_string), auto_merge: resolved.auto_merge };
            if let Err(e) = hub.append(launched) {
                return internal(e);
            }
            Json(json!({"ok": true, "output": stdout, "auto_merge": resolved.auto_merge})).into_response()
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
        let mut b = AnswerBody { text: text.into(), option: option.map(Into::into), by: None, attachments: vec![] };
        b.canonicalize();
        Answer { text: b.text, option: b.option, by: None, device: None, at: 0 }
    }

    fn decision(origin: QuestionOrigin, a: &Answer) -> Value {
        hold_decision(origin, a, json!({"plan": "p"}), "Ana")
    }

    /// The fleet list sorts by creation time, oldest first; a node's state changing (even
    /// several times, in any direction) must never move it.
    #[test]
    fn a_state_change_does_not_move_a_node_in_the_fleet_order() {
        let mut a = alondra_core::Node::new("a", 100);
        let mut b = alondra_core::Node::new("b", 200);
        let mut c = alondra_core::Node::new("c", 300);
        let order_of = |nodes: &[&alondra_core::Node]| {
            let mut v = nodes.to_vec();
            v.sort_by_key(|n| fleet_order_key(n));
            v.iter().map(|n| n.session_id.clone()).collect::<Vec<_>>()
        };
        assert_eq!(order_of(&[&a, &b, &c]), vec!["a", "b", "c"]);
        // Push every state through every other state; the creation-time order must hold.
        for state in [alondra_core::NodeState::Done, alondra_core::NodeState::YourTurn, alondra_core::NodeState::Stalled, alondra_core::NodeState::Working] {
            b.state = state;
            b.state_since = now_millis();
            c.state = state;
            c.state_since = now_millis();
            a.state = state;
            a.state_since = now_millis();
            assert_eq!(order_of(&[&a, &b, &c]), vec!["a", "b", "c"], "state={state:?}");
        }
    }

    #[test]
    fn nodes_created_at_the_same_time_tie_break_on_session_id() {
        let a = alondra_core::Node::new("zzz", 100);
        let b = alondra_core::Node::new("aaa", 100);
        let mut v = [&a, &b];
        v.sort_by_key(|n| fleet_order_key(n));
        assert_eq!(v.map(|n| n.session_id.as_str()), ["aaa", "zzz"]);
    }

    #[test]
    fn a_pr_on_top_of_an_open_pr_waits_for_it_to_be_merged() {
        use alondra_core::pr::{PrState, PullRequest};
        let mut v = View::default();
        for (id, base, head) in [(1, "main", "x"), (2, "x", "y"), (3, "main", "z")] {
            let mut pr = PullRequest::opened(id, &format!("https://github.com/a/b/pull/{}", id + 40), "s", None, 0).unwrap();
            (pr.base, pr.head) = (Some(base.into()), Some(head.into()));
            v.prs.insert(id, pr);
        }
        let cases = [(1, None), (2, Some("merge PR #41 first")), (3, None), (9, None)];
        for (id, refusal) in cases {
            assert_eq!(merge_blocked(&v, id).as_deref(), refusal, "card {id}");
        }
        let card = pr_card(&v, &v.prs[&2]);
        assert_eq!((card["below"].as_u64(), card["below_number"].as_u64(), card["stack_bottom"].as_u64(), card["stack_position"].as_u64()), (Some(1), Some(41), Some(1), Some(1)));
        assert_eq!(v.prs_awaiting_user().count(), 3, "a stacked PR still waits for the user");
        let cases = [
            (PrState::Dismissed, Some("PR #41 is still open on GitHub: merge or close it first"), true),
            (PrState::Merged, None, false),
            (PrState::Closed, None, false),
        ];
        for (lower, refusal, set_aside) in cases {
            v.prs.get_mut(&1).unwrap().state = lower;
            assert_eq!(merge_blocked(&v, 2).as_deref(), refusal, "lower PR {lower:?}");
            assert_eq!(pr_card(&v, &v.prs[&2])["below_set_aside"].as_bool(), Some(set_aside), "lower PR {lower:?}");
        }
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
        assert_eq!(out["hookSpecificOutput"]["decision"]["message"], "Ana denies it");
    }

    /// An MCP elicitation reaches the clients as an Accept / Decline card, and the answer goes back
    /// to Claude Code as the Elicitation hook's `action` and `content`.
    #[test]
    fn an_elicitation_becomes_a_card_and_its_answer_the_hook_response() {
        let context7 = json!({"session_id": "s", "hook_event_name": "Elicitation", "mcp_server_name": "context7", "mode": "form",
            "message": "You're using Context7 anonymously. Get a free API key for higher rate limits.",
            "requested_schema": {"type": "object", "properties": {}}});
        let login = json!({"session_id": "s", "hook_event_name": "Elicitation", "mcp_server_name": "db", "mode": "form", "message": "Log in",
            "requested_schema": {"type": "object", "properties": {"username": {"type": "string", "title": "Username"}, "port": {"type": "integer"}, "remember": {"type": "boolean"}}}});
        let token = json!({"session_id": "s", "hook_event_name": "Elicitation", "mcp_server_name": "gh", "mode": "form", "message": "Paste the token",
            "requested_schema": {"type": "object", "properties": {"token": {"type": "string", "title": "Token"}}}});
        let browser = json!({"session_id": "s", "hook_event_name": "Elicitation", "mcp_server_name": "auth", "mode": "url", "message": "Please authenticate", "url": "https://auth.example.com/login"});
        struct Case {
            what: &'static str,
            payload: Value,
            title: &'static str,
            accept_hint: Option<&'static str>,
            answer: Answer,
            response: Value,
        }
        let cases = [
            Case { what: "context7's anonymous notice, accepted", payload: context7.clone(), title: "You're using Context7 anonymously. Get a free API key for higher rate limits.", accept_hint: None,
                answer: answered("Accept", Some("Accept")), response: json!({"hookEventName": "Elicitation", "action": "accept", "content": {}}) },
            Case { what: "context7's anonymous notice, declined", payload: context7.clone(), title: "You're using Context7 anonymously. Get a free API key for higher rate limits.", accept_hint: None,
                answer: answered("Decline", Some("Decline")), response: json!({"hookEventName": "Elicitation", "action": "decline"}) },
            Case { what: "more context cancels", payload: context7, title: "You're using Context7 anonymously. Get a free API key for higher rate limits.", accept_hint: None,
                answer: answered("Why?", Some(answer_option::MORE_CONTEXT)), response: json!({"hookEventName": "Elicitation", "action": "cancel"}) },
            Case { what: "a one-field form takes the whole answer", payload: token, title: "Paste the token", accept_hint: Some("Write the Token"),
                answer: answered("ghp_123", Some("Accept")), response: json!({"hookEventName": "Elicitation", "action": "accept", "content": {"token": "ghp_123"}}) },
            Case { what: "a form's fields come one per line, typed by the schema", payload: login.clone(), title: "Log in", accept_hint: Some("Write one per line: port: …, remember: …, username: …"),
                answer: answered("Username: ana\nport: 5432\nremember: yes", Some("Accept")), response: json!({"hookEventName": "Elicitation", "action": "accept", "content": {"username": "ana", "port": 5432, "remember": true}}) },
            Case { what: "a form's fields as JSON", payload: login, title: "Log in", accept_hint: Some("Write one per line: port: …, remember: …, username: …"),
                answer: answered(r#"{"username": "ana"}"#, Some("Accept")), response: json!({"hookEventName": "Elicitation", "action": "accept", "content": {"username": "ana"}}) },
            Case { what: "a browser link sends no form", payload: browser, title: "Please authenticate", accept_hint: None,
                answer: answered("Accept", Some("Accept")), response: json!({"hookEventName": "Elicitation", "action": "accept"}) },
        ];
        for c in cases {
            let h = HookEvent { session_id: "s".into(), event: "Elicitation".into(), ..Default::default() };
            let q = held_question(&View::default(), &c.payload, &h, QuestionOrigin::Elicitation);
            assert_eq!(q.title, c.title, "{}", c.what);
            assert_eq!(q.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), [answer_option::ACCEPT, answer_option::DECLINE], "{}", c.what);
            assert_eq!(q.options[0].description.as_deref(), c.accept_hint, "{}", c.what);
            assert!(q.detail.as_deref().is_some_and(|d| d.contains(str_field(&c.payload, "mcp_server_name").unwrap().as_str())), "{}", c.what);
            let schema = c.payload.get("requested_schema").cloned().unwrap_or(Value::Null);
            assert_eq!(hold_decision(QuestionOrigin::Elicitation, &c.answer, schema, "Ana")["hookSpecificOutput"], c.response, "{}", c.what);
        }
    }

    #[test]
    fn what_the_user_writes_when_turning_down_a_plan_or_a_permission_follows_as_a_message() {
        let plan_reason = |a: &Answer| decision(QuestionOrigin::Plan, a)["hookSpecificOutput"]["permissionDecisionReason"].clone();
        let deny_message = |a: &Answer| decision(QuestionOrigin::Permission, a)["hookSpecificOutput"]["decision"]["message"].clone();
        let cases: [(&str, QuestionOrigin, Answer, Option<&str>, Value); 8] = [
            ("changes with text", QuestionOrigin::Plan, answered("Use a feature flag instead", Some("Request changes")), Some("Use a feature flag instead"),
                json!("Ana requested changes; the message follows")),
            ("changes, no text", QuestionOrigin::Plan, answered("Request changes", Some("Request changes")), None, json!("Ana requests changes to the plan")),
            ("free text on a plan", QuestionOrigin::Plan, answered("Split it in two PRs", None), Some("Split it in two PRs"), json!("Ana requested changes; the message follows")),
            ("more context stays in the reason", QuestionOrigin::Plan, answered("Why?", Some(answer_option::MORE_CONTEXT)), None, json!("Why?")),
            ("deny with a reason", QuestionOrigin::Permission, answered("Not on main, branch first", Some("Deny")), Some("Not on main, branch first"),
                json!("Ana denied it; the message follows")),
            ("deny, no text", QuestionOrigin::Permission, answered("Deny", Some("Deny")), None, json!("Ana denies it")),
            ("the old Spanish deny label", QuestionOrigin::Permission, answered("Denegar", Some("Denegar")), None, json!("Ana denies it")),
            ("allow says nothing more", QuestionOrigin::Permission, answered("Allow", Some("Allow")), None, Value::Null),
        ];
        for (what, origin, a, follow, reason) in cases {
            assert_eq!(held_follow_up(origin, &a).as_deref(), follow, "{what}");
            let said = if origin == QuestionOrigin::Plan { plan_reason(&a) } else { deny_message(&a) };
            assert_eq!(said, reason, "{what}");
        }
        assert_eq!(held_follow_up(QuestionOrigin::AskUserQuestion, &answered("Blue", None)), None);
        assert_eq!(held_follow_up(QuestionOrigin::Elicitation, &answered("ghp_1", Some("Accept"))), None);

        let mut v = View::default();
        v.apply(1, 1, &Event::DaemonSeen(alondra_core::DaemonObservation { session_id: "s".into(), short_id: "s".into(), ..Default::default() }));
        let queued = follow_up_message(&v, "s", "Use a feature flag instead");
        v.apply(2, 2, &queued);
        let pending: Vec<&Message> = v.pending_messages("s").collect();
        assert_eq!(pending.len(), 1);
        assert_eq!((pending[0].from, pending[0].text.as_str(), pending[0].answers_question), (MessageFrom::User, "Use a feature flag instead", None));
    }

    #[test]
    fn the_legacy_more_context_option_passes_the_request_through_untouched() {
        let a = answered("Explain the risk", Some("Necesito más contexto"));
        let out = decision(QuestionOrigin::Plan, &a);
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(out["hookSpecificOutput"]["permissionDecisionReason"], "Explain the risk");
    }

    #[test]
    fn resolving_an_action_card_tells_the_node_what_happened() {
        let cases = [
            (answered("Done", Some("Done")), "Ana has done it."),
            (answered("done", None), "Ana has done it."),
            (answered("Key saved in the vault", Some("Done")), "Ana has done it. Ana adds: Key saved in the vault"),
            (answered("I have no access to App Store Connect", Some("I can't / explain")), "Ana cannot do it as asked: I have no access to App Store Connect"),
            (answered("I can't / explain", Some("I can't / explain")), "Ana cannot do it as asked. Find out what is in the way and explain it again with simpler steps."),
            (answered("Which key?", None), "Which key?"),
        ];
        for (a, expected) in cases {
            assert_eq!(action_reply(&a, "Ana"), expected, "{a:?}");
        }
    }

    #[test]
    fn launching_over_todays_slice_needs_the_users_word() {
        let over = alondra_core::usage::Usage { today_spent: Some(41.0), today_share: Some(18.0), week_percent: 53.0, per_day_left: 9.0, over_pace: true, ..Default::default() };
        let within = alondra_core::usage::Usage { over_pace: false, ..over.clone() };
        let cases = [
            ("over pace", Some(&over), false, true),
            ("over pace, the user said go", Some(&over), true, false),
            ("within today's slice", Some(&within), false, false),
            ("no usage known on this Mac", None, false, false),
        ];
        for (what, usage, ok, blocked) in cases {
            assert_eq!(launch_blocked(usage, ok).is_some(), blocked, "{what}");
        }
        assert!(launch_blocked(Some(&over), false).unwrap().contains("41% of the week against a 18% slice"));
    }

    #[test]
    fn a_permission_card_is_titled_by_what_the_node_says_it_does_and_shows_the_whole_command() {
        let command = "git worktree remove --force /Users/ana/Projects/lumo/.claude/worktrees/permiso-camara; git branch -D permiso-camara";
        let explained = "- `worktree remove --force`: borra el worktree aunque tenga cambios sin commit\n\
                         - `branch -D`: borra la rama aunque no esté mergeada; sus commits sin push se pierden";
        let mut v = View::default();
        v.apply(1, 1, &Event::DaemonSeen(alondra_core::DaemonObservation { session_id: "s".into(), short_id: "a1b2c3d4".into(), ..Default::default() }));
        struct Case {
            what: &'static str,
            tool: &'static str,
            input: Value,
            title: String,
            detail: Option<String>,
        }
        let long = "x".repeat(150);
        let cases = [
            Case { what: "the real worktree cleanup, with its description", tool: "Bash",
                input: json!({"command": command, "description": "Remove the camera worktree and its branch"}),
                title: "⚠️ destructivo · Bash: Remove the camera worktree and its branch".into(), detail: Some(format!("{explained}\n\n```\n{command}\n```")) },
            Case { what: "the same command without a description", tool: "Bash", input: json!({"command": command}),
                title: format!("⚠️ destructivo · Bash: {}…", command.chars().take(PERMISSION_TITLE_CHARS).collect::<String>().trim_end()), detail: Some(format!("{explained}\n\n```\n{command}\n```")) },
            Case { what: "a harmless command", tool: "Bash", input: json!({"command": "ls -la", "description": "List files"}),
                title: "Bash: List files".into(), detail: Some("```\nls -la\n```".into()) },
            Case { what: "a file edit", tool: "Edit", input: json!({"file_path": "/tmp/a.rs", "old_string": "a", "new_string": "b"}),
                title: "Edit: /tmp/a.rs".into(), detail: Some("/tmp/a.rs".into()) },
            Case { what: "a long path is cut", tool: "Write", input: json!({"file_path": long}),
                title: format!("Write: {}…", "x".repeat(PERMISSION_TITLE_CHARS)), detail: Some(long.clone()) },
        ];
        for c in cases {
            let p = json!({"session_id": "s", "tool_name": c.tool, "tool_input": c.input});
            let h = HookEvent { session_id: "s".into(), event: "PermissionRequest".into(), tool_name: Some(c.tool.into()), tool_summary: tool_summary(Some(c.tool), p.get("tool_input")), ..Default::default() };
            let q = held_question(&v, &p, &h, QuestionOrigin::Permission);
            assert_eq!(q.title, c.title, "{}", c.what);
            assert_eq!(q.detail, c.detail, "{}", c.what);
            assert_eq!(q.node_name.as_deref(), Some("a1b2c3d4"), "{}: an unnamed node goes by its short id", c.what);
        }
    }

    #[test]
    fn destructive_commands_are_told_apart() {
        let cases = [
            ("git worktree remove --force x", true),
            ("git branch -D feature", true),
            ("rm -rf build", true),
            ("cd x && rm file", true),
            ("git reset --hard origin/main", true),
            ("git push --force-with-lease", true),
            ("git push -f origin main", true),
            ("git clean -fdx", true),
            ("git push origin main", false),
            ("git branch -d merged", false),
            ("cargo test --release", false),
            ("git clean -n", false),
            ("ls -f", false),
        ];
        for (command, expected) in cases {
            assert_eq!(destructive(command), expected, "{command}");
        }
    }

    #[test]
    fn each_risky_flag_is_explained_in_plain_words() {
        let cases: [(&str, &[&str]); 9] = [
            ("git push --force-with-lease origin x", &["`push --force`"]),
            ("git push -f", &["`push --force`"]),
            ("git worktree remove --force wt; git branch -D wt", &["`worktree remove --force`", "`branch -D`"]),
            ("git reset --hard origin/main", &["`reset --hard`"]),
            ("git clean -fdx", &["`clean -f`"]),
            ("cd build && rm -rf out", &["`rm`"]),
            ("brew upgrade --force node", &["`--force`"]),
            ("git rm old.txt", &[]),
            ("git branch -d merged && git push origin main", &[]),
        ];
        for (command, expected) in cases {
            let said: Vec<&str> = risks(command).iter().map(|r| r.split(':').next().unwrap()).collect();
            assert_eq!(said, expected, "{command}");
        }
        assert_eq!(risks("rm a; rm b").len(), 1, "one line per kind of risk");
    }

    #[tokio::test]
    async fn a_question_about_a_report_is_one_card_and_answering_it_reads_the_report() {
        let hub = temp_hub("absorb");
        hub.append(Event::MarkedOrchestrator { session_id: "orq".into() }).unwrap();
        hub.append(Event::Hook(HookEvent { session_id: "audit".into(), event: "UserPromptSubmit".into(), ..Default::default() })).unwrap();
        let now = now_millis();
        hub.append(Event::ReportPosted(Report {
            related: vec![],
            id: 1, session_id: "audit".into(), node_name: None, title: "Auditoría: 13 cosas".into(), body: Some("Detalle adjunto".into()),
            links: vec![], attachments: vec![], kind: ReportKind::Document, created_at: now, read: false, mission: None, looks_like_question: false,
        })).unwrap();
        hub.append(Event::QuestionAsked(Question {
            related: vec![],
            id: 1, session_id: "orq".into(), node_name: None, title: "¿Lanzo un nodo que lo arregle?".into(),
            detail: Some("La auditoría (informe #1) encontró 13 cosas".into()), options: vec![], links: vec![], attachments: vec![], blocking: true,
            origin: QuestionOrigin::Ask, tool_use_id: None, to_orchestrator: false, node_context: None, status: QuestionStatus::Open, answer: None,
            asked_at: now + 20_000, action: false, steps: vec![], mission: None,
        })).unwrap();
        let Json(o) = overview(State(hub.clone())).await;
        assert_eq!(o["news"].as_array().unwrap().len(), 0, "the report is not a card of its own: {}", o["news"]);
        assert_eq!(o["open_questions"][0]["report"]["title"], "Auditoría: 13 cosas");

        let body = AnswerBody { text: "Sí, lánzalo".into(), option: None, by: None, attachments: vec![] };
        let (status, said) = status_and_body(answer(State(hub.clone()), None, Path(1), Json(body)).await).await;
        assert_eq!(status, StatusCode::OK, "{said}");
        assert!(hub.read(|v| v.reports[&1].read), "answering the card settles the report inside it");
        let Json(o) = overview(State(hub.clone())).await;
        assert_eq!(o["news"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn a_report_about_a_pr_goes_inside_its_card_and_the_node_is_told() {
        let cases: [(&str, Vec<Link>, Option<u64>); 3] = [
            ("links the PR", vec![Link { url: "https://github.com/a/b/pull/24".into(), title: None }], Some(1)),
            ("links a page of the PR", vec![Link { url: "https://github.com/a/b/pull/24/checks".into(), title: None }], Some(1)),
            ("links nothing", vec![], None),
        ];
        for (what, links, card) in cases {
            let hub = temp_hub("report-pr");
            hub.append(Event::Hook(HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), ..Default::default() })).unwrap();
            let body = ReportBody {
                caller: Caller { session_id: Some("s".into()), pids: vec![] },
                title: "Listo".into(),
                body: Some("Hecho y probado".into()),
                kind: Some(ReportKind::Final),
                links,
                attachments: vec![],
                related: None,
            };
            let (status, said) = status_and_body(report(State(hub.clone()), Json(body)).await).await;
            assert_eq!(status, StatusCode::OK, "{what}: {said}");
            assert_eq!(said["shown_in_pr"].as_u64(), card, "{what}: {said}");
            assert_eq!(said["note"].as_str().is_some_and(|n| n.contains("PR #24")), card.is_some(), "{what}: {said}");
            let Json(o) = overview(State(hub.clone())).await;
            assert_eq!(o["news"][0]["pr_id"].as_u64(), card, "{what}: {}", o["news"]);
        }
    }

    /// PR #54, 24 Sep: fase-1 moved while its card still offered merging, and the merge came back
    /// as gh's raw "merge commit cannot be cleanly created".
    #[tokio::test]
    async fn a_pr_in_conflict_is_not_merged_and_resolving_it_goes_to_its_node() {
        use alondra_core::pr::MergeMethod;
        let hub = temp_hub("conflicts");
        hub.append(Event::Hook(HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), ..Default::default() })).unwrap();
        hub.append(Event::PrOpened { url: "https://github.com/a/b/pull/54".into(), session_id: "s".into() }).unwrap();
        let card = |mergeable: &str, head: &str| {
            let mut pr = hub.read(|v| v.prs[&1].clone());
            (pr.mergeable, pr.base, pr.head_sha, pr.fetched) = (Some(mergeable.into()), Some("main".into()), Some(head.into()), true);
            pr
        };
        hub.append(Event::PrUpdated(card("CONFLICTING", "abc"))).unwrap();
        let in_inbox = |hub: &Arc<Hub>| hub.read(|v| crate::prs::in_inbox(v).count());

        let (status, said) = status_and_body(merge_pr(State(hub.clone()), None, Path(1), Json(MergeBody { method: MergeMethod::Merge })).await).await;
        assert_eq!(status, StatusCode::CONFLICT, "{said}");
        assert_eq!(said["conflicts"], true);
        assert_eq!(said["error"], "PR #54 conflicts with main: resolve the conflicts first.");
        assert_eq!(hub.read(|v| v.prs[&1].state), alondra_core::pr::PrState::Open);

        let (status, said) = status_and_body(resolve_pr_conflicts(State(hub.clone()), Path(1)).await).await;
        assert_eq!((status, said["sent_to"].as_str()), (StatusCode::OK, Some("s")), "{said}");
        let told = hub.read(|v| v.messages.values().last().map(|m| (m.session_id.clone(), m.text.clone()))).unwrap();
        assert_eq!(told.0, "s");
        assert!(told.1.contains("resolve the conflicts of https://github.com/a/b/pull/54 with main"), "{}", told.1);
        assert_eq!(in_inbox(&hub), 0, "the card waits for the node's push");

        hub.append(Event::PrUpdated(card("MERGEABLE", "def"))).unwrap();
        assert_eq!(in_inbox(&hub), 1, "back once the node pushed");
        assert!(!hub.read(|v| v.prs[&1].conflicts()));
    }

    #[tokio::test]
    async fn the_overview_and_the_node_detail_list_what_runs_and_flag_the_possibly_hung() {
        use alondra_core::{BackgroundItem, BackgroundKind, DaemonObservation};
        let hub = temp_hub("background");
        let now = now_millis();
        let item = |kind, label: &str, started_at| BackgroundItem { kind, label: label.into(), started_at, suspect: false };
        hub.append(Event::DaemonSeen(DaemonObservation {
            session_id: "job".into(), short_id: "job".into(), pid: Some(std::process::id() as i32), tempo: Some("idle".into()), tasks: Some(2),
            background: vec![item(BackgroundKind::Shell, "until pgrep", now - 2 * 3600 * 1000), item(BackgroundKind::Agent, "Review", now - 60_000)],
            ..Default::default()
        })).unwrap();
        let Json(o) = overview(State(hub.clone())).await;
        let listed = &o["nodes"][0]["background"];
        assert_eq!(listed[0]["kind"], "shell");
        assert_eq!(listed[0]["suspect"], true);
        assert_eq!(listed[1]["kind"], "agent");
        assert!(listed[1].get("suspect").is_none(), "{listed}");
        let (status, detail) = status_and_body(node_detail(State(hub.clone()), Path("job".into()), Query(TimelineQuery { limit: None, before: None })).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["node"]["background"][0]["suspect"], true);
        assert!(o["nodes"][0].get("alive").is_none() && detail["node"].get("alive").is_none(), "clients never see alive");
        assert!(hub.read(|v| serde_json::to_value(v).unwrap())["nodes"]["job"]["alive"] == true, "the stored snapshot keeps it");
    }

    #[derive(Clone, Copy, Debug)]
    enum NewCard {
        Ask,
        Report,
    }

    /// David's rule (24 Sep): Alondra never infers which cards go together. A node that already has
    /// open cards declares it with `related`; without it nothing is recorded.
    #[test]
    fn related_arrives_as_a_list_or_as_the_text_older_tool_schemas_send() {
        #[derive(Deserialize)]
        struct Ids {
            #[serde(default, deserialize_with = "card_ids")]
            related: Option<Vec<u64>>,
            #[serde(default, deserialize_with = "card_id")]
            replaces: Option<u64>,
        }
        let cases: [(Value, Option<Option<Vec<u64>>>, Option<u64>); 11] = [
            (json!({}), Some(None), None),
            (json!({"related": null}), Some(None), None),
            (json!({"related": []}), Some(Some(vec![])), None),
            (json!({"related": [3, 4]}), Some(Some(vec![3, 4])), None),
            (json!({"related": "[]"}), Some(Some(vec![])), None),
            (json!({"related": "[3, 4]"}), Some(Some(vec![3, 4])), None),
            (json!({"related": "#3,4"}), Some(Some(vec![3, 4])), None),
            (json!({"related": 3}), Some(Some(vec![3])), None),
            (json!({"related": "tres"}), None, None),
            (json!({"replaces": "191"}), Some(None), Some(191)),
            (json!({"replaces": 7}), Some(None), Some(7)),
        ];
        for (body, related, replaces) in cases {
            let parsed = serde_json::from_value::<Ids>(body.clone());
            assert_eq!(parsed.as_ref().ok().map(|p| p.related.clone()), related, "{body}");
            if let Ok(p) = parsed {
                assert_eq!(p.replaces, replaces, "{body}");
            }
        }
    }

    #[tokio::test]
    async fn a_node_with_open_cards_says_what_a_new_one_belongs_with() {
        const PR: &str = "https://github.com/a/b/pull/24";
        struct Case {
            what: &'static str,
            open_card: bool,
            related: Option<Vec<u64>>,
            links_pr: bool,
            status: StatusCode,
            grouped: bool,
        }
        let case = |what, open_card, related: Option<Vec<u64>>, status, grouped| Case { what, open_card, related, links_pr: false, status, grouped };
        let cases = [
            case("no open cards, nothing declared", false, None, StatusCode::OK, false),
            case("open cards, nothing declared", true, None, StatusCode::UNPROCESSABLE_ENTITY, false),
            case("open cards, declared unrelated", true, Some(vec![]), StatusCode::OK, false),
            case("open cards, declared related", true, Some(vec![1]), StatusCode::OK, true),
            case("another node's card", true, Some(vec![2]), StatusCode::FORBIDDEN, false),
            case("a card already read", true, Some(vec![3]), StatusCode::CONFLICT, false),
            case("a card that does not exist", true, Some(vec![99]), StatusCode::NOT_FOUND, false),
            Case { links_pr: true, ..case("it goes inside a PR card", true, None, StatusCode::OK, false) },
        ];
        for kind in [NewCard::Ask, NewCard::Report] {
            for c in &cases {
                let what = format!("{kind:?}: {}", c.what);
                let hub = temp_hub("related");
                for sid in ["s", "t"] {
                    hub.append(Event::Hook(HookEvent { session_id: sid.into(), event: "UserPromptSubmit".into(), ..Default::default() })).unwrap();
                }
                let now = now_millis();
                let posted = |id: u64, sid: &str| Report {
                    related: vec![], id, session_id: sid.into(), node_name: None, title: format!("Informe {id}"), body: Some("Hecho".into()),
                    links: vec![], attachments: vec![], kind: ReportKind::Final, created_at: now, read: false, mission: None, looks_like_question: false,
                };
                for (id, sid) in [(1, "s"), (2, "t"), (3, "s")] {
                    hub.append(Event::ReportPosted(posted(id, sid))).unwrap();
                }
                hub.append(Event::ReportRead { id: 3 }).unwrap();
                if !c.open_card {
                    hub.append(Event::ReportRead { id: 1 }).unwrap();
                }
                let links = if c.links_pr { vec![Link { url: PR.into(), title: None }] } else { vec![] };
                let caller = Caller { session_id: Some("s".into()), pids: vec![] };
                let response = match kind {
                    NewCard::Ask => ask(State(hub.clone()), Json(AskBody {
                        caller, title: "¿Sigo?".into(), detail: Some("Lo anterior está subido".into()), options: vec![], links, attachments: vec![],
                        blocking: true, to_orchestrator: false, steps: vec![], related: c.related.clone(), replaces: None,
                    })).await,
                    NewCard::Report => report(State(hub.clone()), Json(ReportBody {
                        caller, title: "Anexo".into(), body: Some("Más detalle".into()), kind: Some(ReportKind::Final), links, attachments: vec![],
                        related: c.related.clone(),
                    })).await,
                };
                let (status, said) = status_and_body(response).await;
                assert_eq!(status, c.status, "{what}: {said}");
                let recorded = hub.read(|v| match kind {
                    NewCard::Ask => v.questions.len(),
                    NewCard::Report => v.reports.len() - 3,
                });
                assert_eq!(recorded, usize::from(status == StatusCode::OK), "{what}: a refused card is not recorded");
                if status == StatusCode::UNPROCESSABLE_ENTITY {
                    let msg = said.to_string();
                    assert!(msg.contains("#1 report (finished): Informe 1") && msg.contains("`related`") && msg.contains("`[]`"), "{what}: {msg}");
                }
                let Json(o) = overview(State(hub.clone())).await;
                let carried = match kind {
                    NewCard::Ask => o["open_questions"][0]["reports"].as_array().is_some_and(|r| r.iter().any(|r| r["id"] == 1)),
                    NewCard::Report => o["news"].as_array().unwrap().iter().any(|n| n["earlier"].as_array().is_some_and(|e| e.iter().any(|r| r["id"] == 1))),
                };
                assert_eq!(carried, c.grouped, "{what}: {o}");
            }
        }
    }

    /// alondra/retry-queue, 24 Sep: the iPhone kept a merge "pending" forever because alondrad
    /// answered gh's refusal with a 502, which clients read as "alondrad is down".
    #[tokio::test]
    async fn a_merge_sent_again_succeeds_and_a_refusal_is_named() {
        use alondra_core::pr::{MergeMethod, PrState, PullRequest};
        let closed = |state: PrState| move |pr: PullRequest| Event::PrUpdated(PullRequest { state, ..pr });
        type Before = Box<dyn Fn(PullRequest) -> Event>;
        let cases: [(&str, u64, Option<Before>, StatusCode, Value); 5] = [
            ("merged from a card", 1, Some(Box::new(|_| Event::PrMerged { id: 1, method: MergeMethod::Merge })), StatusCode::OK, json!({"already": true})),
            ("merged on GitHub", 1, Some(Box::new(closed(PrState::Merged))), StatusCode::OK, json!({"already": true})),
            ("closed on GitHub", 1, Some(Box::new(closed(PrState::Closed))), StatusCode::CONFLICT, json!({"kind": "pr_not_open"})),
            ("set aside", 1, Some(Box::new(|_| Event::PrDismissed { id: 1 })), StatusCode::CONFLICT, json!({"kind": "pr_not_open"})),
            ("no such card", 9, None, StatusCode::NOT_FOUND, json!({})),
        ];
        for (what, id, before, status, fields) in cases {
            let hub = temp_hub("merge-again");
            hub.append(Event::PrOpened { url: "https://github.com/a/b/pull/24".into(), session_id: "s".into() }).unwrap();
            if let Some(before) = before {
                let pr = hub.read(|v| v.prs[&1].clone());
                hub.append(before(pr)).unwrap();
            }
            let (got, said) = status_and_body(merge_pr(State(hub.clone()), None, Path(id), Json(MergeBody { method: MergeMethod::Merge })).await).await;
            assert_eq!(got, status, "{what}: {said}");
            for (k, v) in fields.as_object().unwrap() {
                assert_eq!(&said[k], v, "{what}: {said}");
            }
        }
    }

    #[tokio::test]
    async fn answering_a_question_reads_the_reports_it_carries() {
        let hub = temp_hub("related-answer");
        hub.append(Event::Hook(HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), ..Default::default() })).unwrap();
        let now = now_millis();
        hub.append(Event::ReportPosted(Report {
            related: vec![], id: 1, session_id: "s".into(), node_name: None, title: "Menús como tarjeta".into(), body: Some("Hecho".into()),
            links: vec![], attachments: vec![], kind: ReportKind::Final, created_at: now, read: false, mission: None, looks_like_question: false,
        })).unwrap();
        let body = AskBody {
            caller: Caller { session_id: Some("s".into()), pids: vec![] }, title: "¿Sigo con el salto de la TUI?".into(),
            detail: Some("Lo anterior está subido".into()), options: vec![], links: vec![], attachments: vec![], blocking: true,
            to_orchestrator: false, steps: vec![], related: Some(vec![1]), replaces: None,
        };
        let (status, said) = status_and_body(ask(State(hub.clone()), Json(body)).await).await;
        assert_eq!(status, StatusCode::OK, "{said}");
        let body = AnswerBody { text: "Sí".into(), option: None, by: None, attachments: vec![] };
        let (status, said) = status_and_body(answer(State(hub.clone()), None, Path(1), Json(body)).await).await;
        assert_eq!(status, StatusCode::OK, "{said}");
        assert!(hub.read(|v| v.reports[&1].read), "answering reads the report inside the question");
    }

    /// Q#116 "¿Arranco las capturas del iPhone Duo?" and, a minute later, Q#117 "Apple no exige
    /// capturas del iPhone Duo… ¿Las hago igual?" (ios27/rc-y-duo, 23 Sep): the second replaced the
    /// first, and both stayed open.
    #[tokio::test]
    async fn a_question_can_replace_the_nodes_own_open_question() {
        struct Case {
            what: &'static str,
            replaces: Option<u64>,
            related: Option<Vec<u64>>,
            another_open: bool,
            status: StatusCode,
            old: QuestionStatus,
        }
        let case = |what, replaces, related: Option<Vec<u64>>, another_open, status, old| Case { what, replaces, related, another_open, status, old };
        let cases = [
            case("replaces its only open question", Some(1), None, false, StatusCode::OK, QuestionStatus::Withdrawn),
            case("replaces it while another card is open, undeclared", Some(1), None, true, StatusCode::UNPROCESSABLE_ENTITY, QuestionStatus::Open),
            case("replaces it while another card is open, declared apart", Some(1), Some(vec![]), true, StatusCode::OK, QuestionStatus::Withdrawn),
            case("replaces nothing while a card is open", None, None, false, StatusCode::UNPROCESSABLE_ENTITY, QuestionStatus::Open),
            case("another node's question", Some(2), Some(vec![]), false, StatusCode::FORBIDDEN, QuestionStatus::Open),
            case("an answered question", Some(3), Some(vec![]), false, StatusCode::CONFLICT, QuestionStatus::Open),
            case("a question that does not exist", Some(99), Some(vec![]), false, StatusCode::NOT_FOUND, QuestionStatus::Open),
        ];
        for c in cases {
            let what = c.what;
            let hub = temp_hub("replaces");
            for sid in ["mine", "other"] {
                hub.append(Event::Hook(HookEvent { session_id: sid.into(), event: "UserPromptSubmit".into(), ..Default::default() })).unwrap();
            }
            hub.append(Event::ReportPosted(Report {
                related: vec![], id: 1, session_id: "mine".into(), node_name: None, title: "Capturas del iPhone".into(), body: Some("Hechas".into()),
                links: vec![], attachments: vec![], kind: ReportKind::Notice, created_at: 1, read: false, mission: None, looks_like_question: false,
            })).unwrap();
            let asked = |id: u64, sid: &str, related: Vec<CardRef>| Question {
                related, id, session_id: sid.into(), node_name: None, title: "¿Arranco las capturas?".into(), detail: None, options: vec![], links: vec![],
                attachments: vec![], blocking: true, origin: QuestionOrigin::Ask, tool_use_id: None, to_orchestrator: false, node_context: None,
                status: QuestionStatus::Open, answer: None, asked_at: 1, action: false, steps: vec![], mission: None,
            };
            hub.append(Event::QuestionAsked(asked(1, "mine", vec![CardRef::Report(1)]))).unwrap();
            hub.append(Event::QuestionAsked(asked(2, "other", vec![]))).unwrap();
            hub.append(Event::QuestionAsked(asked(3, "mine", vec![]))).unwrap();
            hub.append(Event::QuestionAnswered { id: 3, answer: Answer { text: "Sí".into(), option: None, by: None, device: None, at: 2 } }).unwrap();
            if c.another_open {
                hub.append(Event::QuestionAsked(asked(4, "mine", vec![]))).unwrap();
            }
            let before = hub.read(|v| v.questions.len());
            let body = AskBody {
                caller: Caller { session_id: Some("mine".into()), pids: vec![] },
                title: "Apple no exige capturas del iPhone Duo: ¿las hago igual?".into(),
                detail: Some("No se pueden subir aún".into()),
                options: vec![],
                links: vec![],
                attachments: vec![],
                blocking: true,
                to_orchestrator: false,
                steps: vec![],
                related: c.related.clone(),
                replaces: c.replaces,
            };
            let (status, said) = status_and_body(ask(State(hub.clone()), Json(body)).await).await;
            assert_eq!(status, c.status, "{what}: {said}");
            assert_eq!(hub.read(|v| v.questions[&1].status), c.old, "{what}");
            let after = hub.read(|v| v.questions.len());
            assert_eq!(after - before, usize::from(status == StatusCode::OK), "{what}: a refused question records nothing");
            if status == StatusCode::OK && c.replaces == Some(1) {
                let carried = hub.read(|v| v.question_reports(&v.questions[&(after as u64)]));
                assert_eq!(carried, [1], "{what}: the new question carries what the replaced one carried");
            }
        }
    }

    #[test]
    fn gh_saying_the_pr_is_already_merged_is_told_apart_from_a_refusal() {
        let cases = [
            ("X Pull request a/b#24 was already merged", true),
            ("GraphQL: Pull Request is already merged (mergePullRequest)", true),
            ("X Pull request a/b#54 is not mergeable: the merge commit cannot be cleanly created.", false),
            ("GraphQL: Base branch was modified. Review and try the merge again. (mergePullRequest)", false),
            ("", false),
        ];
        for (stderr, expected) in cases {
            assert_eq!(gh_says_already_merged(stderr), expected, "{stderr}");
        }
    }

    #[tokio::test]
    async fn an_answer_sent_again_succeeds_and_a_different_one_is_refused() {
        let body = |text: &str| Json(AnswerBody { text: text.into(), option: None, by: None, attachments: vec![] });
        let cases: [(&str, Option<&str>, bool, &str, StatusCode, Value); 5] = [
            ("first answer", None, false, "Sí", StatusCode::OK, json!({"id": 1})),
            ("the same answer again", Some("Sí"), false, "Sí", StatusCode::OK, json!({"already": true})),
            ("the same answer after delivery", Some("Sí"), true, "Sí", StatusCode::OK, json!({"already": true})),
            ("a different answer", Some("Sí"), false, "No", StatusCode::CONFLICT, json!({"kind": "question_closed"})),
            ("withdrawn", None, true, "Sí", StatusCode::CONFLICT, json!({"kind": "question_closed"})),
        ];
        for (what, earlier, closed, text, status, fields) in cases {
            let hub = temp_hub("answer-again");
            hub.append(Event::Hook(HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), ..Default::default() })).unwrap();
            hub.append(Event::QuestionAsked(Question {
                related: vec![], id: 1, session_id: "s".into(), node_name: None, title: "¿Sigo?".into(), detail: None, options: vec![], links: vec![],
                attachments: vec![], blocking: true, origin: QuestionOrigin::Ask, tool_use_id: None, to_orchestrator: false,
                node_context: None, status: QuestionStatus::Open, answer: None, asked_at: now_millis(), action: false, steps: vec![], mission: None,
            })).unwrap();
            if let Some(earlier) = earlier {
                let (got, said) = status_and_body(answer(State(hub.clone()), None, Path(1), body(earlier)).await).await;
                assert_eq!(got, StatusCode::OK, "{what}: {said}");
            }
            if closed {
                let status = if earlier.is_some() { QuestionStatus::Delivered } else { QuestionStatus::Withdrawn };
                hub.append(Event::QuestionClosed { id: 1, status, note: None }).unwrap();
            }
            let messages = hub.read(|v| v.messages.len());
            let (got, said) = status_and_body(answer(State(hub.clone()), None, Path(1), body(text)).await).await;
            assert_eq!(got, status, "{what}: {said}");
            for (k, v) in fields.as_object().unwrap() {
                assert_eq!(&said[k], v, "{what}: {said}");
            }
            if said["already"] == json!(true) {
                assert_eq!(hub.read(|v| v.messages.len()), messages, "{what}: the node is not told twice");
            }
        }
    }

    pub(super) fn temp_hub(tag: &str) -> Arc<Hub> {
        hub_with(tag, false)
    }

    fn hub_with(tag: &str, sandbox: bool) -> Arc<Hub> {
        hub_on(tag, sandbox, FakeGithub::default())
    }

    fn hub_on(tag: &str, sandbox: bool, github: FakeGithub) -> Arc<Hub> {
        let home = std::env::temp_dir().join(format!("alondra-http-{tag}-{}-{}", std::process::id(), now_millis()));
        let env = move |k: &str| (sandbox && k == "ALONDRA_SANDBOX").then(|| "1".to_string());
        let config = alondra_core::config::Config::resolve(home, &alondra_core::config::FileConfig::default(), env, || Some("Ana".into()));
        Arc::new(Hub::open(config, false).unwrap().with_github(github))
    }

    /// GitHub as the tests see it: every PR exists but the `missing` ones.
    #[derive(Clone, Default)]
    struct FakeGithub {
        missing: Vec<&'static str>,
        asked: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl crate::prs::Gh for FakeGithub {
        async fn edit_base(&self, _url: &str, _base: &str) -> Result<(), String> {
            Ok(())
        }

        async fn view(&self, _url: &str) -> Option<Value> {
            None
        }

        async fn exists(&self, url: &str) -> bool {
            self.asked.lock().unwrap().push(url.into());
            !self.missing.contains(&url)
        }
    }

    const FIXTURE: &str = "https://github.com/a/b/pull/7";

    fn card_urls(hub: &Hub) -> Vec<String> {
        hub.read(|v| v.prs.values().map(|p| p.url.clone()).collect())
    }

    async fn post_tool_use(hub: &Arc<Hub>, command: &str, stdout: &str) {
        let p = json!({
            "hook_event_name": "PostToolUse", "session_id": "s", "tool_name": "Bash",
            "tool_input": {"command": command}, "tool_response": {"stdout": stdout, "stderr": "", "interrupted": false},
        });
        hook(State(hub.clone()), Json(p)).await;
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
    }

    /// 24 Sep, seq 29139-29143: a node's grep for 'gh pr create' printed Alondra's own test code,
    /// and a/b#7, #24 and #54, its fixture PRs, became cards in David's inbox.
    #[tokio::test]
    async fn only_a_pr_that_gh_pr_create_made_and_github_knows_gets_a_card_from_the_hook() {
        const GREP: &str = "sed -n 680,715p crates/alondrad/src/http.rs; grep -rn 'fn pr_url\\|pull/\\|gh pr create' crates/";
        let grep_out = format!(
            "crates/alondrad/src/http.rs:3031:        hub.append(Event::PrOpened {{ url: \"{FIXTURE}\".into(), session_id: \"s\".into() }}).unwrap();\n\
             crates/alondrad/src/http.rs:2533:        hub.append(Event::PrOpened {{ url: \"https://github.com/a/b/pull/54\".into(), session_id: \"s\".into() }}).unwrap();\n"
        );
        let cases: &[(&str, &str, &str, &[&str], &[&str])] = &[
            ("a grep for gh pr create", GREP, &grep_out, &[], &[]),
            ("a grep whose output ends in a lone PR URL", GREP, &format!("{FIXTURE}\n"), &[], &[]),
            ("an echo", "echo gh pr create", "gh pr create\n", &[], &[]),
            ("a PR made", "gh pr create --title x --body \"see https://github.com/a/b/pull/7\"", "https://github.com/o/r/pull/12\n", &["https://github.com/o/r/pull/12"], &["https://github.com/o/r/pull/12"]),
            ("a PR made after cd", "cd x && gh pr create --fill", "https://github.com/o/r/pull/13\n", &["https://github.com/o/r/pull/13"], &["https://github.com/o/r/pull/13"]),
            ("a PR GitHub does not know", "gh pr create --fill", &format!("{FIXTURE}\n"), &[FIXTURE], &[]),
        ];
        for &(what, command, stdout, asked, cards) in cases {
            let github = FakeGithub { missing: vec![FIXTURE], ..Default::default() };
            let hub = hub_on("hook-pr", false, github.clone());
            post_tool_use(&hub, command, stdout).await;
            assert_eq!(*github.asked.lock().unwrap(), asked, "{what}");
            assert_eq!(card_urls(&hub), cards, "{what}");
        }
    }

    #[tokio::test]
    async fn github_is_not_asked_again_soon_about_a_pr_it_did_not_know() {
        let github = FakeGithub { missing: vec![FIXTURE], ..Default::default() };
        let hub = hub_on("hook-pr-cache", false, github.clone());
        for _ in 0..3 {
            post_tool_use(&hub, "gh pr create --fill", FIXTURE).await;
        }
        assert_eq!(github.asked.lock().unwrap().len(), 1);
        assert!(card_urls(&hub).is_empty());
    }

    #[tokio::test]
    async fn a_fixture_pr_in_a_report_or_question_gets_no_card() {
        let text = format!("Tests pass. Output:\ncrates/alondrad/src/http.rs:3031: url: \"{FIXTURE}\"");
        for kind in [NewCard::Ask, NewCard::Report] {
            let github = FakeGithub { missing: vec![FIXTURE], ..Default::default() };
            let hub = hub_on("fixture-pr", false, github.clone());
            hub.append(Event::Hook(HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), ..Default::default() })).unwrap();
            let caller = Caller { session_id: Some("s".into()), pids: vec![] };
            let links = vec![Link { url: FIXTURE.into(), title: None }];
            let response = match kind {
                NewCard::Ask => ask(State(hub.clone()), Json(AskBody {
                    caller, title: "¿Sigo?".into(), detail: Some(text.clone()), options: vec![], links, attachments: vec![], blocking: true,
                    to_orchestrator: false, steps: vec![], related: None, replaces: None,
                })).await,
                NewCard::Report => report(State(hub.clone()), Json(ReportBody {
                    caller, title: "Listo".into(), body: Some(text.clone()), kind: Some(ReportKind::Notice), links, attachments: vec![], related: None,
                })).await,
            };
            let (status, said) = status_and_body(response).await;
            assert_eq!(status, StatusCode::OK, "{kind:?}: {said}");
            assert_eq!(*github.asked.lock().unwrap(), [FIXTURE], "{kind:?}");
            assert!(card_urls(&hub).is_empty(), "{kind:?}");
        }
    }

    /// test/menu-probe, 24 Sep: a probe run against the real alondrad posted reports #111 and #112
    /// into the user's inbox.
    #[tokio::test]
    async fn test_nodes_reach_only_a_sandbox() {
        for (sandbox, expected) in [(false, StatusCode::UNPROCESSABLE_ENTITY), (true, StatusCode::OK)] {
            let hub = hub_with(if sandbox { "sandbox" } else { "real" }, sandbox);
            for (sid, name) in [("probe", "test/menu-probe"), ("real", "lumo/permiso-camara")] {
                hub.append(Event::DaemonSeen(alondra_core::DaemonObservation { session_id: sid.into(), short_id: sid.into(), name: Some(name.into()), ..Default::default() })).unwrap();
            }
            let body = |sid: &str| ReportBody {
                caller: Caller { session_id: Some(sid.into()), pids: vec![] },
                title: "Menú leído".into(),
                body: Some("La sonda leyó el menú".into()),
                kind: Some(ReportKind::Final),
                links: vec![],
                attachments: vec![],
                related: None,
            };
            let (status, said) = status_and_body(report(State(hub.clone()), Json(body("probe"))).await).await;
            assert_eq!(status, expected, "sandbox {sandbox}: {said}");
            if !sandbox {
                assert!(said["error"].as_str().unwrap().contains("make e2e-sandbox"), "{said}");
            }
            assert_eq!(status_and_body(report(State(hub.clone()), Json(body("real"))).await).await.0, StatusCode::OK, "sandbox {sandbox}: ordinary nodes still report");
        }
        assert!(probe_refused(Some("test/x"), false).is_some());
        assert!(probe_refused(Some("hive/test/x"), false).is_none());
        assert!(probe_refused(None, false).is_none());
    }

    pub(super) async fn status_and_body(r: Response) -> (StatusCode, Value) {
        let status = r.status();
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// lumo/permiso-camara and hive/servidor-colmena, 24 Sep: close ran `claude rm` on sessions
    /// that were never background jobs and failed with "No job matching".
    #[tokio::test]
    async fn closing_an_interactive_session_forgets_it_without_claude_rm() {
        let hub = temp_hub("close");
        hub.append(Event::Hook(HookEvent { session_id: "1234abcd-interactive".into(), event: "UserPromptSubmit".into(), ..Default::default() })).unwrap();
        hub.append(Event::QuestionAsked(Question {
            related: vec![],
            id: 1, session_id: "1234abcd-interactive".into(), node_name: None, title: "t".into(), detail: None, options: vec![], links: vec![],
            attachments: vec![], blocking: true, origin: QuestionOrigin::Ask, tool_use_id: None, to_orchestrator: false, node_context: None,
            status: QuestionStatus::Open, answer: None, asked_at: 1, action: false, steps: vec![], mission: None,
        })).unwrap();
        let (status, body) = status_and_body(close_node(State(hub.clone()), Path("1234abcd-interactive".into()), axum::body::Bytes::from_static(b"{}")).await).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["interactive"], true);
        assert!(body["output"].as_str().unwrap().contains("left the terminal session itself open"), "{body}");
        assert!(hub.read(|v| !v.nodes.contains_key("1234abcd-interactive")));
        assert!(hub.read(|v| !v.questions[&1].status.is_open()));
    }

    /// 24 Sep: David keeps the orchestrator alive on purpose; only the fleet's explicit close
    /// (`force`) may close it, never a card's read-and-close.
    #[tokio::test]
    async fn the_orchestrator_is_closed_only_by_an_explicit_force() {
        for (orchestrator, body, closed) in [
            (true, &b"{}"[..], false),
            (true, &b""[..], false),
            (true, &br#"{"force": true}"#[..], true),
            (false, &b"{}"[..], true),
            (false, &br#"{"force": true}"#[..], true),
        ] {
            let hub = temp_hub("close-orch");
            hub.append(Event::Hook(HookEvent { session_id: "orch".into(), event: "UserPromptSubmit".into(), ..Default::default() })).unwrap();
            if orchestrator {
                hub.append(Event::MarkedOrchestrator { session_id: "orch".into() }).unwrap();
            }
            let (status, reply) = status_and_body(close_node(State(hub.clone()), Path("orch".into()), axum::body::Bytes::copy_from_slice(body)).await).await;
            let case = format!("orchestrator={orchestrator} body={}", String::from_utf8_lossy(body));
            if closed {
                assert_eq!(status, StatusCode::OK, "{case}: {reply}");
            } else {
                assert_eq!(status, StatusCode::CONFLICT, "{case}: {reply}");
                assert_eq!(reply["reasons"][0]["kind"], "orchestrator", "{case}: {reply}");
            }
            assert_eq!(hub.read(|v| v.nodes.contains_key("orch")), !closed, "{case}");
        }
    }

    #[tokio::test]
    /// lumo/permiso-camara and hive/servidor-colmena, 24 Sep: messages for interactive sessions sat
    /// in the queue forever, because `wake` only types into background jobs.
    async fn a_message_for_an_interactive_session_arrives_when_its_turn_ends() {
        let hub = temp_hub("send");
        hub.append(Event::Hook(HookEvent { session_id: "interactive".into(), event: "UserPromptSubmit".into(), ..Default::default() })).unwrap();
        hub.append(Event::DaemonSeen(alondra_core::DaemonObservation { session_id: "job".into(), short_id: "job".into(), ..Default::default() })).unwrap();
        let send = |key: &str| send_message(State(hub.clone()), Path(key.to_string()), Json(MessageBody { text: "go on".into(), from: None, attachments: vec![] }));
        let (status, body) = status_and_body(send("interactive").await).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["interactive"], true);
        assert!(body["note"].as_str().unwrap().contains("when its current turn ends"), "{body}");
        let (status, body) = status_and_body(send("job").await).await;
        assert_eq!((status, body.get("interactive")), (StatusCode::OK, None));
        assert_eq!(status_and_body(send("nobody").await).await.0, StatusCode::NOT_FOUND);

        let stop = |sid: &str| hook(State(hub.clone()), Json(json!({"hook_event_name": "Stop", "session_id": sid})));
        let (_, said) = status_and_body(stop("interactive").await).await;
        let context = said["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string();
        assert_eq!(said["hookSpecificOutput"]["hookEventName"], "Stop", "{said}");
        assert!(context.contains("From Ana:\ngo on"), "{said}");
        assert_eq!(hub.read(|v| v.held_for_turn_end("interactive")), 0);
        let (_, again) = status_and_body(stop("interactive").await).await;
        assert!(again.get("hookSpecificOutput").is_none(), "handed once, not on every stop: {again}");
        let (_, job) = status_and_body(stop("job").await).await;
        assert!(job.get("hookSpecificOutput").is_none(), "wake types into background jobs: {job}");
        assert_eq!(hub.read(|v| v.pending_messages("job").count()), 1);
    }

    /// 24 Sep, alondra/maquina-de-estados: David commented on the state-machine artifact in the
    /// browser and had no way to hand those comments to the node from its card.
    #[tokio::test]
    async fn reviewing_an_artifact_hands_the_verdict_to_its_node_and_answers_it_once() {
        let hub = temp_hub("review");
        hub.append(Event::Hook(HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), ..Default::default() })).unwrap();
        let sent = |id: u64, links: Vec<Link>| Event::ReportPosted(Report {
            related: vec![], id, session_id: "s".into(), node_name: None, title: "The plan".into(), body: Some("Read it".into()),
            links, attachments: vec![], kind: ReportKind::Document, created_at: now_millis(), read: false, mission: None, looks_like_question: false,
        });
        hub.append(sent(1, vec![Link { url: "https://claude.ai/artifact/plan".into(), title: None }])).unwrap();
        hub.append(sent(2, vec![])).unwrap();
        hub.append(Event::Hook(HookEvent { session_id: "s".into(), event: "Stop".into(), ..Default::default() })).unwrap();
        assert_eq!(hub.read(|v| v.nodes["s"].state), alondra_core::NodeState::WaitingMerge);

        let review = |id: u64, verdict: &str| review_artifact(State(hub.clone()), Path(id), Json(serde_json::from_value(json!({"verdict": verdict})).unwrap()));
        assert_eq!(review(2, "comments").await.status(), StatusCode::UNPROCESSABLE_ENTITY, "a report with no artifact");
        assert_eq!(review(9, "comments").await.status(), StatusCode::NOT_FOUND);
        assert_eq!(review(1, "comments").await.status(), StatusCode::OK);
        let said = hub.read(|v| v.pending_messages("s").map(|m| (m.from, m.text.clone())).collect::<Vec<_>>());
        assert_eq!(said.len(), 1);
        assert_eq!(said[0].0, MessageFrom::User, "David's own words, so they reach a done node too");
        assert!(said[0].1.contains("ArtifactComments") && said[0].1.contains("https://claude.ai/artifact/plan"), "{}", said[0].1);
        assert!(hub.read(|v| v.reports[&1].read), "the card leaves the inbox until the node sends the artifact again");
        assert_eq!(hub.read(|v| v.nodes["s"].state), alondra_core::NodeState::WaitingMerge, "until the comments reach the node");
        let (status, body) = status_and_body(review(1, "approved").await).await;
        assert_eq!((status, body["kind"].as_str()), (StatusCode::CONFLICT, Some("report_closed")), "one verdict per round");
    }

    #[tokio::test]
    async fn alondra_never_writes_to_a_done_node_but_david_and_the_orchestrator_reopen_it() {
        let hub = temp_hub("done-final");
        for sid in ["s", "o"] {
            hub.append(Event::Hook(HookEvent { session_id: sid.into(), event: "UserPromptSubmit".into(), ..Default::default() })).unwrap();
        }
        hub.append(Event::MarkedOrchestrator { session_id: "o".into() }).unwrap();
        hub.append(Event::PrOpened { url: "https://github.com/a/b/pull/7".into(), session_id: "s".into() }).unwrap();
        hub.append(Event::PrDismissed { id: 1 }).unwrap();
        hub.append(Event::ReportPosted(Report {
            related: vec![], id: 1, session_id: "s".into(), node_name: None, title: "Hecho".into(), body: None,
            links: vec![], attachments: vec![], kind: ReportKind::Final, created_at: now_millis(), read: false, mission: None, looks_like_question: false,
        }))
        .unwrap();
        hub.append(Event::Hook(HookEvent { session_id: "s".into(), event: "Stop".into(), ..Default::default() })).unwrap();
        assert_eq!(hub.read(|v| v.nodes["s"].state), alondra_core::NodeState::Done);

        let pr = hub.read(|v| v.prs[&1].clone());
        assert_eq!(tell_about_pr(&hub, &pr, "[Alondra] Ana merged it.".into(), MessageFrom::Alondra, Some(MessageAbout::PrMerged { pr_id: 1 }), vec![]).unwrap(), "o", "Alondra's notice goes to the orchestrator");
        let send = |from: MessageFrom| send_message(State(hub.clone()), Path("s".into()), Json(MessageBody { text: "go on".into(), from: Some(from), attachments: vec![] }));
        assert_eq!(status_and_body(send(MessageFrom::Alondra).await).await.0, StatusCode::CONFLICT);
        assert_eq!(hub.read(|v| v.pending_messages("s").count()), 0, "nothing of Alondra's is queued for the done node");

        assert_eq!(tell_about_pr(&hub, &pr, "Ana requests changes".into(), MessageFrom::User, None, vec![]).unwrap(), "s", "David's own words reach it");
        assert_eq!(status_and_body(send(MessageFrom::Orchestrator).await).await.0, StatusCode::OK);
        assert_eq!(hub.read(|v| v.pending_messages("s").map(|m| m.from).collect::<Vec<_>>()), [MessageFrom::User, MessageFrom::Orchestrator]);
    }

    #[tokio::test]
    async fn an_attachment_reaches_the_node_as_a_readable_path_after_the_original_is_gone() {
        let hub = temp_hub("attach");
        hub.append(Event::DaemonSeen(alondra_core::DaemonObservation { session_id: "job".into(), short_id: "job".into(), ..Default::default() })).unwrap();
        let original = hub.home.join("Captura 2026-09-24.png");
        std::fs::write(&original, b"pixels").unwrap();
        let bytes = axum::body::Bytes::from(std::fs::read(&original).unwrap());
        let (status, up) = status_and_body(upload(State(hub.clone()), Query(UploadQuery { name: "Captura 2026-09-24.png".into() }), bytes).await).await;
        assert_eq!(status, StatusCode::OK, "{up}");
        assert_eq!((up["kind"].as_str(), up["bytes"].as_u64()), (Some("image"), Some(6)));
        std::fs::remove_file(&original).unwrap();

        let id = up["id"].as_str().unwrap().to_string();
        let body = MessageBody { text: "Mira esto".into(), from: None, attachments: vec![id] };
        let (status, sent) = status_and_body(send_message(State(hub.clone()), Path("job".into()), Json(body)).await).await;
        assert_eq!(status, StatusCode::OK, "{sent}");
        let m = hub.read(|v| v.pending_messages("job").next().cloned()).unwrap();
        let stored = m.attachments[0].stored_path.clone().unwrap();
        assert!(m.text.starts_with("Mira esto\n\n"), "{}", m.text);
        assert!(m.text.contains(&format!("`{stored}` (image, 6 B)")), "{}", m.text);
        assert_eq!(std::fs::read(&stored).unwrap(), b"pixels");
        assert!(hub.read(|v| v.stored_files()).contains(&PathBuf::from(&stored)));

        let unknown = MessageBody { text: "otra".into(), from: None, attachments: vec!["ffffffffffffffff".into()] };
        let (status, _) = status_and_body(send_message(State(hub.clone()), Path("job".into()), Json(unknown)).await).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "a send whose attachment is gone is refused, not sent without it");
    }

    /// The reports the gate refused on 23-24 Sep, from ~/.alondra/rejected-reports.jsonl.
    #[test]
    fn final_reports_and_the_orchestrator_skip_the_report_gate() {
        let cases = [
            ("BlurCamera: tachado de texto delicado en main y probado; nodo listo para cerrar, fase 2 pasada al orquestador", Some(ReportKind::Final), false, false),
            ("Terminado: no queda nada pendiente mío; tus mensajes del iPhone llegaron, pero tarde", Some(ReportKind::Final), false, false),
            ("Quill no ha recibido tu petición de cambios del PR #88: Alondra cree que tiene un menú abierto y no lo tiene. Arreglo en el PR #33", Some(ReportKind::Notice), false, true),
            ("Las respuestas de Quill no están verificadas todavía", Some(ReportKind::Notice), true, false),
            ("Sigo con la fase 3", Some(ReportKind::Progress), false, false),
            ("Un aviso sin tipo", None, false, true),
        ];
        for (what, kind, orchestrator, expected) in cases {
            assert_eq!(gated(kind, orchestrator), expected, "{what}");
        }
    }

    #[test]
    fn reports_stored_before_the_flag_replay_unflagged() {
        let old = r#"{"id":1,"session_id":"s","node_name":null,"title":"t","kind":"notice","created_at":1,"read":false}"#;
        assert!(!serde_json::from_str::<Report>(old).unwrap().looks_like_question);
        let flagged = Report { looks_like_question: true, ..serde_json::from_str::<Report>(old).unwrap() };
        let back: Report = serde_json::from_str(&serde_json::to_string(&flagged).unwrap()).unwrap();
        assert!(back.looks_like_question);
    }

    #[test]
    fn over_pace_ok_is_taken_as_a_bool_or_as_the_string_an_mcp_client_sends() {
        let body = |extra: &str| serde_json::from_str::<LaunchBody>(&format!(r#"{{"name":"n","cwd":"/tmp","prompt":"p"{extra}}}"#)).map(|b| b.over_pace_ok);
        let cases = [
            ("absent", "", Some(false)),
            ("bool true", r#","over_pace_ok":true"#, Some(true)),
            ("bool false", r#","over_pace_ok":false"#, Some(false)),
            ("string true", r#","over_pace_ok":"true""#, Some(true)),
            ("string False", r#","over_pace_ok":"False""#, Some(false)),
            ("null", r#","over_pace_ok":null"#, Some(false)),
            ("anything else", r#","over_pace_ok":"yes please""#, None),
        ];
        for (what, extra, expected) in cases {
            assert_eq!(body(extra).ok(), expected, "{what}");
        }
    }

    #[test]
    fn a_node_withdraws_only_its_own_open_questions() {
        let mut open = alondra_core::model::Question {
            related: vec![],
            id: 7,
            session_id: "mine".into(),
            node_name: None,
            title: "t".into(),
            detail: None,
            options: vec![],
            links: vec![],
            attachments: vec![],
            blocking: true,
            origin: QuestionOrigin::Ask,
            tool_use_id: None,
            to_orchestrator: false,
            node_context: None,
            status: QuestionStatus::Open,
            answer: None,
            asked_at: 0,
            action: false,
            steps: vec![],
            mission: None,
        };
        let cases = [
            ("its own", Some(Some("mine")), None),
            ("the user from a client", None, None),
            ("another node's", Some(Some("other")), Some(StatusCode::FORBIDDEN)),
            ("a caller Alondra cannot place", Some(None), Some(StatusCode::FORBIDDEN)),
        ];
        for (what, asker, refused) in cases {
            assert_eq!(withdraw_refusal(7, Some(&open), asker).map(|r| r.0), refused, "{what}");
        }
        assert_eq!(withdraw_refusal(7, None, None).map(|r| r.0), Some(StatusCode::NOT_FOUND));
        open.status = QuestionStatus::Answered;
        assert_eq!(withdraw_refusal(7, Some(&open), Some(Some("mine"))).map(|r| r.0), Some(StatusCode::CONFLICT));
    }

    #[test]
    fn free_text_is_not_mistaken_for_an_option() {
        let a = answered("Pedir cambios en el paso 2", None);
        assert_eq!(a.text, "Pedir cambios en el paso 2");
        assert_eq!(decision(QuestionOrigin::Plan, &a)["hookSpecificOutput"]["permissionDecision"], "deny");
    }

    /// `close_node`'s only real risks: a node still busy or mid a live permission hold, or
    /// uncommitted/unpushed work. Its own open questions never appear here — closing withdraws
    /// them instead of refusing over them (91933d17, 23 Sep).
    #[test]
    fn close_only_refuses_for_real_risks() {
        let cases: &[(&str, bool, bool, Option<&str>, &[&str])] = &[
            ("only its own open question: nothing blocks", false, false, None, &[]),
            ("busy wins even with other risks present", true, true, Some("x"), &["busy"]),
            ("a live permission hold blocks", false, true, None, &["permission"]),
            ("unpushed work blocks", false, false, Some("2 commit(s) not pushed in /tmp/x"), &["worktree"]),
            ("permission and worktree both block", false, true, Some("x"), &["permission", "worktree"]),
        ];
        for (what, busy, waiting_permission, leftovers, expected_kinds) in cases {
            let reasons = close_reasons(*busy, *waiting_permission, leftovers.map(str::to_string));
            let kinds: Vec<&str> = reasons.iter().map(|r| r.kind).collect();
            assert_eq!(kinds, *expected_kinds, "{what}");
        }
    }

    #[test]
    fn worktree_leftovers_are_carried_as_the_reasons_detail() {
        let reasons = close_reasons(false, false, Some("2 commit(s) not pushed in /tmp/x".into()));
        assert_eq!(reasons, vec![CloseReason { kind: "worktree", detail: Some("2 commit(s) not pushed in /tmp/x".into()) }]);
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

#[cfg(test)]
mod broken_worktree_tests {
    use super::*;

    fn sh(dir: &std::path::Path, cmd: &str) {
        let ok = std::process::Command::new("sh").arg("-c").arg(cmd).current_dir(dir).status().unwrap().success();
        assert!(ok, "{cmd}");
    }

    fn broken_worktree(tag: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("alondra-wt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        sh(&root, "git init -q --bare origin.git && git init -q repo && cd repo && git commit -q --allow-empty -m a && git remote add origin ../origin.git");
        sh(&root.join("repo"), "git worktree add -q -b wt ../wt && cd ../wt && echo x > f && git add f && git commit -q -m f && git push -q origin wt");
        sh(&root.join("repo"), "rm -rf .git/worktrees/wt");
        (root.clone(), root.join("wt"))
    }

    #[test]
    fn two_closes_at_once_never_share_an_index() {
        let names: std::collections::HashSet<PathBuf> = (0..100).map(|_| scratch_index()).collect();
        assert_eq!(names.len(), 100);
    }

    #[tokio::test]
    async fn an_identical_pushed_broken_worktree_is_deleted() {
        let (root, wt) = broken_worktree("same");
        discard_if_identical(&wt, "wt").await.unwrap();
        assert!(!wt.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_broken_worktree_with_changes_is_kept() {
        let (root, wt) = broken_worktree("changed");
        std::fs::write(wt.join("f"), "changed").unwrap();
        assert!(discard_if_identical(&wt, "wt").await.is_err());
        std::fs::write(wt.join("f"), "x\n").unwrap();
        std::fs::write(wt.join("new"), "n").unwrap();
        assert!(discard_if_identical(&wt, "wt").await.is_err());
        assert!(wt.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod new_work_tests {
    use super::*;
    use super::tests::{status_and_body, temp_hub};

    const OPTIONS: &str = r#"
[defaults]
effort = "light"

[[option]]
id = "how_far"
label = "How far"
kind = "choice"
values = [
  { id = "merge", label = "merge" },
  { id = "install", label = "merge and install", mission = "After the merge, run the install target." },
]

[projects.App]
defaults = { pr = "auto_merge" }
"#;

    fn hub_with_options(tag: &str, options: Option<&str>) -> Arc<Hub> {
        let hub = temp_hub(tag);
        if let Some(text) = options {
            std::fs::write(alondra_core::options::path_in(&hub.home), text).unwrap();
        }
        hub.append(Event::DaemonSeen(alondra_core::DaemonObservation { session_id: "orch".into(), short_id: "orch".into(), name: Some("orchestrator".into()), ..Default::default() })).unwrap();
        hub.append(Event::MarkedOrchestrator { session_id: "orch".into() }).unwrap();
        hub
    }

    fn request(text: &str, project: Option<&str>, options: Value) -> Json<RequestBody> {
        Json(serde_json::from_value(json!({"text": text, "project": project, "options": options})).unwrap())
    }

    fn launch_body(request_id: Option<&str>, cwd: &str, effort: Option<&str>) -> LaunchBody {
        serde_json::from_value(json!({"name": "app/fix", "cwd": cwd, "prompt": "Fix it.", "request_id": request_id, "effort": effort})).unwrap()
    }

    #[tokio::test]
    async fn a_request_carries_its_options_to_the_orchestrator_and_keeps_them_for_launch() {
        let hub = hub_with_options("req-options", Some(OPTIONS));
        let (status, said) = status_and_body(new_request(State(hub.clone()), request("Fix the crash", Some("App"), json!({"how_far": "install"}))).await).await;
        assert_eq!(status, StatusCode::OK, "{said}");
        let id = said["request_id"].as_str().unwrap().to_string();
        let text = hub.read(|v| v.messages.values().last().unwrap().text.clone());
        assert!(text.starts_with("[Alondra] New request from Ana: Fix the crash"), "{text}");
        assert!(text.contains(&format!("(request {id}): project App · PR handling: auto-merge · Effort: light · How far: merge and install.")), "{text}");
        assert!(text.contains(&format!("Pass request_id \"{id}\" to launch")), "{text}");
        let kept = hub.read(|v| v.requests.get(&id).cloned()).unwrap();
        assert!(kept.auto_merge);
        assert_eq!(kept.flags.model.as_deref(), Some("sonnet"));
    }

    #[tokio::test]
    async fn a_request_is_refused_when_its_options_are_wrong_or_nobody_orchestrates() {
        for (what, options, orchestrator, expected) in [
            ("unknown option", json!({"nope": true}), true, StatusCode::BAD_REQUEST),
            ("bad value", json!({"how_far": "moon"}), true, StatusCode::BAD_REQUEST),
            ("no orchestrator", json!({}), false, StatusCode::CONFLICT),
            ("fine", json!({}), true, StatusCode::OK),
        ] {
            let hub = if orchestrator { hub_with_options("req-refused", Some(OPTIONS)) } else { temp_hub("req-no-orch") };
            let (status, said) = status_and_body(new_request(State(hub.clone()), request("x", None, options)).await).await;
            assert_eq!(status, expected, "{what}: {said}");
            assert_eq!(hub.read(|v| v.requests.len()), usize::from(expected == StatusCode::OK), "{what}: a refused request leaves nothing behind");
        }
    }

    #[tokio::test]
    async fn only_a_user_request_lets_a_node_auto_merge() {
        let hub = hub_with_options("launch-guard", Some(OPTIONS));
        let (_, said) = status_and_body(new_request(State(hub.clone()), request("x", Some("App"), json!({}))).await).await;
        let id = said["request_id"].as_str().unwrap().to_string();
        let in_app = crate::options::user_home().join("Projects").join("App").to_string_lossy().to_string();
        for (what, request_id, auto_merge) in [
            ("the user's request", Some(id.as_str()), true),
            ("the orchestrator alone, in a project whose default is auto-merge", None, false),
        ] {
            let resolved = launch_options(&hub, &launch_body(request_id, &in_app, None)).unwrap_or_else(|_| panic!("{what}"));
            assert_eq!(resolved.auto_merge, auto_merge, "{what}");
        }
        let (status, said) = status_and_body(launch_options(&hub, &launch_body(Some("r-0"), &in_app, None)).unwrap_err()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{said}");
        let alone = launch_options(&hub, &launch_body(None, "/tmp", Some("hard"))).unwrap();
        assert_eq!((alone.effort.as_str(), alone.project.as_deref()), ("hard", None));
    }

    #[test]
    fn the_launch_command_carries_the_flags_and_the_options_section() {
        let file = alondra_core::options::OptionsFile::parse(OPTIONS).unwrap();
        let resolved = file.resolve(Some("App"), &[("how_far".to_string(), alondra_core::options::Choice::Value("install".into()))].into()).unwrap();
        let args = crate::options::launch_args("app/fix", None, None, "Fix it.\n", &resolved, Some("r-1"));
        assert_eq!(args[..9], ["--bg", "--name", "app/fix", "--permission-mode", "auto", "--model", "sonnet", "--effort", "medium"]);
        assert_eq!(args[9], "--");
        let prompt = &args[10];
        assert!(prompt.starts_with("Fix it.\n\n## Launch options\n\nThe user chose these for this work (request r-1):"), "{prompt}");
        assert!(prompt.contains("- How far: merge and install. After the merge, run the install target."), "{prompt}");
        let explicit = crate::options::launch_args("n", Some("plan"), Some("haiku"), "p", &resolved, None);
        assert_eq!(explicit[3..9], ["--permission-mode", "plan", "--model", "haiku", "--effort", "medium"]);
    }

    #[test]
    fn a_node_launched_with_auto_merge_raises_no_pr_card() {
        for (auto_merge, cards) in [(true, 0), (false, 1)] {
            let hub = temp_hub("auto-merge-card");
            hub.append(Event::Launched { name: "app/fix".into(), request_id: Some("r-1".into()), auto_merge }).unwrap();
            hub.append(Event::DaemonSeen(alondra_core::DaemonObservation { session_id: "s".into(), short_id: "s".into(), name: Some("app/fix".into()), ..Default::default() })).unwrap();
            hub.append(Event::PrOpened { url: "https://github.com/a/app/pull/3".into(), session_id: "s".into() }).unwrap();
            assert_eq!(hub.read(|v| crate::prs::in_inbox(v).count()), cards, "auto_merge {auto_merge}");
            assert_eq!(hub.read(|v| v.prs.len()), 1, "auto_merge {auto_merge}: the PR is still tracked");
            let mut fetched = hub.read(|v| v.prs[&1].clone());
            fetched.auto_merge = false;
            fetched.fetched = true;
            hub.append(Event::PrUpdated(fetched)).unwrap();
            assert_eq!(hub.read(|v| crate::prs::in_inbox(v).count()), cards, "auto_merge {auto_merge}: a refresh from GitHub keeps it");
        }
    }

    #[tokio::test]
    async fn a_broken_options_file_keeps_the_last_good_one_and_says_why() {
        let hub = hub_with_options("options-broken", Some(OPTIONS));
        let ids = |v: &Value| v["options"].as_array().unwrap().iter().map(|o| o["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        let Json(good) = options_catalog(State(hub.clone()), Query(OptionsQuery { project: Some("App".into()) })).await;
        assert_eq!(ids(&good), ["pr", "effort", "how_far"]);
        assert_eq!(good["options"][0]["default"], "auto_merge");
        assert!(good["error"].is_null());
        std::fs::write(alondra_core::options::path_in(&hub.home), "[[option]]\nid = 1\n").unwrap();
        let Json(broken) = options_catalog(State(hub.clone()), Query(OptionsQuery { project: None })).await;
        assert_eq!(ids(&broken), ["pr", "effort", "how_far"]);
        assert!(broken["error"].as_str().unwrap().contains("options.toml"), "{broken}");
        let fresh = hub_with_options("options-none", None);
        let Json(none) = options_catalog(State(fresh), Query(OptionsQuery { project: None })).await;
        assert_eq!(ids(&none), ["pr", "effort"], "with no file only the built-ins");
    }
}
