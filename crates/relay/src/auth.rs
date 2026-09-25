use alondra_e2e::HttpChallengeVerifier;
use anyhow::{Context, Result, bail};
use axum::http::{HeaderMap, Method, Uri};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::ecdsa::VerifyingKey;
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Mutex, RwLock},
    time::{Duration, Instant},
};
use tokio::sync::Mutex as AsyncMutex;

const CLOCK_SKEW_MS: u64 = 120_000;
const MAX_NONCES: usize = 100_000;
const MAX_JWKS_BYTES: usize = 64 * 1024;
const JWKS_TIMEOUT: Duration = Duration::from_secs(5);
const JWKS_MAX_AGE: Duration = Duration::from_secs(60 * 60);
const UNKNOWN_KID_RETRY: Duration = Duration::from_secs(30);

#[derive(Clone, Deserialize)]
pub struct Config {
    pub listen: String,
    pub issuer: String,
    pub audience: String,
    pub jwks_url: String,
    /// Permit the Docker-only Keycloak service URL on the isolated pilot network.
    #[serde(default)]
    pub allow_internal_http_jwks: bool,
    pub owner_sub: String,
    pub mac_id: String,
    /// SEC1 uncompressed P-256 public key, base64url without padding.
    pub mac_signing_key: String,
    pub device_registry_path: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PhoneDevice {
    pub device_id: String,
    pub owner_sub: String,
    pub signing_key: String,
}

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

#[derive(Clone, Debug)]
pub struct Identity {
    pub device_id: String,
    pub owner_sub: String,
}

pub struct Auth {
    pub config: Config,
    jwks: RwLock<CachedJwks>,
    jwks_refresh: AsyncMutex<()>,
    last_unknown_refresh: Mutex<Option<Instant>>,
    devices: Mutex<HashMap<String, PhoneDevice>>,
    verifier: Mutex<HttpChallengeVerifier>,
}

impl Auth {
    pub async fn open(config: Config) -> Result<Self> {
        if config.issuer.is_empty()
            || config.audience.is_empty()
            || config.owner_sub.is_empty()
            || config.mac_id.is_empty()
        {
            bail!("issuer, audience, owner_sub and mac_id are required");
        }
        if !config.jwks_url.starts_with("https://")
            && !config.jwks_url.starts_with("http://127.0.0.1:")
            && !(config.allow_internal_http_jwks
                && config.jwks_url.starts_with("http://keycloak:8080/"))
        {
            bail!(
                "JWKS URL must use HTTPS, loopback, or explicitly allowed pilot Keycloak network"
            );
        }
        verifying_key(&config.mac_signing_key).context("invalid bootstrap Mac signing key")?;
        let jwks = fetch_jwks(&config.jwks_url).await?;
        Self::from_jwks(config, jwks)
    }

    fn from_jwks(config: Config, jwks: Jwks) -> Result<Self> {
        let jwks = CachedJwks {
            keys: parse_jwks(jwks)?,
            refreshed_at: Instant::now(),
        };
        let devices = if config.device_registry_path.exists() {
            let raw = std::fs::read(&config.device_registry_path)?;
            let list: Vec<PhoneDevice> = serde_json::from_slice(&raw)?;
            list.into_iter().map(|d| (d.device_id.clone(), d)).collect()
        } else {
            HashMap::new()
        };
        let verifier = HttpChallengeVerifier::new(CLOCK_SKEW_MS, MAX_NONCES)
            .map_err(|_| anyhow::anyhow!("could not initialize device challenge verifier"))?;
        Ok(Self {
            config,
            jwks: RwLock::new(jwks),
            jwks_refresh: AsyncMutex::new(()),
            last_unknown_refresh: Mutex::new(None),
            devices: Mutex::new(devices),
            verifier: Mutex::new(verifier),
        })
    }

    #[cfg(test)]
    pub(crate) fn test(config: Config, jwks_json: &[u8]) -> Result<Self> {
        Self::from_jwks(config, serde_json::from_slice(jwks_json)?)
    }

    pub fn enroll(&self, device: PhoneDevice) -> Result<()> {
        if device.owner_sub != self.config.owner_sub
            || device.device_id.is_empty()
            || device.device_id.len() > 128
            || device.device_id == self.config.mac_id
        {
            bail!("invalid phone device registration");
        }
        verifying_key(&device.signing_key)?;
        let mut devices = self.devices.lock().unwrap();
        if let Some(existing) = devices.get(&device.device_id) {
            if existing.owner_sub != device.owner_sub || existing.signing_key != device.signing_key
            {
                bail!("device key replacement requires a new device ID and fresh pairing");
            }
            return Ok(());
        }
        let new_id = device.device_id.clone();
        devices.insert(new_id.clone(), device);
        let mut sorted: Vec<_> = devices.values().cloned().collect();
        sorted.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        if let Err(e) = write_atomic(
            &self.config.device_registry_path,
            &serde_json::to_vec(&sorted)?,
        ) {
            // Enrollment must not become usable unless registry durability succeeds.
            devices.remove(&new_id);
            return Err(e);
        }
        Ok(())
    }

    /// A Mac-signed revocation removes the public key durably before queued data is purged.
    pub fn revoke(&self, device_id: &str) -> Result<()> {
        let mut devices = self.devices.lock().unwrap();
        let Some(previous) = devices.remove(device_id) else {
            return Ok(());
        };
        let mut sorted: Vec<_> = devices.values().cloned().collect();
        sorted.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        if let Err(e) = write_atomic(
            &self.config.device_registry_path,
            &serde_json::to_vec(&sorted)?,
        ) {
            devices.insert(device_id.to_owned(), previous);
            return Err(e);
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn is_phone_registered(&self, device_id: &str) -> bool {
        self.devices.lock().unwrap().contains_key(device_id)
    }

    pub fn authenticate_mac(
        &self,
        headers: &HeaderMap,
        method: &Method,
        uri: &Uri,
        body: &[u8],
        now_ms: u64,
    ) -> Result<Identity> {
        self.verify_device(
            headers,
            method,
            uri,
            body,
            now_ms,
            &self.config.mac_id,
            &self.config.mac_signing_key,
        )?;
        Ok(Identity {
            device_id: self.config.mac_id.clone(),
            owner_sub: self.config.owner_sub.clone(),
        })
    }

    pub async fn authenticate_phone(
        &self,
        headers: &HeaderMap,
        method: &Method,
        uri: &Uri,
        body: &[u8],
        now_ms: u64,
    ) -> Result<Identity> {
        let token = headers
            .get(axum::http::header::AUTHORIZATION)
            .context("missing Authorization")?
            .to_str()?;
        let token = token
            .strip_prefix("Bearer ")
            .context("expected Bearer token")?;
        let sub = self.verify_jwt(token, now_ms).await?;
        let id = header(headers, "x-device-id")?;
        let device = self
            .devices
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .context("unknown device")?;
        if device.owner_sub != sub {
            bail!("device does not belong to token subject");
        }
        self.verify_device(headers, method, uri, body, now_ms, id, &device.signing_key)?;
        Ok(Identity {
            device_id: id.into(),
            owner_sub: sub,
        })
    }

    pub fn phone_key(&self, device_id: &str) -> Result<VerifyingKey> {
        let device = self
            .devices
            .lock()
            .unwrap()
            .get(device_id)
            .cloned()
            .context("unknown device")?;
        verifying_key(&device.signing_key)
    }

    pub fn mac_key(&self) -> Result<VerifyingKey> {
        verifying_key(&self.config.mac_signing_key)
    }

    pub fn server_epoch(&self) -> [u8; 16] {
        self.verifier.lock().unwrap().server_epoch()
    }

    pub fn jwks_ready(&self) -> bool {
        self.jwks.read().unwrap().refreshed_at.elapsed() <= JWKS_MAX_AGE
    }

    /// Replace the complete key set; removed Keycloak keys stop validating immediately.
    pub async fn refresh_jwks(&self) -> Result<()> {
        let _guard = self.jwks_refresh.lock().await;
        let keys = parse_jwks(fetch_jwks(&self.config.jwks_url).await?)?;
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
        let keys = parse_jwks(fetch_jwks(&self.config.jwks_url).await?)?;
        *self.jwks.write().unwrap() = CachedJwks {
            keys,
            refreshed_at: Instant::now(),
        };
        Ok(())
    }

    pub(crate) async fn verify_jwt(&self, jwt: &str, now_ms: u64) -> Result<String> {
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
        if c.iss != self.config.issuer
            || c.sub.is_empty()
            || c.exp <= now_s
            || c.nbf.is_some_and(|nbf| nbf > now_s + 120)
        {
            bail!("invalid JWT claims");
        }
        let audience_ok = c.aud.as_str() == Some(&self.config.audience)
            || c.aud
                .as_array()
                .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(&self.config.audience)));
        if !audience_ok {
            bail!("JWT audience mismatch");
        }
        Ok(c.sub)
    }

    #[allow(clippy::too_many_arguments)] // The signed HTTP request and bound identity are intentionally explicit.
    fn verify_device(
        &self,
        headers: &HeaderMap,
        method: &Method,
        uri: &Uri,
        body: &[u8],
        now_ms: u64,
        id: &str,
        public_key: &str,
    ) -> Result<()> {
        if header(headers, "x-device-id")? != id {
            bail!("device ID mismatch");
        }
        let timestamp_str = header(headers, "x-device-timestamp-ms")?;
        let timestamp: u64 = timestamp_str.parse()?;
        if timestamp.to_string() != timestamp_str || now_ms.abs_diff(timestamp) > CLOCK_SKEW_MS {
            bail!("stale device signature");
        }
        let nonce_vec = URL_SAFE_NO_PAD.decode(header(headers, "x-device-nonce")?)?;
        let nonce: [u8; 16] = nonce_vec
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid nonce"))?;
        let signature = header(headers, "x-device-signature")?;
        if header(headers, "x-relay-epoch")? != URL_SAFE_NO_PAD.encode(self.server_epoch()) {
            bail!("relay epoch mismatch");
        }
        let path = uri.path_and_query().map_or(uri.path(), |p| p.as_str());
        self.verifier
            .lock()
            .unwrap()
            .verify(
                &verifying_key(public_key)?,
                signature,
                id,
                method.as_str(),
                path,
                body,
                timestamp,
                &nonce,
                now_ms,
            )
            .map_err(|_| anyhow::anyhow!("invalid or replayed device challenge"))?;
        Ok(())
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

fn verifying_key(encoded: &str) -> Result<VerifyingKey> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded)?;
    Ok(VerifyingKey::from_sec1_bytes(&bytes)?)
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str> {
    Ok(headers
        .get(name)
        .context("missing device auth header")?
        .to_str()?)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let parent = path
        .parent()
        .context("device registry needs a parent directory")?;
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension("tmp");
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(tmp, path)?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}
