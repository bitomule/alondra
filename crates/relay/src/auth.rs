use alondra_e2e::HttpChallengeVerifier;
use anyhow::{ensure, Context, Result, bail};
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
const IROH_NODE_PROOF_DOMAIN: &[u8] = b"alondra-iroh-node-v1\0";

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

/// The Keycloak offline session whose token registered a push. When that session ends the
/// phone can no longer act on what it is sent, so the registration ends with it.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PushSession {
    pub sid: String,
    pub client_id: String,
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

#[derive(Default, Clone)]
struct DeviceRegistry {
    devices: HashMap<String, PhoneDevice>,
    phone_macs: HashMap<String, String>,
    revoked_ids: HashSet<String>,
    push: HashMap<String, PushRegistration>,
    push_sessions: HashMap<String, PushSession>,
    push_ended: HashSet<String>,
    macs: HashMap<String, MacDevice>,
    revoked_mac_ids: HashSet<String>,
    /// Device or Mac ID → its iroh endpoint ID (hex Ed25519 public key).
    iroh_nodes: HashMap<String, String>,
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
    push_sessions: HashMap<String, PushSession>,
    #[serde(default)]
    push_ended: Vec<String>,
    #[serde(default)]
    macs: Vec<MacDevice>,
    #[serde(default)]
    revoked_mac_ids: Vec<String>,
    #[serde(default)]
    iroh_nodes: HashMap<String, String>,
}

/// A device or Mac names the iroh endpoint that may use the iroh relay on its behalf. `proof` is
/// the endpoint key's Ed25519 signature over the proof domain and the device ID.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrohNodeBinding {
    pub iroh_node_id: String,
    pub proof: String,
}

/// The iroh endpoint a Mac saw one of its devices say hello from, with that hello's signature.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceIrohNodeBinding {
    pub iroh_node_id: String,
    pub hello_signature: String,
}

/// Who an iroh endpoint ID speaks for, so the account gate can be checked before relaying.
#[derive(Debug, PartialEq, Eq)]
pub struct IrohNodeHolder {
    pub owner_sub: String,
    pub mac_id: String,
    pub client_id: Option<String>,
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
#[derive(Deserialize)]
struct SessionClaims {
    sid: Option<String>,
    azp: Option<String>,
    scope: Option<String>,
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
    pub management: Option<crate::management::Management>,
    jwks: RwLock<CachedJwks>,
    jwks_refresh: AsyncMutex<()>,
    last_unknown_refresh: Mutex<Option<Instant>>,
    devices: Mutex<DeviceRegistry>,
    registry_poisoned: AtomicBool,
    verifier: Mutex<HttpChallengeVerifier>,
    #[cfg(test)]
    verification_time: Mutex<Option<u64>>,
    #[cfg(test)]
    live_route_checks: std::sync::atomic::AtomicUsize,
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
            let (
                schema_version,
                list,
                mut phone_macs,
                revoked_ids,
                push,
                mut push_sessions,
                push_ended,
                macs,
                revoked_mac_ids,
                iroh_nodes,
            ) = match serde_json::from_slice(&raw)? {
                RegistryOnDisk::Current(StoredDeviceRegistry {
                    schema_version,
                    devices,
                    phone_macs,
                    revoked_ids,
                    push,
                    push_sessions,
                    push_ended,
                    macs,
                    revoked_mac_ids,
                    iroh_nodes,
                }) => (
                    schema_version,
                    devices,
                    phone_macs,
                    revoked_ids,
                    push,
                    push_sessions,
                    push_ended,
                    macs,
                    revoked_mac_ids,
                    iroh_nodes,
                ),
                RegistryOnDisk::Legacy(devices) => (
                    None,
                    devices,
                    HashMap::new(),
                    Vec::new(),
                    HashMap::new(),
                    HashMap::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    HashMap::new(),
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
            push_sessions.retain(|id, _| push.contains_key(id));
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
            if iroh_nodes
                .keys()
                .any(|id| !devices.contains_key(id) && !macs.contains_key(id))
            {
                bail!("iroh node binding references inactive device");
            }
            let distinct: HashSet<_> = iroh_nodes.values().collect();
            if distinct.len() != iroh_nodes.len()
                || iroh_nodes.values().any(|node| iroh_node_key(node).is_none())
            {
                bail!("invalid or duplicate iroh node binding");
            }
            DeviceRegistry {
                devices,
                phone_macs,
                revoked_ids,
                push,
                push_sessions,
                push_ended: push_ended.into_iter().collect(),
                macs,
                revoked_mac_ids,
                iroh_nodes,
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
            management: None,
            jwks_refresh: AsyncMutex::new(()),
            last_unknown_refresh: Mutex::new(None),
            devices: Mutex::new(devices),
            registry_poisoned: AtomicBool::new(false),
            verifier: Mutex::new(verifier),
            #[cfg(test)]
            verification_time: Mutex::new(None),
            #[cfg(test)]
            live_route_checks: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    #[cfg(test)]
    pub(crate) fn test(config: Config, jwks_json: &[u8]) -> Result<Self> {
        Self::from_jwks(config, serde_json::from_slice(jwks_json)?)
    }

    pub fn enroll(&self, mac: &Identity, device: PhoneDevice) -> Result<()> {
        self.ensure_registry_ready()?;
        if device.owner_sub != mac.owner_sub || !valid_device_id(&device.device_id) {
            bail!(EnrollmentError::InvalidRegistration);
        }
        verifying_key(&device.signing_key).map_err(|_| EnrollmentError::InvalidRegistration)?;
        let mut registry = self.devices.lock().unwrap();
        if registry.revoked_ids.contains(&device.device_id)
            || registry.macs.contains_key(&device.device_id)
            || registry.revoked_mac_ids.contains(&device.device_id)
        {
            bail!(EnrollmentError::Conflict);
        }
        if let Some(existing) = registry.devices.get(&device.device_id) {
            if existing.owner_sub != device.owner_sub
                || existing.signing_key != device.signing_key
                || registry.phone_macs.get(&device.device_id) != Some(&mac.device_id)
            {
                bail!(EnrollmentError::Conflict);
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
        let previous_iroh = registry.iroh_nodes.remove(device_id);
        registry.revoked_ids.insert(device_id.to_owned());
        let newly_ended = registry.push_ended.insert(device_id.to_owned());
        if let Err(e) = self.persist_registry(&registry) {
            registry.revoked_ids.remove(device_id);
            if newly_ended {
                registry.push_ended.remove(device_id);
            }
            if let Some(previous) = previous {
                registry.devices.insert(device_id.to_owned(), previous);
            }
            if let Some(previous) = previous_mac {
                registry.phone_macs.insert(device_id.to_owned(), previous);
            }
            if let Some(previous) = previous_push {
                registry.push.insert(device_id.to_owned(), previous);
            }
            if let Some(previous) = previous_iroh {
                registry.iroh_nodes.insert(device_id.to_owned(), previous);
            }
            return Err(e);
        }
        Ok(())
    }

    /// Store token and binding atomically with device state; an old binding cannot remain after revoke.
    /// A token belongs to one app install, which holds one pairing per Mac: re-pairing with the
    /// same Mac supersedes the older pairing's registration, whose keys the phone no longer has.
    pub fn register_push(
        &self,
        device_id: &str,
        registration: PushRegistration,
        session: Option<PushSession>,
    ) -> Result<()> {
        self.ensure_registry_ready()?;
        crate::push::validate_registration(&registration)?;
        let mut registry = self.devices.lock().unwrap();
        let Some(owner) = registry.devices.get(device_id).map(|d| d.owner_sub.clone()) else {
            bail!("unknown or revoked device");
        };
        if registry.revoked_ids.contains(device_id) {
            bail!("unknown or revoked device");
        }
        let mac = registry.phone_macs.get(device_id).cloned();
        let superseded: Vec<String> = registry
            .push
            .iter()
            .filter(|(id, old)| {
                id.as_str() != device_id
                    && old.token == registration.token
                    && registry.phone_macs.get(id.as_str()) == mac.as_ref()
                    && registry.devices.get(id.as_str()).map(|d| &d.owner_sub) == Some(&owner)
            })
            .map(|(id, _)| id.clone())
            .collect();
        let removed: Vec<(String, PushRegistration)> = superseded
            .into_iter()
            .filter_map(|id| registry.push.remove(&id).map(|old| (id, old)))
            .collect();
        let old = registry.push.insert(device_id.to_owned(), registration);
        let was_ended = registry.push_ended.remove(device_id);
        let old_session = match session {
            Some(session) => registry.push_sessions.insert(device_id.to_owned(), session),
            None => registry.push_sessions.remove(device_id),
        };
        if let Err(e) = self.persist_registry(&registry) {
            if let Some(old) = old {
                registry.push.insert(device_id.to_owned(), old);
            } else {
                registry.push.remove(device_id);
            }
            match old_session {
                Some(old) => registry.push_sessions.insert(device_id.to_owned(), old),
                None => registry.push_sessions.remove(device_id),
            };
            if was_ended {
                registry.push_ended.insert(device_id.to_owned());
            }
            registry.push.extend(removed);
            return Err(e);
        }
        Ok(())
    }

    /// The binding travels in every push, so the phone can stop them even after it lost its
    /// session, its keys and its pairing. Knowing it only ever stops pushes to that phone.
    pub fn unregister_push(&self, push_binding: &str) -> Result<()> {
        use subtle::ConstantTimeEq;
        self.ensure_registry_ready()?;
        let mut registry = self.devices.lock().unwrap();
        let Some(device_id) = registry
            .push
            .iter()
            .find(|(_, registration)| {
                bool::from(registration.push_binding.as_bytes().ct_eq(push_binding.as_bytes()))
            })
            .map(|(id, _)| id.clone())
        else {
            return Ok(());
        };
        let before = registry.clone();
        registry.push.remove(&device_id);
        registry.push_ended.insert(device_id);
        if let Err(e) = self.persist_registry(&registry) {
            *registry = before;
            return Err(e);
        }
        Ok(())
    }

    /// The relay ended this phone's registration on purpose, as opposed to never having had
    /// one: the Mac may forget its binding.
    pub fn push_ended(&self, device_id: &str) -> bool {
        self.ensure_registry_ready().is_ok()
            && self.devices.lock().unwrap().push_ended.contains(device_id)
    }

    /// Every push registration made with an offline session, grouped by account and client so
    /// one identity-provider lookup answers for all of that account's phones.
    pub fn push_sessions(&self) -> HashMap<(String, String), Vec<(String, String)>> {
        let mut grouped: HashMap<(String, String), Vec<(String, String)>> = HashMap::new();
        if self.ensure_registry_ready().is_err() {
            return grouped;
        }
        let registry = self.devices.lock().unwrap();
        for (device_id, session) in &registry.push_sessions {
            let Some(device) = registry.devices.get(device_id) else {
                continue;
            };
            if !registry.push.contains_key(device_id) {
                continue;
            }
            grouped
                .entry((device.owner_sub.clone(), session.client_id.clone()))
                .or_default()
                .push((device_id.clone(), session.sid.clone()));
        }
        grouped
    }

    /// The offline session that registered this push has ended. A registration made since by
    /// another session stays.
    pub fn forget_ended_session_push(&self, device_id: &str, sid: &str) -> Result<()> {
        self.ensure_registry_ready()?;
        let mut registry = self.devices.lock().unwrap();
        if registry
            .push_sessions
            .get(device_id)
            .is_none_or(|session| session.sid != sid)
        {
            return Ok(());
        }
        let before = registry.clone();
        registry.push.remove(device_id);
        registry.push_sessions.remove(device_id);
        registry.push_ended.insert(device_id.to_owned());
        if let Err(e) = self.persist_registry(&registry) {
            *registry = before;
            return Err(e);
        }
        Ok(())
    }

    /// The offline session of a bearer `authenticate_phone` already verified. Online sessions
    /// are not recorded: the phone only signs in with `offline_access`.
    pub fn bearer_push_session(headers: &HeaderMap) -> Option<PushSession> {
        let token = headers
            .get(axum::http::header::AUTHORIZATION)?
            .to_str()
            .ok()?
            .strip_prefix("Bearer ")?;
        let payload = token.split('.').nth(1)?;
        let claims: SessionClaims =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
        let offline = claims
            .scope
            .as_deref()
            .is_some_and(|scope| scope.split(' ').any(|s| s == "offline_access"));
        match (claims.sid, claims.azp) {
            (Some(sid), Some(client_id))
                if offline && valid_session_part(&sid) && valid_session_part(&client_id) =>
            {
                Some(PushSession { sid, client_id })
            }
            _ => None,
        }
    }

    /// APNs said this token will never be delivered to again; keep the slot empty until the
    /// phone registers a token. A registration that replaced it meanwhile stays.
    pub fn forget_dead_push(&self, device_id: &str, token: &str) -> Result<()> {
        self.ensure_registry_ready()?;
        let mut registry = self.devices.lock().unwrap();
        if registry.push.get(device_id).is_none_or(|current| current.token != token) {
            return Ok(());
        }
        let before = registry.clone();
        registry.push.remove(device_id);
        registry.push_ended.insert(device_id.to_owned());
        if let Err(e) = self.persist_registry(&registry) {
            *registry = before;
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
        self.require_live_route(&owner_sub,&request.mac_id,None).await?;
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
            bail!(EnrollmentError::Conflict);
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
            bail!(EnrollmentError::Conflict);
        }
        registry.macs.insert(mac.mac_id.clone(), mac.clone());
        if let Err(err) = self.persist_registry(&registry) {
            registry.macs.remove(&mac.mac_id);
            return Err(err);
        }
        Ok(true)
    }

    /// Product-account cleanup; caller holds device_gate exclusively.
    /// This removes cloud authorization, never local Mac trust or private keys.
    /// Deliberately metadata-only: no public keys or push tokens leave this API.
    pub fn account_device_metadata(&self,owner:&str)->Result<Vec<(String,Vec<String>)>> {
        self.ensure_registry_ready()?;
        let registry=self.devices.lock().unwrap();
        let mut macs=registry.macs.values().filter(|m|m.owner_sub==owner).map(|m| {
            let mut clients=registry.devices.values().filter(|d|d.owner_sub==owner && registry.phone_macs.get(&d.device_id)==Some(&m.mac_id)).map(|d|d.device_id.clone()).collect::<Vec<_>>();clients.sort();
            (m.mac_id.clone(),clients)
        }).collect::<Vec<_>>();macs.sort_by(|a,b|a.0.cmp(&b.0));Ok(macs)
    }
    pub fn reconcile_device_revocation(&self,owner:&str,kind:&str,id:&str,mac_id:&str)->Result<()> {
        self.ensure_registry_ready()?;
        let mut registry=self.devices.lock().unwrap();let before=registry.clone();
        let clients=if kind=="mac" {
            if let Some(mac)=registry.macs.get(id) {ensure!(mac.owner_sub==owner,"revocation owner mismatch");}
            registry.phone_macs.iter().filter(|(_,mac)|mac.as_str()==id).map(|(client,_)|client.clone()).collect::<Vec<_>>()
        } else {vec![id.to_owned()]};
        for client in &clients {
            if let Some(record)=registry.devices.get(client) {
                ensure!(record.owner_sub==owner && registry.phone_macs.get(client).map(String::as_str)==Some(mac_id),"revocation binding mismatch");
            }
        }
        if kind=="mac" {registry.macs.remove(id);registry.revoked_mac_ids.insert(id.to_owned());}
        for client in clients {registry.devices.remove(&client);registry.phone_macs.remove(&client);registry.push.remove(&client);registry.push_ended.insert(client.clone());registry.revoked_ids.insert(client);}
        forget_orphan_iroh_nodes(&mut registry);
        if let Err(error)=self.persist_registry(&registry) {*registry=before;return Err(error)}
        Ok(())
    }

    pub fn revoke_owner(&self,owner:&str)->Result<()> {
        self.ensure_registry_ready()?;
        let mut registry=self.devices.lock().unwrap();
        let before=registry.clone();
        let phones:Vec<String>=registry.devices.values().filter(|d|d.owner_sub==owner).map(|d|d.device_id.clone()).collect();
        let macs:Vec<String>=registry.macs.values().filter(|d|d.owner_sub==owner).map(|d|d.mac_id.clone()).collect();
        if phones.is_empty() && macs.is_empty() {return Ok(())}
        for id in phones {
            registry.devices.remove(&id);registry.phone_macs.remove(&id);registry.push.remove(&id);registry.push_ended.insert(id.clone());registry.revoked_ids.insert(id);
        }
        for id in macs {registry.macs.remove(&id);registry.revoked_mac_ids.insert(id);}
        forget_orphan_iroh_nodes(&mut registry);
        if let Err(error)=self.persist_registry(&registry) {*registry=before;return Err(error)}
        Ok(())
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
        let before = registry.clone();
        registry.revoked_mac_ids.insert(who.device_id.clone());
        let phones: Vec<String> = registry
            .phone_macs
            .iter()
            .filter(|(_, mac)| **mac == who.device_id)
            .map(|(phone, _)| phone.clone())
            .collect();
        for phone in phones {
            if registry.push.remove(&phone).is_some() {
                registry.push_ended.insert(phone);
            }
        }
        forget_orphan_iroh_nodes(&mut registry);
        if let Err(err) = self.persist_registry(&registry) {
            *registry = before;
            registry.macs.insert(who.device_id.clone(), existing);
            return Err(err);
        }
        Ok(())
    }

    /// Calling again with another endpoint replaces the old one, which loses relay access.
    pub fn bind_iroh_node(&self, who: &Identity, binding: IrohNodeBinding) -> Result<()> {
        self.ensure_registry_ready()?;
        let key = iroh_node_key(&binding.iroh_node_id)
            .ok_or(EnrollmentError::InvalidRegistration)?;
        let proof = URL_SAFE_NO_PAD
            .decode(&binding.proof)
            .map_err(|_| EnrollmentError::InvalidRegistration)?;
        let message = [IROH_NODE_PROOF_DOMAIN, who.device_id.as_bytes()].concat();
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
            .verify(&message, &proof)
            .map_err(|_| EnrollmentError::InvalidRegistration)?;
        let mut registry = self.devices.lock().unwrap();
        if !registry.devices.contains_key(&who.device_id)
            && !registry.macs.contains_key(&who.device_id)
        {
            bail!("unknown device");
        }
        if registry
            .iroh_nodes
            .iter()
            .any(|(id, node)| *node == binding.iroh_node_id && *id != who.device_id)
        {
            bail!(EnrollmentError::Conflict);
        }
        let previous = registry
            .iroh_nodes
            .insert(who.device_id.clone(), binding.iroh_node_id);
        if let Err(err) = self.persist_registry(&registry) {
            match previous {
                Some(node) => registry.iroh_nodes.insert(who.device_id.clone(), node),
                None => registry.iroh_nodes.remove(&who.device_id),
            };
            return Err(err);
        }
        Ok(())
    }

    /// A Mac binds the iroh endpoint of one of its own devices, one that has no account to bind
    /// it with (a terminal paired directly). `hello_signature` is the device's own iroh hello to
    /// this Mac from that endpoint, so the device itself vouches for it. Revoking the device, or
    /// the Mac, ends the binding as for any other device.
    pub fn bind_device_iroh_node(&self, mac: &Identity, device_id: &str, binding: DeviceIrohNodeBinding) -> Result<()> {
        self.ensure_registry_ready()?;
        let node = iroh_node_key(&binding.iroh_node_id).ok_or(EnrollmentError::InvalidRegistration)?;
        let mut registry = self.devices.lock().unwrap();
        let device = registry
            .devices
            .get(device_id)
            .filter(|device| device.owner_sub == mac.owner_sub)
            .filter(|_| registry.phone_macs.get(device_id) == Some(&mac.device_id))
            .context("unknown device")?;
        let mac_node = registry
            .iroh_nodes
            .get(&mac.device_id)
            .and_then(|node| iroh_node_key(node))
            .ok_or(EnrollmentError::InvalidRegistration)?;
        alondra_e2e::verify_iroh_hello(
            &verifying_key(&device.signing_key)?,
            &binding.hello_signature,
            &mac.device_id,
            device_id,
            &node,
            &mac_node,
        )
        .map_err(|_| EnrollmentError::InvalidRegistration)?;
        if registry
            .iroh_nodes
            .iter()
            .any(|(id, bound)| *bound == binding.iroh_node_id && id != device_id)
        {
            bail!(EnrollmentError::Conflict);
        }
        let previous = registry.iroh_nodes.insert(device_id.to_owned(), binding.iroh_node_id);
        if let Err(err) = self.persist_registry(&registry) {
            match previous {
                Some(node) => registry.iroh_nodes.insert(device_id.to_owned(), node),
                None => registry.iroh_nodes.remove(device_id),
            };
            return Err(err);
        }
        Ok(())
    }

    pub fn unbind_iroh_node(&self, who: &Identity) -> Result<()> {
        self.ensure_registry_ready()?;
        let mut registry = self.devices.lock().unwrap();
        let Some(previous) = registry.iroh_nodes.remove(&who.device_id) else {
            return Ok(());
        };
        if let Err(err) = self.persist_registry(&registry) {
            registry.iroh_nodes.insert(who.device_id.clone(), previous);
            return Err(err);
        }
        Ok(())
    }

    /// Only an endpoint bound to an enrolled Mac, or to a phone whose Mac is still enrolled,
    /// has a holder. The caller still checks the account gate for that route.
    pub fn iroh_node_holder(&self, iroh_node_id: &str) -> Result<Option<IrohNodeHolder>> {
        self.ensure_registry_ready()?;
        let registry = self.devices.lock().unwrap();
        let Some(id) = registry
            .iroh_nodes
            .iter()
            .find(|(_, node)| node.as_str() == iroh_node_id)
            .map(|(id, _)| id)
        else {
            return Ok(None);
        };
        if let Some(mac) = registry.macs.get(id) {
            return Ok(Some(IrohNodeHolder {
                owner_sub: mac.owner_sub.clone(),
                mac_id: mac.mac_id.clone(),
                client_id: None,
            }));
        }
        let (Some(phone), Some(mac_id)) = (registry.devices.get(id), registry.phone_macs.get(id))
        else {
            return Ok(None);
        };
        match registry.macs.get(mac_id) {
            Some(mac) if mac.owner_sub == phone.owner_sub => Ok(Some(IrohNodeHolder {
                owner_sub: phone.owner_sub.clone(),
                mac_id: mac_id.clone(),
                client_id: Some(id.clone()),
            })),
            _ => Ok(None),
        }
    }

    pub fn is_mac(&self, device_id: &str) -> bool {
        self.devices.lock().unwrap().macs.contains_key(device_id)
    }

    /// Account metadata only: knowing a Mac ID grants no device or content access.
    pub async fn account_owner(&self, headers: &HeaderMap, now_ms: u64) -> Result<String> {
        let token = header(headers, "authorization")?
            .strip_prefix("Bearer ").context("expected Bearer token")?;
        self.verify_jwt(token, now_ms).await
    }

    /// Each enrolled Mac of the owner with the iroh endpoint it bound, if it bound one.
    pub fn macs_for_owner(&self, owner_sub: &str) -> Result<Vec<(String, Option<String>)>> {
        self.ensure_registry_ready()?;
        let registry = self.devices.lock().unwrap();
        let mut macs: Vec<_> = registry.macs.values()
            .filter(|mac| mac.owner_sub == owner_sub && !registry.revoked_mac_ids.contains(&mac.mac_id))
            .map(|mac| (mac.mac_id.clone(), registry.iroh_nodes.get(&mac.mac_id).cloned())).collect();
        macs.sort();
        Ok(macs)
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
            bail!(RegistryUnavailable);
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn is_mac_registered(&self, mac_id: &str) -> bool {
        self.devices.lock().unwrap().macs.contains_key(mac_id)
    }

    pub async fn authenticate_mac(
        &self,
        headers: &HeaderMap,
        method: &Method,
        uri: &Uri,
        body: &[u8],
        now_ms: u64,
    ) -> Result<Identity> {
        self.ensure_registry_ready()?;
        let rejected = |reason| anyhow::Error::new(DeviceRejection::new(reason, headers, method, uri, None, None, None));
        let id = header(headers, "x-device-id").map_err(|_| rejected(Rejection::MalformedHeaders))?;
        let mac = self
            .devices
            .lock()
            .unwrap()
            .macs
            .get(id)
            .cloned()
            .ok_or_else(|| rejected(Rejection::UnknownDevice))?;
        self.verify_device(
            headers,
            method,
            uri,
            body,
            now_ms,
            &mac.mac_id,
            &mac.signing_key,
        )?;
        self.require_live_route(&mac.owner_sub,&mac.mac_id,None).await?;
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
        let mac=self.paired_mac_for_phone(id,&sub)?;
        self.require_live_route(&sub,&mac,Some(id)).await?;
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
        self.require_live_route(&sub,mac_id,Some(device_id)).await?;
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
        self.require_active_owner(&c.sub).await?;
        Ok(c.sub)
    }

    pub async fn require_live_route(&self,subject:&str,mac_id:&str,client_id:Option<&str>)->Result<()> {
        #[cfg(test)]
        self.live_route_checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(db)=&self.management {
            db.require_live_device(subject,mac_id,client_id).await.map_err(|error| {
                if error.downcast_ref::<crate::management::DomainError>()==Some(&crate::management::DomainError::Forbidden) {anyhow::anyhow!(AccountGateError::Denied)} else {
                    #[cfg(feature = "lab-waiter-metrics")]
                    crate::management::lab_metrics::record(&error);
                    anyhow::anyhow!(AccountGateError::Unavailable)
                }
            })?;
        }
        Ok(())
    }
    pub async fn require_live_phone(&self,who:&Identity)->Result<()> {
        let mac=self.paired_mac_for_phone(&who.device_id,&who.owner_sub)?;
        self.mac_for_owner(&mac,&who.owner_sub)?;
        self.require_live_route(&who.owner_sub,&mac,Some(&who.device_id)).await
    }
    pub async fn require_active_owner(&self,subject:&str)->Result<()> {
        if let Some(management)=&self.management {
            management.active_identity(&self.config.issuer,subject).await.map_err(|error| {
                if error.downcast_ref::<crate::management::DomainError>()==Some(&crate::management::DomainError::Forbidden) {
                    anyhow::anyhow!(AccountGateError::Denied)
                } else {
                    #[cfg(feature = "lab-waiter-metrics")]
                    crate::management::lab_metrics::record(&error);
                    anyhow::anyhow!(AccountGateError::Unavailable)
                }
            })?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn live_route_check_count(&self) -> usize {
        self.live_route_checks.load(std::sync::atomic::Ordering::Relaxed)
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
        let reject = |reason, skew_ms, detail_ms, nonce: Option<&[u8; 16]>| {
            anyhow::Error::new(DeviceRejection::new(reason, headers, method, uri, skew_ms, detail_ms, nonce))
        };
        let cf_ray = headers.get("cf-ray").and_then(|v| v.to_str().ok()).map(printable);
        let parsed = || -> Option<(u64, [u8; 16], &str)> {
            let timestamp_str = header(headers, "x-device-timestamp-ms").ok()?;
            let timestamp: u64 = timestamp_str.parse().ok()?;
            if timestamp.to_string() != timestamp_str {
                return None;
            }
            let nonce = URL_SAFE_NO_PAD.decode(header(headers, "x-device-nonce").ok()?).ok()?.try_into().ok()?;
            Some((timestamp, nonce, header(headers, "x-device-signature").ok()?))
        };
        if header(headers, "x-device-id").ok() != Some(id) {
            return Err(reject(Rejection::DeviceMismatch, None, None, None));
        }
        let Some((timestamp, nonce, signature)) = parsed() else {
            return Err(reject(Rejection::MalformedHeaders, None, None, None));
        };
        let current_epoch = URL_SAFE_NO_PAD.encode(self.server_epoch());
        if header(headers, "x-relay-epoch").ok() != Some(current_epoch.as_str()) {
            let mut rejection = DeviceRejection::new(Rejection::EpochMismatch, headers, method, uri, None, None, Some(&nonce));
            let signed = headers.get("x-relay-epoch").map_or(&[][..], |value| value.as_bytes());
            rejection.epoch_tags = Some((short_tag(signed), short_tag(current_epoch.as_bytes())));
            rejection.current_epoch = Some(current_epoch);
            return Err(anyhow::Error::new(rejection));
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
        let skew_ms = Some(timestamp as i64 - now_ms as i64);
        let high_water_ms = verifier.high_water_ms();
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
            .map_err(|error| {
                let (reason, detail_ms) = match error {
                    alondra_e2e::Error::ClockSkew => (Rejection::ClockSkew, None),
                    alondra_e2e::Error::ClockRollback => {
                        (Rejection::ClockRollback, high_water_ms.map(|high| high.saturating_sub(now_ms)))
                    }
                    alondra_e2e::Error::Replay => (
                        Rejection::Replay,
                        verifier.accepted_at_ms(id, &nonce).map(|first| now_ms.saturating_sub(first)),
                    ),
                    alondra_e2e::Error::Capacity => (Rejection::Capacity, None),
                    _ => (Rejection::BadSignature, None),
                };
                let mut rejection = DeviceRejection::new(reason, headers, method, uri, skew_ms, detail_ms, Some(&nonce));
                if reason == Rejection::Replay {
                    rejection.first_cf_ray = verifier.accepted_label(id, &nonce).map(str::to_owned);
                }
                anyhow::Error::new(rejection)
            })?;
        if let Some(ray) = cf_ray {
            verifier.label(id, &nonce, ray);
        }
        Ok(())
    }
}

/// Why a signed device request was refused. The response stays a plain 401; only the relay log
/// carries the reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    UnknownDevice,
    DeviceMismatch,
    MalformedHeaders,
    EpochMismatch,
    ClockSkew,
    ClockRollback,
    Replay,
    Capacity,
    BadSignature,
}

impl Rejection {
    fn code(self) -> &'static str {
        match self {
            Self::UnknownDevice => "unknown_device",
            Self::DeviceMismatch => "device_mismatch",
            Self::MalformedHeaders => "malformed_headers",
            Self::EpochMismatch => "epoch_mismatch",
            Self::ClockSkew => "clock_skew",
            Self::ClockRollback => "clock_rollback",
            Self::Replay => "replay",
            Self::Capacity => "nonce_capacity",
            Self::BadSignature => "bad_signature",
        }
    }
}

/// A refused device request with only the facts needed to tell its cause: never the signature,
/// token or nonce, only a short hash of the nonce so two copies of one request can be matched.
#[derive(Debug)]
pub struct DeviceRejection {
    pub reason: Rejection,
    device: String,
    method: String,
    path: String,
    skew_ms: Option<i64>,
    detail_ms: Option<u64>,
    nonce_tag: Option<String>,
    cf_ray: Option<String>,
    first_cf_ray: Option<String>,
    /// Set when the request signed an epoch from before the relay restarted.
    pub current_epoch: Option<String>,
    /// Short tags of the epoch the request signed and the current one, so a request signed for
    /// an older relay can be told from a copy of one request, without logging either epoch.
    epoch_tags: Option<(String, String)>,
}

impl DeviceRejection {
    fn new(
        reason: Rejection,
        headers: &HeaderMap,
        method: &Method,
        uri: &Uri,
        skew_ms: Option<i64>,
        detail_ms: Option<u64>,
        nonce: Option<&[u8; 16]>,
    ) -> Self {
        Self {
            reason,
            device: header(headers, "x-device-id").map(printable).unwrap_or_default(),
            method: method.as_str().to_owned(),
            path: printable(uri.path()),
            skew_ms,
            detail_ms,
            nonce_tag: nonce.map(|nonce| short_tag(nonce)),
            cf_ray: headers.get("cf-ray").and_then(|v| v.to_str().ok()).map(printable),
            first_cf_ray: None,
            current_epoch: None,
            epoch_tags: None,
        }
    }
}

impl std::fmt::Display for DeviceRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "device request refused: reason={} device={} {} {}", self.reason.code(), self.device, self.method, self.path)?;
        if let Some(skew) = self.skew_ms {
            write!(f, " skew_ms={skew}")?;
        }
        match (self.reason, self.detail_ms) {
            (Rejection::Replay, Some(age)) => write!(f, " first_accepted_ms_ago={age}")?,
            (Rejection::ClockRollback, Some(back)) => write!(f, " clock_back_ms={back}")?,
            _ => {}
        }
        if let Some(tag) = &self.nonce_tag {
            write!(f, " nonce_tag={tag}")?;
        }
        if let Some((signed, current)) = &self.epoch_tags {
            write!(f, " signed_epoch_tag={signed} current_epoch_tag={current}")?;
        }
        if let Some(ray) = &self.cf_ray {
            write!(f, " cf_ray={ray}")?;
        }
        if let Some(ray) = &self.first_cf_ray {
            write!(f, " first_cf_ray={ray}")?;
        }
        Ok(())
    }
}

impl std::error::Error for DeviceRejection {}

fn short_tag(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)[..4].iter().map(|b| format!("{b:02x}")).collect()
}

fn printable(value: &str) -> String {
    value.chars().filter(|c| c.is_ascii_graphic()).take(24).collect()
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

fn valid_session_part(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// An iroh endpoint ID is its Ed25519 public key in lowercase hex, as the iroh relay sends it.
fn iroh_node_key(node: &str) -> Option<[u8; 32]> {
    if node.len() != 64 || !node.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut key = [0u8; 32];
    for (byte, pair) in key.iter_mut().zip(node.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(key)
}

fn forget_orphan_iroh_nodes(registry: &mut DeviceRegistry) {
    let DeviceRegistry { iroh_nodes, devices, macs, .. } = registry;
    iroh_nodes.retain(|id, _| devices.contains_key(id) || macs.contains_key(id));
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
    let mut push_ended: Vec<_> = registry.push_ended.iter().cloned().collect();
    push_ended.sort();
    let push_sessions = registry
        .push_sessions
        .iter()
        .filter(|(id, _)| registry.push.contains_key(*id))
        .map(|(id, session)| (id.clone(), session.clone()))
        .collect();
    write_atomic(
        path,
        &serde_json::to_vec(&StoredDeviceRegistry {
            schema_version: Some(REGISTRY_SCHEMA_VERSION),
            devices,
            phone_macs: registry.phone_macs.clone(),
            revoked_ids,
            push: registry.push.clone(),
            push_sessions,
            push_ended,
            macs,
            revoked_mac_ids,
            iroh_nodes: registry.iroh_nodes.clone(),
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

#[derive(Debug)]
pub enum AccountGateError { Denied, Unavailable }
impl std::fmt::Display for AccountGateError {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str(match self {Self::Denied=>"account_access_denied",Self::Unavailable=>"account_access_unavailable"})}
}
impl std::error::Error for AccountGateError {}

/// Only explicit identity collisions are safe to report as HTTP conflict.
#[derive(Debug)]
pub enum EnrollmentError { Conflict, InvalidRegistration }
impl std::fmt::Display for EnrollmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {Self::Conflict=>"enrollment_conflict",Self::InvalidRegistration=>"invalid_registration"})
    }
}
impl std::error::Error for EnrollmentError {}
#[derive(Debug)]
pub struct RegistryUnavailable;
impl std::fmt::Display for RegistryUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {f.write_str("registry_unavailable")}
}
impl std::error::Error for RegistryUnavailable {}
