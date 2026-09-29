//! Local v2-only HTTP bootstrap. This path never constructs a legacy Hub;
//! the routes are the shared domain handlers with a committed host.
use crate::auth::{Signed, is_local};
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain_v2::{AttachmentRef, DomainError};
use anyhow::{Context, Result, ensure};
use axum::extract::{ConnectInfo, Path as RoutePath, Query, State, rejection::JsonRejection};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) struct V2Only {
    domain: Arc<DomainRuntime>,
    home: PathBuf,
    observer: Box<dyn crate::domain_github::PrObserver>,
    gauges: crate::machine::Gauges,
    /// The one approval list the guard verifies against and the pairing routes extend.
    auth: Arc<crate::auth::Auth>,
    client: Arc<crate::v2_client_routes::Client>,
}

impl V2Only {
    pub(crate) fn open(home: &Path, expected_epoch: &str) -> Result<Arc<Self>> {
        Self::open_with(home, expected_epoch, Box::new(crate::domain_github::GhCli::new()))
    }

    pub(crate) fn open_with(home: &Path, expected_epoch: &str,
        observer: Box<dyn crate::domain_github::PrObserver>) -> Result<Arc<Self>> {
        let database = home.join("domain-v2.db");
        let metadata = std::fs::symlink_metadata(&database).with_context(|| {
            format!("committed domain database missing: {}", database.display())
        })?;
        ensure!(
            metadata.is_file(),
            "committed domain database must be a regular file"
        );
        let domain = Arc::new(DomainRuntime::open(home)?);
        let gauges = crate::machine::Gauges::default();
        ensure!(
            domain.state()?.data_epoch.0 == expected_epoch,
            "cutover epoch does not match domain database"
        );
        Ok(Arc::new(Self {
            home: home.to_owned(),
            observer,
            client: crate::v2_client_routes::Client::open(domain.clone(), home, gauges.clone()),
            domain,
            gauges,
            auth: Arc::new(crate::auth::Auth::open(home)?),
        }))
    }

    pub(crate) fn auth(&self) -> Arc<crate::auth::Auth> {
        self.auth.clone()
    }

    pub(crate) fn domain(&self) -> Arc<DomainRuntime> {
        self.domain.clone()
    }

    pub(crate) fn gauges(&self) -> crate::machine::Gauges {
        self.gauges.clone()
    }

    pub(crate) fn client(&self) -> Arc<crate::v2_client_routes::Client> {
        self.client.clone()
    }
}

impl crate::domain_api::DomainHost for V2Only {
    fn domain(&self) -> &DomainRuntime {
        &self.domain
    }
    fn home(&self) -> &Path {
        &self.home
    }
    fn cutover_committed(&self) -> bool {
        true
    }
    fn pr_observer(&self) -> &dyn crate::domain_github::PrObserver {
        &*self.observer
    }
}

pub(crate) fn router(state: Arc<V2Only>) -> Router {
    let client = state.client();
    Router::new()
        .route("/v2/daemon", get(daemon))
        .route("/v2/pairing", post(new_pairing))
        .route("/v2/pair", post(pair))
        .route("/v2/devices", get(devices))
        .route("/v2/devices/{id}", axum::routing::delete(revoke_device))
        .route("/v2/uploads", post(upload).layer(axum::extract::DefaultBodyLimit::max(crate::uploads::MAX_UPLOAD_BYTES + 4096)))
        .route("/v2/files", get(file))
        .merge(crate::domain_api::routes())
        .with_state(state)
        .merge(crate::v2_client_routes::router(client))
}

fn forbidden(message: &str) -> Response {
    crate::domain_api::domain_error(DomainError::new("forbidden", message))
}

fn local_only(peer: &SocketAddr, headers: &HeaderMap) -> Option<Response> {
    (!is_local(peer, headers)).then(|| forbidden("local request required"))
}

/// The paired device the middleware verified, while its approval persists.
pub(crate) fn signed_device(home: &Path, signed: &Option<Extension<Signed>>) -> Option<Signed> {
    let Extension(signed) = signed.as_ref()?;
    let auth = crate::auth::Auth::open(home).ok()?;
    auth.list().iter().any(|entry| entry.id == signed.device_id && entry.public_key == signed.public_key)
        .then(|| signed.clone())
}

/// The running version, update state and machine gauges, for `alondra update`,
/// `alondra doctor` and the TUI header on this Mac.
async fn daemon(State(state): State<Arc<V2Only>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    let (machine, usage) = match &*state.gauges.lock().expect("machine gauges poisoned") {
        Some((machine, usage)) => (json!(machine), json!(usage)),
        None => (Value::Null, Value::Null),
    };
    Json(json!({"version": env!("CARGO_PKG_VERSION"), "update": crate::updates::status(&state.home),
        "preflight_rejected": crate::relaunch::rejected(), "machine": machine, "usage": usage})).into_response()
}

/// A one-time code for a device on this Mac's network; only a local caller may mint one.
async fn new_pairing(State(state): State<Arc<V2Only>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    let url = alondra_core::config::Config::load().ok().and_then(|config| config.public_url);
    Json(json!({"code": state.auth.new_pairing_code(), "expires_in": 600, "url": url})).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairBody {
    code: String,
    device_name: String,
    public_key: String,
}

async fn pair(State(state): State<Arc<V2Only>>, body: Result<Json<PairBody>, JsonRejection>) -> Response {
    let Ok(Json(body)) = body else {
        return crate::domain_api::domain_error(DomainError::new("invalid_input", "invalid pairing request"));
    };
    match state.auth.pair(&body.code, &body.device_name, &body.public_key) {
        Ok(device) => Json(json!({"device_id": device.id})).into_response(),
        Err(message) => (StatusCode::UNAUTHORIZED, Json(DomainError::new("forbidden", message))).into_response(),
    }
}

async fn devices(State(state): State<Arc<V2Only>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    let list: Vec<Value> = state.auth.list().into_iter()
        .map(|d| json!({"id": d.id, "name": d.name, "created_at": d.created_at, "last_seen": d.last_seen}))
        .collect();
    Json(json!({"devices": list})).into_response()
}

async fn revoke_device(State(state): State<Arc<V2Only>>, ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap, RoutePath(id): RoutePath<String>) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    match state.auth.revoke(&id) {
        Ok(true) => Json(json!({"ok": true})).into_response(),
        Ok(false) => crate::domain_api::domain_error(DomainError::new("not_found", "device does not exist")),
        Err(_) => storage(),
    }
}

#[derive(Deserialize)]
struct UploadQuery {
    name: String,
}

/// The file itself is the body. The reply is the `AttachmentRef` a v2 command
/// carries: the stored copy's digest and its reference under this HOME.
async fn upload(State(state): State<Arc<V2Only>>, signed: Option<Extension<Signed>>,
    Query(query): Query<UploadQuery>, body: axum::body::Bytes) -> Response {
    if signed_device(&state.home, &signed).is_none() {
        return forbidden("signed user device required");
    }
    let home = state.home.clone();
    match tokio::task::spawn_blocking(move || attachment_ref(&home, &query.name, &body)).await {
        Ok(Ok(reference)) => Json(reference).into_response(),
        Ok(Err(error)) => crate::domain_api::domain_error(DomainError::new("invalid_input", error.to_string())),
        Err(_) => storage(),
    }
}

fn attachment_ref(home: &Path, name: &str, bytes: &[u8]) -> anyhow::Result<AttachmentRef> {
    let (id, attachment) = crate::uploads::store(home, name, bytes)?;
    let stored = attachment.stored_path.as_deref().context("upload has no stored copy")?;
    let stored = Path::new(stored);
    let mut file = crate::service_files::open_stored_file(home, stored)?;
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let count = std::io::Read::read(&mut file, &mut buffer)?;
        if count == 0 {
            break;
        }
        sha2::Digest::update(&mut hasher, &buffer[..count]);
        total += count as u64;
    }
    let digest: String = sha2::Digest::finalize(hasher).iter().map(|byte| format!("{byte:02x}")).collect();
    let reference = stored.strip_prefix(home).unwrap_or(stored);
    Ok(AttachmentRef {
        id,
        name: attachment.name,
        content_digest: digest,
        content_reference: reference.to_string_lossy().into_owned(),
        bytes: total,
    })
}

#[derive(Deserialize)]
struct FileQuery {
    path: String,
}

/// A stored copy under this HOME's `files`, by the reference a command or attachment carries.
async fn file(State(state): State<Arc<V2Only>>, signed: Option<Extension<Signed>>, Query(query): Query<FileQuery>) -> Response {
    if signed_device(&state.home, &signed).is_none() {
        return forbidden("signed user device required");
    }
    let path = Path::new(&query.path);
    let path = if path.is_absolute() { path.to_path_buf() } else { state.home.join(path) };
    let home = state.home.clone();
    let opened = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let mut file = crate::service_files::open_stored_file(&home, &path)?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut bytes)?;
        Ok(bytes)
    }).await;
    match opened {
        Ok(Ok(bytes)) => ([(axum::http::header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response(),
        Ok(Err(_)) => crate::domain_api::domain_error(DomainError::new("not_found", "stored copy does not exist")),
        Err(_) => storage(),
    }
}

fn storage() -> Response {
    crate::domain_api::domain_error(DomainError::new("storage_unavailable", "daemon storage could not be read"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Auth;
    use axum::http::StatusCode;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use p256::ecdsa::{Signature, SigningKey, signature::Signer};
    use std::net::SocketAddr;

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
        let auth = state.auth();
        let capability = domain.issue(crate::domain_authority::Principal::Agent {
            session_id: "test-agent".into(), task_binding: None, provider_agent_id: None,
        }).unwrap();
        let bearer = capability.expose_for_child_environment().to_owned();
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
        let daemon: serde_json::Value = client.get(format!("{base}/v2/daemon")).send().await.unwrap().json().await.unwrap();
        assert_eq!(daemon["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(daemon["update"]["current"], env!("CARGO_PKG_VERSION"));
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
        domain.revoke_agent("test-agent").unwrap();
        assert_eq!(client.get(format!("{base}{merge_path}"))
            .header("authorization", format!("Bearer {bearer}"))
            .send().await.unwrap().status(), StatusCode::FORBIDDEN);
        assert_eq!(client.get(format!("{base}/v2/domain/capabilities"))
            .bearer_auth(&bearer).send().await.unwrap().status(), StatusCode::FORBIDDEN);
        server.abort();
        let _ = server.await;
        std::fs::remove_dir_all(home).unwrap();
    }

    fn signed(client: &reqwest::Client, base: &str, key: &SigningKey, device: &str,
        method: &str, path: &str, body: Option<serde_json::Value>) -> reqwest::RequestBuilder {
        static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        let bytes = body.as_ref().map(|body| serde_json::to_vec(body).unwrap()).unwrap_or_default();
        let timestamp = alondra_core::now_millis() + NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let message = crate::auth::signed_message(method, path, &timestamp.to_string(), &bytes);
        let signature: Signature = key.sign(message.as_bytes());
        let request = match method {
            "POST" => client.post(format!("{base}{path}")),
            "PUT" => client.put(format!("{base}{path}")),
            "DELETE" => client.delete(format!("{base}{path}")),
            _ => client.get(format!("{base}{path}")),
        };
        let request = request
            .header("x-alondra-device", device)
            .header("x-alondra-timestamp", timestamp.to_string())
            .header("x-alondra-signature", STANDARD.encode(signature.to_der().as_bytes()));
        if body.is_some() { request.header("content-type", "application/json").body(bytes) } else { request }
    }

    fn signed_raw(client: &reqwest::Client, base: &str, key: &SigningKey, device: &str,
        path: &str, bytes: Vec<u8>) -> reqwest::RequestBuilder {
        static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        let timestamp = alondra_core::now_millis() + NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let message = crate::auth::signed_message("POST", path, &timestamp.to_string(), &bytes);
        let signature: Signature = key.sign(message.as_bytes());
        client.post(format!("{base}{path}"))
            .header("x-alondra-device", device)
            .header("x-alondra-timestamp", timestamp.to_string())
            .header("x-alondra-signature", STANDARD.encode(signature.to_der().as_bytes()))
            .header("content-type", "application/octet-stream")
            .body(bytes)
    }

    #[tokio::test]
    async fn a_paired_local_device_is_the_user_for_commands_and_schedule_writes() {
        let home = std::env::temp_dir().join(format!("alondra-v2-local-user-{}", crate::auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        std::fs::write(alondra_core::options::path_in(&home),
            format!("projects_root = {:?}\n", home.to_str().unwrap())).unwrap();
        let epoch = DomainRuntime::open(&home).unwrap().state().unwrap().data_epoch.0;
        let auth = Auth::open(&home).unwrap();
        let key = SigningKey::from_slice(&[21u8; 32]).unwrap();
        let device = auth.pair(&auth.new_pairing_code(), "terminal",
            &STANDARD.encode(key.verifying_key().to_sec1_bytes())).unwrap();
        let stranger = SigningKey::from_slice(&[22u8; 32]).unwrap();
        let state = V2Only::open(&home, &epoch).unwrap();
        let domain = state.domain();
        let gauges = state.gauges();
        let auth = state.auth();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(state).layer(axum::middleware::from_fn_with_state(auth, crate::auth::guard));
        let server = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
        });
        let client = reqwest::Client::new();
        let envelope = |id: &str, revision: u64| json!({"protocol_version": 2, "data_epoch": epoch,
            "command_id": id, "expected_revision": revision,
            "command": {"kind": "create_request", "request_id": format!("request-{id}"), "body": "Work"}});
        let revision = domain.state().unwrap().revision;
        assert_eq!(client.post(format!("{base}/v2/domain/commands")).json(&envelope("unsigned", revision))
            .send().await.unwrap().status(), StatusCode::FORBIDDEN);
        assert_eq!(signed(&client, &base, &stranger, "unknown-device", "POST", "/v2/domain/commands",
            Some(envelope("unknown", revision))).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        assert_eq!(signed(&client, &base, &stranger, &device.id, "POST", "/v2/domain/commands",
            Some(envelope("forged", revision))).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        assert!(domain.state().unwrap().requests.is_empty());

        let response = signed(&client, &base, &key, &device.id, "POST", "/v2/domain/commands",
            Some(envelope("one", revision))).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let outcome: serde_json::Value = response.json().await.unwrap();
        assert_eq!(outcome["created_ids"][0], "request-one");
        assert!(domain.state().unwrap().requests.contains_key(&"request-one".into()));
        let audit: String = rusqlite::Connection::open(home.join("domain-v2.db")).unwrap()
            .query_row("SELECT actor FROM command_audit ORDER BY revision DESC LIMIT 1", [], |row| row.get(0)).unwrap();
        assert!(audit.contains(&format!("device:{}", device.id)), "{audit}");
        let replay = signed(&client, &base, &key, &device.id, "POST", "/v2/domain/commands",
            Some(envelope("one", revision))).send().await.unwrap();
        assert_eq!(replay.status(), StatusCode::OK, "the same command id replays its receipt");
        assert_eq!(domain.state().unwrap().requests.len(), 1);

        let draft = json!({"title": "Nightly", "text": "Review", "project": null, "options": {},
            "disallowed_tools": [], "frequency": "daily", "hour": 9, "minute": 0,
            "weekday": null, "interval": null, "start_at": null, "enabled": false, "revision": 0});
        assert_eq!(client.post(format!("{base}/v2/domain/schedules")).json(&draft)
            .send().await.unwrap().status(), StatusCode::FORBIDDEN);
        let created = signed(&client, &base, &key, &device.id, "POST", "/v2/domain/schedules", Some(draft.clone()))
            .send().await.unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let created: serde_json::Value = created.json().await.unwrap();
        let schedule_id = created["id"].as_str().unwrap().to_owned();
        assert_eq!(domain.state().unwrap().schedule.schedules.len(), 1);
        let mut update = draft.clone();
        update["title"] = json!("Nightly review");
        update["revision"] = json!(1);
        assert_eq!(signed(&client, &base, &key, &device.id, "PUT", &format!("/v2/domain/schedules/{schedule_id}"), Some(update))
            .send().await.unwrap().status(), StatusCode::OK);
        assert_eq!(signed(&client, &base, &key, &device.id, "POST", &format!("/v2/domain/schedules/{schedule_id}/run"), None)
            .send().await.unwrap().status(), StatusCode::ACCEPTED);
        assert_eq!(domain.state().unwrap().schedule.runs.len(), 1);
        assert_eq!(signed(&client, &base, &key, &device.id, "DELETE", &format!("/v2/domain/schedules/{schedule_id}"), None)
            .send().await.unwrap().status(), StatusCode::CONFLICT, "a launching run blocks deletion");

        // A new terminal pairs over loopback and appears among the devices.
        let code: serde_json::Value = client.post(format!("{base}/v2/pairing")).json(&json!({})).send().await.unwrap().json().await.unwrap();
        assert_eq!(code["expires_in"], 600);
        let newcomer = SigningKey::from_slice(&[23u8; 32]).unwrap();
        let paired = client.post(format!("{base}/v2/pair")).json(&json!({"code": code["code"], "device_name": "Terminal (test)",
            "public_key": STANDARD.encode(newcomer.verifying_key().to_sec1_bytes())})).send().await.unwrap();
        assert_eq!(paired.status(), StatusCode::OK);
        let newcomer_id = paired.json::<serde_json::Value>().await.unwrap()["device_id"].as_str().unwrap().to_owned();
        assert_eq!(client.post(format!("{base}/v2/pair")).json(&json!({"code": code["code"], "device_name": "Again",
            "public_key": STANDARD.encode(newcomer.verifying_key().to_sec1_bytes())})).send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED, "a pairing code works once");
        let listed: serde_json::Value = client.get(format!("{base}/v2/devices")).send().await.unwrap().json().await.unwrap();
        assert!(listed["devices"].as_array().unwrap().iter().any(|d| d["id"] == newcomer_id));
        let proxied = client.get(format!("{base}/v2/devices")).header("x-forwarded-for", "10.0.0.9")
            .send().await.unwrap().status();
        assert!(proxied == StatusCode::FORBIDDEN || proxied == StatusCode::UNAUTHORIZED, "device management is local only: {proxied}");

        // Attachments: the upload answers with the AttachmentRef a v2 command carries.
        let bytes = b"design notes for the task".to_vec();
        assert_eq!(client.post(format!("{base}/v2/uploads?name=notes.md")).body(bytes.clone())
            .send().await.unwrap().status(), StatusCode::FORBIDDEN);
        let uploaded = signed_raw(&client, &base, &newcomer, &newcomer_id, "/v2/uploads?name=notes.md", bytes.clone())
            .send().await.unwrap();
        assert_eq!(uploaded.status(), StatusCode::OK);
        let reference: alondra_core::domain_v2::AttachmentRef = uploaded.json().await.unwrap();
        let expected: String = <sha2::Sha256 as sha2::Digest>::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(reference.content_digest, expected);
        assert_eq!(reference.bytes, bytes.len() as u64);
        assert!(reference.content_reference.starts_with("files/uploads/"), "{}", reference.content_reference);
        assert!(home.join(&reference.content_reference).is_file());
        let path = format!("/v2/files?path={}", reference.content_reference);
        assert_eq!(client.get(format!("{base}{path}")).send().await.unwrap().status(), StatusCode::FORBIDDEN);
        let served = signed(&client, &base, &newcomer, &newcomer_id, "GET", &path, None).send().await.unwrap();
        assert_eq!(served.status(), StatusCode::OK);
        assert_eq!(served.bytes().await.unwrap().to_vec(), bytes);
        let outside = "/v2/files?path=/etc/hosts";
        assert_eq!(signed(&client, &base, &newcomer, &newcomer_id, "GET", outside, None).send().await.unwrap().status(),
            StatusCode::NOT_FOUND, "only stored copies are served");
        let publish = json!({"protocol_version": 2, "data_epoch": epoch, "command_id": "attached",
            "expected_revision": domain.state().unwrap().revision,
            "command": {"kind": "create_request", "request_id": "request-attached", "body": "Work with a file"}});
        assert_eq!(signed(&client, &base, &newcomer, &newcomer_id, "POST", "/v2/domain/commands", Some(publish))
            .send().await.unwrap().status(), StatusCode::OK, "the newly paired terminal is the user too");

        // Machine gauges and usage travel with the daemon read once sampled.
        let daemon: serde_json::Value = client.get(format!("{base}/v2/daemon")).send().await.unwrap().json().await.unwrap();
        assert!(daemon["machine"].is_null());
        *gauges.lock().unwrap() = Some((alondra_core::Machine { at: 1, memory_total: 16, memory_available: 8,
            memory_free_percent: Some(50), load: [1.0, 1.0, 1.0], cpus: 8, cpu_usage: 12.5, disk_total: 100,
            disk_available: 40, unreliable: false }, None));
        let daemon: serde_json::Value = client.get(format!("{base}/v2/daemon")).send().await.unwrap().json().await.unwrap();
        assert_eq!(daemon["machine"]["memory_free_percent"], 50);
        assert_eq!(daemon["machine"]["cpus"], 8);
        assert_eq!(daemon["version"], env!("CARGO_PKG_VERSION"));

        assert_eq!(client.delete(format!("{base}/v2/devices/{newcomer_id}")).send().await.unwrap().status(), StatusCode::OK);
        assert_eq!(signed(&client, &base, &newcomer, &newcomer_id, "GET", &path, None).send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED, "an unpaired terminal reads nothing");

        Auth::open(&home).unwrap().revoke(&device.id).unwrap();
        assert_eq!(signed(&client, &base, &key, &device.id, "POST", "/v2/domain/commands",
            Some(envelope("two", domain.state().unwrap().revision))).send().await.unwrap().status(), StatusCode::FORBIDDEN);
        assert_eq!(domain.state().unwrap().requests.len(), 2);
        server.abort();
        let _ = server.await;
        drop(domain);
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
