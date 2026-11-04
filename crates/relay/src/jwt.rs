//! Access tokens of the identity provider: RS256 signatures checked against its published key
//! set, plus issuer, audience and lifetime. Whether the account behind a valid token may still use
//! the relay is decided elsewhere.

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    sync::{Mutex, RwLock},
    time::{Duration, Instant},
};
use tokio::sync::Mutex as AsyncMutex;

const MAX_JWKS_BYTES: usize = 64 * 1024;
const JWKS_TIMEOUT: Duration = Duration::from_secs(5);
const JWKS_MAX_AGE: Duration = Duration::from_secs(60 * 60);
const UNKNOWN_KID_RETRY: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}
#[derive(Deserialize)]
struct Jwk {
    kid: String,
    kty: String,
    #[serde(rename = "use")]
    usage: Option<String>,
    alg: Option<String>,
    n: String,
    e: String,
}
#[derive(Deserialize)]
struct JwtHeader {
    alg: String,
    kid: String,
}
#[derive(Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    aud: serde_json::Value,
    exp: u64,
    nbf: Option<u64>,
}
type RsaJwks = HashMap<String, (Vec<u8>, Vec<u8>)>;
struct CachedJwks {
    keys: RsaJwks,
    refreshed_at: Instant,
}

pub struct Verifier {
    issuer: String,
    audience: String,
    jwks_url: String,
    jwks: RwLock<CachedJwks>,
    jwks_refresh: AsyncMutex<()>,
    last_unknown_refresh: Mutex<Option<Instant>>,
}

impl Verifier {
    /// `allow_internal_http` admits the Docker-only Keycloak service URL of an isolated network.
    pub async fn open(
        issuer: String,
        audience: String,
        jwks_url: String,
        allow_internal_http: bool,
    ) -> Result<Self> {
        let jwks_url_allowed = jwks_url.starts_with("https://")
            || jwks_url.starts_with("http://127.0.0.1:")
            || (allow_internal_http && jwks_url.starts_with("http://keycloak:8080/"));
        if !jwks_url_allowed {
            bail!(
                "JWKS URL must use HTTPS, loopback, or explicitly allowed pilot Keycloak network"
            );
        }
        let jwks = fetch_jwks(&jwks_url).await?;
        Self::from_jwks(issuer, audience, jwks_url, jwks)
    }

    /// A verifier over a key set already in hand, as a test or an offline check supplies it.
    pub fn from_jwks_json(
        issuer: String,
        audience: String,
        jwks_url: String,
        jwks_json: &[u8],
    ) -> Result<Self> {
        Self::from_jwks(issuer, audience, jwks_url, serde_json::from_slice(jwks_json)?)
    }

    fn from_jwks(issuer: String, audience: String, jwks_url: String, jwks: Jwks) -> Result<Self> {
        if issuer.is_empty() || audience.is_empty() {
            bail!("issuer and audience are required");
        }
        Ok(Self {
            issuer,
            audience,
            jwks_url,
            jwks: RwLock::new(CachedJwks {
                keys: parse_jwks(jwks)?,
                refreshed_at: Instant::now(),
            }),
            jwks_refresh: AsyncMutex::new(()),
            last_unknown_refresh: Mutex::new(None),
        })
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// The key set was fetched within its maximum age.
    pub fn ready(&self) -> bool {
        self.jwks.read().unwrap().refreshed_at.elapsed() <= JWKS_MAX_AGE
    }

    /// Replace the complete key set; removed Keycloak keys stop validating immediately.
    pub async fn refresh(&self) -> Result<()> {
        let _guard = self.jwks_refresh.lock().await;
        let keys = parse_jwks(fetch_jwks(&self.jwks_url).await?)?;
        *self.jwks.write().unwrap() = CachedJwks {
            keys,
            refreshed_at: Instant::now(),
        };
        Ok(())
    }

    async fn refresh_for_unknown_kid(&self, kid: &str) -> Result<()> {
        let _guard = self.jwks_refresh.lock().await;
        if self.jwks.read().unwrap().keys.contains_key(kid) {
            return Ok(());
        }
        let now = Instant::now();
        {
            let mut last = self.last_unknown_refresh.lock().unwrap();
            if last.is_some_and(|t| now.duration_since(t) < UNKNOWN_KID_RETRY) {
                bail!("unknown JWT key ID");
            }
            *last = Some(now);
        }
        let keys = parse_jwks(fetch_jwks(&self.jwks_url).await?)?;
        *self.jwks.write().unwrap() = CachedJwks {
            keys,
            refreshed_at: Instant::now(),
        };
        Ok(())
    }

    /// The subject of a valid token.
    pub async fn subject(&self, jwt: &str, now_ms: u64) -> Result<String> {
        if jwt.len() > 8192 {
            bail!("JWT too large");
        }
        let mut parts = jwt.split('.');
        let h = parts.next().context("JWT header")?;
        let p = parts.next().context("JWT payload")?;
        let s = parts.next().context("JWT signature")?;
        if parts.next().is_some() {
            bail!("invalid JWT compact form");
        }
        let header: JwtHeader = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(h)?)?;
        if header.alg != "RS256" {
            bail!("unsupported JWT algorithm");
        }
        if header.kid.is_empty() || header.kid.len() > 128 {
            bail!("invalid JWT key ID");
        }
        if !self.jwks.read().unwrap().keys.contains_key(&header.kid) {
            self.refresh_for_unknown_kid(&header.kid).await?;
        }
        let signature = URL_SAFE_NO_PAD.decode(s)?;
        {
            let jwks = self.jwks.read().unwrap();
            if jwks.refreshed_at.elapsed() > JWKS_MAX_AGE {
                bail!("JWKS cache expired");
            }
            let (n, e) = jwks.keys.get(&header.kid).context("unknown JWT key ID")?;
            RsaPublicKeyComponents { n, e }
                .verify(
                    &RSA_PKCS1_2048_8192_SHA256,
                    format!("{h}.{p}").as_bytes(),
                    &signature,
                )
                .map_err(|_| anyhow::anyhow!("invalid JWT signature"))?;
        }
        let c: Claims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p)?)?;
        let now_s = now_ms / 1000;
        if c.iss != self.issuer
            || c.sub.is_empty()
            || c.exp <= now_s
            || c.nbf.is_some_and(|nbf| nbf > now_s + 120)
        {
            bail!("invalid JWT claims");
        }
        let audience_ok = c.aud.as_str() == Some(&self.audience)
            || c.aud
                .as_array()
                .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(&self.audience)));
        if !audience_ok {
            bail!("JWT audience mismatch");
        }
        Ok(c.sub)
    }
}

fn parse_jwks(jwks: Jwks) -> Result<RsaJwks> {
    let mut result = HashMap::new();
    let mut ids = HashSet::new();
    for j in jwks.keys {
        if j.kty != "RSA"
            || j.usage.as_deref().is_some_and(|v| v != "sig")
            || j.alg.as_deref().is_some_and(|v| v != "RS256")
        {
            continue;
        }
        if !ids.insert(j.kid.clone()) {
            bail!("duplicate JWK key ID");
        }
        result.insert(
            j.kid,
            (URL_SAFE_NO_PAD.decode(j.n)?, URL_SAFE_NO_PAD.decode(j.e)?),
        );
    }
    if result.is_empty() {
        bail!("JWKS has no RS256 signing keys");
    }
    Ok(result)
}

async fn fetch_jwks(url: &str) -> Result<Jwks> {
    let client = reqwest::Client::builder()
        .timeout(JWKS_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut response = client.get(url).send().await?.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|n| n > MAX_JWKS_BYTES as u64)
    {
        bail!("JWKS response too large");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > MAX_JWKS_BYTES {
            bail!("JWKS response too large");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&body)?)
}
