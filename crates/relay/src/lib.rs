#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

mod activity;
mod auth;
pub mod jwt;
mod management;
mod operator;
mod pairing;
mod push;
mod testflight;
mod web_session;
mod web_assets;

use alondra_e2e::Envelope;
use auth::{Auth, Config, IrohNodeBinding, MacRegistration, PhoneDevice, PushRegistration};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, Method, Request, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{RwLock, Semaphore};

struct Server {
    auth: Auth,
    activity: activity::Store,
    management_keycloak: Option<management::keycloak::Keycloak>,
    web_session: Option<Arc<web_session::WebSession>>,
    web_assets: Option<web_assets::WebAssets>,
    apns: Option<push::Apns>,
    pairing: pairing::Mailbox,
    requests: Arc<Semaphore>,
    device_gate: Arc<RwLock<()>>,
    waitlist: management::waitlist::Limiter,
    epoch: String,
    /// Bearer the iroh relay presents to `/v1/iroh/access`; the route is absent without it.
    iroh_access_token: Option<String>,
    /// The iroh relay this relay admits endpoints to, announced to Macs and phones in bootstrap.
    iroh_relay: Option<String>,
    /// The `owner/repo` crash reports go to, announced in bootstrap when whoever runs the relay sets it.
    report_repo: Option<String>,
    /// The sign-in service apps and the CLI use with this relay, announced in bootstrap.
    identity: Option<Identity>,
    testflight_url: Option<String>,
    operator: operator::Operator,
    #[cfg(test)]
    test_directory: Option<crate::test_support::Directory>,
}

fn router(server: Arc<Server>) -> Router {
    Router::new()
        .merge(management::http::routes())
        .merge(management::waitlist::routes())
        .merge(web_session::routes())
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/v1/bootstrap", get(bootstrap))
        .route("/v1/pairings", post(pairing_open))
        .route("/v1/pairings/{id}", get(pairing_pending))
        .route("/v1/pairings/{id}/candidates", post(pairing_candidate))
        .route("/v1/pairings/{id}/approval", post(pairing_approve).get(pairing_approval))
        .route("/v1/macs", post(enroll_mac).get(list_macs))
        .route("/v1/macs/{id}", axum::routing::delete(revoke_mac))
        .route("/v1/devices", post(enroll))
        .route("/v1/devices/{id}", axum::routing::delete(revoke))
        .route("/v1/devices/{id}/iroh-node", put(bind_device_iroh_node))
        .route("/v1/iroh-node", put(bind_iroh_node).delete(unbind_iroh_node))
        .route("/v1/iroh/access", post(iroh_access))
        .route("/v1/push/register", post(register_push))
        .route("/v1/push/unregister", post(unregister_push))
        .route("/v1/mac/push", post(send_push))
        .route("/v1/activity/register", post(register_activity))
        .route("/v1/activity/unregister", post(unregister_activity))
        .route("/v1/mac/activity", post(report_activity))
        .route("/v1/requests", any(mailbox_removed))
        .route("/v1/requests/{*rest}", any(mailbox_removed))
        .route("/v1/changes", any(mailbox_removed))
        .route("/v1/work", any(mailbox_removed))
        .route("/v1/work/{*rest}", any(mailbox_removed))
        .fallback(web_assets::serve)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(server.clone(),web_session::middleware))
        .layer(middleware::from_fn_with_state(
            server.clone(),
            limit_and_headers,
        ))
        .with_state(server)
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"ok":true})))
}
async fn ready(State(server): State<Arc<Server>>) -> impl IntoResponse {
    if server.auth.jwks_ready() && !server.auth.removals_pending() {
        (StatusCode::OK, Json(json!({"ready":true})))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ready":false})),
        )
    }
}
/// Anyone may read the bootstrap. A Mac that signs it also reads what its account removed from it;
/// a signature that does not hold, such as one over an epoch the relay no longer has, only gets
/// the public answer, so that the Mac can always learn the current epoch.
async fn bootstrap(State(server): State<Arc<Server>>, headers: HeaderMap, method: Method, uri: Uri) -> ApiResult {
    let mut body = bootstrap_body(&server.epoch, server.iroh_relay.as_deref(), server.report_repo.as_deref(), server.identity.as_ref());
    if headers.contains_key("x-device-id") {
        let _gate = server.device_gate.read().await;
        match server.auth.mac_removals(&headers, &method, &uri, now_ms()) {
            Ok(removals) => {
                let clients: Vec<_> = removals.clients.into_iter()
                    .map(|(client_id, removed_at_ms)| json!({"client_id": client_id, "removed_at_ms": removed_at_ms}))
                    .collect();
                body["removals"] = json!({"mac_removed": removals.signing_key.is_some(), "clients": clients});
            }
            Err(failure) if failure.downcast_ref::<auth::AccountGateError>().is_some() => {
                return Err(authentication_error(failure));
            }
            Err(_) => {}
        }
    }
    Ok(Json(body).into_response())
}

fn bootstrap_body(epoch: &str, iroh_relay: Option<&str>, report_repo: Option<&str>, identity: Option<&Identity>) -> Value {
    let mut body = json!({"protocol":1,"epoch":epoch});
    if let Some(iroh_relay) = iroh_relay {
        body["iroh_relay"] = json!(iroh_relay);
    }
    if let Some(report_repo) = report_repo {
        body["report_repo"] = json!(report_repo);
    }
    if let Some(identity) = identity {
        body["identity"] = json!(identity);
    }
    body
}

/// The OIDC issuer whose tokens this relay accepts and the client each kind of app signs in as.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
struct Identity {
    issuer: String,
    apple_client_id: String,
    cli_client_id: String,
}

/// `ALONDRA_APPLE_CLIENT_ID` and `ALONDRA_CLI_CLIENT_ID`: both, or neither for a relay that
/// announces no sign-in service.
fn identity(issuer: &str, apple: Option<String>, cli: Option<String>) -> anyhow::Result<Option<Identity>> {
    let id = |value: Option<String>| value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    let valid = |id: &str| id.len() <= 255 && id.chars().all(|c| c.is_ascii_graphic());
    match (id(apple), id(cli)) {
        (None, None) => Ok(None),
        (Some(apple_client_id), Some(cli_client_id)) => {
            anyhow::ensure!(valid(&apple_client_id) && valid(&cli_client_id), "OIDC client ids must be printable ASCII");
            Ok(Some(Identity { issuer: issuer.to_owned(), apple_client_id, cli_client_id }))
        }
        _ => anyhow::bail!("ALONDRA_APPLE_CLIENT_ID and ALONDRA_CLI_CLIENT_ID must be set together"),
    }
}

/// `ALONDRA_REPORT_REPO`: a GitHub `owner/repo`, or unset for none.
fn report_repo(value: Option<String>) -> anyhow::Result<Option<String>> {
    let Some(value) = value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()) else { return Ok(None) };
    let name = |part: &str| !part.is_empty() && part.len() <= 100 && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    let valid = value.split_once('/').is_some_and(|(owner, repo)| name(owner) && name(repo) && !repo.contains('/'));
    anyhow::ensure!(valid, "ALONDRA_REPORT_REPO must be a GitHub owner/repo");
    Ok(Some(value))
}

async fn pairing_open(
    State(server): State<Arc<Server>>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes,
) -> ApiResult {
    if body.len() > 2048 { return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large")); }
    let mac = server.auth.authenticate_mac(&headers, &method, &uri, &body, now_ms()).await
        .map_err(authentication_error)?;
    let request: pairing::SessionRequest = serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_pairing_session"))?;
    if request.mac_id != mac.device_id { return Err(unauthorized()); }
    server.pairing.open(request, &mac.owner_sub, now_ms())
        .map_err(|code| error(StatusCode::CONFLICT, code))?;
    Ok(StatusCode::CREATED.into_response())
}

async fn pairing_pending(
    State(server): State<Arc<Server>>, Path(id): Path<String>, headers: HeaderMap,
    method: Method, uri: Uri,
) -> ApiResult {
    let mac = server.auth.authenticate_mac(&headers, &method, &uri, &[], now_ms()).await
        .map_err(authentication_error)?;
    match server.pairing.mac(&id, &mac.device_id, now_ms()) {
        Ok(Some(candidate)) => Ok(Json(candidate).into_response()),
        Ok(None) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(_) => Err(error(StatusCode::NOT_FOUND, "pairing_not_found")),
    }
}

async fn pairing_candidate(
    State(server): State<Arc<Server>>, Path(id): Path<String>, headers: HeaderMap,
    method: Method, uri: Uri, body: Bytes,
) -> ApiResult {
    if body.len() > 16 * 1024 { return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large")); }
    let request: pairing::CandidateRequest = serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_candidate"))?;
    let (mac_id, owner) = server.pairing.route(&id, now_ms())
        .map_err(|_| error(StatusCode::NOT_FOUND, "pairing_not_found"))?;
    let who = server.auth.authenticate_pairing_candidate(
        &headers, &method, &uri, &body, now_ms(), &request.device_id, &request.signing_key, &mac_id,
    ).await.map_err(authentication_error)?;
    if who.owner_sub != owner { return Err(unauthorized()); }
    server.pairing.candidate(&id, request, &who.owner_sub, now_ms())
        .map_err(|code| error(StatusCode::CONFLICT, code))?;
    Ok(StatusCode::ACCEPTED.into_response())
}

async fn pairing_approve(
    State(server): State<Arc<Server>>, Path(id): Path<String>, headers: HeaderMap,
    method: Method, uri: Uri, body: Bytes,
) -> ApiResult {
    if body.len() > 4096 { return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large")); }
    let mac = server.auth.authenticate_mac(&headers, &method, &uri, &body, now_ms()).await
        .map_err(authentication_error)?;
    let proof: pairing::ApprovalProof = serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_approval"))?;
    let mac_record = server.auth.mac_public_identity(&mac.device_id)
        .map_err(authentication_error)?;
    if proof.approval.mac_signing_key != mac_record.signing_key
        || proof.approval.mac_agreement_key != mac_record.agreement_key
    { return Err(error(StatusCode::BAD_REQUEST, "approval_mac_key_mismatch")); }
    pairing::verify_proof(&proof)
        .map_err(|code| error(StatusCode::BAD_REQUEST, code))?;
    server.auth.phone_for_mac(&proof.approval.device_id, &mac)
        .map_err(|_| error(StatusCode::CONFLICT, "device_not_enrolled"))?;
    server.pairing.approve(&id, proof, &mac.device_id, &mac.owner_sub, now_ms())
        .map_err(|code| error(StatusCode::CONFLICT, code))?;
    Ok(StatusCode::ACCEPTED.into_response())
}

async fn pairing_approval(
    State(server): State<Arc<Server>>, Path(id): Path<String>, headers: HeaderMap,
    method: Method, uri: Uri,
) -> ApiResult {
    let device_id = headers.get("x-device-id").and_then(|v| v.to_str().ok())
        .ok_or_else(unauthorized)?;
    let (mac_id, key) = server.pairing.candidate_key(&id, device_id, now_ms())
        .map_err(|_| error(StatusCode::NOT_FOUND, "pairing_not_found"))?;
    let who = server.auth.authenticate_pairing_candidate(
        &headers, &method, &uri, &[], now_ms(), device_id, &key, &mac_id,
    ).await.map_err(authentication_error)?;
    match server.pairing.approval(&id, device_id, &who.owner_sub, now_ms()) {
        Ok(Some(proof)) => Ok(Json(proof).into_response()),
        Ok(None) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(_) => Err(error(StatusCode::NOT_FOUND, "pairing_not_found")),
    }
}

async fn enroll_mac(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    if body.len() > 4096 {
        return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"));
    }
    let request: MacRegistration = serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_mac_registration"))?;
    let mac = server
        .auth
        .authorize_mac_enrollment(&headers, &method, &uri, &body, now_ms(), request)
        .await
        .map_err(authentication_error)?;
    let created = server.auth.enroll_mac(mac).map_err(|cause| {
        if cause.is::<auth::AccountGateError>() {
            authentication_error(cause)
        } else {
            enrollment_error(cause, "mac_enrollment_failed")
        }
    })?;
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(json!({"ok":true})),
    )
        .into_response())
}

/// OIDC authenticates ownership for discovery, never authorizes content access. The iroh
/// endpoint lets the owner's apps dial a Mac whose pairing predates it; the Mac still proves
/// itself with its pairing key on that connection.
async fn list_macs(State(server): State<Arc<Server>>, headers: HeaderMap) -> ApiResult {
    let owner = server.auth.account_owner(&headers, now_ms()).await.map_err(authentication_error)?;
    let _gate = server.device_gate.read().await;
    let macs = server.auth.macs_for_owner(&owner)
        .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "mac_directory_unavailable"))?;
    let macs: Vec<Value> = macs.into_iter().map(|(id, iroh)| match iroh {
        Some(endpoint) => json!({"mac_id":id, "iroh_endpoint":endpoint}),
        None => json!({"mac_id":id}),
    }).collect();
    Ok(Json(json!({"macs":macs})).into_response())
}

async fn revoke_mac(
    State(server): State<Arc<Server>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
) -> ApiResult {
    let _gate = server.device_gate.write().await;
    let who = server
        .auth
        .authenticate_mac(&headers, &method, &uri, &[], now_ms()).await
        .map_err(authentication_error)?;
    if who.device_id != id {
        return Err(unauthorized());
    }
    server
        .auth
        .revoke_mac(&who)
        .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "mac_revocation_failed"))?;
    activity::after_revocation(&server).await;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn limit_and_headers(
    State(server): State<Arc<Server>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let permit = match server.requests.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "server_busy").into_response(),
    };
    let mut response = next.run(request).await;
    drop(permit);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    response
}

const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_PUSH_LIFETIME_MS: u64 = 10 * 60_000;

type ApiResult = Result<Response, (StatusCode, Json<Value>)>;
fn error(status: StatusCode, code: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error":code})))
}
fn unauthorized() -> (StatusCode, Json<Value>) {
    error(StatusCode::UNAUTHORIZED, "unauthorized")
}
/// Apps and daemons from before iroh read a 404 as "this relay has no mailbox"; a 410 would
/// read as an outcome they cannot know.
async fn mailbox_removed() -> (StatusCode, Json<Value>) {
    error(StatusCode::NOT_FOUND, "mailbox_removed")
}

async fn enroll(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let _gate = server.device_gate.write().await;
    let mac = server
        .auth
        .authenticate_mac(&headers, &method, &uri, &body, now_ms()).await
        .map_err(authentication_error)?;
    if body.len() > 4096 {
        return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"));
    }
    let device: PhoneDevice = serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_device"))?;
    server.auth.require_live_route(&mac.owner_sub,&mac.device_id,Some(&device.device_id)).await.map_err(authentication_error)?;
    server
        .auth
        .enroll(&mac, device)
        .map_err(|cause| enrollment_error(cause, "device_enrollment_failed"))?;
    Ok((StatusCode::CREATED, Json(json!({"ok":true}))).into_response())
}

async fn revoke(
    State(server): State<Arc<Server>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
) -> ApiResult {
    let _gate = server.device_gate.write().await;
    let mac = server
        .auth
        .authenticate_mac(&headers, &method, &uri, &[], now_ms()).await
        .map_err(authentication_error)?;
    server
        .auth
        .revoke(&mac, &id)
        .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "revocation_failed"))?;
    activity::after_revocation(&server).await;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// The Mac or phone signs for itself; a phone also needs its account token.
async fn iroh_node_owner(
    server: &Server,
    headers: &HeaderMap,
    method: &Method,
    uri: &Uri,
    body: &[u8],
) -> Result<auth::Identity, (StatusCode, Json<Value>)> {
    let id = headers.get("x-device-id").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if server.auth.is_mac(id) {
        let mac = server
            .auth
            .authenticate_mac(headers, method, uri, body, now_ms())
            .await
            .map_err(authentication_error)?;
        server
            .auth
            .require_live_route(&mac.owner_sub, &mac.device_id, None)
            .await
            .map_err(authentication_error)?;
        Ok(mac)
    } else {
        server
            .auth
            .authenticate_phone(headers, method, uri, body, now_ms())
            .await
            .map_err(authentication_error)
    }
}

async fn bind_iroh_node(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    if body.len() > 1024 {
        return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"));
    }
    let _gate = server.device_gate.write().await;
    let who = iroh_node_owner(&server, &headers, &method, &uri, &body).await?;
    let binding: IrohNodeBinding = serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_registration"))?;
    server
        .auth
        .bind_iroh_node(&who, binding)
        .map_err(|cause| enrollment_error(cause, "iroh_node_conflict"))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn bind_device_iroh_node(
    State(server): State<Arc<Server>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    if body.len() > 1024 {
        return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"));
    }
    let _gate = server.device_gate.write().await;
    let mac = server
        .auth
        .authenticate_mac(&headers, &method, &uri, &body, now_ms())
        .await
        .map_err(authentication_error)?;
    server
        .auth
        .require_live_route(&mac.owner_sub, &mac.device_id, Some(&id))
        .await
        .map_err(authentication_error)?;
    let binding: auth::DeviceIrohNodeBinding = serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_registration"))?;
    server
        .auth
        .bind_device_iroh_node(&mac, &id, binding)
        .map_err(|cause| match cause.downcast_ref::<auth::EnrollmentError>() {
            Some(_) => enrollment_error(cause, "iroh_node_conflict"),
            None if cause.is::<auth::RegistryUnavailable>() => error(StatusCode::SERVICE_UNAVAILABLE, "registry_unavailable"),
            None => error(StatusCode::NOT_FOUND, "unknown_device"),
        })?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn unbind_iroh_node(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
) -> ApiResult {
    let _gate = server.device_gate.write().await;
    let who = iroh_node_owner(&server, &headers, &method, &uri, &[]).await?;
    server
        .auth
        .unbind_iroh_node(&who)
        .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "registry_unavailable"))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// iroh-relay's `access.http` check: it relays an endpoint only on `200` with body `true`.
async fn iroh_access(State(server): State<Arc<Server>>, headers: HeaderMap) -> Response {
    use subtle::ConstantTimeEq;
    let Some(expected) = &server.iroh_access_token else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !bool::from(presented.as_bytes().ct_eq(expected.as_bytes())) {
        return (StatusCode::UNAUTHORIZED, "false").into_response();
    }
    let denied = (StatusCode::FORBIDDEN, "false").into_response();
    let Some(node) = headers.get("x-iroh-nodeid").and_then(|v| v.to_str().ok()) else {
        return denied;
    };
    let _gate = server.device_gate.read().await;
    let holder = match server.auth.iroh_node_holder(node) {
        Ok(Some(holder)) => holder,
        Ok(None) => return denied,
        Err(_) => return (StatusCode::SERVICE_UNAVAILABLE, "false").into_response(),
    };
    match server
        .auth
        .require_live_route(&holder.owner_sub, &holder.mac_id, holder.client_id.as_deref())
        .await
    {
        Ok(()) => (StatusCode::OK, "true").into_response(),
        Err(cause) if matches!(cause.downcast_ref(), Some(auth::AccountGateError::Denied)) => {
            denied
        }
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "false").into_response(),
    }
}

async fn register_push(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    if server.apns.is_none() {
        return Err(error(StatusCode::SERVICE_UNAVAILABLE, "push_unavailable"));
    }
    let _gate = server.device_gate.read().await;
    let who = server
        .auth
        .authenticate_phone(&headers, &method, &uri, &body, now_ms())
        .await
        .map_err(authentication_error)?;
    if body.len() > 1024 {
        return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"));
    }
    let registration: PushRegistration = serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_registration"))?;
    push::validate_registration(&registration)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_registration"))?;
    server
        .auth
        .register_push(&who.device_id, registration, Auth::bearer_push_session(&headers))
        .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "push_registration_failed"))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Only a registration the relay ended on purpose is gone for good; the Mac may then forget
/// the phone's binding.
fn missing_push_registration(ended: bool) -> (StatusCode, Json<Value>) {
    if ended {
        error(StatusCode::GONE, "push_registration_gone")
    } else {
        error(StatusCode::NOT_FOUND, "push_registration_not_found")
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PushUnregistration {
    push_binding: String,
}

/// No account token or device signature: a phone that signed out or lost its session still
/// has to be able to stop the pushes it can no longer read.
async fn unregister_push(State(server): State<Arc<Server>>, body: Bytes) -> ApiResult {
    if body.len() > 256 {
        return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"));
    }
    let request: PushUnregistration = serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_unregistration"))?;
    if !push::valid_binding(&request.push_binding) {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_unregistration"));
    }
    let _gate = server.device_gate.read().await;
    server
        .auth
        .unregister_push(&request.push_binding)
        .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "push_unregistration_failed"))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

const PUSH_SESSION_SWEEP: Duration = Duration::from_secs(15 * 60);

/// Pushes registered by a Keycloak offline session stop once that session ends: signed out
/// elsewhere, revoked by an administrator or expired. An unreachable Keycloak removes nothing.
async fn sweep_ended_push_sessions(
    server: &Server,
    keycloak: &management::keycloak::Keycloak,
) {
    for ((owner, client_id), registrations) in server.auth.push_sessions() {
        let live = match keycloak.offline_session_ids(&owner, &client_id).await {
            Ok(live) => live,
            Err(failure) => {
                eprintln!("alondra-relay: could not list offline sessions: {failure:#}");
                continue;
            }
        };
        for (device_id, sid) in registrations {
            if live.contains(&sid) {
                continue;
            }
            let _gate = server.device_gate.read().await;
            if let Err(failure) = server.auth.forget_ended_session_push(&device_id, &sid) {
                eprintln!(
                    "alondra-relay: could not forget the push registration of device {device_id}: {failure:#}"
                );
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PushRequest {
    device_id: String,
    envelope: Envelope,
    #[serde(default)]
    collapse_id: Option<String>,
    #[serde(default)]
    push_type: push::PushType,
    #[serde(default)]
    badge: Option<u32>,
}

async fn send_push(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let apns = server
        .apns
        .as_ref()
        .ok_or_else(|| error(StatusCode::SERVICE_UNAVAILABLE, "push_unavailable"))?;
    // Keep the read gate through APNs acceptance: a completed revocation cannot be followed by a new send.
    let _gate = server.device_gate.read().await;
    let mac = server
        .auth
        .authenticate_mac(&headers, &method, &uri, &body, now_ms()).await
        .map_err(authentication_error)?;
    if body.len() > 4096 {
        return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"));
    }
    let request: PushRequest = serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_push"))?;
    let e = &request.envelope;
    let now = now_ms();
    let delivery = push::Delivery {
        push_type: request.push_type,
        collapse_id: request.collapse_id.clone(),
        badge: request.badge,
    };
    if !delivery.is_valid() {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_push"));
    }
    if e.kind != "push"
        || e.route != mac.device_id
        || e.sender != mac.device_id
        || e.recipient != request.device_id
        || e.expires_at_ms <= now
        || e.expires_at_ms > now.saturating_add(MAX_PUSH_LIFETIME_MS)
    {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_push_route"));
    }
    e.verify_signature(
        &server
            .auth
            .mac_key(&mac.device_id)
            .map_err(authentication_error)?,
        &mac.device_id,
        &request.device_id,
        &mac.device_id,
        now,
    )
    .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_envelope_signature"))?;
    let registration = server
        .auth
        .push_registration(&request.device_id)
        .ok_or_else(|| missing_push_registration(server.auth.push_ended(&request.device_id)))?;
    server
        .auth
        .phone_for_mac(&request.device_id, &mac)
        .map_err(|_| error(StatusCode::NOT_FOUND, "push_registration_not_found"))?;
    let payload_bytes = serde_json::to_vec(&push::payload_for(&delivery, e, &registration.push_binding))
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_push"))?;
    if payload_bytes.len() > 4096 {
        return Err(error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "apns_payload_too_large",
        ));
    }
    server.auth.require_live_route(&mac.owner_sub,&mac.device_id,Some(&request.device_id)).await.map_err(authentication_error)?;
    if let Err(failure) = apns.send(&registration, e, &delivery).await {
        eprintln!(
            "alondra-relay: APNs delivery to device {} failed: {failure:#}",
            request.device_id
        );
        if failure
            .downcast_ref::<push::ApnsRejection>()
            .is_some_and(push::ApnsRejection::registration_is_dead)
            && let Err(forget) = server
                .auth
                .forget_dead_push(&request.device_id, &registration.token)
        {
            eprintln!(
                "alondra-relay: could not forget the dead push registration of device {}: {forget:#}",
                request.device_id
            );
        }
        return Err(apns_failure_response(&failure));
    }
    Ok(StatusCode::ACCEPTED.into_response())
}

/// Signed by one of the phone's pairings. The tokens go only to the relay, never to a Mac.
async fn register_activity(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    if server.apns.is_none() {
        return Err(error(StatusCode::SERVICE_UNAVAILABLE, "push_unavailable"));
    }
    let _gate = server.device_gate.read().await;
    let who = server
        .auth
        .authenticate_phone(&headers, &method, &uri, &body, now_ms())
        .await
        .map_err(authentication_error)?;
    if body.len() > 4096 {
        return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"));
    }
    let registration: activity::Registration = serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_activity_registration"))?;
    registration
        .validate()
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_activity_registration"))?;
    let mac_id = server
        .auth
        .paired_mac_for_phone(&who.device_id, &who.owner_sub)
        .map_err(authentication_error)?;
    let phone_id = server
        .activity
        .register(&who.owner_sub, &who.device_id, &mac_id, registration)
        .map_err(activity_store_error)?;
    if server.activity.owes(&phone_id) {
        activity::push(&server, &phone_id).await;
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn unregister_activity(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let _gate = server.device_gate.read().await;
    let who = server
        .auth
        .authenticate_phone(&headers, &method, &uri, &body, now_ms())
        .await
        .map_err(authentication_error)?;
    if body.len() > 256 {
        return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"));
    }
    let request: activity::Unregistration = serde_json::from_slice(&body)
        .ok()
        .filter(|request: &activity::Unregistration| activity::valid_phone_id(&request.phone_id))
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "invalid_activity_unregistration"))?;
    server
        .activity
        .unregister(&who.owner_sub, &who.device_id, &request.phone_id)
        .map_err(activity_store_error)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn activity_store_error(failure: anyhow::Error) -> (StatusCode, Json<Value>) {
    if failure.is::<activity::Conflict>() {
        error(StatusCode::CONFLICT, "activity_registration_conflict")
    } else {
        error(StatusCode::SERVICE_UNAVAILABLE, "activity_registration_failed")
    }
}

/// Never answers 404, which the daemon reads as a relay without this route and sleeps an hour;
/// a phone the Mac can no longer reach is `410 activity_registration_gone`. APNs failures are
/// not the Mac's to retry: the report is kept and goes out with the next one.
async fn report_activity(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    if server.apns.is_none() {
        return Err(error(StatusCode::SERVICE_UNAVAILABLE, "push_unavailable"));
    }
    let _gate = server.device_gate.read().await;
    let mac = server
        .auth
        .authenticate_mac(&headers, &method, &uri, &body, now_ms())
        .await
        .map_err(authentication_error)?;
    if body.len() > 4096 {
        return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"));
    }
    let request: activity::ReportRequest = serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_activity"))?;
    let e = &request.envelope;
    let now = now_ms();
    if e.kind != "activity"
        || e.route != mac.device_id
        || e.sender != mac.device_id
        || e.recipient != request.device_id
        || e.expires_at_ms <= now
        || e.expires_at_ms > now.saturating_add(MAX_PUSH_LIFETIME_MS)
    {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_activity_route"));
    }
    e.verify_signature(
        &server.auth.mac_key(&mac.device_id).map_err(authentication_error)?,
        &mac.device_id,
        &request.device_id,
        &mac.device_id,
        now,
    )
    .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_envelope_signature"))?;
    let gone = || error(StatusCode::GONE, "activity_registration_gone");
    server.auth.phone_for_mac(&request.device_id, &mac).map_err(|failure| {
        if failure.is::<auth::RegistryUnavailable>() {
            error(StatusCode::SERVICE_UNAVAILABLE, "registry_unavailable")
        } else {
            gone()
        }
    })?;
    server
        .auth
        .require_live_route(&mac.owner_sub, &mac.device_id, Some(&request.device_id))
        .await
        .map_err(authentication_error)?;
    match server.activity.record(&mac.device_id, request) {
        Ok(activity::Recorded::Stored { phone_id }) => activity::push(&server, &phone_id).await,
        Ok(activity::Recorded::Held) => {}
        Ok(activity::Recorded::Gone) => return Err(gone()),
        Err(_) => return Err(error(StatusCode::SERVICE_UNAVAILABLE, "activity_unavailable")),
    }
    Ok(StatusCode::ACCEPTED.into_response())
}

fn apns_failure_response(failure: &anyhow::Error) -> (StatusCode, Json<Value>) {
    match failure.downcast_ref::<push::ApnsRejection>() {
        Some(rejection) if rejection.registration_is_dead() => {
            error(StatusCode::GONE, "push_registration_gone")
        }
        _ => error(StatusCode::BAD_GATEWAY, "apns_delivery_failed"),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Runs the relay with the configuration `ALONDRA_RELAY_CONFIG` names and the `ALONDRA_*`
/// environment, until it fails.
pub async fn run() -> anyhow::Result<()> {
    let path = std::env::var("ALONDRA_RELAY_CONFIG")?;
    let config: Config = serde_json::from_slice(&std::fs::read(path)?)?;
    let listen = config.listen.clone();
    let apns = config.apns.clone().map(push::Apns::open).transpose()?;
    let mut auth = Auth::open(config).await?;
    if let Ok(database_url)=std::env::var("ALONDRA_MANAGEMENT_DATABASE_URL") {
        let management=management::Management::open(&database_url,auth.config.issuer.clone()).await?;
        management.bootstrap(&std::env::var("ALONDRA_MANAGEMENT_ADMIN_SUB")?,"Administrator").await?;
        auth.management=Some(management);
    }
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let epoch = URL_SAFE_NO_PAD.encode(auth.server_epoch());
    let management_keycloak=if auth.management.is_some() {
        let keycloak=management::keycloak::Keycloak::new(
            auth.config.issuer.clone(),
            std::env::var("ALONDRA_MANAGEMENT_KC_ADMIN_REALM_URL")?.parse()?,
            std::env::var("ALONDRA_MANAGEMENT_KC_TOKEN_URL")?.parse()?,
            std::env::var("ALONDRA_MANAGEMENT_KC_CLIENT_ID")?,
            std::fs::read_to_string(std::env::var("ALONDRA_MANAGEMENT_KC_SECRET_FILE")?)?.trim().into())?;
        keycloak.verify_realm().await?;Some(keycloak)
    } else {None};
    let operator = operator::Operator::from_env()?;
    let operator_listen = std::env::var("ALONDRA_OPERATOR_LISTEN").ok();
    anyhow::ensure!(
        operator_listen.is_none() || operator.has_token(),
        "ALONDRA_OPERATOR_LISTEN needs ALONDRA_OPERATOR_TOKEN_FILE"
    );
    if operator.follows_source() {
        auth.await_removal_source();
    }
    let web_session = web_session::WebSession::from_env(&auth)?;
    let web_assets = web_assets::WebAssets::from_env(web_session.is_some())?;
    let iroh_access_token = match std::env::var("ALONDRA_IROH_ACCESS_TOKEN_FILE") {
        Ok(path) => {
            let token = std::fs::read_to_string(path)?.trim().to_owned();
            anyhow::ensure!(token.len() >= 32, "iroh access token must have at least 32 characters");
            Some(token)
        }
        Err(_) => None,
    };
    let iroh_relay = match std::env::var("ALONDRA_IROH_RELAY_URL") {
        Ok(url) => {
            let parsed = reqwest::Url::parse(&url)?;
            let loopback = parsed.scheme() == "http" && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost"));
            anyhow::ensure!(
                (parsed.scheme() == "https" && parsed.host_str().is_some()) || loopback,
                "ALONDRA_IROH_RELAY_URL must be an https URL (loopback HTTP allowed for tests)"
            );
            Some(url)
        }
        Err(_) => None,
    };
    let identity = identity(&auth.config.issuer, std::env::var("ALONDRA_APPLE_CLIENT_ID").ok(), std::env::var("ALONDRA_CLI_CLIENT_ID").ok())?;
    let server = Arc::new(Server {
        activity: activity::Store::open(&auth.config.device_registry_path)?,
        auth,
        web_assets,
        web_session,
        management_keycloak,
        apns,
        pairing: pairing::Mailbox::default(),
        requests: Arc::new(Semaphore::new(128)),
        device_gate: Arc::new(RwLock::new(())),
        waitlist: management::waitlist::Limiter::from_env()?,
        epoch,
        iroh_access_token,
        iroh_relay,
        report_repo: report_repo(std::env::var("ALONDRA_REPORT_REPO").ok())?,
        identity,
        testflight_url: testflight::from_env()?,
        operator,
        #[cfg(test)]
        test_directory: None,
    });
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    let operator_listener = match &operator_listen {
        Some(listen) => Some(tokio::net::TcpListener::bind(listen).await?),
        None => None,
    };
    if server.operator.follows_source() {
        tokio::spawn(operator::follow_source(server.clone()));
    }
    if server.auth.management.is_some() {tokio::spawn(management::worker::run(server.clone()));}
    tokio::spawn(activity::retry_owed(server.clone()));
    if let Some(web)=&server.web_session {tokio::spawn(web_session::reconcile(web.clone()));}
    if server.management_keycloak.is_some() && server.apns.is_some() {
        let sweeping = server.clone();
        tokio::spawn(async move {
            let Some(keycloak) = &sweeping.management_keycloak else { return };
            loop {
                tokio::time::sleep(PUSH_SESSION_SWEEP).await;
                sweep_ended_push_sessions(&sweeping, keycloak).await;
            }
        });
    }
    let refreshing = server.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5 * 60)).await;
            if refreshing.auth.refresh_jwks().await.is_err() {
                eprintln!("alondra-relay: JWKS refresh failed; cached keys expire after one hour");
            }
        }
    });
    println!("alondra-relay listening on {listen}");
    let public = axum::serve(
        listener,
        router(server.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    );
    match operator_listener {
        Some(operator_listener) => {
            println!("alondra-relay operator API listening on {}", operator_listen.unwrap_or_default());
            tokio::try_join!(public, axum::serve(operator_listener, operator::router(server)))?;
        }
        None => public.await?,
    }
    Ok(())
}

#[cfg(test)]
mod http_tests {
    include!("pg_fault_tests.rs");
    include!("activity_http_tests.rs");
    include!("operator_tests.rs");
    use super::*;
    use alondra_e2e::{Context, Envelope, sign_http_challenge};
    use sha2::{Digest, Sha256};
    use auth::PhoneDevice;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use p256::elliptic_curve::sec1::ToSec1Point;
    use p256::{SecretKey, ecdsa::SigningKey};
    use ring::{
        rand::SystemRandom,
        signature::{RSA_PKCS1_SHA256, RsaKeyPair},
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_REGISTRY: AtomicU64 = AtomicU64::new(0);

    fn key_bytes(key: &SigningKey) -> String {
        URL_SAFE_NO_PAD.encode(key.verifying_key().to_sec1_point(false).as_bytes())
    }

    fn jwt_with(sub: &str, now: u64, kid: &str, private_key: &[u8]) -> String {
        let h =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"alg":"RS256","kid":kid})).unwrap());
        let p = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "iss":"https://id.example/realms/alondra", "aud":"alondra-relay", "sub":sub,
                "exp":now / 1000 + 3600
            }))
            .unwrap(),
        );
        let signed = format!("{h}.{p}");
        let key = RsaKeyPair::from_pkcs8(private_key).unwrap();
        let mut sig = vec![0; key.public().modulus_len()];
        key.sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            signed.as_bytes(),
            &mut sig,
        )
        .unwrap();
        format!("{signed}.{}", URL_SAFE_NO_PAD.encode(sig))
    }

    fn jwt(sub: &str, now: u64) -> String {
        jwt_with(
            sub,
            now,
            "test-key",
            include_bytes!("../tests/fixtures/rsa-test-only.pkcs8.der"),
        )
    }

    #[allow(clippy::too_many_arguments)] // Mirrors the signed HTTP request inputs in integration tests.
    fn sign(
        epoch: &[u8; 16],
        builder: reqwest::RequestBuilder,
        key: &SigningKey,
        id: &str,
        method: &str,
        path: &str,
        body: &[u8],
        nonce: u8,
    ) -> reqwest::RequestBuilder {
        let timestamp = now_ms();
        let nonce_bytes = [nonce; 16];
        let signature =
            sign_http_challenge(key, id, method, path, epoch, body, timestamp, &nonce_bytes)
                .unwrap();
        builder
            .header("x-device-id", id)
            .header("x-relay-epoch", URL_SAFE_NO_PAD.encode(epoch))
            .header("x-device-timestamp-ms", timestamp.to_string())
            .header("x-device-nonce", URL_SAFE_NO_PAD.encode(nonce_bytes))
            .header("x-device-signature", signature)
    }

    async fn test_server(
        mac_key: &SigningKey,
    ) -> (String, Arc<Server>, tokio::task::JoinHandle<()>) {
        test_server_with_apns(mac_key, None).await
    }

    async fn test_server_with_apns(
        mac_key: &SigningKey,
        apns_endpoint: Option<String>,
    ) -> (String, Arc<Server>, tokio::task::JoinHandle<()>) {
        test_server_with_management(mac_key,apns_endpoint,None).await
    }

    async fn test_server_with_management(mac_key:&SigningKey,apns_endpoint:Option<String>,management:Option<management::Management>)->(String,Arc<Server>,tokio::task::JoinHandle<()>) {
        test_server_with_iroh_access(mac_key,apns_endpoint,management,Some(TEST_IROH_ACCESS_TOKEN.into())).await
    }

    const TEST_IROH_ACCESS_TOKEN: &str = "test-iroh-access-token-0123456789abcdef";
    const TEST_IROH_RELAY: &str = "https://iroh.example.com";

    async fn test_server_with_iroh_access(mac_key:&SigningKey,apns_endpoint:Option<String>,management:Option<management::Management>,iroh_access_token:Option<String>)->(String,Arc<Server>,tokio::task::JoinHandle<()>) {
        let directory = crate::test_support::Directory::new();
        let registry = directory.parent().unwrap().join(format!(
            "alondra-relay-test-registry-{}-{}-{}.json",
            std::process::id(),
            now_ms(),
            NEXT_TEST_REGISTRY.fetch_add(1, Ordering::Relaxed)
        ));
        let config = Config {
            listen: "127.0.0.1:0".into(),
            issuer: "https://id.example/realms/alondra".into(),
            audience: "alondra-relay".into(),
            jwks_url: "http://127.0.0.1:1/jwks".into(),
            allow_internal_http_jwks: false,
            owner_sub: "owner".into(),
            mac_id: "mac-1".into(),
            mac_signing_key: key_bytes(mac_key),
            device_registry_path: registry,
            apns: None,
        };
        let mut auth = Auth::test(config, include_bytes!("../tests/fixtures/jwks.json")).unwrap();
        auth.management=management;
        let epoch = URL_SAFE_NO_PAD.encode(auth.server_epoch());
        let server = Arc::new(Server {
            activity: activity::Store::open(&auth.config.device_registry_path).unwrap(),
            auth,
            management_keycloak: None,
            web_session: None,
            web_assets: None,
            apns: apns_endpoint.map(push::Apns::test),
            pairing: pairing::Mailbox::default(),
            requests: Arc::new(Semaphore::new(128)),
            device_gate: Arc::new(RwLock::new(())),
            waitlist: management::waitlist::Limiter::new(None),
            epoch,
            iroh_relay: iroh_access_token.as_ref().map(|_| TEST_IROH_RELAY.to_owned()),
            iroh_access_token,
            report_repo: None,
            identity: None,
            testflight_url: None,
            operator: operator::Operator::default(),
            #[cfg(test)]
            test_directory: Some(directory),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let serving = server.clone();
        let join = tokio::spawn(async move {
            axum::serve(
                listener,
                router(serving).into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        (url, server, join)
    }

    #[tokio::test]
    #[ignore="requires disposable local PostgreSQL"]
    async fn management_http_device_labels_are_owned_and_do_not_change_trust()->anyhow::Result<()> {
        let (db,admin,schema)=management::tests::fixture_for_issuer("https://id.example/realms/alondra").await?;
        for (email,subject) in [("label-first@example.com","owner"),("label-second@example.com","other")] {
            let invite=db.create_invitation(admin.id,email,300).await?;
            db.reserve_acceptance(&invite.token,"Owner").await?.complete(subject).await?;
        }
        let mac_key=SigningKey::from_slice(&[21u8;32]).unwrap();let phone_key=SigningKey::from_slice(&[22u8;32]).unwrap();
        let (url,server,join)=test_server_with_management(&mac_key,None,Some(db.clone())).await;
        server.auth.enroll_mac(auth::MacDevice{mac_id:"mac-other".into(),owner_sub:"other".into(),signing_key:key_bytes(&mac_key),agreement_key:key_bytes(&phone_key)})?;
        for (mac,owner,id) in [("mac-1","owner","client-first"),("mac-other","other","client-other")] {
            server.auth.enroll(&auth::Identity{device_id:mac.into(),owner_sub:owner.into()},PhoneDevice{device_id:id.into(),owner_sub:owner.into(),signing_key:key_bytes(&phone_key)})?;
        }
        let client=reqwest::Client::new();let token=jwt("owner",now_ms());
        let rename=|kind:&str,id:&str,key:&str,body:Value|client.post(format!("{url}/v1/account/devices/{kind}/{id}/label")).bearer_auth(&token).header("Idempotency-Key",key).json(&body);
        for (kind,id) in [("macs","mac-other"),("clients","client-other")] {
            let response=rename(kind,id,"foreign-label-request-001",json!({"label":"Stolen","expected_revision":0})).send().await?;
            assert_eq!(response.status(),StatusCode::NOT_FOUND,"foreign aliases must not be writable");
        }
        for (kind,id) in [("macs","mac-1"),("clients","client-first")] {
            let key=format!("own-label-request-{kind}-0001");
            let body=json!({"label":"  Cafe\u{301}  ","expected_revision":0});
            let response=rename(kind,id,&key,body.clone()).send().await?;
            assert_eq!(response.status(),StatusCode::OK);
            assert_eq!(response.json::<Value>().await?,json!({"label":"Café","label_revision":1}));
            let replay=rename(kind,id,&key,body).send().await?;
            assert_eq!(replay.status(),StatusCode::CONFLICT);
            assert_eq!(replay.json::<Value>().await?["error"]["code"],"action_already_applied");
            let stale=rename(kind,id,&format!("stale-label-{kind}-request-001"),json!({"label":"Overwrite","expected_revision":0})).send().await?;
            assert_eq!(stale.status(),StatusCode::CONFLICT);
            assert_eq!(stale.json::<Value>().await?["error"]["code"],"state_conflict");
        }
        for value in [json!({"label":"hidden\u{200d}","expected_revision":1}),json!({"label":"x".repeat(129),"expected_revision":1}),json!({"expected_revision":1})] {
            assert_eq!(rename("macs","mac-1","invalid-label-request-0001",value).send().await?.status(),StatusCode::UNPROCESSABLE_ENTITY);
        }
        let listing:Value=client.get(format!("{url}/v1/account/devices")).bearer_auth(&token).send().await?.json().await?;
        assert_eq!(listing["macs"][0]["label"],"Café");assert_eq!(listing["macs"][0]["label_revision"],1);
        assert_eq!(listing["macs"][0]["clients"][0]["label"],"Café");
        assert!(!listing.to_string().contains("other"));assert!(!listing.to_string().contains("signing_key"));
        assert_eq!(server.auth.mac_for_owner("mac-1","owner")?.to_sec1_point(false),mac_key.verifying_key().to_sec1_point(false));
        assert!(server.auth.phone_key("client-first").is_ok());
        let other:Value=client.get(format!("{url}/v1/account/devices")).bearer_auth(jwt("other",now_ms())).send().await?.json().await?;
        assert!(other["macs"][0]["label"].is_null());assert_eq!(other["macs"][0]["label_revision"],0);
        let revoked=client.delete(format!("{url}/v1/account/devices/clients/client-first")).bearer_auth(&token).header("Idempotency-Key","label-revoke-client-request").send().await?;
        assert_eq!(revoked.status(),StatusCode::ACCEPTED);
        assert_eq!(rename("clients","client-first","label-after-revoke-request",json!({"label":"Again","expected_revision":1})).send().await?.status(),StatusCode::NOT_FOUND);
        assert!(server.auth.phone_key("client-first").is_ok(),"PG denies before registry cleanup");
        join.abort();management::tests::cleanup(db,schema).await
    }

    #[tokio::test]
    #[ignore="requires disposable local PostgreSQL"]
    async fn management_http_own_device_revocation_isolated_immediate_and_reconciled()->anyhow::Result<()> {
        let (db,admin,schema)=management::tests::fixture_for_issuer("https://id.example/realms/alondra").await?;
        for (email,subject) in [("first@example.com","owner"),("second@example.com","other")] {
            let invite=db.create_invitation(admin.id,email,300).await?;
            db.reserve_acceptance(&invite.token,"Owner").await?.complete(subject).await?;
        }
        let mac_key=SigningKey::from_slice(&[21u8;32]).unwrap();let phone_key=SigningKey::from_slice(&[22u8;32]).unwrap();
        let (url,server,join)=test_server_with_management(&mac_key,None,Some(db.clone())).await;
        server.auth.enroll_mac(auth::MacDevice{mac_id:"mac-other".into(),owner_sub:"other".into(),signing_key:key_bytes(&mac_key),agreement_key:key_bytes(&phone_key)})?;
        for (mac,owner,id) in [("mac-1","owner","client-first"),("mac-other","other","client-other")] {
            server.auth.enroll(&auth::Identity{device_id:mac.into(),owner_sub:owner.into()},PhoneDevice{device_id:id.into(),owner_sub:owner.into(),signing_key:key_bytes(&phone_key)})?;
        }
        let client=reqwest::Client::new();
        let auth=jwt("owner",now_ms());
        let listing:Value=client.get(format!("{url}/v1/account/devices")).bearer_auth(&auth).send().await?.json().await?;
        assert_eq!(listing["macs"].as_array().unwrap().len(),1);assert_eq!(listing["macs"][0]["mac_id"],"mac-1");
        assert_eq!(listing["macs"][0]["clients"][0]["device_id"],"client-first");
        assert!(!listing.to_string().contains("signing_key"));assert!(!listing.to_string().contains("client-other"));
        let denied=client.delete(format!("{url}/v1/account/devices/clients/client-other")).bearer_auth(&auth).header("Idempotency-Key","device-revoke-foreign-0001").send().await?;
        assert_eq!(denied.status(),StatusCode::NOT_FOUND);
        let path="/v1/iroh-node";
        let before=sign(&server.auth.server_epoch(),client.delete(format!("{url}{path}")).bearer_auth(&auth),&phone_key,"client-first","DELETE",path,&[],180).send().await?;
        assert_eq!(before.status(),StatusCode::NO_CONTENT);
        let response=client.delete(format!("{url}/v1/account/devices/clients/client-first")).bearer_auth(&auth).header("Idempotency-Key","device-revoke-own-00001").send().await?;
        assert_eq!(response.status(),StatusCode::ACCEPTED);
        let after=sign(&server.auth.server_epoch(),client.delete(format!("{url}{path}")).bearer_auth(&auth),&phone_key,"client-first","DELETE",path,&[],181).send().await?;
        assert_eq!(after.status(),StatusCode::FORBIDDEN);
        assert!(server.auth.phone_key("client-first").is_ok(),"SQL tombstone must deny before registry worker runs");
        let listing:Value=client.get(format!("{url}/v1/account/devices")).bearer_auth(&auth).send().await?.json().await?;
        assert_eq!(listing["macs"][0]["clients"][0]["status"],"revoked");assert_eq!(listing["macs"][0]["clients"][0]["reconciliation"],"pending");
        let body=serde_json::to_vec(&json!({"device_id":"client-first","owner_sub":"owner","signing_key":key_bytes(&phone_key)}))?;
        let response=sign(&server.auth.server_epoch(),client.post(format!("{url}/v1/devices")).body(body.clone()),&mac_key,"mac-1","POST","/v1/devices",&body,182).send().await?;
        assert_eq!(response.status(),StatusCode::FORBIDDEN,"a Mac signature cannot resurrect a PG-revoked binding");
        let repeated=client.delete(format!("{url}/v1/account/devices/clients/client-first")).bearer_auth(&auth).header("Idempotency-Key","device-revoke-own-00001").send().await?;
        assert_eq!(repeated.status(),StatusCode::CONFLICT);
        assert!(management::worker::step(&server).await?);
        assert!(server.auth.phone_key("client-first").is_err());assert!(server.auth.phone_key("client-other").is_ok());
        let listing:Value=client.get(format!("{url}/v1/account/devices")).bearer_auth(&auth).send().await?.json().await?;
        assert_eq!(listing["macs"][0]["clients"][0]["reconciliation"],"completed");
        server.auth.enroll(&auth::Identity{device_id:"mac-1".into(),owner_sub:"owner".into()},PhoneDevice{device_id:"client-descendant".into(),owner_sub:"owner".into(),signing_key:key_bytes(&phone_key)})?;
        let response=client.delete(format!("{url}/v1/account/devices/macs/mac-1")).bearer_auth(&auth).header("Idempotency-Key","device-revoke-mac-00001").send().await?;
        assert_eq!(response.status(),StatusCode::ACCEPTED);
        let response=sign(&server.auth.server_epoch(),client.delete(format!("{url}/v1/iroh-node")),&mac_key,"mac-1","DELETE","/v1/iroh-node",&[],183).send().await?;
        assert_eq!(response.status(),StatusCode::FORBIDDEN);
        assert!(db.require_live_device("owner","fresh-mac",Some("client-descendant")).await.is_err(),"Mac revocation persists child IDs independently from the old route");
        while management::worker::step(&server).await? {}
        assert!(!server.auth.is_mac_registered("mac-1"));assert!(server.auth.is_mac_registered("mac-other"));
        let response=client.delete(format!("{url}/v1/account")).bearer_auth(&auth).header("Idempotency-Key","self-delete-confirm-0001").json(&json!({"confirm":false})).send().await?;
        assert_eq!(response.status(),StatusCode::UNPROCESSABLE_ENTITY);
        assert!(db.active_identity("https://id.example/realms/alondra","owner").await.is_ok());
        join.abort();management::tests::cleanup(db,schema).await
    }

    #[tokio::test]
    #[ignore = "requires disposable local PostgreSQL"]
    async fn waitlist_http_is_uniform_bounded_and_only_admins_manage_it() -> anyhow::Result<()> {
        let (db, admin, schema) =
            management::tests::fixture_for_issuer("https://id.example/realms/alondra").await?;
        let invitation = db.create_invitation(admin.id, "member@example.test", 300).await?;
        db.reserve_acceptance(&invitation.token, "Member").await?.complete("member").await?;
        let key = SigningKey::from_slice(&[31u8; 32]).unwrap();
        let (url, _server, join) = test_server_with_management(&key, None, Some(db.clone())).await;
        let client = reqwest::Client::new();
        let count = |table: &'static str| {
            let pool = db.pool.clone();
            async move {
                sqlx_core::query_scalar::query_scalar::<_, i64>(&format!("SELECT count(*) FROM {table}"))
                    .fetch_one(&pool)
                    .await
            }
        };
        let (accounts, invitations) = (count("management_accounts").await?, count("management_invitations").await?);
        let join_waitlist = |body: Vec<u8>, content_type: &'static str| {
            client
                .post(format!("{url}/api/waitlist"))
                .header("content-type", content_type)
                .body(body)
                .send()
        };
        let mut accepted = Vec::new();
        for email in ["new.person@example.test", "  NEW.Person@Example.TEST ", "member@example.test"] {
            let response = join_waitlist(serde_json::to_vec(&json!({"email": email}))?, "application/json").await?;
            let status = response.status();
            let mut headers = response.headers().clone();
            headers.remove("date");
            accepted.push((status, headers, response.bytes().await?));
        }
        assert_eq!(accepted[0].0, StatusCode::ACCEPTED);
        assert_eq!(serde_json::from_slice::<Value>(&accepted[0].2)?, json!({"status": "accepted"}));
        assert!(accepted.iter().all(|r| r == &accepted[0]), "new, repeated and existing-account emails are indistinguishable");
        let invalid = join_waitlist(br#"{"email":"not-an-address"}"#.to_vec(), "application/json").await?;
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        assert_eq!(invalid.json::<Value>().await?, json!({"error": "invalid_email"}));
        let oversized = json!({"email": format!("{}@example.test", "a".repeat(2000))});
        let oversized = join_waitlist(serde_json::to_vec(&oversized)?, "application/json").await?;
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let limited = join_waitlist(br#"{"email":"sixth@example.test"}"#.to_vec(), "application/json").await?;
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry: u64 = limited.headers()[header::RETRY_AFTER].to_str()?.parse()?;
        assert!((1..=600).contains(&retry));
        assert_eq!(limited.json::<Value>().await?, json!({"error": "rate_limited"}));
        assert_eq!(count("management_waitlist").await?, 2);
        assert_eq!(
            (count("management_accounts").await?, count("management_invitations").await?),
            (accounts, invitations),
            "joining creates no account or invitation"
        );

        let list = |bearer: Option<String>, query: &str| {
            let request = client.get(format!("{url}/v1/admin/waitlist{query}"));
            match bearer { Some(token) => request.bearer_auth(token), None => request }.send()
        };
        assert_eq!(list(None, "").await?.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(list(Some(jwt("member", now_ms())), "").await?.status(), StatusCode::FORBIDDEN);
        let first: Value = list(Some(jwt("stable-admin-sub", now_ms())), "?limit=1").await?.json().await?;
        assert_eq!(first["entries"].as_array().unwrap().len(), 1);
        assert_eq!(first["entries"][0]["email"], "new.person@example.test");
        assert!(first["entries"][0]["created_at_ms"].as_i64().unwrap() > 1_700_000_000_000);
        let cursor = first["next_cursor"].as_str().unwrap();
        let second: Value = list(Some(jwt("stable-admin-sub", now_ms())), &format!("?limit=1&after={cursor}")).await?.json().await?;
        assert_eq!(second["entries"][0]["email"], "member@example.test");
        assert!(second["next_cursor"].is_null());

        let id = first["entries"][0]["id"].as_str().unwrap();
        let remove = |bearer: Option<String>| {
            let request = client.delete(format!("{url}/v1/admin/waitlist/{id}"));
            match bearer { Some(token) => request.bearer_auth(token), None => request }.send()
        };
        assert_eq!(remove(None).await?.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(remove(Some(jwt("member", now_ms()))).await?.status(), StatusCode::FORBIDDEN);
        assert_eq!(count("management_waitlist").await?, 2);
        assert_eq!(remove(Some(jwt("stable-admin-sub", now_ms()))).await?.status(), StatusCode::NO_CONTENT);
        assert_eq!(remove(Some(jwt("stable-admin-sub", now_ms()))).await?.status(), StatusCode::NOT_FOUND);
        assert_eq!(count("management_waitlist").await?, 1);
        let audited: i64 = sqlx_core::query_scalar::query_scalar("SELECT count(*) FROM management_audit WHERE action='waitlist_remove' AND actor_id=$1")
            .bind(admin.id).fetch_one(&db.pool).await?;
        assert_eq!(audited, 1);
        join.abort();
        management::tests::cleanup(db, schema).await
    }

    #[tokio::test]
    #[ignore = "requires disposable local PostgreSQL"]
    async fn inviting_from_the_waitlist_issues_a_link_and_clears_the_request_in_one_step() -> anyhow::Result<()> {
        let (db, admin, schema) =
            management::tests::fixture_for_issuer("https://id.example/realms/alondra").await?;
        let invitation = db.create_invitation(admin.id, "member@example.test", 300).await?;
        db.reserve_acceptance(&invitation.token, "Member").await?.complete("member").await?;
        db.join_waitlist("friend@example.test").await?;
        db.join_waitlist("member@example.test").await?;
        let entries = db.list_waitlist(admin.id, None, 10).await?.entries;
        let id_of = |email: &str| entries.iter().find(|entry| entry.email == email).unwrap().id;
        let (friend, member) = (id_of("friend@example.test"), id_of("member@example.test"));
        let key = SigningKey::from_slice(&[32u8; 32]).unwrap();
        let (url, _server, join) = test_server_with_management(&key, None, Some(db.clone())).await;
        let client = reqwest::Client::new();
        let invite = |subject: &str, id: uuid::Uuid, idempotency: &str| {
            client
                .post(format!("{url}/v1/admin/waitlist/{id}/invite"))
                .bearer_auth(jwt(subject, now_ms()))
                .header("Idempotency-Key", idempotency)
                .json(&json!({"ttl_seconds": 604800}))
                .send()
        };
        let waiting = || {
            let pool = db.pool.clone();
            async move {
                sqlx_core::query_scalar::query_scalar::<_, String>("SELECT email FROM management_waitlist ORDER BY email")
                    .fetch_all(&pool)
                    .await
            }
        };
        let invited = || {
            let pool = db.pool.clone();
            async move {
                sqlx_core::query_scalar::query_scalar::<_, String>("SELECT email FROM management_invitations ORDER BY email")
                    .fetch_all(&pool)
                    .await
            }
        };

        assert_eq!(invite("member", friend, "member-attempt-0001").await?.status(), StatusCode::FORBIDDEN);
        assert_eq!(waiting().await?, ["friend@example.test", "member@example.test"]);

        let issued = invite("stable-admin-sub", friend, "friend-invite-0001").await?;
        assert_eq!(issued.status(), StatusCode::CREATED);
        let issued: Value = issued.json().await?;
        assert_eq!(issued["invitation"]["email"], "friend@example.test");
        assert_eq!(issued["invitation"]["status"], "pending");
        assert_eq!(db.invitation_status(issued["token"].as_str().unwrap()).await?, "pending");
        assert_eq!(waiting().await?, ["member@example.test"]);
        assert_eq!(invited().await?, ["friend@example.test", "member@example.test"]);

        let repeated = invite("stable-admin-sub", friend, "friend-invite-0001").await?;
        assert_eq!(repeated.status(), StatusCode::CONFLICT);
        assert_eq!(repeated.json::<Value>().await?["error"]["code"], "invitation_token_already_issued");
        assert_eq!(invite("stable-admin-sub", friend, "friend-invite-0002").await?.status(), StatusCode::NOT_FOUND);

        let taken = invite("stable-admin-sub", member, "member-invite-0001").await?;
        assert_eq!(taken.status(), StatusCode::CONFLICT);
        assert_eq!(taken.json::<Value>().await?["error"]["code"], "state_conflict");
        assert_eq!(waiting().await?, ["member@example.test"], "a refused invitation keeps the request");
        assert_eq!(invited().await?, ["friend@example.test", "member@example.test"]);

        let audited: Vec<String> = sqlx_core::query_scalar::query_scalar(
            "SELECT action FROM management_audit WHERE actor_id=$1 AND action IN ('invite_create','waitlist_remove') ORDER BY action",
        )
        .bind(admin.id)
        .fetch_all(&db.pool)
        .await?;
        assert_eq!(audited, ["invite_create", "invite_create", "waitlist_remove"]);
        join.abort();
        management::tests::cleanup(db, schema).await
    }

    #[tokio::test]
    #[ignore="requires disposable local PostgreSQL"]
    async fn management_http_private_gate_blocks_unknown_disabled_and_database_outage() -> anyhow::Result<()> {
        let (db,admin,schema)=management::tests::fixture_for_issuer("https://id.example/realms/alondra").await?;
        let invitation=db.create_invitation(admin.id,"gate@example.com",300).await?;
        let owner=db.reserve_acceptance(&invitation.token,"Owner").await?.complete("owner").await?;
        let key=SigningKey::from_slice(&[21u8;32]).unwrap();
        let (url,server,join)=test_server_with_management(&key,None,Some(db.clone())).await;
        server.auth.enroll(&auth::Identity { device_id: "mac-1".into(), owner_sub: "owner".into() },
            PhoneDevice { device_id: "waiting-phone".into(), owner_sub: "owner".into(), signing_key: key_bytes(&key) })?;
        let client=reqwest::Client::new();
        let response=client.get(format!("{url}/v1/macs")).bearer_auth(jwt("uninvited",now_ms())).send().await?;
        assert_eq!(response.status(),StatusCode::FORBIDDEN);
        let response=client.get(format!("{url}/v1/macs")).bearer_auth(jwt("owner",now_ms())).send().await?;
        assert_eq!(response.status(),StatusCode::OK);
        let path="/v1/iroh-node";
        let as_mac=|nonce| sign(&server.auth.server_epoch(),client.delete(format!("{url}{path}")),&key,"mac-1","DELETE",path,&[],nonce).send();
        let as_phone=|nonce| sign(&server.auth.server_epoch(),client.delete(format!("{url}{path}")).bearer_auth(jwt("owner", now_ms())),
            &key,"waiting-phone","DELETE",path,&[],nonce).send();
        assert_eq!(as_mac(211).await?.status(),StatusCode::NO_CONTENT);
        assert_eq!(as_phone(212).await?.status(),StatusCode::NO_CONTENT);
        let disabled = client.post(format!("{url}/v1/admin/accounts/{}/disable", owner.id))
            .bearer_auth(jwt("stable-admin-sub", now_ms()))
            .header("Idempotency-Key", "disable-waiting-account-0001").send().await?;
        assert_eq!(disabled.status(), StatusCode::ACCEPTED);
        assert_eq!(as_mac(213).await?.status(),StatusCode::FORBIDDEN,"signed Mac cannot bypass product suspension");
        assert_eq!(as_phone(214).await?.status(),StatusCode::FORBIDDEN,"account disable denies the phone too");
        db.pool.close().await;
        let response=client.get(format!("{url}/v1/macs")).bearer_auth(jwt("stable-admin-sub",now_ms())).send().await?;
        assert_eq!(response.status(),StatusCode::SERVICE_UNAVAILABLE,"database outage must not fall back to OIDC-only admission");
        join.abort();management::tests::cleanup(db,schema).await
    }

    #[tokio::test]
    async fn mac_directory_requires_oidc_and_isolates_owners_without_granting_content_access() {
        let key = SigningKey::from_slice(&[31u8; 32]).unwrap();
        let (url, server, task) = test_server(&key).await;
        let client = reqwest::Client::new();
        for auth in [None, Some("Bearer invalid".to_string()), Some(format!("Bearer {}", jwt("owner", 0)))] {
            let mut request = client.get(format!("{url}/v1/macs"));
            if let Some(auth) = auth { request = request.header("authorization", auth); }
            let response = request.send().await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(response.json::<Value>().await.unwrap(), json!({"error":"unauthorized"}));
        }
        for (id, owner) in [("mac-1", "owner"), ("mac-z", "owner"), ("mac-other", "other")] {
            server.auth.enroll_mac(auth::MacDevice {
                mac_id: id.into(), owner_sub: owner.into(), signing_key: key_bytes(&key), agreement_key: key_bytes(&key),
            }).unwrap();
        }
        for (owner, expected) in [
            ("owner", json!({"macs":[{"mac_id":"mac-1"},{"mac_id":"mac-z"}]})),
            ("other", json!({"macs":[{"mac_id":"mac-other"}]})),
            ("empty", json!({"macs":[]})),
        ] {
            // Query parameters cannot select another owner's registry.
            let response = client.get(format!("{url}/v1/macs?owner_sub=other&owner=other"))
                .bearer_auth(jwt(owner, now_ms())).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(response.json::<Value>().await.unwrap(), expected, "only IDs; no keys or content");
        }
        let response = client.delete(format!("{url}/v1/iroh-node"))
            .bearer_auth(jwt("owner", now_ms())).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "OIDC alone cannot act as a device");
        let epoch = server.auth.server_epoch();
        let response = sign(&epoch, client.delete(format!("{url}/v1/macs/mac-z")), &key,
            "mac-z", "DELETE", "/v1/macs/mac-z", &[], 91).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let response = client.get(format!("{url}/v1/macs"))
            .bearer_auth(jwt("owner", now_ms())).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.json::<Value>().await.unwrap(), json!({"macs":[{"mac_id":"mac-1"}]}));
        task.abort();
    }

    #[tokio::test]
    async fn pairing_requires_mac_approval_and_returns_key_bound_proof() {
        use p256::ecdsa::{Signature, signature::Signer};
        let mac_sign = SigningKey::from_slice(&[31u8; 32]).unwrap();
        let mac_agreement = SecretKey::from_slice(&[32u8; 32]).unwrap();
        let phone_sign = SigningKey::from_slice(&[33u8; 32]).unwrap();
        let phone_agreement = SecretKey::from_slice(&[34u8; 32]).unwrap();
        let (url, server, task) = test_server(&mac_sign).await;
        let mac_agreement_key = URL_SAFE_NO_PAD.encode(mac_agreement.public_key().to_sec1_point(false).as_bytes());
        server.auth.enroll_mac(auth::MacDevice {
            mac_id: "mac-1".into(), owner_sub: "owner".into(), signing_key: key_bytes(&mac_sign),
            agreement_key: mac_agreement_key.clone(),
        }).unwrap();
        let epoch = server.auth.server_epoch();
        let client = reqwest::Client::new();
        let secret = [9u8; 16];
        let session_id = "pair-http-test";
        let expires = now_ms() + 600_000;
        let session = serde_json::to_vec(&pairing::SessionRequest {
            session_id: session_id.into(), mac_id: "mac-1".into(),
            secret_hash: URL_SAFE_NO_PAD.encode(Sha256::digest(secret)), expires_at_ms: expires,
        }).unwrap();
        let response = sign(&epoch, client.post(format!("{url}/v1/pairings")), &mac_sign,
            "mac-1", "POST", "/v1/pairings", &session, 1).body(session).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let phone_key = key_bytes(&phone_sign);
        let plaintext = serde_json::to_vec(&json!({"device_id":"client-a","signing_key":phone_key,
            "agreement_key":URL_SAFE_NO_PAD.encode(phone_agreement.public_key().to_sec1_point(false).as_bytes()),
            "display_name":"iPhone","owner_sub":"owner","session_id":session_id})).unwrap();
        let envelope = Envelope::seal_random(Context { route: "mac-1", sender: "client-a",
            recipient: "mac-1", request_id: session_id, kind: "pairing", expires_at_ms: expires },
            &plaintext, &phone_sign, &mac_agreement.public_key()).unwrap();
        let candidate = serde_json::to_vec(&pairing::CandidateRequest {
            secret: URL_SAFE_NO_PAD.encode(secret), device_id: "client-a".into(),
            signing_key: phone_key.clone(), envelope,
        }).unwrap();
        let path = format!("/v1/pairings/{session_id}/candidates");
        let response = sign(&epoch, client.post(format!("{url}{path}"))
            .bearer_auth(jwt("owner", now_ms())), &phone_sign, "client-a", "POST", &path,
            &candidate, 2).body(candidate).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let approval_path = format!("/v1/pairings/{session_id}/approval");
        let response = sign(&epoch, client.get(format!("{url}{approval_path}"))
            .bearer_auth(jwt("owner", now_ms())), &phone_sign, "client-a", "GET",
            &approval_path, &[], 3).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let registration = serde_json::to_vec(&PhoneDevice { device_id: "client-a".into(),
            owner_sub: "owner".into(), signing_key: phone_key.clone() }).unwrap();
        let response = sign(&epoch, client.post(format!("{url}/v1/devices")), &mac_sign,
            "mac-1", "POST", "/v1/devices", &registration, 4)
            .body(registration).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let approval = pairing::Approval { v: 1, session_id: session_id.into(), owner_sub: "owner".into(),
            mac_id: "mac-1".into(), mac_signing_key: key_bytes(&mac_sign),
            mac_agreement_key, device_id: "client-a".into(), phone_signing_key: phone_key,
            phone_agreement_key: URL_SAFE_NO_PAD.encode(phone_agreement.public_key().to_sec1_point(false).as_bytes()),
            expires_at_ms: expires };
        let signature: Signature = mac_sign.sign(&approval.signed_bytes());
        let proof = pairing::ApprovalProof { approval, signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()) };
        let body = serde_json::to_vec(&proof).unwrap();
        let response = sign(&epoch, client.post(format!("{url}{approval_path}")), &mac_sign,
            "mac-1", "POST", &approval_path, &body, 5).body(body).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let response = sign(&epoch, client.get(format!("{url}{approval_path}"))
            .bearer_auth(jwt("owner", now_ms())), &phone_sign, "client-a", "GET",
            &approval_path, &[], 6).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let returned: pairing::ApprovalProof = response.json().await.unwrap();
        pairing::verify_proof(&returned).unwrap();
        task.abort();
    }

    #[tokio::test]
    async fn re_pairing_with_the_same_mac_supersedes_the_older_push_registration() {
        let mac_sign = SigningKey::from_slice(&[21u8; 32]).unwrap();
        let (_url, server, task) = test_server_with_apns(&mac_sign, None).await;
        let identity = |mac: &str| auth::Identity {
            device_id: mac.into(),
            owner_sub: "owner".into(),
        };
        for (index, (phone, mac)) in [
            ("phone-old", "mac-1"),
            ("phone-new", "mac-1"),
            ("phone-other-mac", "mac-2"),
            ("phone-other-install", "mac-1"),
        ]
        .into_iter()
        .enumerate()
        {
            let key = SigningKey::from_slice(&[40 + index as u8; 32]).unwrap();
            server
                .auth
                .enroll(
                    &identity(mac),
                    PhoneDevice {
                        device_id: phone.into(),
                        owner_sub: "owner".into(),
                        signing_key: key_bytes(&key),
                    },
                )
                .unwrap();
        }
        let registration = |token: &str, binding: u8| PushRegistration {
            token: token.repeat(32),
            environment: "production".into(),
            push_binding: URL_SAFE_NO_PAD.encode([binding; 32]),
        };
        let auth = &server.auth;
        auth.register_push("phone-old", registration("ab", 1), None).unwrap();
        auth.register_push("phone-other-mac", registration("ab", 2), None).unwrap();
        auth.register_push("phone-other-install", registration("cd", 3), None).unwrap();
        auth.register_push("phone-new", registration("ab", 4), None).unwrap();

        assert!(
            auth.push_registration("phone-old").is_none(),
            "the phone cannot decrypt pushes for a pairing it replaced"
        );
        assert_eq!(auth.push_registration("phone-new"), Some(registration("ab", 4)));
        assert_eq!(auth.push_registration("phone-other-mac"), Some(registration("ab", 2)));
        assert_eq!(auth.push_registration("phone-other-install"), Some(registration("cd", 3)));
        let stored: Value = serde_json::from_slice(
            &std::fs::read(&server.auth.config.device_registry_path).unwrap(),
        )
        .unwrap();
        assert!(stored["push"].get("phone-old").is_none());
        task.abort();
    }

    #[tokio::test]
    async fn a_dead_token_does_not_erase_the_token_the_phone_registered_after_it() {
        let mac_sign = SigningKey::from_slice(&[21u8; 32]).unwrap();
        let (_url, server, task) = test_server_with_apns(&mac_sign, None).await;
        let phone = SigningKey::from_slice(&[50u8; 32]).unwrap();
        server
            .auth
            .enroll(
                &auth::Identity { device_id: "mac-1".into(), owner_sub: "owner".into() },
                PhoneDevice {
                    device_id: "phone-1".into(),
                    owner_sub: "owner".into(),
                    signing_key: key_bytes(&phone),
                },
            )
            .unwrap();
        let registration = |token: &str| PushRegistration {
            token: token.repeat(32),
            environment: "sandbox".into(),
            push_binding: URL_SAFE_NO_PAD.encode([1u8; 32]),
        };
        server.auth.register_push("phone-1", registration("cd"), None).unwrap();
        server.auth.forget_dead_push("phone-1", &"ab".repeat(32)).unwrap();
        assert_eq!(server.auth.push_registration("phone-1"), Some(registration("cd")));
        server.auth.forget_dead_push("phone-1", &"cd".repeat(32)).unwrap();
        assert!(server.auth.push_registration("phone-1").is_none());
        task.abort();
    }

    fn enroll_phones(server: &Server, phones: &[&str]) {
        for (index, phone) in phones.iter().enumerate() {
            let key = SigningKey::from_slice(&[60 + index as u8; 32]).unwrap();
            server
                .auth
                .enroll(
                    &auth::Identity { device_id: "mac-1".into(), owner_sub: "owner".into() },
                    PhoneDevice {
                        device_id: (*phone).into(),
                        owner_sub: "owner".into(),
                        signing_key: key_bytes(&key),
                    },
                )
                .unwrap();
        }
    }

    fn push_for(binding: u8) -> PushRegistration {
        PushRegistration {
            token: format!("{binding:02x}").repeat(32),
            environment: "sandbox".into(),
            push_binding: URL_SAFE_NO_PAD.encode([binding; 32]),
        }
    }

    fn offline_session(sid: &str) -> Option<auth::PushSession> {
        Some(auth::PushSession { sid: sid.into(), client_id: "alondra-ios".into() })
    }

    #[tokio::test]
    async fn a_signed_out_phone_stops_its_pushes_with_the_binding_alone() {
        let mac_sign = SigningKey::from_slice(&[21u8; 32]).unwrap();
        let (url, server, task) = test_server(&mac_sign).await;
        enroll_phones(&server, &["phone-1", "phone-2"]);
        server.auth.register_push("phone-1", push_for(1), offline_session("s1")).unwrap();
        server.auth.register_push("phone-2", push_for(2), None).unwrap();
        let client = reqwest::Client::new();
        let unregister = |body: Vec<u8>| {
            client
                .post(format!("{url}/v1/push/unregister"))
                .header("content-type", "application/json")
                .body(body)
                .send()
        };
        let unknown = json!({"push_binding": URL_SAFE_NO_PAD.encode([9u8; 32])});
        assert_eq!(unregister(serde_json::to_vec(&unknown).unwrap()).await.unwrap().status(), 204);
        assert_eq!(server.auth.push_registration("phone-1"), Some(push_for(1)));
        let malformed = json!({"push_binding": "short"});
        assert_eq!(unregister(serde_json::to_vec(&malformed).unwrap()).await.unwrap().status(), 400);
        assert_eq!(unregister(vec![b' '; 257]).await.unwrap().status(), 413);
        let mine = json!({"push_binding": push_for(1).push_binding});
        assert_eq!(unregister(serde_json::to_vec(&mine).unwrap()).await.unwrap().status(), 204);
        assert_eq!(unregister(serde_json::to_vec(&mine).unwrap()).await.unwrap().status(), 204);
        assert!(server.auth.push_registration("phone-1").is_none());
        assert!(server.auth.push_ended("phone-1"), "the Mac hears 410 and forgets the binding");
        assert!(!server.auth.push_ended("phone-2"));
        assert_eq!(server.auth.push_registration("phone-2"), Some(push_for(2)));
        let stored: Value = serde_json::from_slice(
            &std::fs::read(&server.auth.config.device_registry_path).unwrap(),
        )
        .unwrap();
        assert!(stored["push"].get("phone-1").is_none());
        assert!(stored["push_sessions"].get("phone-1").is_none());
        assert_eq!(stored["push_ended"], json!(["phone-1"]));
        server.auth.register_push("phone-1", push_for(1), None).unwrap();
        assert!(!server.auth.push_ended("phone-1"), "registering again starts over");
        task.abort();
    }

    #[test]
    fn only_a_registration_the_relay_ended_is_gone_for_the_mac() {
        assert_eq!(missing_push_registration(true).0, StatusCode::GONE);
        assert_eq!(missing_push_registration(true).1.0["error"], "push_registration_gone");
        assert_eq!(missing_push_registration(false).0, StatusCode::NOT_FOUND);
        assert_eq!(missing_push_registration(false).1.0["error"], "push_registration_not_found");
    }

    #[tokio::test]
    async fn a_mac_that_leaves_takes_its_phones_push_registrations_with_it() {
        let mac_sign = SigningKey::from_slice(&[21u8; 32]).unwrap();
        let (_url, server, task) = test_server(&mac_sign).await;
        enroll_phones(&server, &["phone-1", "phone-2"]);
        server.auth.register_push("phone-1", push_for(1), None).unwrap();
        server.auth.register_push("phone-2", push_for(2), None).unwrap();
        server.auth.reconcile_device_revocation("owner", "client", "phone-2", "mac-1").unwrap();
        assert!(server.auth.push_registration("phone-2").is_none());
        assert!(server.auth.push_ended("phone-2"));
        assert!(server.auth.push_registration("phone-1").is_some());
        server
            .auth
            .revoke_mac(&auth::Identity { device_id: "mac-1".into(), owner_sub: "owner".into() })
            .unwrap();
        assert!(server.auth.push_registration("phone-1").is_none());
        assert!(server.auth.push_ended("phone-1"));
        task.abort();
    }

    #[test]
    fn only_an_offline_bearer_names_the_session_its_push_ends_with() {
        let bearer = |claims: Value| {
            let mut headers = HeaderMap::new();
            let token = format!(
                "e30.{}.sig",
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
            );
            headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
            Auth::bearer_push_session(&headers)
        };
        assert_eq!(
            bearer(json!({"sid":"cRHwoE82NmiLwk5ZFBWMZUg7","azp":"alondra-ios","scope":"openid offline_access"})),
            Some(auth::PushSession { sid: "cRHwoE82NmiLwk5ZFBWMZUg7".into(), client_id: "alondra-ios".into() })
        );
        assert_eq!(bearer(json!({"sid":"s","azp":"alondra-ios","scope":"openid"})), None);
        assert_eq!(bearer(json!({"azp":"alondra-ios","scope":"offline_access"})), None);
        assert_eq!(bearer(json!({"sid":"a/b","azp":"alondra-ios","scope":"offline_access"})), None);
    }

    async fn fake_keycloak(
        live: Option<Vec<&'static str>>,
    ) -> (management::keycloak::Keycloak, tokio::task::JoinHandle<()>) {
        let live = Arc::new(live);
        let router = Router::new()
            .route(
                "/realms/alondra/protocol/openid-connect/token",
                post(|| async { Json(json!({"access_token":"synthetic"})) }),
            )
            .route(
                "/admin/realms/alondra/users/{sub}/consents",
                get({
                    let live = live.clone();
                    move || async move {
                        if live.is_none() {
                            return StatusCode::BAD_GATEWAY.into_response();
                        }
                        Json(json!([
                            {"clientId":"other","additionalGrants":[{"client":"other-uuid","key":"Offline Token"}]},
                            {"clientId":"alondra-ios","additionalGrants":[{"client":"ios-uuid","key":"Offline Token"}]}
                        ]))
                        .into_response()
                    }
                }),
            )
            .route(
                "/admin/realms/alondra/users/{sub}/offline-sessions/ios-uuid",
                get(move || async move {
                    let ids = live.as_ref().clone().unwrap_or_default();
                    Json(ids.iter().map(|id| json!({"id": id})).collect::<Vec<_>>())
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let keycloak = management::keycloak::Keycloak::new(
            "https://id.example/realms/alondra".into(),
            format!("{base}/admin/realms/alondra/").parse().unwrap(),
            format!("{base}/realms/alondra/protocol/openid-connect/token").parse().unwrap(),
            "management".into(),
            "secret".into(),
        )
        .unwrap();
        (keycloak, task)
    }

    #[tokio::test]
    async fn pushes_stop_when_the_offline_session_that_registered_them_ends() {
        let mac_sign = SigningKey::from_slice(&[21u8; 32]).unwrap();
        let (_url, server, task) = test_server(&mac_sign).await;
        enroll_phones(&server, &["phone-live", "phone-ended", "phone-legacy"]);
        server.auth.register_push("phone-live", push_for(1), offline_session("live")).unwrap();
        server.auth.register_push("phone-ended", push_for(2), offline_session("ended")).unwrap();
        server.auth.register_push("phone-legacy", push_for(3), None).unwrap();

        let (unreachable, keycloak_task) = fake_keycloak(None).await;
        sweep_ended_push_sessions(&server, &unreachable).await;
        keycloak_task.abort();
        assert!(server.auth.push_registration("phone-ended").is_some());

        let (keycloak, keycloak_task) = fake_keycloak(Some(vec!["live"])).await;
        sweep_ended_push_sessions(&server, &keycloak).await;
        keycloak_task.abort();
        assert_eq!(server.auth.push_registration("phone-live"), Some(push_for(1)));
        assert!(server.auth.push_registration("phone-ended").is_none());
        assert!(server.auth.push_ended("phone-ended"));
        assert!(!server.auth.push_ended("phone-live"));
        assert_eq!(server.auth.push_registration("phone-legacy"), Some(push_for(3)));

        server.auth.register_push("phone-ended", push_for(4), offline_session("new")).unwrap();
        server.auth.forget_ended_session_push("phone-ended", "ended").unwrap();
        assert_eq!(server.auth.push_registration("phone-ended"), Some(push_for(4)));
        task.abort();
    }

    #[tokio::test]
    async fn encrypted_push_requires_both_identities_and_purges_registration_on_revoke() {
        use std::sync::Mutex;
        let delivered = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured = delivered.clone();
        let delivered_headers = Arc::new(Mutex::new(Vec::<HeaderMap>::new()));
        let captured_headers = delivered_headers.clone();
        let apns_router = Router::new().route(
            "/3/device/{token}",
            post(move |version: axum::http::Version, headers: HeaderMap, Json(body): Json<Value>| {
                let captured = captured.clone();
                let captured_headers = captured_headers.clone();
                async move {
                    assert_eq!(version, axum::http::Version::HTTP_2);
                    let request_id = body["alondra"]["envelope"]["request_id"].clone();
                    captured.lock().unwrap().push(body);
                    captured_headers.lock().unwrap().push(headers);
                    if request_id == "push-reject" {
                        (StatusCode::BAD_REQUEST, Json(json!({"reason":"BadTopic"}))).into_response()
                    } else if request_id == "push-gone" {
                        (
                            StatusCode::GONE,
                            [("apns-id", "EC1BF194-B3B2-424A-89A9-5A918A6E6B5C")],
                            Json(json!({"reason":"Unregistered","timestamp":1})),
                        )
                            .into_response()
                    } else {
                        StatusCode::OK.into_response()
                    }
                }
            }),
        );
        let apns_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let apns_url = format!("http://{}", apns_listener.local_addr().unwrap());
        let apns_task = tokio::spawn(async move {
            axum::serve(apns_listener, apns_router).await.unwrap();
        });
        let mac_sign = SigningKey::from_slice(&[21u8; 32]).unwrap();
        let phone_sign = SigningKey::from_slice(&[22u8; 32]).unwrap();
        let phone_ecdh = SecretKey::from_slice(&[23u8; 32]).unwrap();
        let (url, server, relay_task) = test_server_with_apns(&mac_sign, Some(apns_url)).await;
        server
            .auth
            .enroll(
                &auth::Identity {
                    device_id: "mac-1".into(),
                    owner_sub: "owner".into(),
                },
                PhoneDevice {
                    device_id: "phone-push".into(),
                    owner_sub: "owner".into(),
                    signing_key: key_bytes(&phone_sign),
                },
            )
            .unwrap();
        let epoch = server.auth.server_epoch();
        let client = reqwest::Client::new();
        let now = now_ms();
        let binding = URL_SAFE_NO_PAD.encode([3u8; 32]);
        let registration = PushRegistration {
            token: "ab".repeat(32),
            environment: "sandbox".into(),
            push_binding: binding.clone(),
        };
        let reg_body = serde_json::to_vec(&registration).unwrap();
        let path = "/v1/push/register";
        let intruder = sign(
            &epoch,
            client
                .post(format!("{url}{path}"))
                .bearer_auth(jwt("intruder", now))
                .body(reg_body.clone()),
            &phone_sign,
            "phone-push",
            "POST",
            path,
            &reg_body,
            50,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(intruder.status(), StatusCode::UNAUTHORIZED);
        let wrong_key = SigningKey::from_slice(&[24u8; 32]).unwrap();
        let forged = sign(
            &epoch,
            client
                .post(format!("{url}{path}"))
                .bearer_auth(jwt("owner", now))
                .body(reg_body.clone()),
            &wrong_key,
            "phone-push",
            "POST",
            path,
            &reg_body,
            51,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(forged.status(), StatusCode::UNAUTHORIZED);
        let mut invalid_registration = registration.clone();
        invalid_registration.push_binding.push('=');
        let invalid_body = serde_json::to_vec(&invalid_registration).unwrap();
        let invalid = sign(
            &epoch,
            client
                .post(format!("{url}{path}"))
                .bearer_auth(jwt("owner", now))
                .body(invalid_body.clone()),
            &phone_sign,
            "phone-push",
            "POST",
            path,
            &invalid_body,
            60,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        let registered = sign(
            &epoch,
            client
                .post(format!("{url}{path}"))
                .bearer_auth(jwt("owner", now))
                .body(reg_body.clone()),
            &phone_sign,
            "phone-push",
            "POST",
            path,
            &reg_body,
            52,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(registered.status(), StatusCode::NO_CONTENT);
        let stored: Value = serde_json::from_slice(
            &std::fs::read(&server.auth.config.device_registry_path).unwrap(),
        )
        .unwrap();
        assert_eq!(stored["push"]["phone-push"]["push_binding"], binding);

        let envelope = Envelope::seal_random(
            Context {
                route: "mac-1",
                sender: "mac-1",
                recipient: "phone-push",
                request_id: "push-1",
                kind: "push",
                expires_at_ms: now_ms() + 120_000,
            },
            br#"{"title":"private title","push_binding":"inside"}"#,
            &mac_sign,
            &phone_ecdh.public_key(),
        )
        .unwrap();
        let body =
            serde_json::to_vec(&json!({"device_id":"phone-push","envelope":envelope})).unwrap();
        let path = "/v1/mac/push";
        let forged = sign(
            &epoch,
            client.post(format!("{url}{path}")).body(body.clone()),
            &wrong_key,
            "mac-1",
            "POST",
            path,
            &body,
            53,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(forged.status(), StatusCode::UNAUTHORIZED);
        let mut wrong_route: Value = serde_json::from_slice(&body).unwrap();
        wrong_route["device_id"] = json!("other-phone");
        let wrong_body = serde_json::to_vec(&wrong_route).unwrap();
        let wrong = sign(
            &epoch,
            client.post(format!("{url}{path}")).body(wrong_body.clone()),
            &mac_sign,
            "mac-1",
            "POST",
            path,
            &wrong_body,
            54,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(wrong.status(), StatusCode::BAD_REQUEST);
        let oversized = vec![b'x'; 4097];
        let large = sign(
            &epoch,
            client.post(format!("{url}{path}")).body(oversized.clone()),
            &mac_sign,
            "mac-1",
            "POST",
            path,
            &oversized,
            55,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(large.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let too_large_for_apns = (2500..3100)
            .step_by(20)
            .find_map(|length| {
                let envelope = Envelope::seal_random(
                    Context {
                        route: "mac-1",
                        sender: "mac-1",
                        recipient: "phone-push",
                        request_id: "push-large",
                        kind: "push",
                        expires_at_ms: now_ms() + 120_000,
                    },
                    &vec![b'a'; length],
                    &mac_sign,
                    &phone_ecdh.public_key(),
                )
                .unwrap();
                let body =
                    serde_json::to_vec(&json!({"device_id":"phone-push","envelope":envelope}))
                        .unwrap();
                (body.len() <= 4096
                    && serde_json::to_vec(&push::payload(&envelope, &binding))
                        .unwrap()
                        .len()
                        > 4096)
                    .then_some(body)
            })
            .expect("APNs overhead creates a 4 KiB payload boundary below the request limit");
        let apns_large = sign(
            &epoch,
            client
                .post(format!("{url}{path}"))
                .body(too_large_for_apns.clone()),
            &mac_sign,
            "mac-1",
            "POST",
            path,
            &too_large_for_apns,
            58,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(apns_large.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let accepted = sign(
            &epoch,
            client.post(format!("{url}{path}")).body(body.clone()),
            &mac_sign,
            "mac-1",
            "POST",
            path,
            &body,
            56,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(accepted.status(), StatusCode::ACCEPTED);
        {
            let pushed = delivered.lock().unwrap();
            assert_eq!(pushed.len(), 1);
            assert_eq!(pushed[0]["aps"]["alert"]["body"], "Nueva actividad");
            assert_eq!(pushed[0]["aps"]["mutable-content"], 1);
            assert_eq!(pushed[0]["alondra"]["push_binding"], binding);
            assert!(pushed[0].to_string().contains("ciphertext"));
            assert!(!pushed[0].to_string().contains("private title"));
            let headers = &delivered_headers.lock().unwrap()[0];
            assert_eq!(headers["apns-push-type"], "alert");
            assert_eq!(headers["apns-priority"], "10");
            assert!(headers.get("apns-collapse-id").is_none(), "an old Mac sends no collapse id");
        }

        let extended = |extra: Value| {
            let mut body = json!({"device_id":"phone-push","envelope":envelope});
            body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            serde_json::to_vec(&body).unwrap()
        };
        let collapse_id = URL_SAFE_NO_PAD.encode([9u8; 16]);
        for (counter, extra, status) in [
            (90, json!({"collapse_id": collapse_id}), StatusCode::ACCEPTED),
            (91, json!({"push_type": "background"}), StatusCode::ACCEPTED),
            (92, json!({"collapse_id": "a".repeat(65)}), StatusCode::BAD_REQUEST),
            (93, json!({"collapse_id": "card\nforged"}), StatusCode::BAD_REQUEST),
            (94, json!({"push_type": "voip"}), StatusCode::BAD_REQUEST),
            (95, json!({"push_type": "badge", "badge": 4}), StatusCode::ACCEPTED),
            (96, json!({"push_type": "badge"}), StatusCode::BAD_REQUEST),
            (97, json!({"badge": 4}), StatusCode::BAD_REQUEST),
            (98, json!({"push_type": "background", "badge": 4}), StatusCode::BAD_REQUEST),
            (99, json!({"push_type": "badge", "badge": 100_000}), StatusCode::BAD_REQUEST),
            (100, json!({"push_type": "badge", "badge": 4, "collapse_id": collapse_id}), StatusCode::BAD_REQUEST),
        ] {
            let body = extended(extra.clone());
            let response = sign(
                &epoch,
                client.post(format!("{url}{path}")).body(body.clone()),
                &mac_sign,
                "mac-1",
                "POST",
                path,
                &body,
                counter,
            )
            .send()
            .await
            .unwrap();
            assert_eq!(response.status(), status, "{extra}");
        }
        {
            let pushed = delivered.lock().unwrap();
            let headers = delivered_headers.lock().unwrap();
            assert_eq!(pushed.len(), 4, "invalid collapse ids, push types and badges never reach APNs");
            assert_eq!(headers[1]["apns-collapse-id"], collapse_id.as_str());
            assert_eq!(headers[1]["apns-push-type"], "alert");
            assert_eq!(pushed[1]["aps"]["mutable-content"], 1);
            assert_eq!(headers[2]["apns-push-type"], "background");
            assert_eq!(headers[2]["apns-priority"], "5", "APNs rejects a background push at priority 10");
            assert_eq!(pushed[2]["aps"], json!({"content-available": 1}), "nothing that would show or sound");
            assert_eq!(pushed[2]["alondra"]["push_binding"], binding);
            assert!(pushed[2]["alondra"]["envelope"]["ciphertext"].is_string());
            assert_eq!(pushed[3], json!({"aps": {"badge": 4}}), "only the count, which the system sets itself");
            assert_eq!(headers[3]["apns-push-type"], "alert");
            assert_eq!(headers[3]["apns-priority"], "5");
            assert_eq!(headers[3]["apns-expiration"], "0", "never stored in place of an alert waiting for an offline phone");
        }
        delivered.lock().unwrap().truncate(1);
        delivered_headers.lock().unwrap().truncate(1);

        let rejected_envelope = Envelope::seal_random(
            Context {
                route: "mac-1",
                sender: "mac-1",
                recipient: "phone-push",
                request_id: "push-reject",
                kind: "push",
                expires_at_ms: now_ms() + 120_000,
            },
            br#"{"event":"unreadable"}"#,
            &mac_sign,
            &phone_ecdh.public_key(),
        )
        .unwrap();
        let rejected_body =
            serde_json::to_vec(&json!({"device_id":"phone-push","envelope":rejected_envelope}))
                .unwrap();
        let rejected = sign(
            &epoch,
            client
                .post(format!("{url}{path}"))
                .body(rejected_body.clone()),
            &mac_sign,
            "mac-1",
            "POST",
            path,
            &rejected_body,
            59,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(
            rejected.status(),
            StatusCode::BAD_GATEWAY,
            "APNs rejection must not look accepted"
        );
        assert_eq!(rejected.json::<Value>().await.unwrap()["error"], "apns_delivery_failed");

        let gone_envelope = Envelope::seal_random(
            Context {
                route: "mac-1",
                sender: "mac-1",
                recipient: "phone-push",
                request_id: "push-gone",
                kind: "push",
                expires_at_ms: now_ms() + 120_000,
            },
            br#"{"event":"unreadable"}"#,
            &mac_sign,
            &phone_ecdh.public_key(),
        )
        .unwrap();
        let gone_body =
            serde_json::to_vec(&json!({"device_id":"phone-push","envelope":gone_envelope}))
                .unwrap();
        let gone = sign(
            &epoch,
            client.post(format!("{url}{path}")).body(gone_body.clone()),
            &mac_sign,
            "mac-1",
            "POST",
            path,
            &gone_body,
            61,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(gone.status(), StatusCode::GONE, "a dead APNs token is not an outage");
        assert_eq!(gone.json::<Value>().await.unwrap()["error"], "push_registration_gone");
        assert!(
            server.auth.push_registration("phone-push").is_none(),
            "Apple never delivers to a dead token again; the phone has to register a live one"
        );
        let stored: Value = serde_json::from_slice(
            &std::fs::read(&server.auth.config.device_registry_path).unwrap(),
        )
        .unwrap();
        assert!(stored["push"].get("phone-push").is_none());

        server
            .auth
            .revoke(
                &auth::Identity {
                    device_id: "mac-1".into(),
                    owner_sub: "owner".into(),
                },
                "phone-push",
            )
            .unwrap();
        assert!(server.auth.push_registration("phone-push").is_none());
        let persisted: Value = serde_json::from_slice(
            &std::fs::read(&server.auth.config.device_registry_path).unwrap(),
        )
        .unwrap();
        assert!(persisted["push"].as_object().unwrap().is_empty());
        let denied = sign(
            &epoch,
            client.post(format!("{url}{path}")).body(body.clone()),
            &mac_sign,
            "mac-1",
            "POST",
            path,
            &body,
            57,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(denied.status(), StatusCode::GONE, "a revoked phone is gone for the Mac");
        relay_task.abort();
        apns_task.abort();
    }

    /// Two physical clients each have independent identities approved by both Macs.
    /// This exercises relay authorization, not physical hardware or client profile UI.
    #[tokio::test]
    async fn multi_mac_links_isolate_accounts_and_revoke_only_the_selected_link() {
        let captured = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let seen = captured.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let apns = tokio::spawn(async move {
            let router = Router::new().route("/3/device/{token}", post(move |Json(body): Json<Value>| {
                let seen = seen.clone();
                async move { seen.lock().unwrap().push(body); StatusCode::OK }
            }));
            axum::serve(listener, router).await.unwrap();
        });
        let mac_ids = ["mac-1", "mac-a2", "mac-b1"];
        let owners = ["owner", "owner", "owner-b"];
        let mac_keys: Vec<_> = (71..74).map(|n| SigningKey::from_slice(&[n; 32]).unwrap()).collect();
        let mac_agreements: Vec<_> = (74..77).map(|n| SecretKey::from_slice(&[n; 32]).unwrap()).collect();
        let (url, server, relay) = test_server_with_apns(&mac_keys[0], Some(endpoint)).await;
        let epoch = server.auth.server_epoch();
        let http = reqwest::Client::new();
        let nonce = std::cell::Cell::new(0u8);
        let request = |key: &SigningKey, id: &str, owner: Option<&str>, method: &str, path: &str, body: Vec<u8>| {
            nonce.set(nonce.get().checked_add(1).unwrap());
            let mut builder = http.request(reqwest::Method::from_bytes(method.as_bytes()).unwrap(), format!("{url}{path}"));
            if let Some(owner) = owner { builder = builder.bearer_auth(jwt(owner, now_ms())); }
            sign(&epoch, builder.body(body.clone()), key, id, method, path, &body, nonce.get())
        };
        for m in 1..3 {
            let body = serde_json::to_vec(&json!({"mac_id":mac_ids[m], "signing_key":key_bytes(&mac_keys[m]),
                "agreement_key":URL_SAFE_NO_PAD.encode(mac_agreements[m].public_key().to_sec1_point(false).as_bytes())})).unwrap();
            assert_eq!(request(&mac_keys[m], mac_ids[m], Some(owners[m]), "POST", "/v1/macs", body)
                .send().await.unwrap().status(), StatusCode::CREATED);
        }
        for (owner, expected) in [("owner", json!([{"mac_id":"mac-1"},{"mac_id":"mac-a2"}])),
            ("owner-b", json!([{"mac_id":"mac-b1"}]))] {
            let reply = http.get(format!("{url}/v1/macs?owner_sub=owner-b"))
                .bearer_auth(jwt(owner, now_ms())).send().await.unwrap();
            assert_eq!(reply.status(), StatusCode::OK);
            assert_eq!(reply.json::<Value>().await.unwrap()["macs"], expected);
        }
        // client-one and client-two each own one link per Mac; owner-b has its own client.
        let links = [("client-one-a1", 0), ("client-one-a2", 1),
            ("client-two-a1", 0), ("client-two-a2", 1), ("client-other-b1", 2)];
        let client_keys: Vec<_> = (81..86).map(|n| SigningKey::from_slice(&[n; 32]).unwrap()).collect();
        let client_agreements: Vec<_> = (86..91).map(|n| SecretKey::from_slice(&[n; 32]).unwrap()).collect();
        for (i, (id, m)) in links.iter().enumerate() {
            let body = serde_json::to_vec(&PhoneDevice { device_id: (*id).into(), owner_sub: owners[*m].into(),
                signing_key: key_bytes(&client_keys[i]) }).unwrap();
            assert_eq!(request(&mac_keys[*m], mac_ids[*m], None, "POST", "/v1/devices", body)
                .send().await.unwrap().status(), StatusCode::CREATED);
            // The same physical client's APNs token can be used on both independent links.
            let token = format!("{:02x}", 10 + i / 2).repeat(32);
            let body = serde_json::to_vec(&json!({"token":token,"environment":"sandbox",
                "push_binding":URL_SAFE_NO_PAD.encode([i as u8 + 1;32])})).unwrap();
            assert_eq!(request(&client_keys[i], id, Some(owners[*m]), "POST", "/v1/push/register", body)
                .send().await.unwrap().status(), StatusCode::NO_CONTENT);
        }
        for (i, (id, m)) in links.iter().enumerate() {
            assert_eq!(request(&client_keys[i], id, Some(owners[*m]), "DELETE", "/v1/iroh-node", vec![])
                .send().await.unwrap().status(), StatusCode::NO_CONTENT);
        }
        assert_eq!(request(&client_keys[0], links[0].0, Some("owner-b"), "DELETE", "/v1/iroh-node", vec![])
            .send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        let registration = serde_json::to_vec(&json!({"token":"ff".repeat(32),"environment":"sandbox",
            "push_binding":URL_SAFE_NO_PAD.encode([99u8;32])})).unwrap();
        assert_eq!(request(&client_keys[0], links[0].0, Some("owner-b"), "POST", "/v1/push/register", registration)
            .send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        for wrong_mac in [1, 2] {
            assert!(!request(&mac_keys[wrong_mac], mac_ids[wrong_mac], None, "DELETE",
                "/v1/devices/client-one-a1", vec![]).send().await.unwrap().status().is_success());
        }
        assert_eq!(request(&mac_keys[0], mac_ids[0], None, "DELETE", "/v1/devices/client-one-a1", vec![])
            .send().await.unwrap().status(), StatusCode::NO_CONTENT);
        assert!(server.auth.push_registration(links[0].0).is_none());
        assert_eq!(request(&client_keys[0], links[0].0, Some("owner"), "DELETE", "/v1/iroh-node", vec![])
            .send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        // A removed registration must not dispatch, even with the approving Mac's signature.
        // Nor may another Mac use a surviving link's registration, even in the same account.
        for (link, mac) in [(0, 0), (1, 0)] {
            let id = links[link].0;
            let push = Envelope::seal_random(Context { route: mac_ids[mac], sender: mac_ids[mac], recipient: id,
                request_id: &format!("denied-push-{link}"), kind: "push", expires_at_ms: now_ms() + 120_000 },
                br#"{"payload":{"aps":{"alert":"must not arrive"},"alondra":{}}}"#,
                &mac_keys[mac], &client_agreements[link].public_key()).unwrap();
            let body = serde_json::to_vec(&json!({"device_id":id,"envelope":push})).unwrap();
            assert_eq!(request(&mac_keys[mac], mac_ids[mac], None, "POST", "/v1/mac/push", body)
                .send().await.unwrap().status(), if link == 0 { StatusCode::GONE } else { StatusCode::NOT_FOUND });
            assert!(captured.lock().unwrap().is_empty(), "denied push reached APNs");
        }
        for (i, (id, m)) in links.iter().enumerate().skip(1) {
            // Both the other link of client-one and all links of client-two remain usable.
            assert_eq!(request(&client_keys[i], id, Some(owners[*m]), "DELETE", "/v1/iroh-node", vec![])
                .send().await.unwrap().status(), StatusCode::NO_CONTENT);
            let registration = server.auth.push_registration(id).unwrap();
            assert_eq!(registration.push_binding, URL_SAFE_NO_PAD.encode([i as u8 + 1;32]));
            let plaintext = serde_json::to_vec(&json!({"push_binding":registration.push_binding,
                "payload":{"aps":{"alert":"private"},"alondra":{}},"with_sound":false})).unwrap();
            let push = Envelope::seal_random(Context { route: mac_ids[*m], sender: mac_ids[*m], recipient: id,
                request_id: &format!("push-{i}"), kind: "push", expires_at_ms: now_ms() + 120_000 },
                &plaintext, &mac_keys[*m], &client_agreements[i].public_key()).unwrap();
            let body = serde_json::to_vec(&json!({"device_id":id,"envelope":push})).unwrap();
            assert_eq!(request(&mac_keys[*m], mac_ids[*m], None, "POST", "/v1/mac/push", body)
                .send().await.unwrap().status(), StatusCode::ACCEPTED);
        }
        let delivered = captured.lock().unwrap();
        assert_eq!(delivered.len(), links.len() - 1);
        for (i, (body, (id, m))) in delivered.iter().zip(links.iter().skip(1)).enumerate() {
            let link = i + 1;
            let envelope: Envelope = serde_json::from_value(body["alondra"]["envelope"].clone()).unwrap();
            let plaintext = envelope.open(mac_keys[*m].verifying_key(), &client_agreements[link],
                mac_ids[*m], id, mac_ids[*m], now_ms()).unwrap();
            let content: Value = serde_json::from_slice(&plaintext).unwrap();
            assert_eq!(content["push_binding"], URL_SAFE_NO_PAD.encode([link as u8 + 1;32]));
            assert_eq!(body["alondra"]["push_binding"], content["push_binding"]);
        }
        drop(delivered);
        relay.abort();
        apns.abort();
    }

    #[tokio::test]
    async fn signed_http_rejects_wrong_identities_and_replay() {
        let mac_sign = SigningKey::from_slice(&[1u8; 32]).unwrap();
        let phone_sign = SigningKey::from_slice(&[2u8; 32]).unwrap();
        let wrong_sign = SigningKey::from_slice(&[3u8; 32]).unwrap();
        let (url, server, join) = test_server(&mac_sign).await;
        let epoch = server.auth.server_epoch();
        let client = reqwest::Client::new();

        let device = PhoneDevice {
            device_id: "phone-1".into(),
            owner_sub: "owner".into(),
            signing_key: key_bytes(&phone_sign),
        };
        let body = serde_json::to_vec(&device).unwrap();
        let response = sign(
            &epoch,
            client.post(format!("{url}/v1/devices")).body(body.clone()),
            &mac_sign,
            "mac-1",
            "POST",
            "/v1/devices",
            &body,
            1,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let now = now_ms();
        let path = "/v1/iroh-node";
        let as_phone = |user: &str, key: &SigningKey, nonce: u8| sign(
            &epoch,
            client.delete(format!("{url}{path}")).bearer_auth(jwt(user, now)),
            key,
            "phone-1",
            "DELETE",
            path,
            &[],
            nonce,
        );
        let as_mac = |key: &SigningKey, nonce: u8| sign(
            &epoch,
            client.delete(format!("{url}{path}")),
            key,
            "mac-1",
            "DELETE",
            path,
            &[],
            nonce,
        );
        assert_eq!(as_phone("intruder", &phone_sign, 2).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        assert_eq!(as_phone("owner", &wrong_sign, 3).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        let request = as_phone("owner", &phone_sign, 4).build().unwrap();
        let clone = request.try_clone().unwrap();
        assert_eq!(client.execute(request).await.unwrap().status(), StatusCode::NO_CONTENT);
        assert_eq!(
            client.execute(clone).await.unwrap().status(),
            StatusCode::UNAUTHORIZED,
            "one-time device nonce must reject replay"
        );
        assert_eq!(as_mac(&wrong_sign, 6).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        assert_eq!(as_mac(&mac_sign, 7).send().await.unwrap().status(), StatusCode::NO_CONTENT);
        let (url_after_restart, restarted, restarted_join) = test_server(&mac_sign).await;
        assert_ne!(restarted.epoch, server.epoch);
        let replay_after_restart = sign(
            &epoch,
            client.delete(format!("{url_after_restart}/v1/iroh-node")),
            &mac_sign,
            "mac-1",
            "DELETE",
            "/v1/iroh-node",
            &[],
            13,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(
            replay_after_restart.status(),
            StatusCode::UNAUTHORIZED,
            "old signed epoch must fail after restart"
        );
        let revoke_path = "/v1/devices/phone-1";
        let unauthorized_revoke = sign(
            &epoch,
            client.delete(format!("{url}{revoke_path}")),
            &wrong_sign,
            "mac-1",
            "DELETE",
            revoke_path,
            &[],
            16,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(unauthorized_revoke.status(), StatusCode::UNAUTHORIZED);
        assert!(server.auth.is_phone_registered("phone-1"));
        let revoked = sign(
            &epoch,
            client.delete(format!("{url}{revoke_path}")),
            &mac_sign,
            "mac-1",
            "DELETE",
            revoke_path,
            &[],
            14,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
        assert!(!server.auth.is_phone_registered("phone-1"));
        let persisted: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&server.auth.config.device_registry_path).unwrap(),
        )
        .unwrap();
        assert!(
            persisted["devices"].as_array().unwrap().is_empty(),
            "revocation must survive relay restart"
        );
        assert_eq!(persisted["revoked_ids"], serde_json::json!(["phone-1"]));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&server.auth.config.device_registry_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "revocation replacement must remain owner-only"
        );
        let after_restart = Auth::test(
            server.auth.config.clone(),
            include_bytes!("../tests/fixtures/jwks.json"),
        )
        .unwrap();
        assert!(!after_restart.is_phone_registered("phone-1"));
        assert!(
            after_restart
                .enroll(
                    &auth::Identity {
                        device_id: "mac-1".into(),
                        owner_sub: "owner".into()
                    },
                    device.clone()
                )
                .is_err(),
            "revoked ID must stay blocked after restart"
        );
        let reenroll_body = serde_json::to_vec(&device).unwrap();
        let reenroll = sign(
            &epoch,
            client
                .post(format!("{url}/v1/devices"))
                .body(reenroll_body.clone()),
            &mac_sign,
            "mac-1",
            "POST",
            "/v1/devices",
            &reenroll_body,
            17,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(reenroll.status(), StatusCode::CONFLICT);
        assert_eq!(as_phone("owner", &phone_sign, 15).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        restarted_join.abort();
        join.abort();
    }

    /// Apps and daemons from before iroh still call the mailbox. They read a 404 as "no
    /// mailbox here"; a 410 would read as a lost result.
    #[tokio::test]
    async fn old_clients_calling_the_mailbox_learn_it_is_gone() {
        let mac_key = SigningKey::from_slice(&[41u8; 32]).unwrap();
        let phone_key = SigningKey::from_slice(&[42u8; 32]).unwrap();
        let (url, server, join) = test_server(&mac_key).await;
        server.auth.enroll(&auth::Identity { device_id: "mac-1".into(), owner_sub: "owner".into() },
            PhoneDevice { device_id: "phone-1".into(), owner_sub: "owner".into(), signing_key: key_bytes(&phone_key) }).unwrap();
        let epoch = server.auth.server_epoch();
        let client = reqwest::Client::new();
        let calls = [
            (true, "POST", "/v1/requests", br#"{"kind":"request"}"#.to_vec()),
            (true, "GET", "/v1/requests/request-1", vec![]),
            (true, "GET", "/v1/requests/request-1?wait_s=25", vec![]),
            (true, "POST", "/v1/requests/request-1/ack", br#"{"revision":1}"#.to_vec()),
            (true, "GET", "/v1/changes?since=0&wait_s=25", vec![]),
            (false, "GET", "/v1/work?wait_s=25", vec![]),
            (false, "POST", "/v1/work/request-1/receipt", vec![]),
            (false, "POST", "/v1/work/request-1/response", br#"{"kind":"response"}"#.to_vec()),
        ];
        for (nonce, (phone, method, path, body)) in calls.into_iter().enumerate() {
            let builder = client
                .request(reqwest::Method::from_bytes(method.as_bytes()).unwrap(), format!("{url}{path}"))
                .body(body.clone());
            let signed = if phone {
                sign(&epoch, builder.bearer_auth(jwt("owner", now_ms())), &phone_key, "phone-1", method, path, &body, nonce as u8 + 1)
            } else {
                sign(&epoch, builder, &mac_key, "mac-1", method, path, &body, nonce as u8 + 1)
            };
            for response in [signed.send().await.unwrap(),
                client.request(reqwest::Method::from_bytes(method.as_bytes()).unwrap(), format!("{url}{path}")).send().await.unwrap()] {
                assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
                assert_eq!(response.json::<Value>().await.unwrap(), json!({"error":"mailbox_removed"}), "{method} {path}");
            }
        }
        let boot: Value = client.get(format!("{url}/v1/bootstrap")).send().await.unwrap().json().await.unwrap();
        assert!(boot.get("request_wait").is_none() && boot.get("result_ack").is_none(), "{boot}");
        join.abort();
    }

    #[tokio::test]
    async fn mac_auth_accepts_requests_verified_in_reverse_arrival_order() {
        let key = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let (url, server, join) = test_server(&key).await;
        let client = reqwest::Client::new();
        let path = "/v1/pairings/probe";
        let uri: Uri = path.parse().unwrap();
        let earlier_arrival = now_ms();
        let later_arrival = earlier_arrival + 1;
        let headers = |nonce| {
            sign(
                &server.auth.server_epoch(),
                client.get(format!("{url}{path}")),
                &key,
                "mac-1",
                "GET",
                path,
                &[],
                nonce,
            )
            .build()
            .unwrap()
            .headers()
            .clone()
        };
        let earlier = headers(1);
        let later = headers(2);
        // Deterministic interleaving: the newer arrival wins the verifier lock.
        // A timestamp sampled at handler entry must not become a clock rollback.
        assert!(
            server
                .auth
                .authenticate_mac(&later, &Method::GET, &uri, &[], later_arrival).await
                .is_ok()
        );
        assert!(
            server
                .auth
                .authenticate_mac(&earlier, &Method::GET, &uri, &[], earlier_arrival).await
                .is_ok()
        );
        assert!(
            server
                .auth
                .authenticate_mac(&earlier, &Method::GET, &uri, &[], later_arrival).await
                .is_err(),
            "accepting reordered requests must not permit nonce replay"
        );
        join.abort();
    }

    #[tokio::test]
    async fn mac_auth_rejects_replay_when_wall_clock_moves_back() {
        let mac_sign = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let (_url, server, join) = test_server(&mac_sign).await;
        let epoch = server.auth.server_epoch();
        let uri: Uri = "/v1/pairings/probe".parse().unwrap();
        let headers_at = |time_ms: u64, nonce: u8| {
            let mut headers = HeaderMap::new();
            let nonce_bytes = [nonce; 16];
            let signature = sign_http_challenge(
                &mac_sign,
                "mac-1",
                "GET",
                uri.path_and_query().unwrap().as_str(),
                &epoch,
                &[],
                time_ms,
                &nonce_bytes,
            )
            .unwrap();
            headers.insert("x-device-id", "mac-1".parse().unwrap());
            headers.insert(
                "x-relay-epoch",
                URL_SAFE_NO_PAD.encode(epoch).parse().unwrap(),
            );
            headers.insert(
                "x-device-timestamp-ms",
                time_ms.to_string().parse().unwrap(),
            );
            headers.insert(
                "x-device-nonce",
                URL_SAFE_NO_PAD.encode(nonce_bytes).parse().unwrap(),
            );
            headers.insert("x-device-signature", signature.parse().unwrap());
            headers
        };
        server.auth.set_verification_time(1_000_000);
        let early = headers_at(1_000_000, 1);
        assert!(
            server
                .auth
                .authenticate_mac(&early, &Method::GET, &uri, &[], 1_000_000).await
                .is_ok()
        );
        server.auth.set_verification_time(1_300_001);
        let later = headers_at(1_300_001, 2);
        assert!(
            server
                .auth
                .authenticate_mac(&later, &Method::GET, &uri, &[], 1_300_001).await
                .is_ok()
        );
        server.auth.set_verification_time(1_000_000);
        assert!(
            server
                .auth
                .authenticate_mac(&early, &Method::GET, &uri, &[], 1_000_000).await
                .is_err(),
            "rollback must not revive a pruned nonce"
        );
        join.abort();
    }

    #[tokio::test]
    async fn keycloak_jwks_rotation_fetches_unknown_kid_and_removes_retired_key() {
        let scratch_directory = crate::test_support::Directory::new();
        let current = Arc::new(std::sync::RwLock::new(
            include_bytes!("../tests/fixtures/jwks.json").to_vec(),
        ));
        let serving = current.clone();
        let jwks_router = Router::new().route(
            "/certs",
            get(move || {
                let bytes = serving.read().unwrap().clone();
                async move { ([(header::CONTENT_TYPE, "application/json")], bytes) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let jwks_url = format!("http://{}/certs", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            axum::serve(listener, jwks_router).await.unwrap();
        });
        let mac_key = SigningKey::from_slice(&[9u8; 32]).unwrap();
        let config = Config {
            listen: "127.0.0.1:0".into(),
            issuer: "https://id.example/realms/alondra".into(),
            audience: "alondra-relay".into(),
            jwks_url,
            allow_internal_http_jwks: false,
            owner_sub: "owner".into(),
            mac_id: "mac-1".into(),
            mac_signing_key: key_bytes(&mac_key),
            device_registry_path: scratch_directory.parent().unwrap().join(format!(
                "alondra-jwks-rotation-{}-{}.json",
                std::process::id(),
                NEXT_TEST_REGISTRY.fetch_add(1, Ordering::Relaxed)
            )),
            apns: None,
        };
        let auth = Auth::open(config).await.unwrap();
        let now = now_ms();
        let old = jwt("owner", now);
        assert_eq!(auth.verify_jwt(&old, now).await.unwrap(), "owner");
        *current.write().unwrap() = include_bytes!("../tests/fixtures/jwks-rotated.json").to_vec();
        let rotated = jwt_with(
            "owner",
            now,
            "rotated-key",
            include_bytes!("../tests/fixtures/rsa-rotated-test-only.pkcs8.der"),
        );
        assert_eq!(
            auth.verify_jwt(&rotated, now).await.unwrap(),
            "owner",
            "unknown kid triggers one bounded JWKS fetch"
        );
        assert!(
            auth.verify_jwt(&old, now).await.is_err(),
            "retired key must disappear when JWKS rotates"
        );
        *current.write().unwrap() = include_bytes!("../tests/fixtures/jwks.json").to_vec();
        auth.refresh_jwks().await.unwrap();
        assert_eq!(
            auth.verify_jwt(&old, now).await.unwrap(),
            "owner",
            "periodic refresh path replaces key set too"
        );
        assert!(auth.verify_jwt(&rotated, now).await.is_err());
        handle.abort();
    }
    #[tokio::test]
    async fn recovered_account_token_cannot_replace_device_key_or_act_as_the_device() {
        let mac = SigningKey::from_slice(&[81;32]).unwrap();
        let approved = SigningKey::from_slice(&[82;32]).unwrap();
        let attacker = SigningKey::from_slice(&[83;32]).unwrap();
        let (url, server, handle) = test_server(&mac).await;
        let epoch = server.auth.server_epoch();
        server.auth.enroll(&auth::Identity {device_id:"mac-1".into(),owner_sub:"owner".into()},
            PhoneDevice {device_id:"phone-approved".into(),owner_sub:"owner".into(),signing_key:key_bytes(&approved)}).unwrap();
        let now=now_ms();
        // Model account recovery's security consequence: a genuinely valid JWT
        // for the original subject, but none of the previously approved keys.
        let recovered=jwt("owner",now);
        assert_eq!(server.auth.verify_jwt(&recovered,now).await.unwrap(),"owner");
        let client=reqwest::Client::new();
        let path="/v1/iroh-node";
        // Positive control: the original device key with the very same recovered
        // account credential is still accepted.
        let response=sign(&epoch,client.delete(format!("{url}{path}")).bearer_auth(&recovered),
            &approved,"phone-approved","DELETE",path,&[],1).send().await.unwrap();
        assert_eq!(response.status(),StatusCode::NO_CONTENT);
        for (id,nonce) in [("phone-approved",2),("phone-attacker",3)] {
            let denied=sign(&epoch,client.delete(format!("{url}{path}")).bearer_auth(&recovered),
                &attacker,id,"DELETE",path,&[],nonce).send().await.unwrap();
            assert_eq!(denied.status(),StatusCode::UNAUTHORIZED);
        }
        let body=serde_json::to_vec(&json!({"device_id":"phone-approved","owner_sub":"owner","signing_key":key_bytes(&attacker)})).unwrap();
        let denied=sign(&epoch,client.post(format!("{url}/v1/devices")).bearer_auth(&recovered).body(body.clone()),
            &attacker,"mac-1","POST","/v1/devices",&body,4).send().await.unwrap();
        assert_eq!(denied.status(),StatusCode::UNAUTHORIZED);
        assert_eq!(server.auth.phone_key("phone-approved").unwrap().to_sec1_point(false),approved.verifying_key().to_sec1_point(false));
        handle.abort();
    }

    #[tokio::test]
    async fn mac_enrollment_is_owner_bound_create_only_and_durable() {
        let pilot_key = SigningKey::from_slice(&[61u8; 32]).unwrap();
        let mac_key = SigningKey::from_slice(&[62u8; 32]).unwrap();
        let replaced_key = SigningKey::from_slice(&[63u8; 32]).unwrap();
        let agreement = SecretKey::from_slice(&[64u8; 32]).unwrap();
        let (url, server, handle) = test_server(&pilot_key).await;
        let epoch = server.auth.server_epoch();
        let client = reqwest::Client::new();
        let body = serde_json::to_vec(&json!({
            "mac_id":"mac-second", "signing_key":key_bytes(&mac_key),
            "agreement_key":URL_SAFE_NO_PAD.encode(agreement.public_key().to_sec1_point(false).as_bytes())
        })).unwrap();
        let path = "/v1/macs";
        let attempt = |key: &SigningKey, owner: &str, bytes: Vec<u8>, nonce: u8| {
            sign(
                &epoch,
                client
                    .post(format!("{url}{path}"))
                    .bearer_auth(jwt(owner, now_ms()))
                    .body(bytes.clone()),
                key,
                "mac-second",
                "POST",
                path,
                &bytes,
                nonce,
            )
        };
        // An account token without possession of the advertised private key cannot enroll it.
        let stolen_account = attempt(&replaced_key, "owner2", body.clone(), 1)
            .send()
            .await
            .unwrap();
        assert_eq!(stolen_account.status(), StatusCode::UNAUTHORIZED);
        let created = attempt(&mac_key, "owner2", body.clone(), 2)
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let repeated = attempt(&mac_key, "owner2", body.clone(), 3)
            .send()
            .await
            .unwrap();
        assert_eq!(repeated.status(), StatusCode::OK);
        let cross_owner = attempt(&mac_key, "owner", body.clone(), 4)
            .send()
            .await
            .unwrap();
        assert_ne!(cross_owner.status(), StatusCode::CREATED);
        let replaced_body = serde_json::to_vec(&json!({
            "mac_id":"mac-second", "signing_key":key_bytes(&replaced_key),
            "agreement_key":URL_SAFE_NO_PAD.encode(agreement.public_key().to_sec1_point(false).as_bytes())
        })).unwrap();
        let replacement = attempt(&replaced_key, "owner2", replaced_body.clone(), 5)
            .send()
            .await
            .unwrap();
        assert_eq!(replacement.status(), StatusCode::CONFLICT);
        assert!(server.auth.mac_for_owner("mac-second", "owner2").is_ok());
        assert!(server.auth.mac_for_owner("mac-second", "owner").is_err());
        assert!(server.auth.mac_for_owner("mac-1", "owner2").is_err());
        let pilot_body = serde_json::to_vec(&json!({
            "mac_id":"mac-1", "signing_key":key_bytes(&pilot_key),
            "agreement_key":URL_SAFE_NO_PAD.encode(agreement.public_key().to_sec1_point(false).as_bytes())
        })).unwrap();
        let pilot_complete = sign(
            &epoch,
            client
                .post(format!("{url}/v1/macs"))
                .bearer_auth(jwt("owner", now_ms()))
                .body(pilot_body.clone()),
            &pilot_key,
            "mac-1",
            "POST",
            "/v1/macs",
            &pilot_body,
            9,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(pilot_complete.status(), StatusCode::CREATED);
        // A valid account token can add a fresh Mac, but that Mac must not inherit
        // a phone already paired to the pilot Mac.
        let stolen_key = SigningKey::from_slice(&[65u8; 32]).unwrap();
        let stolen_body = serde_json::to_vec(&json!({
            "mac_id":"mac-stolen", "signing_key":key_bytes(&stolen_key),
            "agreement_key":URL_SAFE_NO_PAD.encode(agreement.public_key().to_sec1_point(false).as_bytes())
        })).unwrap();
        let fresh_mac = sign(
            &epoch,
            client
                .post(format!("{url}/v1/macs"))
                .bearer_auth(jwt("owner", now_ms()))
                .body(stolen_body.clone()),
            &stolen_key,
            "mac-stolen",
            "POST",
            "/v1/macs",
            &stolen_body,
            10,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(fresh_mac.status(), StatusCode::CREATED);
        let stolen_signed = sign(
            &epoch,
            client.delete(format!("{url}/v1/iroh-node")),
            &stolen_key,
            "mac-stolen",
            "DELETE",
            "/v1/iroh-node",
            &[],
            11,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(stolen_signed.status(), StatusCode::NO_CONTENT);
        let pilot_phone = PhoneDevice {
            device_id: "phone-pilot".into(),
            owner_sub: "owner".into(),
            signing_key: key_bytes(&replaced_key),
        };
        let pilot = auth::Identity {
            device_id: "mac-1".into(),
            owner_sub: "owner".into(),
        };
        let stolen = auth::Identity {
            device_id: "mac-stolen".into(),
            owner_sub: "owner".into(),
        };
        server.auth.enroll(&pilot, pilot_phone.clone()).unwrap();
        assert!(server.auth.enroll(&stolen, pilot_phone).is_err());
        assert!(server.auth.revoke(&stolen, "phone-pilot").is_err());
        assert_eq!(
            server
                .auth
                .paired_mac_for_phone("phone-pilot", "owner")
                .unwrap(),
            "mac-1"
        );
        let new_phone = PhoneDevice {
            device_id: "phone-owner2".into(),
            owner_sub: "owner2".into(),
            signing_key: key_bytes(&replaced_key),
        };
        assert!(
            server
                .auth
                .enroll(
                    &auth::Identity {
                        device_id: "mac-1".into(),
                        owner_sub: "owner".into()
                    },
                    new_phone.clone()
                )
                .is_err()
        );
        server
            .auth
            .enroll(
                &auth::Identity {
                    device_id: "mac-second".into(),
                    owner_sub: "owner2".into(),
                },
                new_phone,
            )
            .unwrap();
        let revoked = sign(
            &epoch,
            client.delete(format!("{url}/v1/macs/mac-second")),
            &mac_key,
            "mac-second",
            "DELETE",
            "/v1/macs/mac-second",
            &[],
            6,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
        assert!(!server.auth.is_mac_registered("mac-second"));
        let revoked_signed = sign(
            &epoch,
            client.delete(format!("{url}/v1/iroh-node")),
            &mac_key,
            "mac-second",
            "DELETE",
            "/v1/iroh-node",
            &[],
            8,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(revoked_signed.status(), StatusCode::UNAUTHORIZED);
        let after = attempt(&mac_key, "owner2", body.clone(), 7)
            .send()
            .await
            .unwrap();
        assert_eq!(after.status(), StatusCode::CONFLICT);
        let restarted = Auth::test(
            server.auth.config.clone(),
            include_bytes!("../tests/fixtures/jwks.json"),
        )
        .unwrap();
        assert!(
            restarted.is_mac_registered("mac-1"),
            "pilot pinned Mac migrates durably"
        );
        assert!(!restarted.is_mac_registered("mac-second"));
        assert!(
            restarted
                .enroll_mac(auth::MacDevice {
                    mac_id: "mac-second".into(),
                    owner_sub: "owner2".into(),
                    signing_key: key_bytes(&mac_key),
                    agreement_key: URL_SAFE_NO_PAD
                        .encode(agreement.public_key().to_sec1_point(false).as_bytes())
                })
                .is_err()
        );
        handle.abort();
    }
    #[tokio::test]
    async fn registry_schema_guard_rejects_rollback_and_ambiguous_bindings() {
        let pilot_key = SigningKey::from_slice(&[71u8; 32]).unwrap();
        let second_key = SigningKey::from_slice(&[72u8; 32]).unwrap();
        let (url, server, handle) = test_server(&pilot_key).await;
        let config = server.auth.config.clone();
        let pilot = auth::Identity {
            device_id: "mac-1".into(),
            owner_sub: "owner".into(),
        };
        server
            .auth
            .enroll(
                &pilot,
                PhoneDevice {
                    device_id: "phone-pilot".into(),
                    owner_sub: "owner".into(),
                    signing_key: key_bytes(&second_key),
                },
            )
            .unwrap();
        let second = auth::MacDevice {
            mac_id: "mac-second".into(),
            owner_sub: "other".into(),
            signing_key: key_bytes(&second_key),
            agreement_key: key_bytes(&second_key),
        };
        server.auth.enroll_mac(second).unwrap();
        let guarded: Value =
            serde_json::from_slice(&std::fs::read(&config.device_registry_path).unwrap()).unwrap();
        assert_eq!(guarded["schema_version"], 2);
        assert_eq!(guarded["phone_macs"]["phone-pilot"], "mac-1");
        assert!(
            config
                .device_registry_path
                .with_extension("schema-v2")
                .exists()
        );

        // The one-Mac pilot format can be upgraded without guessing a route.
        let mut legacy = config.clone();
        legacy.device_registry_path = config.device_registry_path.with_extension("pilot.json");
        let old_phone = PhoneDevice {
            device_id: "phone-old".into(),
            owner_sub: "owner".into(),
            signing_key: key_bytes(&second_key),
        };
        std::fs::write(
            &legacy.device_registry_path,
            serde_json::to_vec(&json!({
                "devices":[old_phone], "revoked_ids":[]
            }))
            .unwrap(),
        )
        .unwrap();
        let upgraded = Auth::test(
            legacy.clone(),
            include_bytes!("../tests/fixtures/jwks.json"),
        )
        .unwrap();
        assert_eq!(
            upgraded.paired_mac_for_phone("phone-old", "owner").unwrap(),
            "mac-1"
        );
        let upgraded_disk: Value =
            serde_json::from_slice(&std::fs::read(&legacy.device_registry_path).unwrap()).unwrap();
        assert_eq!(upgraded_disk["schema_version"], 2);
        assert_eq!(upgraded_disk["phone_macs"]["phone-old"], "mac-1");
        assert!(
            legacy
                .device_registry_path
                .with_extension("schema-v2")
                .exists()
        );

        // A multi-Mac v1 registry with a missing phone binding cannot be guessed.
        let mut ambiguous = guarded.clone();
        ambiguous.as_object_mut().unwrap().remove("schema_version");
        ambiguous["phone_macs"]
            .as_object_mut()
            .unwrap()
            .remove("phone-pilot");
        let mut imported = config.clone();
        imported.device_registry_path =
            config.device_registry_path.with_extension("ambiguous.json");
        std::fs::write(
            &imported.device_registry_path,
            serde_json::to_vec(&ambiguous).unwrap(),
        )
        .unwrap();
        assert!(Auth::test(imported, include_bytes!("../tests/fixtures/jwks.json")).is_err());

        // A rolled-back binary discards unknown v2 fields; the separate guard survives.
        let mut downgraded = guarded;
        for field in ["schema_version", "phone_macs", "macs", "revoked_mac_ids"] {
            downgraded.as_object_mut().unwrap().remove(field);
        }
        std::fs::write(
            &config.device_registry_path,
            serde_json::to_vec(&downgraded).unwrap(),
        )
        .unwrap();
        assert!(Auth::test(config, include_bytes!("../tests/fixtures/jwks.json")).is_err());
        handle.abort();
        drop(url);
    }

    #[tokio::test]
    async fn enrollment_storage_failure_is_503_rolls_back_and_retry_requires_reload() {
        for mac_registration in [true,false] {
            let pilot_key=SigningKey::from_slice(&[81u8;32]).unwrap();
            let new_key=SigningKey::from_slice(&[82u8;32]).unwrap();
            let (url,server,handle)=test_server(&pilot_key).await;
            let config=server.auth.config.clone();
            let registry_before=std::fs::read(&config.device_registry_path).unwrap();
            // Real deterministic I/O failure even as root: the staging file path
            // is a directory. The existing committed registry remains untouched.
            let blocked=config.device_registry_path.with_extension("tmp");
            std::fs::create_dir(&blocked).unwrap();
            let (path,device,signing,body)=if mac_registration {
                ("/v1/macs","mac-storage-test",&new_key,json!({"mac_id":"mac-storage-test",
                    "signing_key":key_bytes(&new_key),"agreement_key":key_bytes(&new_key)}))
            } else {
                ("/v1/devices","mac-1",&pilot_key,json!({"device_id":"client-storage-test",
                    "owner_sub":"owner","signing_key":key_bytes(&new_key)}))
            };
            let bytes=serde_json::to_vec(&body).unwrap();
            let client=reqwest::Client::new();
            for nonce in [81,82] {
                let response=sign(&server.auth.server_epoch(),client.post(format!("{url}{path}"))
                    .bearer_auth(jwt("owner",now_ms())).body(bytes.clone()),signing,device,"POST",path,&bytes,nonce)
                    .send().await.unwrap();
                assert_eq!(response.status(),StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(response.json::<Value>().await.unwrap(),json!({"error":"registry_unavailable"}),
                    "neither filesystem details nor identity-conflict advice may escape");
            }
            assert!(!server.auth.is_mac_registered("mac-storage-test"));
            assert!(!server.auth.is_phone_registered("client-storage-test"));
            assert!(!server.auth.jwks_ready(),"persistence poison remains fail closed");
            assert_eq!(std::fs::read(&config.device_registry_path).unwrap(),registry_before);
            std::fs::remove_dir(&blocked).unwrap();
            let restarted=Auth::test(config,include_bytes!("../tests/fixtures/jwks.json")).unwrap();
            if mac_registration {
                let entry=auth::MacDevice{mac_id:"mac-storage-test".into(),owner_sub:"owner".into(),
                    signing_key:key_bytes(&new_key),agreement_key:key_bytes(&new_key)};
                assert!(restarted.enroll_mac(entry.clone()).unwrap());
                assert!(!restarted.enroll_mac(entry.clone()).unwrap());
                let mut collision=entry;collision.signing_key=key_bytes(&pilot_key);
                assert_eq!(enrollment_error(restarted.enroll_mac(collision).unwrap_err(),"mac_enrollment_failed").0,StatusCode::CONFLICT);
            } else {
                let identity=auth::Identity{device_id:"mac-1".into(),owner_sub:"owner".into()};
                let entry=PhoneDevice{device_id:"client-storage-test".into(),owner_sub:"owner".into(),signing_key:key_bytes(&new_key)};
                restarted.enroll(&identity,entry.clone()).unwrap();
                let mut collision=entry;collision.signing_key=key_bytes(&pilot_key);
                assert_eq!(enrollment_error(restarted.enroll(&identity,collision).unwrap_err(),"device_enrollment_failed").0,StatusCode::CONFLICT);
            }
            handle.abort();
        }
    }

    #[tokio::test]
    async fn post_rename_sync_failure_poison_blocks_auth_until_restart() {
        let pilot_key = SigningKey::from_slice(&[73u8; 32]).unwrap();
        let phone_key = SigningKey::from_slice(&[74u8; 32]).unwrap();
        let (_url, server, handle) = test_server(&pilot_key).await;
        let config = server.auth.config.clone();
        let phone = PhoneDevice {
            device_id: "phone-after-rename".into(),
            owner_sub: "owner".into(),
            signing_key: key_bytes(&phone_key),
        };
        *auth::FAIL_AFTER_RENAME_PATH.lock().unwrap() = Some(config.device_registry_path.clone());
        assert!(
            server
                .auth
                .enroll(
                    &auth::Identity {
                        device_id: "mac-1".into(),
                        owner_sub: "owner".into()
                    },
                    phone
                )
                .is_err()
        );
        assert!(!server.auth.jwks_ready());
        assert!(server.auth.mac_key("mac-1").is_err());
        assert!(server.auth.phone_key("phone-after-rename").is_err());
        assert!(
            !server.auth.is_phone_registered("phone-after-rename"),
            "memory rolled back"
        );
        let on_disk: Value =
            serde_json::from_slice(&std::fs::read(&config.device_registry_path).unwrap()).unwrap();
        assert!(
            on_disk["devices"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["device_id"] == "phone-after-rename"),
            "rename already committed on disk"
        );
        let restarted = Auth::test(config, include_bytes!("../tests/fixtures/jwks.json")).unwrap();
        assert!(restarted.jwks_ready());
        assert!(restarted.is_phone_registered("phone-after-rename"));
        handle.abort();
    }

    mod iroh_access {
        use super::*;
        use ring::signature::{Ed25519KeyPair, KeyPair};

        pub(super) struct IrohKey(Ed25519KeyPair);

        impl IrohKey {
            pub(super) fn new(seed: u8) -> Self {
                Self(Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap())
            }
            pub(super) fn id(&self) -> String {
                self.0.public_key().as_ref().iter().map(|b| format!("{b:02x}")).collect()
            }
            pub(super) fn binding_for(&self, device_id: &str) -> Vec<u8> {
                let proof = self.0.sign(&[b"alondra-iroh-node-v1\0", device_id.as_bytes()].concat());
                serde_json::to_vec(&json!({"iroh_node_id": self.id(),
                    "proof": URL_SAFE_NO_PAD.encode(proof.as_ref())})).unwrap()
            }
        }

        struct Fixture {
            url: String,
            server: Arc<Server>,
            task: tokio::task::JoinHandle<()>,
            mac_key: SigningKey,
            phone_key: SigningKey,
            http: reqwest::Client,
            nonce: std::sync::atomic::AtomicU8,
        }

        impl Fixture {
            async fn new() -> Self {
                Self::with_token(Some(TEST_IROH_ACCESS_TOKEN.into())).await
            }

            async fn with_token(token: Option<String>) -> Self {
                let mac_key = SigningKey::from_slice(&[111u8; 32]).unwrap();
                let phone_key = SigningKey::from_slice(&[112u8; 32]).unwrap();
                let (url, server, task) = test_server_with_iroh_access(&mac_key, None, None, token).await;
                let fixture = Self { url, server, task, mac_key, phone_key,
                    http: reqwest::Client::new(), nonce: Default::default() };
                let body = serde_json::to_vec(&PhoneDevice { device_id: "phone-1".into(),
                    owner_sub: "owner".into(), signing_key: key_bytes(&fixture.phone_key) }).unwrap();
                assert_eq!(fixture.as_mac("POST", "/v1/devices", body).await, StatusCode::CREATED);
                fixture
            }

            async fn signed(&self, key: &SigningKey, id: &str, owner: Option<&str>, method: &str,
                path: &str, body: Vec<u8>) -> StatusCode {
                let nonce = self.nonce.fetch_add(1, Ordering::Relaxed) + 1;
                let mut builder = self.http.request(reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
                    format!("{}{path}", self.url));
                if let Some(owner) = owner { builder = builder.bearer_auth(jwt(owner, now_ms())); }
                sign(&self.server.auth.server_epoch(), builder.body(body.clone()), key, id, method,
                    path, &body, nonce).send().await.unwrap().status()
            }

            async fn as_mac(&self, method: &str, path: &str, body: Vec<u8>) -> StatusCode {
                self.signed(&self.mac_key, "mac-1", None, method, path, body).await
            }

            async fn as_phone(&self, method: &str, path: &str, body: Vec<u8>) -> StatusCode {
                self.signed(&self.phone_key, "phone-1", Some("owner"), method, path, body).await
            }

            async fn access_with(&self, token: Option<&str>, node: &str) -> (StatusCode, String) {
                let mut builder = self.http.post(format!("{}/v1/iroh/access", self.url))
                    .header("X-Iroh-NodeId", node);
                if let Some(token) = token { builder = builder.bearer_auth(token); }
                let response = builder.send().await.unwrap();
                (response.status(), response.text().await.unwrap())
            }

            fn assert_reloads(&self) {
                let restarted = Auth::test(self.server.auth.config.clone(), include_bytes!("../tests/fixtures/jwks.json"));
                assert!(restarted.is_ok(), "revoked bindings must not survive on disk: {:?}", restarted.err());
            }

            async fn access(&self, node: &IrohKey) -> (StatusCode, String) {
                self.access_with(Some(TEST_IROH_ACCESS_TOKEN), &node.id()).await
            }
        }

        fn allowed() -> (StatusCode, String) { (StatusCode::OK, "true".into()) }
        fn denied() -> (StatusCode, String) { (StatusCode::FORBIDDEN, "false".into()) }

        #[tokio::test]
        async fn bound_mac_and_phone_endpoints_are_relayed_and_unknown_ones_are_not() {
            let f = Fixture::new().await;
            let (mac_node, phone_node, stranger) = (IrohKey::new(1), IrohKey::new(2), IrohKey::new(3));
            assert_eq!(f.access(&mac_node).await, denied());
            assert_eq!(f.as_mac("PUT", "/v1/iroh-node", mac_node.binding_for("mac-1")).await, StatusCode::NO_CONTENT);
            assert_eq!(f.as_phone("PUT", "/v1/iroh-node", phone_node.binding_for("phone-1")).await, StatusCode::NO_CONTENT);
            assert_eq!(f.access(&mac_node).await, allowed());
            assert_eq!(f.access(&phone_node).await, allowed());
            assert_eq!(f.access(&stranger).await, denied());
            assert_eq!(f.access_with(Some(TEST_IROH_ACCESS_TOKEN), "not-an-endpoint-id").await, denied());
            f.task.abort();
        }

        #[tokio::test]
        async fn the_mac_directory_names_a_bound_mac_endpoint_only_to_its_owner() {
            let f = Fixture::new().await;
            let (mac_node, phone_node) = (IrohKey::new(6), IrohKey::new(7));
            let directory = |owner: &'static str| {
                let request = f.http.get(format!("{}/v1/macs", f.url)).bearer_auth(jwt(owner, now_ms()));
                async move { request.send().await.unwrap().json::<Value>().await.unwrap() }
            };
            assert_eq!(directory("owner").await, json!({"macs":[{"mac_id":"mac-1"}]}), "nothing bound yet");
            assert_eq!(f.as_mac("PUT", "/v1/iroh-node", mac_node.binding_for("mac-1")).await, StatusCode::NO_CONTENT);
            assert_eq!(f.as_phone("PUT", "/v1/iroh-node", phone_node.binding_for("phone-1")).await, StatusCode::NO_CONTENT);
            assert_eq!(directory("owner").await, json!({"macs":[{"mac_id":"mac-1","iroh_endpoint":mac_node.id()}]}),
                "the Mac's endpoint, never its devices'");
            assert_eq!(directory("other").await, json!({"macs":[]}));
            assert_eq!(f.as_mac("DELETE", "/v1/iroh-node", Vec::new()).await, StatusCode::NO_CONTENT);
            assert_eq!(directory("owner").await, json!({"macs":[{"mac_id":"mac-1"}]}), "an unbound endpoint is gone");
            f.task.abort();
        }

        #[tokio::test]
        async fn the_relay_must_present_the_access_token() {
            let f = Fixture::new().await;
            let node = IrohKey::new(4);
            assert_eq!(f.as_mac("PUT", "/v1/iroh-node", node.binding_for("mac-1")).await, StatusCode::NO_CONTENT);
            assert_eq!(f.access_with(None, &node.id()).await, (StatusCode::UNAUTHORIZED, "false".into()));
            assert_eq!(f.access_with(Some("test-iroh-access-token-0123456789abcdeX"), &node.id()).await,
                (StatusCode::UNAUTHORIZED, "false".into()));
            assert_eq!(f.access(&node).await, allowed());
            f.task.abort();
        }

        #[tokio::test]
        async fn bootstrap_announces_the_iroh_relay_only_when_one_is_configured() {
            for (token, announced) in [(Some(TEST_IROH_ACCESS_TOKEN.to_owned()), Some(TEST_IROH_RELAY)), (None, None)] {
                let f = Fixture::with_token(token).await;
                let boot: Value = f.http.get(format!("{}/v1/bootstrap", f.url)).send().await.unwrap().json().await.unwrap();
                assert_eq!(boot["iroh_relay"].as_str(), announced, "{boot}");
                assert!(boot["epoch"].is_string());
                f.task.abort();
            }
        }

        #[test]
        fn bootstrap_announces_the_report_repo_only_when_one_is_configured() {
            assert_eq!(bootstrap_body("e", None, None, None), json!({"protocol": 1, "epoch": "e"}));
            assert_eq!(bootstrap_body("e", None, Some("owner/reports"), None)["report_repo"], "owner/reports");
            assert_eq!(report_repo(None).unwrap(), None);
            assert_eq!(report_repo(Some(" ".into())).unwrap(), None);
            assert_eq!(report_repo(Some("bitomule/alondra-issues".into())).unwrap().as_deref(), Some("bitomule/alondra-issues"));
            for bad in ["bitomule", "a/b/c", "/repo", "owner/", "https://github.com/a/b", "a b/c"] {
                assert!(report_repo(Some(bad.into())).is_err(), "{bad}");
            }
        }

        #[test]
        fn bootstrap_announces_the_sign_in_service_only_when_both_clients_are_configured() {
            let issuer = "https://id.example/realms/alondra";
            assert_eq!(identity(issuer, None, None).unwrap(), None);
            assert_eq!(identity(issuer, Some(" ".into()), None).unwrap(), None);
            for (apple, cli) in [(Some("app"), None), (None, Some("cli")), (Some("a b"), Some("cli"))] {
                assert!(identity(issuer, apple.map(Into::into), cli.map(Into::into)).is_err(), "{apple:?} {cli:?}");
            }
            let announced = identity(issuer, Some("app".into()), Some("cli".into())).unwrap();
            assert_eq!(bootstrap_body("e", None, None, announced.as_ref())["identity"],
                json!({"issuer": issuer, "apple_client_id": "app", "cli_client_id": "cli"}));
        }

        #[tokio::test]
        async fn without_a_configured_token_the_access_route_does_not_exist() {
            let f = Fixture::with_token(None).await;
            let node = IrohKey::new(5);
            assert_eq!(f.as_mac("PUT", "/v1/iroh-node", node.binding_for("mac-1")).await, StatusCode::NO_CONTENT);
            assert_eq!(f.access(&node).await.0, StatusCode::NOT_FOUND);
            f.task.abort();
        }

        #[tokio::test]
        async fn revoking_the_phone_ends_its_relay_access() {
            let f = Fixture::new().await;
            let (mac_node, phone_node) = (IrohKey::new(6), IrohKey::new(7));
            f.as_mac("PUT", "/v1/iroh-node", mac_node.binding_for("mac-1")).await;
            f.as_phone("PUT", "/v1/iroh-node", phone_node.binding_for("phone-1")).await;
            assert_eq!(f.as_mac("DELETE", "/v1/devices/phone-1", vec![]).await, StatusCode::NO_CONTENT);
            assert_eq!(f.access(&phone_node).await, denied());
            assert_eq!(f.access(&mac_node).await, allowed());
            f.assert_reloads();
            f.task.abort();
        }

        #[tokio::test]
        async fn revoking_the_mac_ends_relay_access_for_it_and_its_phones() {
            let f = Fixture::new().await;
            let (mac_node, phone_node) = (IrohKey::new(8), IrohKey::new(9));
            f.as_mac("PUT", "/v1/iroh-node", mac_node.binding_for("mac-1")).await;
            f.as_phone("PUT", "/v1/iroh-node", phone_node.binding_for("phone-1")).await;
            assert_eq!(f.as_mac("DELETE", "/v1/macs/mac-1", vec![]).await, StatusCode::NO_CONTENT);
            assert_eq!(f.access(&mac_node).await, denied());
            assert_eq!(f.access(&phone_node).await, denied());
            f.assert_reloads();
            f.task.abort();
        }

        #[tokio::test]
        async fn revocations_from_the_account_end_relay_access() {
            let f = Fixture::new().await;
            let (mac_node, phone_node) = (IrohKey::new(10), IrohKey::new(11));
            f.as_mac("PUT", "/v1/iroh-node", mac_node.binding_for("mac-1")).await;
            f.as_phone("PUT", "/v1/iroh-node", phone_node.binding_for("phone-1")).await;
            f.server.auth.reconcile_device_revocation("owner", "client", "phone-1", "mac-1").unwrap();
            assert_eq!(f.access(&phone_node).await, denied());
            assert_eq!(f.access(&mac_node).await, allowed());
            f.server.auth.revoke_owner("owner").unwrap();
            assert_eq!(f.access(&mac_node).await, denied());
            f.assert_reloads();
            f.task.abort();
        }

        #[tokio::test]
        async fn revoking_a_mac_from_the_account_ends_relay_access() {
            let f = Fixture::new().await;
            let (mac_node, phone_node) = (IrohKey::new(12), IrohKey::new(13));
            f.as_mac("PUT", "/v1/iroh-node", mac_node.binding_for("mac-1")).await;
            f.as_phone("PUT", "/v1/iroh-node", phone_node.binding_for("phone-1")).await;
            f.server.auth.reconcile_device_revocation("owner", "mac", "mac-1", "mac-1").unwrap();
            assert_eq!(f.access(&mac_node).await, denied());
            assert_eq!(f.access(&phone_node).await, denied());
            f.assert_reloads();
            f.task.abort();
        }

        #[tokio::test]
        async fn a_new_endpoint_replaces_the_old_one_and_unbinding_removes_it() {
            let f = Fixture::new().await;
            let (old, new) = (IrohKey::new(14), IrohKey::new(15));
            f.as_phone("PUT", "/v1/iroh-node", old.binding_for("phone-1")).await;
            assert_eq!(f.as_phone("PUT", "/v1/iroh-node", new.binding_for("phone-1")).await, StatusCode::NO_CONTENT);
            assert_eq!(f.access(&old).await, denied());
            assert_eq!(f.access(&new).await, allowed());
            assert_eq!(f.as_phone("DELETE", "/v1/iroh-node", vec![]).await, StatusCode::NO_CONTENT);
            assert_eq!(f.access(&new).await, denied());
            f.task.abort();
        }

        #[tokio::test]
        async fn binding_needs_the_endpoint_key_and_cannot_take_another_devices_endpoint() {
            let f = Fixture::new().await;
            let node = IrohKey::new(16);
            let proof_for_other_device = node.binding_for("mac-1");
            assert_eq!(f.as_phone("PUT", "/v1/iroh-node", proof_for_other_device).await, StatusCode::BAD_REQUEST);
            let mut forged: Value = serde_json::from_slice(&IrohKey::new(17).binding_for("phone-1")).unwrap();
            forged["iroh_node_id"] = json!(node.id());
            assert_eq!(f.as_phone("PUT", "/v1/iroh-node", serde_json::to_vec(&forged).unwrap()).await,
                StatusCode::BAD_REQUEST);
            assert_eq!(f.access(&node).await, denied());
            assert_eq!(f.as_mac("PUT", "/v1/iroh-node", node.binding_for("mac-1")).await, StatusCode::NO_CONTENT);
            assert_eq!(f.as_phone("PUT", "/v1/iroh-node", node.binding_for("phone-1")).await, StatusCode::CONFLICT);
            let unsigned = node.binding_for("phone-1");
            let response = f.http.put(format!("{}/v1/iroh-node", f.url)).bearer_auth(jwt("owner", now_ms()))
                .header("x-device-id", "phone-1").body(unsigned).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            f.task.abort();
        }

        impl IrohKey {
            fn bytes(&self) -> [u8; 32] {
                self.0.public_key().as_ref().try_into().unwrap()
            }
        }

        fn vouched(device_key: &SigningKey, device_id: &str, node: &IrohKey, mac_node: &IrohKey) -> Vec<u8> {
            let signature = alondra_e2e::sign_iroh_hello(device_key, "mac-1", device_id, &node.bytes(), &mac_node.bytes()).unwrap();
            serde_json::to_vec(&json!({"iroh_node_id": node.id(), "hello_signature": signature})).unwrap()
        }

        #[tokio::test]
        async fn the_mac_binds_a_device_endpoint_with_that_devices_hello_and_revoking_it_ends_access() {
            let f = Fixture::new().await;
            let (mac_node, terminal) = (IrohKey::new(30), IrohKey::new(31));
            f.as_mac("PUT", "/v1/iroh-node", mac_node.binding_for("mac-1")).await;
            let path = "/v1/devices/phone-1/iroh-node";
            assert_eq!(f.as_mac("PUT", path, vouched(&f.phone_key, "phone-1", &terminal, &mac_node)).await,
                StatusCode::NO_CONTENT);
            assert_eq!(f.access(&terminal).await, allowed());
            assert_eq!(f.as_mac("DELETE", "/v1/devices/phone-1", vec![]).await, StatusCode::NO_CONTENT);
            assert_eq!(f.access(&terminal).await, denied());
            assert_eq!(f.as_mac("PUT", path, vouched(&f.phone_key, "phone-1", &terminal, &mac_node)).await,
                StatusCode::NOT_FOUND);
            f.assert_reloads();
            f.task.abort();
        }

        #[tokio::test]
        async fn the_mac_cannot_bind_an_endpoint_the_device_never_said_hello_from() {
            let f = Fixture::new().await;
            let (mac_node, terminal, other) = (IrohKey::new(32), IrohKey::new(33), IrohKey::new(34));
            let path = "/v1/devices/phone-1/iroh-node";
            assert_eq!(f.as_mac("PUT", path, vouched(&f.phone_key, "phone-1", &terminal, &mac_node)).await,
                StatusCode::BAD_REQUEST, "without the Mac's own endpoint bound there is nothing to check the hello against");
            f.as_mac("PUT", "/v1/iroh-node", mac_node.binding_for("mac-1")).await;
            let mut for_other: Value = serde_json::from_slice(&vouched(&f.phone_key, "phone-1", &terminal, &mac_node)).unwrap();
            for_other["iroh_node_id"] = json!(other.id());
            assert_eq!(f.as_mac("PUT", path, serde_json::to_vec(&for_other).unwrap()).await, StatusCode::BAD_REQUEST);
            let stranger_key = SigningKey::from_slice(&[113u8; 32]).unwrap();
            assert_eq!(f.as_mac("PUT", path, vouched(&stranger_key, "phone-1", &terminal, &mac_node)).await,
                StatusCode::BAD_REQUEST);
            assert_eq!(f.as_mac("PUT", path, vouched(&f.phone_key, "phone-1", &mac_node, &mac_node)).await,
                StatusCode::CONFLICT);
            assert_eq!(f.as_mac("PUT", "/v1/devices/phone-9/iroh-node", vouched(&f.phone_key, "phone-9", &terminal, &mac_node)).await,
                StatusCode::NOT_FOUND);
            assert_eq!(f.as_phone("PUT", path, vouched(&f.phone_key, "phone-1", &terminal, &mac_node)).await,
                StatusCode::UNAUTHORIZED);
            assert_eq!(f.access(&terminal).await, denied());
            f.task.abort();
        }

        #[tokio::test]
        async fn bindings_survive_a_restart_and_older_registries_still_load() {
            let f = Fixture::new().await;
            let node = IrohKey::new(18);
            f.as_phone("PUT", "/v1/iroh-node", node.binding_for("phone-1")).await;
            let config = f.server.auth.config.clone();
            f.task.abort();
            let jwks = include_bytes!("../tests/fixtures/jwks.json");
            let restarted = Auth::test(config.clone(), jwks).unwrap();
            assert_eq!(restarted.iroh_node_holder(&node.id()).unwrap(), Some(auth::IrohNodeHolder {
                owner_sub: "owner".into(), mac_id: "mac-1".into(), client_id: Some("phone-1".into()) }));
            let mut stored: Value = serde_json::from_slice(&std::fs::read(&config.device_registry_path).unwrap()).unwrap();
            stored.as_object_mut().unwrap().remove("iroh_nodes");
            std::fs::write(&config.device_registry_path, serde_json::to_vec(&stored).unwrap()).unwrap();
            let older = Auth::test(config, jwks).unwrap();
            assert_eq!(older.iroh_node_holder(&node.id()).unwrap(), None);
            assert!(older.is_phone_registered("phone-1"));
        }
    }

    fn signed_headers(epoch: &[u8; 16], key: &SigningKey, id: &str, path: &str, timestamp: u64, nonce: [u8; 16]) -> HeaderMap {
        let signature = sign_http_challenge(key, id, "GET", path, epoch, &[], timestamp, &nonce).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-device-id", id.parse().unwrap());
        headers.insert("x-relay-epoch", URL_SAFE_NO_PAD.encode(epoch).parse().unwrap());
        headers.insert("x-device-timestamp-ms", timestamp.to_string().parse().unwrap());
        headers.insert("x-device-nonce", URL_SAFE_NO_PAD.encode(nonce).parse().unwrap());
        headers.insert("x-device-signature", signature.parse().unwrap());
        headers.insert("cf-ray", "8c1f2e3d4c5b6a79-MAD".parse().unwrap());
        headers
    }

    async fn mac_rejection(server: &Server, headers: &HeaderMap, path: &str, at_ms: u64) -> String {
        server.auth.set_verification_time(at_ms);
        let failure = server.auth.authenticate_mac(headers, &Method::GET, &path.parse().unwrap(), &[], at_ms).await
            .expect_err("the relay must refuse this request");
        let rejection = failure.downcast_ref::<auth::DeviceRejection>().expect("a refused request names its reason").to_string();
        assert_eq!(authentication_error(failure).0, StatusCode::UNAUTHORIZED);
        rejection
    }

    #[tokio::test]
    async fn every_refused_mac_request_names_its_cause_without_secrets() {
        let mac_key = SigningKey::from_slice(&[121; 32]).unwrap();
        let (_url, server, serving) = test_server(&mac_key).await;
        let epoch = server.auth.server_epoch();
        let path = "/v1/pairings/probe?wait_s=25";
        let start = now_ms();

        let first = signed_headers(&epoch, &mac_key, "mac-1", path, start, [1; 16]);
        server.auth.set_verification_time(start);
        server.auth.authenticate_mac(&first, &Method::GET, &path.parse().unwrap(), &[], start).await.unwrap();

        let mut resent = first.clone();
        resent.insert("cf-ray", "9d2a3b4c5d6e7f80-MAD".parse().unwrap());
        let replay = mac_rejection(&server, &resent, path, start + 25_000).await;
        assert!(replay.contains("reason=replay"), "{replay}");
        assert!(replay.contains("first_accepted_ms_ago=25000"), "{replay}");
        assert!(replay.contains("device=mac-1 GET /v1/pairings/probe "), "the query is left out: {replay}");
        assert!(replay.contains("cf_ray=9d2a3b4c5d6e7f80-MAD first_cf_ray=8c1f2e3d4c5b6a79-MAD"),
            "a replay names who sent each copy: {replay}");
        for secret in [first["x-device-signature"].to_str().unwrap(), first["x-device-nonce"].to_str().unwrap()] {
            assert!(!replay.contains(secret), "the log line never carries signatures or nonces: {replay}");
        }

        let rollback = mac_rejection(&server, &signed_headers(&epoch, &mac_key, "mac-1", path, start + 24_995, [2; 16]),
            path, start + 24_995).await;
        assert!(rollback.contains("reason=clock_rollback") && rollback.contains("clock_back_ms=5"), "{rollback}");

        let skewed = mac_rejection(&server, &signed_headers(&epoch, &mac_key, "mac-1", path, start - 96_001, [3; 16]),
            path, start + 25_000).await;
        assert!(skewed.contains("reason=clock_skew") && skewed.contains("skew_ms=-121001"), "{skewed}");

        let stale = signed_headers(&[9; 16], &mac_key, "mac-1", path, start + 25_000, [4; 16]);
        let stale_epoch = mac_rejection(&server, &stale, path, start + 25_000).await;
        assert!(stale_epoch.contains("reason=epoch_mismatch"), "{stale_epoch}");
        let tag = |epoch: &[u8; 16]| -> String {
            use sha2::Digest;
            sha2::Sha256::digest(URL_SAFE_NO_PAD.encode(epoch))[..4].iter().map(|b| format!("{b:02x}")).collect()
        };
        assert!(stale_epoch.contains(&format!("signed_epoch_tag={} current_epoch_tag={}", tag(&[9; 16]), tag(&epoch))),
            "a refusal tells an older relay's epoch from a copy of one request: {stale_epoch}");
        for value in [URL_SAFE_NO_PAD.encode([9; 16]), URL_SAFE_NO_PAD.encode(epoch)] {
            assert!(!stale_epoch.contains(&value), "the log line never carries an epoch: {stale_epoch}");
        }
        let failure = server.auth.authenticate_mac(&stale, &Method::GET, &path.parse().unwrap(), &[], start + 25_000).await.unwrap_err();
        let (status, Json(body)) = authentication_error(failure);
        assert_eq!(status, StatusCode::UNAUTHORIZED, "old clients keep bootstrapping again on a 401");
        assert_eq!(body, json!({"error":"relay_epoch_changed","epoch":URL_SAFE_NO_PAD.encode(epoch)}),
            "a stale epoch is named with the current one so clients do not renew their credentials");

        let other_key = SigningKey::from_slice(&[122; 32]).unwrap();
        let forged = mac_rejection(&server, &signed_headers(&epoch, &other_key, "mac-1", path, start + 25_000, [5; 16]),
            path, start + 25_000).await;
        assert!(forged.contains("reason=bad_signature"), "{forged}");

        let unknown = mac_rejection(&server, &signed_headers(&epoch, &mac_key, "mac-9", path, start + 25_000, [6; 16]),
            path, start + 25_000).await;
        assert!(unknown.contains("reason=unknown_device"), "{unknown}");
        serving.abort();
    }
}

fn enrollment_error(cause:anyhow::Error, conflict_code:&str)->(StatusCode,Json<Value>) {
    match cause.downcast_ref::<auth::EnrollmentError>() {
        Some(auth::EnrollmentError::Conflict)=>error(StatusCode::CONFLICT,conflict_code),
        Some(auth::EnrollmentError::InvalidRegistration)=>error(StatusCode::BAD_REQUEST,"invalid_registration"),
        None=>error(StatusCode::SERVICE_UNAVAILABLE,"registry_unavailable"),
    }
}

fn authentication_error(error_value:anyhow::Error)->(StatusCode,Json<Value>) {
    if error_value.is::<auth::RegistryUnavailable>() {
        return error(StatusCode::SERVICE_UNAVAILABLE,"registry_unavailable");
    }
    match error_value.downcast_ref::<auth::AccountGateError>() {
        Some(auth::AccountGateError::Unavailable)=>error(StatusCode::SERVICE_UNAVAILABLE,"account_access_unavailable"),
        Some(auth::AccountGateError::Denied)=>error(StatusCode::FORBIDDEN,"account_access_denied"),
        None=>{
            if let Some(rejection)=error_value.downcast_ref::<auth::DeviceRejection>() {
                eprintln!("alondra-relay: {rejection}");
                // Still a 401, which clients already answer by bootstrapping again; the body
                // tells them it is the epoch and not their credentials.
                if let Some(epoch)=&rejection.current_epoch {
                    return (StatusCode::UNAUTHORIZED,Json(json!({"error":"relay_epoch_changed","epoch":epoch})));
                }
            } else {
                eprintln!("alondra-relay: request refused: {error_value:#}");
            }
            unauthorized()
        }
    }
}
