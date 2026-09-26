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
    sync::{
        Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Mutex as AsyncMutex;

const CLOCK_SKEW_MS: u64 = 120_000;
const MAX_NONCES: usize = 100_000;
const MAX_JWKS_BYTES: usize = 64 * 1024;
const JWKS_TIMEOUT: Duration = Duration::from_secs(5);
const JWKS_MAX_AGE: Duration = Duration::from_secs(60 * 60);
const UNKNOWN_KID_RETRY: Duration = Duration::from_secs(30);
const REGISTRY_SCHEMA_VERSION: u32 = 2;
const REGISTRY_GUARD: &[u8] = b"alondra-relay-device-registry-v2\n";

#[cfg(test)]
pub(crate) static FAIL_AFTER_RENAME_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

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
    /// Optional APNs signing credential. Push endpoints fail closed when omitted.
    #[serde(default)]
    pub apns: Option<crate::push::ApnsConfig>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PushRegistration {
    pub token: String,
    pub environment: String,
    pub push_binding: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PhoneDevice {
    pub device_id: String,
    pub owner_sub: String,
    pub signing_key: String,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MacDevice {
    pub mac_id: String,
    pub owner_sub: String,
    pub signing_key: String,
    pub agreement_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MacRegistration {
    pub mac_id: String,
    pub signing_key: String,
    pub agreement_key: String,
}

#[derive(Default)]
struct DeviceRegistry {
    devices: HashMap<String, PhoneDevice>,
    phone_macs: HashMap<String, String>,
    revoked_ids: HashSet<String>,
    push: HashMap<String, PushRegistration>,
    macs: HashMap<String, MacDevice>,
    revoked_mac_ids: HashSet<String>,
}

#[derive(Serialize, Deserialize)]
struct StoredDeviceRegistry {
    #[serde(default)]
    schema_version: Option<u32>,
    devices: Vec<PhoneDevice>,
    #[serde(default)]
    phone_macs: HashMap<String, String>,
    revoked_ids: Vec<String>,
    #[serde(default)]
    push: HashMap<String, PushRegistration>,
    #[serde(default)]
    macs: Vec<MacDevice>,
    #[serde(default)]
    revoked_mac_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RegistryOnDisk {
    Current(StoredDeviceRegistry),
    Legacy(Vec<PhoneDevice>),
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
    devices: Mutex<DeviceRegistry>,
    registry_poisoned: AtomicBool,
    verifier: Mutex<HttpChallengeVerifier>,
    #[cfg(test)]
    verification_time: Mutex<Option<u64>>,
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
        let jwks_url_allowed = config.jwks_url.starts_with("https://")
            || config.jwks_url.starts_with("http://127.0.0.1:")
            || (config.allow_internal_http_jwks
                && config.jwks_url.starts_with("http://keycloak:8080/"));
        if !jwks_url_allowed {
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
        let guard_path = schema_guard_path(&config.device_registry_path);
        let guarded = guard_path.exists();
        if guarded && std::fs::read(&guard_path)? != REGISTRY_GUARD {
            bail!("invalid device registry schema guard");
        }
        if guarded && !config.device_registry_path.exists() {
            bail!("guarded device registry is missing");
        }
        let mut migrated_registry = !guarded;
        let mut devices = if config.device_registry_path.exists() {
            let raw = std::fs::read(&config.device_registry_path)?;
            let (schema_version, list, mut phone_macs, revoked_ids, push, macs, revoked_mac_ids) =
                match serde_json::from_slice(&raw)? {
                    RegistryOnDisk::Current(StoredDeviceRegistry {
                        schema_version,
                        devices,
                        phone_macs,
                        revoked_ids,
                        push,
                        macs,
                        revoked_mac_ids,
                    }) => (
                        schema_version,
                        devices,
                        phone_macs,
                        revoked_ids,
                        push,
                        macs,
                        revoked_mac_ids,
                    ),
                    RegistryOnDisk::Legacy(devices) => (
                        None,
                        devices,
                        HashMap::new(),
                        Vec::new(),
                        HashMap::new(),
                        Vec::new(),
                        Vec::new(),
                    ),
                };
            if guarded && schema_version != Some(REGISTRY_SCHEMA_VERSION) {
                bail!(
                    "device registry was rewritten by an older version; refusing unsafe migration"
                );
            }
            if !guarded && schema_version.is_some() {
                bail!("device registry guard is missing");
            }
            let devices: HashMap<_, _> =
                list.into_iter().map(|d| (d.device_id.clone(), d)).collect();
            if phone_macs.keys().any(|id| !devices.contains_key(id)) {
                bail!("phone binding references inactive device");
            }
            let missing_bindings = devices.keys().any(|id| !phone_macs.contains_key(id));
            if guarded && missing_bindings {
                bail!("guarded registry is missing phone bindings");
            }
            if missing_bindings
                && (macs.iter().any(|mac| mac.mac_id != config.mac_id)
                    || !revoked_mac_ids.is_empty())
            {
                bail!("ambiguous phone binding in multi-Mac registry");
            }
            for id in devices.keys() {
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    phone_macs.entry(id.clone())
                {
                    entry.insert(config.mac_id.clone());
                    migrated_registry = true;
                }
            }
            let revoked_ids: HashSet<_> = revoked_ids.into_iter().collect();
            if devices.keys().any(|id| revoked_ids.contains(id)) {
                bail!("device registry contains active and revoked duplicate ID");
            }
            if push.keys().any(|id| !devices.contains_key(id)) {
                bail!("push registration references an inactive device");
            }
            let mac_count = macs.len();
            let macs: HashMap<_, _> = macs.into_iter().map(|m| (m.mac_id.clone(), m)).collect();
            if macs.len() != mac_count {
                bail!("duplicate stored Mac ID");
            }
            let revoked_mac_ids: HashSet<_> = revoked_mac_ids.into_iter().collect();
            if macs
                .keys()
                .any(|id| revoked_mac_ids.contains(id) || devices.contains_key(id))
                || revoked_mac_ids.iter().any(|id| devices.contains_key(id))
                || revoked_ids.iter().any(|id| macs.contains_key(id))
            {
                bail!("registry contains duplicate or revoked device ID");
            }
            for mac in macs.values() {
                if !valid_device_id(&mac.mac_id) || mac.owner_sub.is_empty() {
                    bail!("invalid stored Mac identity");
                }
                verifying_key(&mac.signing_key)?;
                if !mac.agreement_key.is_empty() {
                    verifying_key(&mac.agreement_key)?;
                }
            }
            DeviceRegistry {
                devices,
                phone_macs,
                revoked_ids,
                push,
                macs,
                revoked_mac_ids,
            }
        } else {
            DeviceRegistry::default()
        };
        // The pre-service pilot's pinned Mac is imported without replacing any durable entry.
        // A revoked pinned ID stays revoked across restarts.
        if !devices.revoked_mac_ids.contains(&config.mac_id) {
            match devices.macs.get(&config.mac_id) {
                Some(existing)
                    if existing.owner_sub != config.owner_sub
                        || existing.signing_key != config.mac_signing_key =>
                {
                    bail!("bootstrap Mac conflicts with durable registry");
                }
                Some(_) => {}
                None => {
                    if devices.devices.contains_key(&config.mac_id)
                        || devices.revoked_ids.contains(&config.mac_id)
                    {
                        bail!("bootstrap Mac ID conflicts with phone registry");
                    }
                    devices.macs.insert(
                        config.mac_id.clone(),
                        MacDevice {
                            mac_id: config.mac_id.clone(),
                            owner_sub: config.owner_sub.clone(),
                            signing_key: config.mac_signing_key.clone(),
                            agreement_key: String::new(),
                        },
                    );
                    migrated_registry = true;
                }
            }
        }
        for (phone_id, mac_id) in &devices.phone_macs {
            let phone = devices
                .devices
                .get(phone_id)
                .context("missing bound phone")?;
            if let Some(mac) = devices.macs.get(mac_id) {
                if phone.owner_sub != mac.owner_sub {
                    bail!("phone binding crosses owners");
                }
            } else if !devices.revoked_mac_ids.contains(mac_id) {
                bail!("phone binding references unknown Mac");
            }
        }
        if migrated_registry {
            if !guarded {
                write_atomic(&guard_path, REGISTRY_GUARD)?;
            }
            persist_registry_at(&config.device_registry_path, &devices)?;
        }
        let verifier = HttpChallengeVerifier::new(CLOCK_SKEW_MS, MAX_NONCES)
            .map_err(|_| anyhow::anyhow!("could not initialize device challenge verifier"))?;
        Ok(Self {
            config,
            jwks: RwLock::new(jwks),
            jwks_refresh: AsyncMutex::new(()),
            last_unknown_refresh: Mutex::new(None),
            devices: Mutex::new(devices),
            registry_poisoned: AtomicBool::new(false),
            verifier: Mutex::new(verifier),
            #[cfg(test)]
            verification_time: Mutex::new(None),
        })
    }

    #[cfg(test)]
    pub(crate) fn test(config: Config, jwks_json: &[u8]) -> Result<Self> {
        Self::from_jwks(config, serde_json::from_slice(jwks_json)?)
    }

    pub fn enroll(&self, mac: &Identity, device: PhoneDevice) -> Result<()> {
        self.ensure_registry_ready()?;
        if device.owner_sub != mac.owner_sub || !valid_device_id(&device.device_id) {
            bail!("invalid phone device registration");
        }
        verifying_key(&device.signing_key)?;
        let mut registry = self.devices.lock().unwrap();
        if registry.revoked_ids.contains(&device.device_id)
            || registry.macs.contains_key(&device.device_id)
            || registry.revoked_mac_ids.contains(&device.device_id)
        {
            bail!("revoked device ID cannot be enrolled again");
        }
        if let Some(existing) = registry.devices.get(&device.device_id) {
            if existing.owner_sub != device.owner_sub
                || existing.signing_key != device.signing_key
                || registry.phone_macs.get(&device.device_id) != Some(&mac.device_id)
            {
                bail!("device key replacement requires a new device ID and fresh pairing");
            }
            return Ok(());
        }
        let new_id = device.device_id.clone();
        registry.devices.insert(new_id.clone(), device);
        registry
            .phone_macs
            .insert(new_id.clone(), mac.device_id.clone());
        if let Err(e) = self.persist_registry(&registry) {
            // Enrollment must not become usable unless registry durability succeeds.
            registry.devices.remove(&new_id);
            registry.phone_macs.remove(&new_id);
            return Err(e);
        }
        Ok(())
    }

    /// A Mac-signed revocation removes the public key durably before queued data is purged.
    pub fn revoke(&self, mac: &Identity, device_id: &str) -> Result<()> {
        self.ensure_registry_ready()?;
        if !valid_device_id(device_id) {
            bail!("invalid device ID");
        }
        let mut registry = self.devices.lock().unwrap();
        if registry.macs.contains_key(device_id) || registry.revoked_mac_ids.contains(device_id) {
            bail!("not a phone device");
        }
        if let Some(device) = registry.devices.get(device_id) {
            if device.owner_sub != mac.owner_sub {
                bail!("device belongs to another owner");
            }
            if registry.phone_macs.get(device_id) != Some(&mac.device_id) {
                bail!("device is paired to another Mac");
            }
        } else if !registry.revoked_ids.contains(device_id) && !valid_client_id(device_id) {
            bail!("unknown phone device");
        }
        if registry.revoked_ids.contains(device_id) {
            return Ok(());
        }
        let previous = registry.devices.remove(device_id);
        let previous_mac = registry.phone_macs.remove(device_id);
        let previous_push = registry.push.remove(device_id);
        registry.revoked_ids.insert(device_id.to_owned());
        if let Err(e) = self.persist_registry(&registry) {
            registry.revoked_ids.remove(device_id);
            if let Some(previous) = previous {
                registry.devices.insert(device_id.to_owned(), previous);
            }
            if let Some(previous) = previous_mac {
                registry.phone_macs.insert(device_id.to_owned(), previous);
            }
            if let Some(previous) = previous_push {
                registry.push.insert(device_id.to_owned(), previous);
            }
            return Err(e);
        }
        Ok(())
    }

    /// Store token and binding atomically with device state; an old binding cannot remain after revoke.
    pub fn register_push(&self, device_id: &str, registration: PushRegistration) -> Result<()> {
        self.ensure_registry_ready()?;
        crate::push::validate_registration(&registration)?;
        let mut registry = self.devices.lock().unwrap();
        if !registry.devices.contains_key(device_id) || registry.revoked_ids.contains(device_id) {
            bail!("unknown or revoked device");
        }
        let old = registry.push.insert(device_id.to_owned(), registration);
        if let Err(e) = self.persist_registry(&registry) {
            if let Some(old) = old {
                registry.push.insert(device_id.to_owned(), old);
            } else {
                registry.push.remove(device_id);
            }
            return Err(e);
        }
        Ok(())
    }

    pub fn push_registration(&self, device_id: &str) -> Option<PushRegistration> {
        if self.ensure_registry_ready().is_err() {
            return None;
        }
        self.devices.lock().unwrap().push.get(device_id).cloned()
    }

    /// Validate the access token and the proof signed by the key being registered.
    pub async fn authorize_mac_enrollment(
        &self,
        headers: &HeaderMap,
        method: &Method,
        uri: &Uri,
        body: &[u8],
        now_ms: u64,
        request: MacRegistration,
    ) -> Result<MacDevice> {
        self.ensure_registry_ready()?;
        if !valid_device_id(&request.mac_id) || request.agreement_key.is_empty() {
            bail!("invalid Mac registration");
        }
        canonical_public_key(&request.signing_key)?;
        canonical_public_key(&request.agreement_key)?;
        let token = header(headers, "authorization")?
            .strip_prefix("Bearer ")
            .context("expected Bearer token")?;
        let owner_sub = self.verify_jwt(token, now_ms).await?;
        self.verify_device(
            headers,
            method,
            uri,
            body,
            now_ms,
            &request.mac_id,
            &request.signing_key,
        )?;
        Ok(MacDevice {
            mac_id: request.mac_id,
            owner_sub,
            signing_key: request.signing_key,
            agreement_key: request.agreement_key,
        })
    }

    /// Register only a new ID or complete the pilot record under its existing signing key.
    pub fn enroll_mac(&self, mac: MacDevice) -> Result<bool> {
        self.ensure_registry_ready()?;
        let mut registry = self.devices.lock().unwrap();
        if registry.revoked_mac_ids.contains(&mac.mac_id)
            || registry.revoked_ids.contains(&mac.mac_id)
            || registry.devices.contains_key(&mac.mac_id)
        {
            bail!("Mac ID unavailable");
        }
        if let Some(existing) = registry.macs.get(&mac.mac_id).cloned() {
            if existing == mac {
                return Ok(false);
            }
            // The pinned pilot record predates agreement-key storage. Its actual
            // signing key must prove possession before this one-time completion.
            if existing.owner_sub == mac.owner_sub
                && existing.signing_key == mac.signing_key
                && existing.agreement_key.is_empty()
            {
                registry.macs.insert(mac.mac_id.clone(), mac.clone());
                if let Err(err) = self.persist_registry(&registry) {
                    registry.macs.insert(mac.mac_id.clone(), existing);
                    return Err(err);
                }
                return Ok(true);
            }
            bail!("Mac key replacement requires a new ID");
        }
        registry.macs.insert(mac.mac_id.clone(), mac.clone());
        if let Err(err) = self.persist_registry(&registry) {
            registry.macs.remove(&mac.mac_id);
            return Err(err);
        }
        Ok(true)
    }

    pub fn revoke_mac(&self, who: &Identity) -> Result<()> {
        self.ensure_registry_ready()?;
        let mut registry = self.devices.lock().unwrap();
        let existing = registry
            .macs
            .remove(&who.device_id)
            .context("unknown Mac")?;
        if existing.owner_sub != who.owner_sub {
            registry.macs.insert(who.device_id.clone(), existing);
            bail!("Mac owner mismatch");
        }
        registry.revoked_mac_ids.insert(who.device_id.clone());
        if let Err(err) = self.persist_registry(&registry) {
            registry.revoked_mac_ids.remove(&who.device_id);
            registry.macs.insert(who.device_id.clone(), existing);
            return Err(err);
        }
        Ok(())
    }

    /// Account metadata only: knowing a Mac ID grants no device or content access.
    pub async fn account_owner(&self, headers: &HeaderMap, now_ms: u64) -> Result<String> {
        let token = header(headers, "authorization")?
            .strip_prefix("Bearer ").context("expected Bearer token")?;
        self.verify_jwt(token, now_ms).await
    }

    pub fn mac_ids_for_owner(&self, owner_sub: &str) -> Result<Vec<String>> {
        self.ensure_registry_ready()?;
        let registry = self.devices.lock().unwrap();
        let mut ids: Vec<String> = registry.macs.values()
            .filter(|mac| mac.owner_sub == owner_sub && !registry.revoked_mac_ids.contains(&mac.mac_id))
            .map(|mac| mac.mac_id.clone()).collect();
        ids.sort();
        Ok(ids)
    }

    pub fn mac_for_owner(&self, mac_id: &str, owner_sub: &str) -> Result<VerifyingKey> {
        self.ensure_registry_ready()?;
        let registry = self.devices.lock().unwrap();
        let mac = registry.macs.get(mac_id).context("unknown Mac")?;
        if mac.owner_sub != owner_sub {
            bail!("Mac belongs to another owner");
        }
        verifying_key(&mac.signing_key)
    }

    pub fn phone_for_mac(&self, phone_id: &str, mac: &Identity) -> Result<()> {
        self.ensure_registry_ready()?;
        let registry = self.devices.lock().unwrap();
        let phone = registry.devices.get(phone_id).context("unknown phone")?;
        if phone.owner_sub != mac.owner_sub
            || registry.phone_macs.get(phone_id) != Some(&mac.device_id)
        {
            bail!("phone belongs to another owner");
        }
        Ok(())
    }

    pub fn paired_mac_for_phone(&self, phone_id: &str, owner_sub: &str) -> Result<String> {
        self.ensure_registry_ready()?;
        let registry = self.devices.lock().unwrap();
        let phone = registry.devices.get(phone_id).context("unknown phone")?;
        if phone.owner_sub != owner_sub {
            bail!("phone owner mismatch");
        }
        registry
            .phone_macs
            .get(phone_id)
            .cloned()
            .context("phone has no paired Mac")
    }

    fn persist_registry(&self, registry: &DeviceRegistry) -> Result<()> {
        let result = persist_registry_at(&self.config.device_registry_path, registry);
        if result.is_err() {
            // A failure may occur after rename. Do not trust the in-memory rollback
            // until a fresh process has loaded the disk state.
            self.registry_poisoned.store(true, Ordering::Release);
        }
        result
    }

    fn ensure_registry_ready(&self) -> Result<()> {
        if self.registry_poisoned.load(Ordering::Acquire) {
            bail!("registry persistence failed; restart required");
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn is_mac_registered(&self, mac_id: &str) -> bool {
        self.devices.lock().unwrap().macs.contains_key(mac_id)
    }

    pub fn authenticate_mac(
        &self,
        headers: &HeaderMap,
        method: &Method,
        uri: &Uri,
        body: &[u8],
        now_ms: u64,
    ) -> Result<Identity> {
        self.ensure_registry_ready()?;
        let id = header(headers, "x-device-id")?;
        let mac = self
            .devices
            .lock()
            .unwrap()
            .macs
            .get(id)
            .cloned()
            .context("unknown Mac")?;
        self.verify_device(
            headers,
            method,
            uri,
            body,
            now_ms,
            &mac.mac_id,
            &mac.signing_key,
        )?;
        Ok(Identity {
            device_id: mac.mac_id,
            owner_sub: mac.owner_sub,
        })
    }

    pub fn mac_key(&self, mac_id: &str) -> Result<VerifyingKey> {
        self.ensure_registry_ready()?;
        let mac = self
            .devices
            .lock()
            .unwrap()
            .macs
            .get(mac_id)
            .cloned()
            .context("unknown Mac")?;
        verifying_key(&mac.signing_key)
    }


    pub fn mac_public_identity(&self, mac_id: &str) -> Result<MacDevice> {
        self.ensure_registry_ready()?;
        self.devices.lock().unwrap().macs.get(mac_id).cloned().context("unknown Mac")
    }

    pub fn server_epoch(&self) -> [u8; 16] {
        self.verifier.lock().unwrap().server_epoch()
    }

    pub fn jwks_ready(&self) -> bool {
        !self.registry_poisoned.load(Ordering::Acquire)
            && self.jwks.read().unwrap().refreshed_at.elapsed() <= JWKS_MAX_AGE
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

    #[cfg(test)]
    pub fn is_phone_registered(&self, device_id: &str) -> bool {
        self.devices.lock().unwrap().devices.contains_key(device_id)
    }

    pub async fn authenticate_phone(
        &self,
        headers: &HeaderMap,
        method: &Method,
        uri: &Uri,
        body: &[u8],
        now_ms: u64,
    ) -> Result<Identity> {
        self.ensure_registry_ready()?;
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
            .devices
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

    /// A candidate is not enrolled yet. Its account token and proof of possession of the
    /// advertised signing key are both required before its opaque request enters the mailbox.
    pub async fn authenticate_pairing_candidate(
        &self,
        headers: &HeaderMap,
        method: &Method,
        uri: &Uri,
        body: &[u8],
        now_ms: u64,
        device_id: &str,
        signing_key: &str,
        mac_id: &str,
    ) -> Result<Identity> {
        if !valid_client_id(device_id) {
            bail!("invalid candidate ID");
        }
        canonical_public_key(signing_key)?;
        let token = header(headers, "authorization")?
            .strip_prefix("Bearer ")
            .context("expected Bearer token")?;
        let sub = self.verify_jwt(token, now_ms).await?;
        self.mac_for_owner(mac_id, &sub)?;
        self.verify_device(headers, method, uri, body, now_ms, device_id, signing_key)?;
        Ok(Identity { device_id: device_id.to_owned(), owner_sub: sub })
    }

    pub fn phone_key(&self, device_id: &str) -> Result<VerifyingKey> {
        self.ensure_registry_ready()?;
        let device = self
            .devices
            .lock()
            .unwrap()
            .devices
            .get(device_id)
            .cloned()
            .context("unknown device")?;
        verifying_key(&device.signing_key)
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

    #[cfg(test)]
    pub(crate) fn set_verification_time(&self, now_ms: u64) {
        *self.verification_time.lock().unwrap() = Some(now_ms);
    }

    #[allow(clippy::too_many_arguments)] // The signed HTTP request and bound identity are intentionally explicit.
    fn verify_device(
        &self,
        headers: &HeaderMap,
        method: &Method,
        uri: &Uri,
        body: &[u8],
        _request_started_ms: u64,
        id: &str,
        public_key: &str,
    ) -> Result<()> {
        self.ensure_registry_ready()?;
        if header(headers, "x-device-id")? != id {
            bail!("device ID mismatch");
        }
        let timestamp_str = header(headers, "x-device-timestamp-ms")?;
        let timestamp: u64 = timestamp_str.parse()?;
        if timestamp.to_string() != timestamp_str {
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
        let key = verifying_key(public_key)?;
        let mut verifier = self.verifier.lock().unwrap();
        // Request arrival order can differ from verification order after JWT I/O
        // or mutex contention. Sample under this lock so that only an actual
        // wall-clock rollback trips the verifier's rollback protection.
        #[cfg(not(test))]
        let now_ms = crate::now_ms();
        #[cfg(test)]
        let now_ms = self
            .verification_time
            .lock()
            .unwrap()
            .unwrap_or_else(crate::now_ms);
        verifier
            .verify(
                &key,
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

fn canonical_public_key(encoded: &str) -> Result<()> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded)?;
    if bytes.len() != 65 || bytes[0] != 4 || URL_SAFE_NO_PAD.encode(&bytes) != encoded {
        bail!("expected canonical uncompressed P-256 public key");
    }
    VerifyingKey::from_sec1_bytes(&bytes)?;
    Ok(())
}

fn valid_client_id(id: &str) -> bool {
    // Client keys have the same authority on iOS and macOS. Keep persisted pilot
    // phone IDs valid while new clients use the platform-neutral namespace.
    valid_device_id(id) && (id.starts_with("client-") || id.starts_with("phone-"))
}

fn valid_device_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn schema_guard_path(path: &Path) -> PathBuf {
    path.with_extension("schema-v2")
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str> {
    Ok(headers
        .get(name)
        .context("missing device auth header")?
        .to_str()?)
}

fn persist_registry_at(path: &Path, registry: &DeviceRegistry) -> Result<()> {
    let mut devices: Vec<_> = registry.devices.values().cloned().collect();
    devices.sort_by(|a, b| a.device_id.cmp(&b.device_id));
    let mut revoked_ids: Vec<_> = registry.revoked_ids.iter().cloned().collect();
    revoked_ids.sort();
    let mut macs: Vec<_> = registry.macs.values().cloned().collect();
    macs.sort_by(|a, b| a.mac_id.cmp(&b.mac_id));
    let mut revoked_mac_ids: Vec<_> = registry.revoked_mac_ids.iter().cloned().collect();
    revoked_mac_ids.sort();
    write_atomic(
        path,
        &serde_json::to_vec(&StoredDeviceRegistry {
            schema_version: Some(REGISTRY_SCHEMA_VERSION),
            devices,
            phone_macs: registry.phone_macs.clone(),
            revoked_ids,
            push: registry.push.clone(),
            macs,
            revoked_mac_ids,
        })?,
    )
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
    #[cfg(test)]
    {
        let mut fail = FAIL_AFTER_RENAME_PATH.lock().unwrap();
        if fail.as_deref() == Some(path) {
            *fail = None;
            bail!("injected post-rename directory sync failure");
        }
    }
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}
