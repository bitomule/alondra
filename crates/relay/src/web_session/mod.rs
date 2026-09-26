//! Same-origin browser sessions. Browser code receives an opaque HttpOnly cookie
//! and a CSRF token, never OIDC access/refresh tokens or device content keys.
pub mod oidc;
pub mod store;

use crate::{Server, auth::Auth, management::Account, now_ms};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use std::{path::Path, sync::Arc, time::Duration};
use store::{SessionSecrets, Store, digest, random_token};
use tokio::sync::Semaphore;

pub struct WebSession {
    store: Store,
    provider: oidc::Provider,
    origin: String,
    secure: bool,
    permits: Semaphore,
}
#[derive(Clone)]
struct BrowserIdentity {
    cookie: String,
    csrf: String,
    account: Account,
}

pub fn routes() -> Router<Arc<Server>> {
    Router::new()
        .route("/auth/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/auth/logout", post(logout))
        .route("/v1/session", get(session))
}
fn failure(status: StatusCode, code: &'static str) -> Response {
    (
        status,
        Json(json!({"error":{"code":code,"retryable":status==StatusCode::SERVICE_UNAVAILABLE}})),
    )
        .into_response()
}
fn unavailable() -> Response {
    failure(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable")
}
fn unauthorized() -> Response {
    failure(StatusCode::UNAUTHORIZED, "unauthorized")
}
fn forbidden() -> Response {
    failure(StatusCode::FORBIDDEN, "access_denied")
}
fn auth_failure(error: anyhow::Error) -> Response {
    if matches!(
        error.downcast_ref::<crate::auth::AccountGateError>(),
        Some(crate::auth::AccountGateError::Unavailable)
    ) {
        unavailable()
    } else {
        unauthorized()
    }
}

fn private_file(path: &str, max: usize) -> Result<String> {
    use std::{io::Read, os::unix::fs::PermissionsExt};
    let file = std::fs::File::open(path).context("web credential file unavailable")?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.permissions().mode() & 0o077 == 0
            && metadata.len() <= max as u64,
        "web credential file must be private and bounded"
    );
    let mut content = String::new();
    file.take(max as u64 + 1).read_to_string(&mut content)?;
    ensure!(
        content.len() <= max && !content.trim().is_empty(),
        "invalid web credential file"
    );
    Ok(content.trim().into())
}

impl WebSession {
    pub fn from_env(auth: &Auth) -> Result<Option<Arc<Self>>> {
        let origin = match std::env::var("ALONDRA_WEB_ORIGIN") {
            Ok(value) => value,
            Err(std::env::VarError::NotPresent) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let management = auth
            .management
            .as_ref()
            .context("web requires account management")?;
        let url = reqwest::Url::parse(&origin)?;
        let origin = oidc::allowed_origin(&url)?;
        ensure!(
            url.path() == "/" && url.query().is_none(),
            "web origin must not include a path"
        );
        let store = Store::from_key_file(
            management.pool.clone(),
            Path::new(&std::env::var("ALONDRA_WEB_KEY_FILE")?),
        )?;
        let secret = private_file(&std::env::var("ALONDRA_WEB_CLIENT_SECRET_FILE")?, 4096)?;
        let provider = oidc::Provider::new(
            &auth.config.issuer,
            &origin,
            std::env::var("ALONDRA_WEB_CLIENT_ID")?,
            secret,
        )?;
        Ok(Some(Arc::new(Self {
            store,
            provider,
            origin,
            secure: url.scheme() == "https",
            permits: Semaphore::new(2),
        })))
    }
    fn session_name(&self) -> &'static str {
        if self.secure {
            "__Host-alondra_session"
        } else {
            "alondra_dev_session"
        }
    }
    fn flow_name(&self) -> &'static str {
        if self.secure {
            "__Host-alondra_flow"
        } else {
            "alondra_dev_flow"
        }
    }
    fn cookie(&self, name: &str, value: &str, age: i64) -> String {
        format!(
            "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={age}{}",
            if self.secure { "; Secure" } else { "" }
        )
    }
    fn check_origin(&self, headers: &HeaderMap) -> bool {
        let mut origins = headers.get_all(header::ORIGIN).iter();
        origins.next().and_then(|value| value.to_str().ok()) == Some(self.origin.as_str())
            && origins.next().is_none()
    }
    async fn authenticate(
        &self,
        auth: &Auth,
        cookie: &str,
    ) -> std::result::Result<(BrowserIdentity, String), Response> {
        let _lock = self.store.lock(cookie).await.map_err(|_| unavailable())?;
        let now = now_ms() as i64;
        let mut session = self
            .store
            .load_session(cookie, now)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(unauthorized)?;
        if session.refresh_state != "ready" {
            self.store.revoke(cookie).await.map_err(|_| unavailable())?;
            return Err(unauthorized()); // previous process may have spent the refresh token
        }
        if session.access_expires_ms <= now + 30_000 {
            if !self
                .store
                .begin_refresh(cookie, session.revision, now)
                .await
                .map_err(|_| unavailable())?
            {
                return Err(unauthorized());
            }
            let result = tokio::time::timeout(
                Duration::from_secs(25),
                self.provider
                    .refresh(&session.secrets, &session.subject, now),
            )
            .await;
            let tokens = match result {
                Ok(Ok(tokens)) => tokens,
                _ => {
                    self.store.revoke(cookie).await.map_err(|_| unavailable())?;
                    return Err(unauthorized());
                }
            };
            if auth
                .verify_jwt(&tokens.access, now_ms())
                .await
                .ok()
                .as_deref()
                != Some(session.subject.as_str())
            {
                self.store.revoke(cookie).await.map_err(|_| unavailable())?;
                return Err(unauthorized());
            }
            session.secrets.access_token = tokens.access;
            session.secrets.refresh_token = tokens.refresh;
            if !self
                .store
                .finish_refresh(
                    cookie,
                    session.revision,
                    &session.secrets,
                    tokens.expires_ms,
                    now_ms() as i64,
                )
                .await
                .map_err(|_| unavailable())?
            {
                return Err(unauthorized());
            }
        }
        // Signed access token validation includes the product account gate.
        let subject = auth
            .verify_jwt(&session.secrets.access_token, now_ms())
            .await
            .map_err(auth_failure)?;
        if subject != session.subject {
            return Err(unauthorized());
        }
        let account = auth
            .management
            .as_ref()
            .ok_or_else(unavailable)?
            .active_identity(&auth.config.issuer, &subject)
            .await
            .map_err(|error| {
                if error.downcast_ref::<crate::management::DomainError>()
                    == Some(&crate::management::DomainError::Forbidden)
                {
                    forbidden()
                } else {
                    unavailable()
                }
            })?;
        if account.id != session.account_id {
            return Err(forbidden());
        }
        if !self
            .store
            .touch(cookie, now_ms() as i64)
            .await
            .map_err(|_| unavailable())?
        {
            return Err(unauthorized());
        }
        Ok((
            BrowserIdentity {
                cookie: cookie.into(),
                csrf: session.secrets.csrf_token,
                account,
            },
            session.secrets.access_token,
        ))
    }
}

// Reject duplicate named cookies rather than depending on first/last wins. The
// capability is deliberately not URL-decoded or accepted in any query parameter.
fn cookie_value(headers: &HeaderMap, name: &str) -> Result<Option<String>> {
    let mut result = None;
    for line in headers.get_all(header::COOKIE) {
        let line = line.to_str()?;
        ensure!(line.len() <= 8192, "cookie header too large");
        for part in line.split(';') {
            if let Some((key, value)) = part.trim().split_once('=') {
                if key == name {
                    ensure!(result.is_none(), "duplicate session cookie");
                    store::cookie_digest(value)?;
                    result = Some(value.into());
                }
            }
        }
    }
    Ok(result)
}
fn browser_path(path: &str, method: &Method) -> bool {
    path == "/v1/session"
        || path == "/v1/account"
        || path.starts_with("/v1/account/")
        || path.starts_with("/v1/admin/")
        || path == "/auth/logout"
        || path == "/v1/macs" && method == Method::GET
}
pub async fn middleware(
    State(server): State<Arc<Server>>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let Some(web) = &server.web_session else {
        return next.run(request).await;
    };
    let path = request.uri().path();
    if path.starts_with("/v1/invitations/") && !web.check_origin(request.headers()) {
        return forbidden();
    }
    if !browser_path(path, request.method()) {
        return next.run(request).await;
    }
    let cookie = match cookie_value(request.headers(), web.session_name()) {
        Ok(value) => value,
        Err(_) => return unauthorized(),
    };
    // Native account APIs may still use a bearer. Cookie and bearer together are
    // ambiguous and never resolved by trusting an attacker-supplied header.
    let Some(cookie) = cookie else {
        return next.run(request).await;
    };
    if request.headers().contains_key(header::AUTHORIZATION) {
        return forbidden();
    }
    let _permit = match web.permits.try_acquire() {
        Ok(value) => value,
        Err(_) => return unavailable(),
    };
    let (identity, token) = match web.authenticate(&server.auth, &cookie).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    if request.method() != Method::GET && request.method() != Method::HEAD {
        let csrf = request
            .headers()
            .get("x-csrf-token")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        use subtle::ConstantTimeEq;
        if !web.check_origin(request.headers())
            || !bool::from(digest(csrf).ct_eq(&digest(&identity.csrf)))
        {
            return forbidden();
        }
    }
    let bearer = match HeaderValue::from_str(&format!("Bearer {token}")) {
        Ok(value) => value,
        Err(_) => return unauthorized(),
    };
    request.headers_mut().insert(header::AUTHORIZATION, bearer);
    request.extensions_mut().insert(identity);
    next.run(request).await
}
async fn login(State(server): State<Arc<Server>>) -> Response {
    let Some(web) = &server.web_session else {
        return unavailable();
    };
    let _permit = match web.permits.try_acquire() {
        Ok(value) => value,
        Err(_) => return unavailable(),
    };
    let login = match tokio::time::timeout(Duration::from_secs(20), web.provider.login()).await {
        Ok(Ok(value)) => value,
        _ => return unavailable(),
    };
    let cookie = match random_token() {
        Ok(value) => value,
        Err(_) => return unavailable(),
    };
    if web
        .store
        .create_flow(&cookie, &login.state, &login.secrets, now_ms() as i64)
        .await
        .is_err()
    {
        return unavailable();
    }
    let mut response = Redirect::to(&login.url).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        web.cookie(web.flow_name(), &cookie, store::FLOW_MAX_MS / 1000)
            .parse()
            .unwrap(),
    );
    response
}
#[derive(Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    iss: Option<String>,
}
async fn callback(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Query(query): Query<Callback>,
) -> Response {
    let Some(web) = &server.web_session else {
        return unavailable();
    };
    let _permit = match web.permits.try_acquire() {
        Ok(value) => value,
        Err(_) => return unavailable(),
    };
    let mut consumed = false;
    let result = async {
        ensure!(
            query.error.is_none()
                && query
                    .iss
                    .as_deref()
                    .is_none_or(|issuer| issuer == server.auth.config.issuer),
            "OIDC callback rejected"
        );
        let cookie = cookie_value(&headers, web.flow_name())?.context("login cookie missing")?;
        let state = query.state.context("login state missing")?;
        ensure!(state.len() <= 1024, "invalid login state");
        let flow = web
            .store
            .consume_flow(&cookie, &state, now_ms() as i64)
            .await?;
        consumed = true;
        let tokens = web
            .provider
            .exchange(
                query.code.context("login code missing")?,
                &flow,
                now_ms() as i64,
            )
            .await?;
        let subject = server.auth.verify_jwt(&tokens.access, now_ms()).await?;
        ensure!(subject == tokens.subject, "OIDC subject mismatch");
        let account = server
            .auth
            .management
            .as_ref()
            .context("management unavailable")?
            .active_identity(&server.auth.config.issuer, &subject)
            .await?;
        let session_cookie = random_token()?;
        let secrets = SessionSecrets {
            access_token: tokens.access,
            refresh_token: tokens.refresh,
            csrf_token: random_token()?,
            nonce: flow.nonce,
        };
        web.store
            .create_session(
                &session_cookie,
                account.id,
                &subject,
                &secrets,
                tokens.expires_ms,
                now_ms() as i64,
            )
            .await?;
        // Rotate capability even if this browser had an earlier valid session.
        if let Some(previous) = cookie_value(&headers, web.session_name())? {
            let _lock = web.store.lock(&previous).await?;
            web.store.revoke(&previous).await?;
        }
        Ok::<_, anyhow::Error>(session_cookie)
    };
    let mut response = match tokio::time::timeout(Duration::from_secs(30), result).await {
        Ok(Ok(cookie)) => {
            let mut response = Redirect::to("/account").into_response();
            response.headers_mut().append(
                header::SET_COOKIE,
                web.cookie(web.session_name(), &cookie, store::SESSION_MAX_MS / 1000)
                    .parse()
                    .unwrap(),
            );
            response
        }
        _ => Redirect::to("/login-error").into_response(),
    };
    if consumed {
        response.headers_mut().append(
            header::SET_COOKIE,
            web.cookie(web.flow_name(), "", 0).parse().unwrap(),
        );
    }
    response
}
async fn session(identity: Option<axum::Extension<BrowserIdentity>>) -> Response {
    match identity {
        Some(axum::Extension(value)) => {
            Json(json!({"csrf_token":value.csrf,"account":value.account})).into_response()
        }
        None => unauthorized(),
    }
}
async fn logout(
    State(server): State<Arc<Server>>,
    identity: Option<axum::Extension<BrowserIdentity>>,
) -> Response {
    let (Some(web), Some(axum::Extension(identity))) = (&server.web_session, identity) else {
        return unauthorized();
    };
    let _lock = match web.store.lock(&identity.cookie).await {
        Ok(value) => value,
        Err(_) => return unavailable(),
    };
    if web.store.revoke(&identity.cookie).await.is_err() {
        return unavailable();
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        web.cookie(web.session_name(), "", 0).parse().unwrap(),
    );
    response
}

pub async fn reconcile(web: Arc<WebSession>) {
    loop {
        let result = async {
            for hash in web.store.revoke_expired(now_ms() as i64).await? {
                let _lock = web.store.lock_hash(&hash).await?;
                web.store.expire_locked(&hash, now_ms() as i64).await?;
                if let Some(token) = web.store.revoked_token(&hash).await? {
                    if web.provider.revoke(&token).await.is_ok() {
                        web.store.forget_revoked(&hash).await?;
                    }
                }
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if result.is_err() {
            eprintln!("alondra-relay: browser session reconciliation unavailable");
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cookie_parser_rejects_duplicates_and_invalid_capabilities() {
        let token = random_token().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("a=1; session={token}").parse().unwrap(),
        );
        assert_eq!(
            cookie_value(&headers, "session").unwrap(),
            Some(token.clone())
        );
        headers.append(header::COOKIE, format!("session={token}").parse().unwrap());
        assert!(cookie_value(&headers, "session").is_err());
        headers.insert(header::COOKIE, "session=short".parse().unwrap());
        assert!(cookie_value(&headers, "session").is_err());
    }
    #[test]
    fn browser_credentials_never_authorize_content_transport() {
        for path in [
            "/v1/changes",
            "/v1/work",
            "/v1/requests",
            "/v1/devices",
            "/v1/push/register",
            "/v1/pairings",
        ] {
            assert!(!browser_path(path, &Method::GET));
            assert!(!browser_path(path, &Method::POST));
        }
        assert!(browser_path("/v1/macs", &Method::GET));
        assert!(!browser_path("/v1/macs", &Method::POST));
    }
}
