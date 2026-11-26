use alondra_core::clock;
use alondra_core::clock::Instant;
use crate::locks::Poisonable as _;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alondra_core::now_millis;
use alondra_e2e::HttpChallengeVerifier;
use anyhow::{Context, Result};
use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod observation;
use observation::{Metric, Observation, RequestObservation};
pub use observation::ObservationError;

const MAX_SKEW_MS: i64 = 120_000;
const PAIRING_TTL: Duration = Duration::from_secs(600);
const MAX_BODY: usize = 25 * 1024 * 1024;
const REPLAY_MEMORY: usize = 4096;

#[derive(Serialize, Deserialize, Clone)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub public_key: String,
    pub created_at: i64,
    #[serde(default)]
    pub last_seen: Option<i64>,
}

/// Why `approve_service` refused a device.
#[derive(Debug, PartialEq)]
pub enum ServiceApprovalError {
    /// This Mac revoked that device ID; pairing again needs a new device ID.
    Revoked,
    /// This Mac already holds that device ID with another key.
    OtherKey,
    Refused(String),
}

impl From<String> for ServiceApprovalError {
    fn from(reason: String) -> Self {
        Self::Refused(reason)
    }
}

impl From<&str> for ServiceApprovalError {
    fn from(reason: &str) -> Self {
        Self::Refused(reason.into())
    }
}

impl From<ServiceApprovalError> for String {
    fn from(error: ServiceApprovalError) -> Self {
        match error {
            ServiceApprovalError::Revoked => "device was revoked on this Mac; pair again with a new device ID".into(),
            ServiceApprovalError::OtherKey => "device ID already has another key".into(),
            ServiceApprovalError::Refused(reason) => reason,
        }
    }
}

#[derive(Default)]
struct RequestCryptoWork {
    decodes: std::sync::atomic::AtomicUsize,
    preparations: std::sync::atomic::AtomicUsize,
    verifications: std::sync::atomic::AtomicUsize,
}

impl RequestCryptoWork {
    fn counts(&self) -> (usize, usize, usize) {
        use std::sync::atomic::Ordering::Relaxed;
        (self.decodes.load(Relaxed), self.preparations.load(Relaxed), self.verifications.load(Relaxed))
    }
}

// An Arc also identifies one uninterrupted approval, even if the same key is paired again.
struct PreparedKey {
    encoded: String,
    key: std::sync::OnceLock<Result<VerifyingKey, &'static str>>,
}

#[derive(Default)]
struct PairedDevices {
    devices: Vec<Device>,
    keys: Vec<Arc<PreparedKey>>,
}

impl PairedDevices {
    fn with_devices(&self, devices: Vec<Device>) -> Self {
        let keys = devices.iter().map(|device| {
            self.devices.iter().zip(&self.keys)
                .find(|(old, _)| old.id == device.id && old.public_key == device.public_key)
                .map(|(_, key)| key.clone())
                .unwrap_or_else(|| Arc::new(PreparedKey {
                    encoded: device.public_key.clone(), key: std::sync::OnceLock::new(),
                }))
        }).collect();
        Self { devices, keys }
    }

    fn prepared(&self, id: &str) -> Option<(&Device, &Arc<PreparedKey>)> {
        self.devices.iter().zip(&self.keys).find(|(device, _)| device.id == id)
    }
}

impl std::ops::Deref for PairedDevices {
    type Target = [Device];
    fn deref(&self) -> &[Device] { &self.devices }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VerificationStage { Selected, Verified }

pub struct Auth {
    path: PathBuf,
    /// A holder that panics leaves it whole, so it is recovered: each writer saves the file first
    /// and then replaces the list whole, so memory always matches a saved file.
    devices: Mutex<PairedDevices>,
    /// A holder that panics leaves it whole, so it is recovered: a retain and an insert, or one
    /// remove.
    pairing: Mutex<HashMap<String, Instant>>,
    /// A holder that panics leaves it whole, so it is recovered: a push and a pop.
    seen: Mutex<VecDeque<String>>,
    /// A holder that panics leaves it whole, so it is recovered: it records a nonce only after the
    /// signature checks, so a torn change can only refuse a request.
    service_verifier: Mutex<HttpChallengeVerifier>,
    usage: crate::device_usage::DeviceUsage,
    observation: Observation,
    request_crypto: RequestCryptoWork,
    #[cfg(test)]
    verification_hook: Mutex<Option<Arc<dyn Fn(VerificationStage) + Send + Sync>>>,
}

pub use alondra_foundation::random_hex;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn signed_message(method: &str, path_and_query: &str, timestamp: &str, body: &[u8]) -> String {
    format!("{method}\n{path_and_query}\n{timestamp}\n{}", hex(&Sha256::digest(body)))
}

/// What tells one `devices.json` from another without reading it: a save renames a new file in,
/// and any write changes its size or times.
#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIdentity { device: u64, inode: u64, size: u64, modified: (i64, i64), changed: (i64, i64) }

impl FileIdentity {
    fn of(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self { device: metadata.dev(), inode: metadata.ino(), size: metadata.size(),
            modified: (metadata.mtime(), metadata.mtime_nsec()), changed: (metadata.ctime(), metadata.ctime_nsec()) }
    }
}

static APPROVED: Mutex<std::collections::BTreeMap<PathBuf, (FileIdentity, Arc<Vec<Device>>)>> = Mutex::new(std::collections::BTreeMap::new());

/// The devices approved in `home`'s `devices.json`, as it is now: read again only when the file
/// changed, so a request checks its device with one `stat`, and a revocation counts from the
/// moment the file changes, whoever wrote it.
pub fn approved_devices(home: &std::path::Path) -> Result<Arc<Vec<Device>>> {
    use std::io::Read as _;
    let path = home.join("devices.json");
    let mut file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Arc::default()),
        Err(error) => return Err(error.into()),
    };
    let identity = FileIdentity::of(&file.metadata()?);
    if let Some((known, devices)) = APPROVED.lock_or_recover("the approved devices").get(&path)
        && *known == identity
    {
        return Ok(devices.clone());
    }
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    let devices: Arc<Vec<Device>> = Arc::new(serde_json::from_str(&text).context("devices.json ilegible")?);
    APPROVED.lock_or_recover("the approved devices").insert(path, (identity, devices.clone()));
    Ok(devices)
}

impl Auth {
    pub fn start_observation(&self, seconds: u64, delay_seconds: u64) -> Result<(), ObservationError> {
        self.observation.start_window(seconds, delay_seconds)
    }
    pub fn observation_running(&self) -> bool { self.observation.running() }
    /// Counts actual key decoding, key preparation, and ECDSA verification attempts, in that order.
    pub fn request_crypto_counts_for_test(&self) -> (usize, usize, usize) {
        self.request_crypto.counts()
    }

    pub fn verify_request_for_test(&self, headers: &HeaderMap, method: &str, path_and_query: &str,
        body: &[u8], local: bool) -> Result<Signed, &'static str> {
        self.verify(headers, method, path_and_query, body, local)
    }

    #[cfg(test)]
    fn at_verification_stage(&self, stage: VerificationStage) {
        let hook = self.verification_hook.lock().unwrap().clone();
        if let Some(hook) = hook { hook(stage); }
    }

    fn prepare_request_key(&self, encoded: &str, observed: &RequestObservation) -> Result<VerifyingKey, &'static str> {
        self.request_crypto.decodes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        observed.add(Metric::KeyDecodes);
        let raw = B64.decode(encoded).map_err(|_| "stored key is corrupt")?;
        self.request_crypto.preparations.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        observed.add(Metric::KeyPreparations);
        VerifyingKey::from_sec1_bytes(&raw).map_err(|_| "stored key is corrupt")
    }

    fn persisted(&self) -> Result<Vec<Device>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text).context("devices.json ilegible"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
            Err(error) => Err(error.into()),
        }
    }
    pub fn open(home: &std::path::Path) -> Result<Self> {
        let path = home.join("devices.json");
        let devices = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).context("devices.json ilegible")?,
            Err(_) => vec![],
        };
        Ok(Auth {
            path,
            devices: Mutex::new(PairedDevices::default().with_devices(devices)),
            pairing: Mutex::new(HashMap::new()),
            seen: Mutex::new(VecDeque::new()),
            service_verifier: Mutex::new(HttpChallengeVerifier::new(120_000, 4096)?),
            usage: crate::device_usage::DeviceUsage::at(home),
            observation: observation::configured(),
            request_crypto: RequestCryptoWork::default(),
            #[cfg(test)]
            verification_hook: Mutex::default(),
        })
    }

    fn save(&self, devices: &[Device]) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(devices)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn new_pairing_code(&self) -> String {
        let code = random_hex(10);
        let mut p = self.pairing.lock_or_recover("the pairing codes");
        p.retain(|_, t| alondra_core::clock::since(*t) < PAIRING_TTL);
        p.insert(code.clone(), clock::instant());
        code
    }

    pub fn pair(&self, code: &str, name: &str, public_key_b64: &str) -> Result<Device, String> {
        let valid = {
            let mut p = self.pairing.lock_or_recover("the pairing codes");
            p.remove(code).is_some_and(|t| alondra_core::clock::since(t) < PAIRING_TTL)
        };
        if !valid {
            return Err("pairing code expired or unknown".into());
        }
        let raw = B64.decode(public_key_b64).map_err(|_| "public key badly encoded".to_string())?;
        VerifyingKey::from_sec1_bytes(&raw).map_err(|_| "invalid public key".to_string())?;
        let device = Device {
            id: random_hex(8),
            name: name.chars().take(80).collect(),
            public_key: public_key_b64.to_string(),
            created_at: now_millis(),
            last_seen: None,
        };
        let _lease = crate::approval_lease::ApprovalLease::exclusive(self.path.parent().ok_or("approval home missing")?)
            .map_err(|error| error.to_string())?;
        let mut devices = self.devices.lock_or_recover("the paired devices");
        let mut next = self.persisted().map_err(|error| error.to_string())?;
        next.push(device.clone());
        let prepared = devices.with_devices(next);
        self.save(&prepared.devices).map_err(|e| e.to_string())?;
        *devices = prepared;
        Ok(device)
    }

    /// The service CLI calls this only after a human accepts the encrypted candidate on
    /// this Mac. Reuse the daemon's registry lock, avoiding concurrent file rewrites.
    /// A device ID this Mac revoked is refused for good, so a stale confirmation cannot bring it back.
    pub fn approve_service(&self, id: &str, name: &str, signing_key: &str) -> Result<(), ServiceApprovalError> {
        if !(id.starts_with("phone-") || id.starts_with("client-")) || id.len() > 128
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || name.len() > 80 || name.chars().any(char::is_control)
        { return Err("invalid service device".into()); }
        let public_key = service_public_key(signing_key)?;
        let home = self.path.parent().ok_or("approval home missing")?;
        let _lease = crate::approval_lease::ApprovalLease::exclusive(home)
            .map_err(|error| error.to_string())?;
        if crate::revoked_devices::RevokedDevices::open(home).signing_key(id).is_some() {
            return Err(ServiceApprovalError::Revoked);
        }
        let mut devices = self.devices.lock_or_recover("the paired devices");
        let mut next = self.persisted().map_err(|error| error.to_string())?;
        if let Some(existing) = next.iter().find(|d| d.id == id) {
            let same_key = existing.public_key == public_key;
            *devices = devices.with_devices(next);
            return if same_key { Ok(()) } else { Err(ServiceApprovalError::OtherKey) };
        }
        next.push(Device { id: id.into(), name: name.into(), public_key,
            created_at: now_millis(), last_seen: None });
        let prepared = devices.with_devices(next);
        self.save(&prepared.devices).map_err(|e| e.to_string())?;
        *devices = prepared;
        Ok(())
    }

    /// What [`Auth::approve_service`] would answer for this device, read without the lease and
    /// without writing: a request it would refuse never has to reach the user.
    pub fn service_admits(&self, id: &str, signing_key: &str) -> Result<(), ServiceApprovalError> {
        let public_key = service_public_key(signing_key)?;
        let home = self.path.parent().ok_or("approval home missing")?;
        if crate::revoked_devices::RevokedDevices::open(home).signing_key(id).is_some() {
            return Err(ServiceApprovalError::Revoked);
        }
        let persisted = self.persisted().map_err(|error| error.to_string())?;
        match persisted.iter().find(|d| d.id == id) {
            Some(existing) if existing.public_key != public_key => Err(ServiceApprovalError::OtherKey),
            _ => Ok(()),
        }
    }

    pub fn service_epoch(&self) -> [u8; 16] {
        self.service_verifier.lock_or_recover("the service request verifier").server_epoch()
    }

    pub fn verify_service_approval(&self, headers: &HeaderMap, body: &[u8]) -> Result<(), String> {
        #[derive(Deserialize)]
        struct PublicMac { mac_id: String, signing_key: String }
        let home = self.path.parent().ok_or("missing Alondra home")?;
        let public: PublicMac = serde_json::from_slice(&std::fs::read(alondra_core::home_lock::public_identity_path(home).map_err(|_| "service identity path invalid")?)
            .map_err(|_| "service public identity unavailable")?).map_err(|_| "service public identity invalid")?;
        let wire = URL_SAFE_NO_PAD.decode(&public.signing_key).map_err(|_| "service public key invalid")?;
        let key = VerifyingKey::from_sec1_bytes(&wire).map_err(|_| "service public key invalid")?;
        let h = |name| headers.get(name).and_then(|v| v.to_str().ok()).ok_or("missing signed service header");
        if h("x-device-id")? != public.mac_id { return Err("wrong Mac identity".into()); }
        let epoch = h("x-relay-epoch")?;
        if epoch != URL_SAFE_NO_PAD.encode(self.service_epoch()) { return Err("wrong service epoch".into()); }
        let ts: u64 = h("x-device-timestamp-ms")?.parse().map_err(|_| "invalid service timestamp")?;
        let nonce: [u8; 16] = URL_SAFE_NO_PAD.decode(h("x-device-nonce")?)
            .map_err(|_| "invalid service nonce")?.try_into().map_err(|_| "invalid service nonce")?;
        self.service_verifier.lock_or_recover("the service request verifier")
            .verify(&key, h("x-device-signature")?, &public.mac_id, "POST",
                "/service/approve-device", body, ts, &nonce, now_millis() as u64)
            .map_err(|_| "invalid or replayed service signature".into())
    }

    pub fn usage(&self) -> &crate::device_usage::DeviceUsage {
        &self.usage
    }

    pub fn list(&self) -> Vec<Device> {
        self.devices.lock_or_recover("the paired devices").to_vec()
    }

    pub fn revoke(&self, id: &str) -> Result<bool> {
        let _lease = crate::approval_lease::ApprovalLease::exclusive(self.path.parent().context("approval home missing")?)?;
        let mut devices = self.devices.lock_or_recover("the paired devices");
        let mut next = self.persisted()?;
        let previous_len = next.len();
        next.retain(|d| d.id != id);
        let changed = next.len() != previous_len;
        let prepared = devices.with_devices(next);
        if changed {
            self.save(&prepared.devices)?;
        }
        *devices = prepared;
        Ok(changed)
    }

    /// Service enrollment and work may use only a locally paired device with the same key.
    pub fn approved_key_matches(&self, id: &str, public_key: &VerifyingKey) -> bool {
        self.devices.lock_or_recover("the paired devices").iter().any(|device| {
            device.id == id && B64.decode(&device.public_key).ok()
                .is_some_and(|raw| raw.as_slice() == public_key.to_sec1_bytes().as_ref())
        })
    }

    fn verify(&self, headers: &HeaderMap, method: &str, path_and_query: &str, body: &[u8], local: bool) -> Result<Signed, &'static str> {
        self.verify_at(headers, method, path_and_query, body, now_millis(), local)
    }

    fn verify_at(&self, headers: &HeaderMap, method: &str, path_and_query: &str,
        body: &[u8], now: i64, local: bool) -> Result<Signed, &'static str> {
        let observed = self.observation.request(headers, path_and_query, local);
        observed.add(Metric::Attempts);
        let result = (|| {
            let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok());
            let device_id = h("x-alondra-device").ok_or("missing X-Alondra-Device")?;
            let timestamp = h("x-alondra-timestamp").ok_or("missing X-Alondra-Timestamp")?;
            let signature_b64 = h("x-alondra-signature").ok_or("missing X-Alondra-Signature")?;
            let ts: i64 = timestamp.parse().map_err(|_| "invalid timestamp")?;
            if (now - ts).abs() > MAX_SKEW_MS {
                return Err("timestamp out of range");
            }
            let prepared = {
                let devices = self.devices.lock_or_recover("the paired devices");
                devices.prepared(device_id).map(|(_, key)| key.clone())
            }
            .ok_or("device not paired")?;
            #[cfg(test)]
            self.at_verification_stage(VerificationStage::Selected);
            let key = prepared.key.get_or_init(|| self.prepare_request_key(&prepared.encoded, &observed))
                .as_ref().map_err(|reason| *reason)?;
            let der = B64.decode(signature_b64).map_err(|_| "signature badly encoded")?;
            let signature = Signature::from_der(&der).map_err(|_| "malformed signature")?;
            let message = signed_message(method, path_and_query, timestamp, body);
            self.request_crypto.verifications.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            observed.add(Metric::Ecdsa);
            key.verify(message.as_bytes(), &signature).map_err(|_| "wrong signature")?;
            #[cfg(test)]
            self.at_verification_stage(VerificationStage::Verified);
            let devices = self.devices.lock_or_recover("the paired devices");
            let (device, current) = devices.prepared(device_id).ok_or("device not paired")?;
            if !Arc::ptr_eq(current, &prepared) { return Err("device approval changed"); }
            let canonical_signature = B64.encode(signature.normalize_s().to_der().as_bytes());
            let mut seen = self.seen.lock_or_recover("the seen requests");
            if seen.iter().any(|s| s == &canonical_signature) {
                return Err("replayed request");
            }
            seen.push_back(canonical_signature);
            if seen.len() > REPLAY_MEMORY {
                seen.pop_front();
            }
            drop(seen);
            self.usage.touch(&device.id, now_millis(), local);
            Ok(Signed { device_id: device.id.clone(), name: device.name.clone(), public_key: prepared.encoded.clone() })
        })();
        // Verified is this cryptographic/replay check, not the handler's durable authorization.
        observed.add(if result.is_ok() { Metric::Verified } else { Metric::Rejected });
        result
    }
}

#[derive(Clone, Debug)]
pub struct Signed {
    /// Verified device identity; display names are not command receipt scopes.
    pub device_id: String,
    pub name: String,
    /// Exact key verified by middleware; handler compares it with persisted approval.
    pub public_key: String,
}

fn service_public_key(signing_key: &str) -> Result<String, ServiceApprovalError> {
    let raw = URL_SAFE_NO_PAD.decode(signing_key).map_err(|_| "invalid service signing key".to_string())?;
    let key = VerifyingKey::from_sec1_bytes(&raw).map_err(|_| "invalid service signing key".to_string())?;
    if raw.len() != 65 || raw[0] != 4 || URL_SAFE_NO_PAD.encode(&raw) != signing_key {
        return Err("noncanonical service signing key".to_string().into());
    }
    Ok(B64.encode(key.to_sec1_bytes()))
}

pub fn is_local(peer: &SocketAddr, headers: &HeaderMap) -> bool {
    let proxied = headers.contains_key("cf-ray") || headers.contains_key("cf-connecting-ip") || headers.contains_key("x-forwarded-for");
    peer.ip().is_loopback() && !proxied
}

fn unauthorized(reason: &str) -> Response {
    (StatusCode::UNAUTHORIZED, axum::Json(serde_json::json!({"error": reason}))).into_response()
}

pub async fn guard(State(auth): State<Arc<Auth>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, req: Request<Body>, next: Next) -> Response {
    let presents_signature = req.headers().contains_key("x-alondra-signature");
    let local = is_local(&peer, req.headers());
    let observed = auth.observation.request(req.headers(), req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/"), local);
    observed.add(Metric::Guard);
    if !presents_signature { observed.add(Metric::SignatureAbsent); }
    if local && !presents_signature {
        observed.add(Metric::LocalBypass);
        return next.run(req).await;
    }
    let (parts, body) = req.into_parts();
    let Ok(bytes) = to_bytes(body, MAX_BODY).await else {
        observed.add(Metric::BodyRejected);
        return unauthorized("cuerpo demasiado grande");
    };
    let path_and_query = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let signed = match auth.verify(&parts.headers, parts.method.as_str(), path_and_query, &bytes, local) {
        Ok(signed) => signed,
        Err(reason) => return unauthorized(reason),
    };
    let mut req = Request::from_parts(parts, Body::from(bytes));
    req.extensions_mut().insert(signed);
    next.run(req).await
}

#[cfg(test)]
#[cfg(not(feature = "real-hosts"))]
mod tests {
    use super::*;
    use p256::ecdsa::SigningKey;
    use p256::ecdsa::signature::Signer;

    fn auth_with_device() -> (crate::test_support::Owned<Auth>, SigningKey, String) {
        auth_with_observation(Observation::default())
    }

    fn auth_with_observation(observation: Observation) -> (crate::test_support::Owned<Auth>, SigningKey, String) {
        let dir = crate::test_support::Directory::new();
        std::fs::create_dir_all(&dir).unwrap();
        let mut auth = Auth::open(&dir).unwrap();
        auth.observation = observation;
        let signing = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let public = B64.encode(signing.verifying_key().to_sec1_bytes());
        let code = auth.new_pairing_code();
        let device = auth.pair(&code, "iPhone", &public).unwrap();
        (crate::test_support::Owned::new(auth, dir), signing, device.id)
    }

    fn headers(device: &str, ts: i64, sig: &Signature) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-alondra-device", device.parse().unwrap());
        h.insert("x-alondra-timestamp", ts.to_string().parse().unwrap());
        h.insert("x-alondra-signature", B64.encode(sig.to_der().as_bytes()).parse().unwrap());
        h
    }

    fn signed_headers(device: &str, key: &SigningKey, path: &str, at: i64) -> HeaderMap {
        let signature: Signature = key.sign(signed_message("GET", path, &at.to_string(), b"").as_bytes());
        headers(device, at, &signature)
    }

    #[test]
    fn an_open_auth_can_start_and_rearm_observation_without_reopening_devices() {
        let (auth, key, id) = auth_with_device();
        let verify = |request: usize| {
            let at = 1_000_000;
            let path = format!("/views/home?client=tui&request={request}");
            auth.verify_at(&signed_headers(&id, &key, &path, at), "GET", &path, b"", at, true).unwrap();
        };
        verify(0);
        assert_eq!(auth.observation.total(Metric::Ecdsa), 0);
        alondra_core::clock::VirtualRuntime::new(0).unwrap().block_on(async {
            auth.start_observation(10, 0).unwrap();
            assert!(auth.observation_running());
            alondra_core::clock::sleep(std::time::Duration::from_secs(1)).await;
            verify(1);
            verify(2);
            assert_eq!(auth.observation.total(Metric::Ecdsa), 2);
            assert_eq!(auth.observation.total(Metric::KeyPreparations), 0, "the already prepared device key is reused");
            alondra_core::clock::sleep(std::time::Duration::from_secs(11)).await;
            assert!(!auth.observation_running());
            verify(3);
            assert_eq!(auth.observation.total(Metric::Ecdsa), 2);
            auth.start_observation(10, 0).unwrap();
            alondra_core::clock::sleep(std::time::Duration::from_secs(1)).await;
            verify(4);
            assert_eq!(auth.observation.total(Metric::Ecdsa), 1);
            assert_eq!(auth.observation.total(Metric::Verified), 1);
            assert_eq!(auth.request_crypto.counts(), (1, 1, 5));
        });
    }

    #[test]
    fn request_observation_counts_real_crypto_and_every_verification_outcome() {
        let observation = Observation::armed();
        observation.start();
        let (auth, key, id) = auth_with_observation(observation.clone());
        let at = 1_000_000;
        for request in 0..4 {
            let path = format!("/views/home?client=tui&request={request}");
            auth.verify_at(&signed_headers(&id, &key, &path, at), "GET", &path, b"", at, true).unwrap();
        }
        let path = "/changes?client=ios";
        let headers = signed_headers(&id, &key, path, at);
        assert_eq!(auth.verify_at(&headers, "GET", path, b"tampered", at, false).unwrap_err(), "wrong signature");
        auth.verify_at(&headers, "GET", path, b"", at, false).unwrap();
        assert_eq!(auth.verify_at(&headers, "GET", path, b"", at, false).unwrap_err(), "replayed request");
        assert_eq!(auth.verify_at(&headers, "GET", path, b"", at + MAX_SKEW_MS + 1, false).unwrap_err(), "timestamp out of range");
        let mut malformed = headers.clone();
        malformed.insert("x-alondra-signature", "not-base64".parse().unwrap());
        assert_eq!(auth.verify_at(&malformed, "GET", path, b"", at, false).unwrap_err(), "signature badly encoded");
        assert_eq!(observation.total(Metric::Attempts), 9);
        assert_eq!(observation.total(Metric::KeyDecodes), 1);
        assert_eq!(observation.total(Metric::KeyPreparations), 1);
        assert_eq!(observation.total(Metric::Ecdsa), 7);
        assert_eq!(observation.total(Metric::Verified), 5);
        assert_eq!(observation.total(Metric::Rejected), 4);
        assert_eq!(observation.total(Metric::Guard), 0);
        assert_eq!(auth.request_crypto.counts(), (1, 1, 7));
    }

    #[tokio::test]
    async fn guard_observation_counts_unsigned_rejected_and_real_signed_requests() {
        let directory = Arc::new(crate::test_support::Directory::new());
        std::fs::create_dir_all(&**directory).unwrap();
        let observation = Observation::armed();
        observation.start();
        let mut auth = Auth::open(&directory).unwrap();
        auth.observation = observation.clone();
        let key = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let device = auth.pair(&auth.new_pairing_code(), "Synthetic device", &B64.encode(key.verifying_key().to_sec1_bytes())).unwrap();
        let auth = Arc::new(auth);
        let router = axum::Router::new()
            .route("/views/home", axum::routing::get(|request: Request<Body>| async move {
                if request.extensions().get::<Signed>().is_some() { "signed" } else { "unsigned" }
            }))
            .layer(axum::middleware::from_fn_with_state(auth.clone(), guard));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_directory = directory.clone();
        let server = tokio::spawn(async move {
            let _directory = server_directory;
            axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let path = "/views/home?client=mac";
        let url = format!("http://{address}{path}");
        assert_eq!(client.get(&url).send().await.unwrap().text().await.unwrap(), "unsigned");
        assert_eq!(client.get(&url).header("x-forwarded-for", "203.0.113.7").send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        let signed = signed_headers(&device.id, &key, path, now_millis());
        assert_eq!(client.get(&url).headers(signed).send().await.unwrap().text().await.unwrap(), "signed");
        assert_eq!(observation.total(Metric::Guard), 3);
        assert_eq!(observation.total(Metric::SignatureAbsent), 2);
        assert_eq!(observation.total(Metric::LocalBypass), 1);
        assert_eq!(observation.total(Metric::BodyRejected), 0);
        assert_eq!(observation.total(Metric::Attempts), 2);
        assert_eq!(observation.total(Metric::Verified), 1);
        assert_eq!(observation.total(Metric::Rejected), 1);
        assert_eq!(observation.total(Metric::KeyDecodes), 1);
        assert_eq!(observation.total(Metric::KeyPreparations), 1);
        assert_eq!(observation.total(Metric::Ecdsa), 1);
        server.abort();
        let _ = server.await;
        drop(auth);
        drop(directory);
    }

    #[test]
    fn unchanged_device_prepares_once_but_verifies_every_request() {
        let (auth, key, id) = auth_with_device();
        let at = 1_000_000;
        for request in 0..4 {
            let path = format!("/views/home?request={request}");
            auth.verify_at(&signed_headers(&id, &key, &path, at), "GET", &path, b"", at, false).unwrap();
        }
        assert_eq!(auth.request_crypto.counts(), (1, 1, 4));
        for request in 4..8 {
            let path = format!("/views/home?request={request}");
            auth.verify_at(&signed_headers(&id, &key, &path, at), "GET", &path, b"", at, false).unwrap();
        }
        assert_eq!(auth.request_crypto.counts(), (1, 1, 8));
    }

    #[test]
    fn two_devices_with_the_same_key_keep_separate_preparations() {
        let (auth, key, first) = auth_with_device();
        let second = auth.pair(&auth.new_pairing_code(), "Second device",
            &B64.encode(key.verifying_key().to_sec1_bytes())).unwrap().id;
        assert_ne!(first, second);
        for round in 0..2 {
            for (index, id) in [&first, &second].into_iter().enumerate() {
                let path = format!("/views/home?device={index}&round={round}");
                let signed = auth.verify_at(&signed_headers(id, &key, &path, 1_000_000), "GET", &path, b"", 1_000_000, false).unwrap();
                assert_eq!(&signed.device_id, id);
            }
        }
        assert_eq!(auth.request_crypto.counts(), (2, 2, 4));
        let mut replay = signed_headers(&first, &key, "/views/home?device=0&round=0", 1_000_000);
        replay.insert("x-alondra-device", second.parse().unwrap());
        assert_eq!(auth.verify_at(&replay, "GET", "/views/home?device=0&round=0", b"", 1_000_000, false).unwrap_err(), "replayed request");
        assert_eq!(auth.request_crypto.counts(), (2, 2, 5));
    }

    #[test]
    fn concurrent_requests_share_preparation_but_each_verify() {
        let (auth, key, id) = auth_with_device();
        let start = std::sync::Barrier::new(2);
        let paths = ["/views/home?request=first", "/views/home?request=second"];
        let requests = paths.map(|path| signed_headers(&id, &key, path, 1_000_000));
        std::thread::scope(|scope| {
            let readers: Vec<_> = paths.iter().zip(&requests).map(|(path, headers)| {
                let auth = &auth;
                let start = &start;
                scope.spawn(move || {
                    start.wait();
                    auth.verify_at(headers, "GET", path, b"", 1_000_000, false).unwrap()
                })
            }).collect();
            for reader in readers { assert_eq!(reader.join().unwrap().device_id, id); }
        });
        assert_eq!(auth.request_crypto.counts(), (1, 1, 2));
    }

    fn service_auth_with_device() -> (crate::test_support::Owned<Auth>, SigningKey, String) {
        let dir = crate::test_support::Directory::new();
        std::fs::create_dir_all(&dir).unwrap();
        let auth = Auth::open(&dir).unwrap();
        let key = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let id = "client-test".to_owned();
        auth.approve_service(&id, "Synthetic device", &URL_SAFE_NO_PAD.encode(key.verifying_key().to_sec1_bytes())).unwrap();
        (crate::test_support::Owned::new(auth, dir), key, id)
    }

    fn replace_persisted_key(auth: &Auth, id: &str, key: &SigningKey) {
        let mut devices = auth.persisted().unwrap();
        devices.iter_mut().find(|device| device.id == id).unwrap().public_key = B64.encode(key.verifying_key().to_sec1_bytes());
        auth.save(&devices).unwrap();
        auth.approve_service(id, "Synthetic device", &URL_SAFE_NO_PAD.encode(key.verifying_key().to_sec1_bytes())).unwrap();
    }

    #[test]
    fn devices_and_replaced_keys_have_separate_preparations() {
        let (auth, first, id) = service_auth_with_device();
        let second = SigningKey::from_slice(&[8u8; 32]).unwrap();
        auth.approve_service("client-second", "Second device", &URL_SAFE_NO_PAD.encode(second.verifying_key().to_sec1_bytes())).unwrap();
        let path = "/views/home";
        for (device, key) in [(id.as_str(), &first), ("client-second", &second)] {
            auth.verify_at(&signed_headers(device, key, path, 1_000_000), "GET", path, b"", 1_000_000, false).unwrap();
        }
        assert_eq!(auth.request_crypto.counts(), (2, 2, 2));
        replace_persisted_key(&auth, &id, &second);
        let path = "/views/tasks";
        assert_eq!(auth.verify_at(&signed_headers(&id, &first, path, 1_000_000), "GET", path, b"", 1_000_000, false).unwrap_err(), "wrong signature");
        auth.verify_at(&signed_headers(&id, &second, path, 1_000_000), "GET", path, b"", 1_000_000, false).unwrap();
        assert_eq!(auth.request_crypto.counts(), (3, 3, 4));
    }

    #[test]
    fn a_request_cannot_cross_revocation_reapproval_or_key_replacement() {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Change { Revoke, Reapprove, Replace }
        for (stage, change) in [VerificationStage::Selected, VerificationStage::Verified].into_iter()
            .flat_map(|stage| [Change::Revoke, Change::Reapprove, Change::Replace].map(|change| (stage, change)))
        {
            let (auth, key, id) = service_auth_with_device();
            let prepared = Arc::new(std::sync::Barrier::new(2));
            let resume = Arc::new(std::sync::Barrier::new(2));
            let (entered, released) = (prepared.clone(), resume.clone());
            *auth.verification_hook.lock().unwrap() = Some(Arc::new(move |at| {
                if at == stage {
                    entered.wait();
                    released.wait();
                }
            }));
            let headers = signed_headers(&id, &key, "/views/home", 1_000_000);
            std::thread::scope(|scope| {
                let reader = scope.spawn(|| auth.verify_at(&headers, "GET", "/views/home", b"", 1_000_000, false));
                prepared.wait();
                match change {
                    Change::Revoke => { assert!(auth.revoke(&id).unwrap()); }
                    Change::Reapprove => {
                        assert!(auth.revoke(&id).unwrap());
                        auth.approve_service(&id, "New approval", &URL_SAFE_NO_PAD.encode(key.verifying_key().to_sec1_bytes())).unwrap();
                    }
                    Change::Replace => replace_persisted_key(&auth, &id, &SigningKey::from_slice(&[8u8; 32]).unwrap()),
                }
                resume.wait();
                let error = reader.join().unwrap().unwrap_err();
                assert_eq!(error, if change == Change::Revoke { "device not paired" } else { "device approval changed" });
            });
            *auth.verification_hook.lock().unwrap() = None;
            assert_eq!(auth.request_crypto.counts(), (1, 1, 1));
            assert!(auth.seen.lock().unwrap().is_empty(), "an invalidated approval must not consume replay state");
            if change == Change::Reapprove {
                auth.verify_at(&headers, "GET", "/views/home", b"", 1_000_000, false).unwrap();
                assert_eq!(auth.request_crypto.counts(), (2, 2, 2));
            }
        }
    }

    #[test]
    fn failed_persistence_keeps_the_prepared_approval() {
        let (auth, key, id) = auth_with_device();
        auth.verify_at(&signed_headers(&id, &key, "/views/home", 1_000_000), "GET", "/views/home", b"", 1_000_000, false).unwrap();
        std::fs::create_dir(auth.path.with_extension("json.tmp")).unwrap();
        assert!(auth.revoke(&id).is_err());
        auth.verify_at(&signed_headers(&id, &key, "/views/tasks", 1_000_000), "GET", "/views/tasks", b"", 1_000_000, false).unwrap();
        assert_eq!(auth.request_crypto.counts(), (1, 1, 2));
    }

    #[test]
    fn prepared_keys_keep_expiry_replay_and_invalid_signature_checks() {
        let (auth, key, id) = auth_with_device();
        let at = 1_000_000;
        let path = "/views/home";
        let headers = signed_headers(&id, &key, path, at);
        assert_eq!(auth.verify_at(&headers, "GET", path, b"tampered", at, false).unwrap_err(), "wrong signature");
        auth.verify_at(&headers, "GET", path, b"", at, false).unwrap();
        assert_eq!(auth.verify_at(&headers, "GET", path, b"", at, false).unwrap_err(), "replayed request");
        assert_eq!(auth.request_crypto.counts(), (1, 1, 3));
        assert_eq!(auth.verify_at(&headers, "GET", path, b"", at + MAX_SKEW_MS + 1, false).unwrap_err(), "timestamp out of range");
        assert_eq!(auth.request_crypto.counts(), (1, 1, 3));
    }

    #[test]
    fn refreshed_name_keeps_preparation_and_returns_current_metadata() {
        let (auth, key, id) = service_auth_with_device();
        auth.verify_at(&signed_headers(&id, &key, "/views/home", 1_000_000), "GET", "/views/home", b"", 1_000_000, false).unwrap();
        let mut devices = auth.persisted().unwrap();
        devices[0].name = "Renamed device".into();
        auth.save(&devices).unwrap();
        auth.approve_service(&id, "Ignored existing name", &URL_SAFE_NO_PAD.encode(key.verifying_key().to_sec1_bytes())).unwrap();
        let signed = auth.verify_at(&signed_headers(&id, &key, "/views/tasks", 1_000_000), "GET", "/views/tasks", b"", 1_000_000, false).unwrap();
        assert_eq!(signed.name, "Renamed device");
        assert_eq!(auth.request_crypto.counts(), (1, 1, 2));
    }

    #[test]
    fn corrupt_key_does_not_poison_a_later_valid_approval() {
        let (auth, key, id) = service_auth_with_device();
        let mut devices = auth.persisted().unwrap();
        devices[0].public_key = B64.encode([0u8; 65]);
        auth.save(&devices).unwrap();
        let reloaded = Auth::open(auth.path.parent().unwrap()).unwrap();
        let headers = signed_headers(&id, &key, "/views/home", 1_000_000);
        for _ in 0..2 {
            assert_eq!(reloaded.verify_at(&headers, "GET", "/views/home", b"", 1_000_000, false).unwrap_err(), "stored key is corrupt");
        }
        assert_eq!(reloaded.request_crypto.counts(), (1, 1, 0));
        replace_persisted_key(&reloaded, &id, &key);
        reloaded.verify_at(&headers, "GET", "/views/home", b"", 1_000_000, false).unwrap();
        assert_eq!(reloaded.request_crypto.counts(), (2, 2, 1));
    }

    #[test]
    fn devices_are_listed_and_paired_after_a_panic_holding_them() {
        let (auth, _key, id) = auth_with_device();
        crate::locks::poison(&auth.devices);
        assert_eq!(auth.list().iter().map(|device| device.id.clone()).collect::<Vec<_>>(), [id]);
        let code = auth.new_pairing_code();
        let public = B64.encode(SigningKey::from_slice(&[9u8; 32]).unwrap().verifying_key().to_sec1_bytes());
        auth.pair(&code, "iPad", &public).unwrap();
        assert_eq!(auth.list().len(), 2);
    }

    #[test]
    fn valid_signature_passes_and_replay_is_rejected() {
        let (auth, key, id) = auth_with_device();
        let ts = now_millis();
        let msg = signed_message("POST", "/v1/questions/1/answer", &ts.to_string(), b"{\"text\":\"yes\"}");
        let sig: Signature = key.sign(msg.as_bytes());
        let h = headers(&id, ts, &sig);
        let signed = auth.verify(&h, "POST", "/v1/questions/1/answer", b"{\"text\":\"yes\"}", false).unwrap();
        assert_eq!(signed.name, "iPhone");
        assert_eq!(signed.device_id, id);
        assert_eq!(auth.verify(&h, "POST", "/v1/questions/1/answer", b"{\"text\":\"yes\"}", false).unwrap_err(), "replayed request");
    }

    #[test]
    fn a_signed_request_records_the_use_and_whether_it_came_from_this_mac() {
        let (auth, key, id) = auth_with_device();
        let ts = now_millis();
        assert!(auth.usage().snapshot(ts).devices.get(&id).is_none());
        let msg = signed_message("GET", "/views/home", &ts.to_string(), b"");
        let sig: Signature = key.sign(msg.as_bytes());
        auth.verify(&headers(&id, ts, &sig), "GET", "/views/home", b"", true).unwrap();
        let used = &auth.usage().snapshot(ts).devices[&id];
        assert!(used.last_used_ms.is_some_and(|at| at >= ts));
        assert!(used.local);
    }

    #[test]
    fn an_equivalent_signature_cannot_replay_a_captured_request() {
        let (auth, key, id) = auth_with_device();
        let ts = 1000;
        let message = signed_message("GET", "/views/home", &ts.to_string(), b"");
        let signature: Signature = key.sign(message.as_bytes());
        let equivalent = Signature::from_scalars(signature.r().to_bytes(), (-*signature.s()).to_bytes()).unwrap();
        key.verifying_key().verify(message.as_bytes(), &equivalent).unwrap();
        auth.verify_at(&headers(&id, ts, &signature), "GET", "/views/home", b"", ts, false).unwrap();
        assert_eq!(auth.verify_at(&headers(&id, ts, &equivalent), "GET", "/views/home", b"", ts, false)
            .unwrap_err(), "replayed request");
    }

    #[test]
    fn tampered_body_or_path_fails() {
        let (auth, key, id) = auth_with_device();
        let ts = now_millis();
        let msg = signed_message("POST", "/v1/questions/1/answer", &ts.to_string(), b"{\"text\":\"yes\"}");
        let sig: Signature = key.sign(msg.as_bytes());
        let h = headers(&id, ts, &sig);
        assert_eq!(auth.verify(&h, "POST", "/v1/questions/1/answer", b"{\"text\":\"no\"}", false).unwrap_err(), "wrong signature");
        assert_eq!(auth.verify(&h, "POST", "/v1/questions/2/answer", b"{\"text\":\"yes\"}", false).unwrap_err(), "wrong signature");
    }

    #[test]
    fn old_timestamp_and_unknown_device_fail() {
        let (auth, key, id) = auth_with_device();
        let ts = now_millis() - 10 * 60_000;
        let msg = signed_message("GET", "/v1/overview", &ts.to_string(), b"");
        let sig: Signature = key.sign(msg.as_bytes());
        assert_eq!(auth.verify(&headers(&id, ts, &sig), "GET", "/v1/overview", b"", false).unwrap_err(), "timestamp out of range");
        let ts = now_millis();
        let msg = signed_message("GET", "/v1/overview", &ts.to_string(), b"");
        let sig: Signature = key.sign(msg.as_bytes());
        assert_eq!(auth.verify(&headers("other", ts, &sig), "GET", "/v1/overview", b"", false).unwrap_err(), "device not paired");
    }

    #[test]
    fn approved_devices_are_read_again_only_when_devices_json_changes() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        assert!(approved_devices(&home).unwrap().is_empty());
        let auth = Auth::open(&home).unwrap();
        let key = SigningKey::from_slice(&[9u8; 32]).unwrap();
        let public = B64.encode(key.verifying_key().to_sec1_bytes());
        let kept = auth.pair(&auth.new_pairing_code(), "kept", &public).unwrap();
        let revoked = auth.pair(&auth.new_pairing_code(), "revoked", &public).unwrap();
        let first = approved_devices(&home).unwrap();
        assert_eq!(first.iter().map(|device| device.id.as_str()).collect::<Vec<_>>(), [kept.id.as_str(), revoked.id.as_str()]);
        assert!(Arc::ptr_eq(&first, &approved_devices(&home).unwrap()), "an unchanged file is not read again");
        assert!(auth.revoke(&revoked.id).unwrap());
        assert!(approved_devices(&home).unwrap().iter().all(|device| device.id != revoked.id), "a revocation counts at once");
        std::fs::write(home.join("devices.json"), "[]").unwrap();
        assert!(approved_devices(&home).unwrap().is_empty(), "so does a write by another process, in place");
        std::fs::remove_file(home.join("devices.json")).unwrap();
        assert!(approved_devices(&home).unwrap().is_empty());
    }

    #[test]
    fn pairing_code_is_single_use() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(format!("alondra-auth-{}", random_hex(4)));
        std::fs::create_dir_all(&dir).unwrap();
        let auth = Auth::open(&dir).unwrap();
        let key = SigningKey::from_slice(&[9u8; 32]).unwrap();
        let public = B64.encode(key.verifying_key().to_sec1_bytes());
        let code = auth.new_pairing_code();
        assert!(auth.pair(&code, "a", &public).is_ok());
        assert!(auth.pair(&code, "b", &public).is_err());
    }

    #[test]
    fn service_approval_is_idempotent_for_same_key_and_rejects_replacement() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(format!("alondra-service-approval-{}", random_hex(4)));
        std::fs::create_dir_all(&dir).unwrap();
        let auth = Auth::open(&dir).unwrap();
        let first = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let second = SigningKey::from_slice(&[8u8; 32]).unwrap();
        let first_key = URL_SAFE_NO_PAD.encode(first.verifying_key().to_sec1_bytes());
        let second_key = URL_SAFE_NO_PAD.encode(second.verifying_key().to_sec1_bytes());
        auth.approve_service("client-test", "Terminal", &first_key).unwrap();
        assert!(auth.approve_service("client-test", "Other terminal", &second_key).is_err());
        assert!(auth.approve_service("mac-test", "Executor", &first_key).is_err());
        assert!(auth.approve_service("client-bad/id", "Terminal", &first_key).is_err());
        auth.approve_service("phone-test", "iPhone", &first_key).unwrap();
        auth.approve_service("phone-test", "iPhone", &first_key).unwrap();
        assert!(auth.approve_service("phone-test", "other", &second_key).is_err());
        assert_eq!(Auth::open(&dir).unwrap().list().len(), 2);
    }

    #[test]
    fn service_approval_refuses_a_device_this_mac_revoked() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(format!("alondra-service-revoked-{}", random_hex(4)));
        std::fs::create_dir_all(&dir).unwrap();
        let auth = Auth::open(&dir).unwrap();
        let phone = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let agreement = p256::SecretKey::from_slice(&[6u8; 32]).unwrap().public_key();
        let key = URL_SAFE_NO_PAD.encode(phone.verifying_key().to_sec1_bytes());
        auth.approve_service("phone-test", "iPhone", &key).unwrap();
        assert!(auth.revoke("phone-test").unwrap());
        crate::revoked_devices::RevokedDevices::open(&dir)
            .add(vec![crate::revoked_devices::Tombstone::new("phone-test", phone.verifying_key(), &agreement, 1)])
            .unwrap();
        assert_eq!(auth.approve_service("phone-test", "iPhone", &key), Err(ServiceApprovalError::Revoked));
        assert!(Auth::open(&dir).unwrap().list().is_empty());
        auth.approve_service("phone-other", "iPhone", &key).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn local_service_approval_requires_mac_key_and_fresh_challenge() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(format!("alondra-service-proof-{}", random_hex(4)));
        std::fs::create_dir_all(dir.join("service-keys")).unwrap();
        let mac = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let public = URL_SAFE_NO_PAD.encode(mac.verifying_key().to_sec1_bytes());
        std::fs::write(dir.join("service-keys/public.json"),
            serde_json::json!({"mac_id":"mac-test","signing_key":public}).to_string()).unwrap();
        let auth = Auth::open(&dir).unwrap();
        let epoch = auth.service_epoch();
        let now = now_millis() as u64;
        let nonce = [3u8; 16];
        let body = br#"{"device_id":"phone-a","name":"iPhone","signing_key":"key"}"#;
        let signature = alondra_e2e::sign_http_challenge(&mac, "mac-test", "POST",
            "/service/approve-device", &epoch, body, now, &nonce).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-device-id", "mac-test".parse().unwrap());
        headers.insert("x-relay-epoch", URL_SAFE_NO_PAD.encode(epoch).parse().unwrap());
        headers.insert("x-device-timestamp-ms", now.to_string().parse().unwrap());
        headers.insert("x-device-nonce", URL_SAFE_NO_PAD.encode(nonce).parse().unwrap());
        headers.insert("x-device-signature", signature.parse().unwrap());
        assert!(auth.verify_service_approval(&headers, b"different").is_err());
        auth.verify_service_approval(&headers, body).unwrap();
        assert!(auth.verify_service_approval(&headers, body).is_err());
    }

    #[test]
    fn failed_revocation_keeps_device_approved_and_reports_failure() {
        let (auth, _, id) = auth_with_device();
        std::fs::create_dir(auth.path.with_extension("json.tmp")).unwrap();
        assert!(auth.revoke(&id).is_err());
        assert!(auth.list().iter().any(|device| device.id == id));
    }

    #[test]
    fn proxied_requests_are_never_local() {
        let peer: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let mut h = HeaderMap::new();
        assert!(is_local(&peer, &h));
        h.insert("cf-ray", "abc".parse().unwrap());
        assert!(!is_local(&peer, &h));
        let lan: SocketAddr = "192.168.1.20:5000".parse().unwrap();
        assert!(!is_local(&lan, &HeaderMap::new()));
    }
}
