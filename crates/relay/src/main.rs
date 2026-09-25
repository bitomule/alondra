mod auth;
mod state;

use alondra_e2e::EnvelopeV1;
use auth::{Auth, Config, PhoneDevice};
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
    queue: Queue,
    work_waiters: Arc<Semaphore>,
    change_waiters: Arc<Semaphore>,
    requests: Arc<Semaphore>,
    device_gate: Arc<RwLock<()>>,
    epoch: String,
}

fn router(server: Arc<Server>) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/v1/bootstrap", get(bootstrap))
        .route("/v1/devices", post(enroll))
        .route("/v1/devices/{id}", axum::routing::delete(revoke))
        .route("/v1/requests", post(submit))
        .route("/v1/requests/{id}", get(request_status))
        .route("/v1/changes", get(changes))
        .route("/v1/work", get(work))
        .route("/v1/work/{id}/receipt", post(receipt))
        .route("/v1/work/{id}/response", post(respond))
        .layer(DefaultBodyLimit::max(state::MAX_RESPONSE_BYTES))
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
    }
}

async fn enroll(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    server
        .auth
        .authenticate_mac(&headers, &method, &uri, &body, now_ms())
        .map_err(|_| unauthorized())?;
    let _gate = server.device_gate.write().await;
    if body.len() > 4096 {
        return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"));
    }
    let device: PhoneDevice = serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_device"))?;
    server
        .auth
        .enroll(device)
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
    server
        .auth
        .authenticate_mac(&headers, &method, &uri, &[], now_ms())
        .map_err(|_| unauthorized())?;
    let _gate = server.device_gate.write().await;
    server
        .auth
        .revoke(&id)
        .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "revocation_failed"))?;
    server.queue.purge_phone(&id);
    Ok(StatusCode::NO_CONTENT.into_response())
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
        .map_err(|_| unauthorized())?;
    let (fields, v) = parse_envelope(&body, state::MAX_ENVELOPE_BYTES)?;
    if fields.kind != "request"
        || fields.route != server.auth.config.mac_id
        || fields.recipient != server.auth.config.mac_id
        || fields.sender != who.device_id
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
        .map_err(|_| unauthorized())?;
    fields
        .verify_signature(
            &sender_key,
            &who.device_id,
            &server.auth.config.mac_id,
            &server.auth.config.mac_id,
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
        .map_err(|_| unauthorized())?;
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
        .map_err(|_| unauthorized())?;
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
        if !events.is_empty() || wait_s == 0 || tokio::time::Instant::now() >= deadline {
            drop(permit);
            return Ok((
                StatusCode::OK,
                Json(json!({"epoch":server.epoch,"revision":revision,"changes":events})),
            )
                .into_response());
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
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
    let who = server
        .auth
        .authenticate_mac(&headers, &method, &uri, &[], now_ms())
        .map_err(|_| unauthorized())?;
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
    let who = server
        .auth
        .authenticate_mac(&headers, &method, &uri, &body, now_ms())
        .map_err(|_| unauthorized())?;
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
    let who = server
        .auth
        .authenticate_mac(&headers, &method, &uri, &body, now_ms())
        .map_err(|_| unauthorized())?;
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
            &server.auth.mac_key().map_err(|_| unauthorized())?,
            &who.device_id,
            &phone_id,
            &who.device_id,
            now_ms(),
        )
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_envelope_signature"))?;
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
    let auth = Auth::open(config).await?;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let epoch = URL_SAFE_NO_PAD.encode(auth.server_epoch());
    let server = Arc::new(Server {
        auth,
        queue: Queue::new(),
        work_waiters: Arc::new(Semaphore::new(16)),
        change_waiters: Arc::new(Semaphore::new(64)),
        requests: Arc::new(Semaphore::new(128)),
        device_gate: Arc::new(RwLock::new(())),
        epoch,
    });
    let listener = tokio::net::TcpListener::bind(&listen).await?;
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
        };
        let auth = Auth::test(config, include_bytes!("../tests/fixtures/jwks.json")).unwrap();
        let epoch = URL_SAFE_NO_PAD.encode(auth.server_epoch());
        let server = Arc::new(Server {
            auth,
            queue: Queue::new(),
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
        let after_restart = Auth::test(
            server.auth.config.clone(),
            include_bytes!("../tests/fixtures/jwks.json"),
        )
        .unwrap();
        assert!(!after_restart.is_phone_registered("phone-1"));
        assert!(
            after_restart.enroll(device.clone()).is_err(),
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
                .authenticate_mac(&early, &Method::GET, &uri, &[], 1_000_000)
                .is_ok()
        );
        let later = headers_at(1_300_001, 2);
        assert!(
            server
                .auth
                .authenticate_mac(&later, &Method::GET, &uri, &[], 1_300_001)
                .is_ok()
        );
        assert!(
            server
                .auth
                .authenticate_mac(&early, &Method::GET, &uri, &[], 1_000_000)
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
}
