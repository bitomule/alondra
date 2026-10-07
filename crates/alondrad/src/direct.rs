//! The direct listener: phones and terminals on the same Wi-Fi or tailnet reach this Mac
//! without the relay. It carries the same end-to-end envelopes as the relay, so plain HTTP on
//! a hostile network exposes nothing. It is its own router on its own port: the loopback API
//! and its unsigned-local bypass are never mounted here.
pub(crate) mod bonjour;
pub(crate) mod pairing;
pub(crate) mod routes;

use crate::service::{DirectRefusal, Service};
use alondra_e2e::Envelope;
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::elliptic_curve::sec1::ToSec1Point;
use pairing::{MacIdentity, PairingBook, PairingError, RevealBody, StartBody};
use serde_json::json;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

const MAX_REQUEST: usize = 256 * 1024;
const MAX_CONCURRENT: usize = 32;

pub(crate) struct Direct {
    service: Arc<Service>,
    pairings: Arc<PairingBook>,
    name: String,
    signing_key: String,
    agreement_key: String,
    permits: tokio::sync::Semaphore,
    approve: Box<dyn Fn(&pairing::ApprovedDevice) -> Result<(), String> + Send + Sync>,
}

impl Direct {
    pub(crate) fn new(
        service: Arc<Service>,
        pairings: Arc<PairingBook>,
        name: String,
        approve: Box<dyn Fn(&pairing::ApprovedDevice) -> Result<(), String> + Send + Sync>,
    ) -> Self {
        let signing_key = URL_SAFE_NO_PAD
            .encode(service.signing_key().verifying_key().to_sec1_point(false).as_bytes());
        let agreement_key =
            URL_SAFE_NO_PAD.encode(service.agreement_public_key().to_sec1_point(false).as_bytes());
        Self {
            service,
            pairings,
            name,
            signing_key,
            agreement_key,
            permits: tokio::sync::Semaphore::new(MAX_CONCURRENT),
            approve,
        }
    }

    pub(crate) fn identity(&self) -> MacIdentity<'_> {
        MacIdentity {
            mac_id: self.service.mac_id(),
            signing: self.service.signing_key(),
            signing_key: &self.signing_key,
            agreement_secret: self.service.agreement_secret(),
            agreement_key: &self.agreement_key,
        }
    }

    pub(crate) fn pairings(&self) -> &PairingBook {
        &self.pairings
    }

    /// Approves a waiting request from this Mac: pins and approves the device, then signs.
    pub(crate) fn approve(&self, pairing_id: &str) -> Result<(), PairingError> {
        self.pairings.approve(&self.identity(), pairing_id, now_ms(), |device| (self.approve)(device))
    }
}

const DIRECT_DEVICES: &str = "direct-devices.json";

/// The devices paired on the local network, kept beside `service.toml` and not inside it, so a
/// daemon that predates the list still reads its config.
pub(crate) fn paired_directly(home: &std::path::Path, device_id: &str) -> bool {
    std::fs::read(home.join(DIRECT_DEVICES))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<String>>(&bytes).ok())
        .is_some_and(|ids| ids.iter().any(|id| id == device_id))
}

/// Called while the caller holds the service config lock.
pub(crate) fn remember_direct(home: &std::path::Path, device_id: &str) -> std::io::Result<()> {
    let path = home.join(DIRECT_DEVICES);
    let mut ids: Vec<String> = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    if ids.iter().any(|id| id == device_id) {
        return Ok(());
    }
    ids.push(device_id.to_owned());
    let temp = path.with_extension(format!("json.{}", std::process::id()));
    std::fs::write(&temp, serde_json::to_vec(&ids)?)?;
    std::fs::rename(temp, path)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Private, link-local, loopback and Tailscale (CGNAT) sources only: a Mac plugged straight into
/// the internet never answers strangers on this port.
pub(crate) fn allowed_peer(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return allowed_peer(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

fn refuse(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({"error": code}))).into_response()
}

/// Browsers attach `Origin` and may send simple cross-site POSTs; real clients send JSON
/// without it. Requiring both keeps a web page from reaching this port through the browser.
async fn guard(ConnectInfo(peer): ConnectInfo<SocketAddr>, request: Request<Body>, next: Next) -> Response {
    if !allowed_peer(peer.ip()) {
        return refuse(StatusCode::FORBIDDEN, "address_not_allowed");
    }
    let headers = request.headers();
    if headers.contains_key(header::ORIGIN) {
        return refuse(StatusCode::FORBIDDEN, "origin_not_allowed");
    }
    if request.method() == axum::http::Method::POST && !json_body(headers) {
        return refuse(StatusCode::UNSUPPORTED_MEDIA_TYPE, "json_required");
    }
    next.run(request).await
}

pub(crate) fn json_body(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
}

pub(crate) fn router(direct: Arc<Direct>) -> Router {
    Router::new()
        .route("/direct/hello", get(hello))
        .route("/direct/request", post(request))
        .route("/direct/pairing", post(start_pairing))
        .route("/direct/pairing/{id}", get(pairing_status))
        .route("/direct/pairing/{id}/reveal", post(reveal_pairing))
        .layer(DefaultBodyLimit::max(MAX_REQUEST))
        .layer(axum::middleware::from_fn(guard))
        .with_state(direct)
}

async fn hello(State(direct): State<Arc<Direct>>) -> Response {
    Json(json!({
        "mac_id": direct.service.mac_id(),
        "name": direct.name,
        "signing_key": direct.signing_key,
        "agreement_key": direct.agreement_key,
    }))
    .into_response()
}

async fn request(State(direct): State<Arc<Direct>>, body: Bytes) -> Response {
    let Ok(_permit) = direct.permits.try_acquire() else {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "busy");
    };
    let Ok(envelope) = serde_json::from_slice::<Envelope>(&body) else {
        return refuse(StatusCode::BAD_REQUEST, "invalid_envelope");
    };
    match direct.service.handle_direct(envelope).await {
        Ok((sealed, lease)) => {
            let response = ([(header::CONTENT_TYPE, "application/json")], sealed).into_response();
            drop(lease);
            response
        }
        Err(DirectRefusal::Misdirected) => refuse(StatusCode::MISDIRECTED_REQUEST, "wrong_mac"),
        Err(DirectRefusal::Revoked(sealed)) => {
            ([(header::CONTENT_TYPE, "application/json")], sealed).into_response()
        }
        Err(DirectRefusal::Unauthorized) => refuse(StatusCode::UNAUTHORIZED, "unauthorized"),
        Err(DirectRefusal::Conflict) => refuse(StatusCode::CONFLICT, "idempotency_conflict"),
        Err(DirectRefusal::Rejected(detail)) => {
            (StatusCode::BAD_REQUEST, Json(json!({"error": "rejected", "detail": detail}))).into_response()
        }
    }
}

fn pairing_error(error: PairingError) -> Response {
    let status = match &error {
        PairingError::Invalid(_) => StatusCode::BAD_REQUEST,
        PairingError::Busy => StatusCode::TOO_MANY_REQUESTS,
        PairingError::Denied => StatusCode::FORBIDDEN,
        PairingError::Conflict => StatusCode::CONFLICT,
        PairingError::NotFound => StatusCode::NOT_FOUND,
        PairingError::Expired => StatusCode::GONE,
        PairingError::Internal(detail) => {
            log!("direct pairing failed: {detail}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    refuse(status, error.code())
}

async fn start_pairing(
    State(direct): State<Arc<Direct>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> Response {
    let Ok(body) = serde_json::from_slice::<StartBody>(&body) else {
        return refuse(StatusCode::BAD_REQUEST, "invalid_pairing");
    };
    match direct.pairings.start(&direct.identity(), peer.ip(), body, now_ms()) {
        Ok(started) => Json(started).into_response(),
        Err(error) => pairing_error(error),
    }
}

async fn reveal_pairing(State(direct): State<Arc<Direct>>, Path(id): Path<String>, body: Bytes) -> Response {
    let Ok(body) = serde_json::from_slice::<RevealBody>(&body) else {
        return refuse(StatusCode::BAD_REQUEST, "invalid_pairing");
    };
    match direct.pairings.reveal(&direct.identity(), &id, body, now_ms()) {
        Ok(()) => Json(json!({"state": "awaiting_approval"})).into_response(),
        Err(error) => pairing_error(error),
    }
}

async fn pairing_status(State(direct): State<Arc<Direct>>, Path(id): Path<String>) -> Response {
    match direct.pairings.status(&id, now_ms()) {
        Ok(status) => Json(status).into_response(),
        Err(error) => pairing_error(error),
    }
}

/// Serves the direct port for as long as the daemon runs, rebinding after a failure, and keeps
/// this Mac advertised on Bonjour while it listens. A busy port never stops the daemon.
pub(crate) async fn run(direct: Arc<Direct>, sandbox: bool) {
    let config = direct.service.direct_config().clone();
    let addr = SocketAddr::new(config.listen, config.port);
    loop {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                log!("direct listener on http://{addr}");
                let advert = bonjour::advertise(&direct.name, direct.service.mac_id(), config.port, sandbox);
                let app = router(direct.clone()).into_make_service_with_connect_info::<SocketAddr>();
                if let Err(error) = axum::serve(listener, app).await {
                    log!("direct listener stopped: {error}");
                }
                drop(advert);
            }
            Err(error) => log!("direct listener cannot bind {addr}: {error}"),
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

#[cfg(test)]
mod tests;
