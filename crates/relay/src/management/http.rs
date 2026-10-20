//! JSON product API. Browser cookie/CSRF authentication is supplied by the BFF layer.
//! Native/internal callers use the same verified account bearer, never an admin flag.
use super::*;
use crate::{Server, now_ms};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub fn routes() -> Router<Arc<Server>> {
    Router::new()
        .route("/v1/account", get(account).delete(delete_self))
        .route("/v1/account/devices", get(account_devices))
        .route("/v1/account/devices/macs/{id}/label", post(rename_mac))
        .route(
            "/v1/account/devices/clients/{id}/label",
            post(rename_client),
        )
        .route("/v1/account/devices/macs/{id}", delete(revoke_own_mac))
        .route(
            "/v1/account/devices/clients/{id}",
            delete(revoke_own_client),
        )
        .route("/v1/admin/accounts", get(accounts))
        .route("/v1/admin/accounts/{id}/disable", post(disable))
        .route("/v1/admin/accounts/{id}", delete(remove_account))
        .route("/v1/admin/invitations", get(invitations).post(invite))
        .route("/v1/admin/invitations/{id}/cancel", post(cancel))
        .route("/v1/admin/invitations/{id}/reissue", post(reissue))
        .route("/v1/invitations/status", post(invitation_status))
        .route("/v1/invitations/accept", post(accept))
        .layer(axum::extract::DefaultBodyLimit::max(8192))
}
type ApiResult = std::result::Result<Response, Response>;
pub(super) fn failure(error: anyhow::Error) -> Response {
    let (status, code, retryable) = match error.downcast_ref::<DomainError>() {
        Some(DomainError::Forbidden) => (StatusCode::FORBIDDEN, "access_denied", false),
        Some(DomainError::NotFound) => (StatusCode::NOT_FOUND, "not_found", false),
        Some(DomainError::Conflict) => (StatusCode::CONFLICT, "state_conflict", false),
        Some(DomainError::InvalidInput) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_input", false)
        }
        Some(DomainError::ProviderUnavailable) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "provider_unavailable",
            true,
        ),
        Some(DomainError::AlreadyApplied) => {
            (StatusCode::CONFLICT, "action_already_applied", false)
        }
        Some(DomainError::TokenAlreadyIssued) => (
            StatusCode::CONFLICT,
            "invitation_token_already_issued",
            false,
        ),
        Some(DomainError::IdempotencyConflict) => {
            (StatusCode::CONFLICT, "idempotency_conflict", false)
        }
        Some(DomainError::Unavailable) => (StatusCode::GONE, "invitation_unavailable", false),
        None => (StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true),
    };
    (
        status,
        Json(json!({"error":{"code":code,"retryable":retryable}})),
    )
        .into_response()
}
pub(super) fn db(server: &Server) -> std::result::Result<&Management, Response> {
    server
        .auth
        .management
        .as_ref()
        .ok_or_else(|| failure(anyhow::anyhow!("management disabled")))
}
pub(super) async fn actor(server: &Server, headers: &HeaderMap) -> std::result::Result<Account, Response> {
    let subject = server
        .auth
        .account_owner(headers, now_ms())
        .await
        .map_err(|e| crate::authentication_error(e).into_response())?;
    db(server)?
        .active_identity(&server.auth.config.issuer, &subject)
        .await
        .map_err(failure)
}
async fn account(State(server): State<Arc<Server>>, headers: HeaderMap) -> ApiResult {
    Ok(Json(json!({"account":actor(&server,&headers).await?})).into_response())
}
#[derive(serde::Serialize)]
struct ClientMetadata {
    device_id: String,
    label: Option<String>,
    label_revision: i64,
    status: &'static str,
    reconciliation: Option<&'static str>,
}
#[derive(serde::Serialize)]
struct MacMetadata {
    mac_id: String,
    label: Option<String>,
    label_revision: i64,
    status: &'static str,
    reconciliation: Option<&'static str>,
    clients: Vec<ClientMetadata>,
}
async fn owned_metadata(
    server: &Server,
    actor: &Account,
    headers: &HeaderMap,
) -> std::result::Result<Vec<MacMetadata>, Response> {
    let owner = server
        .auth
        .account_owner(headers, now_ms())
        .await
        .map_err(|e| crate::authentication_error(e).into_response())?;
    let revoked = db(server)?
        .revoked_devices(actor.id)
        .await
        .map_err(failure)?;
    let mut rows = server
        .auth
        .account_device_metadata(&owner)
        .map_err(failure)?
        .into_iter()
        .map(|(mac_id, clients)| MacMetadata {
            mac_id,
            label: None,
            label_revision: 0,
            status: "active",
            reconciliation: None,
            clients: clients
                .into_iter()
                .map(|device_id| ClientMetadata {
                    device_id,
                    label: None,
                    label_revision: 0,
                    status: "active",
                    reconciliation: None,
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    for r in revoked {
        let index = if let Some(i) = rows.iter().position(|m| m.mac_id == r.mac_id) {
            i
        } else {
            rows.push(MacMetadata {
                mac_id: r.mac_id.clone(),
                label: None,
                label_revision: 0,
                status: "revoked",
                reconciliation: None,
                clients: Vec::new(),
            });
            rows.len() - 1
        };
        let mac = &mut rows[index];
        let reconciliation = Some(if r.state == "completed" {
            "completed"
        } else {
            "pending"
        });
        if r.kind == "mac" {
            mac.status = "revoked";
            mac.reconciliation = reconciliation;
        } else if let Some(client) = mac.clients.iter_mut().find(|c| c.device_id == r.device_id) {
            client.status = "revoked";
            client.reconciliation = reconciliation;
        } else {
            mac.clients.push(ClientMetadata {
                device_id: r.device_id,
                label: None,
                label_revision: 0,
                status: "revoked",
                reconciliation,
            });
        }
    }
    let labels = db(server)?.device_labels(actor.id).await.map_err(failure)?;
    for mac in &mut rows {
        if let Some(label) = labels
            .iter()
            .find(|l| l.kind == "mac" && l.device_id == mac.mac_id && l.mac_id == mac.mac_id)
        {
            mac.label = label.label.clone();
            mac.label_revision = label.label_revision;
        }
        for client in &mut mac.clients {
            if let Some(label) = labels.iter().find(|l| {
                l.kind == "client" && l.device_id == client.device_id && l.mac_id == mac.mac_id
            }) {
                client.label = label.label.clone();
                client.label_revision = label.label_revision;
            }
        }
        if mac.status == "revoked" {
            for client in &mut mac.clients {
                client.status = "revoked";
                client.reconciliation = mac.reconciliation;
            }
        }
        mac.clients.sort_by(|a, b| a.device_id.cmp(&b.device_id));
    }
    rows.sort_by(|a, b| a.mac_id.cmp(&b.mac_id));
    Ok(rows)
}
async fn account_devices(State(server): State<Arc<Server>>, headers: HeaderMap) -> ApiResult {
    let actor = actor(&server, &headers).await?;
    Ok(Json(json!({"macs":owned_metadata(&server,&actor,&headers).await?})).into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameDevice {
    #[serde(deserialize_with = "Option::<String>::deserialize")]
    label: Option<String>,
    expected_revision: i64,
}
async fn rename_mac(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RenameDevice>,
) -> ApiResult {
    rename_owned(server, headers, id, "mac", body).await
}
async fn rename_client(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RenameDevice>,
) -> ApiResult {
    rename_owned(server, headers, id, "client", body).await
}
async fn rename_owned(
    server: Arc<Server>,
    headers: HeaderMap,
    id: String,
    kind: &str,
    body: RenameDevice,
) -> ApiResult {
    let actor = actor(&server, &headers).await?;
    let owner = server
        .auth
        .account_owner(&headers, now_ms())
        .await
        .map_err(|e| crate::authentication_error(e).into_response())?;
    let mac_id = server
        .auth
        .account_device_metadata(&owner)
        .map_err(failure)?
        .into_iter()
        .find(|(mac, clients)| {
            if kind == "mac" {
                mac == &id
            } else {
                clients.contains(&id)
            }
        })
        .map(|(mac, _)| mac)
        .ok_or_else(|| failure(DomainError::NotFound.into()))?;
    let key = request(
        &headers,
        "rename_device",
        json!({"kind":kind,"id":id,"label":body.label,"expected_revision":body.expected_revision}),
        false,
    )?;
    let label = db(&server)?
        .rename_device_request(
            actor.id,
            kind,
            &id,
            &mac_id,
            body.label.as_deref(),
            body.expected_revision,
            &key,
            || {
                server.auth.account_device_metadata(&owner).map(|rows| {
                    rows.iter().any(|(mac, clients)| {
                        mac == &mac_id && (kind == "mac" || clients.contains(&id))
                    })
                })
            },
        )
        .await
        .map_err(failure)?;
    Ok(Json(json!({"label":label.label,"label_revision":label.label_revision})).into_response())
}

async fn revoke_owned(
    server: Arc<Server>,
    headers: HeaderMap,
    id: String,
    kind: &str,
) -> ApiResult {
    let actor = actor(&server, &headers).await?;
    let metadata = owned_metadata(&server, &actor, &headers).await?;
    let mac = metadata
        .iter()
        .find(|m| {
            if kind == "mac" {
                m.mac_id == id
            } else {
                m.clients.iter().any(|c| c.device_id == id)
            }
        })
        .ok_or_else(|| failure(DomainError::NotFound.into()))?;
    let key = request(
        &headers,
        "revoke_device",
        json!({"kind":kind,"id":id}),
        false,
    )?;
    db(&server)?
        .revoke_device_request(
            actor.id,
            kind,
            &id,
            &mac.mac_id,
            &mac.clients
                .iter()
                .map(|c| c.device_id.clone())
                .collect::<Vec<_>>(),
            &key,
        )
        .await
        .map_err(failure)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"status":"revoked","reconciliation":"pending"})),
    )
        .into_response())
}
async fn revoke_own_mac(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    revoke_owned(server, headers, id, "mac").await
}
async fn revoke_own_client(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    revoke_owned(server, headers, id, "client").await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteSelfBody {
    confirm: bool,
}
async fn delete_self(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Json(body): Json<DeleteSelfBody>,
) -> ApiResult {
    if !body.confirm {
        return Err(failure(DomainError::InvalidInput.into()));
    }
    let actor = actor(&server, &headers).await?;
    let key = request(&headers, "self_delete", json!({"confirm":true}), false)?;
    let account = db(&server)?
        .delete_self_request(actor.id, &key)
        .await
        .map_err(failure)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"account":account,"reconciliation":"pending"})),
    )
        .into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pagination {
    after: Option<Uuid>,
    #[serde(default = "page_size")]
    limit: i64,
}
fn page_size() -> i64 {
    50
}
async fn accounts(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Query(page): Query<Pagination>,
) -> ApiResult {
    let actor = actor(&server, &headers).await?;
    let result = db(&server)?
        .list_accounts(actor.id, page.after, page.limit)
        .await
        .map_err(failure)?;
    Ok(Json(json!({"accounts":result.items,"next_cursor":result.next_cursor})).into_response())
}
async fn invitations(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Query(page): Query<Pagination>,
) -> ApiResult {
    let actor = actor(&server, &headers).await?;
    let result = db(&server)?
        .list_invitations(actor.id, page.after, page.limit)
        .await
        .map_err(failure)?;
    Ok(Json(json!({"invitations":result.items,"next_cursor":result.next_cursor})).into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InviteBody {
    email: String,
    ttl_seconds: i64,
}
fn request(
    headers: &HeaderMap,
    operation: &str,
    body: Value,
    issues_token: bool,
) -> std::result::Result<AdminRequest, Response> {
    let key = headers
        .get("Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| failure(DomainError::InvalidInput.into()))?;
    AdminRequest::new(key, operation, &body, issues_token).map_err(failure)
}
async fn invite(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Json(body): Json<InviteBody>,
) -> ApiResult {
    let actor = actor(&server, &headers).await?;
    let key = request(
        &headers,
        "invite",
        json!({"email":body.email,"ttl_seconds":body.ttl_seconds}),
        true,
    )?;
    let result = db(&server)?
        .create_invitation_request(actor.id, &body.email, body.ttl_seconds, Some(&key))
        .await
        .map_err(failure)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"invitation":result.invitation,"token":result.token})),
    )
        .into_response())
}
async fn cancel(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult {
    let actor = actor(&server, &headers).await?;
    let key = request(&headers, "cancel", json!({"id":id}), false)?;
    Ok(Json(json!({"invitation":db(&server)?.cancel_invitation_request(actor.id,id,Some(&key)).await.map_err(failure)?})).into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReissueBody {
    ttl_seconds: i64,
}
async fn reissue(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ReissueBody>,
) -> ApiResult {
    let actor = actor(&server, &headers).await?;
    let key = request(
        &headers,
        "reissue",
        json!({"id":id,"ttl_seconds":body.ttl_seconds}),
        true,
    )?;
    let result = db(&server)?
        .reissue_invitation_request(actor.id, id, body.ttl_seconds, Some(&key))
        .await
        .map_err(failure)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"invitation":result.invitation,"token":result.token})),
    )
        .into_response())
}
async fn lifecycle(server: Arc<Server>, headers: HeaderMap, id: Uuid, delete: bool) -> ApiResult {
    let actor = actor(&server, &headers).await?;
    let key = request(
        &headers,
        if delete { "delete" } else { "disable" },
        json!({"id":id}),
        false,
    )?;
    let account = db(&server)?
        .disable_account_request(actor.id, id, delete, Some(&key))
        .await
        .map_err(failure)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"account":account,"reconciliation":"pending"})),
    )
        .into_response())
}
async fn disable(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult {
    lifecycle(server, headers, id, false).await
}
async fn remove_account(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult {
    lifecycle(server, headers, id, true).await
}

// Pilot admission endpoints have a global rate bound without trusting proxy IP headers.
static PUBLIC_WINDOW: Mutex<Option<(Instant, u32)>> = Mutex::new(None);
fn public_budget() -> std::result::Result<(), Response> {
    let mut window = PUBLIC_WINDOW
        .lock()
        .map_err(|_| failure(anyhow::anyhow!("rate limiter unavailable")))?;
    let value = window.get_or_insert((Instant::now(), 0));
    if value.0.elapsed() > Duration::from_secs(60) {
        *value = (Instant::now(), 0)
    }
    if value.1 >= 30 {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":{"code":"rate_limited","retryable":true}})),
        )
            .into_response());
    }
    value.1 += 1;
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenBody {
    token: String,
}
async fn invitation_status(
    State(server): State<Arc<Server>>,
    Json(body): Json<TokenBody>,
) -> ApiResult {
    public_budget()?;
    let status = match db(&server)?.invitation_status(&body.token).await {
        Ok(status) => status,
        Err(e) if e.downcast_ref::<DomainError>() == Some(&DomainError::Unavailable) => {
            "unavailable"
        }
        Err(e) => return Err(failure(e)),
    };
    Ok(Json(json!({"status":status})).into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptBody {
    token: String,
    display_name: String,
    password: String,
}
async fn accept(State(server): State<Arc<Server>>, Json(body): Json<AcceptBody>) -> ApiResult {
    public_budget()?;
    let keycloak = server
        .management_keycloak
        .as_ref()
        .ok_or_else(|| failure(anyhow::anyhow!("provisioning unavailable")))?;
    let work = async {
        let reservation = db(&server)?
            .reserve_acceptance(&body.token, &body.display_name)
            .await
            .map_err(failure)?;
        let provisioned = keycloak
            .provision(&reservation, &body.password)
            .await
            .map_err(failure)?;
        reservation
            .complete(&provisioned.subject)
            .await
            .map_err(failure)?;
        Ok::<_,Response>((StatusCode::CREATED,Json(json!({"status":"active","login_required":true,"credential_already_created":provisioned.already_existed}))).into_response())
    };
    tokio::time::timeout(Duration::from_secs(35), work)
        .await
        .map_err(|_| failure(anyhow::anyhow!("provisioning deadline")))?
}
