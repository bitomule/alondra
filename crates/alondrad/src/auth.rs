use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alondra_core::now_millis;
use anyhow::{Context, Result};
use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_SKEW_MS: i64 = 120_000;
const PAIRING_TTL: Duration = Duration::from_secs(600);
const MAX_BODY: usize = 10 * 1024 * 1024;
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
    devices: Mutex<Vec<Device>>,
    pairing: Mutex<HashMap<String, Instant>>,
    seen: Mutex<VecDeque<String>>,
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

impl Auth {
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
        let mut p = self.pairing.lock().expect("pairing lock poisoned");
        p.retain(|_, t| t.elapsed() < PAIRING_TTL);
        p.insert(code.clone(), Instant::now());
        code
    }

    pub fn pair(&self, code: &str, name: &str, public_key_b64: &str) -> Result<Device, String> {
        let valid = {
            let mut p = self.pairing.lock().expect("pairing lock poisoned");
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
        let mut devices = self.devices.lock().expect("devices lock poisoned");
        devices.push(device.clone());
        self.save(&devices).map_err(|e| e.to_string())?;
        Ok(device)
    }

    pub fn list(&self) -> Vec<Device> {
        self.devices.lock().expect("devices lock poisoned").clone()
    }

    pub fn revoke(&self, id: &str) -> bool {
        let mut devices = self.devices.lock().expect("devices lock poisoned");
        let before = devices.len();
        devices.retain(|d| d.id != id);
        let changed = devices.len() != before;
        if changed {
            let _ = self.save(&devices);
        }
        changed
    }

    fn verify(&self, headers: &HeaderMap, method: &str, path_and_query: &str, body: &[u8]) -> Result<Signed, &'static str> {
        let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok());
        let device_id = h("x-alondra-device").ok_or("missing X-Alondra-Device")?;
        let timestamp = h("x-alondra-timestamp").ok_or("missing X-Alondra-Timestamp")?;
        let signature_b64 = h("x-alondra-signature").ok_or("missing X-Alondra-Signature")?;
        let ts: i64 = timestamp.parse().map_err(|_| "invalid timestamp")?;
        if (now_millis() - ts).abs() > MAX_SKEW_MS {
            return Err("timestamp out of range");
        }
        let key_b64 = {
            let devices = self.devices.lock().expect("devices lock poisoned");
            devices.iter().find(|d| d.id == device_id).map(|d| d.public_key.clone())
        }
        .ok_or("device not paired")?;
        let key = B64.decode(key_b64).ok().and_then(|raw| VerifyingKey::from_sec1_bytes(&raw).ok()).ok_or("stored key is corrupt")?;
        let der = B64.decode(signature_b64).map_err(|_| "signature badly encoded")?;
        let signature = Signature::from_der(&der).map_err(|_| "malformed signature")?;
        let message = signed_message(method, path_and_query, timestamp, body);
        key.verify(message.as_bytes(), &signature).map_err(|_| "wrong signature")?;
        let mut seen = self.seen.lock().expect("seen lock poisoned");
        if seen.iter().any(|s| s == signature_b64) {
            return Err("replayed request");
        }
        seen.push_back(signature_b64.to_string());
        if seen.len() > REPLAY_MEMORY {
            seen.pop_front();
        }
        drop(seen);
        let mut devices = self.devices.lock().expect("devices lock poisoned");
        let device = devices.iter_mut().find(|d| d.id == device_id).ok_or("device not paired")?;
        device.last_seen = Some(now_millis());
        Ok(Signed { name: device.name.clone() })
    }
}

#[derive(Clone, Debug)]
pub struct Signed {
    pub name: String,
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
    if req.uri().path() == "/v1/pair" || (is_local(&peer, req.headers()) && !presents_signature) {
        return next.run(req).await;
    }
    let (parts, body) = req.into_parts();
    let Ok(bytes) = to_bytes(body, MAX_BODY).await else {
        return unauthorized("cuerpo demasiado grande");
    };
    let path_and_query = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let signed = match auth.verify(&parts.headers, parts.method.as_str(), path_and_query, &bytes) {
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

    fn auth_with_device() -> (Auth, SigningKey, String) {
        let dir = std::env::temp_dir().join(format!("alondra-auth-{}", random_hex(4)));
        std::fs::create_dir_all(&dir).unwrap();
        let auth = Auth::open(&dir).unwrap();
        let signing = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let public = B64.encode(signing.verifying_key().to_sec1_bytes());
        let code = auth.new_pairing_code();
        let device = auth.pair(&code, "iPhone", &public).unwrap();
        (auth, signing, device.id)
    }

    fn headers(device: &str, ts: i64, sig: &Signature) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-alondra-device", device.parse().unwrap());
        h.insert("x-alondra-timestamp", ts.to_string().parse().unwrap());
        h.insert("x-alondra-signature", B64.encode(sig.to_der().as_bytes()).parse().unwrap());
        h
    }

    #[test]
    fn valid_signature_passes_and_replay_is_rejected() {
        let (auth, key, id) = auth_with_device();
        let ts = now_millis();
        let msg = signed_message("POST", "/v1/questions/1/answer", &ts.to_string(), b"{\"text\":\"yes\"}");
        let sig: Signature = key.sign(msg.as_bytes());
        let h = headers(&id, ts, &sig);
        assert_eq!(auth.verify(&h, "POST", "/v1/questions/1/answer", b"{\"text\":\"yes\"}").unwrap().name, "iPhone");
        assert_eq!(auth.verify(&h, "POST", "/v1/questions/1/answer", b"{\"text\":\"yes\"}").unwrap_err(), "replayed request");
    }

    #[test]
    fn tampered_body_or_path_fails() {
        let (auth, key, id) = auth_with_device();
        let ts = now_millis();
        let msg = signed_message("POST", "/v1/questions/1/answer", &ts.to_string(), b"{\"text\":\"yes\"}");
        let sig: Signature = key.sign(msg.as_bytes());
        let h = headers(&id, ts, &sig);
        assert_eq!(auth.verify(&h, "POST", "/v1/questions/1/answer", b"{\"text\":\"no\"}").unwrap_err(), "wrong signature");
        assert_eq!(auth.verify(&h, "POST", "/v1/questions/2/answer", b"{\"text\":\"yes\"}").unwrap_err(), "wrong signature");
    }

    #[test]
    fn old_timestamp_and_unknown_device_fail() {
        let (auth, key, id) = auth_with_device();
        let ts = now_millis() - 10 * 60_000;
        let msg = signed_message("GET", "/v1/overview", &ts.to_string(), b"");
        let sig: Signature = key.sign(msg.as_bytes());
        assert_eq!(auth.verify(&headers(&id, ts, &sig), "GET", "/v1/overview", b"").unwrap_err(), "timestamp out of range");
        let ts = now_millis();
        let msg = signed_message("GET", "/v1/overview", &ts.to_string(), b"");
        let sig: Signature = key.sign(msg.as_bytes());
        assert_eq!(auth.verify(&headers("other", ts, &sig), "GET", "/v1/overview", b"").unwrap_err(), "device not paired");
    }

    #[test]
    fn pairing_code_is_single_use() {
        let dir = std::env::temp_dir().join(format!("alondra-auth-{}", random_hex(4)));
        std::fs::create_dir_all(&dir).unwrap();
        let auth = Auth::open(&dir).unwrap();
        let key = SigningKey::from_slice(&[9u8; 32]).unwrap();
        let public = B64.encode(key.verifying_key().to_sec1_bytes());
        let code = auth.new_pairing_code();
        assert!(auth.pair(&code, "a", &public).is_ok());
        assert!(auth.pair(&code, "b", &public).is_err());
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
