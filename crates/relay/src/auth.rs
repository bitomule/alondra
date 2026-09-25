use anyhow::{Context, Result, bail};
use axum::http::{HeaderMap, Method, Uri};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::{HashMap, HashSet}, path::{Path, PathBuf}, sync::Mutex};

const CLOCK_SKEW_MS: u64 = 120_000;
const MAX_NONCES: usize = 100_000;

#[derive(Clone, Deserialize)]
pub struct Config {
    pub listen: String,
    pub issuer: String,
    pub audience: String,
    pub jwks_url: String,
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
struct Jwks { keys: Vec<Jwk> }
#[derive(Deserialize)]
struct Jwk { kid: String, kty: String, #[serde(rename = "use")] usage: Option<String>, alg: Option<String>, n: String, e: String }
#[derive(Deserialize)]
struct JwtHeader { alg: String, kid: String }
#[derive(Deserialize)]
struct Claims { iss: String, sub: String, aud: serde_json::Value, exp: u64, nbf: Option<u64> }

#[derive(Clone, Debug)]
pub struct Identity { pub device_id: String, pub owner_sub: String }

pub struct Auth {
    pub config: Config,
    jwks: HashMap<String, (Vec<u8>, Vec<u8>)>,
    devices: Mutex<HashMap<String, PhoneDevice>>,
    nonces: Mutex<HashMap<(String, [u8; 16]), u64>>,
}

impl Auth {
    pub async fn open(config: Config) -> Result<Self> {
        if config.issuer.is_empty() || config.audience.is_empty() || config.owner_sub.is_empty() || config.mac_id.is_empty() {
            bail!("issuer, audience, owner_sub and mac_id are required");
        }
        if !config.jwks_url.starts_with("https://") && !config.jwks_url.starts_with("http://127.0.0.1:") {
            bail!("JWKS URL must use HTTPS (localhost allowed for tests)");
        }
        verifying_key(&config.mac_signing_key).context("invalid bootstrap Mac signing key")?;
        let jwks: Jwks = reqwest::Client::new().get(&config.jwks_url).send().await?.error_for_status()?.json().await?;
        Self::from_jwks(config, jwks)
    }

    fn from_jwks(config: Config, jwks: Jwks) -> Result<Self> {
        let jwks = parse_jwks(jwks)?;
        let devices = if config.device_registry_path.exists() {
            let raw = std::fs::read(&config.device_registry_path)?;
            let list: Vec<PhoneDevice> = serde_json::from_slice(&raw)?;
            list.into_iter().map(|d| (d.device_id.clone(), d)).collect()
        } else { HashMap::new() };
        Ok(Self { config, jwks, devices: Mutex::new(devices), nonces: Mutex::new(HashMap::new()) })
    }

    #[cfg(test)]
    pub(crate) fn test(config: Config, jwks_json: &[u8]) -> Result<Self> {
        Self::from_jwks(config, serde_json::from_slice(jwks_json)?)
    }

    pub fn enroll(&self, device: PhoneDevice) -> Result<()> {
        if device.owner_sub != self.config.owner_sub || device.device_id.is_empty() || device.device_id.len() > 128 || device.device_id == self.config.mac_id {
            bail!("invalid phone device registration");
        }
        verifying_key(&device.signing_key)?;
        let mut devices = self.devices.lock().unwrap();
        if let Some(existing) = devices.get(&device.device_id) {
            if existing.owner_sub != device.owner_sub || existing.signing_key != device.signing_key {
                bail!("device key replacement requires a new device ID and fresh pairing");
            }
            return Ok(());
        }
        let new_id = device.device_id.clone();
        devices.insert(new_id.clone(), device);
        let mut sorted: Vec<_> = devices.values().cloned().collect();
        sorted.sort_by(|a,b| a.device_id.cmp(&b.device_id));
        if let Err(e) = write_atomic(&self.config.device_registry_path, &serde_json::to_vec(&sorted)?) {
            // Enrollment must not become usable unless registry durability succeeds.
            devices.remove(&new_id);
            return Err(e);
        }
        Ok(())
    }

    pub fn authenticate_mac(&self, headers: &HeaderMap, method: &Method, uri: &Uri, body: &[u8], now_ms: u64) -> Result<Identity> {
        self.verify_device(headers, method, uri, body, now_ms, &self.config.mac_id, &self.config.mac_signing_key)?;
        Ok(Identity { device_id: self.config.mac_id.clone(), owner_sub: self.config.owner_sub.clone() })
    }

    pub fn authenticate_phone(&self, headers: &HeaderMap, method: &Method, uri: &Uri, body: &[u8], now_ms: u64) -> Result<Identity> {
        let token = headers.get(axum::http::header::AUTHORIZATION).context("missing Authorization")?.to_str()?;
        let token = token.strip_prefix("Bearer ").context("expected Bearer token")?;
        let sub = self.verify_jwt(token, now_ms)?;
        let id = header(headers, "x-device-id")?;
        let device = self.devices.lock().unwrap().get(id).cloned().context("unknown device")?;
        if device.owner_sub != sub { bail!("device does not belong to token subject"); }
        self.verify_device(headers, method, uri, body, now_ms, id, &device.signing_key)?;
        Ok(Identity { device_id: id.into(), owner_sub: sub })
    }

    fn verify_jwt(&self, jwt: &str, now_ms: u64) -> Result<String> {
        if jwt.len() > 8192 { bail!("JWT too large"); }
        let mut parts = jwt.split('.');
        let h = parts.next().context("JWT header")?;
        let p = parts.next().context("JWT payload")?;
        let s = parts.next().context("JWT signature")?;
        if parts.next().is_some() { bail!("invalid JWT compact form"); }
        let header: JwtHeader = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(h)?)?;
        if header.alg != "RS256" { bail!("unsupported JWT algorithm"); }
        let (n, e) = self.jwks.get(&header.kid).context("unknown JWT key ID")?;
        let signature = URL_SAFE_NO_PAD.decode(s)?;
        RsaPublicKeyComponents { n, e }.verify(&RSA_PKCS1_2048_8192_SHA256, format!("{h}.{p}").as_bytes(), &signature)
            .map_err(|_| anyhow::anyhow!("invalid JWT signature"))?;
        let c: Claims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p)?)?;
        let now_s = now_ms / 1000;
        if c.iss != self.config.issuer || c.sub.is_empty() || c.exp <= now_s || c.nbf.is_some_and(|nbf| nbf > now_s + 120) {
            bail!("invalid JWT claims");
        }
        let audience_ok = c.aud.as_str() == Some(&self.config.audience) || c.aud.as_array().is_some_and(|a| a.iter().any(|x| x.as_str() == Some(&self.config.audience)));
        if !audience_ok { bail!("JWT audience mismatch"); }
        Ok(c.sub)
    }

    fn verify_device(&self, headers: &HeaderMap, method: &Method, uri: &Uri, body: &[u8], now_ms: u64,
                     id: &str, public_key: &str) -> Result<()> {
        if header(headers, "x-device-id")? != id { bail!("device ID mismatch"); }
        let timestamp_str = header(headers, "x-device-timestamp-ms")?;
        let timestamp: u64 = timestamp_str.parse()?;
        if timestamp.to_string() != timestamp_str || now_ms.abs_diff(timestamp) > CLOCK_SKEW_MS { bail!("stale device signature"); }
        let nonce_vec = URL_SAFE_NO_PAD.decode(header(headers, "x-device-nonce")?)?;
        let nonce: [u8; 16] = nonce_vec.try_into().map_err(|_| anyhow::anyhow!("invalid nonce"))?;
        let signature_vec = URL_SAFE_NO_PAD.decode(header(headers, "x-device-signature")?)?;
        let signature = Signature::from_slice(&signature_vec)?;
        let digest: [u8; 32] = Sha256::digest(body).into();
        let path = uri.path_and_query().map_or(uri.path(), |p| p.as_str());
        let message = http_auth_message(id, method.as_str(), path, &digest, timestamp_str, &nonce);
        verifying_key(public_key)?.verify(&message, &signature).map_err(|_| anyhow::anyhow!("invalid device signature"))?;
        let mut nonces = self.nonces.lock().unwrap();
        if nonces.len() >= MAX_NONCES { nonces.retain(|_, t| now_ms.saturating_sub(*t) <= CLOCK_SKEW_MS); }
        if nonces.len() >= MAX_NONCES { bail!("nonce cache saturated"); }
        if nonces.insert((id.into(), nonce), now_ms).is_some() { bail!("replayed device request"); }
        Ok(())
    }
}

pub fn http_auth_message(device_id: &str, method: &str, path_and_query: &str, digest: &[u8; 32], timestamp_ms: &str, nonce: &[u8; 16]) -> Vec<u8> {
    let mut out = b"alondra-http-auth-v1".to_vec();
    for field in [device_id.as_bytes(), method.as_bytes(), path_and_query.as_bytes(), digest.as_slice(), timestamp_ms.as_bytes(), nonce.as_slice()] {
        out.extend_from_slice(&(field.len() as u32).to_be_bytes());
        out.extend_from_slice(field);
    }
    out
}

fn parse_jwks(jwks: Jwks) -> Result<HashMap<String, (Vec<u8>, Vec<u8>)>> {
    let mut result = HashMap::new();
    let mut ids = HashSet::new();
    for j in jwks.keys {
        if j.kty != "RSA" || j.usage.as_deref().is_some_and(|v| v != "sig") || j.alg.as_deref().is_some_and(|v| v != "RS256") { continue; }
        if !ids.insert(j.kid.clone()) { bail!("duplicate JWK key ID"); }
        result.insert(j.kid, (URL_SAFE_NO_PAD.decode(j.n)?, URL_SAFE_NO_PAD.decode(j.e)?));
    }
    if result.is_empty() { bail!("JWKS has no RS256 signing keys"); }
    Ok(result)
}

fn verifying_key(encoded: &str) -> Result<VerifyingKey> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded)?;
    Ok(VerifyingKey::from_sec1_bytes(&bytes)?)
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str> {
    Ok(headers.get(name).context("missing device auth header")?.to_str()?)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("device registry needs a parent directory")?;
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}
