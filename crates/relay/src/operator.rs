//! The operator API: how an account service beside the relay removes devices and accounts,
//! reads which devices an account holds, and ends pushes of finished sign-in sessions. It is
//! served on its own listener, never the public one, behind a bearer token. With an account
//! source configured, the relay also reads the complete list of removals from it on start,
//! after a gap in their numbers, and periodically. `docs/architecture/relay-operator-api.md`
//! is the contract.

use crate::auth::{Removal, RemovalsApplied};
use crate::{Server, error};
use anyhow::{Context, Result, bail, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::sync::Notify;

const MAX_BATCH: usize = 1_000;
const MAX_ID_BYTES: usize = 256;
const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const SOURCE_TIMEOUT: Duration = Duration::from_secs(10);
const SOURCE_PERIOD: Duration = Duration::from_secs(10 * 60);
const SOURCE_RETRY_MIN: Duration = Duration::from_secs(2);
const SOURCE_RETRY_MAX: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct Operator {
    token: Option<String>,
    source: Option<reqwest::Url>,
    pub(crate) gap: Notify,
}

impl Operator {
    /// `ALONDRA_OPERATOR_TOKEN_FILE` holds the bearer both directions use;
    /// `ALONDRA_ACCOUNT_SOURCE_URL` is where the complete list of removals is read.
    pub fn from_env() -> Result<Self> {
        let token = match std::env::var("ALONDRA_OPERATOR_TOKEN_FILE") {
            Ok(path) => Some(std::fs::read_to_string(path)?.trim().to_owned()),
            Err(_) => None,
        };
        let source = std::env::var("ALONDRA_ACCOUNT_SOURCE_URL").ok();
        Self::new(token, source.as_deref())
    }

    pub fn new(token: Option<String>, source: Option<&str>) -> Result<Self> {
        if let Some(token) = &token {
            ensure!(token.len() >= 32, "operator token must have at least 32 characters");
        }
        let source = source.map(source_url).transpose()?;
        ensure!(
            source.is_none() || token.is_some(),
            "ALONDRA_ACCOUNT_SOURCE_URL needs ALONDRA_OPERATOR_TOKEN_FILE"
        );
        Ok(Self { token, source, gap: Notify::new() })
    }

    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    pub fn follows_source(&self) -> bool {
        self.source.is_some()
    }
}

/// HTTPS, or plain HTTP to loopback or to a single-label host of a private container network.
fn source_url(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value).context("ALONDRA_ACCOUNT_SOURCE_URL is not a URL")?;
    let host = url.host_str().unwrap_or_default();
    let allowed = match url.scheme() {
        "https" => !host.is_empty(),
        "http" => matches!(host, "127.0.0.1" | "localhost") || (!host.is_empty() && !host.contains('.') && !host.contains(':')),
        _ => false,
    };
    ensure!(
        allowed,
        "ALONDRA_ACCOUNT_SOURCE_URL must be https, or http to loopback or a single-label container host"
    );
    Ok(url)
}

pub fn router(server: Arc<Server>) -> Router {
    Router::new()
        .route("/v1/operator/removals", post(removals))
        .route("/v1/operator/owners/{owner}/devices", get(owner_devices))
        .route("/v1/operator/push-sessions", get(push_sessions))
        .route("/v1/operator/push-sessions/ended", post(ended_push_sessions))
        .layer(DefaultBodyLimit::max(crate::MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(server.clone(), require_token))
        .layer(middleware::from_fn_with_state(server.clone(), crate::limit_and_headers))
        .with_state(server)
}

async fn require_token(State(server): State<Arc<Server>>, request: Request, next: Next) -> Response {
    use subtle::ConstantTimeEq;
    let Some(expected) = &server.operator.token else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !bool::from(bearer(request.headers()).as_bytes().ct_eq(expected.as_bytes())) {
        return error(StatusCode::UNAUTHORIZED, "operator_token_invalid").into_response();
    }
    next.run(request).await
}

fn bearer(headers: &HeaderMap) -> &str {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemovalBatch {
    removals: Vec<NumberedRemoval>,
}

#[derive(Deserialize)]
struct NumberedRemoval {
    seq: u64,
    #[serde(flatten)]
    removal: Removal,
}

/// The complete list an account source serves.
#[derive(Deserialize, Serialize)]
pub struct RemovalList {
    pub seq: u64,
    pub removals: Vec<Removal>,
}

async fn removals(State(server): State<Arc<Server>>, Json(batch): Json<RemovalBatch>) -> Response {
    if batch.removals.len() > MAX_BATCH || !batch.removals.iter().all(|r| valid_removal(&r.removal)) {
        return error(StatusCode::BAD_REQUEST, "invalid_removals").into_response();
    }
    let batch: Vec<_> = batch.removals.into_iter().map(|r| (r.seq, r.removal)).collect();
    let _gate = server.device_gate.write().await;
    match server.auth.apply_removals(&batch) {
        Ok(RemovalsApplied::Through(applied)) => {
            purge(&server, batch.iter().map(|(_, removal)| removal)).await;
            (StatusCode::OK, Json(json!({"applied": applied}))).into_response()
        }
        Ok(RemovalsApplied::Gap(applied)) => {
            server.operator.gap.notify_one();
            (StatusCode::CONFLICT, Json(json!({"error": "removals_gap", "applied": applied})))
                .into_response()
        }
        Ok(RemovalsApplied::Behind(applied)) => {
            server.operator.gap.notify_one();
            (StatusCode::CONFLICT, Json(json!({"error": "removals_behind", "applied": applied})))
                .into_response()
        }
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "registry_unavailable").into_response(),
    }
}

fn valid_removal(removal: &Removal) -> bool {
    let id = |value: &str| !value.is_empty() && value.len() <= MAX_ID_BYTES;
    match removal {
        Removal::Mac { owner, mac_id } => id(owner) && id(mac_id),
        Removal::Client { owner, client_id, mac_id } => id(owner) && id(client_id) && id(mac_id),
        Removal::Owner { owner } => id(owner),
    }
}

/// Pairings in progress and Live Activity reports of what was removed go with it. Caller holds
/// `device_gate` exclusively.
async fn purge(server: &Server, removals: impl Iterator<Item = &Removal>) {
    for removal in removals {
        match removal {
            Removal::Mac { owner, mac_id } => server.pairing.purge_device(owner, "mac", mac_id),
            Removal::Client { owner, client_id, .. } => {
                server.pairing.purge_device(owner, "client", client_id)
            }
            Removal::Owner { owner } => server.pairing.purge_owner(owner),
        }
    }
    crate::activity::after_revocation(server).await;
}

async fn owner_devices(State(server): State<Arc<Server>>, Path(owner): Path<String>) -> Response {
    let _gate = server.device_gate.read().await;
    match server.auth.account_device_metadata(&owner) {
        Ok(macs) => {
            let macs: Vec<_> = macs
                .into_iter()
                .map(|(mac_id, clients)| json!({"mac_id": mac_id, "clients": clients}))
                .collect();
            (StatusCode::OK, Json(json!({"macs": macs}))).into_response()
        }
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "registry_unavailable").into_response(),
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SessionPush {
    device_id: String,
    sid: String,
}

async fn push_sessions(State(server): State<Arc<Server>>) -> Response {
    let mut sessions: Vec<_> = server.auth.push_sessions().into_iter().collect();
    sessions.sort();
    let sessions: Vec<_> = sessions
        .into_iter()
        .map(|((owner, client_id), mut registrations)| {
            registrations.sort();
            let registrations: Vec<_> = registrations
                .into_iter()
                .map(|(device_id, sid)| SessionPush { device_id, sid })
                .collect();
            json!({"owner": owner, "client_id": client_id, "registrations": registrations})
        })
        .collect();
    (StatusCode::OK, Json(json!({"sessions": sessions}))).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EndedSessions {
    ended: Vec<SessionPush>,
}

async fn ended_push_sessions(
    State(server): State<Arc<Server>>,
    Json(ended): Json<EndedSessions>,
) -> Response {
    if ended.ended.len() > MAX_BATCH {
        return error(StatusCode::BAD_REQUEST, "invalid_push_sessions").into_response();
    }
    let _gate = server.device_gate.read().await;
    for push in &ended.ended {
        if server.auth.forget_ended_session_push(&push.device_id, &push.sid).is_err() {
            return error(StatusCode::SERVICE_UNAVAILABLE, "registry_unavailable").into_response();
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Read the account source's complete list and hold every removal on it.
pub async fn reconcile_from_source(server: &Server) -> Result<()> {
    let (Some(source), Some(token)) = (&server.operator.source, &server.operator.token) else {
        bail!("no account source configured");
    };
    let client = reqwest::Client::builder()
        .timeout(SOURCE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut response = client
        .get(source.clone())
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            body.len().saturating_add(chunk.len()) <= MAX_SOURCE_BYTES,
            "account source list too large"
        );
        body.extend_from_slice(&chunk);
    }
    let list: RemovalList = serde_json::from_slice(&body)?;
    let (removals, invalid): (Vec<_>, Vec<_>) = list.removals.into_iter().partition(valid_removal);
    if !invalid.is_empty() {
        eprintln!("alondra-relay: the account source lists {} removals with an invalid ID; skipped", invalid.len());
    }
    let _gate = server.device_gate.write().await;
    let changed = server.auth.reconcile_removals(list.seq, &removals)?;
    if !changed.is_empty() {
        purge(server, changed.iter()).await;
    }
    Ok(())
}

/// Reconcile until it first succeeds, then again after each gap and every `SOURCE_PERIOD`.
pub async fn follow_source(server: Arc<Server>) {
    let mut retry = SOURCE_RETRY_MIN;
    loop {
        match reconcile_from_source(&server).await {
            Ok(()) => {
                retry = SOURCE_RETRY_MIN;
                tokio::select! {
                    _ = tokio::time::sleep(SOURCE_PERIOD) => {}
                    _ = server.operator.gap.notified() => {}
                }
            }
            Err(failure) => {
                eprintln!("alondra-relay: could not read the account source's removals: {failure:#}");
                tokio::time::sleep(retry).await;
                retry = (retry * 2).min(SOURCE_RETRY_MAX);
            }
        }
    }
}
