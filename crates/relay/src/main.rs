mod auth;
mod state;

use auth::{Auth, Config, PhoneDevice};
use axum::{Json, Router, body::Bytes, extract::{DefaultBodyLimit, Path, Query, State}, http::{HeaderMap, Method, StatusCode, Uri}, response::{IntoResponse, Response}, routing::{get, post}};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use state::{Queue, QueueError};
use std::{sync::Arc, time::{Duration, SystemTime, UNIX_EPOCH}};
use tokio::sync::Semaphore;

struct Server {
    auth: Auth,
    queue: Queue,
    work_waiters: Arc<Semaphore>,
    change_waiters: Arc<Semaphore>,
    epoch: String,
}

fn router(server: Arc<Server>) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/v1/devices", post(enroll))
        .route("/v1/requests", post(submit))
        .route("/v1/requests/{id}", get(request_status))
        .route("/v1/changes", get(changes))
        .route("/v1/work", get(work))
        .route("/v1/work/{id}/receipt", post(receipt))
        .route("/v1/work/{id}/response", post(respond))
        .layer(DefaultBodyLimit::max(state::MAX_RESPONSE_BYTES))
        .with_state(server)
}

async fn health() -> impl IntoResponse { (StatusCode::OK, Json(json!({"ok":true}))) }

type ApiResult = Result<Response, (StatusCode, Json<Value>)>;
fn error(status: StatusCode, code: &str) -> (StatusCode, Json<Value>) { (status, Json(json!({"error":code}))) }
fn unauthorized() -> (StatusCode, Json<Value>) { error(StatusCode::UNAUTHORIZED, "unauthorized") }
fn queued_error(e: QueueError) -> (StatusCode, Json<Value>) {
    match e {
        QueueError::Full => error(StatusCode::TOO_MANY_REQUESTS, "queue_full"),
        QueueError::Conflict => error(StatusCode::CONFLICT, "request_id_conflict"),
        QueueError::NotFound | QueueError::WrongDevice => error(StatusCode::NOT_FOUND, "not_found"),
        QueueError::NotReceived => error(StatusCode::CONFLICT, "mac_receipt_required"),
    }
}

async fn enroll(State(server): State<Arc<Server>>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> ApiResult {
    server.auth.authenticate_mac(&headers, &method, &uri, &body, now_ms()).map_err(|_| unauthorized())?;
    if body.len() > 4096 { return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large")); }
    let device: PhoneDevice = serde_json::from_slice(&body).map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_device"))?;
    server.auth.enroll(device).map_err(|_| error(StatusCode::CONFLICT, "device_enrollment_failed"))?;
    Ok((StatusCode::CREATED, Json(json!({"ok":true}))).into_response())
}

#[derive(Deserialize)]
struct EnvelopeFields {
    v: u8,
    route: String,
    sender: String,
    recipient: String,
    request_id: String,
    kind: String,
    expires_at_ms: u64,
    signature: String,
    ciphertext: String,
}

fn parse_envelope(body: &[u8], max: usize) -> Result<(EnvelopeFields, Value), (StatusCode, Json<Value>)> {
    if body.len() > max { return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "envelope_too_large")); }
    let v: Value = serde_json::from_slice(body).map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_envelope"))?;
    let fields: EnvelopeFields = serde_json::from_value(v.clone()).map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_envelope"))?;
    if fields.v != 1 || fields.route.len() > 128 || fields.sender.len() > 128 || fields.recipient.len() > 128 ||
        fields.request_id.is_empty() || fields.request_id.len() > 128 || fields.signature.is_empty() || fields.ciphertext.is_empty() {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_envelope"));
    }
    Ok((fields, v))
}

async fn submit(State(server): State<Arc<Server>>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> ApiResult {
    let who = server.auth.authenticate_phone(&headers, &method, &uri, &body, now_ms()).map_err(|_| unauthorized())?;
    let (fields, v) = parse_envelope(&body, state::MAX_ENVELOPE_BYTES)?;
    if fields.kind != "request" || fields.route != server.auth.config.mac_id || fields.recipient != server.auth.config.mac_id ||
        fields.sender != who.device_id { return Err(error(StatusCode::BAD_REQUEST, "invalid_route")); }
    let now = now_ms();
    if fields.expires_at_ms <= now || fields.expires_at_ms > now.saturating_add(state::MAX_LIFETIME_MS) {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_expiry"));
    }
    // The envelope signature is verified by the E2E crate before any work is admitted.
    verify_envelope(&server, &fields, &v, &who.device_id)?;
    let digest: [u8; 32] = Sha256::digest(&body).into();
    let result = server.queue.submit(&who.owner_sub, &who.device_id, &fields.route, &fields.request_id, digest, v, fields.expires_at_ms, now)
        .map_err(queued_error)?;
    Ok((StatusCode::ACCEPTED, Json(json!({"request_id":result.request_id,"status":result.status,"revision":result.revision}))).into_response())
}

async fn request_status(State(server): State<Arc<Server>>, Path(id): Path<String>, headers: HeaderMap, method: Method, uri: Uri) -> ApiResult {
    let who = server.auth.authenticate_phone(&headers, &method, &uri, &[], now_ms()).map_err(|_| unauthorized())?;
    let result = server.queue.get(&who.owner_sub, &who.device_id, &id, now_ms()).map_err(queued_error)?;
    Ok((StatusCode::OK, Json(json!(result))).into_response())
}

#[derive(Deserialize)]
struct ChangesQuery { #[serde(default)] since: u64, #[serde(default = "default_wait")] wait_s: u64 }
fn default_wait() -> u64 { 25 }

async fn changes(State(server): State<Arc<Server>>, Query(query): Query<ChangesQuery>, headers: HeaderMap, method: Method, uri: Uri) -> ApiResult {
    let who = server.auth.authenticate_phone(&headers, &method, &uri, &[], now_ms()).map_err(|_| unauthorized())?;
    let wait_s = query.wait_s.min(25);
    let permit = server.change_waiters.clone().try_acquire_owned().map_err(|_| error(StatusCode::TOO_MANY_REQUESTS, "too_many_polls"))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait_s);
    loop {
        let notified = server.queue.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let (revision, events) = server.queue.changes(&who.owner_sub, &who.device_id, query.since, now_ms());
        if !events.is_empty() || wait_s == 0 || tokio::time::Instant::now() >= deadline {
            drop(permit);
            return Ok((StatusCode::OK, Json(json!({"epoch":server.epoch,"revision":revision,"changes":events}))).into_response());
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            drop(permit);
            return Ok((StatusCode::OK, Json(json!({"epoch":server.epoch,"revision":revision,"changes":[]}))).into_response());
        }
    }
}

#[derive(Deserialize)]
struct WorkQuery { #[serde(default = "default_wait")] wait_s: u64 }

async fn work(State(server): State<Arc<Server>>, Query(query): Query<WorkQuery>, headers: HeaderMap, method: Method, uri: Uri) -> ApiResult {
    let who = server.auth.authenticate_mac(&headers, &method, &uri, &[], now_ms()).map_err(|_| unauthorized())?;
    let wait_s = query.wait_s.min(25);
    let permit = server.work_waiters.clone().try_acquire_owned().map_err(|_| error(StatusCode::TOO_MANY_REQUESTS, "too_many_polls"))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait_s);
    loop {
        let notified = server.queue.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(item) = server.queue.next_work(&who.device_id, now_ms()) {
            drop(permit);
            return Ok((StatusCode::OK, Json(json!({"request_id":item.request_id,"envelope":item.envelope}))).into_response());
        }
        if wait_s == 0 || tokio::time::Instant::now() >= deadline { drop(permit); return Ok(StatusCode::NO_CONTENT.into_response()); }
        if tokio::time::timeout_at(deadline, notified).await.is_err() { drop(permit); return Ok(StatusCode::NO_CONTENT.into_response()); }
    }
}

async fn receipt(State(server): State<Arc<Server>>, Path(id): Path<String>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> ApiResult {
    let who = server.auth.authenticate_mac(&headers, &method, &uri, &body, now_ms()).map_err(|_| unauthorized())?;
    if !body.is_empty() { return Err(error(StatusCode::BAD_REQUEST, "receipt_body_must_be_empty")); }
    let status = server.queue.mark_received(&who.device_id, &id, now_ms()).map_err(queued_error)?;
    Ok((StatusCode::OK, Json(json!({"request_id":status.request_id,"status":status.status}))).into_response())
}

async fn respond(State(server): State<Arc<Server>>, Path(id): Path<String>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> ApiResult {
    let who = server.auth.authenticate_mac(&headers, &method, &uri, &body, now_ms()).map_err(|_| unauthorized())?;
    let (fields, v) = parse_envelope(&body, state::MAX_RESPONSE_BYTES)?;
    if fields.kind != "response" || fields.request_id != id || fields.route != who.device_id || fields.sender != who.device_id {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_route"));
    }
    verify_envelope(&server, &fields, &v, &who.device_id)?;
    let status = server.queue.respond(&who.device_id, &fields.recipient, &id, v, now_ms()).map_err(queued_error)?;
    Ok((StatusCode::OK, Json(json!({"request_id":status.request_id,"status":status.status}))).into_response())
}

fn verify_envelope(_server: &Server, _fields: &EnvelopeFields, _value: &Value, _expected_sender: &str) -> Result<(), (StatusCode, Json<Value>)> {
    // Replaced with alondra-e2e::EnvelopeV1::verify_signature after crypto crate is merged.
    Err(error(StatusCode::SERVICE_UNAVAILABLE, "e2e_verifier_unavailable"))
}

fn now_ms() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64 }

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = std::env::var("ALONDRA_RELAY_CONFIG")?;
    let config: Config = serde_json::from_slice(&std::fs::read(path)?)?;
    let listen = config.listen.clone();
    let auth = Auth::open(config).await?;
    let mut epoch_bytes = [0u8; 16];
    use ring::rand::{SecureRandom, SystemRandom};
    SystemRandom::new().fill(&mut epoch_bytes).map_err(|_| anyhow::anyhow!("could not generate relay epoch"))?;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let server = Arc::new(Server { auth, queue: Queue::new(), work_waiters: Arc::new(Semaphore::new(16)), change_waiters: Arc::new(Semaphore::new(64)), epoch: URL_SAFE_NO_PAD.encode(epoch_bytes) });
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    println!("alondra-relay listening on {listen}");
    axum::serve(listener, router(server)).await?;
    Ok(())
}
