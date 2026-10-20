//! Resource-oriented domain API. Both local HTTP and encrypted transport call the
//! same runtime through `DomainHost`.
use crate::auth::{Signed, is_local};
use crate::domain_authority::Principal;
use crate::domain_store::{StoreError, TransportIdentity};
use crate::domain_runtime::DomainRuntime;
use std::path::Path as FsPath;
use alondra_core::domain::*;
use axum::extract::{ConnectInfo, Path, RawQuery, State, rejection::JsonRejection};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncReadExt;

/// What a domain router needs from its daemon.
pub(crate) trait DomainHost: Send + Sync + 'static {
    fn domain(&self) -> &DomainRuntime;
    fn home(&self) -> &FsPath;
    fn new_work_client(&self) -> Option<&crate::client_routes::Client> { None }
}

/// A local command carries what the producer or the terminal sends, not what
/// fits one relay response; the relay path keeps its own budget.
const LOCAL_COMMAND_BYTES: usize = 8 * 1024 * 1024;

pub fn routes<H: DomainHost>() -> Router<Arc<H>> {
    let router = Router::new()
        .route("/schedules", get(schedule_list::<H>).post(schedule_mutation::<H>))
        .route("/schedules/{id}", get(schedule_detail::<H>).put(schedule_mutation::<H>).delete(schedule_mutation::<H>))
        .route("/schedules/{id}/runs", post(schedule_mutation::<H>))
        .route("/capabilities", get(capabilities::<H>))
        .route("/changes", get(changes::<H>))
        .route("/delivery-attempts", get(delivery_evidence::<H>))
        .route("/pull-requests/{id}/diff", get(pull_request_diff::<H>))
        .route("/requests", post(request_resource::<H>))
        .route("/tasks/{id}", patch(resource_command::<H>))
        .route("/questions", post(resource_command::<H>))
        .route("/question-groups", post(resource_command::<H>))
        .route("/action-requests", post(resource_command::<H>))
        .route("/messages", post(resource_command::<H>))
        .route("/problems", post(resource_command::<H>))
        .route("/document-revisions/{id}/content", get(document_content::<H>));
    let mutations = [
        ("/requests/{id}/close", "POST"), ("/requests/{id}/title", "POST"),
        ("/requests/{request_id}/tasks", "POST"),
        ("/tasks/{id}/assignee", "PUT"),
        ("/tasks/{id}/dependencies", "PUT"), ("/tasks/{id}/relations", "PUT"), ("/tasks/{id}/priority", "PUT"), ("/tasks/{id}/verdict", "POST"),
        ("/tasks/{id}/start", "POST"),
        ("/tasks/{id}/documents", "POST"), ("/tasks/{id}/documents/{document_id}", "PUT"),
        ("/tasks/{id}/documents/{document_id}/review", "PUT"),
        ("/tasks/{id}/document-links", "POST"), ("/tasks/{id}/pull-requests", "POST"),
        ("/tasks/{id}/results", "POST"), ("/tasks/{id}/stack-readiness", "POST"),
        ("/tasks/{id}/completion", "POST"), ("/tasks/{id}/cancellation", "POST"),
        ("/tasks/{id}/reopening", "POST"), ("/tasks/{id}/plan-review", "POST"),
        ("/tasks/{id}/archival", "POST"), ("/tasks/{id}/cleanup", "POST"), ("/tasks/{id}/post-merge-work", "PUT"),
        ("/interactions/{id}/answers", "POST"), ("/interactions/{id}/settlement", "POST"),
        ("/interactions/{id}/withdrawal", "POST"), ("/interactions/{id}/clarification", "POST"),
        ("/interactions/{id}/subject", "PUT"),
        ("/interactions/{id}/feedback", "POST"), ("/interactions/{id}/acceptance", "POST"),
        ("/interactions/{id}/merge-authorizations", "POST"), ("/interactions/{id}/read-state", "PUT"),
    ];
    let router = mutations.into_iter().fold(router, |router, (path, method)| {
        let endpoint = match method {
            "POST" => post(resource_command::<H>),
            "PUT" => put(resource_command::<H>),
            "PATCH" => patch(resource_command::<H>),
            _ => unreachable!("resource command methods are explicit"),
        };
        router.route(path, endpoint
            .layer(axum::extract::DefaultBodyLimit::max(LOCAL_COMMAND_BYTES)))
    });
    router
}

async fn blocking_read<H: DomainHost>(host: Arc<H>, read: impl FnOnce(&H) -> Response + Send + 'static) -> Response {
    let work = host.domain().read_work.clone();
    work.run(move || read(&host)).await.unwrap_or_else(|_| storage_error())
}

fn schedule_read<H: DomainHost>(host: &H, path: &str) -> Response {
    crate::domain_schedule_api::response(crate::domain_schedule_api::read(host.domain(), path))
}
async fn schedule_list<H: DomainHost>(State(host): State<Arc<H>>, signed: Option<Extension<Signed>>) -> Response {
    blocking_read(host, move |host| {
        if !signed_read(host, &signed) { return unsigned_read(); }
        let response = schedule_read(host, "/schedules");
        if !signed_read(host, &signed) { return unsigned_read(); }
        response
    }).await
}
async fn schedule_detail<H: DomainHost>(State(host): State<Arc<H>>, signed: Option<Extension<Signed>>, Path(id): Path<String>) -> Response {
    blocking_read(host, move |host| {
        if !signed_read(host, &signed) { return unsigned_read(); }
        let response = schedule_read(host, &format!("/schedules/{id}"));
        if !signed_read(host, &signed) { return unsigned_read(); }
        response
    }).await
}
/// A paired device speaking over loopback is the user, exactly as it is over
/// the relay. The device's approval is rechecked under the writer lock.
async fn schedule_mutation<H: DomainHost>(
    State(host): State<Arc<H>>,
    signed: Option<Extension<Signed>>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let Some(device) = signed_device(&*host, &signed) else {
        return unsigned_read();
    };
    let bodyless = method == axum::http::Method::DELETE
        || (method == axum::http::Method::POST && uri.path().ends_with("/runs"));
    let body = match body {
        Ok(Json(body)) => Some(body),
        Err(_) if bodyless => None,
        Err(_) => return domain_error(DomainError::new("invalid_input", "invalid Schedule draft")),
    };
    let work = host.domain().command_work.clone();
    work.run(move || {
        let home = host.home().to_owned();
        crate::domain_schedule_api::response(crate::domain_schedule_api::mutation(
            host.domain(), host.home(), method.as_str(), uri.path(), body.as_ref(), None,
            || approved(&home, &device),
        ))
    }).await.unwrap_or_else(|_| storage_error())
}
async fn document_content<H: DomainHost>(
    State(host): State<Arc<H>>,
    signed: Option<Extension<Signed>>,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
) -> Response {
    if !signed_read(&*host, &signed) {
        return unsigned_read();
    }
    let response = document_content_inner(host.domain(), host.home(), id, query).await;
    if !signed_read(&*host, &signed) {
        return unsigned_read();
    }
    response
}

pub(crate) async fn document_content_inner(domain: &DomainRuntime, home: &FsPath, id: String, query: Option<String>) -> Response {
    let Some((offset, version)) = query.as_deref().and_then(document_content_query) else {
        return domain_error(DomainError::new(
            "invalid_input",
            "offset and version query are invalid",
        ));
    };
    let state = match domain.state() {
        Ok(state) => state,
        Err(_) => return storage_error(),
    };
    let Some(revision) = state.document_revisions.get(&DocumentRevisionId(id)) else {
        return domain_error(DomainError::new(
            "not_found",
            "document revision does not exist",
        ));
    };
    let reference = revision.content_reference.clone();
    let digest = revision.content_digest.clone();
    let response_digest = digest.clone();
    let revision_id = revision.id.clone();
    let home = home.to_path_buf();
    let reads = domain.stored_reads.clone();
    let chunk = tokio::task::spawn_blocking(move || {
        reads.verified(&home, &reference, &digest, offset, version)
    })
    .await;
    match chunk {
        Ok(Ok(mut chunk)) => {
            chunk["revision_id"] = json!(revision_id);
            chunk["content_digest"] = json!(response_digest);
            Json(chunk).into_response()
        }
        _ => domain_error(DomainError::new(
            "content_unavailable",
            "stored document content is unavailable",
        )),
    }
}

pub(crate) fn document_content_query(query: &str) -> Option<(u64, Option<String>)> {
    let (mut offset, mut version) = (None, None);
    for field in query.split('&') {
        let (key, value) = field.split_once('=')?;
        match key {
            "offset"
                if offset.is_none()
                    && !value.is_empty()
                    && value.bytes().all(|b| b.is_ascii_digit()) =>
            {
                offset = value.parse::<u64>().ok();
            }
            "version"
                if version.is_none()
                    && !value.is_empty()
                    && value.len() <= 256
                    && value.bytes().all(|b| b.is_ascii_digit() || b == b'-') =>
            {
                version = Some(value.to_owned());
            }
            _ => return None,
        }
    }
    let offset = offset?;
    if offset > 0 && version.is_none() {
        return None;
    }
    Some((offset, version))
}
async fn task_actions<H: DomainHost>(
    State(host): State<Arc<H>>,
    signed: Option<Extension<Signed>>,
    Path(id): Path<String>,
) -> Response {
    if !signed_read(&*host, &signed) {
        return unsigned_read();
    }
    let response = task_actions_inner(host.domain(), id);
    if !signed_read(&*host, &signed) {
        return unsigned_read();
    }
    response
}

pub(crate) fn task_actions_inner(domain: &DomainRuntime, id: String) -> Response {
    let state = match domain.state() {
        Ok(state) => state,
        Err(_) => return storage_error(),
    };
    match state.task_actions(&TaskId(id)) {
        Some(actions) => Json(json!({"protocol_version": PROTOCOL_VERSION,
            "data_epoch": state.data_epoch, "revision": state.revision,
            "actions": actions}))
        .into_response(),
        None => domain_error(DomainError::new("not_found", "task does not exist")),
    }
}
const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
const MAX_RELAY_DIFF_RESPONSE: usize = 120 * 1024;

async fn pull_request_diff<H: DomainHost>(
    State(host): State<Arc<H>>,
    signed: Option<Extension<Signed>>,
    Path(id): Path<String>,
) -> Response {
    if !signed_read(&*host, &signed) {
        return unsigned_read();
    }
    let response = pull_request_diff_inner(host.domain(), id).await;
    if !signed_read(&*host, &signed) {
        return unsigned_read();
    }
    response
}

pub(crate) async fn pull_request_diff_inner(domain: &DomainRuntime, id: String) -> Response {
    let state = match domain.state() {
        Ok(state) => state,
        Err(_) => return storage_error(),
    };
    let Some(pr) = state.pull_requests.get(&PrId(id)) else {
        return domain_error(DomainError::new("not_found", "pull request does not exist"));
    };
    // The registered host/repository/number is the identity. Never interpret a bare
    // numeric PR or an arbitrary URL supplied by a reader.
    let url = format!("https://{}/{}/pull/{}", pr.host, pr.repository, pr.number);
    let expected_head = pr.head.clone();
    let before = match live_head(&url).await {
        Ok(head) => head,
        Err(error) => return error,
    };
    if before != expected_head {
        return domain_error(DomainError::new("stale_head", "registered PR head changed"));
    }
    let (bytes, truncated) = match bounded_diff(&url).await {
        Ok(result) => result,
        Err(error) => return error,
    };
    let after = match live_head(&url).await {
        Ok(head) => head,
        Err(error) => return error,
    };
    if after != before {
        return domain_error(DomainError::new(
            "stale_head",
            "PR head changed while loading diff",
        ));
    }
    let mut diff = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_DIFF_BYTES)]).into_owned();
    if truncated {
        while diff.ends_with('\u{fffd}') {
            diff.pop();
        }
    }
    let mut body = json!({"pr_id":pr.id,"host":pr.host,"repository":pr.repository,
        "number":pr.number,"head":before,"diff":diff,"truncated":truncated});
    bound_diff_json(&mut body);
    Json(body).into_response()
}

pub(crate) fn bound_diff_json(body: &mut Value) {
    // Bound the *encoded JSON*, not source bytes: backslashes/control chars can
    // expand substantially when relayed. All readers receive an explicit
    // truncated flag rather than a transport-level 502.
    while serde_json::to_vec(body).is_ok_and(|encoded| encoded.len() > MAX_RELAY_DIFF_RESPONSE) {
        let Some(diff) = body.get("diff").and_then(Value::as_str) else {
            break;
        };
        if diff.is_empty() {
            break;
        }
        let mut cut = diff.len() * 3 / 4;
        while cut > 0 && !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        let replacement = diff[..cut].to_owned();
        body["diff"] = json!(replacement);
        body["truncated"] = json!(true);
    }
}

async fn bounded_diff(url: &str) -> Result<(Vec<u8>, bool), Response> {
    let mut child = tokio::process::Command::new("gh")
        .args(["pr", "diff", url])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| {
            domain_error(DomainError::new(
                "external_read_failed",
                "could not start PR diff",
            ))
        })?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut bytes = Vec::new();
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        stdout
            .take((MAX_DIFF_BYTES + 1) as u64)
            .read_to_end(&mut bytes),
    )
    .await;
    if !matches!(read, Ok(Ok(_))) {
        let _ = child.kill().await;
        return Err(domain_error(DomainError::new(
            "external_read_failed",
            "PR diff timed out",
        )));
    }
    let truncated = bytes.len() > MAX_DIFF_BYTES;
    if truncated {
        let _ = child.kill().await;
        let _ = child.wait().await;
    } else {
        let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
        if !matches!(status, Ok(Ok(status)) if status.success()) {
            return Err(domain_error(DomainError::new(
                "external_read_failed",
                "could not load PR diff",
            )));
        }
    }
    Ok((bytes, truncated))
}

async fn live_head(url: &str) -> Result<String, Response> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        tokio::process::Command::new("gh")
            .args(["pr", "view", url, "--json", "headRefOid"])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let output = match output {
        Ok(Ok(output)) if output.status.success() => output,
        _ => {
            return Err(domain_error(DomainError::new(
                "external_read_failed",
                "could not verify PR head",
            )));
        }
    };
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        domain_error(DomainError::new(
            "external_read_failed",
            "invalid PR head response",
        ))
    })?;
    value
        .get("headRefOid")
        .and_then(Value::as_str)
        .filter(|head| head.len() == 40 && head.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_owned)
        .ok_or_else(|| domain_error(DomainError::new("external_read_failed", "invalid PR head")))
}
async fn changes<H: DomainHost>(
    State(host): State<Arc<H>>,
    signed: Option<Extension<Signed>>,
    RawQuery(query): RawQuery,
) -> Response {
    if !signed_read(&*host, &signed) {
        return unsigned_read();
    }
    let response = match (host.new_work_client(), signed_device(&*host, &signed)) {
        (Some(client), Some(device)) => crate::views::feed::handle(client, &device.device_id,
            query.as_deref().unwrap_or(""), alondra_core::views::Transport::Loopback).await,
        _ => domain_error(DomainError::new("forbidden", "signed user device required")),
    };
    if !signed_read(&*host, &signed) {
        return unsigned_read();
    }
    response
}

async fn capabilities<H: DomainHost>(
    State(host): State<Arc<H>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    signed: Option<Extension<Signed>>,
    headers: HeaderMap,
) -> Response {
    blocking_read(host, move |host| {
        let bearer = headers.get("authorization").and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        let producer_current = || bearer.is_some_and(|token| signed.is_none()
            && is_local(&peer, &headers)
            && host.domain().resolve_bearer(token).is_ok());
        if !signed_read(host, &signed) && !producer_current() {
            return domain_error(DomainError::new("forbidden", "signed device or local producer capability required"));
        }
        let response = capabilities_supported(host.domain(), true);
        if !signed_read(host, &signed) && !producer_current() {
            return domain_error(DomainError::new("forbidden", "signed device or local producer capability required"));
        }
        response
    }).await
}

fn capabilities_inner(domain: &DomainRuntime) -> Response {
    capabilities_supported(domain, false)
}
fn capabilities_supported(domain: &DomainRuntime, operation_results: bool) -> Response {
    match domain.state() {
        Ok(state) => {
            let mut body = json!({"protocol_version": PROTOCOL_VERSION, "data_epoch": state.data_epoch, "revision": state.revision});
            if operation_results { body["operation_results"] = json!(true);body["operation_journal"] = json!(true); }
            Json(body).into_response()
        },
        Err(_) => storage_error(),
    }
}
/// Encrypted reads enter through a verified, still-approved device proof. They
/// never round-trip over the unsigned loopback HTTP listener.
pub(crate) async fn encrypted_read_runtime(
    domain: &DomainRuntime, home: &FsPath,
    approved: &crate::service::ApprovedDevice<'_>, path: &str,
) -> anyhow::Result<Response> {
    approved.recheck()?;
    let response = if crate::domain_schedule_api::approved_read_path(path) {
        crate::domain_schedule_api::response(crate::domain_schedule_api::read(domain, path))
    } else if let Some(query) = path.strip_prefix("/delivery-attempts?") {
        delivery_evidence_inner(domain, query)
    } else if path == "/capabilities" {
        capabilities_inner(domain)
    } else if let Some(id) = path.strip_prefix("/pull-requests/")
        .and_then(|suffix| suffix.strip_suffix("/diff")) {
        let Some(id) = decoded_pull_request_id(id) else {
            anyhow::bail!("unapproved encrypted domain read path");
        };
        pull_request_diff_inner(domain, id).await
    } else if let Some((id, query)) = path.strip_prefix("/document-revisions/")
        .and_then(|suffix| suffix.split_once("/content?")) {
        document_content_inner(domain, home, id.to_owned(), Some(query.to_owned())).await
    } else {
        anyhow::bail!("unapproved encrypted domain read path")
    };
    approved.recheck()?;
    Ok(response)
}

pub(crate) fn delivery_evidence_query(query: &str) -> Option<i64> {
    let value = query.strip_prefix("after=")?;
    if value.is_empty() || value.len() > 19 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse::<i64>().ok()
}

pub(crate) fn resolve_delivery_attempt_id(path: &str) -> Option<i64> {
    let value = path.strip_prefix("/delivery-attempts/")?
        .strip_suffix("/resolve")?;
    if value.is_empty() || value.len() > 19 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse::<i64>().ok().filter(|id| *id > 0)
}

async fn delivery_evidence<H: DomainHost>(
    State(host): State<Arc<H>>,
    signed: Option<Extension<Signed>>,
    RawQuery(query): RawQuery,
) -> Response {
    blocking_read(host, move |host| {
        if !signed_read(host, &signed) {
            return unsigned_read();
        }
        let response = delivery_evidence_inner(host.domain(), query.as_deref().unwrap_or_default());
        if !signed_read(host, &signed) {
            return unsigned_read();
        }
        response
    }).await
}

pub(crate) fn delivery_evidence_inner(domain: &DomainRuntime, query: &str) -> Response {
    let Some(after) = delivery_evidence_query(query) else {
        return domain_error(DomainError::new("invalid_input", "after cursor is invalid"));
    };
    let mut store = domain.store.lock().expect("domain store poisoned");
    let state = match store.load() {
        Ok(state) => state,
        Err(_) => return storage_error(),
    };
    let mut items = match store.delivery_evidence_page(after) {
        Ok(items) => items,
        Err(_) => return storage_error(),
    };
    let has_more = items.len() > 100;
    items.truncate(100);
    let next_after = items.last().map_or(after, |item| item.attempt_id);
    Json(json!({"protocol_version":PROTOCOL_VERSION,"data_epoch":state.data_epoch,
        "revision":state.revision,"attempts":items,"next_after":next_after,"has_more":has_more})).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum DeliveryDecision { Retry, Discard }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveDelivery {
    protocol_version: u32,
    data_epoch: DataEpoch,
    expected_version: u64,
    decision: DeliveryDecision,
}

pub(crate) fn encrypted_resolve_delivery_runtime(
    domain: &DomainRuntime, home: &FsPath, sender: &str, attempt_id: i64, body: Option<&Value>,
    identity: TransportIdentity<'_>, authorize: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<Value> {
    if sender != identity.0 || attempt_id < 1 {
        return Ok(json!({"status":422,"body":DomainError::new("invalid_input", "invalid delivery identity")}));
    }
    let Some(body) = body.cloned().and_then(|value| serde_json::from_value::<ResolveDelivery>(value).ok()) else {
        return Ok(json!({"status":422,"body":DomainError::new("invalid_input", "invalid delivery resolution")}));
    };
    if body.protocol_version != PROTOCOL_VERSION {
        return Ok(json!({"status":409,"body":DomainError::new("incompatible_protocol", "domain protocol 2 is required")}));
    }
    let mut store = domain.store.lock().expect("domain store poisoned");
    // Match DomainRuntime's lock order: domain store → approval lease. A
    // revocation cannot complete after this check but before the resolution
    // and its transport receipt commit.
    let _approval = crate::approval_lease::ApprovalLease::shared(home)?;
    if authorize().is_err() {
        return Ok(json!({"status":403,"body":DomainError::new("forbidden", "device approval was revoked")}));
    }
    let result = store.resolve_uncertain_authenticated(attempt_id, body.expected_version,
        &body.data_epoch.0, matches!(body.decision, DeliveryDecision::Retry), identity);
    Ok(match result {
        Ok(value) => json!({"status":200,"body":value}),
        Err(StoreError::Domain(error)) => json!({"status":status_for(&error).as_u16(),"body":error}),
        Err(StoreError::Storage(error)) => return Err(error),
    })
}
/// What the Agent of an auto-merge Task is told when it registers its PR.
pub(crate) fn auto_merge_notice(domain: &DomainRuntime, command: &Command) -> Option<String> {
    let Command::RegisterPr { task_id, .. } = command else { return None };
    let state = domain.state().ok()?;
    let task = state.tasks.get(task_id)?;
    if !state.requests.get(&task.request_id)?.auto_merge {
        return None;
    }
    Some("This task merges automatically: present the pull request as your final result with present_result, and once its checks pass Alondra merges it and tells you; do not merge it yourself.".into())
}

async fn resource_command<H: DomainHost>(
    State(host): State<Arc<H>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    signed: Option<Extension<Signed>>,
    headers: HeaderMap,
    method: axum::http::Method,
    uri: axum::http::Uri,
    Path(params): Path<HashMap<String, String>>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let Ok(Json(mut body)) = body else {
        return domain_error(DomainError::new("invalid_input", "invalid resource command body"));
    };
    if let Err(error) = check_launch_shape(method.as_str(), uri.path(), &body) {
        return domain_error(error);
    }
    if let Ok(state) = host.domain().state() {
        if let Err(error) = crate::pr_ids::assign(&state, method.as_str(), uri.path(), params.get("id").map(String::as_str), &mut body)
            .and_then(|()| crate::pr_heads::check(&state, method.as_str(), uri.path(), &mut body))
        {
            return domain_error(error);
        }
    }
    let Some(envelope) = resource_envelope(method.as_str(), uri.path(), &params, body) else {
        return domain_error(DomainError::new("invalid_input", "unsupported resource operation"));
    };
    execute_command(host, peer, signed, headers, envelope).await
}

/// A new Task's launch plan, read on its own so a malformed one is refused saying what is wrong
/// with it rather than as an unknown command.
pub(crate) fn check_launch_shape(method: &str, path: &str, body: &Value) -> Result<(), DomainError> {
    let creates_task = method == "POST" && path.starts_with("/requests/") && path.ends_with("/tasks");
    let settles_into_task = method == "POST" && path.starts_with("/interactions/") && path.ends_with("/settlement");
    let replans_task = method == "POST" && path.starts_with("/tasks/") && path.ends_with("/plan-review");
    let launch = match (creates_task, settles_into_task, replans_task) {
        (true, _, _) => body.get("launch"),
        (_, true, _) => body.get("decision").and_then(|decision| decision.get("launch")),
        (_, _, true) => body.get("outcome").and_then(|outcome| outcome.get("launch")),
        _ => None,
    };
    match launch.filter(|launch| !launch.is_null()) {
        Some(launch) => serde_json::from_value::<crate::launch_plans::LaunchPlan>(launch.clone()).map(drop)
            .map_err(|error| DomainError::new("invalid_input", format!("launch: {error}"))),
        None => Ok(()),
    }
}

/// `POST /requests` accepts the resource form for producers and the established
/// launch form for a paired User. Both persist through DomainRuntime in one
/// transaction; only the User form resolves launch options and issue context.
async fn request_resource<H: DomainHost>(
    State(host): State<Arc<H>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    signed: Option<Extension<Signed>>,
    headers: HeaderMap,
    method: axum::http::Method,
    uri: axum::http::Uri,
    Path(params): Path<HashMap<String, String>>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return domain_error(DomainError::new("invalid_input", "invalid Request body"));
    };
    if body.get("text").is_some() {
        if headers.contains_key("authorization") || !is_local(&peer, &headers) {
            return domain_error(DomainError::new("forbidden", "the launch form is available to a paired User only"));
        }
        let Some(device) = signed_device(&*host, &signed) else { return unsigned_read(); };
        let work = host.domain().external_work.clone();
        let runtime = tokio::runtime::Handle::current();
        return work.run(move || runtime.block_on(async move {
            let Some(client) = host.new_work_client() else { return storage_error(); };
            let user_id = format!("device:{}", device.device_id);
            let home = host.home().to_owned();
            match client.write(&user_id, crate::client_routes::Write::NewRequest, &body, None,
                &|| approved(&home, &device)).await {
                Ok(reply) => reply.into_response(),
                Err(_) => storage_error(),
            }
        })).await.unwrap_or_else(|_| storage_error());
    }
    if signed_device(&*host, &signed).is_some() {
        return domain_error(DomainError::new("invalid_input", "a paired User must submit the Request launch form"));
    }
    if !headers.contains_key("authorization") {
        return unsigned_read();
    }
    let Some(envelope) = resource_envelope(method.as_str(), uri.path(), &params, body) else {
        return domain_error(DomainError::new("invalid_input", "invalid Request resource"));
    };
    execute_command(host, peer, signed, headers, envelope).await
}

pub(crate) fn resource_envelope(
    method: &str,
    path: &str,
    params: &HashMap<String, String>,
    mut body: Value,
) -> Option<CommandEnvelope> {
    if params.values().any(|value| !valid_resource_id(value)) { return None; }
    let task = params.get("id");
    let request = params.get("request_id");
    let interaction = params.get("id");
    let doc = params.get("document_id");
    let (kind, path_field): (&str, Option<(&str, &str)>) = match (method, path) {
        ("POST", "/requests") => ("create_request", None),
        ("POST", "/questions") => ("ask_question", None),
        ("POST", "/question-groups") => ("ask_questions", None),
        ("POST", "/action-requests") => ("request_action", None),
        ("POST", "/messages") => ("publish_information", None),
        ("POST", "/problems") => ("raise_problem", None),
        ("POST", p) if p.starts_with("/requests/") && p.ends_with("/title") => ("title_request", Some(("request_id", params.get("id")?))),
        ("POST", p) if p.starts_with("/requests/") && p.ends_with("/close") => ("close_request", Some(("request_id", params.get("id")?))),
        ("POST", p) if p.starts_with("/requests/") && p.ends_with("/tasks") => ("create_task", Some(("request_id", request?))),
        ("PATCH", p) if p.starts_with("/tasks/") => ("retitle_task", Some(("task_id", task?))),
        ("PUT", p) if p.starts_with("/tasks/") && p.ends_with("/assignee") => ("assign_task", Some(("task_id", task?))),
        ("PUT", p) if p.starts_with("/tasks/") && p.ends_with("/dependencies") => ("set_dependencies", Some(("task_id", task?))),
        ("PUT", p) if p.starts_with("/tasks/") && p.ends_with("/relations") => ("set_relations", Some(("task_id", task?))),
        ("PUT", p) if p.starts_with("/tasks/") && p.ends_with("/priority") => ("set_priority", Some(("task_id", task?))),
        ("POST", p) if p.starts_with("/tasks/") && p.ends_with("/verdict") => ("record_verdict", Some(("task_id", task?))),
        ("POST", p) if p.starts_with("/tasks/") && p.ends_with("/start") => ("start_task", Some(("task_id", task?))),
        ("POST", p) if p.starts_with("/tasks/") && p.ends_with("/documents") => ("create_document", Some(("task_id", task?))),
        ("PUT", p) if p.starts_with("/tasks/") && p.ends_with("/review") => {
            let document_id = doc?;
            if body.get("document_id").is_some_and(|value| value.as_str() != Some(document_id)) { return None; }
            body["document_id"] = json!(document_id);
            ("decide_document_review", Some(("task_id", task?)))
        }
        ("PUT", p) if p.starts_with("/tasks/") && p.contains("/documents/") && !p.ends_with("/review") => {
            let id = params.get("id")?;
            let document_id = doc?;
            if body.get("document_id").is_some_and(|value| value.as_str() != Some(document_id)) { return None; }
            body["document_id"] = json!(document_id);
            ("revise_document", Some(("task_id", id)))
        }
        ("POST", p) if p.starts_with("/tasks/") && p.ends_with("/document-links") => ("link_task_document", Some(("task_id", task?))),
        ("POST", p) if p.starts_with("/tasks/") && p.ends_with("/pull-requests") => ("register_pr", Some(("task_id", task?))),
        ("POST", p) if p.starts_with("/tasks/") && p.ends_with("/results") => ("present_result", Some(("task_id", task?))),
        ("POST", p) if p.starts_with("/tasks/") && p.ends_with("/stack-readiness") => ("publish_stack_readiness", Some(("task_id", task?))),
        ("POST", p) if p.starts_with("/tasks/") && p.ends_with("/completion") => ("complete_task", Some(("task_id", task?))),
        ("POST", p) if p.starts_with("/tasks/") && p.ends_with("/cancellation") => ("cancel_task", Some(("task_id", task?))),
        ("POST", p) if p.starts_with("/tasks/") && p.ends_with("/reopening") => ("reopen_task", Some(("task_id", task?))),
        ("POST", p) if p.starts_with("/tasks/") && p.ends_with("/plan-review") => ("settle_plan_review", Some(("task_id", task?))),
        ("POST", p) if p.starts_with("/tasks/") && p.ends_with("/archival") => ("archive_task", Some(("task_id", task?))),
        ("POST", p) if p.starts_with("/tasks/") && p.ends_with("/cleanup") => ("confirm_cleanup", Some(("task_id", task?))),
        ("PUT", p) if p.starts_with("/tasks/") && p.ends_with("/post-merge-work") => ("declare_post_merge_work", Some(("task_id", task?))),
        ("POST", p) if p.starts_with("/interactions/") && p.ends_with("/answers") => ("answer_question", Some(("interaction_id", interaction?))),
        ("POST", p) if p.starts_with("/interactions/") && p.ends_with("/settlement") => ("settle_suggestion", Some(("interaction_id", interaction?))),
        ("POST", p) if p.starts_with("/interactions/") && p.ends_with("/withdrawal") => ("withdraw", Some(("interaction_id", interaction?))),
        ("POST", p) if p.starts_with("/interactions/") && p.ends_with("/clarification") => ("request_clarification", Some(("interaction_id", interaction?))),
        ("PUT", p) if p.starts_with("/interactions/") && p.ends_with("/subject") => ("link_interaction_subject", Some(("interaction_id", interaction?))),
        ("PUT", p) if p.starts_with("/interactions/") && p.ends_with("/read-state") => ("mark_read", Some(("interaction_id", interaction?))),
        ("POST", p) if p.starts_with("/interactions/") && p.ends_with("/feedback") => ("submit_feedback", Some(("presentation_id", interaction?))),
        ("POST", p) if p.starts_with("/interactions/") && p.ends_with("/acceptance") => ("accept_delivery", Some(("presentation_id", interaction?))),
        ("POST", p) if p.starts_with("/interactions/") && p.ends_with("/merge-authorizations") => ("authorize_merge", Some(("presentation_id", interaction?))),
        _ => return None,
    };
    let object = body.as_object_mut()?;
    if object.contains_key("kind") || object.contains_key("protocol_version") { return None; }
    let data_epoch = object.get("data_epoch")?.clone();
    let command_id = object.get("command_id")?.clone();
    let expected_revision = object.get("expected_revision")?.clone();
    if let Some((field, value)) = path_field {
        if object.get(field).is_some_and(|existing| existing.as_str() != Some(value)) {
            return None;
        }
        object.insert(field.into(), json!(value));
    }
    object.insert("kind".into(), json!(kind));
    let mut envelope = body.clone();
    let envelope_body = envelope.as_object_mut()?;
    for key in ["data_epoch", "command_id", "expected_revision"] { envelope_body.remove(key); }
    serde_json::from_value(json!({
        "protocol_version": 2,
        "data_epoch": data_epoch,
        "command_id": command_id,
        "expected_revision": expected_revision,
        "command": envelope,
    })).ok()
}

pub(crate) fn resource_envelope_from_path(method: &str, path: &str, body: Value) -> Option<CommandEnvelope> {
    let segments: Vec<_> = path.strip_prefix('/')?.split('/').collect();
    let mut params = HashMap::new();
    match segments.as_slice() {
        ["requests", id, "close" | "title"] => { params.insert("id".into(), decode_path_id(id)?); }
        ["requests", request_id, "tasks"] => { params.insert("request_id".into(), decode_path_id(request_id)?); }
        ["tasks", id, "documents", document_id] => {
            params.insert("id".into(), decode_path_id(id)?);
            params.insert("document_id".into(), decode_path_id(document_id)?);
        }
        ["tasks", id, "documents", document_id, "review"] => {
            params.insert("id".into(), decode_path_id(id)?);
            params.insert("document_id".into(), decode_path_id(document_id)?);
        }
        ["tasks", id, ..] | ["interactions", id, ..] => { params.insert("id".into(), decode_path_id(id)?); }
        _ => {}
    }
    resource_envelope(method, path, &params, body)
}

pub(crate) fn is_resource_mutation_path(method: &str, path: &str) -> bool {
    let segments: Vec<_> = path.strip_prefix('/').unwrap_or_default().split('/').collect();
    let registered = match (method, segments.as_slice()) {
        ("POST", ["requests"] | ["questions"] | ["question-groups"] | ["action-requests"] | ["messages"] | ["problems"])
        | ("POST", ["requests", _, "close"] | ["requests", _, "title"] | ["requests", _, "tasks"])
        | ("POST", ["tasks", _, "verdict"] | ["tasks", _, "start"] | ["tasks", _, "documents"] | ["tasks", _, "document-links"]
            | ["tasks", _, "pull-requests"] | ["tasks", _, "results"] | ["tasks", _, "stack-readiness"]
            | ["tasks", _, "completion"] | ["tasks", _, "cancellation"] | ["tasks", _, "reopening"] | ["tasks", _, "archival"] | ["tasks", _, "cleanup"]
            | ["interactions", _, "answers"] | ["interactions", _, "withdrawal"] | ["interactions", _, "clarification"]
            | ["interactions", _, "feedback"] | ["interactions", _, "acceptance"] | ["interactions", _, "merge-authorizations"]) => true,
        ("PUT", ["tasks", _, "assignee"] | ["tasks", _, "post-merge-work"] | ["tasks", _, "dependencies"] | ["tasks", _, "relations"] | ["tasks", _, "priority"]
            | ["tasks", _, "documents", _] | ["tasks", _, "documents", _, "review"]
            | ["interactions", _, "subject"] | ["interactions", _, "read-state"]) => true,
        ("PATCH", ["tasks", _]) => true,
        _ => false,
    };
    registered && match segments.as_slice() {
        ["requests"] | ["questions"] | ["question-groups"] | ["action-requests"] | ["messages"] | ["problems"] => true,
        ["requests", id, ..] => valid_resource_id(id),
        ["tasks", id, ..] | ["interactions", id, ..] => valid_resource_id(id),
        _ => false,
    }
}

pub(crate) fn approved_resource_read_path(path: &str) -> bool {
    crate::domain_schedule_api::approved_read_path(path)
        || path == "/capabilities"
        || path.strip_prefix("/delivery-attempts?").is_some_and(|query| delivery_evidence_query(query).is_some())
        || path.strip_prefix("/pull-requests/").and_then(|s| s.strip_suffix("/diff")).is_some_and(encoded_pull_request_id)
        || path.strip_prefix("/document-revisions/").and_then(|s| s.split_once("/content?"))
            .is_some_and(|(id, query)| valid_resource_id(id) && document_content_query(query).is_some())
}

fn valid_resource_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn decode_path_id(value: &str) -> Option<String> {
    percent_decoded(value).filter(|decoded| valid_resource_id(decoded))
}

pub(crate) fn encoded_pull_request_id(raw: &str) -> bool {
    decoded_pull_request_id(raw).is_some()
}

/// A PR id is the registered `host/owner/repo#number`, so it travels percent-encoded once.
pub(crate) fn decoded_pull_request_id(raw: &str) -> Option<String> {
    let id = percent_decoded(raw)?;
    (alondra_core::views::id::valid(&id) && alondra_core::views::id::path_segment(&id) == raw).then_some(id)
}

/// Path segments are percent-encoded (RFC 3986 §2.1); the caller checks the decoded id's grammar.
fn percent_decoded(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let pair = bytes.get(index + 1..index + 3)?;
            if !pair.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            decoded.push(u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

async fn execute_command<H: DomainHost>(
    host: Arc<H>, peer: SocketAddr, signed: Option<Extension<Signed>>, headers: HeaderMap,
    envelope: CommandEnvelope,
) -> Response {
    if let Some(header) = headers.get("authorization") {
        if signed.is_some() || !is_local(&peer, &headers) {
            return domain_error(DomainError::new(
                "forbidden",
                "producer capabilities are local only",
            ));
        }
        let Some(token) = header.to_str().ok().and_then(|value| value.strip_prefix("Bearer ")) else {
            return domain_error(DomainError::new("forbidden", "invalid domain capability"));
        };
        let token = token.to_owned();
        let resolving = host.clone();
        let credential = token.clone();
        let principal = match host.domain().producer_work.run(move || resolving.domain().resolve_bearer(&credential)).await {
            Ok(Ok(principal)) => principal,
            Ok(Err(error)) => return store_error(error),
            Err(_) => return storage_error(),
        };
        if let Some(client) = host.new_work_client() {
            if let Err(error) = client.verify_presentation(&envelope.command).await {
                return domain_error(error);
            }
        }
        if let (Principal::Agent { session_id, .. }, Command::PresentResult { task_id, interaction_id, targets, is_final: true, .. })
            = (&principal, &envelope.command)
            && let Ok(state) = host.domain().state()
            && !state.interactions.contains_key(interaction_id)
            && let Some(refusal) = crate::worktree::final_refusal(&state, session_id, task_id, targets).await
        {
            return domain_error(refusal);
        }
        if let (Principal::Agent { session_id, .. }, Command::PresentResult { task_id, interaction_id, checklist, is_final: true, .. }) = (&principal, &envelope.command)
            && !checklist.is_empty()
            && let Ok(state) = host.domain().state()
            && !state.interactions.contains_key(interaction_id)
            && let Some(refusal) = crate::worktree::evidence_refusal(&state, session_id, task_id, checklist).await
        {
            return domain_error(refusal);
        }
        if let (Principal::Agent { .. }, Command::ConfirmCleanup { task_id, discard }) = (&principal, &envelope.command)
            && let Ok(state) = host.domain().state()
            && let Some(task) = state.tasks.get(task_id)
        {
            let left: Vec<(String, String)> = crate::task_worktrees::leftovers(task).await.into_iter()
                .filter(|(path, _)| !discard.contains(path))
                .collect();
            if !left.is_empty() {
                let mut refusal = DomainError::new("worktree_not_clean", crate::task_worktrees::settle_text(task_id, &left));
                refusal.subject = Some("discard".into());
                return domain_error(refusal);
            }
        }
        if let (Principal::Agent { .. }, Command::PresentResult { targets, is_final: true, .. }) = (&principal, &envelope.command)
            && let Ok(state) = host.domain().state()
            && let Some(refusal) = crate::domain_merges::draft_refusal(&state, targets, &crate::domain_github::GhCli::new()).await
        {
            return domain_error(refusal);
        }
        let work = host.domain().producer_work.clone();
        let mut envelope = envelope;
        return work.run(move || {
            if let Err(error) = check_launch_plan(host.domain(), host.home(), &envelope.command) {
                return domain_error(error);
            }
            let kept = match crate::result_files::keep(host.home(), &mut envelope.command) {
                Ok(kept) => kept,
                Err(error) => return domain_error(error),
            };
            let executed = host.domain().execute_bearer(&token, &envelope);
            if executed.is_err() && let Some(file) = kept {
                let _ = std::fs::remove_file(file);
            }
            match executed {
                Ok(outcome) => {
                    let mut answer = json!(outcome);
                    if let Some(notice) = auto_merge_notice(host.domain(), &envelope.command) {
                        answer["notice"] = json!(notice);
                    }
                    Json(answer).into_response()
                }
                Err(error) => store_error(error),
            }
        }).await.unwrap_or_else(|_| storage_error());
    }
    if headers.contains_key("x-alondra-session")
        || headers.contains_key("x-alondra-orchestrator-execution")
    {
        return domain_error(DomainError::new(
            "incompatible_protocol",
            "this producer must resume with a domain capability",
        ));
    }
    // A device paired over loopback is the user, locally as over the relay.
    let Some(device) = signed_device(&*host, &signed) else {
        return unsigned_read();
    };
    let work = host.domain().command_work.clone();
    work.run(move || {
        let home = host.home().to_owned();
        let result = host.domain().execute(
            &Actor::User { user_id: format!("device:{}", device.device_id) },
            &envelope,
            None,
            || approved(&home, &device)
                .map_err(|_| DomainError::new("forbidden", "device approval was revoked").into()),
        );
        match result {
            Ok(outcome) => Json(outcome).into_response(),
            Err(error) => store_error(error),
        }
    }).await.unwrap_or_else(|_| storage_error())
}

/// The exact device the middleware verified, only while its approval persists.
fn signed_device<H: DomainHost>(host: &H, signed: &Option<Extension<Signed>>) -> Option<Signed> {
    let Some(Extension(signed)) = signed else {
        return None;
    };
    approved(host.home(), signed).ok().map(|()| signed.clone())
}

fn approved(home: &FsPath, device: &Signed) -> anyhow::Result<()> {
    let auth = crate::auth::Auth::open(home)?;
    anyhow::ensure!(auth.list().iter().any(|entry| entry.id == device.device_id
        && entry.public_key == device.public_key), "device approval was revoked");
    Ok(())
}

/// Sender is the verified envelope identity; a body cannot change this principal.
/// The caller retains its ciphertext journal and rechecks device approval at commit.
pub(crate) fn encrypted_command_runtime(
    domain: &DomainRuntime, sender: &str, body: Option<&Value>,
    identity: TransportIdentity<'_>, authorize: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<Value> {
    if sender != identity.0 {
        return Ok(json!({"status":422,"body":DomainError::new("invalid_input", "invalid encrypted sender identity")}));
    }
    let envelope: CommandEnvelope = match body
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
    {
        Some(envelope) => envelope,
        None => {
            return Ok(
                json!({"status":422,"body":DomainError::new("invalid_input", "invalid domain command envelope")}),
            );
        }
    };
    let result = domain.execute(
        &Actor::User {
            user_id: format!("device:{sender}"),
        },
        &envelope,
        Some(identity),
        || {
            authorize()
                .map_err(|_| DomainError::new("forbidden", "device approval was revoked").into())
        },
    );
    Ok(match result {
        Ok(outcome) => json!({"status":200,"body":outcome}),
        Err(StoreError::Domain(error)) => {
            json!({"status":status_for(&error).as_u16(),"body":error})
        }
        Err(StoreError::Storage(error)) => return Err(error),
    })
}

pub(crate) async fn encrypted_resource_command_runtime(
    domain: &DomainRuntime, new_work: Option<&crate::client_routes::Client>,
    sender: &str, method: &str, path: &str, body: Option<&Value>,
    identity: TransportIdentity<'_>, authorize: impl Fn() -> anyhow::Result<()> + Sync,
) -> anyhow::Result<Value> {
    if sender != identity.0 {
        return Ok(json!({"status":422,"body":DomainError::new("invalid_input", "invalid encrypted sender identity")}));
    }
    resource_command_runtime(domain, new_work, sender, method, path, body, Some(identity), authorize).await
}

pub(crate) async fn resource_command_runtime(
    domain: &DomainRuntime, new_work: Option<&crate::client_routes::Client>,
    sender: &str, method: &str, path: &str, body: Option<&Value>,
    identity: Option<TransportIdentity<'_>>, authorize: impl Fn() -> anyhow::Result<()> + Sync,
) -> anyhow::Result<Value> {
    if method == "POST" && path == "/requests" && body.is_some_and(|body| body.get("text").is_some()) {
        let Some(client) = new_work else { anyhow::bail!("request launch support unavailable"); };
        let Some(body) = body else { anyhow::bail!("request body required"); };
        let reply = client.write(&format!("device:{sender}"), crate::client_routes::Write::NewRequest,
            body, identity, &authorize).await?;
        return Ok(reply.into_value());
    }
    let envelope = body.cloned()
        .and_then(|value| resource_envelope_from_path(method, path, value))
        .ok_or_else(|| DomainError::new("invalid_input", "invalid resource command"));
    let Ok(envelope) = envelope else {
        return Ok(json!({"status":422,"body":DomainError::new("invalid_input", "invalid resource command")}));
    };
    let result = domain.execute(
        &Actor::User { user_id: format!("device:{sender}") }, &envelope, identity,
        || authorize().map_err(|_| DomainError::new("forbidden", "device approval was revoked").into()),
    );
    Ok(match result {
        Ok(outcome) => json!({"status":200,"body":outcome}),
        Err(StoreError::Domain(error)) => json!({"status":status_for(&error).as_u16(),"body":error}),
        Err(StoreError::Storage(error)) => return Err(error),
    })
}
pub(crate) fn status_for(error: &DomainError) -> StatusCode {
    match error.code.as_str() {
        "forbidden" | "not_assigned" => StatusCode::FORBIDDEN,
        "not_found" => StatusCode::NOT_FOUND,
        "invalid_input" | "invalid_action_for_kind" | "launch_plan_required" => StatusCode::UNPROCESSABLE_ENTITY,
        "payload_too_large" => StatusCode::PAYLOAD_TOO_LARGE,
        "storage_unavailable" => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::CONFLICT,
    }
}
pub(crate) fn domain_error(error: DomainError) -> Response {
    (status_for(&error), Json(error)).into_response()
}
fn unsigned_read() -> Response {
    domain_error(DomainError::new("forbidden", "signed user device required"))
}
fn signed_read<H: DomainHost>(host: &H, signed: &Option<Extension<Signed>>) -> bool {
    signed_device(host, signed).is_some()
}
fn storage_error() -> Response {
    domain_error(DomainError::new(
        "storage_unavailable",
        "domain transaction could not be completed",
    ))
}
/// Whether the launch plan of a new Task is one a launch would accept: its folder exists and its
/// model and effort fit the request. A Task without a plan is refused by the domain itself.
pub(crate) fn check_launch_plan(domain: &DomainRuntime, home: &std::path::Path, command: &Command) -> Result<(), DomainError> {
    match command {
        Command::CreateTask { request_id, launch: Some(plan), .. }
        | Command::SettleSuggestion { decision: SuggestionDecision::NewTask { request_id, launch: plan, .. }, .. } =>
            crate::launch_plans::validate(domain, home, request_id, plan),
        Command::SettlePlanReview { task_id, outcome: alondra_core::domain::PlanReviewOutcome::PlanChanged { launch: plan, .. }, .. } => {
            let state = domain.state().map_err(|_| DomainError::new("storage_unavailable", "domain state unavailable"))?;
            let task = state.tasks.get(task_id).ok_or_else(|| DomainError::new("not_found", "task does not exist"))?;
            crate::launch_plans::validate(domain, home, &task.request_id, plan)
        }
        _ => Ok(()),
    }
}
fn store_error(error: StoreError) -> Response {
    match error {
        StoreError::Domain(error) => domain_error(error),
        StoreError::Storage(_) => storage_error(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_authority::Principal;
    use base64::{
        Engine as _,
        engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    };
    use p256::ecdsa::{Signature, SigningKey, signature::Signer};
    use sha2::{Digest as _, Sha256};
    use std::collections::BTreeSet;

    #[test]
    fn runtime_progress_producer_capability_resolution_and_commit_run_off_worker() {
        let directory = crate::test_support::Directory::new();
        std::fs::create_dir_all(&directory).unwrap();
        drop(DomainRuntime::open(&directory).unwrap());
        let host = crate::domain_bootstrap::DomainDaemon::open(&directory).unwrap();
        let domain = host.domain();
        let state = domain.state().unwrap();
        domain.execute(&Actor::User { user_id: "fixture".into() }, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "request".into(), expected_revision: state.revision,
            command: Command::CreateRequest { request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false },
        }, None, || Ok(())).unwrap();
        let token = domain.issue(Principal::Orchestrator { generation: 0, execution_id: "fixture".into() }).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {}", token.expose_for_child_environment()).parse().unwrap());
        let state = domain.state().unwrap();
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "task".into(), expected_revision: state.revision,
            command: Command::CreateTask { plan_items: None, task_id: "task".into(), request_id: "request".into(), title: "Work".into(), agent_id: None,
                review_prs: false, dependencies: Default::default(), stack: None, launch: Some(serde_json::from_value(json!({"name":"node", "cwd":directory.to_string_lossy(), "model":"sonnet", "reason":"fixture"})).unwrap()) },
        };
        let driver = std::thread::current().id();
        let threads = Arc::new(std::sync::Mutex::new(Vec::new()));
        let running = threads.clone();
        domain.producer_work.on_start(move || {
            let thread = std::thread::current().id();
            assert_ne!(thread, driver, "producer authority/store work occupies the async worker");
            running.lock().unwrap().push(thread);
        });
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let reply = execute_command(host, "127.0.0.1:12345".parse().unwrap(), None, headers, envelope).await;
            assert_eq!(reply.status(), StatusCode::OK);
        });
        assert_eq!(threads.lock().unwrap().len(), 2, "both capability resolution and the actual command use the producer lane");
        assert!(domain.state().unwrap().tasks.contains_key(&"task".into()));
    }

    #[tokio::test]
    async fn committed_local_changes_accepts_only_views_queries() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-changes-dispatch-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir(&home).unwrap();
        drop(DomainRuntime::open(&home).unwrap());
        let auth = crate::auth::Auth::open(&home).unwrap();
        let key = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let device = auth.pair(&auth.new_pairing_code(), "test-device",
            &STANDARD.encode(key.verifying_key().to_sec1_bytes())).unwrap();
        let signed = Some(Extension(Signed { device_id: device.id,
            name: device.name, public_key: device.public_key }));
        let host = crate::domain_bootstrap::DomainDaemon::open(&home).unwrap();
        let epoch = host.domain().state().unwrap().data_epoch.0.clone();

        let legacy = changes::<crate::domain_bootstrap::DomainDaemon>(State(host.clone()), signed.clone(),
            RawQuery(Some(format!("data_epoch={epoch}&after=0&wait=0")))).await;
        assert_eq!(legacy.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let views = changes::<crate::domain_bootstrap::DomainDaemon>(State(host.clone()), signed,
            RawQuery(Some("protocol=1&client=tui&lang=en&wait=0".into()))).await;
        assert_eq!(views.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(views.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["protocol"], 1);
        assert_eq!(body["superseded"], false);

        let unsigned = changes::<crate::domain_bootstrap::DomainDaemon>(State(host.clone()), None,
            RawQuery(Some("protocol=1&client=tui&lang=en&wait=0".into()))).await;
        assert_eq!(unsigned.status(), StatusCode::FORBIDDEN);
        drop(host);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn delivery_recovery_routes_accept_only_bounded_exact_ids_and_cursors() {
        assert_eq!(delivery_evidence_query("after=0"), Some(0));
        assert_eq!(delivery_evidence_query("after=42"), Some(42));
        assert_eq!(resolve_delivery_attempt_id("/delivery-attempts/42/resolve"), Some(42));
        for query in ["", "after=-1", "after=1&other=2", "after=999999999999999999999"] {
            assert_eq!(delivery_evidence_query(query), None);
        }
        for path in ["/delivery-attempts/0/resolve",
            "/delivery-attempts/1/resolve?retry=1",
            "/delivery-attempts/../resolve"] {
            assert_eq!(resolve_delivery_attempt_id(path), None);
        }
    }
    #[test]
    fn encrypted_delivery_resolution_requires_authorized_exact_decision_and_replays() {
        let (home, host) = fixture();
        let _directory = host.test_directory.clone();
        let mut store = host.domain.store.lock().unwrap();
        let message = store.pending_messages().unwrap().into_iter()
            .find(|item| item.recipient_kind == "user").unwrap();
        let claim = store.claim_message(message.id).unwrap().unwrap();
        store.finish_message(claim.attempt_id, claim.version,
            alondra_core::delivery::State::Uncertain, Some("push_unknown")).unwrap();
        drop(store);
        let epoch = host.domain.state().unwrap().data_epoch.clone();
        let body = json!({"protocol_version":2,"data_epoch":epoch,"expected_version":2,
            "decision":"retry"});
        let digest = [31u8; 32];
        let denied = encrypted_resolve_delivery_runtime(&host.domain, &host.home, "phone-a", claim.attempt_id,
            Some(&body), ("phone-a", "work-a", &digest), || anyhow::bail!("revoked")).unwrap();
        assert_eq!(denied["status"], 403);
        let first = encrypted_resolve_delivery_runtime(&host.domain, &host.home, "phone-a", claim.attempt_id,
            Some(&body), ("phone-a", "work-a", &digest), || Ok(())).unwrap();
        assert_eq!(first["status"], 200);
        assert_eq!(first["body"]["state"], "retryable");
        assert_eq!(first, encrypted_resolve_delivery_runtime(&host.domain, &host.home, "phone-a", claim.attempt_id,
            Some(&body), ("phone-a", "work-a", &digest), || Ok(())).unwrap());
        let changed = encrypted_resolve_delivery_runtime(&host.domain, &host.home, "phone-a", claim.attempt_id,
            Some(&json!({"protocol_version":2,"data_epoch":epoch,"expected_version":2,
                "decision":"discard"})), ("phone-a", "work-a", &[32u8; 32]), || Ok(())).unwrap();
        assert_eq!(changed["body"]["code"], "idempotency_conflict");
        let response = delivery_evidence_inner(&host.domain, "after=0");
        assert_eq!(response.status(), StatusCode::OK);
        drop(host);
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn revocation_waits_for_delivery_resolution_receipt_commit() {
        use std::sync::mpsc;
        use std::time::Duration;

        let (home, host) = fixture();

        let _directory = host.test_directory.clone();
        let auth = crate::auth::Auth::open(&home).unwrap();
        let key = SigningKey::from_slice(&[77u8; 32]).unwrap();
        let code = auth.new_pairing_code();
        let device = auth.pair(&code, "Recovery phone",
            &STANDARD.encode(key.verifying_key().to_sec1_bytes())).unwrap();
        let mut store = host.domain.store.lock().unwrap();
        let message = store.pending_messages().unwrap().into_iter()
            .find(|item| item.recipient_kind == "user").unwrap();
        let claim = store.claim_message(message.id).unwrap().unwrap();
        store.finish_message(claim.attempt_id, claim.version,
            alondra_core::delivery::State::Uncertain, Some("push_unknown")).unwrap();
        drop(store);
        let epoch = host.domain.state().unwrap().data_epoch.clone();
        let body = json!({"protocol_version":2,"data_epoch":epoch,"expected_version":2,
            "decision":"retry"});
        let (checked_tx, checked_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let writer_home = home.clone();
        let writer_host = host.clone();
        let writer_id = device.id.clone();
        let writer = std::thread::spawn(move || {
            encrypted_resolve_delivery_runtime(&writer_host.domain, &writer_host.home, &writer_id, claim.attempt_id,
                Some(&body), (&writer_id, "recovery-work", &[31u8; 32]), || {
                    anyhow::ensure!(crate::auth::Auth::open(&writer_home)?.list().iter()
                        .any(|entry| entry.id == writer_id));
                    checked_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                    Ok(())
                }).unwrap()
        });
        checked_rx.recv().unwrap();
        let (revoked_tx, revoked_rx) = mpsc::channel();
        let revoker_home = home.clone();
        let revoke_id = device.id.clone();
        let revoker = std::thread::spawn(move || {
            revoked_tx.send(crate::auth::Auth::open(&revoker_home).unwrap()
                .revoke(&revoke_id).unwrap()).unwrap();
        });
        assert!(revoked_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "revocation returned before the recovery commit");
        resume_tx.send(()).unwrap();
        assert_eq!(writer.join().unwrap()["status"], 200);
        assert!(revoked_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        revoker.join().unwrap();
        assert!(!crate::auth::Auth::open(&home).unwrap().list().iter()
            .any(|entry| entry.id == device.id));
        drop(host);
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn serialized_diff_fits_encrypted_relay_budget_and_reports_truncation() {
        let mut body = json!({"pr_id":"pr-a","host":"github.com","repository":"owner/repo",
            "number":1,"head":"a".repeat(40),"diff":"\\\n\t".repeat(100_000),
            "truncated":false});
        bound_diff_json(&mut body);
        assert!(body["truncated"].as_bool().unwrap());
        assert!(serde_json::to_vec(&body).unwrap().len() <= MAX_RELAY_DIFF_RESPONSE);
        assert!(body["diff"].as_str().unwrap().chars().count() > 0);
    }
    fn envelope(host: &Host, id: &str, command: Command) -> CommandEnvelope {
        let state = host.domain.state().unwrap();
        CommandEnvelope {
            protocol_version: 2,
            data_epoch: state.data_epoch.clone(),
            command_id: id.into(),
            expected_revision: state.revision,
            command,
        }
    }

    #[test]
    fn resource_router_constructs_without_method_collisions() {
        let _ = routes::<crate::domain_bootstrap::DomainDaemon>();
    }

    #[test]
    fn resource_mutation_allowlist_tracks_the_registered_methods() {
        for (method, path) in [
            ("POST", "/requests"), ("POST", "/requests/r-1/tasks"),
            ("POST", "/questions"), ("POST", "/action-requests"), ("POST", "/messages"), ("POST", "/problems"),
            ("PATCH", "/tasks/t-1"), ("PUT", "/tasks/t-1/assignee"),
            ("POST", "/tasks/t-1/results"), ("PUT", "/interactions/i-1/subject"), ("POST", "/tasks/t-1/cleanup"), ("PUT", "/tasks/t-1/post-merge-work"),
            ("POST", "/schedules"),
        ] {
            let schedule = crate::domain_schedule_api::approved_mutation_path(method, path, true);
            assert!(is_resource_mutation_path(method, path) || schedule, "{method} {path}");
        }
        assert!(!is_resource_mutation_path("POST", "/commands"));
        assert!(!is_resource_mutation_path("POST", "/merge-authorizations/claim"),
            "semantic merge resolution is local producer authority, never a relay route");
        assert!(!is_resource_mutation_path("POST", "/tasks/t-1/merge-authorizations/auth-1/claim"));
        for (method, path) in [
            ("GET", "/questions"), ("PUT", "/questions"),
            ("POST", "/questions/extra"), ("POST", "/action-requests/extra"),
            ("POST", "/messages/extra"),
        ] {
            assert!(!is_resource_mutation_path(method, path), "{method} {path}");
        }
        assert!(!is_resource_mutation_path("POST", "/tasks/t-1/unknown"));
        assert!(!approved_resource_read_path("/"));
        assert!(!approved_resource_read_path("/tasks?limit=20"));
        assert!(!approved_resource_read_path("/tasks?unknown=1"));
        assert!(!approved_resource_read_path("/merge-authorizations/auth-1"));
        assert!(approved_resource_read_path("/pull-requests/github.com%2Fowner%2Frepo%23154/diff"));
        assert!(approved_resource_read_path("/delivery-attempts?after=0"));
        assert!(!approved_resource_read_path("/delivery-attempts?after=0&all=1"));
        assert_eq!(
            decoded_pull_request_id("github.com%2Fowner%2Frepo%23154")
                .as_deref(),
            Some("github.com/owner/repo#154")
        );
        assert!(
            decoded_pull_request_id("github.com%252Fowner%252Frepo%2523154")
                .is_none()
        );
        assert!(!approved_resource_read_path("/pull-requests/..%2F154/diff"));
        assert!(!approved_resource_read_path("/pull-requests/github.com%2Fowner%2Frepo%3F154/diff"));
        assert!(!approved_resource_read_path("/merge-authorizations/../auth-1"));
        assert!(!is_resource_mutation_path("POST", "/tasks//start"));
        assert!(!is_resource_mutation_path("POST", "/tasks/../start"));
    }

    #[test]
    fn resource_routes_adapt_to_strict_typed_commands() {
        let meta = json!({"data_epoch":"epoch","command_id":"stable-1","expected_revision":7});
        let mut create_task = meta.clone();
        create_task["task_id"] = json!("task-1");
        create_task["title"] = json!("Build");
        create_task["agent_id"] = Value::Null;
        create_task["review_prs"] = json!(false);
        create_task["dependencies"] = json!([]);
        create_task["stack"] = Value::Null;
        let adapted = resource_envelope_from_path("POST", "/requests/r-1/tasks", create_task).unwrap();
        assert_eq!(adapted.command, Command::CreateTask { plan_items: None,
            task_id: "task-1".into(), request_id: "r-1".into(), title: "Build".into(),
            agent_id: None, review_prs: false, dependencies: Default::default(), stack: None, launch: None,
        });
        let mut assign = meta.clone();
        assign["agent_id"] = json!("node-1");
        let adapted = resource_envelope_from_path("PUT", "/tasks/task-1/assignee", assign).unwrap();
        assert_eq!(adapted.command, Command::AssignTask { task_id: "task-1".into(), agent_id: "node-1".into(), model: None });
        let adapted = resource_envelope_from_path("POST", "/tasks/task-1/cleanup", meta.clone()).unwrap();
        assert_eq!(adapted.command, Command::ConfirmCleanup { task_id: "task-1".into(), discard: Vec::new() });
        let mut declare = meta.clone();
        declare["reason"] = json!("release.sh");
        let adapted = resource_envelope_from_path("PUT", "/tasks/task-1/post-merge-work", declare).unwrap();
        assert_eq!(adapted.command, Command::DeclarePostMergeWork { task_id: "task-1".into(), reason: Some("release.sh".into()) });
        let adapted = resource_envelope_from_path("PUT", "/tasks/task-1/post-merge-work", meta.clone()).unwrap();
        assert_eq!(adapted.command, Command::DeclarePostMergeWork { task_id: "task-1".into(), reason: None });
        let mut mismatched = meta.clone();
        mismatched["task_id"] = json!("other-task");
        mismatched["title"] = json!("Build");
        assert!(resource_envelope_from_path("PATCH", "/tasks/task-1", mismatched).is_none());
        let mut unknown = meta.clone();
        unknown["surprise"] = json!(true);
        assert!(resource_envelope_from_path("POST", "/messages", unknown).is_none());
        let mut legacy_shape = json!({"data_epoch":"epoch","command_id":"stable-1","expected_revision":7,
            "protocol_version":2,"request_id":"request-1","body":"Work","auto_merge":false});
        assert!(resource_envelope_from_path("POST", "/requests", legacy_shape.take()).is_none());
    }
    fn seeded(host: &Host) {
        for (actor, id, command) in [
            (
                Actor::System,
                "agent",
                Command::RegisterAgent {
                    agent_id: "agent-a".into(),
                    name: "Synthetic agent".into(), at: 0
                },
            ),
            (
                Actor::User {
                    user_id: "local-user".into(),
                },
                "request",
                Command::CreateRequest {
                    request_id: "request-a".into(),
                    body: "Synthetic request".into(), auto_merge: false, maintenance: false, message: false,
                },
            ),
            (
                Actor::Orchestrator { generation: 0 },
                "task",
                Command::CreateTask { plan_items: None,
                    task_id: "task-a".into(),
                    request_id: "request-a".into(),
                    title: "Synthetic task".into(),
                    agent_id: Some("agent-a".into()),
                    review_prs: true,
                    dependencies: BTreeSet::new(),
                    stack: None,
                    launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
                },
            ),
            (
                Actor::System,
                "info",
                Command::PublishInformation {
                    interaction_id: "info-a".into(),
                    scope: Scope::Task {
                        task_id: "task-a".into(),
                    },
                    recipient: Recipient::User,
                    body: "Information".into(),
                    attachments: vec![], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Know, "Read the update"),
                },
            ),
        ] {
            host.domain
                .execute(&actor, &envelope(host, id, command), None, || Ok(()))
                .unwrap();
        }
    }
    struct Host {
        daemon: Arc<crate::domain_bootstrap::DomainDaemon>,
        domain: Arc<DomainRuntime>,
        home: std::path::PathBuf,
        test_directory: Arc<crate::test_support::Directory>,
    }

    fn fixture() -> (std::path::PathBuf, Arc<Host>) {
        let directory = crate::test_support::Directory::new();
        std::fs::create_dir_all(&directory).unwrap();
        drop(DomainRuntime::open(&directory).unwrap());
        let daemon = crate::domain_bootstrap::DomainDaemon::open(&directory).unwrap();
        let host = Arc::new(Host {
            domain: daemon.domain(),
            daemon,
            home: directory.to_path_buf(),
            test_directory: Arc::new(directory),
        });
        seeded(&host);
        (host.home.clone(), host)
    }
    async fn server(host: Arc<Host>) -> (String, tokio::task::JoinHandle<()>, SigningKey, String) {
        let auth = Arc::new(crate::auth::Auth::open(&host.home).unwrap());
        let key = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let code = auth.new_pairing_code();
        let device = auth
            .pair(
                &code,
                "Test iPhone",
                &STANDARD.encode(key.verifying_key().to_sec1_bytes()),
            )
            .unwrap();
        let app = crate::domain_api::routes::<crate::domain_bootstrap::DomainDaemon>()
        .with_state(host.daemon.clone())
        .layer(axum::middleware::from_fn_with_state(
            auth,
            crate::auth::guard,
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        (base, handle, key, device.id)
    }
    fn signed_get(
        client: &reqwest::Client,
        base: &str,
        key: &SigningKey,
        device_id: &str,
        path: &str,
    ) -> reqwest::RequestBuilder {
        // The verifier rejects replayed signatures. Two GETs in the same
        // millisecond otherwise sign identical bytes and make this test flaky.
        static LAST_TIMESTAMP: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        let now = alondra_core::now_millis();
        let timestamp = loop {
            let previous = LAST_TIMESTAMP.load(std::sync::atomic::Ordering::SeqCst);
            let next = now.max(previous + 1);
            if LAST_TIMESTAMP.compare_exchange(
                previous, next,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            ).is_ok() { break next; }
        };
        let message = crate::auth::signed_message("GET", path, &timestamp.to_string(), b"");
        let signature: Signature = key.sign(message.as_bytes());
        client
            .get(format!("{base}{path}"))
            .header("x-alondra-device", device_id)
            .header("x-alondra-timestamp", timestamp.to_string())
            .header(
                "x-alondra-signature",
                STANDARD.encode(signature.to_der().as_bytes()),
            )
    }
    #[tokio::test]
    async fn local_user_write_waits_for_cutover_while_encrypted_receipts_and_capabilities_work() {
        let (home, host) = fixture();
        let _directory = host.test_directory.clone();
        let (base, server, key, device_id) = server(host.clone()).await;
        let client = reqwest::Client::new();
        let request = envelope(
            &host,
            "read-once",
            Command::MarkRead {
                interaction_id: "info-a".into(),
            },
        );
        let first = client.post(format!("{base}/commands")).json(&request).send().await.unwrap();
        assert_eq!(first.status(), reqwest::StatusCode::NOT_FOUND, "there is no generic command endpoint");
        let body = serde_json::to_value(&request).unwrap();
        let result = encrypted_command_runtime(
            &host.domain,
            "phone-a",
            Some(&body),
            ("phone-a", "request-one", &[1; 32]),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(result["status"], 200);
        assert_eq!(
            result,
            encrypted_command_runtime(
                &host.domain,
                "phone-a",
                Some(&body),
                ("phone-a", "request-one", &[1; 32]),
                || Ok(())
            )
            .unwrap()
        );
        let conflict = encrypted_command_runtime(
            &host.domain,
            "phone-b",
            Some(&body),
            ("phone-b", "request-one", &[1; 32]),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            conflict["status"], 409,
            "another sender is not a retry of the original user"
        );
        let remote = envelope(
            &host,
            "remote-read",
            Command::MarkRead {
                interaction_id: "info-a".into(),
            },
        );
        let body = serde_json::to_value(remote).unwrap();
        let result = encrypted_command_runtime(
            &host.domain,
            "phone-a",
            Some(&body),
            ("phone-a", "request-two", &[2; 32]),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(result["status"], 200);
        assert_eq!(
            result,
            encrypted_command_runtime(
                &host.domain,
                "phone-a",
                Some(&body),
                ("phone-a", "request-two", &[2; 32]),
                || Ok(())
            )
            .unwrap()
        );
        let pending_before_mismatch = host.domain.store.lock().unwrap().pending_messages().unwrap();
        let mismatch = encrypted_command_runtime(
            &host.domain,
            "phone-a",
            Some(&body),
            ("phone-a", "request-two", &[3; 32]),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(mismatch["body"]["code"], "idempotency_conflict");
        assert_eq!(
            host.domain.store.lock().unwrap().pending_messages().unwrap(),
            pending_before_mismatch,
        );
        assert!(
            host.domain
                .store
                .lock()
                .unwrap()
                .delivery_evidence()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            client
                .get(format!("{base}/capabilities"))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        let capabilities: Value = signed_get(&client, &base, &key, &device_id, "/capabilities")
            .send().await.unwrap().json().await.unwrap();
        assert_eq!(capabilities["data_epoch"], host.domain.state().unwrap().data_epoch.0);
        assert_eq!(
            signed_get(&client, &base, &key, &device_id, "/inbox?filter=all").send().await.unwrap().status(),
            reqwest::StatusCode::NOT_FOUND
        );
        crate::auth::Auth::open(&home)
            .unwrap()
            .revoke(&device_id)
            .unwrap();
        assert_eq!(
            signed_get(&client, &base, &key, &device_id, "/capabilities")
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        server.abort();
        let _ = server.await;
        drop(host);
        std::fs::remove_dir_all(home).unwrap();
    }
    #[tokio::test]
    async fn a_producer_cannot_choose_user_authority_or_spoof_an_actor() {
        let (home, host) = fixture();
        let _directory = host.test_directory.clone();
        let (base, server, _, _) = server(host.clone()).await;
        let client = reqwest::Client::new();
        let token = host
            .domain
            .issue(Principal::Agent {
                session_id: "agent-a".into(),
                task_binding: None,
                provider_agent_id: None,
            })
            .unwrap();
        let cursor = client.get(format!("{base}/capabilities"))
            .bearer_auth(token.expose_for_child_environment()).send().await.unwrap();
        assert_eq!(cursor.status(), reqwest::StatusCode::OK);
        assert_eq!(cursor.json::<Value>().await.unwrap()["protocol_version"], PROTOCOL_VERSION);
        assert_eq!(client.get(format!("{base}/capabilities"))
            .bearer_auth("not-a-capability").send().await.unwrap().status(),
            reqwest::StatusCode::FORBIDDEN);
        assert_eq!(client.get(format!("{base}/merge-authorizations/missing"))
            .send().await.unwrap().status(), reqwest::StatusCode::NOT_FOUND);
        assert_eq!(client.get(format!("{base}/merge-authorizations/missing"))
            .bearer_auth(token.expose_for_child_environment()).send().await.unwrap().status(),
            reqwest::StatusCode::NOT_FOUND);
        let before = host.domain.state().unwrap();
        let request = json!({"data_epoch":before.data_epoch,"command_id":"bad-read",
            "expected_revision":before.revision});
        let anonymous = client
            .put(format!("{base}/interactions/info-a/read-state"))
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(anonymous.status(), reqwest::StatusCode::FORBIDDEN);
        for bearer in [token.expose_for_child_environment(), "not-a-capability"] {
            let response = client
                .put(format!("{base}/interactions/info-a/read-state"))
                .bearer_auth(bearer)
                .json(&request)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN, "a producer cannot mark the user's information read");
        }
        let response = client
            .put(format!("{base}/interactions/info-a/read-state"))
            .header("x-alondra-session", "agent-a")
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
        let mut forged = request.clone();
        forged["actor"] = json!({"kind":"system"});
        let response = client
            .put(format!("{base}/interactions/info-a/read-state"))
            .json(&forged)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(before, host.domain.state().unwrap());
        let revoked = encrypted_command_runtime(
            &host.domain,
            "phone-a",
            Some(&serde_json::to_value(&request).unwrap()),
            ("phone-a", "revoked", &[4; 32]),
            || anyhow::bail!("device revoked"),
        )
        .unwrap();
        assert_eq!(revoked["status"], 422);
        assert_eq!(revoked["body"]["code"], "invalid_input");
        assert_eq!(before, host.domain.state().unwrap());
        server.abort();
        let _ = server.await;
        drop(host);
        std::fs::remove_dir_all(home).unwrap();
    }
    #[tokio::test]
    async fn document_chunks_use_registered_domain_identity() {
        let (home, host) = fixture();
        let _directory = host.test_directory.clone();
        let data = b"# Synthetic design\n";
        std::fs::create_dir_all(home.join("files")).unwrap();
        std::fs::write(home.join("files/design.md"), data).unwrap();
        let before_bad_document = host.domain.state().unwrap();
        let invalid = host.domain.execute(
            &Actor::Agent { agent_id: "agent-a".into() },
            &envelope(&host, "document-wrong-hash", Command::CreateDocument {
                task_id: "task-a".into(), document_id: "bad-doc".into(),
                title: "Bad".into(), revision_id: "bad-rev".into(),
                content_digest: "0".repeat(64), content_reference: "files/design.md".into(),
                role: "result".into(),
            }), None, || Ok(()));
        assert!(invalid.is_err());
        assert_eq!(host.domain.state().unwrap(), before_bad_document);
        let valid_digest: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
        let rejected = host.domain.execute(
            &Actor::Agent { agent_id: "agent-a".into() },
            &envelope(&host, "document-wrong-task", Command::CreateDocument {
                task_id: "other-task".into(), document_id: "orphan-doc".into(),
                title: "Orphan".into(), revision_id: "orphan-rev".into(),
                content_digest: valid_digest.clone(), content_reference: "files/design.md".into(),
                role: "result".into(),
            }), None, || Ok(()));
        assert!(rejected.is_err());
        assert!(!home.join("files/domain-v2").join(&valid_digest).exists());
        host.domain
            .execute(
                &Actor::Agent {
                    agent_id: "agent-a".into(),
                },
                &envelope(
                    &host,
                    "document",
                    Command::CreateDocument {
                        task_id: "task-a".into(),
                        document_id: "doc-a".into(),
                        title: "Design".into(),
                        revision_id: "rev-a".into(),
                        content_digest: Sha256::digest(data)
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect(),
                        content_reference: "files/design.md".into(),
                        role: "result".into(),
                    },
                ),
                None,
                || Ok(()),
            )
            .unwrap();
        let managed = host.domain.state().unwrap().document_revisions[&DocumentRevisionId("rev-a".into())]
            .content_reference.clone();
        assert!(managed.starts_with("files/domain-v2/"));
        let (base, server, key, device_id) = server(host.clone()).await;
        let client = reqwest::Client::new();
        let actions_path = "/tasks/task-a/actions";
        assert_eq!(signed_get(&client, &base, &key, &device_id, actions_path)
            .send().await.unwrap().status(), reqwest::StatusCode::NOT_FOUND);
        host.domain.execute(
            &Actor::User { user_id: format!("device:{device_id}") },
            &envelope(&host, "approve-document", Command::DecideDocumentReview {
                task_id: "task-a".into(), document_id: "doc-a".into(),
                revision_id: "rev-a".into(), accepted: true,
            }), None, || Ok(())).unwrap();
        // Replacing the upload after approval cannot alter the accepted revision.
        std::fs::write(home.join("files/design.md"), b"# Changed after review\n").unwrap();
        let chunk_path = "/document-revisions/rev-a/content?offset=0";
        assert_eq!(
            client
                .get(format!("{base}{chunk_path}"))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        let chunk: Value = signed_get(&client, &base, &key, &device_id, chunk_path)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(chunk["revision_id"], "rev-a");
        assert_eq!(
            chunk["content_digest"],
            Sha256::digest(data)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(chunk["data"].as_str().unwrap())
                .unwrap(),
            data
        );
        assert_eq!(chunk["eof"], true);
        // Even a same-UID writer that tampers with the managed copy gets a hard
        // content error, never changed bytes labeled with the accepted digest.
        std::fs::write(home.join(managed), b"# Tampered managed copy\n").unwrap();
        let tampered = signed_get(&client, &base, &key, &device_id, chunk_path)
            .send().await.unwrap();
        assert_eq!(tampered.status(), reqwest::StatusCode::CONFLICT);
        assert_eq!(tampered.json::<Value>().await.unwrap()["code"], "content_unavailable");
        assert!(document_content_query("offset=1").is_none());
        assert!(document_content_query("offset=0&offset=1").is_none());
        assert!(document_content_query("offset=0&version=../").is_none());
        server.abort();
        let _ = server.await;
        drop(host);
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[cfg(test)]
mod domain_encrypted_tests {
    use super::*;
    use crate::auth::Auth;
    use crate::domain_runtime::DomainRuntime;

    #[test]
    fn encrypted_user_command_replays_receipt_and_revocation_fences_write_without_hub() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!("alondra-relay-{}", crate::auth::random_hex(8)));
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        let epoch = domain.state().unwrap().data_epoch.clone();
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: epoch,
            command_id: "encrypted-request".into(),
            expected_revision: 0,
            command: Command::CreateRequest {
                request_id: "request-from-phone".into(),
                body: "Synthetic encrypted request".into(), auto_merge: false, maintenance: false, message: false,
            },
        };
        let body = serde_json::to_value(&envelope).unwrap();
        let digest = [7u8; 32];
        let authorized = || Ok(());
        let first = encrypted_command_runtime(&domain, "phone-a", Some(&body),
            ("phone-a", "sealed-request-1", &digest), authorized).unwrap();
        assert_eq!(first["status"], 200);
        assert_eq!(first["body"]["revision"], 1);
        let replay = encrypted_command_runtime(&domain, "phone-a", Some(&body),
            ("phone-a", "sealed-request-1", &digest), || Ok(())).unwrap();
        assert_eq!(replay, first);
        assert_eq!(domain.state().unwrap().revision, 1);
        let forged_identity = encrypted_command_runtime(&domain, "phone-a", Some(&body),
            ("phone-b", "sealed-request-forged", &digest), || Ok(())).unwrap();
        assert_eq!(forged_identity["status"], 422);
        let wrong_sender = encrypted_command_runtime(&domain, "phone-b", Some(&body),
            ("phone-b", "sealed-request-1", &digest), || Ok(())).unwrap();
        assert_eq!(wrong_sender["status"], 409);
        let revoked = CommandEnvelope {
            command_id: "encrypted-request-2".into(),
            expected_revision: 1,
            command: Command::CreateRequest {
                request_id: "request-after-revocation".into(),
                body: "Must not commit".into(), auto_merge: false, maintenance: false, message: false,
            },
            ..envelope
        };
        let revoked_body = serde_json::to_value(revoked).unwrap();
        let denied = encrypted_command_runtime(&domain, "phone-a", Some(&revoked_body),
            ("phone-a", "sealed-request-2", &[8u8; 32]),
            || anyhow::bail!("device revoked")).unwrap();
        assert_eq!(denied["status"], 403);
        assert!(!domain.state().unwrap().requests.contains_key(&"request-after-revocation".into()));
        drop(Auth::open(&home).unwrap());
        drop(domain);
        std::fs::remove_dir_all(home).unwrap();
    }
}
