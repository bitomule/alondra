//! Stateless action identifiers and the daemon's domain action adapter.
//!
//! Action IDs deliberately carry the subject revision. The renderer keeps no action table;
//! every submission is checked against the current domain snapshot and then committed through
//! the same command path as the resource API.

use alondra_core::domain_v2::{
    Actor, AuthorizationId, Command, CommandEnvelope, CommandId, DataEpoch, DomainError, Hold, Instruction,
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
    let spanish = request.lang == "es";
    let receipt_key = (device.to_owned(), request.command_id.clone());
    let body = serde_json::to_vec(&request).unwrap_or_default();
    let mut receipts = if identity.is_none() && external {
        Some(action_receipts().lock().await)
    } else { None };
    if let Some(receipts) = receipts.as_mut() {
        receipts.retain(|_, entry| entry.at.elapsed() < std::time::Duration::from_secs(600));
        if let Some(entry) = receipts.get(&receipt_key) {
            if entry.body != body { return error("idempotency_conflict", if spanish {
                "El identificador del comando ya se usó con otra acción"
            } else { "command_id was reused with different input" }); }
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
        Err(_) => error("storage_unavailable", if spanish {
            "No se pudo completar la acción"
        } else { "action could not be completed" }),
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
    matches!(verb, Verb::Reply | Verb::AskPr | Verb::Conflicts | Verb::Msg | Verb::Relaunch | Verb::Close | Verb::Interrupt |
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
    let lang = match request.lang.as_str() { "en" => Lang::En, "es" => Lang::Es, _ => return Ok(error_value("invalid_input", "invalid language")) };
    let action = match ActionId::parse(&request.action_id) {
        Ok(action) => action,
        Err(_) => return Ok(error_value_lang(lang, "invalid_input", "invalid action_id")),
    };
    if request.protocol != VIEWS_PROTOCOL {
        return Ok(error_value_lang(lang, "incompatible_protocol", "Update Alondra"));
    }
    if !matches!(request.client.as_str(), "ios" | "mac" | "tui") || !is_uuid_v4(&request.command_id) {
        return Ok(error_value_lang(lang, "invalid_input", "invalid action request"));
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
    if matches!(action.verb, Verb::Changes | Verb::AskPr | Verb::Reply)
        && request.input.text.as_deref().is_none_or(|text| text.trim().is_empty())
        && request.input.comments.is_empty() {
        return Ok(rejected_field(lang, "text", "This option needs text"));
    }
    if action.verb == Verb::Ans && request.option_id.as_deref().is_none_or(str::is_empty)
        && request.input.text.as_deref().is_none_or(|text| text.trim().is_empty()) {
        return Ok(rejected_field(lang, "option_id", "Choose an option"));
    }
    if is_external(action.verb) && !handed_back_in_domain(&state, &action) {
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
        return Ok(error_value_lang(lang, "forbidden", "device approval was revoked"));
    }
    let user_id = format!("device:{device}");
    let transport = identity.map(|(sender, request_id, digest)| (sender, request_id, digest));
    let stamp = match action.verb {
        Verb::Reply => "Reply sent",
        Verb::AskPr => "Question sent",
        Verb::Conflicts => "Conflicts requested",
        Verb::Msg => "Message sent",
        Verb::Relaunch => "Relaunched",
        Verb::Close => "Closed",
        Verb::Interrupt => "Interrupted",
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
        Verb::Reply => "Respuesta enviada",
        Verb::AskPr => "Pregunta enviada", Verb::Conflicts => "Conflictos comunicados",
        Verb::Msg => "Mensaje enviado", Verb::Relaunch => "Relanzada", Verb::Close => "Cerrado", Verb::Interrupt => "Interrumpido",
        Verb::OrchRetry => "Reintentado", Verb::OrchAck => "Confirmado", Verb::OrchStop => "Detenido",
        Verb::SchedRun => "Ejecución iniciada", Verb::SchedToggle => "Tarea actualizada",
        Verb::SchedDelete => "Tarea eliminada", Verb::SchedSave => "Tarea guardada",
        Verb::NewRequest => "Solicitud enviada", _ => stamp,
    }} else { stamp };
    let to_applied = |status: u16, body: Value| -> Value { applied(action.verb, lang, stamp, state.revision, status, body) };
    match action.verb {
        Verb::Reply => {
            let interaction_id = InteractionId(action.subject.clone());
            let Some(interaction) = state.interactions.get(&interaction_id)
                .filter(|interaction| !interaction.withdrawn && interaction.revision == action.revision)
            else {
                return Ok(no_longer_available(lang, "This item is no longer available"));
            };
            let (agent, task_id) = match &interaction.scope {
                Scope::Task { task_id } => {
                    let Some(agent) = state.tasks.get(task_id).and_then(|task| task.agent_id.clone()) else {
                        return Ok(no_longer_available(lang, "This item has no active node"));
                    };
                    (agent, Some(task_id.0.clone()))
                }
                Scope::Orchestrator { .. } => return Ok(no_longer_available(lang, "This item has no active node")),
            };
            if !state.agents.contains_key(&agent) { return Ok(no_longer_available(lang, "This node is no longer available")); }
            let comments: Vec<_> = request.input.comments.iter()
                .map(|comment| (comment.anchor.clone(), comment.text.clone(), comment.quote.clone())).collect();
            let text = alondra_core::views::review_export::text_with_quotes(&comments, request.input.text.as_deref().unwrap_or_default());
            if text.is_empty() { return Ok(error_value_lang(lang, "invalid_input", "text is required")); }
            let mark_read = matches!(interaction.payload, InteractionPayload::Information { .. });
            let body = json!({"text":text,"attachments":attachments,"kind":"reply",
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
                return Ok(no_longer_available(lang, "This pull request has no active node"));
            };
            let text = request.input.text.as_deref().unwrap_or_default().trim();
            if text.is_empty() { return Ok(error_value_lang(lang, "invalid_input", "text is required")); }
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
            if text.is_empty() { return Ok(error_value_lang(lang, "invalid_input", "text is required")); }
            let body = json!({"text":text,"attachments":attachments});
            let reply = client.write(&user_id, Write::OrchestratorMessage, &body, transport, &authorize).await?;
            Ok(to_applied(reply.status.as_u16(), reply.body))
        }
        Verb::Msg => {
            let agent = AgentId(action.subject.clone());
            if !state.agents.contains_key(&agent) { return Ok(no_longer_available(lang, "This node is no longer available")); }
            let text = request.input.text.as_deref().unwrap_or_default().trim();
            if text.is_empty() { return Ok(error_value_lang(lang, "invalid_input", "text is required")); }
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
        Verb::Relaunch => {
            let actor = Actor::User { user_id: user_id.clone() };
            let launched = crate::domain_orchestrator_tools::relaunch(domain, home, &actor, &TaskId(action.subject.clone()), &crate::orchestrate::ClaudeCli).await;
            let (status, body) = relaunch_reply(launched);
            Ok(to_applied(status, body))
        }
        Verb::Close => {
            let agent = AgentId(action.subject.clone());
            if !state.agents.contains_key(&agent) { return Ok(no_longer_available(lang, "This node is no longer available")); }
            let force = request.input.fields.get("force").and_then(Value::as_bool).unwrap_or(false);
            let reply = client.write(&user_id, Write::Close(agent.0), &json!({"force":force}), transport, &authorize).await?;
            Ok(to_applied(reply.status.as_u16(), reply.body))
        }
        Verb::Interrupt => {
            let agent = AgentId(action.subject.clone());
            if !state.agents.contains_key(&agent) { return Ok(no_longer_available(lang, "This node is no longer available")); }
            match domain.live.pushes.interrupt(&agent.0) {
                Ok(()) => Ok(to_applied(200, json!({"interrupted": agent.0}))),
                Err(_) => Ok(to_applied(409, json!({"code": "session_unreachable",
                    "message": "The node is not connected to Alondra: its session is stopped or its Claude Code mod is not running. Nothing was sent"}))),
            }
        }
        Verb::OrchRetry | Verb::OrchAck | Verb::OrchStop => {
            let runtime = crate::domain_orchestrator::load(home)?;
            let Some((run_id, decision)) = orchestrator_decision(&runtime, action) else {
                return Ok(no_longer_available(lang, "This run is no longer active"));
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
            if text.is_empty() { return Ok(error_value_lang(lang, "invalid_input", "text is required")); }
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

fn relaunch_reply(result: Result<Value, DomainError>) -> (u16, Value) {
    match result {
        Ok(launched) => (200, launched),
        Err(error) => {
            let status = match error.code.as_str() {
                "not_found" => 404,
                "conflict" | "agent_busy" => 409,
                _ => 422,
            };
            (status, json!({"code": error.code, "message": error.message}))
        }
    }
}

fn orchestrator_decision<'a>(
    runtime: &crate::domain_orchestrator::Runtime,
    action: &'a ActionId,
) -> Option<(&'a str, crate::domain_orchestrator::Decision)> {
    if action.subject != "orchestrator" {
        return None;
    }
    let run_id = action.extra.as_deref()?;
    let ambiguous = runtime.active_run.is_some() && runtime.last_error.is_some();
    if runtime.active_run.as_ref().is_none_or(|run| run.id != run_id) {
        return None;
    }
    let decision = match action.verb {
        Verb::OrchRetry if ambiguous => crate::domain_orchestrator::Decision::Retry,
        Verb::OrchAck if ambiguous => crate::domain_orchestrator::Decision::Acknowledge,
        Verb::OrchStop if !ambiguous => crate::domain_orchestrator::Decision::Cancel,
        _ => return None,
    };
    Some((run_id, decision))
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
        let options = schedule_option_specs(home, &request.input.fields, None);
        let body = schedule_draft(&request.input.fields, None, &options);
        if let Some(rejection) = schedule_field_rejection(&request.input.fields, &body, lang) {
            return Ok(rejection);
        }
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
            let mut body = schedule_draft(&serde_json::Map::new(), Some(schedule), &[]);
            body["enabled"] = json!(!schedule.enabled);
            ("PUT", format!("/schedules/{raw_id}"), Some(body))
        }
        Verb::SchedSave => {
            let options = schedule_option_specs(home, &request.input.fields, Some(schedule));
            let draft = schedule_draft(&request.input.fields, Some(schedule), &options);
            if let Some(rejection) = schedule_field_rejection(&request.input.fields, &draft, lang) {
                return Ok(rejection);
            }
            ("PUT", format!("/schedules/{raw_id}"), Some(draft))
        },
        _ => return Ok(no_longer_available(lang, "This schedule action is unavailable")),
    };
    let result = crate::domain_schedule_api::mutation(domain, home, method, &path,
        body.as_ref(), identity, authorize);
    Ok(schedule_reply(result, lang, stamp, state.revision))
}

fn schedule_option_specs(home: &std::path::Path, fields: &serde_json::Map<String, Value>, schedule: Option<&alondra_core::domain_v2::Schedule>) -> Vec<alondra_core::options::Spec> {
    let project = fields.get("project").and_then(Value::as_str)
        .or_else(|| schedule.and_then(|schedule| schedule.project.as_deref()))
        .filter(|project| !project.is_empty());
    let file = std::fs::read_to_string(alondra_core::options::path_in(home)).ok()
        .and_then(|contents| alondra_core::options::OptionsFile::parse(&contents).ok())
        .unwrap_or_default();
    file.catalog(project)
}

fn schedule_draft(fields: &serde_json::Map<String, Value>, schedule: Option<&alondra_core::domain_v2::Schedule>, option_specs: &[alondra_core::options::Spec]) -> Value {
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
    let previous_project = schedule.and_then(|schedule| schedule.project.as_deref()).unwrap_or("");
    let selected_project = project.as_str().unwrap_or("");
    let mut options = serde_json::Map::new();
    if previous_project == selected_project {
        for (id, choice) in schedule.into_iter().flat_map(|schedule| &schedule.options) {
            let value = json!(choice);
            if option_specs.is_empty() || option_specs.iter().find(|spec| spec.id == id.as_str())
                .is_some_and(|spec| valid_schedule_option(spec, &value) && value != json!(spec.default))
            {
                options.insert(id.clone(), value);
            }
        }
    }
    for spec in option_specs {
        if let Some(value) = fields.get(&spec.id) {
            options.remove(&spec.id);
            if valid_schedule_option(spec, value) && value != &json!(spec.default) {
                options.insert(spec.id.clone(), value.clone());
            }
        }
    }
    json!({
        "title":text("name", schedule.map_or("", |schedule| schedule.title.as_str())),
        "text":text("mission", schedule.map_or("", |schedule| schedule.text.as_str())),
        "project":project,
        "options":Value::Object(options),
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

fn valid_schedule_option(spec: &alondra_core::options::Spec, value: &Value) -> bool {
    match spec.kind {
        alondra_core::options::Kind::Toggle => value.is_boolean(),
        alondra_core::options::Kind::Choice => value.as_str().is_some_and(|selected| {
            spec.values.iter().any(|candidate| candidate.id == selected)
        }),
    }
}

const SCHEDULE_DATE_LIMIT_MILLIS: i64 = 4_102_444_800_000;

fn schedule_field_message(lang: Lang, field: &'static str, english: &'static str, spanish: &'static str) -> Value {
    let message = if lang == Lang::Es { spanish } else { english };
    json!({"protocol":VIEWS_PROTOCOL,"outcome":"rejected","field":field,"message":message})
}

fn schedule_field_rejection(fields: &serde_json::Map<String, Value>, draft: &Value, lang: Lang) -> Option<Value> {
    let reject = |field, english, spanish| Some(schedule_field_message(lang, field, english, spanish));
    let title = draft["title"].as_str().unwrap_or_default();
    if title.trim().is_empty() || title.len() > 120 {
        return reject("name", "Enter a name of up to 120 characters.", "Escribe un nombre de hasta 120 caracteres.");
    }
    let mission = draft["text"].as_str().unwrap_or_default();
    if mission.trim().is_empty() || mission.len() > 20_000 {
        return reject("mission", "Enter instructions of up to 20,000 characters.", "Escribe instrucciones de hasta 20.000 caracteres.");
    }
    let valid_time = fields.get("time").and_then(Value::as_str).is_none_or(|time| {
        time.split_once(':').is_some_and(|(hour, minute)| {
            hour.parse::<u8>().is_ok_and(|hour| hour < 24) && minute.parse::<u8>().is_ok_and(|minute| minute < 60)
        })
    });
    if !valid_time {
        return reject("time", "Choose a valid time.", "Elige una hora válida.");
    }
    let frequency = draft["frequency"].as_str().unwrap_or_default();
    if frequency == "weekly" && !draft["weekday"].as_i64().is_some_and(|weekday| (0..=6).contains(&weekday)) {
        return reject("weekday", "Choose a day of the week.", "Elige un día de la semana.");
    }
    let custom = matches!(frequency, "every_hours" | "every_days" | "every_months");
    if custom && !draft["interval"].as_i64().is_some_and(|interval| (1..=365).contains(&interval)) {
        return reject("interval", "Choose an interval from 1 to 365.", "Elige un intervalo entre 1 y 365.");
    }
    if (custom || frequency == "once")
        && !draft["start_at"].as_i64().is_some_and(|at| (0..=SCHEDULE_DATE_LIMIT_MILLIS).contains(&at)) {
        return reject("start_at", "Choose a date before 2100.", "Elige una fecha anterior al año 2100.");
    }
    let tools = draft["disallowed_tools"].as_array().map(Vec::as_slice).unwrap_or_default();
    if tools.len() > 100 || tools.iter().filter_map(Value::as_str).any(|pattern| {
        pattern.len() > 200 || pattern.chars().any(char::is_whitespace) || pattern.starts_with('-')
    }) {
        return reject("disallowed_tools", "Enter up to 100 tool patterns without spaces.", "Escribe hasta 100 patrones de herramientas sin espacios.");
    }
    None
}

fn schedule_daemon_rejection(message: &str, lang: Lang) -> Option<Value> {
    let (field, english, spanish) = match message {
        "one-time date must be in the future" => ("start_at", "Choose a future date and time.", "Elige una fecha y hora futuras."),
        "project cannot be empty" | "project must be one of the available project folders" =>
            ("project", "Choose an available project.", "Elige un proyecto disponible."),
        "projects directory does not exist" =>
            ("project", "The Projects folder is unavailable.", "La carpeta Proyectos no está disponible."),
        _ => return None,
    };
    Some(schedule_field_message(lang, field, english, spanish))
}

fn schedule_reply(result: Value, lang: Lang, stamp: &str, revision: u64) -> Value {
    let status = result["status"].as_u64().unwrap_or(500) as u16;
    let body = result["body"].clone();
    if (200..300).contains(&status) {
        json!({"protocol":VIEWS_PROTOCOL,"outcome":"applied","stamp":stamp,"revision":revision,"detail":body})
    } else if status == 404 || status == 409 {
        no_longer_available(lang, body.get("message").and_then(Value::as_str).unwrap_or("This schedule changed"))
    } else {
        let message = body.get("message").and_then(Value::as_str).unwrap_or("Schedule action failed");
        if let Some(rejection) = schedule_daemon_rejection(message, lang) {
            return rejection;
        }
        let code = body.get("code").and_then(Value::as_str).unwrap_or("schedule_rejected");
        json!({"protocol":VIEWS_PROTOCOL,"outcome":"rejected","code":code,"message":rejection_message(lang, code, message)})
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
    let lang = match request.lang.as_str() { "en" => Lang::En, "es" => Lang::Es, _ => return Ok(error_value("invalid_input", "invalid language")) };
    if request.protocol != VIEWS_PROTOCOL {
        return Ok(error_value_lang(lang, "incompatible_protocol", "Update Alondra"));
    }
    if !matches!(request.client.as_str(), "ios" | "mac" | "tui") {
        return Ok(error_value_lang(lang, "invalid_input", "invalid client"));
    }
    let action = match ActionId::parse(&request.action_id) {
        Ok(action) => action,
        Err(_) => return Ok(error_value_lang(lang, "invalid_input", "invalid action_id")),
    };
    if !is_uuid_v4(&request.command_id) {
        return Ok(error_value_lang(lang, "invalid_input", "command_id must be a UUID v4"));
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
    let conflicts = (action.verb == Verb::Conflicts)
        .then(|| state.pull_requests.get(&alondra_core::domain_v2::PrId(action.subject.clone())))
        .flatten();
    let (actor, command_id) = match conflicts {
        Some(pr) => (Actor::System, conflicts_command_id(pr)),
        None => (Actor::User { user_id: format!("device:{device}") }, command_id),
    };
    let envelope = CommandEnvelope {
        protocol_version: 2,
        data_epoch: DataEpoch(request.data_epoch),
        command_id,
        expected_revision: action.revision as Revision,
        command,
    };
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
    let tasks: Vec<_> = archive_targets(state, action.revision)
        .into_iter()
        .map(|task| task.id.clone())
        .collect();
    let mut revision = state.revision;
    for task_id in tasks {
        let current = domain.state()?;
        let Some(task) = current.tasks.get(&task_id).filter(|task| {
            task.state == alondra_core::domain_v2::TaskState::Done
                && !task.archived
                && task.revision <= action.revision
        }) else {
            continue;
        };
        let envelope = CommandEnvelope {
            protocol_version: 2,
            data_epoch: current.data_epoch.clone(),
            command_id: archive_command_id(&request.command_id, &task.id),
            expected_revision: current.revision,
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
            let option_id = request.option_id.clone().unwrap_or_default();
            let mut inputs = std::collections::BTreeMap::new();
            let option = options.iter().find(|option| option.id == option_id);
            if option_id.is_empty() {
                let text = request.input.text.as_ref()?.trim();
                if text.is_empty() { return None; }
                inputs.insert("text".into(), alondra_core::domain_v2::InputValue::Text(text.into()));
            } else if let Some(text) = request.input.text.as_ref() {
                let option = option?;
                let input = option.inputs.iter().find(|input| input.kind == alondra_core::domain_v2::InputKind::Text)?;
                inputs.insert(input.id.clone(), alondra_core::domain_v2::InputValue::Text(text.clone()));
            } else if option.is_none() {
                return None;
            }
            Some((Command::AnswerQuestion {
                interaction_id: interaction.id.clone(), option_id,
                inputs, attachments: attachments.to_vec(),
            }, match option {
                Some(option) => format!("{} · {}", stamp("Answered", "Respondida"), option.label),
                None => stamp("Answered", "Respondida"),
            }))
        }
        Verb::Ctx => {
            let interaction = state.interactions.get(&InteractionId(action.subject.clone()))?;
            let (InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. }) = &interaction.payload else { return None; };
            if state.holds.contains_key(&interaction.id) { return None; }
            let note = request.input.text.as_deref().unwrap_or_default().trim();
            let inputs = std::collections::BTreeMap::from([("text".to_owned(), alondra_core::domain_v2::InputValue::Text(context_request(note)))]);
            Some((Command::AnswerQuestion { interaction_id: interaction.id.clone(), option_id: String::new(),
                inputs, attachments: vec![] }, stamp("Context requested", "Contexto solicitado")))
        }
        Verb::Reply => {
            let interaction = state.interactions.get(&InteractionId(action.subject.clone()))?;
            let InteractionPayload::Delivery { targets, .. } = &interaction.payload else { return None; };
            let comments: Vec<_> = request.input.comments.iter()
                .map(|comment| (comment.anchor.clone(), comment.text.clone(), comment.quote.clone())).collect();
            let text = alondra_core::views::review_export::text_with_quotes(&comments, request.input.text.as_deref().unwrap_or_default());
            if text.trim().is_empty() { return None; }
            Some((Command::SubmitFeedback { presentation_id: interaction.id.clone(), targets: targets.clone(),
                body: text, attachments: attachments.to_vec(), instruction: Instruction::Reply },
                stamp("Reply sent", "Respuesta enviada")))
        }
        Verb::AskPr | Verb::Conflicts => {
            let pr = state.pull_requests.get(&alondra_core::domain_v2::PrId(action.subject.clone()))?;
            let presentation = pr_presentation(state, pr)?;
            let InteractionPayload::Delivery { targets, .. } = &presentation.payload else { return None; };
            let (body, attachments, instruction, stamp) = if action.verb == Verb::AskPr {
                let text = request.input.text.as_deref().unwrap_or_default().trim();
                if text.is_empty() { return None; }
                (text.to_owned(), attachments.to_vec(), Instruction::Question, stamp("Question sent", "Pregunta enviada"))
            } else {
                (crate::domain_messages::conflicts_text(&crate::domain_messages::user_name(), pr), vec![],
                    Instruction::Conflicts, stamp("Conflicts requested", "Conflictos comunicados"))
            };
            Some((Command::SubmitFeedback { presentation_id: presentation.id.clone(), targets: targets.clone(),
                body, attachments, instruction }, stamp))
        }
        Verb::Retake => {
            let interaction = state.interactions.get(&InteractionId(action.subject.clone()))?;
            let reopened = state.interactions.get(&InteractionId(request.option_id.clone().filter(|id| !id.is_empty())?))?;
            if state.group_members(interaction).iter().all(|member| member.id != reopened.id) {
                return None;
            }
            Some((Command::ReopenAnswer { interaction_id: reopened.id.clone() }, stamp("Question reopened", "Pregunta reabierta")))
        }
        Verb::Clarify => {
            let interaction = state.interactions.get(&InteractionId(action.subject.clone()))?;
            let reason = match request.option_id.as_deref().filter(|id| !id.is_empty()) {
                Some(name) => alondra_core::views::action_id::clarification_reason(name)?,
                None => alondra_core::domain_v2::ClarificationReason::Unclear,
            };
            let text = request.input.text.as_deref().map(str::trim).filter(|text| !text.is_empty()).map(str::to_owned);
            Some((Command::RequestClarification { interaction_id: interaction.id.clone(), reason, text },
                stamp("Clarification requested", "Aclaración solicitada")))
        }
        Verb::Read => {
            let interaction = state.interactions.get(&InteractionId(action.subject.clone()))?;
            Some((Command::MarkRead { interaction_id: interaction.id.clone() }, stamp("Marked as read", "Marcada como leída")))
        }
        Verb::Accept => {
            let interaction = state.interactions.get(&InteractionId(action.subject.clone()))?;
            if held_plan(state, interaction) && matches!(interaction.payload, InteractionPayload::Question { answer: None, .. }) {
                return Some((Command::AnswerQuestion { interaction_id: interaction.id.clone(), option_id: Hold::APPROVE.into(),
                    inputs: Default::default(), attachments: vec![] }, stamp("Approved", "Aprobado")));
            }
            let InteractionPayload::Delivery { targets, acceptance: None, .. } = &interaction.payload else { return None; };
            Some((Command::AcceptDelivery { presentation_id: interaction.id.clone(), targets: targets.clone() }, stamp("Accepted", "Aceptada")))
        }
        Verb::Changes => {
            let (interaction, pr_head) = if state.pull_requests.contains_key(&alondra_core::domain_v2::PrId(action.subject.clone())) {
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
            let comments: Vec<_> = request.input.comments.iter()
                .map(|comment| (comment.anchor.clone(), comment.text.clone(), comment.quote.clone())).collect();
            let body = alondra_core::views::review_export::text_with_quotes(&comments, request.input.text.as_deref().unwrap_or_default());
            if body.trim().is_empty() && request.input.comments.is_empty() { return None; }
            if let InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. } = &interaction.payload {
                let option_id = if held_plan(state, interaction) {
                    Hold::REQUEST_CHANGES.to_owned()
                } else if matches!(interaction.subject, Some(alondra_core::domain_v2::InteractionSubject::DocumentReview { .. })) {
                    String::new()
                } else {
                    return None;
                };
                let inputs = std::collections::BTreeMap::from([(Hold::NOTE.to_owned(), alondra_core::domain_v2::InputValue::Text(body))]);
                return Some((Command::AnswerQuestion { interaction_id: interaction.id.clone(), option_id,
                    inputs, attachments: attachments.to_vec() }, stamp("Changes requested", "Cambios solicitados")));
            }
            let InteractionPayload::Delivery { targets, .. } = &interaction.payload else { return None; };
            if let Some(head) = pr_head.as_deref() {
                if !targets.iter().any(|target| matches!(target, alondra_core::domain_v2::DeliveryTarget::PullRequest { head: target_head, .. } if target_head == head)) {
                    return None;
                }
            }
            Some((Command::SubmitFeedback { presentation_id: interaction.id.clone(), targets: targets.clone(), body,
                attachments: attachments.to_vec(), instruction: Instruction::Changes }, stamp("Changes requested", "Cambios solicitados")))
        }
        Verb::Merge => {
            let (interaction, target_pr) = if state.pull_requests.contains_key(&alondra_core::domain_v2::PrId(action.subject.clone())) {
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
            if state.pull_requests.values().any(|upstream| upstream.id != pr.id
                && upstream.state == alondra_core::domain_v2::PrState::Open
                && upstream.repository == pr.repository && upstream.head == pr.base) {
                return None;
            }
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

/// A reply to a delivery, a question about a presented PR or a request to resolve its conflicts
/// is feedback on that delivery, so its card leaves the queue; with no delivery to hold it is
/// only typed to the node.
fn handed_back_in_domain(state: &alondra_core::domain_v2::DomainState, action: &ActionId) -> bool {
    match action.verb {
        Verb::Reply => state.interactions.get(&InteractionId(action.subject.clone()))
            .is_some_and(|interaction| matches!(interaction.payload, InteractionPayload::Delivery { .. })),
        Verb::AskPr | Verb::Conflicts => state.pull_requests.get(&alondra_core::domain_v2::PrId(action.subject.clone()))
            .is_some_and(|pr| pr_presentation(state, pr).is_some()),
        _ => false,
    }
}

/// Resolve conflicts is Alondra's own request, whether the user tapped it or Alondra noticed:
/// one per PR head, so a second one for the same head changes nothing.
pub(crate) fn conflicts_command_id(pr: &alondra_core::domain_v2::PullRequest) -> CommandId {
    CommandId(format!("conflicts-{}-{}", pr.id, pr.head))
}

/// The open PR's current final delivery, presented at the PR's head.
pub(crate) fn pr_presentation<'a>(
    state: &'a alondra_core::domain_v2::DomainState,
    pr: &alondra_core::domain_v2::PullRequest,
) -> Option<&'a alondra_core::domain_v2::Interaction> {
    if pr.state != alondra_core::domain_v2::PrState::Open { return None; }
    let task = state.tasks.get(&pr.task_id).filter(|task| !task.state.terminal())?;
    let presentation = state.interactions.get(task.final_presentation.as_ref()?)?;
    let InteractionPayload::Delivery { targets, .. } = &presentation.payload else { return None; };
    targets.iter().any(|target| matches!(target, alondra_core::domain_v2::DeliveryTarget::PullRequest { pr_id, head }
        if pr_id == &pr.id && head == &pr.head)).then_some(presentation)
}

fn held_plan(state: &alondra_core::domain_v2::DomainState, interaction: &alondra_core::domain_v2::Interaction) -> bool {
    state.holds.get(&interaction.id) == Some(&Hold::Plan)
}

fn changed_pr_head(
    state: &alondra_core::domain_v2::DomainState,
    action: &ActionId,
    lang: Lang,
) -> Option<Value> {
    let (pr_id, presentation_id, old_prefix) = match action.verb {
        Verb::Changes if state.pull_requests.contains_key(&alondra_core::domain_v2::PrId(action.subject.clone())) => (
            alondra_core::domain_v2::PrId(action.subject.clone()),
            None,
            action.extra.as_deref()?,
        ),
        Verb::Merge => {
            let (prefix, presentation_id) = action.extra.as_deref()?.split_once('-')?;
            let pr_id = if state.pull_requests.contains_key(&alondra_core::domain_v2::PrId(action.subject.clone())) {
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
            let id = if state.pull_requests.contains_key(&alondra_core::domain_v2::PrId(action.subject.clone())) { presentation } else { action.subject.as_str() };
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

fn context_request(note: &str) -> String {
    let detail = if note.is_empty() { String::new() } else { format!(" [What is missing: {note}]") };
    format!("I need more context before deciding.{detail} Explain what is at stake, the options with their consequences and your recommendation, and ask again.")
}

fn no_longer_available(lang: Lang, message: &str) -> Value {
    json!({"protocol":VIEWS_PROTOCOL,"outcome":"no_longer_available","message":localize(lang, message)})
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

fn error_value_lang(lang: Lang, code: &str, message: &'static str) -> Value {
    error_value(code, localize(lang, message))
}

fn rejected_field(lang: Lang, field: &str, message: &str) -> Value {
    json!({"protocol":VIEWS_PROTOCOL,"outcome":"rejected","field":field,"message":localize(lang, message)})
}

/// What a local reply's status and body mean to the client. A message the node has taken says so,
/// one waiting on a busy node says that, and a node that is not connected is refused as such,
/// not as an action that is no longer available.
fn applied(verb: Verb, lang: Lang, stamp: &str, revision: u64, status: u16, body: Value) -> Value {
    let code = body.get("code").and_then(Value::as_str);
    if (200..300).contains(&status) {
        let stamp = match (verb, body.get("delivery_state").and_then(Value::as_str), lang == Lang::Es) {
            (Verb::Msg, Some("handed"), false) => "Message delivered",
            (Verb::Msg, Some("handed"), true) => "Mensaje entregado",
            (Verb::Msg, Some("dispatching"), false) => "Message queued: the node is busy and takes it when its turn ends",
            (Verb::Msg, Some("dispatching"), true) => "Mensaje en cola: el nodo está ocupado y lo recibe cuando termine su turno",
            _ => stamp,
        };
        json!({"protocol":VIEWS_PROTOCOL,"outcome":"applied","stamp":stamp,"revision":revision,"detail":body})
    } else if status == 404 || (status == 409 && code != Some("session_unreachable")) {
        no_longer_available(lang, body.get("message").and_then(Value::as_str).unwrap_or("This action is no longer available"))
    } else {
        let message = body.get("message").and_then(Value::as_str).unwrap_or("Action was rejected");
        let code = code.unwrap_or("action_rejected");
        json!({"protocol":VIEWS_PROTOCOL,"outcome":"rejected","message":rejection_message(lang, code, message),"code":code})
    }
}

fn rejection_message(lang: Lang, code: &str, english: &str) -> String {
    if lang != Lang::Es { return english.to_owned(); }
    match code {
        "incompatible_protocol" => "Actualiza Alondra para completar esta acción",
        "forbidden" => "Este dispositivo ya no tiene permiso para completar la acción",
        "invalid_input" => "Revisa los datos de la acción e inténtalo de nuevo",
        "not_found" | "no_longer_available" => "Este elemento ya no está disponible",
        "conflict" | "stale_head" | "stale_revision" | "stale_epoch" | "stale_generation"
        | "acceptance_stale" => "El elemento ha cambiado; actualiza la vista e inténtalo de nuevo",
        "stack_not_ready" => "La pila todavía no está lista para esta acción",
        "task_not_active" => "La tarea ya no está activa",
        "withdrawn" => "La persona autora retiró esta pregunta",
        "dependency_not_done" => "Completa primero la tarea dependiente",
        "not_assigned" => "La tarea ya no está asignada a este nodo",
        "agent_busy" => "Ese nodo está en otra tarea hasta que acabe: lanza un nodo nuevo para esta",
        "invalid_action_for_kind" => "Esta acción no se puede aplicar a este elemento",
        "acceptance_missing" => "Autoriza primero la fusión de la PR presentada",
        "subject_already_linked" => "Este elemento ya está vinculado a otra tarea",
        "delivery_uncertain" => "No se pudo confirmar si el nodo recibió el mensaje",
        "blocking_interaction" => "Responde primero a la interacción pendiente de la tarea",
        "merge_not_confirmed" => "No se pudo confirmar la fusión de la PR",
        "task_not_cancelled" => "La tarea todavía no está cancelada",
        "interactive_session" => "La sesión del nodo requiere interacción manual",
        "launch_failed" => "No se pudo iniciar la sesión del nodo",
        "session_unreachable" => "El nodo no está conectado a Alondra: su sesión está parada o su mod de Claude Code no se está ejecutando. No se ha enviado nada",
        "unavailable" => "Esta acción no está disponible ahora mismo",
        "task_already_has_pr" => "Esta tarea ya tiene una PR asociada",
        "pr_owned_by_other_task" => "Esta PR ya pertenece a otra tarea",
        "idempotency_conflict" => "El identificador del comando ya se usó con otros datos",
        "dependency_cycle" => "La dependencia crearía un ciclo entre tareas",
        "content_unavailable" => "El contenido ya no está disponible",
        "payload_too_large" => "La solicitud supera el tamaño permitido",
        "storage_unavailable" => "Los datos de Alondra no están disponibles ahora mismo",
        "external_read_failed" | "github_unavailable" => "No se pudo comprobar el estado remoto",
        "schedule_rejected" => "No se pudo completar la acción de programación",
        _ => "No se pudo completar la acción",
    }.to_owned()
}

fn localize<'a>(lang: Lang, message: &'a str) -> &'a str {
    if lang != Lang::Es { return message; }
    match message {
        "invalid action_id" => "Acción no válida",
        "Update Alondra" => "Actualiza Alondra",
        "invalid action request" => "Solicitud de acción no válida",
        "invalid client" => "Cliente no válido",
        "device approval was revoked" => "Se ha revocado la aprobación del dispositivo",
        "text is required" | "This option needs text" => "Escribe un texto",
        "command_id must be a UUID v4" => "El identificador del comando no es válido",
        "An attachment is no longer available" => "Un archivo adjunto ya no está disponible",
        "Choose an option" => "Elige una opción",
        "This item is no longer available" => "Este elemento ya no está disponible",
        "This item has changed" => "Este elemento ha cambiado",
        "This action is no longer available" => "Esta acción ya no está disponible",
        "This item has no active node" => "Este elemento no tiene un nodo activo",
        "This node is no longer available" => "Este nodo ya no está disponible",
        "This pull request is no longer open" => "Esta PR ya no está abierta",
        "This pull request is no longer available" => "Esta PR ya no está disponible",
        "This pull request has no active node" => "Esta PR no tiene un nodo activo",
        "This task is no longer assigned to the node" => "Esta tarea ya no está asignada al nodo",
        "This run is no longer active" => "Esta ejecución ya no está activa",
        "This schedule is no longer available" => "Esta programación ya no está disponible",
        "This schedule has changed" | "This schedule changed" => "Esta programación ha cambiado",
        "This schedule action is unavailable" => "Esta acción de programación no está disponible",
        "This view has changed" => "Esta vista ha cambiado",
        "A completed task changed; refresh the view" => "Una tarea completada ha cambiado; actualiza la vista",
        _ => "Ya no está disponible",
    }
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

    fn choice_spec(id: &str, selected: &str, default: &str) -> alondra_core::options::Spec {
        alondra_core::options::Spec {
            id: id.into(), label: id.into(), kind: alondra_core::options::Kind::Choice,
            values: vec![
                alondra_core::options::SpecValue { id: selected.into(), label: selected.into(), description: None },
                alondra_core::options::SpecValue { id: default.into(), label: default.into(), description: None },
            ],
            default: alondra_core::options::Choice::Value(default.into()), builtin: false,
        }
    }

    #[test]
    fn schedule_draft_accepts_only_options_in_the_selected_catalog() {
        let fields = json!({"name":"Morning", "mission":"Review work", "project":"App",
            "pr":"review", "effort":"high", "injected":"unexpected"});
        let specs = [choice_spec("pr", "review", "auto"), choice_spec("effort", "high", "normal")];
        let draft = schedule_draft(fields.as_object().unwrap(), None, &specs);
        assert_eq!(draft["options"], json!({"pr":"review", "effort":"high"}));
        assert_eq!(draft["project"], "App");
    }

    #[test]
    fn schedule_draft_drops_old_project_options_when_project_changes() {
        let schedule: alondra_core::domain_v2::Schedule = serde_json::from_value(json!({
            "id":"schedule-1", "title":"Morning", "text":"Review work",
            "project":"Old", "options":{"old_option":"old_value"},
            "disallowed_tools":[], "frequency":"daily", "hour":9, "minute":0,
            "weekday":null, "interval":null, "start_at":null, "enabled":true,
            "next_run":null, "revision":1, "deleted_at":null
        })).unwrap();
        let fields = json!({"project":"New", "new_option":"new_value", "old_option":"stale"});
        let specs = [choice_spec("new_option", "new_value", "default")];
        let draft = schedule_draft(fields.as_object().unwrap(), Some(&schedule), &specs);
        assert_eq!(draft["options"], json!({"new_option":"new_value"}));
        let same_project = json!({"project":"Old"});
        let draft = schedule_draft(same_project.as_object().unwrap(), Some(&schedule), &[]);
        assert_eq!(draft["options"], json!({"old_option":"old_value"}));
    }

    fn apply_seed_command(
        domain: &crate::domain_runtime::DomainRuntime,
        actor: Actor,
        command_id: &str,
        command: Command,
    ) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: 2,
            data_epoch: state.data_epoch,
            command_id: command_id.into(),
            expected_revision: state.revision,
            command,
        }, None, || Ok(())).unwrap();
    }

    fn semantic_receipt_count(home: &std::path::Path) -> i64 {
        let database = crate::domain_database_migration::active_path(home);
        let connection = rusqlite::Connection::open(database).unwrap();
        connection.query_row("SELECT count(*) FROM semantic_receipts", [], |row| row.get(0)).unwrap()
    }

    #[tokio::test]
    async fn archive_done_is_idempotent() {
        use alondra_core::domain_v2::DeliveryTarget;
        use std::{fs, path::PathBuf};

        struct TestHome(PathBuf);
        impl Drop for TestHome {
            fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
        }

        let home = std::env::temp_dir().join(format!("alondra-archive-action-{}", crate::auth::random_hex(8)));
        fs::create_dir(&home).unwrap();
        let _home = TestHome(home.clone());
        let domain = std::sync::Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let client = crate::v2_client_routes::Client::open(domain.clone(), &home, Default::default());
        apply_seed_command(&domain, Actor::System, "register-agent", Command::RegisterAgent {
            agent_id: "agent-a".into(), name: "Agent A".into(),
        });
        apply_seed_command(&domain, Actor::User { user_id: "user".into() }, "create-request", Command::CreateRequest {
            request_id: "request".into(), body: "Work".into(), auto_merge: false,
        });
        apply_seed_command(&domain, Actor::User { user_id: "user".into() }, "title-request", Command::TitleRequest {
            request_id: "request".into(), title: "Work".into(),
        });
        for task_id in ["task-a", "task-b"] {
            apply_seed_command(&domain, Actor::User { user_id: "user".into() }, &format!("create-{task_id}"), Command::CreateTask {
                task_id: task_id.into(), request_id: "request".into(), title: task_id.into(),
                agent_id: Some("agent-a".into()), review_prs: false,
                dependencies: Default::default(), stack: None,
            });
            let delivery_id = format!("delivery-{task_id}");
            apply_seed_command(&domain, Actor::Agent { agent_id: "agent-a".into() }, &format!("present-{task_id}"), Command::PresentResult {
                attachments: vec![], task_id: task_id.into(), interaction_id: delivery_id.clone().into(),
                body: "Complete".into(), targets: vec![DeliveryTarget::Text { text: "Complete".into() }],
                is_final: true, card: alondra_core::domain_v2::CardSpec::delivery("Review the result", &[], true),
            });
            apply_seed_command(&domain, Actor::User { user_id: "user".into() }, &format!("accept-{task_id}"), Command::AcceptDelivery {
                presentation_id: delivery_id.clone().into(),
                targets: vec![DeliveryTarget::Text { text: "Complete".into() }],
            });
        }

        let before = domain.state().unwrap();
        let action = ActionId::new(Verb::ArchiveDone, "view", before.revision, None).unwrap();
        let request = ActionRequest {
            protocol: VIEWS_PROTOCOL,
            command_id: "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10".into(),
            data_epoch: before.data_epoch.0.clone(),
            client: "ios".into(),
            lang: "en".into(),
            action_id: action.to_string(),
            option_id: None,
            input: ActionInput::default(),
        };
        let receipts_before = semantic_receipt_count(&home);
        let first = execute_action(&domain, &client, &home, "test-device", request.clone(), None, || Ok(())).await.unwrap();
        assert_eq!(first["outcome"], "applied", "{first}");
        let archived = domain.state().unwrap();
        assert!(archived.tasks.values().all(|task| task.archived));
        assert_eq!(semantic_receipt_count(&home), receipts_before + 2,
            "the first application commits one durable archive command per task");

        let second = execute_action(&domain, &client, &home, "test-device", request, None, || Ok(())).await.unwrap();
        assert_eq!(second["outcome"], "applied", "{second}");
        let repeated = domain.state().unwrap();
        assert_eq!(repeated.revision, archived.revision, "repeating the action changes no domain revision");
        assert!(repeated.tasks.values().all(|task| task.archived));
        assert_eq!(semantic_receipt_count(&home), receipts_before + 2,
            "the repeated action creates no duplicate durable commands");
    }

    #[test]
    fn accepts_only_uuid_v4_command_ids() {
        assert!(is_uuid_v4("5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10"));
        assert!(!is_uuid_v4("5f0c2d9e-8d0b-1c71-9b0e-2b6f5d8f1a10"));
        assert!(!is_uuid_v4("5f0c2d9e-8d0b-4c71-7b0e-2b6f5d8f1a10"));
        assert!(!is_uuid_v4("not-a-uuid"));
    }

    #[test]
    fn spanish_action_errors_keep_the_specific_reason() {
        assert_eq!(rejected_field(Lang::Es, "attachments", "An attachment is no longer available")["message"],
            "Un archivo adjunto ya no está disponible");
        assert_eq!(rejected_field(Lang::Es, "option_id", "Choose an option")["message"],
            "Elige una opción");
        assert_eq!(no_longer_available(Lang::Es, "This pull request is no longer open")["message"],
            "Esta PR ya no está abierta");
        assert_eq!(rejection_message(Lang::Es, "stale_head", "head changed"),
            "El elemento ha cambiado; actualiza la vista e inténtalo de nuevo");
        assert_eq!(rejection_message(Lang::Es, "storage_unavailable", "database unavailable"),
            "Los datos de Alondra no están disponibles ahora mismo");
        assert_eq!(rejection_message(Lang::En, "storage_unavailable", "database unavailable"),
            "database unavailable");
        for (code, expected) in [
            ("stack_not_ready", "La pila todavía no está lista para esta acción"),
            ("acceptance_stale", "El elemento ha cambiado; actualiza la vista e inténtalo de nuevo"),
            ("task_not_active", "La tarea ya no está activa"),
            ("withdrawn", "La persona autora retiró esta pregunta"),
            ("dependency_not_done", "Completa primero la tarea dependiente"),
            ("subject_already_linked", "Este elemento ya está vinculado a otra tarea"),
            ("delivery_uncertain", "No se pudo confirmar si el nodo recibió el mensaje"),
            ("blocking_interaction", "Responde primero a la interacción pendiente de la tarea"),
            ("merge_not_confirmed", "No se pudo confirmar la fusión de la PR"),
            ("task_not_cancelled", "La tarea todavía no está cancelada"),
            ("incompatible_protocol", "Actualiza Alondra para completar esta acción"),
            ("interactive_session", "La sesión del nodo requiere interacción manual"),
            ("launch_failed", "No se pudo iniciar la sesión del nodo"),
            ("session_unreachable", "El nodo no está conectado a Alondra: su sesión está parada o su mod de Claude Code no se está ejecutando. No se ha enviado nada"),
            ("unavailable", "Esta acción no está disponible ahora mismo"),
        ] {
            assert_eq!(rejection_message(Lang::Es, code, "domain detail"), expected, "{code}");
        }
        assert_eq!(rejection_message(Lang::Es, "revision_conflict", "legacy"), "No se pudo completar la acción");
    }

    #[test]
    fn a_message_says_whether_the_node_took_it_is_busy_or_is_not_connected() {
        let reply = |lang, status, body: Value| applied(Verb::Msg, lang, "Message sent", 7, status, body);
        let handed = reply(Lang::En, 200, json!({"delivery_state": "handed"}));
        assert_eq!((handed["outcome"].as_str(), handed["stamp"].as_str()), (Some("applied"), Some("Message delivered")));
        assert_eq!(reply(Lang::Es, 200, json!({"delivery_state": "handed"}))["stamp"], "Mensaje entregado");
        let busy = reply(Lang::En, 200, json!({"delivery_state": "dispatching"}));
        assert_eq!(busy["stamp"], "Message queued: the node is busy and takes it when its turn ends");
        assert!(reply(Lang::Es, 200, json!({"delivery_state": "dispatching"}))["stamp"].as_str().unwrap().starts_with("Mensaje en cola"));
        let refused = reply(Lang::En, 409, json!({"code": "session_unreachable", "message": "the node is not connected to Alondra"}));
        assert_eq!((refused["outcome"].as_str(), refused["code"].as_str(), refused["message"].as_str()),
            (Some("rejected"), Some("session_unreachable"), Some("the node is not connected to Alondra")));
        let es = reply(Lang::Es, 409, json!({"code": "session_unreachable", "message": "x"}));
        assert!(es["message"].as_str().unwrap().starts_with("El nodo no está conectado a Alondra"), "{es}");
        assert_eq!(reply(Lang::En, 409, json!({"code": "conflict", "message": "stale"}))["outcome"], "no_longer_available");
        assert_eq!(applied(Verb::Reply, Lang::En, "Reply sent", 7, 200, json!({"delivery_state": "handed"}))["stamp"], "Reply sent");
    }

    #[test]
    fn more_context_without_a_note_still_requests_explanation() {
        assert_eq!(context_request(""),
            "I need more context before deciding. Explain what is at stake, the options with their consequences and your recommendation, and ask again.");
        assert!(context_request("Timing").contains("[What is missing: Timing]"));
    }

    #[test]
    fn a_disabled_action_is_not_executed() {
        use crate::domain_orchestrator::{Decision, Run, Runtime};

        let retry = ActionId::new(Verb::OrchRetry, "orchestrator", 1, Some("run-1".into())).unwrap();
        let acknowledge = ActionId::new(Verb::OrchAck, "orchestrator", 1, Some("run-1".into())).unwrap();
        let stop = ActionId::new(Verb::OrchStop, "orchestrator", 1, Some("run-1".into())).unwrap();
        assert!(orchestrator_decision(&Runtime::default(), &retry).is_none(),
            "a retry that is not rendered for an active run must not execute");

        let mut runtime = Runtime {
            active_run: Some(Run {
                id: "run-1".into(), message_ids: vec![], attempts: vec![],
                started_at: 0, spawned: false,
            }),
            ..Default::default()
        };
        assert!(orchestrator_decision(&runtime, &retry).is_none());
        assert_eq!(orchestrator_decision(&runtime, &stop).map(|(_, decision)| decision), Some(Decision::Cancel));

        runtime.last_error = Some("uncertain".into());
        assert_eq!(orchestrator_decision(&runtime, &retry).map(|(_, decision)| decision), Some(Decision::Retry));
        assert_eq!(orchestrator_decision(&runtime, &acknowledge).map(|(_, decision)| decision), Some(Decision::Acknowledge));
        assert!(orchestrator_decision(&runtime, &stop).is_none());

        let forged = ActionId::new(Verb::OrchRetry, "another-subject", 1, Some("run-1".into())).unwrap();
        assert!(orchestrator_decision(&runtime, &forged).is_none());
    }

    #[test]
    fn free_text_question_maps_to_empty_option_and_text_input() {
        use alondra_core::domain_v2::{Author, Interaction, InteractionPayload, Recipient, Scope, InputValue, QuestionOption};
        let mut state = alondra_core::domain_v2::DomainState::new("epoch");
        state.interactions.insert("q-1".into(), Interaction {
            attachments: vec![], id: "q-1".into(), scope: Scope::Task { task_id: "t-1".into() },
            author: Author::System, recipient: Recipient::User, body: "Pick one or explain".into(),
            blocking: true, subject: None, payload: InteractionPayload::Question {
                options: vec![QuestionOption {
                    id: "yes".into(), label: "Yes".into(), description: None, inputs: vec![],
                }], answer: None,
            }, revision: 1, order: 1, withdrawn: false, card: alondra_core::domain_v2::CardSpec::derived(alondra_core::domain_v2::CardKind::Know, "Read the update"), clarification: None,
        });
        let request = ActionRequest {
            protocol: VIEWS_PROTOCOL, command_id: "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10".into(),
            data_epoch: "epoch".into(), client: "ios".into(), lang: "en".into(),
            action_id: String::new(), option_id: Some(String::new()),
            input: ActionInput { text: Some("  My own answer  ".into()), ..Default::default() },
        };
        let action = ActionId::new(Verb::Ans, "q-1", 1, None).unwrap();
        let Some((Command::AnswerQuestion { option_id, inputs, .. }, _)) =
            map_domain_action(&state, &action, &request, &[], Lang::En) else { panic!("free answer") };
        assert!(option_id.is_empty());
        assert_eq!(inputs.get("text"), Some(&InputValue::Text("My own answer".into())));
        let mut blank = request;
        blank.input.text = Some("   ".into());
        assert!(map_domain_action(&state, &action, &blank, &[], Lang::En).is_none());
        blank.option_id = Some("yes".into());
        blank.input.text = None;
        let Some((Command::AnswerQuestion { option_id, inputs, .. }, _)) =
            map_domain_action(&state, &action, &blank, &[], Lang::En) else { panic!("option answer") };
        assert_eq!(option_id, "yes");
        assert!(inputs.is_empty());
    }

    #[test]
    fn requesting_changes_on_a_document_question_answers_it_with_the_block_comments() {
        use alondra_core::domain_v2::{Author, InputValue, Interaction, InteractionSubject, QuestionOption, Recipient, Scope};
        let mut state = alondra_core::domain_v2::DomainState::new("epoch");
        state.interactions.insert("q-1".into(), Interaction {
            attachments: vec![], id: "q-1".into(), scope: Scope::Task { task_id: "t-1".into() },
            author: Author::Agent { agent_id: "a-1".into() }, recipient: Recipient::User, body: "Approve the plan?".into(),
            blocking: true, subject: Some(InteractionSubject::DocumentReview {
                document_id: "plan".into(), revision_id: "plan-r1".into() }),
            payload: InteractionPayload::Question {
                options: vec![QuestionOption { id: "approve".into(), label: "Approve".into(), description: None, inputs: vec![] }],
                answer: None,
            }, revision: 1, order: 1, withdrawn: false, card: alondra_core::domain_v2::CardSpec::derived(alondra_core::domain_v2::CardKind::Know, "Read the update"), clarification: None,
        });
        let request = ActionRequest {
            protocol: VIEWS_PROTOCOL, command_id: "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10".into(),
            data_epoch: "epoch".into(), client: "ios".into(), lang: "es".into(),
            action_id: String::new(), option_id: None,
            input: ActionInput { text: Some("Split it in two PRs".into()),
                comments: vec![Comment { anchor: "p-2".into(), text: "Drop this step".into(), quote: None }],
                ..Default::default() },
        };
        let changes = ActionId::new(Verb::Changes, "q-1", 1, None).unwrap();
        let Some((Command::AnswerQuestion { interaction_id, option_id, inputs, .. }, stamp)) =
            map_domain_action(&state, &changes, &request, &[], Lang::Es) else { panic!("changes answer the question") };
        assert_eq!((interaction_id.0.as_str(), option_id.as_str(), stamp.as_str()), ("q-1", "", "Cambios solicitados"));
        assert_eq!(inputs.get("text"), Some(&InputValue::Text(
            "I reviewed your code and have the following comments. Please address them.\n\n1. `p-2` - Drop this step\n\nSplit it in two PRs".into())));
        state.interactions.get_mut(&"q-1".into()).unwrap().subject = None;
        assert!(map_domain_action(&state, &changes, &request, &[], Lang::Es).is_none(),
            "a question about no document is answered, not reviewed");
    }

    #[test]
    fn a_held_plan_is_approved_or_sent_back_with_the_comments_on_its_blocks() {
        use alondra_core::domain_v2::{Author, InputValue, Interaction, Recipient, Scope};
        let mut state = alondra_core::domain_v2::DomainState::new("epoch");
        state.interactions.insert("hold-1".into(), Interaction {
            attachments: vec![], id: "hold-1".into(), scope: Scope::Task { task_id: "t-1".into() },
            author: Author::System, recipient: Recipient::User, body: "# Plan\n\n1. Split the lexer".into(),
            blocking: true, subject: None,
            payload: InteractionPayload::Question { options: Hold::Plan.options(), answer: None },
            revision: 1, order: 1, withdrawn: false, card: alondra_core::domain_v2::CardSpec::derived(alondra_core::domain_v2::CardKind::Know, "Read the update"), clarification: None,
        });
        state.holds.insert("hold-1".into(), Hold::Plan);
        let request = ActionRequest {
            protocol: VIEWS_PROTOCOL, command_id: "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10".into(),
            data_epoch: "epoch".into(), client: "ios".into(), lang: "en".into(),
            action_id: String::new(), option_id: None,
            input: ActionInput { comments: vec![Comment { anchor: "p-1".into(), text: "Keep the lexer".into(), quote: None }],
                ..Default::default() },
        };
        let changes = ActionId::new(Verb::Changes, "hold-1", 1, None).unwrap();
        let Some((Command::AnswerQuestion { option_id, inputs, .. }, _)) =
            map_domain_action(&state, &changes, &request, &[], Lang::En) else { panic!("changes answer the plan") };
        assert_eq!(option_id, Hold::REQUEST_CHANGES);
        assert_eq!(inputs.get(Hold::NOTE), Some(&InputValue::Text(
            "I reviewed your code and have the following comments. Please address them.\n\n1. `p-1` - Keep the lexer".into())));
        let quoted = ActionRequest { input: ActionInput { text: Some("Looks right.".into()),
            comments: vec![Comment { anchor: "paragraph:1:0".into(), text: "Keep the lexer".into(), quote: Some("Split the lexer".into()) }],
            ..Default::default() }, ..request.clone() };
        let Some((Command::AnswerQuestion { inputs, .. }, _)) =
            map_domain_action(&state, &changes, &quoted, &[], Lang::En) else { panic!("changes answer the plan") };
        assert_eq!(inputs.get(Hold::NOTE), Some(&InputValue::Text(
            "I reviewed this and have the following comments. Please address them.\n\n> Split the lexer\nKeep the lexer\n\nLooks right.".into())));
        let approve = ActionId::new(Verb::Accept, "hold-1", 1, None).unwrap();
        let Some((Command::AnswerQuestion { option_id, inputs, .. }, stamp)) =
            map_domain_action(&state, &approve, &request, &[], Lang::En) else { panic!("approve answers the plan") };
        assert_eq!((option_id.as_str(), stamp.as_str()), (Hold::APPROVE, "Approved"));
        assert!(inputs.is_empty());
        state.holds.clear();
        assert!(map_domain_action(&state, &approve, &request, &[], Lang::En).is_none(), "only a held plan is approved");
        assert!(map_domain_action(&state, &changes, &request, &[], Lang::En).is_none());
    }

    #[test]
    fn canonical_pr_subject_maps_changes_merge_and_stale_head() {
        use alondra_core::domain_v2::{Author, DeliveryTarget, Interaction, PrState, PullRequest,
            Recipient, Scope};
        let id = "github.com/bitomule/alondra#154";
        let mut state = alondra_core::domain_v2::DomainState::new("epoch");
        state.pull_requests.insert(id.into(), PullRequest {
            id: id.into(), task_id: "t-1".into(), host: "github.com".into(),
            repository: "bitomule/alondra".into(), number: 154,
            head: "abcdef123456".into(), base: "main".into(), state: PrState::Open,
            merged_head: None, allowed_methods: vec![MergeMethod::Merge], review: None,
        });
        state.interactions.insert("i-1".into(), Interaction {
            attachments: vec![], id: "i-1".into(), scope: Scope::Task { task_id: "t-1".into() },
            author: Author::System, recipient: Recipient::User, body: "Review".into(),
            blocking: true, subject: None,
            payload: InteractionPayload::Delivery {
                targets: vec![DeliveryTarget::PullRequest { pr_id: id.into(), head: "abcdef123456".into() }],
                is_final: false, acceptance: None, feedback: vec![],
            }, revision: 1, order: 1, withdrawn: false, card: alondra_core::domain_v2::CardSpec::derived(alondra_core::domain_v2::CardKind::Know, "Read the update"), clarification: None,
        });
        let request = ActionRequest {
            protocol: VIEWS_PROTOCOL, command_id: "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10".into(),
            data_epoch: "epoch".into(), client: "ios".into(), lang: "en".into(),
            action_id: String::new(), option_id: Some("merge".into()),
            input: ActionInput { text: Some("Please revise".into()), ..Default::default() },
        };
        let changes = ActionId::new(Verb::Changes, id, 1, Some("abcdef12".into())).unwrap();
        assert!(matches!(map_domain_action(&state, &changes, &request, &[], Lang::En),
            Some((Command::SubmitFeedback { .. }, _))));
        let merge = ActionId::new(Verb::Merge, id, 1, Some("abcdef12-i-1".into())).unwrap();
        assert!(matches!(map_domain_action(&state, &merge, &request, &[], Lang::En),
            Some((Command::AuthorizeMerge { .. }, _))));
        state.pull_requests.get_mut(&id.into()).unwrap().base = "upstream-head".into();
        state.pull_requests.insert("github.com/bitomule/alondra#153".into(), PullRequest {
            id: "github.com/bitomule/alondra#153".into(), task_id: "t-2".into(), host: "github.com".into(),
            repository: "bitomule/alondra".into(), number: 153, head: "upstream-head".into(),
            base: "main".into(), state: PrState::Open, merged_head: None,
            allowed_methods: vec![MergeMethod::Merge], review: None,
        });
        assert!(map_domain_action(&state, &merge, &request, &[], Lang::En).is_none());
        let old = ActionId::new(Verb::Changes, id, 1, Some("00000000".into())).unwrap();
        assert_eq!(changed_pr_head(&state, &old, Lang::En).unwrap()["outcome"], "changed");
    }

    struct ActionHome(std::path::PathBuf);
    impl Drop for ActionHome {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    fn action_home() -> (ActionHome, std::sync::Arc<crate::domain_runtime::DomainRuntime>, std::sync::Arc<crate::v2_client_routes::Client>) {
        let home = std::env::temp_dir().join(format!("alondra-view-actions-{}", crate::auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        let domain = std::sync::Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let client = crate::v2_client_routes::Client::open(domain.clone(), &home, Default::default());
        (ActionHome(home), domain, client)
    }

    fn action_request(domain: &crate::domain_runtime::DomainRuntime, command_id: &str, lang: &str,
        action_id: String, option_id: Option<&str>, fields: Value) -> ActionRequest {
        ActionRequest {
            protocol: VIEWS_PROTOCOL,
            command_id: command_id.into(),
            data_epoch: domain.state().unwrap().data_epoch.0.clone(),
            client: "ios".into(),
            lang: lang.into(),
            action_id,
            option_id: option_id.map(str::to_owned),
            input: ActionInput {
                fields: fields.as_object().cloned().unwrap_or_default().into_iter().collect(),
                ..Default::default()
            },
        }
    }

    #[tokio::test]
    async fn an_answered_question_is_no_longer_available() {
        use alondra_core::domain_v2::{Recipient, Scope, QuestionOption};
        let (home, domain, client) = action_home();
        let user = Actor::User { user_id: "user".into() };
        apply_seed_command(&domain, Actor::System, "register-agent", Command::RegisterAgent {
            agent_id: "agent-a".into(), name: "Agent A".into(),
        });
        apply_seed_command(&domain, user.clone(), "create-request", Command::CreateRequest {
            request_id: "request".into(), body: "Work".into(), auto_merge: false,
        });
        apply_seed_command(&domain, user, "create-task", Command::CreateTask {
            task_id: "task".into(), request_id: "request".into(), title: "Task".into(),
            agent_id: Some("agent-a".into()), review_prs: false, dependencies: Default::default(), stack: None,
        });
        apply_seed_command(&domain, Actor::Agent { agent_id: "agent-a".into() }, "ask", Command::AskQuestion {
            attachments: vec![], interaction_id: "question".into(), scope: Scope::Task { task_id: "task".into() },
            recipient: Recipient::User, body: "Ship it?".into(), blocking: true,
            options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None, inputs: vec![] }], card: alondra_core::domain_v2::CardSpec::derived(alondra_core::domain_v2::CardKind::Provide, "Answer the question"),
        });
        let state = domain.state().unwrap();
        let revision = state.interactions[&"question".into()].revision;
        let action = ActionId::new(Verb::Ans, "question", revision, None).unwrap().to_string();
        let first = action_request(&domain, "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10", "en", action.clone(), Some("yes"), json!({}));
        let applied = execute_action(&domain, &client, &home.0, "test-device", first, None, || Ok(())).await.unwrap();
        assert_eq!(applied["outcome"], "applied", "{applied}");
        let revision_after_answer = domain.state().unwrap().revision;
        let second = action_request(&domain, "7a1d3c5e-2b4f-4a6c-8d9e-0f1a2b3c4d5e", "en", action, Some("yes"), json!({}));
        let stale = execute_action(&domain, &client, &home.0, "test-device", second, None, || Ok(())).await.unwrap();
        assert_eq!(stale["outcome"], "no_longer_available", "{stale}");
        assert_eq!(domain.state().unwrap().revision, revision_after_answer, "the second answer changes nothing");
    }

    fn presented_pr(domain: &crate::domain_runtime::DomainRuntime) {
        use alondra_core::domain_v2::DeliveryTarget;
        let user = Actor::User { user_id: "user".into() };
        let agent = Actor::Agent { agent_id: "agent-a".into() };
        apply_seed_command(domain, Actor::System, "register-agent", Command::RegisterAgent {
            agent_id: "agent-a".into(), name: "Agent A".into(),
        });
        apply_seed_command(domain, user.clone(), "create-request", Command::CreateRequest {
            request_id: "request".into(), body: "Work".into(), auto_merge: false,
        });
        apply_seed_command(domain, user, "create-task", Command::CreateTask {
            task_id: "task".into(), request_id: "request".into(), title: "Task".into(),
            agent_id: Some("agent-a".into()), review_prs: false, dependencies: Default::default(), stack: None,
        });
        apply_seed_command(domain, agent.clone(), "register-pr", Command::RegisterPr {
            task_id: "task".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "me/app".into(),
            number: 7, head: "0123456789abcdef".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
        });
        apply_seed_command(domain, agent, "present", Command::PresentResult {
            attachments: vec![], task_id: "task".into(), interaction_id: "result".into(), body: "Ready".into(),
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "0123456789abcdef".into() }],
            is_final: true, card: alondra_core::domain_v2::CardSpec::delivery("Review the result", &[], true),
        });
    }

    fn waits_for_the_user(domain: &crate::domain_runtime::DomainRuntime) -> bool {
        domain.state().unwrap().projection().inbox.iter().any(|item| item.pending)
    }

    #[tokio::test]
    async fn interrupting_a_node_writes_one_interrupt_down_its_connection_and_is_refused_when_it_is_gone() {
        let (home, domain, client) = action_home();
        apply_seed_command(&domain, Actor::System, "register-agent", Command::RegisterAgent {
            agent_id: "agent-a".into(), name: "Agent A".into(),
        });
        let interrupt = |command_id: &str| {
            let action = ActionId::new(Verb::Interrupt, "agent-a", domain.state().unwrap().revision, None).unwrap().to_string();
            action_request(&domain, command_id, "es", action, None, json!({}))
        };
        let refused = execute_action(&domain, &client, &home.0, "test-device", interrupt("0b6f9a52-3c1e-4d7a-9f20-6a1b2c3d4e5f"), None, || Ok(())).await.unwrap();
        assert_eq!((refused["outcome"].as_str(), refused["code"].as_str()), (Some("rejected"), Some("session_unreachable")), "{refused}");

        let node = crate::node_push::testing::Node::connect(&domain, "agent-a");
        let applied = execute_action(&domain, &client, &home.0, "test-device", interrupt("1c7f0b63-4d2f-4e8b-8a31-7b2c3d4e5f60"), None, || Ok(())).await.unwrap();
        assert_eq!((applied["outcome"].as_str(), applied["stamp"].as_str()), (Some("applied"), Some("Interrumpido")), "{applied}");
        tokio::task::yield_now().await;
        assert_eq!(node.interrupts(), 1);
    }

    #[tokio::test]
    async fn resolving_conflicts_or_asking_about_a_presented_pr_hands_it_back_to_its_node() {
        use alondra_core::domain_v2::{Feedback, Instruction};
        for (verb, text, author, instruction) in [(Verb::Conflicts, None, Feedback::ALONDRA, Instruction::Conflicts),
            (Verb::AskPr, Some("Why a new table?"), "device:test-device", Instruction::Question)] {
            let (home, domain, client) = action_home();
            presented_pr(&domain);
            assert!(waits_for_the_user(&domain));
            let action = ActionId::new(verb, "pr", domain.state().unwrap().revision, None).unwrap().to_string();
            let mut request = action_request(&domain, "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10", "es", action, None, json!({}));
            request.input.text = text.map(str::to_owned);
            let applied = execute_action(&domain, &client, &home.0, "test-device", request, None, || Ok(())).await.unwrap();
            assert_eq!(applied["outcome"], "applied", "{applied}");
            let state = domain.state().unwrap();
            let InteractionPayload::Delivery { feedback, .. } = &state.interactions[&"result".into()].payload else { unreachable!() };
            assert_eq!(feedback.iter().map(|f| (f.user_id.as_str(), f.instruction)).collect::<Vec<_>>(), [(author, instruction)]);
            assert_eq!(state.task_handed_back(&"task".into()), Some(instruction));
            assert!(!waits_for_the_user(&domain), "{verb:?} takes the card out of Waiting for You");
        }
    }

    #[tokio::test]
    async fn asking_for_more_context_answers_the_question_so_its_card_leaves() {
        use alondra_core::domain_v2::{Recipient, Scope, QuestionOption};
        let (home, domain, client) = action_home();
        presented_pr(&domain);
        apply_seed_command(&domain, Actor::Agent { agent_id: "agent-a".into() }, "ask", Command::AskQuestion {
            attachments: vec![], interaction_id: "question".into(), scope: Scope::Task { task_id: "task".into() },
            recipient: Recipient::User, body: "Which API?".into(), blocking: true,
            options: vec![QuestionOption { id: "new".into(), label: "New".into(), description: None, inputs: vec![] }], card: alondra_core::domain_v2::CardSpec::derived(alondra_core::domain_v2::CardKind::Provide, "Answer the question"),
        });
        let revision = domain.state().unwrap().interactions[&"question".into()].revision;
        let action = ActionId::new(Verb::Ctx, "question", revision, None).unwrap().to_string();
        let mut request = action_request(&domain, "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10", "en", action, None, json!({}));
        request.input.text = Some("the cost".into());
        let applied = execute_action(&domain, &client, &home.0, "test-device", request, None, || Ok(())).await.unwrap();
        assert_eq!((applied["outcome"].as_str(), applied["stamp"].as_str()), (Some("applied"), Some("Context requested")), "{applied}");
        let state = domain.state().unwrap();
        let InteractionPayload::Question { answer: Some(answer), .. } = &state.interactions[&"question".into()].payload else { unreachable!() };
        assert_eq!(answer.inputs["text"], alondra_core::domain_v2::InputValue::Text(context_request("the cost")));
        assert!(!state.projection().inbox.iter().any(|item| item.id == "interaction:question" && item.pending));
    }

    #[test]
    fn a_moved_pr_head_answers_changed_with_a_fresh_id() {
        use alondra_core::domain_v2::{PrState, PullRequest};
        let id = "github.com/bitomule/alondra#154";
        let mut state = alondra_core::domain_v2::DomainState::new("epoch");
        state.pull_requests.insert(id.into(), PullRequest {
            id: id.into(), task_id: "t-1".into(), host: "github.com".into(),
            repository: "bitomule/alondra".into(), number: 154,
            head: "bbbbbbbb2222".into(), base: "main".into(), state: PrState::Open,
            merged_head: None, allowed_methods: vec![MergeMethod::Merge], review: None,
        });
        let reviewed = ActionId::new(Verb::Changes, id, 1, Some("aaaaaaaa".into())).unwrap();
        let reply = changed_pr_head(&state, &reviewed, Lang::Es).unwrap();
        assert_eq!(reply["outcome"], "changed");
        assert_eq!(reply["confirm"]["button"], "Enviar igualmente");
        let fresh = ActionId::parse(reply["confirm"]["action_id"].as_str().unwrap()).unwrap();
        assert_eq!((fresh.verb, fresh.subject.as_str(), fresh.extra.as_deref()), (Verb::Changes, id, Some("bbbbbbbb")));
        assert_ne!(fresh, reviewed);
        assert!(changed_pr_head(&state, &fresh, Lang::Es).is_none(), "the fresh id is accepted as is");
    }

    #[tokio::test]
    async fn sched_save_rejects_with_the_field() {
        let (home, domain, client) = action_home();
        let save = ActionId::new(Verb::SchedSave, "view", domain.state().unwrap().revision, None).unwrap().to_string();
        let valid = json!({"name":"Morning", "mission":"Review work", "repeat":"daily", "time":"09:00"});
        let cases = [
            (json!({"name":"  "}), "name", "Enter a name of up to 120 characters.", "Escribe un nombre de hasta 120 caracteres."),
            (json!({"mission":""}), "mission", "Enter instructions of up to 20,000 characters.", "Escribe instrucciones de hasta 20.000 caracteres."),
            (json!({"time":"25:00"}), "time", "Choose a valid time.", "Elige una hora válida."),
            (json!({"repeat":"weekly", "weekday":null}), "weekday", "Choose a day of the week.", "Elige un día de la semana."),
            (json!({"repeat":"every_days", "interval":0, "start_at":1_800_000_000_000_i64}), "interval", "Choose an interval from 1 to 365.", "Elige un intervalo entre 1 y 365."),
            (json!({"repeat":"once", "start_at":5_000_000_000_000_i64}), "start_at", "Choose a date before 2100.", "Elige una fecha anterior al año 2100."),
            (json!({"disallowed_tools":"Bash(rm *)"}), "disallowed_tools", "Enter up to 100 tool patterns without spaces.", "Escribe hasta 100 patrones de herramientas sin espacios."),
        ];
        for (change, field, english, spanish) in cases {
            for (lang, message) in [("en", english), ("es", spanish)] {
                let mut fields = valid.as_object().unwrap().clone();
                fields.extend(change.as_object().unwrap().clone());
                let request = action_request(&domain, "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10", lang, save.clone(), None, Value::Object(fields));
                let revision = domain.state().unwrap().revision;
                let reply = execute_action(&domain, &client, &home.0, "test-device", request, None, || Ok(())).await.unwrap();
                assert_eq!(reply["outcome"], "rejected", "{reply}");
                assert_eq!(reply["field"], field, "{reply}");
                assert_eq!(reply["message"], message, "{reply}");
                assert_eq!(domain.state().unwrap().revision, revision, "a rejected save changes nothing");
            }
        }
    }

    #[test]
    fn action_route_is_an_exact_post() {
        assert!(approved_path("POST", "/actions"));
        assert!(!approved_path("GET", "/actions"));
        assert!(!approved_path("POST", "/actions?protocol=1"));
        assert!(!approved_path("POST", "/actions/"));
    }
}
