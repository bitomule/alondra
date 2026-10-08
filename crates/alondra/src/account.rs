//! Interactive service login for the macOS CLI. The local daemon never reads this Keychain item.
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom, SystemRandom};
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use url::Url;

const ISSUER: &str = "https://alondra-auth.davidcollado.dev/realms/alondra";
const CLIENT_ID: &str = "alondra-cli";
const KEYCHAIN_SERVICE: &str = "dev.davidcollado.alondra.service-login";
const CALLBACK_PATH: &str = "/oauth2redirect/alondra-cli";
const LOGIN_SCOPE: &str = "openid profile offline_access";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

#[cfg(all(test, target_os = "macos"))]
#[path = "account_native_smoke.rs"]
mod native_smoke;

#[derive(Debug)]
struct SessionExpired;

impl std::fmt::Display for SessionExpired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Alondra login expired or revoked")
    }
}

impl std::error::Error for SessionExpired {}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    userinfo_endpoint: String,
    revocation_endpoint: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct Userinfo {
    sub: String,
    preferred_username: Option<String>,
}

#[derive(Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionState {
    #[default]
    Active,
    PendingRevocation,
}

#[derive(Clone, Serialize, Deserialize)]
struct Session {
    issuer: String,
    client_id: String,
    sub: String,
    refresh_token: String,
    #[serde(default)]
    state: SessionState,
}

#[derive(Clone)]
pub struct AccountToken {
    pub sub: String,
    pub access_token: String,
    valid_for: Duration,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .build(),
        )
        .timeout_global(Some(Duration::from_secs(10)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into()
}

fn endpoint(issuer: &str, value: &str) -> Result<Url> {
    let issuer = Url::parse(issuer)?;
    let url = Url::parse(value)?;
    if issuer.scheme() != "https"
        || url.scheme() != "https"
        || issuer.host_str() != url.host_str()
        || issuer.port() != url.port()
        || !url.username().is_empty()
        || url.password().is_some()
        || !url
            .path()
            .starts_with(&format!("{}/", issuer.path().trim_end_matches('/')))
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("OIDC endpoint is outside the expected HTTPS issuer");
    }
    Ok(url)
}

fn discovery(http: &ureq::Agent) -> Result<Discovery> {
    let url = format!("{ISSUER}/.well-known/openid-configuration");
    let mut response = http.get(&url).call().context("OIDC discovery failed")?;
    if response.status().as_u16() != 200 {
        bail!("OIDC discovery returned {}", response.status());
    }
    let d: Discovery = response
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_json()?;
    if d.issuer != ISSUER {
        bail!("unexpected OIDC issuer");
    }
    endpoint(ISSUER, &d.authorization_endpoint)?;
    endpoint(ISSUER, &d.token_endpoint)?;
    endpoint(ISSUER, &d.userinfo_endpoint)?;
    if let Some(revocation) = &d.revocation_endpoint {
        endpoint(ISSUER, revocation)?;
    }
    Ok(d)
}

fn random_b64(bytes: usize) -> Result<String> {
    let mut value = vec![0; bytes];
    SystemRandom::new()
        .fill(&mut value)
        .map_err(|_| anyhow!("OS randomness failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(value))
}

fn form(fields: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().copied())
        .finish()
}

fn post_form(http: &ureq::Agent, url: &str, fields: &[(&str, &str)]) -> Result<(u16, String)> {
    let encoded = form(fields);
    let mut response = http
        .post(url)
        .header("content-type", "application/x-www-form-urlencoded")
        .send(encoded.as_bytes())?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_string()?;
    Ok((status, body))
}

fn revoke_token(http: &ureq::Agent, d: &Discovery, refresh_token: &str) -> Result<()> {
    let url = d
        .revocation_endpoint
        .as_deref()
        .context("issuer has no revocation endpoint")?;
    let (status, _) = post_form(
        http,
        url,
        &[
            ("client_id", CLIENT_ID),
            ("token", refresh_token),
            ("token_type_hint", "refresh_token"),
        ],
    )?;
    if status != 200 {
        bail!("OIDC token revocation failed ({status})");
    }
    Ok(())
}

fn userinfo(http: &ureq::Agent, d: &Discovery, access_token: &str) -> Result<Userinfo> {
    let mut response = http
        .get(&d.userinfo_endpoint)
        .header("authorization", format!("Bearer {access_token}"))
        .call()?;
    if response.status().as_u16() != 200 {
        bail!("OIDC userinfo returned {}", response.status());
    }
    let user: Userinfo = response
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_json()?;
    if user.sub.is_empty() {
        bail!("OIDC userinfo omitted subject");
    }
    Ok(user)
}

fn callback_target(raw: &str, expected_state: &str, expected_port: u16) -> Result<Option<String>> {
    let first = raw.lines().next().context("empty callback")?;
    let mut parts = first.split_ascii_whitespace();
    if parts.next() != Some("GET") {
        bail!("invalid callback method");
    }
    let path = parts.next().context("missing callback target")?;
    let url = Url::parse(&format!("http://127.0.0.1{path}"))?;
    if url.path() != CALLBACK_PATH {
        return Ok(None);
    }
    let header_host = raw.lines().find_map(|l| {
        l.strip_prefix("Host: ")
            .or_else(|| l.strip_prefix("host: "))
    });
    if header_host.is_none_or(|h| h.trim() != format!("127.0.0.1:{expected_port}")) {
        bail!("invalid callback host");
    }
    let mut state = None;
    let mut code = None;
    let mut error = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "state" => {
                if state.replace(value.into_owned()).is_some() {
                    bail!("duplicate callback state");
                }
            }
            "code" => {
                if code.replace(value.into_owned()).is_some() {
                    bail!("duplicate callback code");
                }
            }
            "error" => {
                if error.replace(value.into_owned()).is_some() {
                    bail!("duplicate callback error");
                }
            }
            _ => {}
        }
    }
    let state = state.context("callback omitted state")?;
    if !bool::from(state.as_bytes().ct_eq(expected_state.as_bytes())) {
        bail!("callback state mismatch");
    }
    if let Some(error) = error {
        bail!("login refused: {error}");
    }
    let code = code.context("callback omitted code")?;
    if code.is_empty() || code.len() > 4096 {
        bail!("invalid authorization code");
    }
    Ok(Some(code))
}

fn read_request(reader: &mut impl Read) -> Result<String> {
    let mut request = Vec::with_capacity(512);
    let mut chunk = [0u8; 1024];
    while request.len() < 8192 {
        let size = reader.read(&mut chunk)?;
        if size == 0 {
            bail!("incomplete callback HTTP request");
        }
        request.extend_from_slice(&chunk[..size]);
        if request.len() > 8192 {
            bail!("callback HTTP request too large");
        }
        if request.windows(4).any(|part| part == b"\r\n\r\n") {
            return Ok(std::str::from_utf8(&request)?.to_owned());
        }
    }
    bail!("callback HTTP request too large")
}

fn receive_callback(listener: TcpListener, state: &str) -> Result<String> {
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, peer)) => {
                if !peer.ip().is_loopback() {
                    continue;
                }
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                let result =
                    read_request(&mut stream).and_then(|raw| callback_target(&raw, state, port));
                let success = matches!(result, Ok(Some(_)));
                respond(&mut stream, success);
                if let Ok(Some(code)) = result {
                    return Ok(code);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100))
            }
            Err(e) => return Err(e.into()),
        }
    }
    bail!("login timed out; run `alondra login` again")
}

fn respond(stream: &mut TcpStream, success: bool) {
    let (status, body) = if success {
        (
            "200 OK",
            "Alondra: login completed. You may close this tab.",
        )
    } else {
        (
            "400 Bad Request",
            "Alondra: login was not completed. Return to the terminal.",
        )
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

// Only errSecItemNotFound means the user has no saved login. In particular,
// a locked Keychain or a denied ACL must never launch a replacement browser login.
fn decode_stored(bytes: Result<Vec<u8>, security_framework::base::Error>) -> Result<Option<Session>> {
    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(error) if error.code() == -25300 => return Ok(None), // errSecItemNotFound
        Err(error) => return Err(error).context("Alondra login is saved in Keychain but cannot be accessed; unlock the login Keychain or restore access for the signed Alondra CLI"),
    };
    let session: Session = serde_json::from_slice(&bytes)
        .context("saved Alondra login is invalid; repair or remove its Keychain item before running `alondra login`")?;
    if session.issuer != ISSUER || session.client_id != CLIENT_ID || session.sub.is_empty() {
        bail!("Keychain login belongs to another issuer or client");
    }
    Ok(Some(session))
}

fn stored_optional() -> Result<Option<Session>> {
    decode_stored(get_generic_password(KEYCHAIN_SERVICE, ISSUER))
}

fn require_active(session: &Session) -> Result<()> {
    if session.state == SessionState::PendingRevocation {
        bail!("signed out locally; server revocation is pending; run `alondra account` when online or `alondra login` to reconnect");
    }
    Ok(())
}

fn stored() -> Result<Session> {
    let session = stored_optional()?.context("no Alondra service login in Keychain; run `alondra login`")?;
    require_active(&session)?;
    Ok(session)
}

pub fn login() -> Result<()> {
    let _lock = AccountLock::acquire()?;
    let previous = stored_optional()?;
    let http = agent();
    let d = discovery(&http)?;
    // Keycloak may reuse an offline session across authorizations. Revoking the
    // previous grant after code exchange could revoke the newly issued grant too.
    replace_previous_login(previous.as_ref(),
        |old| revoke_token(&http, &d, &old.refresh_token),
        || delete_generic_password(KEYCHAIN_SERVICE, ISSUER).map_err(Into::into),
    )?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let redirect = format!(
        "http://127.0.0.1:{}{CALLBACK_PATH}",
        listener.local_addr()?.port()
    );
    let state = random_b64(32)?;
    let verifier = random_b64(32)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut url = Url::parse(&d.authorization_endpoint)?;
    url.query_pairs_mut()
        .append_pair("client_id", CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect)
        .append_pair("scope", LOGIN_SCOPE)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    println!("Opening Alondra login in your browser…");
    println!("If the browser does not appear, open this link within five minutes:\n{url}");
    if !Command::new("open")
        .arg(url.as_str())
        .status()
        .is_ok_and(|status| status.success())
    {
        eprintln!("Could not open the browser automatically; use the link above.");
    }
    let code = receive_callback(listener, &state)?;
    let (status, body) = post_form(
        &http,
        &d.token_endpoint,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", &redirect),
            ("code", &code),
            ("code_verifier", &verifier),
        ],
    )?;
    if status != 200 {
        bail!("OIDC token exchange failed ({status})");
    }
    let token: TokenResponse = serde_json::from_str(&body)?;
    let refresh = token
        .refresh_token
        .context("issuer did not issue a refresh token")?;
    let user = userinfo(&http, &d, &token.access_token)?;
    let session = Session {
        issuer: ISSUER.into(),
        client_id: CLIENT_ID.into(),
        sub: user.sub,
        refresh_token: refresh,
        state: SessionState::Active,
    };
    set_generic_password(KEYCHAIN_SERVICE, ISSUER, &serde_json::to_vec(&session)?)?;
    println!(
        "Signed in as {}.",
        user.preferred_username.as_deref().unwrap_or(&session.sub)
    );
    Ok(())
}

fn replace_previous_login(
    previous: Option<&Session>,
    revoke: impl FnOnce(&Session) -> Result<()>,
    clear: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if let Some(previous) = previous {
        revoke(previous).context("previous session could not be revoked; old Keychain login kept")?;
        clear().context("previous session revoked but its Keychain item could not be removed")?;
    }
    Ok(())
}

fn refreshed() -> Result<(AccountToken, Option<String>)> {
    let _lock = AccountLock::acquire()?;
    refreshed_locked()
}

fn refreshed_locked() -> Result<(AccountToken, Option<String>)> {
    let started = Instant::now();
    let session = stored()?;
    let http = agent();
    let d = discovery(&http)?;
    let (status, body) = post_form(
        &http,
        &d.token_endpoint,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", &session.refresh_token),
        ],
    )?;
    check_refresh_status(status, &body)?;
    let token: TokenResponse = serde_json::from_str(&body)?;
    complete_refresh(
        session, token, started,
        |updated| set_generic_password(KEYCHAIN_SERVICE, ISSUER, &serde_json::to_vec(updated)?)
            .context("could not save renewed Alondra login in Keychain"),
        |access| userinfo(&http, &d, access),
    )
}

fn check_refresh_status(status: u16, body: &str) -> Result<()> {
    #[derive(Deserialize)]
    struct OAuthError { error: String }
    if matches!(status, 400 | 401)
        && serde_json::from_str::<OAuthError>(body)
            .is_ok_and(|error| error.error == "invalid_grant") {
        return Err(SessionExpired.into());
    }
    if status != 200 {
        // Never echo response bodies, which can contain credentials or proxy diagnostics.
        bail!("OIDC token refresh failed ({status}); saved login retained");
    }
    Ok(())
}

fn complete_refresh(
    session: Session,
    token: TokenResponse,
    started: Instant,
    persist: impl FnOnce(&Session) -> Result<()>,
    identify: impl FnOnce(&str) -> Result<Userinfo>,
) -> Result<(AccountToken, Option<String>)> {
    require_active(&session)?;
    // Commit rotation before any further network operation. A failed userinfo call
    // must not strand a one-use refresh token. No access token leaves this function
    // until the account binding has been checked.
    if let Some(refresh) = token.refresh_token {
        persist(&Session { refresh_token: refresh, ..session.clone() })?;
    }
    let user = identify(&token.access_token)?;
    if user.sub != session.sub {
        bail!("account subject changed; run `alondra logout` and sign in again");
    }
    let lifetime = Duration::from_secs(token.expires_in.unwrap_or(0).saturating_sub(30));
    Ok((
        AccountToken {
            sub: user.sub,
            access_token: token.access_token,
            valid_for: lifetime.saturating_sub(started.elapsed()),
        },
        user.preferred_username,
    ))
}

pub fn token_for_enrollment() -> Result<AccountToken> {
    Ok(refreshed()?.0)
}

/// Noninteractive setup may refresh existing credentials, but must never replace
/// the login grant or open a browser when credentials are absent or expired.
pub(crate) fn token_for_setup(no_input: bool) -> Result<AccountToken> {
    setup_token_with(no_input, token_for_enrollment, ensure_token_for_enrollment)
}

fn setup_token_with(
    no_input: bool,
    refresh_existing: impl FnOnce() -> Result<AccountToken>,
    ensure_interactive: impl FnOnce() -> Result<AccountToken>,
) -> Result<AccountToken> {
    if no_input { refresh_existing() } else { ensure_interactive() }
}

pub fn ensure_token_for_enrollment() -> Result<AccountToken> {
    if stored_optional()?.is_none() {
        login()?;
    }
    match token_for_enrollment() {
        Ok(token) => Ok(token),
        Err(error) if error.is::<SessionExpired>() => {
            login()?;
            token_for_enrollment()
        }
        Err(error) => Err(error),
    }
}

pub fn status() -> Result<()> {
    let _lock = AccountLock::acquire()?;
    let session = stored_optional()?.context("no Alondra service login in Keychain; run `alondra login`")?;
    if session.state == SessionState::PendingRevocation {
        report_logout(finish_logout(session)?);
        return Ok(());
    }
    let (account, preferred_username) = refreshed_locked()?;
    println!("Signed in as {}.", preferred_username.as_deref().unwrap_or(&account.sub));
    Ok(())
}

pub fn logout() -> Result<()> {
    let _lock = AccountLock::acquire()?;
    let Some(session) = stored_optional()? else {
        println!("Signed out on this Mac.");
        return Ok(());
    };
    // The lock excludes every refresh/read/write until pending state is durable.
    report_logout(finish_logout(session)?);
    Ok(())
}

fn report_logout(revoked: bool) {
    println!("Signed out on this Mac.");
    if !revoked {
        eprintln!("Server revocation is pending. The saved credential cannot sign in; run `alondra account` when online to complete revocation.");
    }
}

fn finish_logout(session: Session) -> Result<bool> {
    logout_transition(session,
        |pending| set_generic_password(KEYCHAIN_SERVICE, ISSUER, &serde_json::to_vec(pending)?)
            .context("could not save local sign-out; logout was not completed"),
        |pending| {
            let http = agent();
            let d = discovery(&http)?;
            revoke_token(&http, &d, &pending.refresh_token)
        },
        || delete_generic_password(KEYCHAIN_SERVICE, ISSUER).map_err(Into::into),
    )
}

fn logout_transition(
    mut session: Session,
    persist: impl FnOnce(&Session) -> Result<()>,
    revoke: impl FnOnce(&Session) -> Result<()>,
    clear: impl FnOnce() -> Result<()>,
) -> Result<bool> {
    if session.state == SessionState::Active {
        session.state = SessionState::PendingRevocation;
        persist(&session)?;
    }
    if revoke(&session).is_err() { return Ok(false); }
    clear().context("server session revoked; local sign-out remains pending until Keychain cleanup succeeds")?;
    Ok(true)
}

#[cfg(test)]
mod persistent_session_tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn session() -> Session {
        Session { issuer: ISSUER.into(), client_id: CLIENT_ID.into(),
            sub: "owner".into(), refresh_token: "old-refresh".into(), state: SessionState::Active }
    }
    fn token() -> TokenResponse {
        TokenResponse { access_token: "access".into(), refresh_token: Some("new-refresh".into()), expires_in: Some(300) }
    }
    fn owner() -> Userinfo {
        Userinfo { sub: "owner".into(), preferred_username: None }
    }

    #[test]
    fn legacy_active_session_deserializes_and_offline_logout_survives_restart() {
        let mut legacy=serde_json::to_value(session()).unwrap();
        legacy.as_object_mut().unwrap().remove("state");
        let active: Session=serde_json::from_value(legacy).unwrap();
        assert!(active.state == SessionState::Active);
        let saved=RefCell::new(serde_json::to_vec(&active).unwrap());
        assert!(!logout_transition(active,
            |pending| { *saved.borrow_mut()=serde_json::to_vec(pending)?; Ok(()) },
            |_| bail!("offline"), || panic!("must retain revocation credential")).unwrap());
        let restarted: Session=serde_json::from_slice(&saved.borrow()).unwrap();
        assert!(require_active(&restarted).is_err());
        assert_eq!(restarted.refresh_token,"old-refresh");
        let revoked=Cell::new(false);
        let removed=Cell::new(false);
        assert!(logout_transition(restarted, |_| panic!("already durable"),
            |pending| { assert_eq!(pending.refresh_token,"old-refresh"); revoked.set(true); Ok(()) },
            || { assert!(revoked.get()); removed.set(true); Ok(()) }).unwrap());
        assert!(removed.get());
    }

    #[test]
    fn pending_session_invalidates_cached_token_and_never_refreshes() {
        let active=session();
        let cache=(serde_json::to_vec(&active).unwrap(), AccountToken {
            sub: active.sub.clone(), access_token:"cached-access".into(), valid_for:Duration::from_secs(300),
        }, Instant::now());
        assert!(cached_for_session(Some(&cache), &active).unwrap().is_some());
        let saved=RefCell::new(active.clone());
        logout_transition(active, |pending| { *saved.borrow_mut()=pending.clone(); Ok(()) },
            |_| bail!("offline"), || panic!("no deletion")).unwrap();
        let other_process: Session=serde_json::from_slice(&serde_json::to_vec(&*saved.borrow()).unwrap()).unwrap();
        assert!(cached_for_session(Some(&cache), &other_process).is_err());
        assert!(complete_refresh(other_process, token(), Instant::now(),
            |_| panic!("pending token must not rotate"), |_| panic!("pending token must not identify")).is_err());
    }

    #[test]
    fn failed_local_logout_write_never_revokes_or_reports_completion() {
        let saved=session();
        let result=logout_transition(saved.clone(), |_| bail!("Keychain write denied"),
            |_| panic!("must persist before network"), || panic!("must keep item"));
        assert!(result.is_err());
        assert!(require_active(&saved).is_ok());
    }

    #[test]
    fn failed_clear_after_revocation_keeps_pending_until_idempotent_retry() {
        let saved=RefCell::new(Some(session()));
        let revocations=Cell::new(0);
        let result=logout_transition(session(),
            |pending| { *saved.borrow_mut()=Some(pending.clone()); Ok(()) },
            |_| { revocations.set(revocations.get()+1); Ok(()) },
            || bail!("Keychain deletion failed"));
        assert!(result.is_err());
        let pending=saved.borrow().as_ref().unwrap().clone();
        assert!(require_active(&pending).is_err());
        assert!(cached_for_session(None, &pending).is_err());
        assert!(logout_transition(pending, |_| panic!("pending state already durable"),
            |_| { revocations.set(revocations.get()+1); Ok(()) },
            || { *saved.borrow_mut()=None; Ok(()) }).unwrap());
        assert_eq!(revocations.get(),2,"revocation is retried idempotently before clearing");
        assert!(saved.borrow().is_none());
    }

    #[test]
    fn pending_login_must_finish_revocation_before_clearing_or_authorizing() {
        let mut pending=session(); pending.state=SessionState::PendingRevocation;
        assert!(replace_previous_login(Some(&pending), |_| bail!("still offline"),
            || panic!("must not clear pending credential")).is_err());
        let revoked=Cell::new(false);
        replace_previous_login(Some(&pending), |_| { revoked.set(true); Ok(()) },
            || { assert!(revoked.get()); Ok(()) }).unwrap();
    }

    #[test]
    fn previous_session_is_revoked_then_removed_before_new_authorization() {
        let events = RefCell::new(Vec::new());
        replace_previous_login(Some(&session()),
            |_| { events.borrow_mut().push("revoke"); Ok(()) },
            || { events.borrow_mut().push("clear"); Ok(()) }).unwrap();
        assert_eq!(*events.borrow(), ["revoke", "clear"]);
        assert!(replace_previous_login(Some(&session()),
            |_| bail!("revocation unavailable"), || panic!("must keep prior login")).is_err());
        replace_previous_login(None, |_| panic!("no prior session"), || panic!("no item")).unwrap();
    }

    #[test]
    fn only_missing_keychain_item_allows_new_login() {
        assert!(decode_stored(Err(security_framework::base::Error::from_code(-25300))).unwrap().is_none());
        for code in [-25308, -25293, -128] { // interaction forbidden, authentication failed, cancelled
            assert!(decode_stored(Err(security_framework::base::Error::from_code(code))).is_err());
        }
        assert!(decode_stored(Ok(b"corrupt".to_vec())).is_err());
        let mut wrong = session();
        wrong.client_id = "other-client".into();
        assert!(decode_stored(Ok(serde_json::to_vec(&wrong).unwrap())).is_err());
        assert_eq!(decode_stored(Ok(serde_json::to_vec(&session()).unwrap())).unwrap().unwrap().sub, "owner");
    }

    #[test]
    fn only_explicit_invalid_grant_requires_reauthentication() {
        for status in [400, 401] {
            assert!(check_refresh_status(status, r#"{"error":"invalid_grant"}"#).unwrap_err().is::<SessionExpired>());
            for body in [r#"{"error":"invalid_client"}"#, r#"{"error":"invalid_request"}"#, "proxy error", "{}"] {
                assert!(!check_refresh_status(status, body).unwrap_err().is::<SessionExpired>());
            }
        }
        assert!(!check_refresh_status(503, r#"{"error":"invalid_grant"}"#).unwrap_err().is::<SessionExpired>());
        assert!(check_refresh_status(200, "{}").is_ok());
    }

    #[test]
    fn rotated_refresh_survives_userinfo_failure_and_next_attempt() {
        let saved = RefCell::new(session());
        let result = complete_refresh(session(), token(), Instant::now(),
            |updated| { *saved.borrow_mut() = updated.clone(); Ok(()) },
            |_| { assert_eq!(saved.borrow().refresh_token, "new-refresh"); bail!("userinfo unavailable") });
        assert!(result.is_err());
        let recovered = saved.borrow().clone();
        assert_eq!(recovered.refresh_token, "new-refresh");
        let result = complete_refresh(recovered, token(), Instant::now(), |_| Ok(()), |_| Ok(owner())).unwrap();
        assert_eq!(result.0.sub, "owner");
    }

    #[test]
    fn persistence_failure_stops_before_userinfo_and_subject_change_stays_closed() {
        let called = Cell::new(false);
        assert!(complete_refresh(session(), token(), Instant::now(),
            |_| bail!("Keychain unavailable"), |_| { called.set(true); Ok(owner()) }).is_err());
        assert!(!called.get());
        assert!(complete_refresh(session(), token(), Instant::now(), |_| Ok(()),
            |_| Ok(Userinfo { sub: "other".into(), preferred_username: None })).is_err());
    }

    #[test]
    fn access_cache_uses_expiry_margin_and_elapsed_time() {
        let (access, _) = complete_refresh(session(), token(), Instant::now() - Duration::from_secs(10), |_| Ok(()), |_| Ok(owner())).unwrap();
        assert!(access.valid_for > Duration::from_secs(250));
        assert!(access.valid_for <= Duration::from_secs(260));
        let mut short = token();
        short.expires_in = Some(20);
        assert_eq!(complete_refresh(session(), short, Instant::now(), |_| Ok(()), |_| Ok(owner())).unwrap().0.valid_for, Duration::ZERO);
        let mut unrotated = token();
        unrotated.refresh_token = None;
        assert!(complete_refresh(session(), unrotated, Instant::now(), |_| panic!("no replacement to save"), |_| Ok(owner())).is_ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fragmented {
        chunks: Vec<&'static [u8]>,
    }

    impl Read for Fragmented {
        fn read(&mut self, target: &mut [u8]) -> std::io::Result<usize> {
            if self.chunks.is_empty() {
                return Ok(0);
            }
            let part = self.chunks.remove(0);
            target[..part.len()].copy_from_slice(part);
            Ok(part.len())
        }
    }

    #[test]
    fn callback_http_can_arrive_in_multiple_reads() {
        let mut request = Fragmented {
            chunks: vec![
                b"GET /oauth2redirect/alondra-cli?state=ok&code=abc HTTP/1.1\r\nHo",
                b"st: 127.0.0.1:1234\r\n\r\n",
            ],
        };
        let raw = read_request(&mut request).unwrap();
        assert_eq!(
            callback_target(&raw, "ok", 1234).unwrap(),
            Some("abc".into())
        );
    }

    #[test]
    fn callback_rejects_missing_or_wrong_state_and_duplicate_codes() {
        let ok = "GET /oauth2redirect/alondra-cli?state=expected&code=a%2Fb HTTP/1.1\r\nHost: 127.0.0.1:1234\r\n\r\n";
        assert_eq!(
            callback_target(ok, "expected", 1234).unwrap(),
            Some("a/b".into())
        );
        assert!(callback_target(ok, "other", 1234).is_err());
        assert!(callback_target(ok, "expected", 1235).is_err());
        assert!(callback_target(&ok.replace("&code=a%2Fb", ""), "expected", 1234).is_err());
        assert!(
            callback_target(
                &ok.replace("&code=a%2Fb", "&code=a&code=b"),
                "expected",
                1234
            )
            .is_err()
        );
    }

    #[test]
    fn discovered_endpoints_stay_below_https_issuer() {
        assert!(
            endpoint(
                ISSUER,
                "https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/token"
            )
            .is_ok()
        );
        assert!(
            endpoint(
                ISSUER,
                "https://elsewhere.example/realms/alondra/protocol/openid-connect/token"
            )
            .is_err()
        );
        assert!(
            endpoint(
                ISSUER,
                "http://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/token"
            )
            .is_err()
        );
    }
}

/// Refresh tokens rotate. Serialize Keychain read/refresh/write across every CLI process.
struct AccountLock(std::fs::File);
impl AccountLock {
    fn acquire() -> Result<Self> {
        let path = Self::path()?;
        std::fs::create_dir_all(path.parent().context("account lock directory")?)?;
        Self::at(&path)
    }
    fn path() -> Result<std::path::PathBuf> {
        // Keychain credentials have user-wide scope. Neither HOME nor ALONDRA_HOME
        // may split the lock for the same global Keychain item.
        let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0u8; 16384];
        let code = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                record.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if code != 0 || result.is_null() {
            bail!("cannot resolve OS account home for login lock");
        }
        let record = unsafe { record.assume_init() };
        if record.pw_dir.is_null() {
            bail!("OS account has no home directory");
        }
        use std::os::unix::ffi::OsStrExt;
        let home = std::path::Path::new(std::ffi::OsStr::from_bytes(
            unsafe { std::ffi::CStr::from_ptr(record.pw_dir) }.to_bytes(),
        ));
        Ok(home.join("Library/Caches/dev.davidcollado.alondra/service-login.lock"))
    }
    fn for_service() -> Result<Self> {
        let path = Self::path()?;
        std::fs::create_dir_all(path.parent().context("account lock directory")?)?;
        Self::open(&path, true)
    }
    fn at(path: &std::path::Path) -> Result<Self> {
        Self::open(path, false)
    }
    fn open(path: &std::path::Path, nonblocking: bool) -> Result<Self> {
        use std::os::{
            fd::AsRawFd,
            unix::fs::{MetadataExt, OpenOptionsExt},
        };
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.uid() != unsafe { libc::getuid() } || meta.mode() & 0o077 != 0 {
            bail!("unsafe global account lock file");
        }
        let flags = libc::LOCK_EX | if nonblocking { libc::LOCK_NB } else { 0 };
        if unsafe { libc::flock(file.as_raw_fd(), flags) } != 0 {
            let error = std::io::Error::last_os_error();
            if nonblocking && error.kind() == std::io::ErrorKind::WouldBlock {
                return Err(AccountBusy.into());
            }
            return Err(error.into());
        }
        Ok(Self(file))
    }
}
impl Drop for AccountLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

type CachedAccount = (Vec<u8>, AccountToken, Instant);
static SERVICE_TOKEN_CACHE: std::sync::Mutex<Option<CachedAccount>> = std::sync::Mutex::new(None);

fn cached_for_session(cache: Option<&CachedAccount>, session: &Session) -> Result<Option<AccountToken>> {
    require_active(session)?;
    let bytes = serde_json::to_vec(session)?;
    Ok(cache.filter(|(generation, token, created)| generation == &bytes && created.elapsed() < token.valid_for)
        .map(|(_, token, _)| token.clone()))
}

/// Contention is temporary and does not indicate expired credentials.
#[derive(Debug)]
pub(crate) struct AccountBusy;
impl std::fmt::Display for AccountBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("account login or refresh is busy; retry when it finishes")
    }
}
impl std::error::Error for AccountBusy {}

/// A short-lived access-token cache avoids a refresh for every encrypted file chunk.
/// Inspect persisted session under the process lock, so logout/account changes invalidate it.
pub fn token_for_service() -> Result<AccountToken> {
    let mut cache = SERVICE_TOKEN_CACHE
        .try_lock()
        .map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => anyhow::Error::new(AccountBusy),
            std::sync::TryLockError::Poisoned(_) => anyhow!("account token cache lock poisoned"),
        })?;
    let _lock = AccountLock::for_service()?;
    let session = stored().context("sign in with alondra login before connecting to a remote Mac")?;
    if let Some(token) = cached_for_session(cache.as_ref(), &session)? {
        return Ok(token);
    }
    let (token, _) = refreshed_locked()?;
    *cache = Some((
        serde_json::to_vec(&stored()?)?,
        token.clone(),
        Instant::now(),
    ));
    Ok(token)
}

#[cfg(test)]
mod account_lock_tests {
    use super::*;
    #[test]
    fn lock_probe_child() {
        use std::os::fd::AsRawFd;
        let Ok(path) = std::env::var("ALONDRA_TEST_ACCOUNT_LOCK") else {
            return;
        };
        println!("lock-path={}", AccountLock::path().unwrap().display());
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let acquired = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        assert_eq!(
            acquired,
            std::env::var("ALONDRA_TEST_EXPECT_LOCK").unwrap() == "free"
        );
    }
    #[test]
    fn account_lock_is_shared_across_processes_and_profile_homes() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(format!(
            "alondra-account-lock-test-{}-{}",
            std::process::id(),
            crate::service_client::now_ms().unwrap()
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("lock");
        let lock = AccountLock::at(&path).unwrap();
        let began = Instant::now();
        let error = match AccountLock::open(&path, true) {
            Ok(_) => panic!("service must not take a held refresh lock"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("busy"));
        assert!(error.is::<AccountBusy>());
        assert!(
            began.elapsed() < Duration::from_secs(1),
            "service login never waits behind browser login"
        );
        let probe = |expected: &str, profile: &str| {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "account::account_lock_tests::lock_probe_child",
                    "--nocapture",
                ])
                .env("ALONDRA_TEST_ACCOUNT_LOCK", &path)
                .env("ALONDRA_TEST_EXPECT_LOCK", expected)
                .env("ALONDRA_HOME", profile)
                .env("HOME", profile)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains(&format!(
                "lock-path={}",
                AccountLock::path().unwrap().display()
            )));
        };
        probe("blocked", "/tmp/other-alondra-profile-a");
        probe("blocked", "/tmp/other-alondra-profile-b");
        drop(lock);
        probe("free", "/tmp/other-alondra-profile-b");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod setup_auth_mode_tests {
    use super::*;

    fn fixture_token() -> AccountToken {
        AccountToken { sub: "synthetic-account".into(), access_token: "synthetic-token".into(), valid_for: Duration::from_secs(60) }
    }

    #[test]
    fn no_input_auth_failures_never_open_or_replace_login() {
        for error in [anyhow!("no stored session"), SessionExpired.into(), anyhow!("refresh transport unavailable")] {
            let expected = error.to_string();
            let result = setup_token_with(true, || Err(error), || panic!("interactive login must not run"));
            assert_eq!(result.err().unwrap().to_string(), expected);
        }
    }

    #[test]
    fn no_input_refreshes_existing_session_and_interactive_setup_retains_login() {
        let token = setup_token_with(true, || Ok(fixture_token()), || panic!("valid session must not open login")).unwrap();
        assert_eq!(token.sub, "synthetic-account");
        assert_eq!(token.access_token, "synthetic-token");
        assert_eq!(token.valid_for, Duration::from_secs(60));
        let token = setup_token_with(false, || panic!("interactive mode delegates existing login policy"), || Ok(fixture_token())).unwrap();
        assert_eq!(token.sub, "synthetic-account");
    }
}
