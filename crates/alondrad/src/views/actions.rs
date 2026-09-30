//! Stateless action identifiers and the daemon's domain action adapter.
//!
//! Action IDs deliberately carry the subject revision. The renderer keeps no action table;
//! every submission is checked against the current domain snapshot and then committed through
//! the same command path as the resource API.

use alondra_core::domain_v2::{
    Actor, AuthorizationId, Command, CommandEnvelope, CommandId, DataEpoch, DomainError,
    InteractionId, InteractionPayload, MergeMethod, Revision, TaskId,
};
use alondra_core::views::{action_id::{ActionId, Verb}, Lang, VIEWS_PROTOCOL};
use axum::{extract::{rejection::JsonRejection, State}, http::StatusCode, response::{IntoResponse, Response}, Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActionRequest {
    pub protocol: u32,
    pub command_id: String,
    pub data_epoch: String,
    pub client: String,
    pub lang: String,
    pub action_id: String,
    #[serde(default)]
    pub option_id: Option<String>,
    #[serde(default)]
    pub input: ActionInput,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActionInput {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub attachments: Vec<String>,
    #[serde(default)]
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub fields: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Comment {
    pub anchor: String,
    pub text: String,
    #[serde(default)]
    pub quote: Option<String>,
}

/// Whether an encrypted relay write is the exact protocol route handled here.
pub(crate) fn approved_path(method: &str, path: &str) -> bool {
    method == "POST" && path == "/actions"
}

/// Local paired-device route. The relay calls `execute` after its own receipt and approval
/// checks, preserving the same domain command and response contract.
pub(crate) async fn local(
    State(state): State<std::sync::Arc<crate::domain_bootstrap::V2Only>>,
    signed: Option<Extension<crate::auth::Signed>>,
    body: Result<Json<ActionRequest>, JsonRejection>,
) -> Response {
    let home = crate::domain_api::DomainHost::home(&*state).to_owned();
    let Some(device) = crate::domain_bootstrap::signed_device(&home, &signed) else {
        return error("forbidden", "signed user device required");
    };
    let Ok(Json(request)) = body else { return error("invalid_input", "invalid action request"); };
    let domain = state.domain();
    let client = state.client();
    execute(&domain, &client, &home, &device.device_id, request, None, || {
        crate::domain_bootstrap::signed_device(&home, &signed)
            .map(|_| ())
            .ok_or_else(|| anyhow::anyhow!("device approval was revoked"))
    }).await
}

pub(crate) async fn relay(
    client: &crate::v2_client_routes::Client,
    sender: &str,
    body: &Value,
    identity: (&str, &str, &[u8; 32]),
    authorize: impl Fn() -> anyhow::Result<()> + Sync,
) -> anyhow::Result<Value> {
    let request: ActionRequest = match serde_json::from_value(body.clone()) {
        Ok(request) => request,
        Err(_) => return Ok(json!({"status":422,"body":DomainError::new("invalid_input", "invalid action request")})),
    };
    if sender != identity.0 {
        return Ok(json!({"status":403,"body":DomainError::new("forbidden", "invalid encrypted sender identity")}));
    }
    // Relay request receipts already deduplicate the transport command. Domain receipts still
    // bind the submitted command_id to the semantic action and actor.
    let response = execute_action(&client.domain, client, client.home(), sender, request, Some(identity), authorize).await?;
    Ok(json!({"status":200,"body":response}))
}

async fn execute(
    domain: &crate::domain_runtime::DomainRuntime,
    client: &crate::v2_client_routes::Client,
    home: &std::path::Path,
    device: &str,
    request: ActionRequest,
    identity: Option<(&str, &str, &[u8; 32])>,
    authorize: impl Fn() -> anyhow::Result<()> + Sync,
) -> Response {
    // Domain commands have durable receipts. The remaining loopback operations use the
    // short-lived per-device receipt table promised by the views protocol. Holding this
    // async lock through dispatch also coalesces simultaneous retries of one command.
    let external = ActionId::parse(&request.action_id).is_ok_and(|action| is_external(action.verb));
    let receipt_key = (device.to_owned(), request.command_id.clone());
    let body = serde_json::to_vec(&request).unwrap_or_default();
    let mut receipts = if identity.is_none() && external {
        Some(action_receipts().lock().await)
    } else { None };
    if let Some(receipts) = receipts.as_mut() {
        receipts.retain(|_, entry| entry.at.elapsed() < std::time::Duration::from_secs(600));
        if let Some(entry) = receipts.get(&receipt_key) {
            if entry.body != body { return error("idempotency_conflict", "command_id was reused with different input"); }
            return (StatusCode::OK, Json(entry.reply.clone())).into_response();
        }
    }
    match execute_action(domain, client, home, device, request, identity, authorize).await {
        Ok(reply) => {
            if let Some(receipts) = receipts.as_mut() {
                if receipts.len() >= 1024 {
                    if let Some(oldest) = receipts.iter().min_by_key(|(_, entry)| entry.at).map(|(key, _)| key.clone()) {
                        receipts.remove(&oldest);
                    }
                }
                receipts.insert(receipt_key, LocalActionReceipt { body, reply: reply.clone(), at: std::time::Instant::now() });
            }
            (StatusCode::OK, Json(reply)).into_response()
        },
        Err(_) => error("storage_unavailable", "action could not be completed"),
    }
}

struct LocalActionReceipt {
    body: Vec<u8>,
    reply: Value,
    at: std::time::Instant,
}

fn action_receipts() -> &'static tokio::sync::Mutex<std::collections::HashMap<(String, String), LocalActionReceipt>> {
    static RECEIPTS: std::sync::OnceLock<tokio::sync::Mutex<std::collections::HashMap<(String, String), LocalActionReceipt>>> = std::sync::OnceLock::new();
    RECEIPTS.get_or_init(|| tokio::sync::Mutex::new(std::collections::HashMap::new()))
}

fn is_external(verb: Verb) -> bool {
    matches!(verb, Verb::Ctx | Verb::Reply | Verb::AskPr | Verb::Conflicts | Verb::Msg | Verb::Close |
        Verb::OrchRetry | Verb::OrchAck | Verb::OrchStop | Verb::SchedRun | Verb::SchedToggle |
        Verb::SchedDelete | Verb::SchedSave | Verb::NewRequest)
}

async fn execute_action(
    domain: &crate::domain_runtime::DomainRuntime,
    client: &crate::v2_client_routes::Client,
    home: &std::path::Path,
    device: &str,
    request: ActionRequest,
    identity: Option<(&str, &str, &[u8; 32])>,
    authorize: impl Fn() -> anyhow::Result<()> + Sync,
) -> anyhow::Result<Value> {
    let action = match ActionId::parse(&request.action_id) {
        Ok(action) => action,
        Err(_) => return Ok(error_value("invalid_input", "invalid action_id")),
    };
    if request.protocol != VIEWS_PROTOCOL {
        return Ok(error_value("incompatible_protocol", "Update Alondra"));
    }
    let lang = match request.lang.as_str() { "en" => Lang::En, "es" => Lang::Es, _ => return Ok(error_value("invalid_input", "invalid language")) };
    if !matches!(request.client.as_str(), "ios" | "mac" | "tui") || !is_uuid_v4(&request.command_id) {
        return Ok(error_value("invalid_input", "invalid action request"));
    }
    let attachments = match resolve_attachments(home, &request.input.attachments) {
        Ok(attachments) => attachments,
        Err(_) => return Ok(rejected_field(lang, "attachments", "An attachment is no longer available")),
    };
    let state = domain.state()?;
    if state.data_epoch.0 != request.data_epoch {
        return Ok(no_longer_available(lang, "This item is no longer available"));
    }
    if action.revision > state.revision {
        return Ok(no_longer_available(lang, "This item has changed"));
    }
    if let Some(response) = changed_pr_head(&state, &action, lang) {
        return Ok(response);
    }
    if action.verb == Verb::Changes && request.input.text.as_deref().is_none_or(|text| text.trim().is_empty())
        && request.input.comments.is_empty() {
        return Ok(rejected_field(lang, "text", "This option needs text"));
    }
    if action.verb == Verb::Ans && request.option_id.is_none() {
        return Ok(rejected_field(lang, "option_id", "Choose an option"));
    }
    if is_external(action.verb) {
        execute_external(domain, client, home, device, &request, &attachments, &action, identity, authorize, lang).await
    } else {
        execute_value(domain, device, request, &attachments, identity, authorize)
    }
}

async fn execute_external(
    domain: &crate::domain_runtime::DomainRuntime,
    client: &crate::v2_client_routes::Client,
    home: &std::path::Path,
    device: &str,
    request: &ActionRequest,
    attachments: &[alondra_core::domain_v2::AttachmentRef],
    action: &ActionId,
    identity: Option<(&str, &str, &[u8; 32])>,
    authorize: impl Fn() -> anyhow::Result<()> + Sync,
    lang: Lang,
) -> anyhow::Result<Value> {
    use crate::v2_client_routes::Write;
    use alondra_core::domain_v2::{AgentId, PrId, Scope};

    let state = domain.state()?;
    if authorize().is_err() {
        return Ok(error_value("forbidden", "device approval was revoked"));
    }
    let user_id = format!("device:{device}");
    let transport = identity.map(|(sender, request_id, digest)| (sender, request_id, digest));
    let stamp = match action.verb {
        Verb::Ctx => "Context requested",
        Verb::Reply => "Reply sent",
        Verb::AskPr => "Question sent",
        Verb::Conflicts => "Conflicts requested",
        Verb::Msg => "Message sent",
        Verb::Close => "Closed",
        Verb::OrchRetry => "Retried",
        Verb::OrchAck => "Acknowledged",
        Verb::OrchStop => "Stopped",
        Verb::SchedRun => "Run started",
        Verb::SchedToggle => "Schedule updated",
        Verb::SchedDelete => "Schedule deleted",
        Verb::SchedSave => "Schedule saved",
        Verb::NewRequest => "Request sent",
        _ => return Ok(no_longer_available(lang, "This action is no longer available")),
    };
    let stamp = if lang == Lang::Es { match action.verb {
        Verb::Ctx => "Contexto solicitado", Verb::Reply => "Respuesta enviada",
        Verb::AskPr => "Pregunta enviada", Verb::Conflicts => "Conflictos comunicados",
        Verb::Msg => "Mensaje enviado", Verb::Close => "Cerrado",
        Verb::OrchRetry => "Reintentado", Verb::OrchAck => "Confirmado", Verb::OrchStop => "Detenido",
        Verb::SchedRun => "Ejecución iniciada", Verb::SchedToggle => "Tarea actualizada",
        Verb::SchedDelete => "Tarea eliminada", Verb::SchedSave => "Tarea guardada",
        Verb::NewRequest => "Solicitud enviada", _ => stamp,
    }} else { stamp };
    let to_applied = |status: u16, body: Value| -> Value {
        if (200..300).contains(&status) {
            json!({"protocol":VIEWS_PROTOCOL,"outcome":"applied","stamp":stamp,"revision":state.revision,"detail":body})
        } else if status == 404 || status == 409 {
            no_longer_available(lang, body.get("message").and_then(Value::as_str).unwrap_or("This action is no longer available"))
        } else {
            json!({"protocol":VIEWS_PROTOCOL,"outcome":"rejected","message":body.get("message").and_then(Value::as_str).unwrap_or("Action was rejected"),"code":body.get("code").cloned().unwrap_or(json!("action_rejected"))})
        }
    };
    match action.verb {
        Verb::Ctx | Verb::Reply => {
            let interaction_id = InteractionId(action.subject.clone());
            let Some(interaction) = state.interactions.get(&interaction_id)
                .filter(|interaction| !interaction.withdrawn && interaction.revision == action.revision)
            else {
                return Ok(no_longer_available(lang, "This item is no longer available"));
            };
            let (agent, task_id) = match &interaction.scope {
                Scope::Task { task_id } => {
                    let Some(agent) = state.tasks.get(task_id).and_then(|task| task.agent_id.clone()) else {
                        return Ok(no_longer_available(lang, "This item has no active agent"));
                    };
                    (agent, Some(task_id.0.clone()))
                }
                Scope::Orchestrator { .. } => return Ok(no_longer_available(lang, "This item has no active agent")),
            };
            if !state.agents.contains_key(&agent) { return Ok(no_longer_available(lang, "This agent is no longer available")); }
            let note = request.input.text.as_deref().unwrap_or_default().trim();
            if note.is_empty() { return Ok(error_value("invalid_input", "text is required")); }
            let (kind, text, mark_read) = if action.verb == Verb::Ctx {
                ("more_context", format!("I need more context before deciding. [What is missing: {note}] Explain what is at stake, the options with their consequences and your recommendation, and ask again."), false)
            } else { ("reply", note.to_owned(), true) };
            let body = json!({"text":text,"attachments":attachments,"kind":kind,
                "mark_read":mark_read,"interaction_id":interaction_id,"task_id":task_id});
            let reply = crate::domain_bootstrap::send_agent_message(domain, home, device, &agent, body).await;
            let status = reply["status"].as_u64().unwrap_or(500) as u16;
            Ok(to_applied(status, reply["body"].clone()))
        }
        Verb::AskPr => {
            let pr_id = PrId(action.subject.clone());
            let Some(pr) = state.pull_requests.get(&pr_id).filter(|pr| pr.state == alondra_core::domain_v2::PrState::Open) else {
                return Ok(no_longer_available(lang, "This pull request is no longer open"));
            };
            let Some(task) = state.tasks.get(&pr.task_id) else { return Ok(no_longer_available(lang, "This pull request is no longer available")); };
            let Some(agent) = task.agent_id.as_ref().filter(|agent| state.agents.contains_key(*agent)) else {
                return Ok(no_longer_available(lang, "This pull request has no active agent"));
            };
            let text = request.input.text.as_deref().unwrap_or_default().trim();
            if text.is_empty() { return Ok(error_value("invalid_input", "text is required")); }
            let body = json!({"text":text,"attachments":attachments,"kind":"pr_question",
                "pr_id":pr.id,"task_id":task.id});
            let reply = crate::domain_bootstrap::send_agent_message(domain, home, device, agent, body).await;
            Ok(to_applied(reply["status"].as_u64().unwrap_or(500) as u16, reply["body"].clone()))
        }
        Verb::Conflicts => {
            let pr_id = PrId(action.subject.clone());
            let Some(_pr) = state.pull_requests.get(&pr_id).filter(|pr| pr.state == alondra_core::domain_v2::PrState::Open) else {
                return Ok(no_longer_available(lang, "This pull request is no longer open"));
            };
            let reply = client.write(&user_id, Write::ResolveConflicts(pr_id.0), &json!({}), transport, &authorize).await?;
            Ok(to_applied(reply.status.as_u16(), reply.body))
        }
        Verb::Msg if action.subject == "view" => {
            let text = request.input.text.as_deref().unwrap_or_default().trim();
            if text.is_empty() { return Ok(error_value("invalid_input", "text is required")); }
            let body = json!({"text":text,"attachments":attachments});
            let reply = client.write(&user_id, Write::OrchestratorMessage, &body, transport, &authorize).await?;
            Ok(to_applied(reply.status.as_u16(), reply.body))
        }
        Verb::Msg => {
            let agent = AgentId(action.subject.clone());
            if !state.agents.contains_key(&agent) { return Ok(no_longer_available(lang, "This agent is no longer available")); }
            let text = request.input.text.as_deref().unwrap_or_default().trim();
            if text.is_empty() { return Ok(error_value("invalid_input", "text is required")); }
            let task_id = if let Some(id) = action.extra.as_deref() {
                let task = state.tasks.get(&TaskId(id.into()))
                    .filter(|task| task.agent_id.as_ref() == Some(&agent));
                let Some(task) = task else { return Ok(no_longer_available(lang, "This task is no longer assigned to the node")); };
                Some(task.id.clone())
            } else { None };
            let body = json!({"text":text,"attachments":attachments,"kind":"message","task_id":task_id});
            let reply = crate::domain_bootstrap::send_agent_message(domain, home, device, &agent, body).await;
            Ok(to_applied(reply["status"].as_u64().unwrap_or(500) as u16, reply["body"].clone()))
        }
        Verb::Close => {
            let agent = AgentId(action.subject.clone());
            if !state.agents.contains_key(&agent) { return Ok(no_longer_available(lang, "This agent is no longer available")); }
            let force = request.input.fields.get("force").and_then(Value::as_bool).unwrap_or(false);
            let reply = client.write(&user_id, Write::Close(agent.0), &json!({"force":force}), transport, &authorize).await?;
            Ok(to_applied(reply.status.as_u16(), reply.body))
        }
        Verb::OrchRetry | Verb::OrchAck | Verb::OrchStop => {
            let Some(run_id) = action.extra.as_deref() else { return Ok(no_longer_available(lang, "This run is no longer active")); };
            let runtime = crate::domain_orchestrator::load(home)?;
            let ambiguous = runtime.active_run.is_some() && runtime.last_error.is_some();
            if runtime.active_run.as_ref().is_none_or(|run| run.id != run_id)
                || matches!(action.verb, Verb::OrchRetry | Verb::OrchAck) && !ambiguous
                || action.verb == Verb::OrchStop && ambiguous
            {
                return Ok(no_longer_available(lang, "This run is no longer active"));
            }
            let decision = match action.verb {
                Verb::OrchRetry => crate::domain_orchestrator::Decision::Retry,
                Verb::OrchAck => crate::domain_orchestrator::Decision::Acknowledge,
                _ => crate::domain_orchestrator::Decision::Cancel,
            };
            let reply = crate::domain_bootstrap::apply_orchestrator_decision(client.domain.clone(), home, decision, Some(&json!({"run_id":run_id})));
            Ok(to_applied(reply["status"].as_u64().unwrap_or(500) as u16, reply["body"].clone()))
        }
        Verb::SchedRun | Verb::SchedToggle | Verb::SchedDelete | Verb::SchedSave => {
            schedule_action(domain, home, request, action, identity, &authorize, lang, stamp).await
        }
        Verb::NewRequest => {
            let fields = &request.input.fields;
            let text = fields.get("text").and_then(Value::as_str).unwrap_or_default().trim();
            if text.is_empty() { return Ok(error_value("invalid_input", "text is required")); }
            let mut options = fields.get("options").and_then(Value::as_object).cloned().unwrap_or_default();
            for (key, value) in fields {
                if matches!(key.as_str(), "text" | "attachments" | "project" | "issue" | "options") { continue; }
                if value.is_boolean() || value.is_string() { options.insert(key.clone(), value.clone()); }
            }
            let body = json!({"request_id":request.command_id.clone(),
                "text":text,
                "attachments":attachments,
                "project":fields.get("project").and_then(Value::as_str).filter(|value| !value.is_empty()),
                "issue":fields.get("issue").and_then(Value::as_str).filter(|value| !value.is_empty()),
                "options":options});
            let reply = client.write(&user_id, Write::NewRequest, &body, transport, &authorize).await?;
            Ok(to_applied(reply.status.as_u16(), reply.body))
        }
        _ => Ok(no_longer_available(lang, "This action is no longer available")),
    }
}

async fn schedule_action(
    domain: &crate::domain_runtime::DomainRuntime,
    home: &std::path::Path,
    request: &ActionRequest,
    action: &ActionId,
    identity: Option<(&str, &str, &[u8; 32])>,
    authorize: &(dyn Fn() -> anyhow::Result<()> + Sync),
    lang: Lang,
    stamp: &str,
) -> anyhow::Result<Value> {
    use alondra_core::domain_v2::ScheduleId;
    let state = domain.state()?;
    if action.verb == Verb::SchedSave && action.subject == "view" {
        let body = schedule_draft(&request.input.fields, None);
        let result = crate::domain_schedule_api::mutation(domain, home, "POST", "/schedules", Some(&body), identity, authorize);
        return Ok(schedule_reply(result, lang, stamp, state.revision));
    }
    let Some(raw_id) = action.subject.strip_prefix("s-") else {
        return Ok(no_longer_available(lang, "This schedule is no longer available"));
    };
    let schedule_id = ScheduleId(raw_id.to_owned());
    let Some(schedule) = state.schedule.schedules.get(&schedule_id).filter(|schedule| schedule.deleted_at.is_none()) else {
        return Ok(no_longer_available(lang, "This schedule is no longer available"));
    };
    if action.revision != schedule.revision as u64 {
        return Ok(no_longer_available(lang, "This schedule has changed"));
    }
    let (method, path, body) = match action.verb {
        Verb::SchedRun => ("POST", format!("/schedules/{raw_id}/runs"), None),
        Verb::SchedDelete => ("DELETE", format!("/schedules/{raw_id}"), None),
        Verb::SchedToggle => {
            let mut body = schedule_draft(&serde_json::Map::new(), Some(schedule));
            body["enabled"] = json!(!schedule.enabled);
            ("PUT", format!("/schedules/{raw_id}"), Some(body))
        }
        Verb::SchedSave => ("PUT", format!("/schedules/{raw_id}"), Some(schedule_draft(&request.input.fields, Some(schedule)))),
        _ => return Ok(no_longer_available(lang, "This schedule action is unavailable")),
    };
    let result = crate::domain_schedule_api::mutation(domain, home, method, &path,
        body.as_ref(), identity, authorize);
    Ok(schedule_reply(result, lang, stamp, state.revision))
}

fn schedule_draft(fields: &serde_json::Map<String, Value>, schedule: Option<&alondra_core::domain_v2::Schedule>) -> Value {
    let text = |key: &str, fallback: &str| -> String {
        fields.get(key).and_then(Value::as_str).unwrap_or(fallback).to_owned()
    };
    let time = fields.get("time").and_then(Value::as_str);
    let (hour, minute) = time.and_then(|time| {
        let (hour, minute) = time.split_once(':')?;
        Some((hour.parse::<u8>().ok()?, minute.parse::<u8>().ok()?))
    }).or_else(|| schedule.map(|schedule| (schedule.hour, schedule.minute))).unwrap_or((9, 0));
    let frequency = fields.get("repeat").cloned().unwrap_or_else(|| {
        schedule.map(|schedule| json!(schedule.frequency)).unwrap_or(json!("daily"))
    });
    let project = if let Some(value) = fields.get("project") {
        value.as_str().filter(|value| !value.is_empty()).map(|value| json!(value)).unwrap_or(Value::Null)
    } else {
        schedule.and_then(|schedule| schedule.project.as_ref().map(|value| json!(value))).unwrap_or(Value::Null)
    };
    let disallowed_tools = fields.get("disallowed_tools").and_then(Value::as_str)
        .map(|value| value.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>())
        .map(|lines| json!(lines)).unwrap_or_else(|| schedule.map(|schedule| json!(schedule.disallowed_tools)).unwrap_or(json!([])));
    json!({
        "title":text("name", schedule.map_or("", |schedule| schedule.title.as_str())),
        "text":text("mission", schedule.map_or("", |schedule| schedule.text.as_str())),
        "project":project,
        "options":schedule.map(|schedule| json!(schedule.options)).unwrap_or(json!({})),
        "disallowed_tools":disallowed_tools,
        "frequency":frequency,
        "hour":hour,
        "minute":minute,
        "weekday":fields.get("weekday").cloned().unwrap_or_else(|| schedule.map(|schedule| json!(schedule.weekday)).unwrap_or(Value::Null)),
        "interval":fields.get("interval").cloned().unwrap_or_else(|| schedule.map(|schedule| json!(schedule.interval)).unwrap_or(Value::Null)),
        "start_at":fields.get("start_at").cloned().unwrap_or_else(|| schedule.map(|schedule| json!(schedule.start_at)).unwrap_or(Value::Null)),
        "enabled":fields.get("active").and_then(Value::as_bool).unwrap_or_else(|| schedule.is_none_or(|schedule| schedule.enabled)),
        "revision":schedule.map_or(0, |schedule| schedule.revision),
    })
}

fn schedule_reply(result: Value, lang: Lang, stamp: &str, revision: u64) -> Value {
    let status = result["status"].as_u64().unwrap_or(500) as u16;
    let body = result["body"].clone();
    if (200..300).contains(&status) {
        json!({"protocol":VIEWS_PROTOCOL,"outcome":"applied","stamp":stamp,"revision":revision,"detail":body})
    } else if status == 404 || status == 409 {
        no_longer_available(lang, body.get("message").and_then(Value::as_str).unwrap_or("This schedule changed"))
    } else {
        json!({"protocol":VIEWS_PROTOCOL,"outcome":"rejected","code":body.get("code").cloned().unwrap_or(json!("schedule_rejected")),"message":body.get("message").and_then(Value::as_str).unwrap_or("Schedule action failed")})
    }
}

fn execute_value(
    domain: &crate::domain_runtime::DomainRuntime,
    device: &str,
    request: ActionRequest,
    attachments: &[alondra_core::domain_v2::AttachmentRef],
    identity: Option<(&str, &str, &[u8; 32])>,
    authorize: impl Fn() -> anyhow::Result<()> + Sync,
) -> anyhow::Result<Value> {
    if request.protocol != VIEWS_PROTOCOL {
        return Ok(error_value("incompatible_protocol", "Update Alondra"));
    }
    let lang = match request.lang.as_str() { "en" => Lang::En, "es" => Lang::Es, _ => return Ok(error_value("invalid_input", "invalid language")) };
    if !matches!(request.client.as_str(), "ios" | "mac" | "tui") {
        return Ok(error_value("invalid_input", "invalid client"));
    }
    let action = match ActionId::parse(&request.action_id) {
        Ok(action) => action,
        Err(_) => return Ok(error_value("invalid_input", "invalid action_id")),
    };
    if !is_uuid_v4(&request.command_id) {
        return Ok(error_value("invalid_input", "command_id must be a UUID v4"));
    }
    let command_id = CommandId(request.command_id.clone());
    let state = match domain.state() {
        Ok(state) => state,
        Err(error) => return Err(error),
    };
    if state.data_epoch.0 != request.data_epoch {
        return Ok(no_longer_available(lang, "This item is no longer available"));
    }
    if action.verb == Verb::ArchiveDone {
        return archive_done(domain, device, &request, &action, &state, lang, authorize);
    }
    let Some((command, stamp)) = map_domain_action(&state, &action, &request, attachments, lang) else {
        return Ok(no_longer_available(lang, "This action is no longer available"));
    };
    let envelope = CommandEnvelope {
        protocol_version: 2,
        data_epoch: DataEpoch(request.data_epoch),
        command_id,
        expected_revision: action.revision as Revision,
        command,
    };
    let actor = Actor::User { user_id: format!("device:{device}") };
    let transport = identity.map(|(sender, request_id, digest)| (sender, request_id, digest));
    let outcome = match domain.execute(&actor, &envelope, transport,
        || authorize().map_err(|_| DomainError::new("forbidden", "device approval was revoked").into())) {
        Ok(outcome) => outcome,
        Err(crate::domain_store::StoreError::Domain(error))
            if matches!(error.code.as_str(), "stale_revision" | "not_found" | "invalid_action_for_kind" | "task_not_active") =>
        {
            return Ok(no_longer_available(lang, "This action is no longer available"));
        }
        Err(crate::domain_store::StoreError::Domain(error)) => {
            return Ok(json!({"protocol":VIEWS_PROTOCOL,"outcome":"rejected","message":error.message,"code":error.code}));
        }
        Err(crate::domain_store::StoreError::Storage(error)) => return Err(error.into()),
    };
    let response = json!({"protocol":VIEWS_PROTOCOL,"outcome":"applied","stamp":stamp,"revision":outcome.revision});
    Ok(response)
}

fn archive_done(
    domain: &crate::domain_runtime::DomainRuntime,
    device: &str,
    request: &ActionRequest,
    action: &ActionId,
    state: &alondra_core::domain_v2::DomainState,
    lang: Lang,
    authorize: impl Fn() -> anyhow::Result<()> + Sync,
) -> anyhow::Result<Value> {
    if action.subject != "view" || action.revision > state.revision {
        return Ok(no_longer_available(lang, "This view has changed"));
    }
    let tasks = archive_targets(state, action.revision);
    let mut revision = state.revision;
    for task in tasks {
        let envelope = CommandEnvelope {
            protocol_version: 2,
            data_epoch: state.data_epoch.clone(),
            command_id: archive_command_id(&request.command_id, &task.id),
            expected_revision: task.revision,
            command: Command::ArchiveTask { task_id: task.id.clone(), archived: true },
        };
        match domain.execute(&Actor::User { user_id: format!("device:{device}") }, &envelope, None,
            || authorize().map_err(|_| DomainError::new("forbidden", "device approval was revoked").into())) {
            Ok(outcome) => revision = outcome.revision,
            Err(crate::domain_store::StoreError::Domain(error))
                if matches!(error.code.as_str(), "stale_revision" | "not_found" | "invalid_action_for_kind") =>
            {
                return Ok(no_longer_available(lang, "A completed task changed; refresh the view"));
            }
            Err(crate::domain_store::StoreError::Domain(error)) => {
                return Ok(json!({"protocol":VIEWS_PROTOCOL,"outcome":"rejected","code":error.code,"message":error.message}));
            }
            Err(crate::domain_store::StoreError::Storage(error)) => return Err(error.into()),
        }
    }
    Ok(json!({"protocol":VIEWS_PROTOCOL,"outcome":"applied","stamp":if lang == Lang::Es {"Archivadas"} else {"Archived"},"revision":revision}))
}

fn archive_targets(
    state: &alondra_core::domain_v2::DomainState,
    action_revision: u64,
) -> Vec<&alondra_core::domain_v2::Task> {
    state.tasks.values()
        .filter(|task| task.state == alondra_core::domain_v2::TaskState::Done
            && !task.archived && task.revision <= action_revision)
        .collect()
}

fn archive_command_id(parent: &str, task: &TaskId) -> CommandId {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(task.0.as_bytes());
    let suffix: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    CommandId(format!("{parent}/{suffix}"))
}

fn map_domain_action(
    state: &alondra_core::domain_v2::DomainState,
    action: &ActionId,
    request: &ActionRequest,
    attachments: &[alondra_core::domain_v2::AttachmentRef],
    lang: Lang,
) -> Option<(Command, String)> {
    let stamp = |en: &str, es: &str| if lang == Lang::Es { es } else { en }.to_owned();
    match action.verb {
        Verb::Ans => {
            let interaction = state.interactions.get(&InteractionId(action.subject.clone()))?;
            let (InteractionPayload::Question { options, answer: None } | InteractionPayload::ActionRequest { options, answer: None }) = &interaction.payload else { return None; };
            let option_id = request.option_id.clone()?;
            let option = options.iter().find(|option| option.id == option_id)?;
            let mut inputs = std::collections::BTreeMap::new();
            if let Some(text) = request.input.text.as_ref() {
                let input = option.inputs.iter().find(|input| input.kind == alondra_core::domain_v2::InputKind::Text)?;
                inputs.insert(input.id.clone(), alondra_core::domain_v2::InputValue::Text(text.clone()));
            }
            Some((Command::AnswerQuestion {
                interaction_id: interaction.id.clone(), option_id,
                inputs, attachments: attachments.to_vec(),
            }, format!("{} · {}", stamp("Answered", "Respondida"), option.label)))
        }
        Verb::Read => {
            let interaction = state.interactions.get(&InteractionId(action.subject.clone()))?;
            Some((Command::MarkRead { interaction_id: interaction.id.clone() }, stamp("Marked as read", "Marcada como leída")))
        }
        Verb::Accept => {
            let interaction = state.interactions.get(&InteractionId(action.subject.clone()))?;
            let InteractionPayload::Delivery { targets, acceptance: None, .. } = &interaction.payload else { return None; };
            Some((Command::AcceptDelivery { presentation_id: interaction.id.clone(), targets: targets.clone() }, stamp("Accepted", "Aceptada")))
        }
        Verb::Changes => {
            let (interaction, pr_head) = if action.subject.starts_with("pr-") {
                let pr = state.pull_requests.get(&alondra_core::domain_v2::PrId(action.subject.clone()))?;
                let head = action.extra.as_deref()?;
                if head != head_prefix(&pr.head) {
                    return None;
                }
                let presentation = state.interactions.values().find(|interaction| {
                    matches!(&interaction.payload, InteractionPayload::Delivery { targets, .. }
                        if targets.iter().any(|target| matches!(target,
                            alondra_core::domain_v2::DeliveryTarget::PullRequest { pr_id, head }
                                if pr_id == &pr.id && head == &pr.head)))
                })?;
                (presentation, Some(pr.head.clone()))
            } else {
                (state.interactions.get(&InteractionId(action.subject.clone()))?, None)
            };
            let InteractionPayload::Delivery { targets, .. } = &interaction.payload else { return None; };
            if let Some(head) = pr_head.as_deref() {
                if !targets.iter().any(|target| matches!(target, alondra_core::domain_v2::DeliveryTarget::PullRequest { head: target_head, .. } if target_head == head)) {
                    return None;
                }
            }
            let comments: Vec<_> = request.input.comments.iter().map(|comment| (comment.anchor.clone(), comment.text.clone())).collect();
            let body = alondra_core::views::review_export::text(&comments, request.input.text.as_deref().unwrap_or_default());
            if body.trim().is_empty() && request.input.comments.is_empty() { return None; }
            Some((Command::SubmitFeedback { presentation_id: interaction.id.clone(), targets: targets.clone(), body,
                attachments: attachments.to_vec() }, stamp("Changes requested", "Cambios solicitados")))
        }
        Verb::Merge => {
            let (interaction, target_pr) = if action.subject.starts_with("pr-") {
                let (head, presentation_id) = action.extra.as_deref()?.split_once('-')?;
                let pr = state.pull_requests.get(&alondra_core::domain_v2::PrId(action.subject.clone()))?;
                if head != head_prefix(&pr.head) { return None; }
                let presentation = state.interactions.get(&InteractionId(presentation_id.to_owned()))?;
                (presentation, Some(pr.id.clone()))
            } else {
                (state.interactions.get(&InteractionId(action.subject.clone()))?, None)
            };
            let InteractionPayload::Delivery { targets, .. } = &interaction.payload else { return None; };
            let (pr_id, head) = targets.iter().find_map(|target| match target {
                alondra_core::domain_v2::DeliveryTarget::PullRequest { pr_id, head } => Some((pr_id, head)),
                _ => None,
            }).filter(|(pr_id, _)| target_pr.as_ref().is_none_or(|expected| expected == *pr_id))?;
            let (prefix, expected_presentation) = action.extra.as_deref()?.split_once('-')?;
            if expected_presentation != interaction.id.0 || prefix != head_prefix(head) { return None; }
            let pr = state.pull_requests.get(pr_id)?;
            if pr.head != *head || pr.state != alondra_core::domain_v2::PrState::Open { return None; }
            let method = match request.option_id.as_deref()? {
                "merge" => MergeMethod::Merge,
                "squash" => MergeMethod::Squash,
                "rebase" => MergeMethod::Rebase,
                _ => return None,
            };
            if !pr.allowed_methods.contains(&method) { return None; }
            Some((Command::AuthorizeMerge {
                authorization_id: AuthorizationId(format!("auth-{}", request.command_id)),
                presentation_id: interaction.id.clone(), pr_id: pr_id.clone(),
                expected_head: head.clone(), method,
            }, stamp("Merge authorized", "Integración autorizada")))
        }
        Verb::Withdraw => {
            if !state.requests.get(&alondra_core::domain_v2::RequestId(action.subject.clone()))?.closed {
                Some((Command::CloseRequest { request_id: action.subject.clone().into(), answer: None, withdrawn: true, title: None }, stamp("Withdrawn", "Retirada")))
            } else { None }
        }
        Verb::Cancel => {
            let task = state.tasks.get(&TaskId(action.subject.clone()))?;
            Some((Command::CancelTask { task_id: task.id.clone() }, stamp("Cancelled", "Cancelada")))
        }
        Verb::Reopen => {
            let task = state.tasks.get(&TaskId(action.subject.clone()))?;
            Some((Command::ReopenTask { task_id: task.id.clone() }, stamp("Reopened", "Reabierta")))
        }
        Verb::Archive | Verb::Unarchive => {
            let task = state.tasks.get(&TaskId(action.subject.clone()))?;
            Some((Command::ArchiveTask { task_id: task.id.clone(), archived: action.verb == Verb::Archive }, stamp("Updated", "Actualizada")))
        }
        _ => None,
    }
}

fn changed_pr_head(
    state: &alondra_core::domain_v2::DomainState,
    action: &ActionId,
    lang: Lang,
) -> Option<Value> {
    let (pr_id, presentation_id, old_prefix) = match action.verb {
        Verb::Changes if action.subject.starts_with("pr-") => (
            alondra_core::domain_v2::PrId(action.subject.clone()),
            None,
            action.extra.as_deref()?,
        ),
        Verb::Merge => {
            let (prefix, presentation_id) = action.extra.as_deref()?.split_once('-')?;
            let pr_id = if action.subject.starts_with("pr-") {
                alondra_core::domain_v2::PrId(action.subject.clone())
            } else {
                let interaction = state.interactions.get(&InteractionId(action.subject.clone()))?;
                let InteractionPayload::Delivery { targets, .. } = &interaction.payload else { return None; };
                targets.iter().find_map(|target| match target {
                    alondra_core::domain_v2::DeliveryTarget::PullRequest { pr_id, .. } => Some(pr_id.clone()),
                    _ => None,
                })?
            };
            (pr_id, Some(presentation_id), prefix)
        }
        _ => return None,
    };
    let pr = state.pull_requests.get(&pr_id)?;
    let new_prefix = head_prefix(&pr.head);
    if old_prefix == new_prefix { return None; }
    let (verb, subject, revision, extra) = match action.verb {
        Verb::Changes => (Verb::Changes, pr_id.0.clone(), state.revision, new_prefix.to_owned()),
        Verb::Merge => {
            let presentation = presentation_id?;
            let id = if action.subject.starts_with("pr-") { presentation } else { action.subject.as_str() };
            let revision = state.interactions.get(&InteractionId(id.to_owned())).map_or(state.revision, |interaction| interaction.revision);
            (Verb::Merge, action.subject.clone(), revision, format!("{new_prefix}-{presentation}"))
        }
        _ => return None,
    };
    let fresh = ActionId::new(verb, subject, revision, Some(extra)).ok()?.to_string();
    Some(json!({
        "protocol":VIEWS_PROTOCOL,
        "outcome":"changed",
        "message":if lang == Lang::Es {"La PR tiene commits nuevos desde tu revisión"} else {"The PR has new commits since your review"},
        "confirm":{"action_id":fresh,"button":if lang == Lang::Es {"Enviar igualmente"} else {"Send anyway"}}
    }))
}

fn no_longer_available(lang: Lang, message: &str) -> Value {
    json!({"protocol":VIEWS_PROTOCOL,"outcome":"no_longer_available","message": if lang == Lang::Es { "Ya no está disponible" } else { message }})
}

fn resolve_attachments(
    home: &std::path::Path,
    ids: &[String],
) -> anyhow::Result<Vec<alondra_core::domain_v2::AttachmentRef>> {
    let plain_ids: Vec<String> = ids.iter()
        .map(|id| id.strip_prefix("att:").unwrap_or(id).to_owned())
        .collect();
    let files = crate::uploads::resolve(home, &plain_ids)
        .map_err(|error| anyhow::anyhow!(error))?;
    plain_ids.into_iter().zip(files).map(|(id, file)| {
        let path = file.stored_path.ok_or_else(|| anyhow::anyhow!("missing stored attachment"))?;
        crate::service_files::attachment_ref(home, id, file.name, std::path::Path::new(&path))
    }).collect()
}

fn error_value(code: &str, message: &str) -> Value {
    json!({"protocol":VIEWS_PROTOCOL,"outcome":"rejected","code":code,"message":message})
}

fn rejected_field(lang: Lang, field: &str, message: &str) -> Value {
    json!({"protocol":VIEWS_PROTOCOL,"outcome":"rejected","field":field,"message":if lang == Lang::Es { "Esta opción necesita texto" } else { message }})
}

fn error(code: &str, message: &str) -> Response {
    crate::domain_api::domain_error(DomainError::new(code, message))
}

fn is_uuid_v4(value: &str) -> bool {
    value.len() == 36
        && [8, 13, 18, 23]
            .into_iter()
            .all(|index| value.as_bytes()[index] == b'-')
        && value.bytes().enumerate().all(|(index, byte)| {
            [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit()
        })
        && value.as_bytes()[14] == b'4'
        && matches!(
            value.as_bytes()[19].to_ascii_lowercase(),
            b'8' | b'9' | b'a' | b'b'
        )
}

fn head_prefix(head: &str) -> &str {
    head.get(..8).unwrap_or(head)
}

#[cfg(test)]
mod tests {
    use super::*;

    use alondra_core::domain_v2::{RequestId, Task, TaskState};

    fn completed_task(id: &str, revision: u64) -> Task {
        Task {
            id: TaskId(id.into()), request_id: RequestId("request".into()), agent_id: None,
            title: id.into(), state: TaskState::Done, revision, started: true,
            review_prs: false, final_presentation: None, pr_id: None,
            dependencies: Default::default(), stack: None, documents: Default::default(),
            archived: false,
        }
    }

    #[test]
    fn archive_batch_retry_keeps_child_identity_and_original_targets() {
        let mut state = alondra_core::domain_v2::DomainState::new("epoch");
        state.tasks.insert(TaskId("task-a".into()), completed_task("task-a", 5));
        state.tasks.insert(TaskId("task-b".into()), completed_task("task-b", 6));
        let original: Vec<_> = archive_targets(&state, 6).into_iter()
            .map(|task| (task.id.clone(), archive_command_id("request-uuid", &task.id)))
            .collect();
        assert_eq!(original.len(), 2);

        // Simulate a stopped batch after its first durable child commit. A new
        // completion must not be pulled into the retry of the old action.
        state.tasks.get_mut(&original[0].0).unwrap().archived = true;
        state.tasks.insert(TaskId("task-c".into()), completed_task("task-c", 7));
        let remaining: Vec<_> = archive_targets(&state, 6).into_iter()
            .map(|task| (task.id.clone(), archive_command_id("request-uuid", &task.id)))
            .collect();
        assert_eq!(remaining, vec![original[1].clone()]);
        assert_ne!(original[0].1, original[1].1);
        assert!(original[0].1.0.len() <= 256);
    }

    #[test]
    fn accepts_only_uuid_v4_command_ids() {
        assert!(is_uuid_v4("5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10"));
        assert!(!is_uuid_v4("5f0c2d9e-8d0b-1c71-9b0e-2b6f5d8f1a10"));
        assert!(!is_uuid_v4("5f0c2d9e-8d0b-4c71-7b0e-2b6f5d8f1a10"));
        assert!(!is_uuid_v4("not-a-uuid"));
    }

    #[test]
    fn action_route_is_an_exact_post() {
        assert!(approved_path("POST", "/actions"));
        assert!(!approved_path("GET", "/actions"));
        assert!(!approved_path("POST", "/actions?protocol=1"));
        assert!(!approved_path("POST", "/actions/"));
    }
}
