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
use std::sync::Arc;

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
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
) -> Response {
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
    let revision_id = revision.id.clone();
    let home = hub.home.clone();
    let chunk = tokio::task::spawn_blocking(move || {
        crate::service_files::read_domain_chunk(&home, &reference, offset, version)
    })
    .await;
    match chunk {
        Ok(Ok(mut chunk)) => {
            chunk["revision_id"] = json!(revision_id);
            chunk["content_digest"] = json!(digest);
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
async fn task_actions(State(hub): State<Arc<Hub>>, Path(id): Path<String>) -> Response {
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

async fn pull_request_diff(State(hub): State<Arc<Hub>>, Path(id): Path<String>) -> Response {
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
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        tokio::process::Command::new("gh")
            .args(["pr", "diff", &url])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let output = match output {
        Ok(Ok(output)) if output.status.success() => output,
        _ => {
            return domain_error(DomainError::new(
                "external_read_failed",
                "could not load PR diff",
            ));
        }
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
    let mut diff = String::from_utf8_lossy(&output.stdout).into_owned();
    let truncated = diff.len() > MAX_DIFF_BYTES;
    if truncated {
        let cut = (0..=MAX_DIFF_BYTES)
            .rev()
            .find(|index| diff.is_char_boundary(*index))
            .unwrap_or(0);
        diff.truncate(cut);
    }
    Json(
        json!({"pr_id":pr.id,"host":pr.host,"repository":pr.repository,
        "number":pr.number,"head":before,"diff":diff,"truncated":truncated}),
    )
    .into_response()
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
async fn changes(State(hub): State<Arc<Hub>>, RawQuery(query): RawQuery) -> Response {
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
async fn capabilities(State(hub): State<Arc<Hub>>) -> Response {
    match hub.domain.state() {
        Ok(state) => Json(json!({"protocol_version": PROTOCOL_VERSION, "data_epoch": state.data_epoch, "revision": state.revision})).into_response(),
        Err(_) => storage_error(),
    }
}
async fn projection(State(hub): State<Arc<Hub>>) -> Response {
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
        let user_id = match signed {
            Some(Extension(signed)) => format!("device:{}", signed.device_id),
            None if is_local(&peer, &headers) => "local-user".into(),
            None => {
                return domain_error(DomainError::new("forbidden", "authenticated user required"));
            }
        };
        hub.domain
            .execute(&Actor::User { user_id }, &envelope, None, || Ok(()))
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
        authorize,
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
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest as _, Sha256};
    use std::collections::BTreeSet;
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
    async fn server(hub: Arc<Hub>) -> (String, tokio::task::JoinHandle<()>) {
        let auth = Arc::new(crate::auth::Auth::open(&hub.home).unwrap());
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
        (base, handle)
    }
    #[tokio::test]
    async fn http_and_encrypted_dispatch_share_receipts_and_read_semantics() {
        let (home, hub) = fixture();
        let (base, server) = server(hub.clone()).await;
        let client = reqwest::Client::new();
        let request = envelope(
            &hub,
            "read-once",
            Command::MarkRead {
                interaction_id: "info-a".into(),
            },
        );
        let first = client
            .post(format!("{base}/v2/domain/commands"))
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(first.status(), reqwest::StatusCode::OK);
        let outcome: CommandOutcome = first.json().await.unwrap();
        assert!(outcome.effects.is_empty());
        let retry: CommandOutcome = client
            .post(format!("{base}/v2/domain/commands"))
            .json(&request)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(outcome, retry);
        let body = serde_json::to_value(&request).unwrap();
        let conflict = encrypted_command(
            &hub,
            "phone-a",
            Some(&body),
            ("phone-a", "request-one", &[1; 32]),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            conflict["status"], 409,
            "another sender is not a retry of the local user"
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
        let projection: DomainProjection = client
            .get(format!("{base}/v2/domain"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(projection, hub.domain.projection().unwrap());
        server.abort();
        let _ = server.await;
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }
    #[tokio::test]
    async fn a_producer_cannot_choose_user_authority_or_spoof_an_actor() {
        let (home, hub) = fixture();
        let (base, server) = server(hub.clone()).await;
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
                        content_digest: format!("{:x}", Sha256::digest(data)),
                        content_reference: "files/design.md".into(),
                        role: "result".into(),
                    },
                ),
                None,
                || Ok(()),
            )
            .unwrap();
        let (base, server) = server(hub.clone()).await;
        let client = reqwest::Client::new();
        let actions: Value = client
            .get(format!("{base}/v2/domain/tasks/task-a/actions"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(actions["protocol_version"], 2);
        assert_eq!(actions["actions"].as_array().unwrap().len(), 3);
        let chunk: Value = client
            .get(format!(
                "{base}/v2/domain/document-revisions/rev-a/content?offset=0"
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(chunk["revision_id"], "rev-a");
        assert_eq!(
            chunk["content_digest"],
            format!("{:x}", Sha256::digest(data))
        );
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(chunk["data"].as_str().unwrap())
                .unwrap(),
            data
        );
        assert_eq!(chunk["eof"], true);
        assert!(document_content_query("offset=1").is_none());
        assert!(document_content_query("offset=0&offset=1").is_none());
        assert!(document_content_query("offset=0&version=../").is_none());
        server.abort();
        let _ = server.await;
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }
}
