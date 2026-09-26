mod auth;
mod management;
mod pairing;
mod push;
mod state;
mod web_session;
mod web_assets;

use alondra_e2e::EnvelopeV1;
use auth::{Auth, Config, MacRegistration, PhoneDevice, PushRegistration};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, Method, Request, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use state::{Queue, QueueError};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{RwLock, Semaphore};

struct Server {
    auth: Auth,
    management_keycloak: Option<management::keycloak::Keycloak>,
    web_session: Option<Arc<web_session::WebSession>>,
    web_assets: Option<web_assets::WebAssets>,
    apns: Option<push::Apns>,
    queue: Queue,
    pairing: pairing::Mailbox,
    work_waiters: Arc<Semaphore>,
    change_waiters: Arc<Semaphore>,
    requests: Arc<Semaphore>,
    device_gate: Arc<RwLock<()>>,
    epoch: String,
}

fn router(server: Arc<Server>) -> Router {
    Router::new()
        .merge(management::http::routes())
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
        .route("/v1/push/register", post(register_push))
        .route("/v1/mac/push", post(send_push))
        .route("/v1/requests", post(submit))
        .route("/v1/requests/{id}", get(request_status))
        .route("/v1/changes", get(changes))
        .route("/v1/work", get(work))
        .route("/v1/work/{id}/receipt", post(receipt))
        .route("/v1/work/{id}/response", post(respond))
        .fallback(web_assets::serve)
        .layer(DefaultBodyLimit::max(state::MAX_RESPONSE_BYTES))
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
    if server.auth.jwks_ready() {
        (StatusCode::OK, Json(json!({"ready":true})))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ready":false})),
        )
    }
}
async fn bootstrap(State(server): State<Arc<Server>>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({"protocol":1,"epoch":server.epoch})),
    )
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
    let created = server
        .auth
        .enroll_mac(mac)
        .map_err(|_| error(StatusCode::CONFLICT, "mac_enrollment_failed"))?;
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

/// OIDC authenticates ownership for discovery, never authorizes content access.
async fn list_macs(State(server): State<Arc<Server>>, headers: HeaderMap) -> ApiResult {
    let owner = server.auth.account_owner(&headers, now_ms()).await.map_err(authentication_error)?;
    let _gate = server.device_gate.read().await;
    let ids = server.auth.mac_ids_for_owner(&owner)
        .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "mac_directory_unavailable"))?;
    let macs: Vec<Value> = ids.into_iter().map(|id| json!({"mac_id":id})).collect();
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

type ApiResult = Result<Response, (StatusCode, Json<Value>)>;
fn error(status: StatusCode, code: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error":code})))
}
fn unauthorized() -> (StatusCode, Json<Value>) {
    error(StatusCode::UNAUTHORIZED, "unauthorized")
}
fn queued_error(e: QueueError) -> (StatusCode, Json<Value>) {
    match e {
        QueueError::Full => error(StatusCode::TOO_MANY_REQUESTS, "queue_full"),
        QueueError::Conflict => error(StatusCode::CONFLICT, "request_id_conflict"),
        QueueError::NotFound | QueueError::WrongDevice => error(StatusCode::NOT_FOUND, "not_found"),
        QueueError::NotReceived => error(StatusCode::CONFLICT, "mac_receipt_required"),
        QueueError::Expired => error(StatusCode::GONE, "outcome_unknown"),
        QueueError::ResultEvicted => error(StatusCode::GONE, "result_evicted"),
    }
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
        .map_err(|_| error(StatusCode::CONFLICT, "device_enrollment_failed"))?;
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
    server.queue.purge_phone(&id);
    Ok(StatusCode::NO_CONTENT.into_response())
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
        .register_push(&who.device_id, registration)
        .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "push_registration_failed"))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PushRequest {
    device_id: String,
    envelope: EnvelopeV1,
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
    if e.kind != "push"
        || e.route != mac.device_id
        || e.sender != mac.device_id
        || e.recipient != request.device_id
        || e.expires_at_ms <= now
        || e.expires_at_ms > now.saturating_add(state::MAX_LIFETIME_MS)
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
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "push_registration_not_found"))?;
    server
        .auth
        .phone_for_mac(&request.device_id, &mac)
        .map_err(|_| error(StatusCode::NOT_FOUND, "push_registration_not_found"))?;
    let payload_bytes = serde_json::to_vec(&push::payload(e, &registration.push_binding))
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_push"))?;
    if payload_bytes.len() > 4096 {
        return Err(error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "apns_payload_too_large",
        ));
    }
    server.auth.require_live_route(&mac.owner_sub,&mac.device_id,Some(&request.device_id)).await.map_err(authentication_error)?;
    apns.send(&registration, e)
        .await
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "apns_delivery_failed"))?;
    Ok(StatusCode::ACCEPTED.into_response())
}

fn parse_envelope(
    body: &[u8],
    max: usize,
) -> Result<(EnvelopeV1, Value), (StatusCode, Json<Value>)> {
    if body.len() > max {
        return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "envelope_too_large"));
    }
    let fields: EnvelopeV1 = serde_json::from_slice(body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_envelope"))?;
    if fields.request_id.is_empty()
        || fields.request_id.len() > 128
        || !fields
            .request_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_request_id"));
    }
    let v = serde_json::to_value(&fields)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_envelope"))?;
    Ok((fields, v))
}

async fn submit(
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
    let (fields, v) = parse_envelope(&body, state::MAX_ENVELOPE_BYTES)?;
    if fields.kind != "request"
        || fields.route != fields.recipient
        || fields.sender != who.device_id
    {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_route"));
    }
    server
        .auth
        .mac_for_owner(&fields.route, &who.owner_sub)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_route"))?;
    if server
        .auth
        .paired_mac_for_phone(&who.device_id, &who.owner_sub)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_route"))?
        != fields.route
    {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_route"));
    }
    let now = now_ms();
    if fields.expires_at_ms <= now
        || fields.expires_at_ms > now.saturating_add(state::MAX_LIFETIME_MS)
    {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_expiry"));
    }
    // The envelope signature is verified by the E2E crate before any work is admitted.
    let sender_key = server
        .auth
        .phone_key(&who.device_id)
        .map_err(authentication_error)?;
    fields
        .verify_signature(
            &sender_key,
            &who.device_id,
            &fields.route,
            &fields.route,
            now,
        )
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_envelope_signature"))?;
    let digest: [u8; 32] = Sha256::digest(&body).into();
    let result = server
        .queue
        .submit(
            &who.owner_sub,
            &who.device_id,
            &fields.route,
            &fields.request_id,
            digest,
            v,
            fields.expires_at_ms,
            now,
        )
        .map_err(queued_error)?;
    let code = match result.status {
        state::JobStatus::Expired => StatusCode::GONE,
        state::JobStatus::Completed => StatusCode::OK,
        _ => StatusCode::ACCEPTED,
    };
    Ok((code, Json(json!({"request_id":result.request_id,"status":result.status,"revision":result.revision}))).into_response())
}

async fn request_status(
    State(server): State<Arc<Server>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
) -> ApiResult {
    let _gate = server.device_gate.read().await;
    let who = server
        .auth
        .authenticate_phone(&headers, &method, &uri, &[], now_ms())
        .await
        .map_err(authentication_error)?;
    let result = server
        .queue
        .get(&who.owner_sub, &who.device_id, &id, now_ms())
        .map_err(queued_error)?;
    Ok((StatusCode::OK, Json(json!(result))).into_response())
}

#[derive(Deserialize)]
struct ChangesQuery {
    #[serde(default)]
    since: u64,
    epoch: Option<String>,
    #[serde(default = "default_wait")]
    wait_s: u64,
}
fn default_wait() -> u64 {
    25
}

async fn changes(
    State(server): State<Arc<Server>>,
    Query(query): Query<ChangesQuery>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
) -> ApiResult {
    // The read lock covers this bounded poll so revocation cannot race a ciphertext read.
    let _gate = server.device_gate.read().await;
    let who = server
        .auth
        .authenticate_phone(&headers, &method, &uri, &[], now_ms())
        .await
        .map_err(authentication_error)?;
    if query.epoch.as_deref().is_some_and(|e| e != server.epoch) {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({"error":"relay_epoch_changed","epoch":server.epoch})),
        ));
    }
    let wait_s = query.wait_s.min(25);
    let permit = server
        .change_waiters
        .clone()
        .try_acquire_owned()
        .map_err(|_| error(StatusCode::TOO_MANY_REQUESTS, "too_many_polls"))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait_s);
    loop {
        let notified = server.queue.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        server.auth.require_live_phone(&who).await.map_err(authentication_error)?;
        let (revision, events) =
            server
                .queue
                .changes(&who.owner_sub, &who.device_id, query.since, now_ms());
        if query.since > revision {
            drop(permit);
            return Err((
                StatusCode::CONFLICT,
                Json(json!({"error":"stale_cursor","epoch":server.epoch,"revision":revision})),
            ));
        }
        if !events.is_empty() || wait_s == 0 || tokio::time::Instant::now() >= deadline
            || server.queue.compacted_since(&who.owner_sub, &who.device_id, query.since) {
            drop(permit);
            return Ok((
                StatusCode::OK,
                Json(json!({"epoch":server.epoch,"revision":revision,"changes":events})),
            )
                .into_response());
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            server.auth.require_live_phone(&who).await.map_err(authentication_error)?;
            drop(permit);
            return Ok((
                StatusCode::OK,
                Json(json!({"epoch":server.epoch,"revision":revision,"changes":[]})),
            )
                .into_response());
        }
    }
}

#[derive(Deserialize)]
struct WorkQuery {
    #[serde(default = "default_wait")]
    wait_s: u64,
}

async fn work(
    State(server): State<Arc<Server>>,
    Query(query): Query<WorkQuery>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
) -> ApiResult {
    let _gate = server.device_gate.read().await;
    let who = server
        .auth
        .authenticate_mac(&headers, &method, &uri, &[], now_ms()).await
        .map_err(authentication_error)?;
    let wait_s = query.wait_s.min(25);
    let permit = server
        .work_waiters
        .clone()
        .try_acquire_owned()
        .map_err(|_| error(StatusCode::TOO_MANY_REQUESTS, "too_many_polls"))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait_s);
    loop {
        let notified = server.queue.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        server.auth.require_live_route(&who.owner_sub,&who.device_id,None).await.map_err(authentication_error)?;
        if let Some(item) = server.queue.next_work(&who.device_id, now_ms()) {
            drop(permit);
            return Ok((
                StatusCode::OK,
                Json(json!({"request_id":item.request_id,"envelope":item.envelope})),
            )
                .into_response());
        }
        if wait_s == 0 || tokio::time::Instant::now() >= deadline {
            drop(permit);
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            server.auth.require_live_route(&who.owner_sub,&who.device_id,None).await.map_err(authentication_error)?;
            drop(permit);
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
    }
}

async fn receipt(
    State(server): State<Arc<Server>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let _gate = server.device_gate.read().await;
    let who = server
        .auth
        .authenticate_mac(&headers, &method, &uri, &body, now_ms()).await
        .map_err(authentication_error)?;
    if !body.is_empty() {
        return Err(error(StatusCode::BAD_REQUEST, "receipt_body_must_be_empty"));
    }
    let status = server
        .queue
        .mark_received(&who.device_id, &id, now_ms())
        .map_err(queued_error)?;
    Ok((
        StatusCode::OK,
        Json(json!({"request_id":status.request_id,"status":status.status})),
    )
        .into_response())
}

async fn respond(
    State(server): State<Arc<Server>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let _gate = server.device_gate.read().await;
    let who = server
        .auth
        .authenticate_mac(&headers, &method, &uri, &body, now_ms()).await
        .map_err(authentication_error)?;
    let (fields, v) = parse_envelope(&body, state::MAX_RESPONSE_BYTES)?;
    let phone_id = server
        .queue
        .phone_for_mac(&who.device_id, &id, now_ms())
        .map_err(queued_error)?;
    if fields.kind != "response"
        || fields.request_id != id
        || fields.route != who.device_id
        || fields.sender != who.device_id
        || fields.recipient != phone_id
    {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_route"));
    }
    if fields.expires_at_ms > now_ms().saturating_add(state::MAX_LIFETIME_MS) {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_expiry"));
    }
    fields
        .verify_signature(
            &server
                .auth
                .mac_key(&who.device_id)
                .map_err(authentication_error)?,
            &who.device_id,
            &phone_id,
            &who.device_id,
            now_ms(),
        )
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_envelope_signature"))?;
    server.auth.require_live_route(&who.owner_sub,&who.device_id,Some(&phone_id)).await.map_err(authentication_error)?;
    let status = server
        .queue
        .respond(&who.device_id, &phone_id, &id, v, now_ms())
        .map_err(queued_error)?;
    Ok((
        StatusCode::OK,
        Json(json!({"request_id":status.request_id,"status":status.status})),
    )
        .into_response())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
            std::env::var("ALONDRA_MANAGEMENT_KC_ADMIN_REALM_URL")?.parse()?,
            std::env::var("ALONDRA_MANAGEMENT_KC_TOKEN_URL")?.parse()?,
            std::env::var("ALONDRA_MANAGEMENT_KC_CLIENT_ID")?,
            std::fs::read_to_string(std::env::var("ALONDRA_MANAGEMENT_KC_SECRET_FILE")?)?.trim().into())?;
        keycloak.verify_realm().await?;Some(keycloak)
    } else {None};
    let web_session = web_session::WebSession::from_env(&auth)?;
    let web_assets = web_assets::WebAssets::from_env(web_session.is_some())?;
    let server = Arc::new(Server {
        auth,
        web_assets,
        web_session,
        management_keycloak,
        apns,
        queue: Queue::new(),
        pairing: pairing::Mailbox::default(),
        work_waiters: Arc::new(Semaphore::new(16)),
        change_waiters: Arc::new(Semaphore::new(64)),
        requests: Arc::new(Semaphore::new(128)),
        device_gate: Arc::new(RwLock::new(())),
        epoch,
    });
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    if server.auth.management.is_some() {tokio::spawn(management::worker::run(server.clone()));}
    if let Some(web)=&server.web_session {tokio::spawn(web_session::reconcile(web.clone()));}
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
    axum::serve(listener, router(server)).await?;
    Ok(())
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use alondra_e2e::{Context, EnvelopeV1, sign_http_challenge};
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
        let registry = std::env::temp_dir().join(format!(
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
            auth,
            management_keycloak: None,
            web_session: None,
            web_assets: None,
            apns: apns_endpoint.map(push::Apns::test),
            queue: Queue::new(),
            pairing: pairing::Mailbox::default(),
            work_waiters: Arc::new(Semaphore::new(16)),
            change_waiters: Arc::new(Semaphore::new(64)),
            requests: Arc::new(Semaphore::new(128)),
            device_gate: Arc::new(RwLock::new(())),
            epoch,
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let serving = server.clone();
        let join = tokio::spawn(async move {
            axum::serve(listener, router(serving)).await.unwrap();
        });
        (url, server, join)
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
        let path="/v1/changes?since=0&wait_s=25";
        let pending=sign(&server.auth.server_epoch(),client.get(format!("{url}{path}")).bearer_auth(&auth),&phone_key,"client-first","GET",path,&[],181).send();
        let pending=tokio::spawn(pending);
        tokio::time::timeout(std::time::Duration::from_secs(3),async {while server.change_waiters.available_permits()==64 {tokio::task::yield_now().await;}}).await?;
        let response=client.delete(format!("{url}/v1/account/devices/clients/client-first")).bearer_auth(&auth).header("Idempotency-Key","device-revoke-own-00001").send().await?;
        assert_eq!(response.status(),StatusCode::ACCEPTED);
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(3),pending).await???.status(),StatusCode::FORBIDDEN);
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
        let response=client.delete(format!("{url}/v1/account/devices/macs/mac-1")).bearer_auth(&auth).header("Idempotency-Key","device-revoke-mac-00001").send().await?;
        assert_eq!(response.status(),StatusCode::ACCEPTED);
        let response=sign(&server.auth.server_epoch(),client.get(format!("{url}/v1/work?wait_s=0")),&mac_key,"mac-1","GET","/v1/work?wait_s=0",&[],183).send().await?;
        assert_eq!(response.status(),StatusCode::FORBIDDEN);
        assert!(management::worker::step(&server).await?);assert!(!server.auth.is_mac_registered("mac-1"));assert!(server.auth.is_mac_registered("mac-other"));
        let response=client.delete(format!("{url}/v1/account")).bearer_auth(&auth).header("Idempotency-Key","self-delete-confirm-0001").json(&json!({"confirm":false})).send().await?;
        assert_eq!(response.status(),StatusCode::UNPROCESSABLE_ENTITY);
        assert!(db.active_identity("https://id.example/realms/alondra","owner").await.is_ok());
        join.abort();management::tests::cleanup(db,schema).await
    }

    #[tokio::test]
    #[ignore="requires disposable local PostgreSQL"]
    async fn management_http_private_gate_blocks_unknown_disabled_and_database_outage_after_wait() -> anyhow::Result<()> {
        let (db,admin,schema)=management::tests::fixture_for_issuer("https://id.example/realms/alondra").await?;
        let invitation=db.create_invitation(admin.id,"gate@example.com",300).await?;
        let owner=db.reserve_acceptance(&invitation.token,"Owner").await?.complete("owner").await?;
        let key=SigningKey::from_slice(&[21u8;32]).unwrap();
        let (url,server,join)=test_server_with_management(&key,None,Some(db.clone())).await;
        let client=reqwest::Client::new();
        let response=client.get(format!("{url}/v1/macs")).bearer_auth(jwt("uninvited",now_ms())).send().await?;
        assert_eq!(response.status(),StatusCode::FORBIDDEN);
        let response=client.get(format!("{url}/v1/macs")).bearer_auth(jwt("owner",now_ms())).send().await?;
        assert_eq!(response.status(),StatusCode::OK);
        let path="/v1/work?wait_s=25";
        let pending=sign(&server.auth.server_epoch(),client.get(format!("{url}{path}")),&key,"mac-1","GET",path,&[],211).send();
        let pending=tokio::spawn(pending);
        tokio::time::timeout(std::time::Duration::from_secs(3),async {
            while server.work_waiters.available_permits()==16 {tokio::task::yield_now().await;}
        }).await?;
        db.disable_account(admin.id,owner.id,false).await?;
        server.queue.changed.notify_waiters();
        let response=tokio::time::timeout(std::time::Duration::from_secs(3),pending).await???;
        assert_eq!(response.status(),StatusCode::FORBIDDEN,"already authenticated long poll must revalidate after wake");
        let response=sign(&server.auth.server_epoch(),client.get(format!("{url}/v1/work?wait_s=0")),&key,"mac-1","GET","/v1/work?wait_s=0",&[],212).send().await?;
        assert_eq!(response.status(),StatusCode::FORBIDDEN,"signed Mac cannot bypass product suspension");
        db.pool.close().await;
        let response=client.get(format!("{url}/v1/macs")).bearer_auth(jwt("stable-admin-sub",now_ms())).send().await?;
        assert_eq!(response.status(),StatusCode::SERVICE_UNAVAILABLE,"database outage must not fall back to OIDC-only admission");
        join.abort();management::tests::cleanup(db,schema).await
    }

    #[tokio::test]
    async fn compacted_http_result_requires_auth_and_changes_cursor_does_not_spin() {
        let mac_key = SigningKey::from_slice(&[21u8; 32]).unwrap();
        let phone_key = SigningKey::from_slice(&[22u8; 32]).unwrap();
        let (url, server, join) = test_server(&mac_key).await;
        let epoch = server.auth.server_epoch();
        server.auth.enroll(&auth::Identity {
            device_id: "mac-1".into(), owner_sub: "owner".into(),
        }, PhoneDevice {
            device_id: "phone-1".into(), owner_sub: "owner".into(),
            signing_key: key_bytes(&phone_key),
        }).unwrap();
        let now = now_ms();
        for index in 0..state::MAX_STORED {
            let id = format!("retained-{index}");
            server.queue.submit("owner", "phone-1", "mac-1", &id, [1; 32],
                json!({"ciphertext":"opaque"}), now + 120_000, now).unwrap();
            server.queue.next_work("mac-1", now).unwrap();
            server.queue.mark_received("mac-1", &id, now).unwrap();
            server.queue.respond("mac-1", "phone-1", &id,
                json!({"ciphertext":"opaque"}), now).unwrap();
        }
        let (before, _) = server.queue.changes("owner", "phone-1", 0, now);
        // Another device causes compaction without adding a change for phone-1.
        server.queue.submit("other", "other-phone", "other-mac", "pressure", [2; 32],
            json!({"ciphertext":"opaque"}), now + 120_000, now).unwrap();
        let client = reqwest::Client::new();
        let path = "/v1/requests/retained-0";
        assert_eq!(client.get(format!("{url}{path}")).send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED);
        let response = sign(&epoch, client.get(format!("{url}{path}"))
            .bearer_auth(jwt("owner", now)), &phone_key, "phone-1", "GET", path, &[], 1)
            .send().await.unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
        assert_eq!(response.json::<Value>().await.unwrap(), json!({"error":"result_evicted"}));

        let path = format!("/v1/changes?since={before}&epoch={}&wait_s=25", server.epoch);
        let response = tokio::time::timeout(Duration::from_secs(2),
            sign(&epoch, client.get(format!("{url}{path}")).bearer_auth(jwt("owner", now)),
                &phone_key, "phone-1", "GET", &path, &[], 2).send()).await
            .expect("compaction must wake the cursor even without retained events").unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.json::<Value>().await.unwrap();
        assert_eq!(body["changes"], json!([]));
        let revision = body["revision"].as_u64().unwrap();
        assert!(revision > before);
        let path = format!("/v1/changes?since={revision}&epoch={}&wait_s=1", server.epoch);
        let started = tokio::time::Instant::now();
        let response = sign(&epoch, client.get(format!("{url}{path}"))
            .bearer_auth(jwt("owner", now)), &phone_key, "phone-1", "GET", &path, &[], 3)
            .send().await.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(900), "same cursor must long poll");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.json::<Value>().await.unwrap()["changes"], json!([]));
        let path = "/v1/devices/phone-1";
        let response = sign(&epoch, client.delete(format!("{url}{path}")), &mac_key,
            "mac-1", "DELETE", path, &[], 4).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let path = "/v1/requests/retained-0";
        let response = sign(&epoch, client.get(format!("{url}{path}"))
            .bearer_auth(jwt("owner", now)), &phone_key, "phone-1", "GET", path, &[], 5)
            .send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(server.queue.get("owner", "phone-1", "retained-0", now).unwrap_err(),
            QueueError::NotFound);
        join.abort();
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
        let response = client.get(format!("{url}/v1/work"))
            .bearer_auth(jwt("owner", now_ms())).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "OIDC alone cannot read Mac work");
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
        let envelope = EnvelopeV1::seal_random(Context { route: "mac-1", sender: "client-a",
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
    async fn encrypted_push_requires_both_identities_and_purges_registration_on_revoke() {
        use std::sync::Mutex;
        let delivered = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured = delivered.clone();
        let apns_router = Router::new().route(
            "/3/device/{token}",
            post(move |version: axum::http::Version, Json(body): Json<Value>| {
                let captured = captured.clone();
                async move {
                    assert_eq!(version, axum::http::Version::HTTP_2);
                    let reject = body["alondra"]["envelope"]["request_id"] == "push-reject";
                    captured.lock().unwrap().push(body);
                    if reject {
                        StatusCode::BAD_REQUEST
                    } else {
                        StatusCode::OK
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

        let envelope = EnvelopeV1::seal_random(
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
                let envelope = EnvelopeV1::seal_random(
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
        }

        let rejected_envelope = EnvelopeV1::seal_random(
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
        assert_eq!(denied.status(), StatusCode::NOT_FOUND);
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
        let envelope = |i: usize, m: usize, rid: &str| {
            let sealed = EnvelopeV1::seal_random(Context { route: mac_ids[m], sender: links[i].0,
                recipient: mac_ids[m], request_id: rid, kind: "request", expires_at_ms: now_ms() + 120_000 },
                br#"{"method":"GET","path":"/v1/tasks"}"#, &client_keys[i], &mac_agreements[m].public_key()).unwrap();
            serde_json::to_vec(&sealed).unwrap()
        };
        for (i, (id, m)) in links.iter().enumerate() {
            assert_eq!(request(&client_keys[i], id, Some(owners[*m]), "POST", "/v1/requests",
                envelope(i, *m, &format!("matrix-{i}"))).send().await.unwrap().status(), StatusCode::ACCEPTED);
        }
        // Possessing valid account credentials and signing keys does not authorize another Mac.
        for target in [1, 2] {
            assert_eq!(request(&client_keys[0], links[0].0, Some("owner"), "POST", "/v1/requests",
                envelope(0, target, &format!("reroute-{target}"))).send().await.unwrap().status(), StatusCode::BAD_REQUEST);
        }
        assert_eq!(request(&client_keys[0], links[0].0, Some("owner-b"), "POST", "/v1/requests",
            envelope(0, 0, "wrong-account")).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        let registration = serde_json::to_vec(&json!({"token":"ff".repeat(32),"environment":"sandbox",
            "push_binding":URL_SAFE_NO_PAD.encode([99u8;32])})).unwrap();
        assert_eq!(request(&client_keys[0], links[0].0, Some("owner-b"), "POST", "/v1/push/register", registration)
            .send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        for wrong_mac in [1, 2] {
            assert!(!request(&mac_keys[wrong_mac], mac_ids[wrong_mac], None, "DELETE",
                "/v1/devices/client-one-a1", vec![]).send().await.unwrap().status().is_success());
        }
        // Read isolation uses actual authorized HTTP polling, not registry shape alone.
        let changes_path = "/v1/changes?since=0&wait_s=0";
        for (i, (id, m)) in links.iter().enumerate() {
            let reply = request(&client_keys[i], id, Some(owners[*m]), "GET", changes_path, vec![])
                .send().await.unwrap();
            assert_eq!(reply.status(), StatusCode::OK);
            let events = reply.json::<Value>().await.unwrap();
            assert_eq!(events["changes"].as_array().unwrap().len(), 1);
            assert_eq!(events["changes"][0]["request_id"], format!("matrix-{i}"));
        }
        assert_eq!(request(&mac_keys[0], mac_ids[0], None, "DELETE", "/v1/devices/client-one-a1", vec![])
            .send().await.unwrap().status(), StatusCode::NO_CONTENT);
        assert!(server.auth.push_registration(links[0].0).is_none());
        assert_eq!(server.queue.get("owner", links[0].0, "matrix-0", now_ms()).unwrap_err(), QueueError::NotFound);
        assert_eq!(request(&client_keys[0], links[0].0, Some("owner"), "GET", changes_path, vec![])
            .send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        // A removed registration must not dispatch, even with the approving Mac's signature.
        // Nor may another Mac use a surviving link's registration, even in the same account.
        for (link, mac) in [(0, 0), (1, 0)] {
            let id = links[link].0;
            let push = EnvelopeV1::seal_random(Context { route: mac_ids[mac], sender: mac_ids[mac], recipient: id,
                request_id: &format!("denied-push-{link}"), kind: "push", expires_at_ms: now_ms() + 120_000 },
                br#"{"payload":{"aps":{"alert":"must not arrive"},"alondra":{}}}"#,
                &mac_keys[mac], &client_agreements[link].public_key()).unwrap();
            let body = serde_json::to_vec(&json!({"device_id":id,"envelope":push})).unwrap();
            assert_eq!(request(&mac_keys[mac], mac_ids[mac], None, "POST", "/v1/mac/push", body)
                .send().await.unwrap().status(), StatusCode::NOT_FOUND);
            assert!(captured.lock().unwrap().is_empty(), "denied push reached APNs");
        }
        for (i, (id, m)) in links.iter().enumerate().skip(1) {
            // Both the other link of client-one and all links of client-two remain usable.
            assert_eq!(request(&client_keys[i], id, Some(owners[*m]), "POST", "/v1/requests",
                envelope(i, *m, &format!("after-revoke-{i}"))).send().await.unwrap().status(), StatusCode::ACCEPTED);
            let registration = server.auth.push_registration(id).unwrap();
            assert_eq!(registration.push_binding, URL_SAFE_NO_PAD.encode([i as u8 + 1;32]));
            let plaintext = serde_json::to_vec(&json!({"push_binding":registration.push_binding,
                "payload":{"aps":{"alert":"private"},"alondra":{}},"with_sound":false})).unwrap();
            let push = EnvelopeV1::seal_random(Context { route: mac_ids[*m], sender: mac_ids[*m], recipient: id,
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
            let envelope: EnvelopeV1 = serde_json::from_value(body["alondra"]["envelope"].clone()).unwrap();
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
    async fn signed_opaque_http_flow_rejects_wrong_identities_and_replay() {
        let mac_sign = SigningKey::from_slice(&[1u8; 32]).unwrap();
        let phone_sign = SigningKey::from_slice(&[2u8; 32]).unwrap();
        let wrong_sign = SigningKey::from_slice(&[3u8; 32]).unwrap();
        let mac_ecdh = SecretKey::from_slice(&[4u8; 32]).unwrap();
        let phone_ecdh = SecretKey::from_slice(&[5u8; 32]).unwrap();
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
        let request = EnvelopeV1::seal_random(
            Context {
                route: "mac-1",
                sender: "phone-1",
                recipient: "mac-1",
                request_id: "request-1",
                kind: "request",
                expires_at_ms: now + 120_000,
            },
            br#"{"method":"POST","path":"/v1/requests","body":{"text":"private"}}"#,
            &phone_sign,
            &mac_ecdh.public_key(),
        )
        .unwrap();
        let body = serde_json::to_vec(&request).unwrap();
        let wrong_user = sign(
            &epoch,
            client
                .post(format!("{url}/v1/requests"))
                .bearer_auth(jwt("intruder", now))
                .body(body.clone()),
            &phone_sign,
            "phone-1",
            "POST",
            "/v1/requests",
            &body,
            2,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(wrong_user.status(), StatusCode::UNAUTHORIZED);
        let wrong_device = sign(
            &epoch,
            client
                .post(format!("{url}/v1/requests"))
                .bearer_auth(jwt("owner", now))
                .body(body.clone()),
            &wrong_sign,
            "phone-1",
            "POST",
            "/v1/requests",
            &body,
            3,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(wrong_device.status(), StatusCode::UNAUTHORIZED);
        let oversized = vec![b'x'; state::MAX_ENVELOPE_BYTES + 1];
        let too_large = sign(
            &epoch,
            client
                .post(format!("{url}/v1/requests"))
                .bearer_auth(jwt("owner", now))
                .body(oversized.clone()),
            &phone_sign,
            "phone-1",
            "POST",
            "/v1/requests",
            &oversized,
            30,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(too_large.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            server
                .queue
                .changes("owner", "phone-1", 0, now_ms())
                .1
                .len(),
            0,
            "oversized body must not dispatch partial work"
        );
        let accepted = sign(
            &epoch,
            client
                .post(format!("{url}/v1/requests"))
                .bearer_auth(jwt("owner", now))
                .body(body.clone()),
            &phone_sign,
            "phone-1",
            "POST",
            "/v1/requests",
            &body,
            4,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(accepted.status(), StatusCode::ACCEPTED);
        assert_eq!(accepted.json::<Value>().await.unwrap()["status"], "queued");
        let duplicate = sign(
            &epoch,
            client
                .post(format!("{url}/v1/requests"))
                .bearer_auth(jwt("owner", now))
                .body(body.clone()),
            &phone_sign,
            "phone-1",
            "POST",
            "/v1/requests",
            &body,
            5,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(duplicate.status(), StatusCode::ACCEPTED);

        let bad_mac = sign(
            &epoch,
            client.get(format!("{url}/v1/work?wait_s=0")),
            &wrong_sign,
            "mac-1",
            "GET",
            "/v1/work?wait_s=0",
            &[],
            6,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(bad_mac.status(), StatusCode::UNAUTHORIZED);
        let work = sign(
            &epoch,
            client.get(format!("{url}/v1/work?wait_s=0")),
            &mac_sign,
            "mac-1",
            "GET",
            "/v1/work?wait_s=0",
            &[],
            7,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(work.status(), StatusCode::OK);
        let work: Value = work.json().await.unwrap();
        let forwarded: EnvelopeV1 = serde_json::from_value(work["envelope"].clone()).unwrap();
        let plaintext = forwarded
            .open(
                phone_sign.verifying_key(),
                &mac_ecdh,
                "phone-1",
                "mac-1",
                "mac-1",
                now_ms(),
            )
            .unwrap();
        assert!(String::from_utf8(plaintext).unwrap().contains("private"));

        let result = EnvelopeV1::seal_random(
            Context {
                route: "mac-1",
                sender: "mac-1",
                recipient: "phone-1",
                request_id: "request-1",
                kind: "response",
                expires_at_ms: now_ms() + 120_000,
            },
            br#"{"ok":true,"request_id":"request-1"}"#,
            &mac_sign,
            &phone_ecdh.public_key(),
        )
        .unwrap();
        let result_body = serde_json::to_vec(&result).unwrap();
        let early = sign(
            &epoch,
            client
                .post(format!("{url}/v1/work/request-1/response"))
                .body(result_body.clone()),
            &mac_sign,
            "mac-1",
            "POST",
            "/v1/work/request-1/response",
            &result_body,
            8,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(
            early.status(),
            StatusCode::CONFLICT,
            "Mac must persist a receipt before side effects"
        );
        let receipt = sign(
            &epoch,
            client.post(format!("{url}/v1/work/request-1/receipt")),
            &mac_sign,
            "mac-1",
            "POST",
            "/v1/work/request-1/receipt",
            &[],
            9,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(receipt.status(), StatusCode::OK);
        let posted = sign(
            &epoch,
            client
                .post(format!("{url}/v1/work/request-1/response"))
                .body(result_body.clone()),
            &mac_sign,
            "mac-1",
            "POST",
            "/v1/work/request-1/response",
            &result_body,
            10,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(posted.status(), StatusCode::OK);

        let status_path = "/v1/requests/request-1";
        let status = sign(
            &epoch,
            client
                .get(format!("{url}{status_path}"))
                .bearer_auth(jwt("owner", now)),
            &phone_sign,
            "phone-1",
            "GET",
            status_path,
            &[],
            11,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        let status: Value = status.json().await.unwrap();
        assert_eq!(status["status"], "completed");
        let opaque: EnvelopeV1 = serde_json::from_value(status["response"].clone()).unwrap();
        assert_eq!(
            opaque
                .open(
                    mac_sign.verifying_key(),
                    &phone_ecdh,
                    "mac-1",
                    "phone-1",
                    "mac-1",
                    now_ms()
                )
                .unwrap(),
            br#"{"ok":true,"request_id":"request-1"}"#
        );

        let path = "/v1/changes?since=0&wait_s=0";
        let signed = sign(
            &epoch,
            client
                .get(format!("{url}{path}"))
                .bearer_auth(jwt("owner", now)),
            &phone_sign,
            "phone-1",
            "GET",
            path,
            &[],
            12,
        );
        let request = signed.build().unwrap();
        let clone = request.try_clone().unwrap();
        let changes = client.execute(request).await.unwrap();
        assert_eq!(changes.status(), StatusCode::OK);
        let changes: Value = changes.json().await.unwrap();
        assert_eq!(changes["epoch"], server.epoch);
        assert_eq!(changes["changes"][0]["status"], "completed");
        assert!(
            changes["changes"][0].get("response").unwrap().is_null(),
            "changes carry status only"
        );
        assert_eq!(
            client.execute(clone).await.unwrap().status(),
            StatusCode::UNAUTHORIZED,
            "one-time device nonce must reject replay"
        );
        let (url_after_restart, restarted, restarted_join) = test_server(&mac_sign).await;
        assert_ne!(restarted.epoch, server.epoch);
        let replay_after_restart = sign(
            &epoch,
            client.get(format!("{url_after_restart}/v1/work?wait_s=0")),
            &mac_sign,
            "mac-1",
            "GET",
            "/v1/work?wait_s=0",
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
        assert_eq!(
            server
                .queue
                .changes("owner", "phone-1", 0, now_ms())
                .1
                .len(),
            0,
            "revocation purges even completed ciphertext"
        );
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
        let denied = sign(
            &epoch,
            client
                .get(format!("{url}{status_path}"))
                .bearer_auth(jwt("owner", now)),
            &phone_sign,
            "phone-1",
            "GET",
            status_path,
            &[],
            15,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        restarted_join.abort();
        join.abort();
    }

    #[tokio::test]
    async fn mac_auth_rejects_replay_when_wall_clock_moves_back() {
        let mac_sign = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let (_url, server, join) = test_server(&mac_sign).await;
        let epoch = server.auth.server_epoch();
        let uri: Uri = "/v1/work?wait_s=0".parse().unwrap();
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
        let early = headers_at(1_000_000, 1);
        assert!(
            server
                .auth
                .authenticate_mac(&early, &Method::GET, &uri, &[], 1_000_000).await
                .is_ok()
        );
        let later = headers_at(1_300_001, 2);
        assert!(
            server
                .auth
                .authenticate_mac(&later, &Method::GET, &uri, &[], 1_300_001).await
                .is_ok()
        );
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
            device_registry_path: std::env::temp_dir().join(format!(
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
    async fn recovered_account_token_cannot_replace_device_key_or_read_its_content() {
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
        server.queue.submit("owner","phone-approved","mac-1","prior-content",[4;32],
            json!({"ciphertext":"opaque request"}),now+60_000,now).unwrap();
        server.queue.next_work("mac-1",now).unwrap();
        server.queue.mark_received("mac-1","prior-content",now).unwrap();
        server.queue.respond("mac-1","phone-approved","prior-content",json!({"ciphertext":"opaque response"}),now).unwrap();
        let client=reqwest::Client::new();
        let path="/v1/requests/prior-content";
        // Positive control: the content exists and remains retrievable with the
        // original device key plus the very same recovered account credential.
        let response=sign(&epoch,client.get(format!("{url}{path}")).bearer_auth(&recovered),
            &approved,"phone-approved","GET",path,&[],1).send().await.unwrap();
        assert_eq!(response.status(),StatusCode::OK);
        for (id,nonce) in [("phone-approved",2),("phone-attacker",3)] {
            let denied=sign(&epoch,client.get(format!("{url}{path}")).bearer_auth(&recovered),
                &attacker,id,"GET",path,&[],nonce).send().await.unwrap();
            assert_eq!(denied.status(),StatusCode::UNAUTHORIZED);
            assert!(!denied.text().await.unwrap().contains("opaque response"));
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
        server
            .queue
            .submit(
                "owner",
                "phone-pilot",
                "mac-1",
                "prior-content",
                [9; 32],
                json!({"ciphertext":"prior-owner"}),
                now_ms() + 60_000,
                now_ms(),
            )
            .unwrap();
        assert!(
            server.queue.next_work("mac-second", now_ms()).is_none(),
            "a newly registered Mac must not receive the pilot Mac's prior ciphertext"
        );
        // A valid account token can add a fresh Mac, but that Mac must not inherit
        // a phone or ciphertext already paired to the pilot Mac.
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
        let stolen_work = sign(
            &epoch,
            client.get(format!("{url}/v1/work?wait_s=0")),
            &stolen_key,
            "mac-stolen",
            "GET",
            "/v1/work?wait_s=0",
            &[],
            11,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(stolen_work.status(), StatusCode::NO_CONTENT);
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
        let now = now_ms();
        let rerouted = EnvelopeV1::seal_random(
            Context {
                route: "mac-stolen",
                sender: "phone-pilot",
                recipient: "mac-stolen",
                request_id: "reroute",
                kind: "request",
                expires_at_ms: now + 60_000,
            },
            br#"{"private":true}"#,
            &replaced_key,
            &agreement.public_key(),
        )
        .unwrap();
        let rerouted_body = serde_json::to_vec(&rerouted).unwrap();
        let rerouted_response = sign(
            &epoch,
            client
                .post(format!("{url}/v1/requests"))
                .bearer_auth(jwt("owner", now))
                .body(rerouted_body.clone()),
            &replaced_key,
            "phone-pilot",
            "POST",
            "/v1/requests",
            &rerouted_body,
            12,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(rerouted_response.status(), StatusCode::BAD_REQUEST);
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
        let revoked_work = sign(
            &epoch,
            client.get(format!("{url}/v1/work?wait_s=0")),
            &mac_key,
            "mac-second",
            "GET",
            "/v1/work?wait_s=0",
            &[],
            8,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(revoked_work.status(), StatusCode::UNAUTHORIZED);
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
}

fn authentication_error(error_value:anyhow::Error)->(StatusCode,Json<Value>) {
    match error_value.downcast_ref::<auth::AccountGateError>() {
        Some(auth::AccountGateError::Unavailable)=>error(StatusCode::SERVICE_UNAVAILABLE,"account_access_unavailable"),
        Some(auth::AccountGateError::Denied)=>error(StatusCode::FORBIDDEN,"account_access_denied"),
        None=>unauthorized(),
    }
}
