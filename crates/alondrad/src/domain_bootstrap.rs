//! Local v2-only HTTP bootstrap. This path never constructs a legacy Hub.
//! The encrypted service adapter is a separate cutover gate.
use crate::auth::{Auth, Signed, is_local};
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use alondra_core::domain_v2::{CommandEnvelope, DomainError, PROTOCOL_VERSION};
use anyhow::{Context, Result, ensure};
use axum::extract::{ConnectInfo, Path as RoutePath, RawQuery, State, rejection::JsonRejection};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::json;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) struct V2Only {
    domain: Arc<DomainRuntime>,
    home: PathBuf,
}

impl V2Only {
    pub(crate) fn open(home: &Path, expected_epoch: &str) -> Result<Arc<Self>> {
        let database = home.join("domain-v2.db");
        let metadata = std::fs::symlink_metadata(&database).with_context(|| {
            format!("committed domain database missing: {}", database.display())
        })?;
        ensure!(
            metadata.is_file(),
            "committed domain database must be a regular file"
        );
        let domain = Arc::new(DomainRuntime::open(home)?);
        ensure!(
            domain.state()?.data_epoch.0 == expected_epoch,
            "cutover epoch does not match domain database"
        );
        Ok(Arc::new(Self {
            domain,
            home: home.to_owned(),
        }))
    }

    pub(crate) fn domain(&self) -> Arc<DomainRuntime> {
        self.domain.clone()
    }

    fn signed_current(&self, signed: &Option<Extension<Signed>>) -> bool {
        let Some(Extension(signed)) = signed else {
            return false;
        };
        Auth::open(&self.home).ok().is_some_and(|auth| {
            auth.list().iter().any(|device| {
                device.id == signed.device_id && device.public_key == signed.public_key
            })
        })
    }
}

pub(crate) fn router(state: Arc<V2Only>) -> Router {
    Router::new()
        .route("/v2/domain", get(projection))
        .route("/v2/domain/schedules", get(schedule_list).post(schedule_write_blocked))
        .route("/v2/domain/schedules/{id}", get(schedule_detail).put(schedule_write_blocked).delete(schedule_write_blocked))
        .route("/v2/domain/schedules/{id}/run", post(schedule_write_blocked))
        .route("/v2/domain/capabilities", get(capabilities))
        .route("/v2/domain/changes", get(changes))
        .route("/v2/domain/merge-authorizations/{id}", get(merge_authorization))
        .route("/v2/domain/delivery-attempts", get(delivery_attempts))
        .route("/v2/domain/tasks/{id}/actions", get(task_actions))
        .route("/v2/domain/document-revisions/{id}/content", get(document_content))
        .route("/v2/domain/pull-requests/{id}/diff", get(pull_request_diff))
        .route(
            "/v2/domain/commands",
            post(command).layer(axum::extract::DefaultBodyLimit::max(180 * 1024)),
        )
        .with_state(state)
}
async fn schedule_list(State(state): State<Arc<V2Only>>, signed: Option<Extension<Signed>>) -> Response {
    if !state.signed_current(&signed) { return error("forbidden", "signed user device required"); }
    let response = crate::domain_schedule_api::response(crate::domain_schedule_api::read(&state.domain, "/v2/domain/schedules"));
    if !state.signed_current(&signed) { return error("forbidden", "signed user device required"); }
    response
}
async fn schedule_detail(State(state): State<Arc<V2Only>>, signed: Option<Extension<Signed>>, RoutePath(id): RoutePath<String>) -> Response {
    if !state.signed_current(&signed) { return error("forbidden", "signed user device required"); }
    let path = format!("/v2/domain/schedules/{id}");
    let response = crate::domain_schedule_api::response(crate::domain_schedule_api::read(&state.domain, &path));
    if !state.signed_current(&signed) { return error("forbidden", "signed user device required"); }
    response
}
async fn schedule_write_blocked() -> Response {
    crate::domain_schedule_api::local_write_blocked()
}

async fn merge_authorization(
    State(state): State<Arc<V2Only>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    RoutePath(id): RoutePath<String>,
) -> Response {
    if !is_local(&peer, &headers) {
        return error("forbidden", "local Agent capability required");
    }
    let Some(token) = headers.get("authorization").and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer ")) else {
        return error("forbidden", "Agent capability required");
    };
    crate::domain_api::merge_authorization_inner(&state.domain, token, id)
}

async fn delivery_attempts(
    State(state): State<Arc<V2Only>>,
    signed: Option<Extension<Signed>>,
    RawQuery(query): RawQuery,
) -> Response {
    if !state.signed_current(&signed) {
        return error("forbidden", "signed user device required");
    }
    let response = crate::domain_api::delivery_evidence_inner(
        &state.domain, query.as_deref().unwrap_or(""));
    if !state.signed_current(&signed) {
        return error("forbidden", "signed user device required");
    }
    response
}

async fn task_actions(
    State(state): State<Arc<V2Only>>,
    signed: Option<Extension<Signed>>,
    RoutePath(id): RoutePath<String>,
) -> Response {
    if !state.signed_current(&signed) {
        return error("forbidden", "signed user device required");
    }
    let response = crate::domain_api::task_actions_inner(&state.domain, id);
    if !state.signed_current(&signed) {
        return error("forbidden", "signed user device required");
    }
    response
}

async fn document_content(
    State(state): State<Arc<V2Only>>,
    signed: Option<Extension<Signed>>,
    RoutePath(id): RoutePath<String>,
    RawQuery(query): RawQuery,
) -> Response {
    if !state.signed_current(&signed) {
        return error("forbidden", "signed user device required");
    }
    let response = crate::domain_api::document_content_inner(
        &state.domain, &state.home, id, query).await;
    if !state.signed_current(&signed) {
        return error("forbidden", "signed user device required");
    }
    response
}

async fn pull_request_diff(
    State(state): State<Arc<V2Only>>,
    signed: Option<Extension<Signed>>,
    RoutePath(id): RoutePath<String>,
) -> Response {
    if !state.signed_current(&signed) {
        return error("forbidden", "signed user device required");
    }
    let response = crate::domain_api::pull_request_diff_inner(&state.domain, id).await;
    if !state.signed_current(&signed) {
        return error("forbidden", "signed user device required");
    }
    response
}

fn domain_error(domain: DomainError) -> Response {
    let status = match domain.code.as_str() {
        "forbidden" | "not_assigned" => StatusCode::FORBIDDEN,
        "not_found" => StatusCode::NOT_FOUND,
        "invalid_input" | "invalid_action_for_kind" => StatusCode::UNPROCESSABLE_ENTITY,
        "payload_too_large" => StatusCode::PAYLOAD_TOO_LARGE,
        "storage_unavailable" => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::CONFLICT,
    };
    (status, Json(domain)).into_response()
}

fn error(code: &str, message: &str) -> Response {
    domain_error(DomainError::new(code, message))
}

fn store_error(error_value: StoreError) -> Response {
    match error_value {
        StoreError::Domain(domain) => domain_error(domain),
        StoreError::Storage(_) => error(
            "storage_unavailable",
            "domain transaction could not be completed",
        ),
    }
}

async fn projection(
    State(state): State<Arc<V2Only>>,
    signed: Option<Extension<Signed>>,
) -> Response {
    if !state.signed_current(&signed) {
        return error("forbidden", "signed user device required");
    }
    let response = match state.domain.snapshot(crate::service_overview::MAX_BYTES) {
        Ok(bytes) => ([(header::CONTENT_TYPE, "application/json")], bytes).into_response(),
        Err(value) if value.is::<crate::http::OverviewTooLarge>() => error(
            "payload_too_large",
            "domain projection exceeds snapshot limits",
        ),
        Err(_) => error(
            "storage_unavailable",
            "domain transaction could not be completed",
        ),
    };
    if !state.signed_current(&signed) {
        return error("forbidden", "signed user device required");
    }
    response
}

async fn capabilities(
    State(state): State<Arc<V2Only>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    signed: Option<Extension<Signed>>,
    headers: HeaderMap,
) -> Response {
    let bearer = headers.get("authorization").and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let producer_current = || bearer.is_some_and(|token| signed.is_none()
        && is_local(&peer, &headers) && state.domain.resolve_bearer(token).is_ok());
    if !state.signed_current(&signed) && !producer_current() {
        return error("forbidden", "signed device or local producer capability required");
    }
    let response = match state.domain.state() {
        Ok(domain) => Json(json!({"protocol_version":PROTOCOL_VERSION,"data_epoch":domain.data_epoch,"revision":domain.revision})).into_response(),
        Err(_) => error("storage_unavailable", "domain transaction could not be completed"),
    };
    if !state.signed_current(&signed) && !producer_current() {
        return error("forbidden", "signed device or local producer capability required");
    }
    response
}

async fn changes(
    State(state): State<Arc<V2Only>>,
    signed: Option<Extension<Signed>>,
    RawQuery(query): RawQuery,
) -> Response {
    if !state.signed_current(&signed) {
        return error("forbidden", "signed user device required");
    }
    let Some((epoch, after, wait)) = query.as_deref().and_then(crate::domain_api::changes_query)
    else {
        return error(
            "invalid_input",
            "changes requires data_epoch, after and wait (0–50) exactly once",
        );
    };
    let mut receiver = state.domain.subscribe();
    let initial = match state.domain.state() {
        Ok(value) => value,
        Err(_) => {
            return error(
                "storage_unavailable",
                "domain transaction could not be completed",
            );
        }
    };
    if initial.data_epoch.0 == epoch && initial.revision == after && wait > 0 {
        let _ =
            tokio::time::timeout(std::time::Duration::from_secs(wait), receiver.changed()).await;
    }
    let response = match state.domain.state() {
        Ok(domain) => Json(json!({"protocol_version":PROTOCOL_VERSION,"data_epoch":domain.data_epoch,
            "revision":domain.revision,"changed":domain.data_epoch.0 != epoch || domain.revision != after})).into_response(),
        Err(_) => error("storage_unavailable", "domain transaction could not be completed"),
    };
    if !state.signed_current(&signed) {
        return error("forbidden", "signed user device required");
    }
    response
}

async fn command(
    State(state): State<Arc<V2Only>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    signed: Option<Extension<Signed>>,
    headers: HeaderMap,
    body: std::result::Result<Json<CommandEnvelope>, JsonRejection>,
) -> Response {
    let Ok(Json(envelope)) = body else {
        return error("invalid_input", "invalid domain command envelope");
    };
    if headers.contains_key("x-alondra-session")
        || headers.contains_key("x-alondra-orchestrator-execution")
    {
        return error(
            "incompatible_protocol",
            "this producer must resume with a domain v2 capability",
        );
    }
    let Some(token) = headers.get("authorization") else {
        return error(
            "forbidden",
            "local user presence is required for domain commands",
        );
    };
    if signed.is_some() || !is_local(&peer, &headers) {
        return error("forbidden", "producer capabilities are local only");
    }
    let Some(token) = token
        .to_str()
        .ok()
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return error("forbidden", "invalid domain capability");
    };
    match state.domain.execute_bearer(token, &envelope) {
        Ok(outcome) => Json(outcome).into_response(),
        Err(value) => store_error(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use p256::ecdsa::{Signature, SigningKey, signature::Signer};

    #[tokio::test]
    async fn standalone_router_reads_real_domain_and_has_no_legacy_routes() {
        let home = std::env::temp_dir().join(format!(
            "alondra-v2-bootstrap-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        let epoch = domain.state().unwrap().data_epoch.0;
        drop(domain);
        let auth = Auth::open(&home).unwrap();
        let key = SigningKey::from_slice(&[9u8; 32]).unwrap();
        let pairing = auth.new_pairing_code();
        let device = auth
            .pair(
                &pairing,
                "test-device",
                &STANDARD.encode(key.verifying_key().to_sec1_bytes()),
            )
            .unwrap();
        let state = V2Only::open(&home, &epoch).unwrap();
        assert!(V2Only::open(&home, "wrong-epoch").is_err());
        let domain = state.domain();
        let capability = domain.issue(crate::domain_authority::Principal::Agent {
            session_id: "test-agent".into(), task_binding: None, provider_agent_id: None,
        }).unwrap();
        let bearer = capability.expose_for_child_environment().to_owned();
        let auth = Arc::new(Auth::open(&home).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(state).layer(axum::middleware::from_fn_with_state(
            auth,
            crate::auth::guard,
        ));
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::new();
        assert_eq!(
            client
                .get(format!("{base}/v1/overview"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            client
                .get(format!("{base}/v2/domain"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let path = "/v2/domain";
        let timestamp = alondra_core::now_millis().to_string();
        let message = crate::auth::signed_message("GET", path, &timestamp, b"");
        let signature: Signature = key.sign(message.as_bytes());
        let response = client
            .get(format!("{base}{path}"))
            .header("x-alondra-device", &device.id)
            .header("x-alondra-timestamp", timestamp)
            .header(
                "x-alondra-signature",
                STANDARD.encode(signature.to_der().as_bytes()),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["protocol_version"], 2);
        assert_eq!(body["data_epoch"].as_str(), Some(epoch.as_str()));
        let producer = client.get(format!("{base}/v2/domain/capabilities"))
            .bearer_auth(&bearer).send().await.unwrap();
        assert_eq!(producer.status(), StatusCode::OK);
        let cursor: serde_json::Value = producer.json().await.unwrap();
        assert_eq!(cursor["data_epoch"].as_str(), Some(epoch.as_str()));
        assert_eq!(client.get(format!("{base}/v2/domain/capabilities"))
            .bearer_auth("invalid").send().await.unwrap().status(), StatusCode::FORBIDDEN);
        for (path, expected) in [
            ("/v2/domain/schedules", StatusCode::OK),
            ("/v2/domain/schedules/missing", StatusCode::NOT_FOUND),
            ("/v2/domain/delivery-attempts?after=0", StatusCode::OK),
            ("/v2/domain/tasks/missing/actions", StatusCode::NOT_FOUND),
            ("/v2/domain/document-revisions/missing/content?offset=0", StatusCode::NOT_FOUND),
            ("/v2/domain/pull-requests/missing/diff", StatusCode::NOT_FOUND),
        ] {
            assert_eq!(client.get(format!("{base}{path}")).send().await.unwrap().status(),
                StatusCode::FORBIDDEN, "unsigned {path}");
            let timestamp = alondra_core::now_millis().to_string();
            let message = crate::auth::signed_message("GET", path, &timestamp, b"");
            let signature: Signature = key.sign(message.as_bytes());
            let response = client.get(format!("{base}{path}"))
                .header("x-alondra-device", &device.id)
                .header("x-alondra-timestamp", timestamp)
                .header("x-alondra-signature", STANDARD.encode(signature.to_der().as_bytes()))
                .send().await.unwrap();
            assert_eq!(response.status(), expected, "signed {path}");
            if expected == StatusCode::NOT_FOUND {
                let body: serde_json::Value = response.json().await.unwrap();
                assert_eq!(body["code"], "not_found", "handler must answer {path}");
            }
        }
        let merge_path = "/v2/domain/merge-authorizations/missing";
        assert_eq!(client.get(format!("{base}{merge_path}"))
            .header("authorization", format!("Bearer {}", "a".repeat(64)))
            .send().await.unwrap().status(), StatusCode::FORBIDDEN);
        let response = client.get(format!("{base}{merge_path}"))
            .header("authorization", format!("Bearer {bearer}"))
            .send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["code"], "not_found", "handler must answer merge authorization read");
        domain.revoke_agent("test-agent");
        assert_eq!(client.get(format!("{base}{merge_path}"))
            .header("authorization", format!("Bearer {bearer}"))
            .send().await.unwrap().status(), StatusCode::FORBIDDEN);
        assert_eq!(client.get(format!("{base}/v2/domain/capabilities"))
            .bearer_auth(&bearer).send().await.unwrap().status(), StatusCode::FORBIDDEN);
        server.abort();
        let _ = server.await;
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn committed_boot_requires_existing_database() {
        let home = std::env::temp_dir().join(format!(
            "alondra-v2-bootstrap-missing-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir(&home).unwrap();
        assert!(V2Only::open(&home, "epoch").is_err());
        assert!(
            !home.join("domain-v2.db").exists(),
            "must not create an empty authority on missing stage"
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[cfg(test)]
mod vertical_tests;
