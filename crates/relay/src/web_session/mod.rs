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
    // Keep at most two authentication pipelines holding session advisory locks.
    // Short bursts can wait; the outer request semaphore bounds queued work.
    async fn acquire_auth(
        &self,
    ) -> std::result::Result<tokio::sync::SemaphorePermit<'_>, Response> {
        match tokio::time::timeout(Duration::from_millis(500), self.permits.acquire()).await {
            Ok(Ok(permit)) => Ok(permit),
            _ => Err(unavailable()),
        }
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
            // Discovery is read-only. A transient failure here has not spent the
            // refresh token and must not force another login.
            let prepared = self
                .provider
                .prepare_refresh()
                .await
                .map_err(|_| unavailable())?;
            if !self
                .store
                .begin_refresh(cookie, session.revision, now_ms() as i64)
                .await
                .map_err(|_| unavailable())?
            {
                return Err(unauthorized());
            }
            let result = tokio::time::timeout(
                Duration::from_secs(25),
                self.provider.refresh(
                    &prepared,
                    &session.secrets,
                    &session.subject,
                    now_ms() as i64,
                ),
            )
            .await;
            let tokens = match result {
                Ok(Ok(tokens)) => tokens,
                _ => {
                    self.store.revoke(cookie).await.map_err(|_| unavailable())?;
                    return Err(unauthorized());
                }
            };
            session.secrets.access_token = tokens.access;
            session.secrets.refresh_token = tokens.refresh;
            if !self
                .store
                .retain_rotated(
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
            if !tokens.valid_id
                || auth
                    .verify_jwt(&session.secrets.access_token, now_ms())
                    .await
                    .ok()
                    .as_deref()
                    != Some(session.subject.as_str())
            {
                self.store.revoke(cookie).await.map_err(|_| unavailable())?;
                return Err(unauthorized());
            }
            if !self
                .store
                .activate_rotated(cookie, session.revision + 1, now_ms() as i64)
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
    // Logout validates its own local capability and CSRF under a lock; do not
    // refresh a token or require an active account merely to erase that session.
    if path == "/auth/logout" && request.method() == Method::POST {
        return logout(State(server.clone()), request.headers().clone()).await;
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
    if request.method() != Method::GET
        && request.method() != Method::HEAD
        && !web.check_origin(request.headers())
    {
        return forbidden();
    }
    let permit = match web.acquire_auth().await {
        Ok(value) => value,
        Err(response) => return response,
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
    // The session lock has already been released by authenticate. Product
    // handlers must not occupy the small authentication admission budget.
    drop(permit);
    next.run(request).await
}
async fn login(State(server): State<Arc<Server>>) -> Response {
    let Some(web) = &server.web_session else {
        return unavailable();
    };
    let _permit = match web.acquire_auth().await {
        Ok(value) => value,
        Err(response) => return response,
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
    let _permit = match web.acquire_auth().await {
        Ok(value) => value,
        Err(response) => return response,
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
        // Rotate only the local browser capability. Keycloak refresh tokens may
        // share a client session; revoking the old grant could kill this new login.
        if let Some(previous) = cookie_value(&headers, web.session_name())? {
            let _lock = web.store.lock(&previous).await?;
            web.store.forget_session(&previous).await?;
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
async fn logout(State(server): State<Arc<Server>>, headers: HeaderMap) -> Response {
    let Some(web) = &server.web_session else {
        return unavailable();
    };
    // This is only local browser logout, not Keycloak SSO logout. Never accept
    // an injected bearer alongside the cookie or turn it into account authority.
    if !web.check_origin(&headers) || headers.contains_key(header::AUTHORIZATION) {
        return forbidden();
    }
    let cookie = match cookie_value(&headers, web.session_name()) {
        Ok(value) => value,
        Err(_) => return unauthorized(),
    };
    let _permit = match web.acquire_auth().await {
        Ok(value) => value,
        Err(response) => return response,
    };
    if let Some(cookie) = cookie {
        let _lock = match web.store.lock(&cookie).await {
            Ok(value) => value,
            Err(_) => return unavailable(),
        };
        let csrf = match web.store.logout_csrf(&cookie).await {
            Ok(value) => value,
            Err(_) => return unavailable(),
        };
        if let Some(expected) = csrf {
            let mut supplied = headers.get_all("x-csrf-token").iter();
            let actual = supplied
                .next()
                .and_then(|value| value.to_str().ok())
                .unwrap_or("");
            use subtle::ConstantTimeEq;
            if supplied.next().is_some() || !bool::from(digest(actual).ct_eq(&digest(&expected))) {
                return forbidden();
            }
            if web.store.forget_session(&cookie).await.is_err() {
                return unavailable();
            }
        }
        // Already absent is idempotent cleanup: there is no remaining authority
        // or encrypted credential to delete and no provider request to replay.
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
                // Local logout/expiry is not Keycloak SSO logout. Tokens never
                // reach the browser; deleting its capability and encrypted secrets
                // prevents reuse without revoking another login's shared grant.
                web.store.forget_revoked(&hash).await?;
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
    #[tokio::test]
    #[ignore = "requires disposable local PostgreSQL and loopback HTTP"]
    async fn http_logout_works_without_identity_provider_for_expired_and_revoked_sessions()
    -> Result<()> {
        use base64::Engine as _;
        use sqlx_core::{query::query, row::Row};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let idp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let issuer = format!("http://{}/realms/test", idp_listener.local_addr()?);
        let mode = Arc::new(AtomicUsize::new(0));
        let token_calls = Arc::new(AtomicUsize::new(0));
        let mock_mode = mode.clone();
        let mock_tokens = token_calls.clone();
        let mock_issuer = issuer.clone();
        let idp = tokio::spawn(async move {
            axum::serve(
                idp_listener,
                Router::new().fallback(move |uri: axum::http::Uri, method: Method| {
                    let calls = observed.clone();
                    let mode = mock_mode.clone();
                    let tokens = mock_tokens.clone();
                    let issuer = mock_issuer.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        if mode.load(Ordering::SeqCst) == 0 { return StatusCode::SERVICE_UNAVAILABLE.into_response(); }
                        match uri.path() {
                            "/realms/test/.well-known/openid-configuration" => Json(json!({
                                "issuer":issuer, "authorization_endpoint":format!("{issuer}/authorize"),
                                "token_endpoint":format!("{issuer}/token"), "jwks_uri":format!("{issuer}/jwks"),
                                "response_types_supported":["code"], "subject_types_supported":["public"],
                                "id_token_signing_alg_values_supported":["RS256"],
                                "token_endpoint_auth_methods_supported":["client_secret_basic"]
                            })).into_response(),
                            "/realms/test/jwks" => ([(header::CONTENT_TYPE,"application/json")], include_str!("../../tests/fixtures/jwks.json")).into_response(),
                            "/realms/test/token" if method == Method::POST => {
                                tokens.fetch_add(1, Ordering::SeqCst);
                                Json(json!({"access_token":"invalid-issued-access", "refresh_token":"rotated-before-jwt-denial", "token_type":"Bearer", "expires_in":300})).into_response()
                            }
                            _ => StatusCode::NOT_FOUND.into_response(),
                        }
                    }
                }),
            )
            .await
            .unwrap();
        });
        let (db, account, schema) = crate::management::tests::fixture_for_issuer(&issuer).await?;
        sqlx_core::raw_sql::raw_sql(include_str!("../../migrations/0002_web_sessions.sql"))
            .execute(&db.pool)
            .await?;
        let scratch_directory = crate::test_support::Directory::new();
        let registry = scratch_directory.parent().unwrap().join(format!(
            "alondra-web-logout-test-{}.json",
            uuid::Uuid::new_v4()
        ));
        let signer = p256::ecdsa::SigningKey::from_slice(&[11; 32])?;
        let config = crate::auth::Config {
            listen: "127.0.0.1:0".into(),
            issuer: issuer.clone(),
            audience: "alondra-relay".into(),
            jwks_url: format!("{issuer}/jwks"),
            allow_internal_http_jwks: false,
            owner_sub: "stable-admin-sub".into(),
            mac_id: "mac-test".into(),
            mac_signing_key: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(signer.verifying_key().to_sec1_point(false).as_bytes()),
            device_registry_path: registry.clone(),
            apns: None,
        };
        let mut auth = Auth::test(config, include_bytes!("../../tests/fixtures/jwks.json"))?;
        auth.management = Some(db.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let web = Arc::new(WebSession {
            store: Store::from_key(db.pool.clone(), &[7; 32])?,
            provider: oidc::Provider::new(
                &issuer,
                &origin,
                "synthetic-web".into(),
                "synthetic-secret".into(),
            )?,
            origin: origin.clone(),
            secure: false,
            permits: Semaphore::new(2),
        });
        let server = Arc::new(Server {
            auth,
            management_keycloak: None,
            web_session: Some(web.clone()),
            web_assets: None,
            apns: None,
            pairing: crate::pairing::Mailbox::default(),
            requests: Arc::new(Semaphore::new(8)),
            device_gate: Arc::new(tokio::sync::RwLock::new(())),
            waitlist: crate::management::waitlist::Limiter::new(None),
            epoch: "synthetic".into(),
            iroh_access_token: None,
            iroh_relay: None,
            test_directory: Some(scratch_directory),
        });
        let run = tokio::spawn(async move {
            axum::serve(listener, crate::router(server)).await.unwrap();
        });
        let http = reqwest::Client::new();
        let now = now_ms() as i64;
        for state in ["expired", "refreshing", "revoked"] {
            let cookie = random_token()?;
            let csrf = random_token()?;
            let secrets = SessionSecrets {
                access_token: "deliberately-invalid-jwt".into(),
                refresh_token: "synthetic-refresh".into(),
                csrf_token: csrf.clone(),
                nonce: random_token()?,
            };
            web.store
                .create_session(
                    &cookie,
                    account.id,
                    "stable-admin-sub",
                    &secrets,
                    now - 1,
                    now - store::SESSION_MAX_MS - 1,
                )
                .await?;
            if state != "expired" {
                query("UPDATE web_sessions SET refresh_state=$2 WHERE cookie_hash=$1")
                    .bind(store::cookie_digest(&cookie)?)
                    .bind(state)
                    .execute(&db.pool)
                    .await?;
            }
            let request = || {
                http.post(format!("{origin}/auth/logout"))
                    .header(header::COOKIE, format!("{}={cookie}", web.session_name()))
            };
            assert_eq!(
                request()
                    .header(header::ORIGIN, "https://adversary.example")
                    .header("x-csrf-token", &csrf)
                    .send()
                    .await?
                    .status(),
                StatusCode::FORBIDDEN
            );
            assert_eq!(
                request()
                    .header(header::ORIGIN, &origin)
                    .header("x-csrf-token", "wrong")
                    .send()
                    .await?
                    .status(),
                StatusCode::FORBIDDEN
            );
            assert_eq!(
                request()
                    .header(header::ORIGIN, &origin)
                    .header("x-csrf-token", &csrf)
                    .bearer_auth("injected")
                    .send()
                    .await?
                    .status(),
                StatusCode::FORBIDDEN
            );
            assert_eq!(
                web.store.logout_csrf(&cookie).await?.as_deref(),
                Some(csrf.as_str()),
                "rejected requests must preserve the row"
            );
            let response = request()
                .header(header::ORIGIN, &origin)
                .header("x-csrf-token", &csrf)
                .send()
                .await?;
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            assert!(
                response.headers()[header::SET_COOKIE]
                    .to_str()?
                    .contains("Max-Age=0")
            );
            assert!(web.store.logout_csrf(&cookie).await?.is_none());
            assert_eq!(
                request()
                    .header(header::ORIGIN, &origin)
                    .send()
                    .await?
                    .status(),
                StatusCode::NO_CONTENT,
                "retry after deletion needs no now-unavailable CSRF row"
            );
            assert_eq!(
                request()
                    .header(header::ORIGIN, "https://adversary.example")
                    .send()
                    .await?
                    .status(),
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "local logout must never contact the failing identity provider"
        );
        // A hostile unsafe request cannot even start discovery/refresh; a
        // discovery failure on a legitimate read must leave the token unspent.
        let ready_cookie = random_token()?;
        let ready_secrets = SessionSecrets {
            access_token: "deliberately-invalid-jwt".into(),
            refresh_token: "unspent-refresh".into(),
            csrf_token: random_token()?,
            nonce: random_token()?,
        };
        web.store
            .create_session(
                &ready_cookie,
                account.id,
                "stable-admin-sub",
                &ready_secrets,
                now - 1,
                now,
            )
            .await?;
        let cookie_header = format!("{}={ready_cookie}", web.session_name());
        assert_eq!(
            http.delete(format!("{origin}/v1/account"))
                .header(header::COOKIE, &cookie_header)
                .header(header::ORIGIN, "https://adversary.example")
                .header("x-csrf-token", &ready_secrets.csrf_token)
                .send()
                .await?
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let before = web.store.load_session(&ready_cookie, now).await?.unwrap();
        assert_eq!(before.refresh_state, "ready");
        assert_eq!(before.revision, 0);
        assert_eq!(
            http.get(format!("{origin}/v1/session"))
                .header(header::COOKIE, &cookie_header)
                .send()
                .await?
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "discovery failed before any token POST"
        );
        let after = web.store.load_session(&ready_cookie, now).await?.unwrap();
        assert_eq!(after.refresh_state, "ready");
        assert_eq!(after.revision, 0);
        assert_eq!(after.secrets.refresh_token, "unspent-refresh");
        // Now allow real discovery + token HTTP, but issue an invalid access JWT.
        // The new refresh credential must be durable before that validation fails.
        mode.store(1, Ordering::SeqCst);
        assert_eq!(
            http.get(format!("{origin}/v1/session"))
                .header(header::COOKIE, &cookie_header)
                .send()
                .await?
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(token_calls.load(Ordering::SeqCst), 1);
        let row = query("SELECT revision,refresh_state FROM web_sessions WHERE cookie_hash=$1")
            .bind(store::cookie_digest(&ready_cookie)?)
            .fetch_one(&db.pool)
            .await?;
        assert_eq!(row.try_get::<i64, _>("revision")?, 1);
        assert_eq!(row.try_get::<String, _>("refresh_state")?, "revoked");
        assert_eq!(
            web.store
                .revoked_token(&store::cookie_digest(&ready_cookie)?)
                .await?
                .as_deref(),
            Some("rotated-before-jwt-denial")
        );
        assert_eq!(
            http.get(format!("{origin}/v1/session"))
                .header(header::COOKIE, &cookie_header)
                .send()
                .await?
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            token_calls.load(Ordering::SeqCst),
            1,
            "denied session must never replay its refresh"
        );
        web.store.forget_session(&ready_cookie).await?;
        // Admission queues a short burst without broadening concurrency. Hold
        // both slots deterministically, then release one after HTTP is in flight.
        let queued_cookie = random_token()?;
        web.store
            .create_session(
                &queued_cookie,
                account.id,
                "stable-admin-sub",
                &ready_secrets,
                now - 1,
                now,
            )
            .await?;
        let held = web.permits.acquire_many(2).await?;
        assert_eq!(
            http.post(format!("{origin}/auth/logout"))
                .header(header::ORIGIN, "https://adversary.example")
                .header(
                    header::COOKIE,
                    format!("{}={queued_cookie}", web.session_name())
                )
                .send()
                .await?
                .status(),
            StatusCode::FORBIDDEN
        );
        let pending = http
            .post(format!("{origin}/auth/logout"))
            .header(header::ORIGIN, &origin)
            .header(
                header::COOKIE,
                format!("{}={queued_cookie}", web.session_name()),
            )
            .header("x-csrf-token", &ready_secrets.csrf_token)
            .send();
        tokio::pin!(pending);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut pending)
                .await
                .is_err(),
            "third request must wait, not return an immediate 503"
        );
        drop(held);
        assert_eq!(pending.await?.status(), StatusCode::NO_CONTENT);
        assert!(web.store.logout_csrf(&queued_cookie).await?.is_none());
        let saturated = random_token()?;
        web.store
            .create_session(
                &saturated,
                account.id,
                "stable-admin-sub",
                &ready_secrets,
                now - 1,
                now,
            )
            .await?;
        let held = web.permits.acquire_many(2).await?;
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(2),
                http.post(format!("{origin}/auth/logout"))
                    .header(header::ORIGIN, &origin)
                    .header(
                        header::COOKIE,
                        format!("{}={saturated}", web.session_name())
                    )
                    .header("x-csrf-token", &ready_secrets.csrf_token)
                    .send()
            )
            .await??
            .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(
            web.store.logout_csrf(&saturated).await?.is_some(),
            "admission timeout must not alter the credential"
        );
        drop(held);
        web.store.forget_session(&saturated).await?;
        let remaining: i64 = query("SELECT count(*) AS n FROM web_sessions")
            .fetch_one(&db.pool)
            .await?
            .try_get("n")?;
        assert_eq!(remaining, 0);
        run.abort();
        idp.abort();
        let _ = run.await;
        let _ = idp.await;
        drop(web);
        crate::management::tests::cleanup(db, schema).await?;
        let _ = std::fs::remove_file(registry);
        Ok(())
    }
}
