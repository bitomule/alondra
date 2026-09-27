//! Versioned domain commands. Both local HTTP and encrypted transport call the same runtime.
use crate::auth::{Signed, is_local};
use crate::domain_store::{StoreError, TransportIdentity};
use crate::hub::Hub;
use alondra_core::domain_v2::*;
use axum::extract::{ConnectInfo, Path, RawQuery, State, rejection::JsonRejection};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncReadExt;

pub fn routes() -> Router<Arc<Hub>> {
    Router::new()
        .route("/v2/domain", get(projection))
        .route("/v2/domain/capabilities", get(capabilities))
        .route("/v2/domain/changes", get(changes))
        .route("/v2/domain/pull-requests/{id}/diff", get(pull_request_diff))
        .route("/v2/domain/tasks/{id}/actions", get(task_actions))
        .route(
            "/v2/domain/document-revisions/{id}/content",
            get(document_content),
        )
        .route(
            "/v2/domain/commands",
            post(command).layer(axum::extract::DefaultBodyLimit::max(180 * 1024)),
        )
}
async fn document_content(
    State(hub): State<Arc<Hub>>,
    signed: Option<Extension<Signed>>,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
) -> Response {
    if !signed_read(&hub, &signed) {
        return unsigned_read();
    }
    let response = document_content_inner(hub.clone(), id, query).await;
    if !signed_read(&hub, &signed) {
        return unsigned_read();
    }
    response
}

async fn document_content_inner(hub: Arc<Hub>, id: String, query: Option<String>) -> Response {
    let Some((offset, version)) = query.as_deref().and_then(document_content_query) else {
        return domain_error(DomainError::new(
            "invalid_input",
            "offset and version query are invalid",
        ));
    };
    let state = match hub.domain.state() {
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
    let home = hub.home.clone();
    let chunk = tokio::task::spawn_blocking(move || {
        crate::service_files::read_verified_domain_chunk(&home, &reference, &digest, offset, version)
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
async fn task_actions(
    State(hub): State<Arc<Hub>>,
    signed: Option<Extension<Signed>>,
    Path(id): Path<String>,
) -> Response {
    if !signed_read(&hub, &signed) {
        return unsigned_read();
    }
    task_actions_inner(hub, id)
}

fn task_actions_inner(hub: Arc<Hub>, id: String) -> Response {
    let state = match hub.domain.state() {
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

async fn pull_request_diff(
    State(hub): State<Arc<Hub>>,
    signed: Option<Extension<Signed>>,
    Path(id): Path<String>,
) -> Response {
    if !signed_read(&hub, &signed) {
        return unsigned_read();
    }
    let response = pull_request_diff_inner(hub.clone(), id).await;
    if !signed_read(&hub, &signed) {
        return unsigned_read();
    }
    response
}

async fn pull_request_diff_inner(hub: Arc<Hub>, id: String) -> Response {
    let state = match hub.domain.state() {
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

fn bound_diff_json(body: &mut Value) {
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
pub(crate) fn changes_query(query: &str) -> Option<(String, Revision, u64)> {
    let (mut epoch, mut after, mut wait) = (None, None, None);
    for field in query.split('&') {
        let (key, value) = field.split_once('=')?;
        match key {
            "data_epoch"
                if epoch.is_none()
                    && !value.is_empty()
                    && value.len() <= 256
                    && value.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
                    }) =>
            {
                epoch = Some(value.to_owned())
            }
            "after"
                if after.is_none()
                    && !value.is_empty()
                    && value.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                let parsed: u64 = value.parse().ok()?;
                if parsed > MAX_REVISION {
                    return None;
                }
                after = Some(parsed);
            }
            "wait"
                if wait.is_none()
                    && !value.is_empty()
                    && value.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                let parsed: u64 = value.parse().ok()?;
                if parsed > 50 {
                    return None;
                }
                wait = Some(parsed);
            }
            _ => return None,
        }
    }
    Some((epoch?, after?, wait?))
}
async fn changes(
    State(hub): State<Arc<Hub>>,
    signed: Option<Extension<Signed>>,
    RawQuery(query): RawQuery,
) -> Response {
    if !signed_read(&hub, &signed) {
        return unsigned_read();
    }
    let response = changes_inner(hub.clone(), query).await;
    if !signed_read(&hub, &signed) {
        return unsigned_read();
    }
    response
}

async fn changes_inner(hub: Arc<Hub>, query: Option<String>) -> Response {
    let Some((epoch, after, wait)) = query.as_deref().and_then(changes_query) else {
        return domain_error(DomainError::new(
            "invalid_input",
            "changes requires data_epoch, after and wait (0–50) exactly once",
        ));
    };
    let mut receiver = hub.domain.subscribe();
    let initial = match hub.domain.state() {
        Ok(state) => state,
        Err(_) => return storage_error(),
    };
    if initial.data_epoch.0 == epoch && initial.revision == after && wait > 0 {
        let _ =
            tokio::time::timeout(std::time::Duration::from_secs(wait), receiver.changed()).await;
    }
    let state = match hub.domain.state() {
        Ok(state) => state,
        Err(_) => return storage_error(),
    };
    Json(json!({"protocol_version":PROTOCOL_VERSION,"data_epoch":state.data_epoch,"revision":state.revision,
        "changed":state.data_epoch.0 != epoch || state.revision != after})).into_response()
}
async fn capabilities(State(hub): State<Arc<Hub>>, signed: Option<Extension<Signed>>) -> Response {
    if !signed_read(&hub, &signed) {
        return unsigned_read();
    }
    capabilities_inner(hub)
}

fn capabilities_inner(hub: Arc<Hub>) -> Response {
    match hub.domain.state() {
        Ok(state) => Json(json!({"protocol_version": PROTOCOL_VERSION, "data_epoch": state.data_epoch, "revision": state.revision})).into_response(),
        Err(_) => storage_error(),
    }
}
async fn projection(State(hub): State<Arc<Hub>>, signed: Option<Extension<Signed>>) -> Response {
    if !signed_read(&hub, &signed) {
        return unsigned_read();
    }
    projection_inner(hub)
}

fn projection_inner(hub: Arc<Hub>) -> Response {
    match hub.domain.snapshot(crate::service_overview::MAX_BYTES) {
        Ok(bytes) => (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            bytes,
        )
            .into_response(),
        Err(error) if error.is::<crate::http::OverviewTooLarge>() => {
            domain_error(DomainError::new(
                "payload_too_large",
                "domain projection exceeds snapshot limits",
            ))
        }
        Err(_) => storage_error(),
    }
}

/// Encrypted reads enter through a verified, still-approved device proof. They
/// never round-trip over the unsigned loopback HTTP listener.
pub(crate) async fn encrypted_read(
    hub: Arc<Hub>,
    approved: &crate::service::ApprovedDevice<'_>,
    path: &str,
) -> anyhow::Result<Response> {
    approved.recheck()?;
    let response = if path == "/v2/domain" {
        projection_inner(hub)
    } else if path == "/v2/domain/capabilities" {
        capabilities_inner(hub)
    } else if let Some(query) = path.strip_prefix("/v2/domain/changes?") {
        changes_inner(hub, Some(query.to_owned())).await
    } else if let Some(id) = path
        .strip_prefix("/v2/domain/pull-requests/")
        .and_then(|suffix| suffix.strip_suffix("/diff"))
    {
        pull_request_diff_inner(hub, id.to_owned()).await
    } else if let Some(id) = path
        .strip_prefix("/v2/domain/tasks/")
        .and_then(|suffix| suffix.strip_suffix("/actions"))
    {
        task_actions_inner(hub, id.to_owned())
    } else if let Some((id, query)) = path
        .strip_prefix("/v2/domain/document-revisions/")
        .and_then(|suffix| suffix.split_once("/content?"))
    {
        document_content_inner(hub, id.to_owned(), Some(query.to_owned())).await
    } else {
        anyhow::bail!("unapproved encrypted domain read path")
    };
    approved.recheck()?;
    Ok(response)
}
async fn command(
    State(hub): State<Arc<Hub>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    signed: Option<Extension<Signed>>,
    headers: HeaderMap,
    body: Result<Json<CommandEnvelope>, JsonRejection>,
) -> Response {
    let envelope = match body {
        Ok(Json(value)) => value,
        Err(_) => {
            return domain_error(DomainError::new(
                "invalid_input",
                "invalid domain command envelope",
            ));
        }
    };
    let result = if let Some(header) = headers.get("authorization") {
        if signed.is_some() || !is_local(&peer, &headers) {
            return domain_error(DomainError::new(
                "forbidden",
                "producer capabilities are local only",
            ));
        }
        let Some(token) = header
            .to_str()
            .ok()
            .and_then(|value| value.strip_prefix("Bearer "))
        else {
            return domain_error(DomainError::new("forbidden", "invalid domain capability"));
        };
        hub.domain.execute_bearer(token, &envelope)
    } else {
        if headers.contains_key("x-alondra-session")
            || headers.contains_key("x-alondra-orchestrator-execution")
        {
            return domain_error(DomainError::new(
                "incompatible_protocol",
                "this producer must resume with a domain v2 capability",
            ));
        }
        // A same-UID Agent can call local pairing and create its own signing
        // key. Signed loopback writes must remain closed until a user-presence
        // enrollment channel exists. Apps use the encrypted service path.
        let Some(Extension(_signed)) = signed else {
            return domain_error(DomainError::new("forbidden", "signed user device required"));
        };
        return domain_error(DomainError::new(
            "forbidden",
            "local user presence is required for domain commands",
        ));
    };
    match result {
        Ok(outcome) => Json(outcome).into_response(),
        Err(error) => store_error(error),
    }
}

/// Sender is the verified envelope identity; a body cannot change this principal.
/// The caller retains its ciphertext journal and rechecks device approval at commit.
pub fn encrypted_command(
    hub: &Hub,
    sender: &str,
    body: Option<&Value>,
    identity: TransportIdentity<'_>,
    authorize: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<Value> {
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
    let result = hub.domain.execute(
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
fn status_for(error: &DomainError) -> StatusCode {
    match error.code.as_str() {
        "forbidden" | "not_assigned" => StatusCode::FORBIDDEN,
        "not_found" => StatusCode::NOT_FOUND,
        "invalid_input" | "invalid_action_for_kind" => StatusCode::UNPROCESSABLE_ENTITY,
        "payload_too_large" => StatusCode::PAYLOAD_TOO_LARGE,
        _ => StatusCode::CONFLICT,
    }
}
fn domain_error(error: DomainError) -> Response {
    (status_for(&error), Json(error)).into_response()
}
fn unsigned_read() -> Response {
    domain_error(DomainError::new("forbidden", "signed user device required"))
}
fn signed_read(hub: &Hub, signed: &Option<Extension<Signed>>) -> bool {
    let Some(Extension(signed)) = signed else {
        return false;
    };
    crate::auth::Auth::open(&hub.home).ok().is_some_and(|auth| {
        auth.list()
            .iter()
            .any(|device| device.id == signed.device_id && device.public_key == signed.public_key)
    })
}
fn storage_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(DomainError::new(
            "storage_unavailable",
            "domain transaction could not be completed",
        )),
    )
        .into_response()
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
    fn serialized_diff_fits_encrypted_relay_budget_and_reports_truncation() {
        let mut body = json!({"pr_id":"pr-a","host":"github.com","repository":"owner/repo",
            "number":1,"head":"a".repeat(40),"diff":"\\\n\t".repeat(100_000),
            "truncated":false});
        bound_diff_json(&mut body);
        assert!(body["truncated"].as_bool().unwrap());
        assert!(serde_json::to_vec(&body).unwrap().len() <= MAX_RELAY_DIFF_RESPONSE);
        assert!(body["diff"].as_str().unwrap().chars().count() > 0);
    }
    fn envelope(hub: &Hub, id: &str, command: Command) -> CommandEnvelope {
        let state = hub.domain.state().unwrap();
        CommandEnvelope {
            protocol_version: 2,
            data_epoch: state.data_epoch,
            command_id: id.into(),
            expected_revision: state.revision,
            command,
        }
    }
    fn seeded(hub: &Hub) {
        for (actor, id, command) in [
            (
                Actor::System,
                "agent",
                Command::RegisterAgent {
                    agent_id: "agent-a".into(),
                    name: "Synthetic agent".into(),
                },
            ),
            (
                Actor::User {
                    user_id: "local-user".into(),
                },
                "request",
                Command::CreateRequest {
                    request_id: "request-a".into(),
                    body: "Synthetic request".into(),
                },
            ),
            (
                Actor::User {
                    user_id: "local-user".into(),
                },
                "task",
                Command::CreateTask {
                    task_id: "task-a".into(),
                    request_id: "request-a".into(),
                    title: "Synthetic task".into(),
                    agent_id: Some("agent-a".into()),
                    review_prs: true,
                    dependencies: BTreeSet::new(),
                    stack: None,
                },
            ),
            (
                Actor::Agent {
                    agent_id: "agent-a".into(),
                },
                "info",
                Command::PublishInformation {
                    interaction_id: "info-a".into(),
                    scope: Scope::Task {
                        task_id: "task-a".into(),
                    },
                    recipient: Recipient::User,
                    body: "Information".into(),
                    attachments: vec![],
                },
            ),
        ] {
            hub.domain
                .execute(&actor, &envelope(hub, id, command), None, || Ok(()))
                .unwrap();
        }
    }
    fn fixture() -> (std::path::PathBuf, Arc<Hub>) {
        let home = std::env::temp_dir().join(format!(
            "alondra-domain-api-{}",
            crate::auth::random_hex(16)
        ));
        let config = alondra_core::config::Config::resolve(
            home.clone(),
            &Default::default(),
            |_| None,
            || Some("Fixture".into()),
        );
        let hub = Arc::new(Hub::open(config, false).unwrap());
        seeded(&hub);
        (home, hub)
    }
    async fn server(hub: Arc<Hub>) -> (String, tokio::task::JoinHandle<()>, SigningKey, String) {
        let auth = Arc::new(crate::auth::Auth::open(&hub.home).unwrap());
        let key = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let code = auth.new_pairing_code();
        let device = auth
            .pair(
                &code,
                "Test iPhone",
                &STANDARD.encode(key.verifying_key().to_sec1_bytes()),
            )
            .unwrap();
        let app = crate::http::router(
            hub.clone(),
            auth.clone(),
            Arc::new(crate::push::Push::open(&hub.home).unwrap()),
        )
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
    fn signed_post(
        client: &reqwest::Client,
        base: &str,
        key: &SigningKey,
        device_id: &str,
        envelope: &CommandEnvelope,
    ) -> reqwest::RequestBuilder {
        static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        let body = serde_json::to_vec(envelope).unwrap();
        let timestamp =
            alondra_core::now_millis() + NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let message = crate::auth::signed_message(
            "POST",
            "/v2/domain/commands",
            &timestamp.to_string(),
            &body,
        );
        let signature: Signature = key.sign(message.as_bytes());
        client
            .post(format!("{base}/v2/domain/commands"))
            .header("content-type", "application/json")
            .header("x-alondra-device", device_id)
            .header("x-alondra-timestamp", timestamp.to_string())
            .header(
                "x-alondra-signature",
                STANDARD.encode(signature.to_der().as_bytes()),
            )
            .body(body)
    }
    fn signed_get(
        client: &reqwest::Client,
        base: &str,
        key: &SigningKey,
        device_id: &str,
        path: &str,
    ) -> reqwest::RequestBuilder {
        let timestamp = alondra_core::now_millis();
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
    async fn local_user_write_fails_closed_while_encrypted_receipts_and_reads_work() {
        let (home, hub) = fixture();
        let (base, server, key, device_id) = server(hub.clone()).await;
        let client = reqwest::Client::new();
        let request = envelope(
            &hub,
            "read-once",
            Command::MarkRead {
                interaction_id: "info-a".into(),
            },
        );
        let first = signed_post(&client, &base, &key, &device_id, &request)
            .send()
            .await
            .unwrap();
        assert_eq!(first.status(), reqwest::StatusCode::FORBIDDEN);
        let body = serde_json::to_value(&request).unwrap();
        let result = encrypted_command(
            &hub,
            "phone-a",
            Some(&body),
            ("phone-a", "request-one", &[1; 32]),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(result["status"], 200);
        assert_eq!(
            result,
            encrypted_command(
                &hub,
                "phone-a",
                Some(&body),
                ("phone-a", "request-one", &[1; 32]),
                || Ok(())
            )
            .unwrap()
        );
        let conflict = encrypted_command(
            &hub,
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
            &hub,
            "remote-read",
            Command::MarkRead {
                interaction_id: "info-a".into(),
            },
        );
        let body = serde_json::to_value(remote).unwrap();
        let result = encrypted_command(
            &hub,
            "phone-a",
            Some(&body),
            ("phone-a", "request-two", &[2; 32]),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(result["status"], 200);
        assert_eq!(
            result,
            encrypted_command(
                &hub,
                "phone-a",
                Some(&body),
                ("phone-a", "request-two", &[2; 32]),
                || Ok(())
            )
            .unwrap()
        );
        let mismatch = encrypted_command(
            &hub,
            "phone-a",
            Some(&body),
            ("phone-a", "request-two", &[3; 32]),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(mismatch["body"]["code"], "idempotency_conflict");
        assert!(
            hub.domain
                .store
                .lock()
                .unwrap()
                .pending_messages()
                .unwrap()
                .is_empty()
        );
        assert!(
            hub.domain
                .store
                .lock()
                .unwrap()
                .delivery_evidence()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            client
                .get(format!("{base}/v2/domain"))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        let projection: DomainProjection =
            signed_get(&client, &base, &key, &device_id, "/v2/domain")
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
        assert_eq!(projection, hub.domain.projection().unwrap());
        crate::auth::Auth::open(&home)
            .unwrap()
            .revoke(&device_id)
            .unwrap();
        assert_eq!(
            signed_get(&client, &base, &key, &device_id, "/v2/domain")
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        server.abort();
        let _ = server.await;
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }
    #[tokio::test]
    async fn a_producer_cannot_choose_user_authority_or_spoof_an_actor() {
        let (home, hub) = fixture();
        let (base, server, _, _) = server(hub.clone()).await;
        let client = reqwest::Client::new();
        let token = hub
            .domain
            .issue(Principal::Agent {
                session_id: "agent-a".into(),
                task_binding: None,
                provider_agent_id: None,
            })
            .unwrap();
        let request = envelope(
            &hub,
            "bad-read",
            Command::MarkRead {
                interaction_id: "info-a".into(),
            },
        );
        let before = hub.domain.state().unwrap();
        let anonymous = client
            .post(format!("{base}/v2/domain/commands"))
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(anonymous.status(), reqwest::StatusCode::FORBIDDEN);
        for bearer in [token.expose_for_child_environment(), "not-a-capability"] {
            let response = client
                .post(format!("{base}/v2/domain/commands"))
                .bearer_auth(bearer)
                .json(&request)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
        }
        let response = client
            .post(format!("{base}/v2/domain/commands"))
            .header("x-alondra-session", "agent-a")
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
        let mut forged = serde_json::to_value(&request).unwrap();
        forged["actor"] = json!({"kind":"system"});
        let response = client
            .post(format!("{base}/v2/domain/commands"))
            .json(&forged)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(before, hub.domain.state().unwrap());
        assert!(
            encrypted_command(
                &hub,
                "phone-a",
                Some(&serde_json::to_value(&request).unwrap()),
                ("phone-a", "revoked", &[4; 32]),
                || anyhow::bail!("device revoked")
            )
            .is_err()
        );
        assert_eq!(before, hub.domain.state().unwrap());
        server.abort();
        let _ = server.await;
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }
    #[tokio::test]
    async fn task_actions_and_document_chunks_use_registered_domain_identity() {
        let (home, hub) = fixture();
        let data = b"# Synthetic design\n";
        std::fs::write(home.join("files/design.md"), data).unwrap();
        let before_bad_document = hub.domain.state().unwrap();
        let invalid = hub.domain.execute(
            &Actor::Agent { agent_id: "agent-a".into() },
            &envelope(&hub, "document-wrong-hash", Command::CreateDocument {
                task_id: "task-a".into(), document_id: "bad-doc".into(),
                title: "Bad".into(), revision_id: "bad-rev".into(),
                content_digest: "0".repeat(64), content_reference: "files/design.md".into(),
                role: "result".into(),
            }), None, || Ok(()));
        assert!(invalid.is_err());
        assert_eq!(hub.domain.state().unwrap(), before_bad_document);
        let valid_digest: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
        let rejected = hub.domain.execute(
            &Actor::Agent { agent_id: "agent-a".into() },
            &envelope(&hub, "document-wrong-task", Command::CreateDocument {
                task_id: "other-task".into(), document_id: "orphan-doc".into(),
                title: "Orphan".into(), revision_id: "orphan-rev".into(),
                content_digest: valid_digest.clone(), content_reference: "files/design.md".into(),
                role: "result".into(),
            }), None, || Ok(()));
        assert!(rejected.is_err());
        assert!(!home.join("files/domain-v2").join(&valid_digest).exists());
        hub.domain
            .execute(
                &Actor::Agent {
                    agent_id: "agent-a".into(),
                },
                &envelope(
                    &hub,
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
        let managed = hub.domain.state().unwrap().document_revisions[&DocumentRevisionId("rev-a".into())]
            .content_reference.clone();
        assert!(managed.starts_with("files/domain-v2/"));
        let (base, server, key, device_id) = server(hub.clone()).await;
        let client = reqwest::Client::new();
        let actions_path = "/v2/domain/tasks/task-a/actions";
        assert_eq!(
            client
                .get(format!("{base}{actions_path}"))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        let actions: Value = signed_get(&client, &base, &key, &device_id, actions_path)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(actions["protocol_version"], 2);
        assert_eq!(actions["actions"].as_array().unwrap().len(), 3);
        hub.domain.execute(
            &Actor::User { user_id: format!("device:{device_id}") },
            &envelope(&hub, "approve-document", Command::DecideDocumentReview {
                task_id: "task-a".into(), document_id: "doc-a".into(),
                revision_id: "rev-a".into(), accepted: true,
            }), None, || Ok(())).unwrap();
        // Replacing the upload after approval cannot alter the accepted revision.
        std::fs::write(home.join("files/design.md"), b"# Changed after review\n").unwrap();
        let chunk_path = "/v2/domain/document-revisions/rev-a/content?offset=0";
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
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }
}
