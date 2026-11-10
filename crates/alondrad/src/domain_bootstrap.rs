//! Local HTTP bootstrap: the routes are the shared domain handlers.
use crate::locks::Poisonable as _;
use crate::auth::{Signed, is_local};
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{AttachmentRef, DomainError};
use anyhow::{Context, Result, ensure};
use axum::extract::{ConnectInfo, Path as RoutePath, Query, RawQuery, State, rejection::JsonRejection};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) struct DomainDaemon {
    domain: Arc<DomainRuntime>,
    home: PathBuf,
    gauges: crate::machine::Gauges,
    /// The one approval list the guard verifies against and the pairing routes extend.
    pub(crate) auth: Arc<crate::auth::Auth>,
    client: Arc<crate::client_routes::Client>,
    service: std::sync::OnceLock<Arc<crate::service::Service>>,
    direct: std::sync::OnceLock<Arc<crate::direct::Direct>>,
    delivery_reviews: crate::delivery_review::DeliveryReviews,
}

fn require_database(home: &Path) -> Result<()> {
    let database = crate::domain_database_migration::active_path(home);
    let metadata = std::fs::symlink_metadata(&database).with_context(|| {
        format!("committed domain database missing: {}", database.display())
    })?;
    ensure!(
        metadata.is_file(),
        "committed domain database must be a regular file"
    );
    Ok(())
}

impl DomainDaemon {
    pub(crate) fn open(home: &Path) -> Result<Arc<Self>> {
        require_database(home)?;
        Self::over(home, DomainRuntime::open(home)?)
    }

    /// The daemon over the domain its start already opened, so the journal is replayed once.
    pub(crate) fn over(home: &Path, domain: DomainRuntime) -> Result<Arc<Self>> {
        Self::over_shared(home,Arc::new(domain))
    }

    pub(crate) fn over_shared(home: &Path, domain: Arc<DomainRuntime>) -> Result<Arc<Self>> {
        require_database(home)?;
        let gauges = crate::machine::Gauges::default();
        Ok(Arc::new(Self {
            home: home.to_owned(),
            client: crate::client_routes::Client::open(domain.clone(), home, gauges.clone()),
            domain,
            gauges,
            auth: Arc::new(crate::auth::Auth::open(home)?),
            service: std::sync::OnceLock::new(),
            direct: std::sync::OnceLock::new(),
            delivery_reviews: Default::default(),
        }))
    }

    pub(crate) fn attach_service(&self, service: Arc<crate::service::Service>) {
        let _ = self.service.set(service);
    }

    pub(crate) fn attach_direct(&self, direct: Arc<crate::direct::Direct>) {
        let _ = self.direct.set(direct);
    }

    fn reload_service_devices(&self) -> Result<()> {
        self.service.get().map_or(Ok(()), |service| service.reload_devices())
    }

    /// Unpairs `id` on every path at once: a request it still signs is answered, sealed, that it
    /// was revoked. Blocks on the approval lease.
    pub(crate) fn revoke_device(&self, id: &str) -> Result<bool> {
        let revoked = self.auth.revoke(id)?;
        if revoked && let Some(service) = self.service.get() {
            service.remember_revoked(id);
        }
        self.reload_service_devices()?;
        Ok(revoked)
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

    pub(crate) fn client(&self) -> Arc<crate::client_routes::Client> {
        self.client.clone()
    }
}

impl crate::domain_api::DomainHost for DomainDaemon {
    fn domain(&self) -> &DomainRuntime {
        &self.domain
    }
    fn home(&self) -> &Path {
        &self.home
    }
    fn new_work_client(&self) -> Option<&crate::client_routes::Client> {
        Some(&self.client)
    }
    fn delivery_reviews(&self) -> &crate::delivery_review::DeliveryReviews {
        &self.delivery_reviews
    }
}

pub(crate) fn router(state: Arc<DomainDaemon>) -> Router {
    let client = state.client();
    Router::new()
        .route("/daemon", get(daemon))
        .route("/pairing", post(new_pairing))
        .route("/pair", post(pair))
        .route("/devices", get(devices))
        .route("/devices/pending", get(pending_devices))
        .route("/devices/pending/{id}/approve", post(approve_pending_device))
        .route("/devices/pending/{id}/deny", post(deny_pending_device))
        .route("/metrics", get(metrics))
        .route("/cleanup", post(cleanup))
        .route("/sdk-handover", get(sdk_handover))
        .route("/devices/{id}", axum::routing::delete(revoke_device))
        .route("/service/bootstrap", get(service_bootstrap))
        .route("/service/approve-device", post(service_approve_device))
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
        .route("/orchestrator/launch-plan", post(orchestrator_launch_plan).get(orchestrator_get_launch_plan))
        .route("/orchestrator/launch-plan/update", post(orchestrator_update_launch_plan))
        .route("/orchestrator/send", post(orchestrator_send))
        .route("/orchestrator/interrupt", post(orchestrator_interrupt))
        .route("/sandbox/interrupt/{session}", post(sandbox_interrupt))
        .route("/orchestrator/machine", get(orchestrator_machine))
        .route("/orchestrator/log", get(orchestrator_log))
        .route("/orchestrator/outcomes", post(orchestrator_outcomes))
        .route("/orchestrator/schedules", post(orchestrator_schedule))
        .route("/orchestrator/tasks_in_progress", get(orchestrator_tasks_in_progress))
        .route("/orchestrator/node", get(orchestrator_node))
        .route("/orchestrator/read", post(orchestrator_read))
        .route("/tools/read-skill", post(read_role_skill))
        .route("/agents/context", post(read_task_context))
        .route("/agents/plan-mode",post(request_plan_mode))
        .route("/agents/waiting-machine", post(agent_waiting_machine))
        .route("/cards/open", get(open_cards))
        .route(crate::settings::PATH, get(settings_read).put(settings_write))
        .route(crate::settings::FOLDERS, get(settings_folders))
        .route(crate::settings::CONFIG, axum::routing::put(settings_config))
        .route(crate::accounts::PATH, get(accounts_read).put(accounts_write))
        .route(crate::accounts::CHECK, post(accounts_check))
        .merge(crate::domain_api::routes())
        .merge(crate::views::routes::router())
        .merge(crate::notices::router())
        .with_state(state)
        .merge(crate::client_routes::router(client))
}

fn forbidden(message: &str) -> Response {
    crate::domain_api::domain_error(DomainError::new("forbidden", message))
}

fn local_only(peer: &SocketAddr, headers: &HeaderMap) -> Option<Response> {
    (!is_local(peer, headers)).then(|| forbidden("local request required"))
}

/// A request from Alondra's own clients on this Mac: loopback, no browser `Origin`, and a
/// loopback `Host`, so a web page open on the Mac is refused even under a name that resolves to
/// this Mac.
fn local_client_only(peer: &SocketAddr, headers: &HeaderMap) -> Option<Response> {
    if let Some(refused) = local_only(peer, headers) {
        return Some(refused);
    }
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(|host| host.rsplit_once(':').map_or(host, |(name, _)| name))
        .unwrap_or_default();
    let loopback_host = matches!(host, "127.0.0.1" | "localhost" | "[::1]");
    (headers.contains_key(axum::http::header::ORIGIN) || !loopback_host)
        .then(|| forbidden("pairing comes only from Alondra's own clients on this Mac"))
}

/// A decision taken on this Mac: a local client sending JSON, so a web page open on the Mac
/// cannot approve a pairing request it created itself.
fn local_decision_only(peer: &SocketAddr, headers: &HeaderMap) -> Option<Response> {
    local_client_only(peer, headers).or_else(|| {
        (!crate::direct::json_body(headers)).then(|| forbidden("pairing decisions come only from this Mac"))
    })
}

fn pairing_refused(error: crate::direct::pairing::PairingError) -> Response {
    use crate::direct::pairing::PairingError;
    let status = match &error {
        PairingError::NotFound => StatusCode::NOT_FOUND,
        PairingError::Expired => StatusCode::GONE,
        PairingError::Internal(detail) => {
            log!("pairing decision failed: {detail}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
        _ => StatusCode::CONFLICT,
    };
    (status, Json(json!({"error": error.code()}))).into_response()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// The pairing requests from the local network that wait for someone on this Mac.
async fn pending_devices(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    let pending = state.direct.get().map(|direct| direct.pairings().pending(now_ms())).unwrap_or_default();
    Json(json!({"pending": pending})).into_response()
}

async fn approve_pending_device(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    RoutePath(id): RoutePath<String>,
) -> Response {
    if let Some(refused) = local_decision_only(&peer, &headers) {
        return refused;
    }
    let Some(direct) = state.direct.get().cloned() else {
        return pairing_refused(crate::direct::pairing::PairingError::NotFound);
    };
    match tokio::task::spawn_blocking(move || direct.approve(&id)).await {
        Ok(Ok(())) => Json(json!({"ok": true})).into_response(),
        Ok(Err(error)) => pairing_refused(error),
        Err(_) => storage(),
    }
}

async fn deny_pending_device(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    RoutePath(id): RoutePath<String>,
) -> Response {
    if let Some(refused) = local_decision_only(&peer, &headers) {
        return refused;
    }
    let Some(direct) = state.direct.get() else {
        return pairing_refused(crate::direct::pairing::PairingError::NotFound);
    };
    match direct.pairings().deny(&id, now_ms()) {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(error) => pairing_refused(error),
    }
}

/// The paired device the middleware verified, while its approval persists.
pub(crate) fn signed_device(home: &Path, signed: &Option<Extension<Signed>>) -> Option<Signed> {
    let Extension(signed) = signed.as_ref()?;
    crate::auth::approved_devices(home).ok()?
        .iter()
        .any(|entry| entry.id == signed.device_id && entry.public_key == signed.public_key)
        .then(|| signed.clone())
}

/// Who may read and change this Mac's settings: this Mac unsigned, or an approved device. A
/// browser page open on the Mac sends an `Origin` and is refused.
fn settings_caller(state: &DomainDaemon, peer: &SocketAddr, headers: &HeaderMap, signed: &Option<Extension<Signed>>) -> Result<alondra_core::views::Lang, Response> {
    if headers.contains_key(axum::http::header::ORIGIN) {
        return Err(forbidden("settings change only from Alondra's own clients"));
    }
    if signed.is_some() {
        return signed_device(&state.home, signed)
            .map(|device| state.client.language_for_device(&device.device_id))
            .ok_or_else(|| forbidden("signed user device required"));
    }
    match local_only(peer, headers) {
        Some(refused) => Err(refused),
        None => Ok(alondra_core::views::Lang::En),
    }
}

fn relay_on(state: &DomainDaemon) -> bool {
    state.service.get().is_some_and(|service| service.has_relay())
}

async fn settings_read(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    signed: Option<Extension<Signed>>,
) -> Response {
    if let Err(refused) = settings_caller(&state, &peer, &headers, &signed) {
        return refused;
    }
    let relay = relay_on(&state);
    match tokio::task::spawn_blocking(move || crate::settings::read(&state.home, &crate::options::user_home(), relay)).await {
        Ok(reply) => reply.into_response(),
        Err(_) => storage(),
    }
}

async fn settings_folders(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    signed: Option<Extension<Signed>>,
    uri: axum::http::Uri,
) -> Response {
    if let Err(refused) = settings_caller(&state, &peer, &headers, &signed) {
        return refused;
    }
    let path_and_query = uri.path_and_query().map_or(uri.path(), |p| p.as_str()).to_owned();
    let Some(path) = crate::settings::folders_query(&path_and_query) else {
        return crate::domain_api::domain_error(DomainError::new("invalid_input", "invalid folders query"));
    };
    match tokio::task::spawn_blocking(move || crate::settings::folders(&crate::options::user_home(), path.as_deref())).await {
        Ok(reply) => reply.into_response(),
        Err(_) => storage(),
    }
}

async fn settings_write(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    signed: Option<Extension<Signed>>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let lang = match settings_caller(&state, &peer, &headers, &signed) {
        Ok(lang) => lang,
        Err(refused) => return refused,
    };
    let Ok(Json(body)) = body else {
        return crate::domain_api::domain_error(DomainError::new("invalid_input", "invalid settings"));
    };
    let relay = relay_on(&state);
    match tokio::task::spawn_blocking(move || crate::settings::write(&state.home, &crate::options::user_home(), relay, &body, lang)).await {
        Ok(reply) => reply.into_response(),
        Err(_) => storage(),
    }
}

#[derive(Deserialize, Default)]
struct AccountsQuery {
    capabilities: Option<String>,
}

impl AccountsQuery {
    fn provider_accounts(&self) -> bool {
        self.capabilities.as_deref().is_some_and(|caps| caps.split(',').any(|cap| cap.trim() == "provider_accounts"))
    }
}

async fn accounts_read(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    signed: Option<Extension<Signed>>,
    Query(query): Query<AccountsQuery>,
) -> Response {
    let lang = match settings_caller(&state, &peer, &headers, &signed) {
        Ok(lang) => lang,
        Err(refused) => return refused,
    };
    let (domain, home) = (state.domain(), state.home.clone());
    match tokio::task::spawn_blocking(move || crate::accounts::read(&domain, &home, lang)).await {
        Ok(reply) => crate::accounts::for_client(reply, query.provider_accounts()).into_response(),
        Err(_) => storage(),
    }
}

async fn accounts_write(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    signed: Option<Extension<Signed>>,
    Query(query): Query<AccountsQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let lang = match settings_caller(&state, &peer, &headers, &signed) {
        Ok(lang) => lang,
        Err(refused) => return refused,
    };
    let Ok(Json(body)) = body else {
        return crate::domain_api::domain_error(DomainError::new("invalid_input", "invalid accounts"));
    };
    let (domain, home) = (state.domain(), state.home.clone());
    match tokio::task::spawn_blocking(move || crate::accounts::write(&domain, &home, &body, lang)).await {
        Ok(reply) => crate::accounts::for_client(reply, query.provider_accounts()).into_response(),
        Err(_) => storage(),
    }
}

async fn accounts_check(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    signed: Option<Extension<Signed>>,
    Query(query): Query<AccountsQuery>,
) -> Response {
    let lang = match settings_caller(&state, &peer, &headers, &signed) {
        Ok(lang) => lang,
        Err(refused) => return refused,
    };
    let (domain, home) = (state.domain(), state.home.clone());
    match tokio::task::spawn_blocking(move || {
        crate::accounts::check(&domain, &home);
        crate::accounts::read(&domain, &home, lang)
    }).await {
        Ok(reply) => crate::accounts::for_client(reply, query.provider_accounts()).into_response(),
        Err(_) => storage(),
    }
}

/// `alondra config set` on this Mac: never a device, and never a page open in a browser.
async fn settings_config(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    signed: Option<Extension<Signed>>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    if signed.is_some() || headers.contains_key(axum::http::header::ORIGIN) {
        return forbidden("config changes only from this Mac's terminal");
    }
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    let Ok(Json(body)) = body else {
        return crate::domain_api::domain_error(DomainError::new("invalid_input", "invalid setting"));
    };
    match tokio::task::spawn_blocking(move || crate::settings::write_config(&state.home, &body)).await {
        Ok(reply) => reply.into_response(),
        Err(_) => storage(),
    }
}

/// The running version, update state and machine gauges, for `alondra update`,
/// `alondra doctor` and the TUI header on this Mac.
async fn daemon(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    let (machine, usage) = match &*state.gauges.lock_or_recover("the machine gauges") {
        Some((machine, usage, _)) => (json!(machine), json!(usage)),
        None => (Value::Null, Value::Null),
    };
    Json(
        json!({"version": env!("CARGO_PKG_VERSION"),
        "home": state.home, "sandbox": state.domain.live.sandbox.load(std::sync::atomic::Ordering::Relaxed),
        "views_protocol": crate::views::this_service().views_protocol,
        "update":crate::updates::status(&state.home),
        "preflight_rejected": crate::relaunch::rejected(), "supervised": crate::relaunch::under_launchd(),
        "machine": machine, "usage": usage,
        "claude": crate::claude_check::status(),
        "direct": state.service.get().filter(|_| state.direct.get().is_some()).map(|service| json!({
            "port": service.direct_config().port, "bonjour": crate::direct::bonjour::status()})),
        "iroh": state.service.get().filter(|service| service.iroh_config().enabled).map(|_| crate::iroh_endpoint::status())}),
    )
    .into_response()
}

/// The challenge `alondra device invite` signs to approve a phone through the service; local only.
async fn service_bootstrap(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    Json(json!({"epoch": base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD,
        state.auth.service_epoch())})).into_response()
}

#[derive(Deserialize)]
struct ServiceApproveBody {
    device_id: String,
    name: String,
    signing_key: String,
}

/// A phone the service paired, approved by `alondra device invite` signing with this Mac's key.
async fn service_approve_device(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    if body.len() > 2048 {
        return (StatusCode::PAYLOAD_TOO_LARGE, "service approval too large").into_response();
    }
    if let Err(error) = state.auth.verify_service_approval(&headers, &body) {
        return (StatusCode::UNAUTHORIZED, error).into_response();
    }
    let Ok(body) = serde_json::from_slice::<ServiceApproveBody>(&body) else {
        return (StatusCode::BAD_REQUEST, "invalid service approval").into_response();
    };
    let auth = state.auth.clone();
    let approved = tokio::task::spawn_blocking(move || {
        auth.approve_service(&body.device_id, &body.name, &body.signing_key)
    })
    .await
    .unwrap_or_else(|error| Err(error.to_string().into()));
    match approved {
        Ok(()) => {}
        Err(crate::auth::ServiceApprovalError::Revoked) => {
            return (StatusCode::GONE, String::from(crate::auth::ServiceApprovalError::Revoked)).into_response();
        }
        Err(crate::auth::ServiceApprovalError::Refused(error)) => {
            return (StatusCode::CONFLICT, error).into_response();
        }
    }
    match state.reload_service_devices() {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, format!("service devices not reloaded: {error:#}")).into_response(),
    }
}

/// A one-time code for a device on this Mac's network; only a local client may mint one.
async fn new_pairing(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(refused) = local_client_only(&peer, &headers) {
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
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Json<PairBody>, JsonRejection>,
) -> Response {
    if let Some(refused) = local_decision_only(&peer, &headers) {
        return refused;
    }
    let Ok(Json(body)) = body else {
        return crate::domain_api::domain_error(DomainError::new(
            "invalid_input",
            "invalid pairing request",
        ));
    };
    let auth = state.auth.clone();
    let paired = tokio::task::spawn_blocking(move || {
        auth.pair(&body.code, &body.device_name, &body.public_key)
    })
    .await
    .unwrap_or_else(|error| Err(error.to_string()));
    match paired {
        Ok(device) => Json(json!({"device_id": device.id})).into_response(),
        Err(message) => (
            StatusCode::UNAUTHORIZED,
            Json(DomainError::new("forbidden", message)),
        )
            .into_response(),
    }
}

async fn devices(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    let service = state.service.get();
    let now = alondra_core::now_millis();
    let usage = state.auth.usage().snapshot(now);
    let list: Vec<Value> = state.auth.list().into_iter()
        .map(|d| {
            let origin = service.map_or("mac", |service| service.device_origin(&d.id));
            let used = usage.devices.get(&d.id);
            let last_seen = used.and_then(|used| used.last_used_ms).max(d.last_seen);
            let push_failing = used.is_some_and(|used| used.push_missing.is_some());
            json!({"id": d.id, "name": d.name, "created_at": d.created_at, "last_seen": last_seen,
                "push_failing": push_failing, "origin": origin})
        })
        .collect();
    let expired: Vec<&crate::device_usage::Expired> = usage.expired.iter()
        .filter(|expired| now - expired.expired_at_ms < crate::device_usage::EXPIRED_KEPT_MS)
        .collect();
    Json(json!({"devices": list, "expired": expired})).into_response()
}

/// The report the Metrics screen shows, for `alondra metrics`; local only. Its query is a metrics
/// view key's: `period`, `compare`, `project`, `model` and `effort`.
async fn metrics(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    let Some(query) = alondra_core::metrics::report::Query::parse(query.as_deref().unwrap_or_default()) else {
        return crate::domain_api::domain_error(DomainError::new("invalid_input", "the metrics query names an unknown period, comparison or filter"));
    };
    let domain = state.domain();
    match tokio::task::spawn_blocking(move || domain.metrics_inputs(alondra_core::now_millis())).await {
        Ok(Ok(inputs)) => Json(alondra_core::metrics::report::report(&inputs, &query)).into_response(),
        _ => crate::domain_api::domain_error(DomainError::new("storage_unavailable", "the metrics could not be read")),
    }
}

/// What the move to the Agent SDK does with each task in progress, for `alondra handover
/// --dry-run`: local, and it changes nothing.
async fn sdk_handover(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    let domain = state.domain();
    match tokio::task::spawn_blocking(move || crate::sdk_handover::dry_run(&domain).map(|rows| (domain.sdk().is_some(), rows))).await {
        Ok(Ok((backend, rows))) => Json(json!({"sdk_backend": backend, "tasks": rows})).into_response(),
        _ => crate::domain_api::domain_error(DomainError::new("storage_unavailable", "the tasks in progress could not be read")),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CleanupBody {
    #[serde(default)]
    dry_run: bool,
}

/// Closes the sessions of finished tasks and the ones stopped without a task, for `alondra
/// cleanup`: local, and only a paired device may ask since it closes sessions.
async fn cleanup(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    signed: Option<Extension<Signed>>,
    body: Result<Json<CleanupBody>, JsonRejection>,
) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    if signed_device(&state.home, &signed).is_none() {
        return forbidden("signed user device required");
    }
    let Ok(Json(body)) = body else {
        return crate::domain_api::domain_error(DomainError::new("invalid_input", "invalid cleanup request"));
    };
    let sessions = state.client().sessions();
    match crate::cleanup::run(&state.domain, sessions.as_ref(), &crate::watcher::claude_home(), body.dry_run, alondra_core::now_millis(), crate::cleanup::STOP_WAIT).await {
        Ok(report) => Json(report).into_response(),
        Err((status, error)) => (status, Json(json!({"error": error}))).into_response(),
    }
}

async fn revoke_device(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    RoutePath(id): RoutePath<String>,
) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    let revoked = match tokio::task::spawn_blocking(move || state.revoke_device(&id)).await {
        Ok(Ok(revoked)) => revoked,
        _ => return storage(),
    };
    if revoked {
        Json(json!({"ok": true})).into_response()
    } else {
        crate::domain_api::domain_error(DomainError::new("not_found", "device does not exist"))
    }
}

#[derive(Deserialize)]
struct UploadQuery {
    name: String,
}

/// The file itself is the body. The reply is the `AttachmentRef` a v2 command
/// carries: the stored copy's digest and its reference under this HOME.
async fn upload(
    State(state): State<Arc<DomainDaemon>>,
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
    State(state): State<Arc<DomainDaemon>>,
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
    let user = alondra_core::domain::Actor::User {
        user_id: format!("device:{}", device.device_id),
    };
    match crate::domain_messages::deliver(
        &state.domain,
        &state.home,
        &user,
        &crate::domain_messages::user_name(),
        &id.into(),
        body,
        true,
    )
    .await
    {
        Ok(answer) => Json(answer).into_response(),
        Err(error) => crate::domain_api::domain_error(error),
    }
}

async fn orchestrator_decision(
    State(state): State<Arc<DomainDaemon>>,
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
    let body = body.map(|Json(body)| body);
    let domain = state.domain();
    let home = state.home.clone();
    match tokio::task::spawn_blocking(move || apply_orchestrator_decision(domain, &home, decision, body.as_ref()))
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

/// Shared adapter used by `/agents/{id}/messages` and the views action dispatcher.
pub(crate) async fn send_agent_message(
    domain: &DomainRuntime,
    home: &Path,
    sender: &str,
    agent: &alondra_core::domain::AgentId,
    body: Value,
) -> Value {
    crate::domain_messages::relayed(domain, home, sender, agent.clone(), Some(&body)).await
}

/// Shared adapter used by explicit orchestrator routes and the views action dispatcher.
pub(crate) fn apply_orchestrator_decision(
    domain: Arc<DomainRuntime>,
    home: &Path,
    decision: crate::domain_orchestrator::Decision,
    body: Option<&Value>,
) -> Value {
    crate::domain_orchestrator::decide(&crate::domain_orchestrator::executor(domain, home), decision, body)
}

/// The orchestrator execution calling a tool route: local, with its current bearer.
fn tool_caller(
    state: &DomainDaemon,
    peer: &SocketAddr,
    headers: &HeaderMap,
    what: Option<&str>,
) -> Result<alondra_core::domain::Actor, Response> {
    if let Some(refused) = local_only(peer, headers) {
        return Err(refused);
    }
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    crate::domain_orchestrator_tools::orchestrator(&state.domain, bearer, what)
        .map_err(crate::domain_api::domain_error)
}

fn tool_answer(answer: Result<Value, DomainError>) -> Response {
    match answer {
        Ok(answer) => Json(answer).into_response(),
        Err(error) => crate::domain_api::domain_error(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchOutcomeQuery {
    #[serde(default)] list_runs:bool,
    #[serde(default)] run_id:Option<String>,
    #[serde(default)] after:String,
    #[serde(default)] limit:Option<usize>,
}
async fn orchestrator_outcomes(
    State(state):State<Arc<DomainDaemon>>,ConnectInfo(peer):ConnectInfo<SocketAddr>,headers:HeaderMap,
    body:Result<Json<BatchOutcomeQuery>,JsonRejection>,
) -> Response {
    if let Err(refused)=tool_caller(&state,&peer,&headers,None) {return refused;}
    let Ok(Json(query))=body else {return crate::domain_api::domain_error(DomainError::new("invalid_input","Invalid outcome query"));};
    let bearer=headers.get("authorization").and_then(|header|header.to_str().ok()).and_then(|header|header.strip_prefix("Bearer ")).unwrap_or_default();
    tool_answer(state.domain.read_batch_outcomes(bearer,query.run_id.as_deref(),query.list_runs,&query.after,query.limit.unwrap_or(32)))
}

async fn orchestrator_read(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Json<crate::scoped_reads::Query>, JsonRejection>,
) -> Response {
    if let Err(refused) = tool_caller(&state, &peer, &headers, None) { return refused; }
    let Ok(Json(query)) = body else { return crate::domain_api::domain_error(DomainError::new("invalid_input", "Invalid bounded read request")); };
    let home = state.home.clone();
    let result = state.domain.read_work.run(move || {
        let options = crate::domain_schedule_runner::options_at(&home).map_err(|_| DomainError::new("invalid_input", "Project settings could not be read"))?;
        let root = options.projects_root(&crate::options::user_home());
        crate::scoped_reads::execute(&root, query).map_err(|error| DomainError::new("read_unavailable", format!("{error}")))
    }).await;
    tool_answer(result.unwrap_or_else(|_| Err(DomainError::new("storage_unavailable", "The bounded read stopped before returning a result"))))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillQuery {
    name: String,
    #[serde(default = "skill_start")]
    start_line: usize,
}
fn skill_start() -> usize { 1 }

async fn read_role_skill(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Json<SkillQuery>, JsonRejection>,
) -> Response {
    if let Some(refused) = local_only(&peer, &headers) { return refused; }
    let Some(bearer) = headers.get("authorization").and_then(|value| value.to_str().ok()).and_then(|value| value.strip_prefix("Bearer ")) else {
        return crate::domain_api::domain_error(DomainError::new("forbidden", "A current role capability is required"));
    };
    let principal = match state.domain.resolve_bearer(bearer) { Ok(principal) => principal, Err(_) => return crate::domain_api::domain_error(DomainError::new("forbidden", "A current role capability is required")) };
    let Ok(Json(query)) = body else { return crate::domain_api::domain_error(DomainError::new("invalid_input", "Invalid skill request")); };
    let (root, name) = match (principal, query.name.as_str()) {
        (crate::domain_authority::Principal::Agent { .. }, "alondra") => (crate::plugins::node_in(&state.home), "alondra"),
        (crate::domain_authority::Principal::Orchestrator { .. }, "alondra-orchestrator") => (crate::plugins::orchestrator_in(&state.home), "alondra-orchestrator"),
        _ => return crate::domain_api::domain_error(DomainError::new("forbidden", "Only the current role's installed skill may be read")),
    };
    let Some(root) = root else { return crate::domain_api::domain_error(DomainError::new("not_found", "The role's Alondra integration assets are not installed")); };
    let result = state.domain.read_work.run(move || {
        let request = serde_json::from_value(serde_json::json!({"operation":"read_file","arguments":{"path":format!("skills/{name}/SKILL.md"),"start_line":query.start_line,"max_lines":400}}))
            .map_err(|_| DomainError::new("invalid_input", "Invalid skill page"))?;
        crate::scoped_reads::execute(&root, request).map_err(|error| DomainError::new("read_unavailable", format!("{error}")))
    }).await;
    tool_answer(result.unwrap_or_else(|_| Err(DomainError::new("storage_unavailable", "The role skill could not be read"))))
}

async fn read_task_context(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Json<crate::task_context::Query>, JsonRejection>,
) -> Response {
    if let Some(refused) = local_only(&peer, &headers) { return refused; }
    let Some(bearer) = headers.get("authorization").and_then(|value| value.to_str().ok()).and_then(|value| value.strip_prefix("Bearer ")) else {
        return crate::domain_api::domain_error(DomainError::new("forbidden", "A current task capability is required"));
    };
    let bearer = bearer.to_owned();
    let Ok(Json(query)) = body else { return crate::domain_api::domain_error(DomainError::new("invalid_input", "Invalid task context page")); };
    let domain = state.domain();
    let result = state.domain.read_work.run(move || crate::task_context::read(&domain, &bearer, query)).await;
    tool_answer(result.unwrap_or_else(|_| Err(DomainError::new("storage_unavailable", "The task context could not be read"))))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanModeQuery { task_id: alondra_core::domain::TaskId }

async fn request_plan_mode(
    State(state): State<Arc<DomainDaemon>>,ConnectInfo(peer): ConnectInfo<SocketAddr>,headers: HeaderMap,
    body: Result<Json<PlanModeQuery>,JsonRejection>,
) -> Response {
    if let Some(refused) = local_only(&peer,&headers) { return refused; }
    let Some(bearer) = headers.get("authorization").and_then(|value| value.to_str().ok()).and_then(|value| value.strip_prefix("Bearer ")) else { return forbidden("A current node capability is required"); };
    let Some(operation) = headers.get("x-alondra-mcp-operation").and_then(|value| value.to_str().ok()) else { return crate::domain_api::domain_error(DomainError::new("incompatible_protocol","Update the node's Alondra MCP integration before requesting plan mode")); };
    let Ok(Json(query)) = body else { return crate::domain_api::domain_error(DomainError::new("invalid_input","Invalid plan mode request")); };
    let domain = state.domain(); let bearer = bearer.to_owned(); let operation = operation.to_owned();
    let result = state.domain.command_work.run(move || domain.request_plan_mode(&bearer,&query.task_id,&operation)).await;
    match result {
        Ok(Ok(_)) => Json(json!({"pending":true,"instruction":"End this turn now. Alondra applies plan mode before the next turn. Produce the full checklist plan in native planning mode and wait for Alondra approval before implementation."})).into_response(),
        Ok(Err(crate::domain_store::StoreError::Domain(error))) => crate::domain_api::domain_error(error),
        _ => storage(),
    }
}

async fn tracked_tool(
    state: &Arc<DomainDaemon>, headers: &HeaderMap, kind: &'static str, request: Value,
    work: impl std::future::Future<Output=std::result::Result<Value,DomainError>> + Send + 'static,
) -> std::result::Result<Value,DomainError> {
    let bearer = headers.get("authorization").and_then(|value| value.to_str().ok()).and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| DomainError::new("forbidden","The orchestrator capability is required"))?;
    let identity = headers.get("x-alondra-mcp-operation").and_then(|value| value.to_str().ok());
    crate::domain_runtime::batch_operations::execute(state.domain(),bearer,identity,kind,request,work).await
}

async fn orchestrator_launch(
    State(state): State<Arc<DomainDaemon>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap,
    body: Result<Json<Value>,JsonRejection>,
) -> Response {
    if let Err(refused) = tool_caller(&state,&peer,&headers,None) { return refused; }
    let Ok(Json(request)) = body else { return crate::domain_api::domain_error(DomainError::new("invalid_input","Invalid launch")); };
    let body = match serde_json::from_value(request.clone()) { Ok(body) => body, Err(_) => return crate::domain_api::domain_error(DomainError::new("invalid_input","Invalid launch")) };
    let domain = state.domain();
    let home = state.home.clone();
    let work = async move {
        let choices_domain = Arc::clone(&domain);
        crate::domain_orchestrator_tools::launch_replacing(domain,home,body,move |request| crate::domain_orchestrator_tools::request_choices(&choices_domain,request).ok().flatten()).await
    };
    tool_answer(tracked_tool(&state,&headers,"launch",request,work).await)
}

async fn orchestrator_launch_plan(
    State(state): State<Arc<DomainDaemon>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap,
    body: Result<Json<Value>,JsonRejection>,
) -> Response {
    if let Err(refused) = tool_caller(&state,&peer,&headers,None) { return refused; }
    let Ok(Json(request)) = body else { return crate::domain_api::domain_error(DomainError::new("invalid_input","Invalid launch plan")); };
    let body = match serde_json::from_value(request.clone()) { Ok(body) => body, Err(_) => return crate::domain_api::domain_error(DomainError::new("invalid_input","Invalid launch plan")) };
    let domain = state.domain(); let home = state.home.clone();
    let work = async move { crate::domain_orchestrator_tools::set_launch_plan(&domain,&home,body) };
    tool_answer(tracked_tool(&state,&headers,"set_launch_plan",request,work).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchPlanQuery {
    task_id: alondra_core::domain::TaskId,
}

async fn orchestrator_get_launch_plan(
    State(state): State<Arc<DomainDaemon>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap,
    Query(query): Query<LaunchPlanQuery>,
) -> Response {
    if let Err(refused) = tool_caller(&state, &peer, &headers, None) { return refused; }
    tool_answer(crate::domain_orchestrator_tools::get_launch_plan(&state.domain, &query.task_id))
}

async fn orchestrator_update_launch_plan(
    State(state): State<Arc<DomainDaemon>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Err(refused) = tool_caller(&state, &peer, &headers, None) { return refused; }
    let Ok(Json(request)) = body else { return crate::domain_api::domain_error(DomainError::new("invalid_input", "Invalid launch-plan changes")); };
    let body = match serde_json::from_value(request.clone()) {
        Ok(body) => body,
        Err(error) => return crate::domain_api::domain_error(DomainError::new("invalid_input", format!("Invalid launch-plan changes: {error}"))),
    };
    let domain = state.domain(); let home = state.home.clone();
    let work = async move { crate::domain_orchestrator_tools::update_launch_plan(&domain, &home, body) };
    tool_answer(tracked_tool(&state, &headers, "update_launch_plan", request, work).await)
}

async fn orchestrator_send(
    State(state): State<Arc<DomainDaemon>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap,
    body: Result<Json<Value>,JsonRejection>,
) -> Response {
    let actor = match tool_caller(&state,&peer,&headers,None) { Ok(actor) => actor, Err(refused) => return refused };
    let Ok(Json(request)) = body else { return crate::domain_api::domain_error(DomainError::new("invalid_input","Invalid message")); };
    let body = match serde_json::from_value(request.clone()) { Ok(body) => body, Err(_) => return crate::domain_api::domain_error(DomainError::new("invalid_input","Invalid message")) };
    let domain = state.domain(); let home = state.home.clone();
    let work = async move { crate::domain_orchestrator_tools::send(&domain,&home,&actor,body).await };
    tool_answer(tracked_tool(&state,&headers,"send",request,work).await)
}

/// Lets a sandbox's operator interrupt a probe node, which has no orchestrator to ask.
async fn sandbox_interrupt(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    axum::extract::Path(session): axum::extract::Path<String>,
) -> Response {
    if !state.domain.live.sandbox.load(std::sync::atomic::Ordering::Relaxed) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    match state.domain.live.pushes.interrupt(&session) {
        Ok(()) => Json(json!({"interrupted": session})).into_response(),
        Err(_) => crate::domain_api::domain_error(DomainError::new("session_unreachable", "no connection from that session")),
    }
}

async fn orchestrator_interrupt(
    State(state): State<Arc<DomainDaemon>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap,
    body: Result<Json<Value>,JsonRejection>,
) -> Response {
    if let Err(refused) = tool_caller(&state,&peer,&headers,None) { return refused; }
    let Ok(Json(request)) = body else { return crate::domain_api::domain_error(DomainError::new("invalid_input","Invalid interrupt")); };
    let body = match serde_json::from_value(request.clone()) { Ok(body) => body, Err(_) => return crate::domain_api::domain_error(DomainError::new("invalid_input","Invalid interrupt")) };
    let domain = state.domain();
    let work = async move { crate::domain_orchestrator_tools::interrupt(&domain,body) };
    tool_answer(tracked_tool(&state,&headers,"interrupt",request,work).await)
}

/// A Schedule the orchestrator creates for the user, materialized as the user's own drafts are.
async fn orchestrator_schedule(
    State(state): State<Arc<DomainDaemon>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap,
    body: Result<Json<Value>,JsonRejection>,
) -> Response {
    if let Err(refused) = tool_caller(&state,&peer,&headers,None) { return refused; }
    let Ok(Json(request)) = body else { return crate::domain_api::domain_error(DomainError::new("invalid_input","Invalid schedule draft")); };
    let body = request.clone(); let home = state.home.clone(); let domain = state.domain();
    let work = async move {
        let value = tokio::task::spawn_blocking(move || crate::domain_schedule_api::orchestrator_mutation(&domain,&home,"POST","/schedules",Some(&body),None,|| Ok(()))).await
            .map_err(|_| DomainError::new("operation_uncertain","Schedule creation stopped before returning its result"))?;
        match value["status"].as_u64() {
            Some(200..=299) => Ok(value),
            _ => Err(serde_json::from_value(value["body"].clone()).unwrap_or_else(|_| DomainError::new("storage_unavailable","The schedule result could not be read"))),
        }
    };
    match tracked_tool(&state,&headers,"schedule_create",request,work).await {
        Ok(value) => crate::domain_schedule_api::response(value),
        Err(error) => crate::domain_api::domain_error(error),
    }
}

fn gauges_json(state:&DomainDaemon)->(Value,Value) {
    match &*state.gauges.lock_or_recover("the machine gauges") {
        Some((machine,usage,_))=>(json!(machine),json!(usage)),
        None=>(Value::Null,Value::Null),
    }
}

async fn orchestrator_tasks_in_progress(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Err(refused) = tool_caller(&state, &peer, &headers, None) {
        return refused;
    }
    let (machine, usage) = gauges_json(&state);
    tool_answer(crate::domain_orchestrator_reads::tasks_in_progress(
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
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<NodeQuery>,
) -> Response {
    if let Err(refused) = tool_caller(&state, &peer, &headers, None) {
        return refused;
    }
    tool_answer(crate::domain_orchestrator_reads::node(
        &state.domain,
        &query.node,
    ))
}

#[derive(Deserialize)]
struct WaitingBody {
    /// Missing from a CLI older than the field.
    #[serde(default)]
    resource: Option<alondra_core::machine_incident::MachineResource>,
    reason: String,
}

/// An Agent declares it is stopped waiting for the machine, as 8ccd131's `wait_for_machine` did:
/// its live state says so until its next turn, the machine watch tells the user, and the
/// admission gate brings it back in its turn.
async fn agent_waiting_machine(
    State(state): State<Arc<DomainDaemon>>,
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
    let live = &state.domain.live;
    let turn = live.node(&session_id).map_or(0, |node| node.turn);
    let now = alondra_core::now_millis();
    live.machine_watch.declare(&session_id, body.resource, &body.reason, turn, now);
    live.gate.hold_back(&state.domain, &session_id, crate::pauses::Reason::Declared { reason: body.reason.clone() }, now);
    live.apply(&alondra_core::Event::WaitingMachine {
        session_id,
        reason: body.reason,
    });
    Json(json!({"ok": true})).into_response()
}

#[derive(Deserialize)]
struct OpenCardsQuery {
    task_id: Option<String>,
}

/// The caller's own open cards, so it replaces one instead of sending the same card again: a
/// node's on its Task, the orchestrator's on one Task or everywhere.
async fn open_cards(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<OpenCardsQuery>,
) -> Response {
    if let Some(refused) = local_only(&peer, &headers) {
        return refused;
    }
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let (reader, bound) = match bearer.map(|token| state.domain.resolve_bearer(token)) {
        Some(Ok(crate::domain_authority::Principal::Agent { session_id, task_binding, .. })) => {
            (alondra_core::domain::Actor::Agent { agent_id: session_id.into() }, task_binding)
        }
        Some(Ok(crate::domain_authority::Principal::Orchestrator { generation, .. })) => {
            (alondra_core::domain::Actor::Orchestrator { generation }, None)
        }
        _ => return forbidden("an Agent's or the orchestrator's capability is required"),
    };
    let task = match (bound, query.task_id) {
        (Some(bound), Some(asked)) if bound != asked => return forbidden("this node is bound to another task"),
        (bound, asked) => asked.or(bound).map(alondra_core::domain::TaskId),
    };
    match state.domain.state() {
        Ok(domain) => Json(json!({"cards": domain.open_card_rows(&reader, task.as_ref())})).into_response(),
        Err(error) => crate::domain_api::domain_error(DomainError::new("unavailable", error.to_string())),
    }
}

/// Machine capacity and the subscription's usage, as the `/daemon` gauges hold them.
async fn orchestrator_machine(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Err(refused) = tool_caller(&state, &peer, &headers, None) {
        return refused;
    }
    let (machine, usage) = match &*state.gauges.lock_or_recover("the machine gauges") {
        Some((machine, usage, _)) => (json!(machine), json!(usage)),
        None => (Value::Null, Value::Null),
    };
    let domain = state.domain();
    let tasks = match tokio::task::spawn_blocking(move || domain.metrics_inputs(alondra_core::now_millis())).await {
        Ok(Ok(inputs)) => inputs.tasks,
        _ => vec![],
    };
    let read = state.domain.read();
    let (available, evidence) = available_models(read.state(), &tasks, alondra_core::now_millis());
    Json(json!({"available_models": available, "model_evidence": evidence, "machine": machine, "usage": usage, "account_order":read.state().account_order,
        "model_profiles":read.state().model_profiles,"model_catalogs":read.state().model_catalogs})).into_response()
}

/// The models the orchestrator chooses from, as `model_table` writes them, and the public evidence
/// of each one's sheet. Until the daemon records the accounts, every catalog's account counts.
pub(crate) fn available_models(state: &alondra_core::domain::DomainState, tasks: &[alondra_core::metrics::report::TaskRow], now: alondra_core::Millis)
    -> (Option<String>, std::collections::BTreeMap<String, String>) {
    use alondra_core::accounts::{AccountKey, AccountState};
    use alondra_core::model_table::AccountRoom;
    let today = crate::node_reports::local_day(now);
    let rooms: Vec<AccountRoom> = if state.accounts.is_empty() {
        state.model_catalogs.iter().map(|catalog| AccountRoom { account: catalog.account.clone(), name: catalog.account.account.as_str().to_owned(), margin: None }).collect()
    } else {
        state.accounts.iter().filter(|account| account.state != AccountState::Disabled).map(|account| {
            let usage = state.account_usage.iter().find(|usage| usage.provider == account.provider && usage.account == account.id);
            AccountRoom { account: AccountKey::new(account.provider, account.id.clone()), name: account.name.clone(),
                margin: alondra_core::usage::own_margin(account.state, usage, state.account_reserve, now, &today) }
        }).collect()
    };
    let table = alondra_core::model_table::available_models(&state.model_catalogs, &rooms, tasks);
    let evidence = state.model_catalogs.iter().filter(|catalog| rooms.iter().any(|room| room.account == catalog.account))
        .flat_map(|catalog| catalog.models.iter().filter(|entry| !entry.hidden).filter_map(|entry| {
            let sheet = alondra_core::model_sheets::sheet_for(catalog.account.provider, &entry.model)?;
            Some((format!("{}/{}", catalog.account.provider.as_str(), entry.model), sheet.evidence.clone()))
        }))
        .collect();
    (table, evidence)
}

const LOG_LINES: usize = 200;
const MAX_LOG_LINES: usize = 1000;
const MAX_LOG_BYTES: u64 = 512 * 1024;

#[derive(Deserialize)]
struct LogQuery {
    lines: Option<usize>,
}

/// The end of the daemon's own log. The orchestrator reads it through this route because its
/// file tools never reach Alondra's home, which holds keys and the nodes' capabilities.
async fn orchestrator_log(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<LogQuery>,
) -> Response {
    if let Err(refused) = tool_caller(&state, &peer, &headers, None) {
        return refused;
    }
    let lines = query.lines.unwrap_or(LOG_LINES).clamp(1, MAX_LOG_LINES);
    match alondra_core::daemon_log::tail(&state.home, lines, MAX_LOG_BYTES) {
        Ok(text) => Json(json!({"log": text.join("\n")})).into_response(),
        Err(error) => crate::domain_api::domain_error(DomainError::new(
            "storage_unavailable",
            format!("cannot read alondrad.log: {error}"),
        )),
    }
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
    State(state): State<Arc<DomainDaemon>>,
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

    #[tokio::test]
    async fn the_device_list_shows_the_recorded_use_and_the_devices_unpaired_lately() {
        use crate::device_usage::{EXPIRED_KEPT_MS, Expired, ExpiryReason};
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*home).unwrap();
        drop(DomainRuntime::open(&home).unwrap());
        let state = DomainDaemon::open(&home).unwrap();
        let key = STANDARD.encode(SigningKey::from_slice(&[7u8; 32]).unwrap().verifying_key().to_sec1_bytes());
        state.auth.pair(&state.auth.new_pairing_code(), "iPhone", &key).unwrap();
        let id = state.auth.list()[0].id.clone();
        let now = alondra_core::now_millis();
        let usage = state.auth.usage();
        usage.touch(&id, now - 60_000, false);
        usage.push_not_found(&id, now);
        let expired = |id: &str, at| Expired { id: id.into(), name: "Old iPhone".into(), reason: ExpiryReason::Unused, days: Some(30.0), expired_at_ms: at };
        usage.expire(expired("phone-recent", now - 3_600_000));
        usage.expire(expired("phone-forgotten", now - EXPIRED_KEPT_MS - 1));
        let response = devices(State(state), ConnectInfo("127.0.0.1:4000".parse::<SocketAddr>().unwrap()), HeaderMap::new()).await;
        let body: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap();
        assert_eq!(body["devices"][0]["last_seen"], now - 60_000);
        assert_eq!(body["devices"][0]["push_failing"], true);
        assert_eq!(body["expired"], json!([{"id": "phone-recent", "name": "Old iPhone", "reason": "unused", "days": 30.0, "expired_at_ms": now - 3_600_000}]));
    }

    #[tokio::test]
    async fn cleanup_needs_a_local_paired_device() {
        let scratch_directory = crate::test_support::Directory::new();
        use crate::node_close::{Answer, Sessions};
        struct Untouchable(std::sync::atomic::AtomicUsize);
        impl Sessions for Untouchable {
            fn claude<'a>(&'a self, _: Vec<String>) -> Answer<'a, Result<crate::node_close::Ran, String>> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok(crate::node_close::Ran::ok("[]")) })
            }
            fn leftovers<'a>(&'a self, _: &'a str) -> Answer<'a, Option<String>> {
                Box::pin(async { None })
            }
        }
        let home = scratch_directory.parent().unwrap().join(format!("alondra-cleanup-route-{}", crate::auth::random_hex(8)));
        std::fs::create_dir_all(&home).unwrap();
        drop(DomainRuntime::open(&home).unwrap());
        let state = DomainDaemon::open(&home).unwrap();
        let sessions = Arc::new(Untouchable(Default::default()));
        state.client().use_sessions(sessions.clone());
        let ask = |peer: &str| cleanup(
            State(state.clone()),
            ConnectInfo(peer.parse::<SocketAddr>().unwrap()),
            HeaderMap::new(),
            None,
            Ok(Json(CleanupBody { dry_run: true })),
        );
        let remote = ask("203.0.113.9:4000").await;
        assert_eq!(remote.status(), StatusCode::FORBIDDEN, "a request from another machine");
        let unsigned = ask("127.0.0.1:4000").await;
        assert_eq!(unsigned.status(), StatusCode::FORBIDDEN, "a local request without a paired device");
        assert_eq!(sessions.0.load(std::sync::atomic::Ordering::SeqCst), 0, "nothing was asked of claude");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn the_old_code_pairing_answers_only_on_loopback() {
        let scratch_directory = crate::test_support::Directory::new();
        use axum::extract::connect_info::MockConnectInfo;
        let home = scratch_directory.parent().unwrap().join(format!("alondra-pair-loopback-{}", crate::auth::random_hex(8)));
        std::fs::create_dir_all(&home).unwrap();
        drop(DomainRuntime::open(&home).unwrap());
        for (peer, refused) in [("192.168.1.5:5000", true), ("127.0.0.1:5000", false)] {
            let state = DomainDaemon::open(&home).unwrap();
            let auth = state.auth();
            let app = router(state)
                .layer(axum::middleware::from_fn_with_state(auth, crate::auth::guard))
                .layer(MockConnectInfo(peer.parse::<SocketAddr>().unwrap()));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let body = reqwest::Client::new()
                .post(format!("{base}/pair"))
                .header("content-type", "application/json")
                .body(r#"{"code":"nope","device_name":"x","public_key":"y"}"#)
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            assert_eq!(!body.contains("pairing code"), refused, "{peer}: {body}");
            server.abort();
        }
    }

    #[tokio::test]
    async fn pairing_answers_only_a_loopback_host_without_a_browser_origin() {
        use axum::extract::connect_info::MockConnectInfo;
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        drop(DomainRuntime::open(&home).unwrap());
        let state = DomainDaemon::open(&home).unwrap();
        let auth = state.auth();
        let app = router(state)
            .layer(axum::middleware::from_fn_with_state(auth.clone(), crate::auth::guard))
            .layer(MockConnectInfo("127.0.0.1:5000".parse::<SocketAddr>().unwrap()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();
        let paired_keys = || std::fs::read_to_string(home.join("devices.json")).unwrap_or_default();
        let browser_headers: [&[(&str, &str)]; 3] = [
            &[("host", "evil.example:47800"), ("origin", "http://evil.example:47800")],
            &[("host", "127.0.0.1:47800"), ("origin", "http://evil.example:47800")],
            &[("host", "evil.example:47800")],
        ];
        for (seed, extra) in browser_headers.into_iter().enumerate() {
            let with = |mut request: reqwest::RequestBuilder| {
                for (name, value) in extra {
                    request = request.header(*name, *value);
                }
                request
            };
            let invite = with(client.post(format!("{base}/pairing"))).send().await.unwrap();
            assert_eq!(invite.status(), StatusCode::FORBIDDEN, "{extra:?}: a code for a browser page");
            let key = STANDARD.encode(SigningKey::from_slice(&[31 + seed as u8; 32]).unwrap().verifying_key().to_sec1_bytes());
            let paired = with(client.post(format!("{base}/pair")))
                .json(&json!({"code": auth.new_pairing_code(), "device_name": "page", "public_key": key}))
                .send()
                .await
                .unwrap();
            assert_eq!(paired.status(), StatusCode::FORBIDDEN, "{extra:?}: a browser page pairs its key");
            assert!(!paired_keys().contains(&key), "{extra:?}: the page's key was saved");
        }
        let invite: Value = client.post(format!("{base}/pairing")).send().await.unwrap().json().await.unwrap();
        let key = STANDARD.encode(SigningKey::from_slice(&[41u8; 32]).unwrap().verifying_key().to_sec1_bytes());
        let paired = client
            .post(format!("{base}/pair"))
            .json(&json!({"code": invite["code"], "device_name": "Mac", "public_key": key}))
            .send()
            .await
            .unwrap();
        assert_eq!(paired.status(), StatusCode::OK, "the Mac's own client pairs itself");
        assert!(paired_keys().contains(&key));
        server.abort();
    }

    #[test]
    fn pairing_decisions_need_loopback_json_and_no_browser_origin() {
        let loopback: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let lan: SocketAddr = "192.168.1.5:5000".parse().unwrap();
        let headers = |pairs: &[(&'static str, &'static str)]| {
            let mut map = HeaderMap::new();
            for (name, value) in pairs {
                map.insert(*name, value.parse().unwrap());
            }
            map
        };
        let good = headers(&[("host", "127.0.0.1:47800"), ("content-type", "application/json")]);
        assert!(local_decision_only(&loopback, &good).is_none());
        assert!(local_decision_only(&loopback, &headers(&[("host", "localhost:47800"), ("content-type", "application/json; charset=utf-8")])).is_none());
        assert!(local_decision_only(&lan, &good).is_some());
        assert!(local_decision_only(&loopback, &headers(&[("host", "127.0.0.1:47800"), ("content-type", "application/json"), ("origin", "https://evil.example")])).is_some());
        assert!(local_decision_only(&loopback, &headers(&[("host", "127.0.0.1:47800"), ("content-type", "text/plain")])).is_some());
        assert!(local_decision_only(&loopback, &headers(&[("host", "evil.example:47800"), ("content-type", "application/json")])).is_some());
    }

    #[tokio::test]
    async fn pair_approves_a_service_phone_with_the_macs_signature() {
        let scratch_directory = crate::test_support::Directory::new();
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-service-approval-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir_all(home.join("service-keys")).unwrap();
        let mac = SigningKey::from_slice(&[7u8; 32]).unwrap();
        std::fs::write(home.join("service-keys/public.json"), json!({"mac_id": "mac-test",
            "signing_key": URL_SAFE_NO_PAD.encode(mac.verifying_key().to_sec1_bytes())}).to_string()).unwrap();
        drop(DomainRuntime::open(&home).unwrap());
        let state = DomainDaemon::open(&home).unwrap();
        let auth = state.auth();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(state).layer(axum::middleware::from_fn_with_state(auth.clone(), crate::auth::guard));
        let server = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
        });
        let client = reqwest::Client::new();
        let boot: Value = client.get(format!("{base}/service/bootstrap")).send().await.unwrap().json().await.unwrap();
        let epoch: [u8; 16] = URL_SAFE_NO_PAD.decode(boot["epoch"].as_str().unwrap()).unwrap().try_into().unwrap();
        let phone = SigningKey::from_slice(&[5u8; 32]).unwrap();
        let body = json!({"device_id": "phone-a", "name": "iPhone",
            "signing_key": URL_SAFE_NO_PAD.encode(phone.verifying_key().to_sec1_bytes())}).to_string();
        let now = alondra_core::now_millis() as u64;
        let nonce = [3u8; 16];
        let signature = alondra_e2e::sign_http_challenge(&mac, "mac-test", "POST",
            "/service/approve-device", &epoch, body.as_bytes(), now, &nonce).unwrap();
        let approved = client.post(format!("{base}/service/approve-device"))
            .header("x-device-id", "mac-test")
            .header("x-relay-epoch", URL_SAFE_NO_PAD.encode(epoch))
            .header("x-device-timestamp-ms", now.to_string())
            .header("x-device-nonce", URL_SAFE_NO_PAD.encode(nonce))
            .header("x-device-signature", signature)
            .body(body).send().await.unwrap();
        assert_eq!(approved.status(), StatusCode::NO_CONTENT);
        assert!(auth.list().iter().any(|device| device.id == "phone-a"));
        server.abort();
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[tokio::test]
    async fn approving_and_revoking_a_phone_reloads_the_running_service() {
        let scratch_directory = crate::test_support::Directory::new();
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use std::os::unix::fs::PermissionsExt;
        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-service-reload-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir_all(home.join("service-keys")).unwrap();
        let mac = SigningKey::from_slice(&[7u8; 32]).unwrap();
        std::fs::write(home.join("service-keys/public.json"), json!({"mac_id": "mac-test",
            "signing_key": URL_SAFE_NO_PAD.encode(mac.verifying_key().to_sec1_bytes())}).to_string()).unwrap();
        for (name, secret) in [("service-keys/mac-signing.key", [1u8; 32]), ("service-keys/mac-agreement.key", [2u8; 32])] {
            let path = home.join(name);
            std::fs::write(&path, URL_SAFE_NO_PAD.encode(secret)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let config = |devices: &str| format!(
            "relay_url = \"http://127.0.0.1:1/\"\nmac_id = \"mac-test\"\nowner_sub = \"owner\"\nsigning_key_path = \"service-keys/mac-signing.key\"\nagreement_key_path = \"service-keys/mac-agreement.key\"\n{devices}"
        );
        std::fs::write(home.join("service.toml"), config("devices = []\n")).unwrap();
        drop(DomainRuntime::open(&home).unwrap());
        let state = DomainDaemon::open(&home).unwrap();
        let auth = state.auth();
        let service = Arc::new(crate::service::Service::open(&home, 0, auth.clone()).unwrap().unwrap());
        state.attach_service(service.clone());
        assert!(!service.has_device("phone-a"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(state).layer(axum::middleware::from_fn_with_state(auth.clone(), crate::auth::guard));
        let server = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
        });
        let client = reqwest::Client::new();
        let boot: Value = client.get(format!("{base}/service/bootstrap")).send().await.unwrap().json().await.unwrap();
        let epoch: [u8; 16] = URL_SAFE_NO_PAD.decode(boot["epoch"].as_str().unwrap()).unwrap().try_into().unwrap();
        let phone = SigningKey::from_slice(&[5u8; 32]).unwrap();
        let signing_wire = URL_SAFE_NO_PAD.encode(phone.verifying_key().to_sec1_bytes());
        let agreement_wire = URL_SAFE_NO_PAD.encode(p256::SecretKey::from_slice(&[6u8; 32]).unwrap().public_key().to_sec1_bytes());
        std::fs::write(home.join("service.toml"), config(&format!(
            "[[devices]]\ndevice_id = \"phone-a\"\nsigning_key = \"{signing_wire}\"\nagreement_key = \"{agreement_wire}\"\n"
        ))).unwrap();
        let body = json!({"device_id": "phone-a", "name": "iPhone", "signing_key": signing_wire}).to_string();
        let now = alondra_core::now_millis() as u64;
        let nonce = [3u8; 16];
        let signature = alondra_e2e::sign_http_challenge(&mac, "mac-test", "POST",
            "/service/approve-device", &epoch, body.as_bytes(), now, &nonce).unwrap();
        let approved = client.post(format!("{base}/service/approve-device"))
            .header("x-device-id", "mac-test")
            .header("x-relay-epoch", URL_SAFE_NO_PAD.encode(epoch))
            .header("x-device-timestamp-ms", now.to_string())
            .header("x-device-nonce", URL_SAFE_NO_PAD.encode(nonce))
            .header("x-device-signature", signature)
            .body(body).send().await.unwrap();
        assert_eq!(approved.status(), StatusCode::NO_CONTENT);
        assert!(service.has_device("phone-a"), "approval reaches the running service");

        std::fs::write(home.join("service.toml"), config("devices = []\n")).unwrap();
        let revoked = client.delete(format!("{base}/devices/phone-a")).send().await.unwrap();
        assert_eq!(revoked.status(), StatusCode::OK);
        assert!(!service.has_device("phone-a"), "revocation reaches the running service");

        let body = json!({"device_id": "phone-a", "name": "iPhone", "signing_key": signing_wire}).to_string();
        let now = alondra_core::now_millis() as u64;
        let nonce = [4u8; 16];
        let signature = alondra_e2e::sign_http_challenge(&mac, "mac-test", "POST",
            "/service/approve-device", &epoch, body.as_bytes(), now, &nonce).unwrap();
        let again = client.post(format!("{base}/service/approve-device"))
            .header("x-device-id", "mac-test")
            .header("x-relay-epoch", URL_SAFE_NO_PAD.encode(epoch))
            .header("x-device-timestamp-ms", now.to_string())
            .header("x-device-nonce", URL_SAFE_NO_PAD.encode(nonce))
            .header("x-device-signature", signature)
            .body(body).send().await.unwrap();
        assert_eq!(again.status(), StatusCode::GONE, "a revoked device ID is never approved again");
        assert!(!auth.list().iter().any(|device| device.id == "phone-a"));
        assert!(!service.has_device("phone-a"));
        server.abort();
        drop(service);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn bootstrap_refuses_an_unsupported_schema_and_preserves_the_domain() {
        use alondra_core::domain::{Actor, Command, CommandEnvelope, PROTOCOL_VERSION};
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-domain-bootstrap-schema-{}", crate::auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        let before = domain.state().unwrap();
        domain.execute(&Actor::User { user_id: "bootstrap-test".into() }, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: before.data_epoch.clone(),
            command_id: "preserved-request".into(), expected_revision: before.revision,
            command: Command::CreateRequest { request_id: "preserved".into(),
                body: "Existing work".into(), auto_merge: false, maintenance: false, message: false },
        }, None, || Ok(())).unwrap();
        let expected = domain.state().unwrap();
        drop(domain);
        let path = home.join("domain.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);
        let bytes = std::fs::read(&path).unwrap();
        let error = match DomainDaemon::open(&home) {
            Ok(_) => panic!("bootstrap must refuse an unsupported domain schema"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("unsupported domain database schema"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let conn = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(conn.query_row("SELECT body FROM requests", [], |row|
            row.get::<_, String>(0)).unwrap(), "Existing work");
        assert_eq!(conn.pragma_query_value(None, "user_version", |row|
            row.get::<_, i64>(0)).unwrap(), 99);
        conn.pragma_update(None, "user_version", crate::domain_store::SCHEMA_VERSION).unwrap();
        drop(conn);
        let state = DomainDaemon::open(&home).unwrap();
        assert_eq!(state.domain().state().unwrap(), expected);
        drop(state);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn views_and_changes_answer_while_a_writer_holds_the_domain_store() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        drop(DomainRuntime::open(&home).unwrap());
        let auth = Auth::open(&home).unwrap();
        let key = SigningKey::from_slice(&[9u8; 32]).unwrap();
        let pairing = auth.new_pairing_code();
        let device = auth.pair(&pairing, "test-device", &STANDARD.encode(key.verifying_key().to_sec1_bytes())).unwrap();
        let state = DomainDaemon::open(&home).unwrap();
        let domain = state.domain();
        let app = router(state.clone()).layer(axum::middleware::from_fn_with_state(state.auth(), crate::auth::guard));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
        });
        let client = reqwest::Client::new();
        let signed = |path: &str| {
            let timestamp = alondra_core::now_millis().to_string();
            let signature: Signature = key.sign(crate::auth::signed_message("GET", path, &timestamp, b"").as_bytes());
            client.get(format!("{base}{path}"))
                .header("x-alondra-device", &device.id)
                .header("x-alondra-timestamp", timestamp)
                .header("x-alondra-signature", STANDARD.encode(signature.to_der().as_bytes()))
        };
        let writer = domain.store.lock().unwrap();
        let mut answers = Vec::new();
        for path in [
            "/views/home?protocol=1&client=tui&lang=en",
            "/views/tasks?protocol=1&client=tui&lang=en",
            "/changes?protocol=1&client=tui&lang=en&wait=0&views=home%40v0000000000000000",
        ] {
            // Only a hang guard: a read that waits for the writer never answers while it is held.
            answers.push((path, tokio::time::timeout(std::time::Duration::from_secs(60), signed(path).send()).await));
        }
        drop(writer);
        for (path, answer) in answers {
            let response = answer.unwrap_or_else(|_| panic!("{path} waited for the writer")).unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let body: serde_json::Value = response.json().await.unwrap();
            assert!(body.get("error").is_none(), "{path}: {body}");
        }
        server.abort();
    }

    /// Opt-in measurement, never an ordinary test: the views' latency while commits hold the
    /// writer 200 ms of every 300 ms, as on a Mac whose disk is saturated. Run it with
    /// `--test_arg=--ignored --test_filter=views_latency_while_commits_hold_the_writer`.
    #[ignore]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn views_latency_while_commits_hold_the_writer() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        crate::domain_store::load_bench::seed(&mut crate::domain_store::DomainStore::open(
            &crate::domain_database_migration::active_path(&home)).unwrap(), 20);
        let auth = Auth::open(&home).unwrap();
        let key = SigningKey::from_slice(&[9u8; 32]).unwrap();
        let pairing = auth.new_pairing_code();
        let device = auth.pair(&pairing, "test-device", &STANDARD.encode(key.verifying_key().to_sec1_bytes())).unwrap();
        let state = DomainDaemon::open(&home).unwrap();
        let domain = state.domain();
        let app = router(state.clone()).layer(axum::middleware::from_fn_with_state(state.auth(), crate::auth::guard));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
        });
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let committer = {
            let (domain, stop) = (domain.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut attempt = 0u8;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    {
                        let mut store = domain.store.lock().unwrap();
                        attempt = attempt.wrapping_add(1).max(1);
                        store.set_start_retry(&"task-110".into(), Some((attempt, 0))).unwrap();
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            })
        };
        let client = reqwest::Client::new();
        let mut timings = Vec::new();
        for round in 0..200 {
            let path = if round % 2 == 0 { "/views/home?protocol=1&client=tui&lang=en" } else { "/views/tasks?protocol=1&client=tui&lang=en" };
            let timestamp = alondra_core::now_millis().to_string();
            let signature: Signature = key.sign(crate::auth::signed_message("GET", path, &timestamp, b"").as_bytes());
            let started = std::time::Instant::now();
            let response = client.get(format!("{base}{path}"))
                .header("x-alondra-device", &device.id)
                .header("x-alondra-timestamp", timestamp)
                .header("x-alondra-signature", STANDARD.encode(signature.to_der().as_bytes()))
                .send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            response.bytes().await.unwrap();
            timings.push(started.elapsed().as_secs_f64() * 1_000.0);
            tokio::time::sleep(std::time::Duration::from_millis(37)).await;
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        committer.join().unwrap();
        server.abort();
        timings.sort_by(f64::total_cmp);
        let at = |share: f64| timings[((timings.len() as f64 * share) as usize).min(timings.len() - 1)];
        println!("views while commits hold the writer: n={} p50={:.1} ms p90={:.1} ms p99={:.1} ms max={:.1} ms",
            timings.len(), at(0.5), at(0.9), at(0.99), timings[timings.len() - 1]);
    }

    #[tokio::test]
    async fn standalone_router_reads_real_domain_and_has_no_legacy_routes() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-bootstrap-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        let epoch = domain.state().unwrap().data_epoch.0.clone();
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
        let state = DomainDaemon::open(&home).unwrap();
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
        for path in ["/metrics", "/metrics?period=all&compare=none"] {
            let report: alondra_core::metrics::report::MetricsReport =
                client.get(format!("{base}{path}")).send().await.unwrap().json().await.unwrap();
            assert!(report.by_model.is_empty() && report.figures.sums.tasks == 0, "{path}");
        }
        let refused = client.get(format!("{base}/metrics?period=1y")).send().await.unwrap();
        assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY, "an unknown period is refused, not read as the default");
        let unsigned = client.post(format!("{base}/cleanup")).json(&json!({"dry_run": true})).send().await.unwrap();
        assert_eq!(unsigned.status(), StatusCode::FORBIDDEN, "cleanup needs a paired device: it closes sessions");
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
        let path = "/views/home?protocol=1&client=tui&lang=en";
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
        assert_eq!(body["protocol"], 1);
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
            ("/views/home?protocol=1&client=tui&lang=en", StatusCode::OK),
            ("/delivery-attempts?after=0", StatusCode::OK),
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
        for path in ["/tasks/missing/actions"] {
            assert_eq!(client.get(format!("{base}{path}")).send().await.unwrap().status(),
                StatusCode::NOT_FOUND, "retired {path}");
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
            StatusCode::NOT_FOUND
        );
        let response = client
            .get(format!("{base}{merge_path}"))
            .header("authorization", format!("Bearer {bearer}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
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
        assert_eq!(
            client
                .get(format!("{base}/orchestrator/log"))
                .bearer_auth(&bearer)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN,
            "only the orchestrator reads the daemon log"
        );
        let lines: Vec<String> = (1..=300).map(|n| format!("line {n}")).collect();
        std::fs::write(home.join("alondrad.log"), lines.join("\n")).unwrap();
        let log = client
            .get(format!("{base}/orchestrator/log?lines=2"))
            .bearer_auth(orchestrator.expose_for_child_environment())
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        assert_eq!(log["log"], "line 299\nline 300");
        let log = client
            .get(format!("{base}/orchestrator/log"))
            .bearer_auth(orchestrator.expose_for_child_environment())
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        assert_eq!(log["log"].as_str().unwrap().lines().count(), 200);
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
                .get(format!("{base}/orchestrator/tasks_in_progress"))
                .bearer_auth(&bearer)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let plan_path = "/orchestrator/launch-plan?task_id=missing";
        for token in ["invalid", bearer.as_str()] {
            assert_eq!(client.get(format!("{base}{plan_path}")).bearer_auth(token).send().await.unwrap().status(), StatusCode::FORBIDDEN);
            assert_eq!(client.post(format!("{base}/orchestrator/launch-plan/update")).bearer_auth(token)
                .json(&json!({"task_id": "missing", "changes": {"model": "sonnet"}})).send().await.unwrap().status(), StatusCode::FORBIDDEN);
        }
        assert_eq!(orchestrator_get(plan_path).await.unwrap().status(), StatusCode::NOT_FOUND);
        let missing_update = client.post(format!("{base}/orchestrator/launch-plan/update"))
            .bearer_auth(orchestrator.expose_for_child_environment())
            .json(&json!({"task_id": "missing", "changes": {"model": "sonnet"}})).send().await.unwrap();
        assert_eq!(missing_update.status(), StatusCode::NOT_FOUND);
        use alondra_core::domain::{Actor, Command, CommandEnvelope, LaunchPlan, PROTOCOL_VERSION};
        let execute = |actor, command| {
            let state = domain.state().unwrap();
            domain.execute_fixture(&actor, CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
                command_id: crate::auth::random_hex(8).into(), expected_revision: state.revision, command }).unwrap();
        };
        execute(Actor::User { user_id: "user".into() }, Command::CreateRequest { request_id: "plan-read".into(), body: "Review stored work".into(),
            auto_merge: false, maintenance: false, message: false });
        let plan = LaunchPlan { mission: Some("The saved mission\n\nAdditional context stays here.".into()),
            model: Some("sonnet".into()), reason: Some("Implementation".into()), ..LaunchPlan::new("stored-node", home.to_str().unwrap()) };
        execute(Actor::Orchestrator { generation: domain.state().unwrap().orchestrator_generation }, Command::CreateTask {
            task_id: "plan-read".into(), request_id: "plan-read".into(), title: "Stored work".into(), agent_id: None, review_prs: false,
            dependencies: Default::default(), stack: None, launch: Some(plan.clone()), plan_items: None, continues: None });
        let stored = orchestrator_get("/orchestrator/launch-plan?task_id=plan-read").await.unwrap();
        assert_eq!(stored.status(), StatusCode::OK);
        assert_eq!(stored.json::<Value>().await.unwrap()["launch"], json!(plan));
        let updated = client.post(format!("{base}/orchestrator/launch-plan/update"))
            .bearer_auth(orchestrator.expose_for_child_environment())
            .json(&json!({"task_id": "plan-read", "changes": {"reason": "Better measured fit"}})).send().await.unwrap();
        assert_eq!(updated.status(), StatusCode::OK);
        let stored: Value = orchestrator_get("/orchestrator/launch-plan?task_id=plan-read").await.unwrap().json().await.unwrap();
        assert_eq!(stored["launch"]["mission"], json!(plan.mission));
        assert_eq!(stored["launch"]["name"], "stored-node");
        assert_eq!(stored["launch"]["reason"], "Better measured fit");
        let tasks_in_progress = orchestrator_get("/orchestrator/tasks_in_progress").await.unwrap();
        assert_eq!(tasks_in_progress.status(), StatusCode::OK);
        assert!(tasks_in_progress.json::<serde_json::Value>().await.unwrap()["nodes"].is_array());
        assert_eq!(
            orchestrator_get("/orchestrator/node?node=nobody")
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        domain.live.observe(&json!({"session_id": "test-agent", "hook_event_name": "UserPromptSubmit", "cwd": "/tmp", "prompt": "go"}));
        let waiting = json!({"resource": "build_queue", "reason": "Bazel remote cache token expired, build queue stuck"});
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
            StatusCode::OK,
            "the words of the reason never refuse a wait"
        );
        let declared = |session: &str| domain.live.machine_watch.declared()[session].resource;
        assert_eq!(declared("test-agent"), Some(alondra_core::machine_incident::MachineResource::BuildQueue));
        let post = |body: Value| client.post(format!("{base}/agents/waiting-machine")).bearer_auth(&bearer).json(&body).send();
        assert_eq!(post(json!({"reason": "disk full"})).await.unwrap().status(), StatusCode::OK, "a CLI older than resource");
        assert_eq!(declared("test-agent"), None);
        assert!(post(json!({"resource": "usage", "reason": "weekly limit"})).await.unwrap().status().is_client_error(),
            "no resource names the subscription's usage");
        let node = domain.live.node("test-agent").unwrap();
        assert_eq!(
            node.turn_signal,
            Some(alondra_core::TurnSignal::WaitingMachine),
            "{node:?}"
        );
        domain.revoke_agent("test-agent").unwrap();
        // The removed GET merge-authorizations route cannot prove revocation.
        // Check it on the retained authenticated capability route below.
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
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-local-user-{}",
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
            .data_epoch.clone()
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
        let state = DomainDaemon::open(&home).unwrap();
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
        let reference: alondra_core::domain::AttachmentRef = uploaded.json().await.unwrap();
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
                memory_pressure: None,
                compressor: None,
                load: [1.0, 1.0, 1.0],
                cpus: 8,
                cpu_usage: 12.5,
                disk_total: 100,
                disk_available: 40,
                disk_purgeable: 0,
                unreliable: false,
            },
            None,
            vec![],
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
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-bootstrap-missing-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir(&home).unwrap();
        assert!(DomainDaemon::open(&home).is_err());
        assert!(
            !home.join("domain.db").exists(),
            "must not create an empty authority on missing stage"
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[cfg(test)]
mod vertical_tests;
