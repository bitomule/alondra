use crate::locks::Poisonable as _;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

pub struct Auth {
    path: PathBuf,
    /// A holder that panics leaves it whole, so it is recovered: each writer saves the file first
    /// and then replaces the list whole, so memory always matches a saved file.
    devices: Mutex<Vec<Device>>,
    /// A holder that panics leaves it whole, so it is recovered: a retain and an insert, or one
    /// remove.
    pairing: Mutex<HashMap<String, Instant>>,
    /// A holder that panics leaves it whole, so it is recovered: a push and a pop.
    seen: Mutex<VecDeque<String>>,
    /// A holder that panics leaves it whole, so it is recovered: it records a nonce only after the
    /// signature checks, so a torn change can only refuse a request.
    service_verifier: Mutex<HttpChallengeVerifier>,
    usage: crate::device_usage::DeviceUsage,
}

pub fn random_hex(bytes: usize) -> String {
    use std::io::Read;
    let mut buf = vec![0u8; bytes];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

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
            devices: Mutex::new(devices),
            pairing: Mutex::new(HashMap::new()),
            seen: Mutex::new(VecDeque::new()),
            service_verifier: Mutex::new(HttpChallengeVerifier::new(120_000, 4096)?),
            usage: crate::device_usage::DeviceUsage::at(home),
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
        p.retain(|_, t| t.elapsed() < PAIRING_TTL);
        p.insert(code.clone(), Instant::now());
        code
    }

    pub fn pair(&self, code: &str, name: &str, public_key_b64: &str) -> Result<Device, String> {
        let valid = {
            let mut p = self.pairing.lock_or_recover("the pairing codes");
            p.remove(code).is_some_and(|t| t.elapsed() < PAIRING_TTL)
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
        self.save(&next).map_err(|e| e.to_string())?;
        *devices = next;
        Ok(device)
    }

    /// The service CLI calls this only after a human accepts the encrypted candidate on
    /// this Mac. Reuse the daemon's registry lock, avoiding concurrent file rewrites.
    pub fn approve_service(&self, id: &str, name: &str, signing_key: &str) -> Result<(), String> {
        if !(id.starts_with("phone-") || id.starts_with("client-")) || id.len() > 128
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || name.len() > 80 || name.chars().any(char::is_control)
        { return Err("invalid service device".into()); }
        let raw = URL_SAFE_NO_PAD.decode(signing_key).map_err(|_| "invalid service signing key")?;
        let key = VerifyingKey::from_sec1_bytes(&raw).map_err(|_| "invalid service signing key")?;
        if raw.len() != 65 || raw[0] != 4 || URL_SAFE_NO_PAD.encode(&raw) != signing_key {
            return Err("noncanonical service signing key".into());
        }
        let public_key = B64.encode(key.to_sec1_bytes());
        let _lease = crate::approval_lease::ApprovalLease::exclusive(self.path.parent().ok_or("approval home missing")?)
            .map_err(|error| error.to_string())?;
        let mut devices = self.devices.lock_or_recover("the paired devices");
        let mut next = self.persisted().map_err(|error| error.to_string())?;
        if let Some(existing) = next.iter().find(|d| d.id == id) {
            let same_key = existing.public_key == public_key;
            *devices = next;
            return if same_key { Ok(()) } else { Err("device ID already has another key".into()) };
        }
        next.push(Device { id: id.into(), name: name.into(), public_key,
            created_at: now_millis(), last_seen: None });
        self.save(&next).map_err(|e| e.to_string())?;
        *devices = next;
        Ok(())
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
        self.devices.lock_or_recover("the paired devices").clone()
    }

    pub fn revoke(&self, id: &str) -> Result<bool> {
        let _lease = crate::approval_lease::ApprovalLease::exclusive(self.path.parent().context("approval home missing")?)?;
        let mut devices = self.devices.lock_or_recover("the paired devices");
        let mut next = self.persisted()?;
        let previous_len = next.len();
        next.retain(|d| d.id != id);
        let changed = next.len() != previous_len;
        if changed {
            self.save(&next)?;
        }
        *devices = next;
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
        let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok());
        let device_id = h("x-alondra-device").ok_or("missing X-Alondra-Device")?;
        let timestamp = h("x-alondra-timestamp").ok_or("missing X-Alondra-Timestamp")?;
        let signature_b64 = h("x-alondra-signature").ok_or("missing X-Alondra-Signature")?;
        let ts: i64 = timestamp.parse().map_err(|_| "invalid timestamp")?;
        if (now - ts).abs() > MAX_SKEW_MS {
            return Err("timestamp out of range");
        }
        let key_b64 = {
            let devices = self.devices.lock_or_recover("the paired devices");
            devices.iter().find(|d| d.id == device_id).map(|d| d.public_key.clone())
        }
        .ok_or("device not paired")?;
        let key = B64.decode(&key_b64).ok().and_then(|raw| VerifyingKey::from_sec1_bytes(&raw).ok()).ok_or("stored key is corrupt")?;
        let der = B64.decode(signature_b64).map_err(|_| "signature badly encoded")?;
        let signature = Signature::from_der(&der).map_err(|_| "malformed signature")?;
        let message = signed_message(method, path_and_query, timestamp, body);
        key.verify(message.as_bytes(), &signature).map_err(|_| "wrong signature")?;
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
        let devices = self.devices.lock_or_recover("the paired devices");
        let device = devices.iter().find(|d| d.id == device_id).ok_or("device not paired")?;
        self.usage.touch(&device.id, now_millis(), local);
        Ok(Signed { device_id: device.id.clone(), name: device.name.clone(), public_key: key_b64 })
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
    if local && !presents_signature {
        return next.run(req).await;
    }
    let (parts, body) = req.into_parts();
    let Ok(bytes) = to_bytes(body, MAX_BODY).await else {
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
mod tests {
    use super::*;
    use p256::ecdsa::SigningKey;
    use p256::ecdsa::signature::Signer;

    fn auth_with_device() -> (crate::test_support::Owned<Auth>, SigningKey, String) {
        let dir = crate::test_support::Directory::new();
        std::fs::create_dir_all(&dir).unwrap();
        let auth = Auth::open(&dir).unwrap();
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
