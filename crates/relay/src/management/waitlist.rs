//! Private-alpha waitlist. Joining stores one normalized address and nothing else:
//! it never creates a Keycloak user, an invitation or access, and never sends mail.
//! The public answer is identical for new, repeated and existing-account addresses.
use super::http::{actor, admin_request, db, failure};
use super::*;
use crate::Server;
use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, Path, Query, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MAX_BODY_BYTES: usize = 1024;
const PER_CLIENT: Budget = Budget {
    requests: 5,
    window: Duration::from_secs(10 * 60),
};
const GLOBAL: Budget = Budget {
    requests: 300,
    window: Duration::from_secs(10 * 60),
};
const MAX_TRACKED_CLIENTS: usize = 10_000;

pub fn routes() -> Router<Arc<Server>> {
    Router::new()
        .route("/api/waitlist", post(join))
        .route("/v1/admin/waitlist", get(list))
        .route("/v1/admin/waitlist/{id}", delete(remove))
        .route("/v1/admin/waitlist/{id}/invite", post(invite))
}

/// Trim, lowercase and check an address. ASCII only; no provider-specific
/// rewriting such as dropping dots or `+tags`.
pub fn normalize(input: &str) -> Option<String> {
    let value = input.trim();
    if value.len() > 254 || !value.is_ascii() {
        return None;
    }
    let value = value.to_ascii_lowercase();
    let (local, domain) = value.split_once('@')?;
    let local_ok = (1..=64).contains(&local.len())
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+/=?^_`{|}~-".contains(&b));
    let labels = domain.split('.').collect::<Vec<_>>();
    let domain_ok = labels.len() >= 2
        && labels.iter().all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        && !labels[labels.len() - 1].bytes().all(|b| b.is_ascii_digit());
    (local_ok && domain_ok).then_some(value)
}

#[derive(Clone, Copy)]
struct Budget {
    requests: u32,
    window: Duration,
}
#[derive(Clone, Copy)]
struct Window {
    start: Instant,
    count: u32,
}
impl Window {
    fn current(slot: &mut Option<Window>, budget: Budget, now: Instant) -> &mut Window {
        let expired = slot.is_none_or(|w| now.duration_since(w.start) >= budget.window);
        if expired {
            *slot = Some(Window {
                start: now,
                count: 0,
            });
        }
        slot.as_mut().expect("window initialized")
    }
    fn retry_after(&self, budget: Budget, now: Instant) -> Duration {
        budget.window.saturating_sub(now.duration_since(self.start))
    }
}

/// In-memory fixed windows, per client and global. Clients beyond the tracking
/// bound, and requests without a usable address, share one bucket.
pub struct Limiter {
    client_ip_header: Option<HeaderName>,
    state: Mutex<LimiterState>,
}
#[derive(Default)]
struct LimiterState {
    global: Option<Window>,
    clients: HashMap<Option<IpAddr>, Window>,
}
impl Limiter {
    pub fn new(client_ip_header: Option<HeaderName>) -> Self {
        Self {
            client_ip_header,
            state: Mutex::default(),
        }
    }
    /// `ALONDRA_CLIENT_IP_HEADER` names a header set by the only proxy that can
    /// reach the relay (Cloudflare's `CF-Connecting-IP`). Unset, the TCP peer is used.
    pub fn from_env() -> anyhow::Result<Self> {
        let header = match std::env::var("ALONDRA_CLIENT_IP_HEADER") {
            Ok(name) if !name.trim().is_empty() => Some(HeaderName::try_from(name.trim())?),
            _ => None,
        };
        Ok(Self::new(header))
    }
    pub fn client(&self, headers: &HeaderMap, peer: Option<IpAddr>) -> Option<IpAddr> {
        let ip = match &self.client_ip_header {
            Some(name) => {
                let mut values = headers.get_all(name).iter();
                match (values.next(), values.next()) {
                    (Some(value), None) => value.to_str().ok()?.trim().parse().ok()?,
                    _ => return None,
                }
            }
            None => peer?,
        };
        Some(match ip {
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => IpAddr::V4(v4),
                None => {
                    let s = v6.segments();
                    IpAddr::V6(std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
                }
            },
            v4 => v4,
        })
    }
    fn admit(&self, client: Option<IpAddr>, now: Instant) -> Result<(), Duration> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.clients.len() >= MAX_TRACKED_CLIENTS {
            state
                .clients
                .retain(|_, w| now.duration_since(w.start) < PER_CLIENT.window);
        }
        let key =
            if state.clients.len() >= MAX_TRACKED_CLIENTS && !state.clients.contains_key(&client) {
                None
            } else {
                client
            };
        let mut slot = state.clients.get(&key).copied();
        let own = *Window::current(&mut slot, PER_CLIENT, now);
        if own.count >= PER_CLIENT.requests {
            return Err(own.retry_after(PER_CLIENT, now));
        }
        let global = Window::current(&mut state.global, GLOBAL, now);
        if global.count >= GLOBAL.requests {
            return Err(global.retry_after(GLOBAL, now));
        }
        global.count += 1;
        state.clients.insert(
            key,
            Window {
                count: own.count + 1,
                ..own
            },
        );
        Ok(())
    }
}

fn public_error(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({"error": code}))).into_response()
}
#[derive(Deserialize)]
struct JoinBody {
    email: String,
}
async fn join(State(server): State<Arc<Server>>, request: Request<Body>) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip());
    let client = server.waitlist.client(request.headers(), peer);
    if let Err(wait) = server.waitlist.admit(client, Instant::now()) {
        let mut response = public_error(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
        let seconds = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(seconds.max(1)));
        return response;
    }
    let json = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
    let bytes = match axum::body::to_bytes(request.into_body(), MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => return public_error(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"),
    };
    let Some(email) = json
        .then(|| serde_json::from_slice::<JoinBody>(&bytes).ok())
        .flatten()
        .and_then(|body| normalize(&body.email))
    else {
        return public_error(StatusCode::BAD_REQUEST, "invalid_email");
    };
    let Ok(management) = db(&server) else {
        return public_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable");
    };
    if management.join_waitlist(&email).await.is_err() {
        return public_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable");
    }
    (StatusCode::ACCEPTED, Json(json!({"status": "accepted"}))).into_response()
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct WaitlistEntry {
    pub id: Uuid,
    pub email: String,
    pub created_at_ms: i64,
}
pub struct WaitlistPage {
    pub entries: Vec<WaitlistEntry>,
    pub next_cursor: Option<i64>,
}

impl Management {
    pub async fn join_waitlist(&self, email: &str) -> Result<()> {
        ensure!(
            normalize(email).as_deref() == Some(email),
            DomainError::InvalidInput
        );
        sqlx::query("INSERT INTO management_waitlist(id,email) VALUES($1,$2) ON CONFLICT ON CONSTRAINT management_waitlist_email_key DO NOTHING")
            .bind(Uuid::new_v4())
            .bind(email)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    pub async fn list_waitlist(
        &self,
        actor: Uuid,
        after: Option<i64>,
        limit: i64,
    ) -> Result<WaitlistPage> {
        ensure!((1..=100).contains(&limit), DomainError::InvalidInput);
        let mut tx = self.admin_tx(actor).await?;
        let rows = sqlx::query("SELECT seq,id,email,(extract(epoch FROM created_at)*1000)::bigint AS created_at_ms FROM management_waitlist WHERE ($1::bigint IS NULL OR seq>$1) ORDER BY seq LIMIT $2")
            .bind(after)
            .bind(limit + 1)
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;
        let mut cursor = None;
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows.iter().take(limit as usize) {
            cursor = Some(row.try_get::<i64, _>("seq")?);
            entries.push(WaitlistEntry {
                id: row.try_get("id")?,
                email: row.try_get("email")?,
                created_at_ms: row.try_get("created_at_ms")?,
            });
        }
        Ok(WaitlistPage {
            entries,
            next_cursor: if rows.len() > limit as usize {
                cursor
            } else {
                None
            },
        })
    }
    pub async fn remove_waitlist(&self, actor: Uuid, id: Uuid) -> Result<()> {
        let mut tx = self.admin_tx(actor).await?;
        let removed = sqlx::query("DELETE FROM management_waitlist WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        ensure!(removed == 1, DomainError::NotFound);
        audit(&mut tx, Some(actor), "waitlist_remove", id).await?;
        tx.commit().await?;
        Ok(())
    }
    /// Invites the request's address and takes it off the list together: a conflict leaves both as they were.
    pub async fn invite_from_waitlist(
        &self,
        actor: Uuid,
        id: Uuid,
        seconds: i64,
        request: Option<&AdminRequest>,
    ) -> Result<IssuedInvitation> {
        let seconds = ttl(seconds)?;
        let mut tx = self.admin_tx(actor).await?;
        if let Some(request) = request {
            request.reserve(&mut tx, actor).await?;
        }
        let address: Option<String> =
            sqlx::query_scalar("DELETE FROM management_waitlist WHERE id=$1 RETURNING email")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(address) = address else {
            return domain(DomainError::NotFound);
        };
        audit(&mut tx, Some(actor), "waitlist_remove", id).await?;
        let issued = issue_invitation(&mut tx, actor, &email(&address)?, seconds).await?;
        tx.commit().await?;
        Ok(issued)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pagination {
    after: Option<i64>,
    #[serde(default = "page_size")]
    limit: i64,
}
fn page_size() -> i64 {
    50
}
async fn list(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Query(page): Query<Pagination>,
) -> std::result::Result<Response, Response> {
    let actor = actor(&server, &headers).await?;
    let page = db(&server)?
        .list_waitlist(actor.id, page.after, page.limit)
        .await
        .map_err(failure)?;
    Ok(Json(json!({
        "entries": page.entries,
        "next_cursor": page.next_cursor.map(|c| c.to_string()),
    }))
    .into_response())
}
async fn remove(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> std::result::Result<Response, Response> {
    let actor = actor(&server, &headers).await?;
    db(&server)?
        .remove_waitlist(actor.id, id)
        .await
        .map_err(failure)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InviteBody {
    ttl_seconds: i64,
}
async fn invite(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<InviteBody>,
) -> std::result::Result<Response, Response> {
    let actor = actor(&server, &headers).await?;
    let key = admin_request(
        &headers,
        "waitlist_invite",
        json!({"id":id,"ttl_seconds":body.ttl_seconds}),
        true,
    )?;
    let result = db(&server)?
        .invite_from_waitlist(actor.id, id, body.ttl_seconds, Some(&key))
        .await
        .map_err(failure)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"invitation":result.invitation,"token":result.token})),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_accepts_trimmed_mixed_case_and_rejects_malformed() {
        assert_eq!(
            normalize("  Ana.Garcia+alpha@Example.COM \n").as_deref(),
            Some("ana.garcia+alpha@example.com")
        );
        assert_eq!(normalize("a@b.co").as_deref(), Some("a@b.co"));
        for bad in [
            "",
            "plain",
            "@example.com",
            "ana@",
            "ana@example",
            "ana@@example.com",
            "a@b@example.com",
            "ana @example.com",
            ".ana@example.com",
            "ana.@example.com",
            "an..a@example.com",
            "ana@-example.com",
            "ana@example-.com",
            "ana@example..com",
            "ana@example.com.",
            "ana@127.0.0.1",
            "\"ana\"@example.com",
            "añá@example.com",
            "ana@exämple.com",
            "ana\u{0}@example.com",
        ] {
            assert_eq!(normalize(bad), None, "{bad:?}");
        }
        assert_eq!(normalize(&format!("{}@example.com", "a".repeat(65))), None);
        assert_eq!(normalize(&format!("a@{}.com", "b".repeat(64))), None);
        let long = format!("a@{}.com", vec!["b".repeat(60); 5].join("."));
        assert!(long.len() > 254);
        assert_eq!(normalize(&long), None);
    }

    #[test]
    fn limiter_bounds_each_client_then_everyone_and_reports_retry_after() {
        let limiter = Limiter::new(None);
        let start = Instant::now();
        let first = Some(IpAddr::from([198, 51, 100, 1]));
        for _ in 0..PER_CLIENT.requests {
            assert!(limiter.admit(first, start).is_ok());
        }
        let wait = limiter
            .admit(first, start + Duration::from_secs(60))
            .unwrap_err();
        assert_eq!(wait, PER_CLIENT.window - Duration::from_secs(60));
        assert!(
            limiter
                .admit(Some(IpAddr::from([198, 51, 100, 2])), start)
                .is_ok(),
            "another client keeps its own budget"
        );
        assert!(limiter.admit(first, start + PER_CLIENT.window).is_ok());

        let global = Limiter::new(None);
        for n in 0..GLOBAL.requests {
            let ip = IpAddr::from((0x0a00_0000u32 + n).to_be_bytes());
            assert!(global.admit(Some(ip), start).is_ok());
        }
        let wait = global
            .admit(
                Some(IpAddr::from([203, 0, 113, 9])),
                start + Duration::from_secs(1),
            )
            .unwrap_err();
        assert_eq!(wait, GLOBAL.window - Duration::from_secs(1));
    }

    #[test]
    fn limiter_shares_one_bucket_once_tracking_is_full() {
        let limiter = Limiter::new(None);
        let now = Instant::now();
        {
            let mut state = limiter.state.lock().unwrap();
            for n in 0..MAX_TRACKED_CLIENTS as u32 {
                let ip = IpAddr::from((0x0a00_0000u32 + n).to_be_bytes());
                state.clients.insert(
                    Some(ip),
                    Window {
                        start: now,
                        count: 1,
                    },
                );
            }
        }
        for n in 0..PER_CLIENT.requests {
            let ip = IpAddr::from((0xc000_0200u32 + n).to_be_bytes());
            assert!(limiter.admit(Some(ip), now).is_ok());
        }
        assert!(
            limiter
                .admit(Some(IpAddr::from([192, 0, 2, 200])), now)
                .is_err(),
            "untracked clients cannot evade the shared overflow bucket"
        );
    }

    #[test]
    fn client_address_uses_only_the_configured_proxy_header() {
        let peer = Some(IpAddr::from([172, 18, 0, 1]));
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", HeaderValue::from_static("203.0.113.7"));
        headers.insert("x-forwarded-for", HeaderValue::from_static("192.0.2.1"));
        assert_eq!(Limiter::new(None).client(&headers, peer), peer);
        let cloudflare = Limiter::new(Some(HeaderName::from_static("cf-connecting-ip")));
        assert_eq!(
            cloudflare.client(&headers, peer),
            Some(IpAddr::from([203, 0, 113, 7]))
        );
        assert_eq!(cloudflare.client(&HeaderMap::new(), peer), None);
        headers.append("cf-connecting-ip", HeaderValue::from_static("203.0.113.8"));
        assert_eq!(cloudflare.client(&headers, peer), None, "ambiguous header");
        let mut v6 = HeaderMap::new();
        v6.insert(
            "cf-connecting-ip",
            HeaderValue::from_static("2001:db8:1:2:aaaa:bbbb:cccc:dddd"),
        );
        assert_eq!(
            cloudflare.client(&v6, peer),
            Some("2001:db8:1:2::".parse().unwrap()),
            "one IPv6 /64 is one client"
        );
        v6.insert("cf-connecting-ip", HeaderValue::from_static("not-an-ip"));
        assert_eq!(cloudflare.client(&v6, peer), None);
    }
}
