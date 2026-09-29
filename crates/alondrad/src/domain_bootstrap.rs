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
    pub(crate) fn open(home: &Path) -> Result<Arc<Self>> {
        Self::open_with(home, Box::new(crate::domain_github::GhCli::new()))
    }

    pub(crate) fn open_with(
        home: &Path,
        observer: Box<dyn crate::domain_github::PrObserver>,
    ) -> Result<Arc<Self>> {
        let database = crate::domain_database_migration::active_path(home);
        let metadata = std::fs::symlink_metadata(&database).with_context(|| {
            format!("committed domain database missing: {}", database.display())
        })?;
        ensure!(
            metadata.is_file(),
            "committed domain database must be a regular file"
        );
        let domain = Arc::new(DomainRuntime::open(home)?);
        let gauges = crate::machine::Gauges::default();
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
    fn new_work_client(&self) -> Option<&crate::v2_client_routes::Client> {
        Some(&self.client)
    }
}

pub(crate) fn router(state: Arc<V2Only>) -> Router {
    let client = state.client();
    Router::new()
        .route("/daemon", get(daemon))
        .route("/pairing", post(new_pairing))
        .route("/pair", post(pair))
        .route("/devices", get(devices))
        .route("/devices/{id}", axum::routing::delete(revoke_device))
        .route(
            "/uploads",
            post(upload).layer(axum::extract::DefaultBodyLimit::max(
                crate::uploads::MAX_UPLOAD_BYTES + 4096,
            )),
        )
        .route("/files", get(file))
        .route("/agents/{id}/messages", post(agent_message))
        .route("/orchestrator/cancel", post(orchestrator_decision))
        .route("/orchestrator/acknowledge", post(orchestrator_decision))
        .route("/orchestrator/retry", post(orchestrator_decision))
        .route("/orchestrator/launch", post(orchestrator_launch))
        .route("/orchestrator/send", post(orchestrator_send))
        .route("/orchestrator/interrupt", post(orchestrator_interrupt))
        .route("/orchestrator/machine", get(orchestrator_machine))
        .route("/orchestrator/schedules", post(orchestrator_schedule))
        .route("/orchestrator/fleet", get(orchestrator_fleet))
        .route("/orchestrator/node", get(orchestrator_node))
        .route("/orchestrator/candidates", get(orchestrator_candidates))
        .route("/agents/waiting-machine", post(agent_waiting_machine))
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
    auth.list()
        .iter()
        .any(|entry| entry.id == signed.device_id && entry.public_key == signed.public_key)
        .then(|| signed.clone())
}

/// The running version, update state and machine gauges, for `alondra update`,
/// `alondra doctor` and the TUI header on this Mac.
async fn daemon(
    State(state): State<Arc<V2Only>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    let (machine, usage) = match &*state.gauges.lock().expect("machine gauges poisoned") {
        Some((machine, usage)) => (json!(machine), json!(usage)),
        None => (Value::Null, Value::Null),
    };
    Json(
        json!({"version": env!("CARGO_PKG_VERSION"), "update": crate::updates::status(&state.home),
        "preflight_rejected": crate::relaunch::rejected(), "machine": machine, "usage": usage}),
    )
    .into_response()
}

/// A one-time code for a device on this Mac's network; only a local caller may mint one.
async fn new_pairing(
    State(state): State<Arc<V2Only>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    let url = alondra_core::config::Config::load()
        .ok()
        .and_then(|config| config.public_url);
    Json(json!({"code": state.auth.new_pairing_code(), "expires_in": 600, "url": url}))
        .into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairBody {
    code: String,
    device_name: String,
    public_key: String,
}

async fn pair(
    State(state): State<Arc<V2Only>>,
    body: Result<Json<PairBody>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return crate::domain_api::domain_error(DomainError::new(
            "invalid_input",
            "invalid pairing request",
        ));
    };
    match state
        .auth
        .pair(&body.code, &body.device_name, &body.public_key)
    {
        Ok(device) => Json(json!({"device_id": device.id})).into_response(),
        Err(message) => (
            StatusCode::UNAUTHORIZED,
            Json(DomainError::new("forbidden", message)),
        )
            .into_response(),
    }
}

async fn devices(
    State(state): State<Arc<V2Only>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    let list: Vec<Value> = state.auth.list().into_iter()
        .map(|d| json!({"id": d.id, "name": d.name, "created_at": d.created_at, "last_seen": d.last_seen}))
        .collect();
    Json(json!({"devices": list})).into_response()
}

async fn revoke_device(
    State(state): State<Arc<V2Only>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    RoutePath(id): RoutePath<String>,
) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    match state.auth.revoke(&id) {
        Ok(true) => Json(json!({"ok": true})).into_response(),
        Ok(false) => {
            crate::domain_api::domain_error(DomainError::new("not_found", "device does not exist"))
        }
        Err(_) => storage(),
    }
}

#[derive(Deserialize)]
struct UploadQuery {
    name: String,
}

/// The file itself is the body. The reply is the `AttachmentRef` a v2 command
/// carries: the stored copy's digest and its reference under this HOME.
async fn upload(
    State(state): State<Arc<V2Only>>,
    signed: Option<Extension<Signed>>,
    Query(query): Query<UploadQuery>,
    body: axum::body::Bytes,
) -> Response {
    if signed_device(&state.home, &signed).is_none() {
        return forbidden("signed user device required");
    }
    let home = state.home.clone();
    match tokio::task::spawn_blocking(move || attachment_ref(&home, &query.name, &body)).await {
        Ok(Ok(reference)) => Json(reference).into_response(),
        Ok(Err(error)) => {
            crate::domain_api::domain_error(DomainError::new("invalid_input", error.to_string()))
        }
        Err(_) => storage(),
    }
}

async fn agent_message(
    State(state): State<Arc<V2Only>>,
    signed: Option<Extension<Signed>>,
    RoutePath(id): RoutePath<String>,
    body: Result<Json<crate::domain_messages::MessageBody>, JsonRejection>,
) -> Response {
    let Some(device) = signed_device(&state.home, &signed) else {
        return forbidden("signed user device required");
    };
    let Ok(Json(body)) = body else {
        return crate::domain_api::domain_error(DomainError::new(
            "invalid_input",
            "invalid message",
        ));
    };
    let user = alondra_core::domain_v2::Actor::User {
        user_id: format!("device:{}", device.device_id),
    };
    match crate::domain_messages::deliver(
        &state.domain,
        &state.home,
        &user,
        &crate::domain_messages::user_name(),
        &id.into(),
        body,
        &crate::domain_messages::ClaudeAgents,
        &Arc::new(crate::domain_transport::Tmux),
    )
    .await
    {
        Ok(answer) => Json(answer).into_response(),
        Err(error) => crate::domain_api::domain_error(error),
    }
}

async fn orchestrator_decision(
    State(state): State<Arc<V2Only>>,
    signed: Option<Extension<Signed>>,
    uri: axum::http::Uri,
    body: Option<Json<Value>>,
) -> Response {
    if signed_device(&state.home, &signed).is_none() {
        return forbidden("signed user device required");
    }
    let Some(decision) = crate::domain_orchestrator::decision_route(uri.path()) else {
        return crate::domain_api::domain_error(DomainError::new(
            "not_found",
            "unknown orchestrator decision",
        ));
    };
    let executor = crate::domain_orchestrator::executor(state.domain(), &state.home);
    let body = body.map(|Json(body)| body);
    match tokio::task::spawn_blocking(move || {
        crate::domain_orchestrator::decide(&executor, decision, body.as_ref())
    })
    .await
    {
        Ok(answer) => {
            let status = answer["status"]
                .as_u64()
                .and_then(|s| StatusCode::from_u16(s as u16).ok())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(answer["body"].clone())).into_response()
        }
        Err(_) => storage(),
    }
}

/// The orchestrator execution calling a tool route: local, with its current bearer.
fn tool_caller(
    state: &V2Only,
    peer: &SocketAddr,
    headers: &HeaderMap,
) -> Result<alondra_core::domain_v2::Actor, Response> {
    if let Some(refused) = local_only(peer, headers) {
        return Err(refused);
    }
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    crate::domain_orchestrator_tools::orchestrator(&state.domain, bearer)
        .map_err(crate::domain_api::domain_error)
}

fn tool_answer(answer: Result<Value, DomainError>) -> Response {
    match answer {
        Ok(answer) => Json(answer).into_response(),
        Err(error) => crate::domain_api::domain_error(error),
    }
}

async fn orchestrator_launch(
    State(state): State<Arc<V2Only>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Json<crate::domain_orchestrator_tools::LaunchBody>, JsonRejection>,
) -> Response {
    let actor = match tool_caller(&state, &peer, &headers) {
        Ok(actor) => actor,
        Err(refused) => return refused,
    };
    let Ok(Json(body)) = body else {
        return crate::domain_api::domain_error(DomainError::new(
            "invalid_input",
            "invalid launch",
        ));
    };
    let domain = state.domain();
    let choices = |request: &alondra_core::domain_v2::RequestId| {
        crate::domain_orchestrator_tools::request_choices(&domain, request)
            .ok()
            .flatten()
    };
    tool_answer(
        crate::domain_orchestrator_tools::launch(
            &state.domain,
            &state.home,
            &actor,
            body,
            choices,
            &crate::orchestrate::ClaudeCli,
        )
        .await,
    )
}

async fn orchestrator_send(
    State(state): State<Arc<V2Only>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Json<crate::domain_orchestrator_tools::SendBody>, JsonRejection>,
) -> Response {
    let actor = match tool_caller(&state, &peer, &headers) {
        Ok(actor) => actor,
        Err(refused) => return refused,
    };
    let Ok(Json(body)) = body else {
        return crate::domain_api::domain_error(DomainError::new(
            "invalid_input",
            "invalid message",
        ));
    };
    tool_answer(
        crate::domain_orchestrator_tools::send(
            &state.domain,
            &state.home,
            &actor,
            body,
            &crate::domain_messages::ClaudeAgents,
            &Arc::new(crate::domain_transport::Tmux),
        )
        .await,
    )
}

async fn orchestrator_interrupt(
    State(state): State<Arc<V2Only>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Json<crate::domain_orchestrator_tools::NodeBody>, JsonRejection>,
) -> Response {
    if let Err(refused) = tool_caller(&state, &peer, &headers) {
        return refused;
    }
    let Ok(Json(body)) = body else {
        return crate::domain_api::domain_error(DomainError::new(
            "invalid_input",
            "invalid interrupt",
        ));
    };
    tool_answer(
        crate::domain_orchestrator_tools::interrupt(
            &state.domain,
            body,
            &crate::domain_messages::ClaudeAgents,
            crate::wake::interrupt,
        )
        .await,
    )
}

/// A Schedule the orchestrator creates for the user, materialized as the user's own drafts are.
async fn orchestrator_schedule(
    State(state): State<Arc<V2Only>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Err(refused) = tool_caller(&state, &peer, &headers) {
        return refused;
    }
    let Ok(Json(body)) = body else {
        return crate::domain_api::domain_error(DomainError::new(
            "invalid_input",
            "invalid Schedule draft",
        ));
    };
    let home = state.home.clone();
    let domain = state.domain();
    match tokio::task::spawn_blocking(move || {
        crate::domain_schedule_api::mutation(
            &domain,
            &home,
            "POST",
            "/schedules",
            Some(&body),
            None,
            || Ok(()),
        )
    })
    .await
    {
        Ok(answer) => crate::domain_schedule_api::response(answer),
        Err(_) => storage(),
    }
}

fn gauges_json(state: &V2Only) -> (Value, Value) {
    match &*state.gauges.lock().expect("machine gauges poisoned") {
        Some((machine, usage)) => (json!(machine), json!(usage)),
        None => (Value::Null, Value::Null),
    }
}

async fn orchestrator_fleet(
    State(state): State<Arc<V2Only>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Err(refused) = tool_caller(&state, &peer, &headers) {
        return refused;
    }
    let (machine, usage) = gauges_json(&state);
    tool_answer(crate::domain_orchestrator_reads::fleet(
        &state.domain,
        machine,
        usage,
    ))
}

#[derive(Deserialize)]
struct NodeQuery {
    node: String,
}

async fn orchestrator_node(
    State(state): State<Arc<V2Only>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<NodeQuery>,
) -> Response {
    if let Err(refused) = tool_caller(&state, &peer, &headers) {
        return refused;
    }
    tool_answer(crate::domain_orchestrator_reads::node(
        &state.domain,
        &query.node,
    ))
}

#[derive(Deserialize)]
struct CandidatesQuery {
    task: Option<String>,
    repo: Option<String>,
}

async fn orchestrator_candidates(
    State(state): State<Arc<V2Only>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<CandidatesQuery>,
) -> Response {
    if let Err(refused) = tool_caller(&state, &peer, &headers) {
        return refused;
    }
    tool_answer(crate::domain_orchestrator_reads::candidates(
        &state.domain,
        query.task.as_deref(),
        query.repo.as_deref(),
    ))
}

#[derive(Deserialize)]
struct WaitingBody {
    reason: String,
}

/// An Agent declares it is stopped waiting for the machine, as 8ccd131's `wait_for_machine` did:
/// its live state says so until its next turn.
async fn agent_waiting_machine(
    State(state): State<Arc<V2Only>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Json<WaitingBody>, JsonRejection>,
) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let session_id = match bearer.map(|token| state.domain.resolve_bearer(token)) {
        Some(Ok(crate::domain_authority::Principal::Agent { session_id, .. })) => session_id,
        _ => return forbidden("an Agent's capability is required"),
    };
    let Ok(Json(body)) = body else {
        return crate::domain_api::domain_error(DomainError::new(
            "invalid_input",
            "invalid reason",
        ));
    };
    state
        .domain
        .live
        .apply(&alondra_core::Event::WaitingMachine {
            session_id,
            reason: body.reason,
        });
    Json(json!({"ok": true})).into_response()
}

/// Machine capacity and the subscription's usage, as the `/daemon` gauges hold them.
async fn orchestrator_machine(
    State(state): State<Arc<V2Only>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Err(refused) = tool_caller(&state, &peer, &headers) {
        return refused;
    }
    let (machine, usage) = match &*state.gauges.lock().expect("machine gauges poisoned") {
        Some((machine, usage)) => (json!(machine), json!(usage)),
        None => (Value::Null, Value::Null),
    };
    Json(json!({"machine": machine, "usage": usage})).into_response()
}

fn attachment_ref(home: &Path, name: &str, bytes: &[u8]) -> anyhow::Result<AttachmentRef> {
    let (id, attachment) = crate::uploads::store(home, name, bytes)?;
    let stored = attachment
        .stored_path
        .as_deref()
        .context("upload has no stored copy")?;
    crate::service_files::attachment_ref(home, id, attachment.name, Path::new(stored))
}

#[derive(Deserialize)]
struct FileQuery {
    path: String,
}

/// A stored copy under this HOME's `files`, by the reference a command or attachment carries.
async fn file(
    State(state): State<Arc<V2Only>>,
    signed: Option<Extension<Signed>>,
    Query(query): Query<FileQuery>,
) -> Response {
    if signed_device(&state.home, &signed).is_none() {
        return forbidden("signed user device required");
    }
    let path = Path::new(&query.path);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        state.home.join(path)
    };
    let home = state.home.clone();
    let opened = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let mut file = crate::service_files::open_stored_file(&home, &path)?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut bytes)?;
        Ok(bytes)
    })
    .await;
    match opened {
        Ok(Ok(bytes)) => (
            [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
            bytes,
        )
            .into_response(),
        Ok(Err(_)) => crate::domain_api::domain_error(DomainError::new(
            "not_found",
            "stored copy does not exist",
        )),
        Err(_) => storage(),
    }
}

fn storage() -> Response {
    crate::domain_api::domain_error(DomainError::new(
        "storage_unavailable",
        "daemon storage could not be read",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Auth;
    use axum::http::StatusCode;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use p256::ecdsa::{Signature, SigningKey, signature::Signer};
    use std::net::SocketAddr;

    #[test]
    fn a_store_reset_for_another_schema_still_boots() {
        let home = std::env::temp_dir().join(format!(
            "alondra-v2-bootstrap-reset-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir(&home).unwrap();
        drop(DomainRuntime::open(&home).unwrap());
        rusqlite::Connection::open(home.join("domain.db"))
            .unwrap()
            .pragma_update(None, "user_version", 99)
            .unwrap();
        let state = V2Only::open(&home).unwrap();
        assert_eq!(state.domain().state().unwrap().revision, 0);
        std::fs::remove_dir_all(&home).unwrap();
    }

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
        let state = V2Only::open(&home).unwrap();
        let domain = state.domain();
        let auth = state.auth();
        let capability = domain
            .issue(crate::domain_authority::Principal::Agent {
                session_id: "test-agent".into(),
                task_binding: None,
                provider_agent_id: None,
            })
            .unwrap();
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
        let daemon: serde_json::Value = client
            .get(format!("{base}/daemon"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(daemon["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(daemon["update"]["current"], env!("CARGO_PKG_VERSION"));
        for (method, path) in [
            ("POST", "/v1/nodes/n/messages"),
            ("PUT", "/v1"),
            ("DELETE", "/v1/questions/1"),
        ] {
            let response = client
                .request(method.parse().unwrap(), format!("{base}{path}"))
                .body(r#"{"text":"legacy"}"#)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
        }
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
                .get(format!("{base}/v1x"))
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
            StatusCode::NOT_FOUND
        );
        let path = "/summary";
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
        let producer = client
            .get(format!("{base}/capabilities"))
            .bearer_auth(&bearer)
            .send()
            .await
            .unwrap();
        assert_eq!(producer.status(), StatusCode::OK);
        let cursor: serde_json::Value = producer.json().await.unwrap();
        assert_eq!(cursor["data_epoch"].as_str(), Some(epoch.as_str()));
        assert_eq!(
            client
                .get(format!("{base}/capabilities"))
                .bearer_auth("invalid")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        for (path, expected) in [
            ("/schedules", StatusCode::OK),
            ("/schedules/missing", StatusCode::NOT_FOUND),
            ("/delivery-attempts?after=0", StatusCode::OK),
            ("/tasks/missing/actions", StatusCode::NOT_FOUND),
            (
                "/document-revisions/missing/content?offset=0",
                StatusCode::NOT_FOUND,
            ),
            ("/pull-requests/missing/diff", StatusCode::NOT_FOUND),
        ] {
            assert_eq!(
                client
                    .get(format!("{base}{path}"))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::FORBIDDEN,
                "unsigned {path}"
            );
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
            assert_eq!(response.status(), expected, "signed {path}");
            if expected == StatusCode::NOT_FOUND {
                let body: serde_json::Value = response.json().await.unwrap();
                assert_eq!(body["code"], "not_found", "handler must answer {path}");
            }
        }
        let merge_path = "/merge-authorizations/missing";
        assert_eq!(
            client
                .get(format!("{base}{merge_path}"))
                .header("authorization", format!("Bearer {}", "a".repeat(64)))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let response = client
            .get(format!("{base}{merge_path}"))
            .header("authorization", format!("Bearer {bearer}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(
            body["code"], "not_found",
            "handler must answer merge authorization read"
        );
        assert_eq!(
            client
                .get(format!("{base}/orchestrator/machine"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            client
                .get(format!("{base}/orchestrator/machine"))
                .bearer_auth(&bearer)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN,
            "an Agent's bearer is not the orchestrator's"
        );
        let orchestrator = domain.bind_orchestrator("execution-1").unwrap();
        let machine = client
            .get(format!("{base}/orchestrator/machine"))
            .bearer_auth(orchestrator.expose_for_child_environment())
            .send()
            .await
            .unwrap();
        assert_eq!(machine.status(), StatusCode::OK);
        assert!(
            machine
                .json::<serde_json::Value>()
                .await
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("usage")
        );
        let launch = client
            .post(format!("{base}/orchestrator/launch"))
            .bearer_auth(orchestrator.expose_for_child_environment())
            .json(&json!({"name": "n", "cwd": "/tmp", "task": "missing"}))
            .send()
            .await
            .unwrap();
        assert_eq!(launch.status(), StatusCode::NOT_FOUND);
        std::fs::write(
            alondra_core::options::path_in(&home),
            format!("projects_root = {:?}\n", home.to_str().unwrap()),
        )
        .unwrap();
        let draft = json!({"title": "Nightly", "text": "Review the project", "project": null, "frequency": "daily", "hour": 9, "minute": 0});
        assert_eq!(
            client
                .post(format!("{base}/orchestrator/schedules"))
                .bearer_auth(&bearer)
                .json(&draft)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN,
            "an Agent does not create Schedules"
        );
        let created = client
            .post(format!("{base}/orchestrator/schedules"))
            .bearer_auth(orchestrator.expose_for_child_environment())
            .json(&draft)
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        assert_eq!(
            domain
                .state()
                .unwrap()
                .schedule
                .schedules
                .values()
                .next()
                .unwrap()
                .title,
            "Nightly"
        );
        let orchestrator_get = |path: &str| {
            client
                .get(format!("{base}{path}"))
                .bearer_auth(orchestrator.expose_for_child_environment())
                .send()
        };
        assert_eq!(
            client
                .get(format!("{base}/orchestrator/fleet"))
                .bearer_auth(&bearer)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let fleet = orchestrator_get("/orchestrator/fleet").await.unwrap();
        assert_eq!(fleet.status(), StatusCode::OK);
        assert!(fleet.json::<serde_json::Value>().await.unwrap()["nodes"].is_array());
        assert_eq!(
            orchestrator_get("/orchestrator/node?node=nobody")
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            orchestrator_get("/orchestrator/candidates")
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        domain.live.observe(&json!({"session_id": "test-agent", "hook_event_name": "UserPromptSubmit", "cwd": "/tmp", "prompt": "go"}));
        let waiting = json!({"reason": "the build queue is full"});
        assert_eq!(
            client
                .post(format!("{base}/agents/waiting-machine"))
                .bearer_auth(orchestrator.expose_for_child_environment())
                .json(&waiting)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN,
            "only an Agent waits for the machine"
        );
        assert_eq!(
            client
                .post(format!("{base}/agents/waiting-machine"))
                .bearer_auth(&bearer)
                .json(&waiting)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let node = domain.live.node("test-agent").unwrap();
        assert_eq!(
            node.turn_signal,
            Some(alondra_core::TurnSignal::WaitingMachine),
            "{node:?}"
        );
        domain.revoke_agent("test-agent").unwrap();
        assert_eq!(
            client
                .get(format!("{base}{merge_path}"))
                .header("authorization", format!("Bearer {bearer}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            client
                .get(format!("{base}/capabilities"))
                .bearer_auth(&bearer)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        server.abort();
        let _ = server.await;
        std::fs::remove_dir_all(home).unwrap();
    }

    fn signed(
        client: &reqwest::Client,
        base: &str,
        key: &SigningKey,
        device: &str,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> reqwest::RequestBuilder {
        static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        let bytes = body
            .as_ref()
            .map(|body| serde_json::to_vec(body).unwrap())
            .unwrap_or_default();
        let timestamp =
            alondra_core::now_millis() + NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            .header(
                "x-alondra-signature",
                STANDARD.encode(signature.to_der().as_bytes()),
            );
        if body.is_some() {
            request
                .header("content-type", "application/json")
                .body(bytes)
        } else {
            request
        }
    }

    fn signed_raw(
        client: &reqwest::Client,
        base: &str,
        key: &SigningKey,
        device: &str,
        path: &str,
        bytes: Vec<u8>,
    ) -> reqwest::RequestBuilder {
        static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        let timestamp =
            alondra_core::now_millis() + NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let message = crate::auth::signed_message("POST", path, &timestamp.to_string(), &bytes);
        let signature: Signature = key.sign(message.as_bytes());
        client
            .post(format!("{base}{path}"))
            .header("x-alondra-device", device)
            .header("x-alondra-timestamp", timestamp.to_string())
            .header(
                "x-alondra-signature",
                STANDARD.encode(signature.to_der().as_bytes()),
            )
            .header("content-type", "application/octet-stream")
            .body(bytes)
    }

    #[tokio::test]
    async fn a_paired_local_device_is_the_user_for_commands_and_schedule_writes() {
        let home = std::env::temp_dir().join(format!(
            "alondra-v2-local-user-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir(&home).unwrap();
        std::fs::write(
            alondra_core::options::path_in(&home),
            format!("projects_root = {:?}\n", home.to_str().unwrap()),
        )
        .unwrap();
        let _domain_epoch = DomainRuntime::open(&home)
            .unwrap()
            .state()
            .unwrap()
            .data_epoch
            .0;
        let auth = Auth::open(&home).unwrap();
        let key = SigningKey::from_slice(&[21u8; 32]).unwrap();
        let device = auth
            .pair(
                &auth.new_pairing_code(),
                "terminal",
                &STANDARD.encode(key.verifying_key().to_sec1_bytes()),
            )
            .unwrap();
        let stranger = SigningKey::from_slice(&[22u8; 32]).unwrap();
        let state = V2Only::open(&home).unwrap();
        let domain = state.domain();
        let gauges = state.gauges();
        let auth = state.auth();
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
        let request = json!({"request_id":"request-one", "text":"Work", "project":null,
            "options":{}, "issue":null, "attachments":[]});
        assert_eq!(
            client
                .post(format!("{base}/requests"))
                .json(&request)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            signed(
                &client,
                &base,
                &stranger,
                "unknown-device",
                "POST",
                "/requests",
                Some(request.clone())
            )
            .send()
            .await
            .unwrap()
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            signed(
                &client,
                &base,
                &stranger,
                &device.id,
                "POST",
                "/requests",
                Some(request.clone())
            )
            .send()
            .await
            .unwrap()
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert!(domain.state().unwrap().requests.is_empty());

        let response = signed(
            &client,
            &base,
            &key,
            &device.id,
            "POST",
            "/requests",
            Some(request.clone()),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let outcome: serde_json::Value = response.json().await.unwrap();
        assert_eq!(outcome["created_ids"][0], "request-one");
        assert!(
            domain
                .state()
                .unwrap()
                .requests
                .contains_key(&"request-one".into())
        );
        let audit: String = rusqlite::Connection::open(home.join("domain.db"))
            .unwrap()
            .query_row(
                "SELECT actor FROM command_audit ORDER BY revision DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(audit.contains(&format!("device:{}", device.id)), "{audit}");
        let replay = signed(
            &client,
            &base,
            &key,
            &device.id,
            "POST",
            "/requests",
            Some(request.clone()),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(
            replay.status(),
            StatusCode::OK,
            "the same command id replays its receipt"
        );
        assert_eq!(domain.state().unwrap().requests.len(), 1);

        // Producers use the same Request resource with the flat, typed command
        // fields. The HTTP adapter supplies the CommandEnvelope metadata itself.
        let producer = domain
            .issue(crate::domain_authority::Principal::Orchestrator {
                generation: 0,
                execution_id: "exec-1".into(),
            })
            .unwrap();
        let current = domain.state().unwrap();
        let typed_request = json!({"request_id":"request-mcp", "body":"Define work", "auto_merge":false,
            "data_epoch":current.data_epoch, "command_id":"mcp-create-request", "expected_revision":current.revision});
        drop(current);
        let typed = client
            .post(format!("{base}/requests"))
            .bearer_auth(producer.expose_for_child_environment())
            .json(&typed_request)
            .send()
            .await
            .unwrap();
        assert_eq!(typed.status(), StatusCode::OK);
        assert!(
            domain
                .state()
                .unwrap()
                .requests
                .contains_key(&"request-mcp".into())
        );
        let wrong_actor = client
            .post(format!("{base}/requests"))
            .bearer_auth(producer.expose_for_child_environment())
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(
            wrong_actor.status(),
            StatusCode::FORBIDDEN,
            "a producer cannot submit the User launch form"
        );

        let draft = json!({"title": "Nightly", "text": "Review", "project": null, "options": {},
            "disallowed_tools": [], "frequency": "daily", "hour": 9, "minute": 0,
            "weekday": null, "interval": null, "start_at": null, "enabled": false, "revision": 0});
        assert_eq!(
            client
                .post(format!("{base}/schedules"))
                .json(&draft)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let created = signed(
            &client,
            &base,
            &key,
            &device.id,
            "POST",
            "/schedules",
            Some(draft.clone()),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let created: serde_json::Value = created.json().await.unwrap();
        let schedule_id = created["id"].as_str().unwrap().to_owned();
        assert_eq!(domain.state().unwrap().schedule.schedules.len(), 1);
        let mut update = draft.clone();
        update["title"] = json!("Nightly review");
        update["revision"] = json!(1);
        assert_eq!(
            signed(
                &client,
                &base,
                &key,
                &device.id,
                "PUT",
                &format!("/schedules/{schedule_id}"),
                Some(update)
            )
            .send()
            .await
            .unwrap()
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            signed(
                &client,
                &base,
                &key,
                &device.id,
                "POST",
                &format!("/schedules/{schedule_id}/runs"),
                None
            )
            .send()
            .await
            .unwrap()
            .status(),
            StatusCode::ACCEPTED
        );
        assert_eq!(domain.state().unwrap().schedule.runs.len(), 1);
        assert_eq!(
            signed(
                &client,
                &base,
                &key,
                &device.id,
                "DELETE",
                &format!("/schedules/{schedule_id}"),
                None
            )
            .send()
            .await
            .unwrap()
            .status(),
            StatusCode::CONFLICT,
            "a launching run blocks deletion"
        );

        // A new terminal pairs over loopback and appears among the devices.
        let code: serde_json::Value = client
            .post(format!("{base}/pairing"))
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(code["expires_in"], 600);
        let newcomer = SigningKey::from_slice(&[23u8; 32]).unwrap();
        let paired = client
            .post(format!("{base}/pair"))
            .json(
                &json!({"code": code["code"], "device_name": "Terminal (test)",
            "public_key": STANDARD.encode(newcomer.verifying_key().to_sec1_bytes())}),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(paired.status(), StatusCode::OK);
        let newcomer_id = paired.json::<serde_json::Value>().await.unwrap()["device_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            client
                .post(format!("{base}/pair"))
                .json(&json!({"code": code["code"], "device_name": "Again",
            "public_key": STANDARD.encode(newcomer.verifying_key().to_sec1_bytes())}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED,
            "a pairing code works once"
        );
        let listed: serde_json::Value = client
            .get(format!("{base}/devices"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(
            listed["devices"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["id"] == newcomer_id)
        );
        let proxied = client
            .get(format!("{base}/devices"))
            .header("x-forwarded-for", "10.0.0.9")
            .send()
            .await
            .unwrap()
            .status();
        assert!(
            proxied == StatusCode::FORBIDDEN || proxied == StatusCode::UNAUTHORIZED,
            "device management is local only: {proxied}"
        );

        // Attachments: the upload answers with the AttachmentRef a v2 command carries.
        let bytes = b"design notes for the task".to_vec();
        assert_eq!(
            client
                .post(format!("{base}/uploads?name=notes.md"))
                .body(bytes.clone())
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let uploaded = signed_raw(
            &client,
            &base,
            &newcomer,
            &newcomer_id,
            "/uploads?name=notes.md",
            bytes.clone(),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(uploaded.status(), StatusCode::OK);
        let reference: alondra_core::domain_v2::AttachmentRef = uploaded.json().await.unwrap();
        let expected: String = <sha2::Sha256 as sha2::Digest>::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(reference.content_digest, expected);
        assert_eq!(reference.bytes, bytes.len() as u64);
        assert!(
            reference.content_reference.starts_with("files/uploads/"),
            "{}",
            reference.content_reference
        );
        assert!(home.join(&reference.content_reference).is_file());
        let path = format!("/files?path={}", reference.content_reference);
        assert_eq!(
            client
                .get(format!("{base}{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let served = signed(&client, &base, &newcomer, &newcomer_id, "GET", &path, None)
            .send()
            .await
            .unwrap();
        assert_eq!(served.status(), StatusCode::OK);
        assert_eq!(served.bytes().await.unwrap().to_vec(), bytes);
        let outside = "/files?path=/etc/hosts";
        assert_eq!(
            signed(
                &client,
                &base,
                &newcomer,
                &newcomer_id,
                "GET",
                outside,
                None
            )
            .send()
            .await
            .unwrap()
            .status(),
            StatusCode::NOT_FOUND,
            "only stored copies are served"
        );
        let message = json!({"text": "hi"});
        assert_eq!(
            client
                .post(format!("{base}/agents/agent-z/messages"))
                .json(&message)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN,
            "a message needs the signed user"
        );
        let unknown = signed(
            &client,
            &base,
            &key,
            &device.id,
            "POST",
            "/agents/agent-z/messages",
            Some(message),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            unknown.json::<serde_json::Value>().await.unwrap()["code"],
            "not_found"
        );
        assert_eq!(
            client
                .post(format!("{base}/orchestrator/retry"))
                .json(&json!({"run_id": "run-7"}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN,
            "a decision needs the signed user"
        );
        let none = signed(
            &client,
            &base,
            &key,
            &device.id,
            "POST",
            "/orchestrator/acknowledge",
            Some(json!({"run_id": "run-7"})),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(none.status(), StatusCode::CONFLICT);
        assert_eq!(
            none.json::<serde_json::Value>().await.unwrap()["code"],
            "conflict"
        );
        let publish = json!({"request_id":"request-attached", "text":"Work with a file", "project":null,
            "options":{}, "issue":null, "attachments":[]});
        assert_eq!(
            signed(
                &client,
                &base,
                &newcomer,
                &newcomer_id,
                "POST",
                "/requests",
                Some(publish)
            )
            .send()
            .await
            .unwrap()
            .status(),
            StatusCode::OK,
            "the newly paired terminal is the user too"
        );

        // Machine gauges and usage travel with the daemon read once sampled.
        let daemon: serde_json::Value = client
            .get(format!("{base}/daemon"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(daemon["machine"].is_null());
        *gauges.lock().unwrap() = Some((
            alondra_core::Machine {
                at: 1,
                memory_total: 16,
                memory_available: 8,
                memory_free_percent: Some(50),
                load: [1.0, 1.0, 1.0],
                cpus: 8,
                cpu_usage: 12.5,
                disk_total: 100,
                disk_available: 40,
                unreliable: false,
            },
            None,
        ));
        let daemon: serde_json::Value = client
            .get(format!("{base}/daemon"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(daemon["machine"]["memory_free_percent"], 50);
        assert_eq!(daemon["machine"]["cpus"], 8);
        assert_eq!(daemon["version"], env!("CARGO_PKG_VERSION"));

        assert_eq!(
            client
                .delete(format!("{base}/devices/{newcomer_id}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            signed(&client, &base, &newcomer, &newcomer_id, "GET", &path, None)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED,
            "an unpaired terminal reads nothing"
        );

        Auth::open(&home).unwrap().revoke(&device.id).unwrap();
        let revoked_request = json!({"request_id":"request-two", "text":"Work", "project":null,
            "options":{}, "issue":null, "attachments":[]});
        assert_eq!(
            signed(
                &client,
                &base,
                &key,
                &device.id,
                "POST",
                "/requests",
                Some(revoked_request)
            )
            .send()
            .await
            .unwrap()
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(domain.state().unwrap().requests.len(), 3);
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
        assert!(V2Only::open(&home).is_err());
        assert!(
            !home.join("domain.db").exists(),
            "must not create an empty authority on missing stage"
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[cfg(test)]
mod vertical_tests;
