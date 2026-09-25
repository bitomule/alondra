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
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

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
}

#[derive(Deserialize)]
struct Userinfo {
    sub: String,
    preferred_username: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Session {
    issuer: String,
    client_id: String,
    sub: String,
    refresh_token: String,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
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
                let mut bytes = [0u8; 8192];
                let count = stream.read(&mut bytes)?;
                let raw = std::str::from_utf8(&bytes[..count]).context("invalid callback HTTP")?;
                let result = callback_target(raw, state, port);
                let success = matches!(result, Ok(Some(_)));
                respond(&mut stream, success);
                if let Some(code) = result? {
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

fn stored() -> Result<Session> {
    let bytes = get_generic_password(KEYCHAIN_SERVICE, ISSUER)
        .context("no Alondra service login in Keychain; run `alondra login`")?;
    let session: Session = serde_json::from_slice(&bytes)?;
    if session.issuer != ISSUER || session.client_id != CLIENT_ID || session.sub.is_empty() {
        bail!("Keychain login belongs to another issuer or client");
    }
    Ok(session)
}

pub fn login() -> Result<()> {
    let http = agent();
    let d = discovery(&http)?;
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
        .append_pair("scope", "openid profile")
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    println!("Opening Alondra login in your browser…");
    let status = Command::new("open")
        .arg(url.as_str())
        .status()
        .context("opening browser")?;
    if !status.success() {
        bail!("could not open browser; login was not started");
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
    };
    set_generic_password(KEYCHAIN_SERVICE, ISSUER, &serde_json::to_vec(&session)?)?;
    println!(
        "Signed in as {}.",
        user.preferred_username.as_deref().unwrap_or(&session.sub)
    );
    Ok(())
}

pub fn status() -> Result<()> {
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
    if status != 200 {
        bail!("session expired or revoked; run `alondra login` again");
    }
    let token: TokenResponse = serde_json::from_str(&body)?;
    let user = userinfo(&http, &d, &token.access_token)?;
    if user.sub != session.sub {
        bail!("account subject changed; run `alondra logout` and sign in again");
    }
    if let Some(refresh) = token.refresh_token {
        let updated = Session {
            refresh_token: refresh,
            ..session
        };
        set_generic_password(KEYCHAIN_SERVICE, ISSUER, &serde_json::to_vec(&updated)?)?;
    }
    println!(
        "Signed in as {}.",
        user.preferred_username.as_deref().unwrap_or(&user.sub)
    );
    Ok(())
}

pub fn logout() -> Result<()> {
    let session = stored()?;
    let http = agent();
    let mut revoked = false;
    if let Ok(d) = discovery(&http)
        && let Some(url) = d.revocation_endpoint
    {
        revoked = post_form(
            &http,
            &url,
            &[
                ("client_id", CLIENT_ID),
                ("token", &session.refresh_token),
                ("token_type_hint", "refresh_token"),
            ],
        ).is_ok_and(|(status, _)| status == 200);
    }
    delete_generic_password(KEYCHAIN_SERVICE, ISSUER)?;
    println!("Signed out on this Mac.");
    if !revoked {
        eprintln!("Could not confirm server-side token revocation; existing sessions expire under Keycloak policy.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
