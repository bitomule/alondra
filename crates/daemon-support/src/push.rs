use crate::locks::Poisonable as _;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use alondra_core::now_millis;
use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const JWT_LIFETIME_MS: i64 = 50 * 60 * 1000;

#[derive(Deserialize)]
struct ApnsConfig {
    key_path: PathBuf,
    key_id: String,
    team_id: String,
    topic: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Registration {
    pub token: String,
    pub environment: String,
    /// Random client-side context identifier. Old registrations omit it and
    /// their notifications remain non-actionable in clients that enforce it.
    #[serde(default)]
    pub push_binding: Option<String>,
    #[serde(default)]
    pub quiet_start: Option<String>,
    #[serde(default)]
    pub quiet_end: Option<String>,
    pub updated_at: i64,
}

/// A canonical base64url encoding of 32 random bytes. It is an opaque context
/// label, not an authentication credential.
pub fn valid_push_binding(value: &str) -> bool {
    B64URL.decode(value).is_ok_and(|bytes| bytes.len() == 32 && B64URL.encode(bytes) == value)
}

pub struct Push {
    /// A holder that panics leaves it whole, so it is recovered: a clone or one retain.
    registrations: Mutex<HashMap<String, Registration>>,
    apns: Option<Apns>,
}

struct Apns {
    key: SigningKey,
    key_id: String,
    team_id: String,
    topic: String,
    client: reqwest::Client,
    /// A holder that panics leaves it whole, so it is recovered: one assignment of a token signed
    /// before it.
    jwt: Mutex<Option<(String, i64)>>,
}

impl Push {
    pub fn open(home: &Path) -> Result<Self> {
        let registrations = std::fs::read_to_string(home.join("push.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let apns = match std::fs::read_to_string(home.join("apns.json")) {
            Ok(text) => Some(Apns::load(serde_json::from_str(&text).context("apns.json ilegible")?)?),
            Err(_) => None,
        };
        if apns.is_none() {
            log!("no ~/.alondra/apns.json, notifications are not sent");
        }
        Ok(Push { registrations: Mutex::new(registrations), apns })
    }

    pub fn enabled(&self) -> bool {
        self.apns.is_some()
    }

    fn registrations(&self) -> Vec<Registration> {
        self.registrations.lock_or_recover("the push registrations").values().cloned().collect()
    }

    fn forget_token(&self, token: &str) {
        let mut regs = self.registrations.lock_or_recover("the push registrations");
        regs.retain(|_, r| r.token != token);
    }

    pub async fn broadcast(&self, payload: &Value, with_sound: bool) {
        let _ = self.broadcast_report(payload, with_sound).await;
    }

    /// APNs acceptance is a technical handoff, not proof the person saw it.
    /// A partial batch is explicit so domain delivery does not mark every
    /// target handed after one device failed.
    pub async fn broadcast_report(&self, payload: &Value, with_sound: bool) -> (usize, usize) {
        let Some(apns) = &self.apns else { return (0, 0) };
        let mut attempted = 0;
        let mut accepted = 0;
        for reg in self.registrations() {
            attempted += 1;
            let mut body = payload_for_registration(payload, &reg);
            let quiet = in_quiet_hours(reg.quiet_start.as_deref(), reg.quiet_end.as_deref(), local_minutes());
            let level = if quiet { "passive" } else { "active" };
            body["aps"]["interruption-level"] = json!(level);
            if (quiet || !with_sound)
                && let Some(aps) = body["aps"].as_object_mut()
            {
                aps.remove("sound");
            }
            let priority = if quiet { 5 } else { 10 };
            match apns.send(&reg.token, &reg.environment, &body, priority).await {
                Ok((200, _)) => { accepted += 1; }
                Ok((410, _)) | Ok((400, _)) => {
                    log!("APNs rejects a token, forgetting it");
                    self.forget_token(&reg.token);
                }
                Ok((status, text)) => log!("APNs {status}: {text}"),
                Err(e) => log!("APNs does not respond: {e}"),
            }
        }
        (attempted, accepted)
    }
}

fn payload_for_registration(payload: &Value, reg: &Registration) -> Value {
    let mut body = payload.clone();
    if let Some(binding) = &reg.push_binding {
        body["alondra"]["push_binding"] = json!(binding);
    }
    body
}

impl Apns {
    fn load(cfg: ApnsConfig) -> Result<Self> {
        let pem = std::fs::read_to_string(&cfg.key_path).with_context(|| format!("cannot read {}", cfg.key_path.display()))?;
        let key = SigningKey::from_pkcs8_pem(&pem).map_err(|e| anyhow::anyhow!("invalid .p8 key: {e}"))?;
        let client = reqwest::Client::builder().http2_prior_knowledge().timeout(Duration::from_secs(15)).build()?;
        Ok(Apns { key, key_id: cfg.key_id, team_id: cfg.team_id, topic: cfg.topic, client, jwt: Mutex::new(None) })
    }

    fn jwt(&self) -> String {
        let mut cached = self.jwt.lock_or_recover("the APNs token");
        if let Some((token, at)) = cached.as_ref()
            && now_millis() - at < JWT_LIFETIME_MS
        {
            return token.clone();
        }
        let header = B64URL.encode(json!({"alg": "ES256", "kid": self.key_id}).to_string());
        let claims = B64URL.encode(json!({"iss": self.team_id, "iat": now_millis() / 1000}).to_string());
        let unsigned = format!("{header}.{claims}");
        let signature: Signature = self.key.sign(unsigned.as_bytes());
        let token = format!("{unsigned}.{}", B64URL.encode(signature.to_bytes()));
        *cached = Some((token.clone(), now_millis()));
        token
    }

    async fn send(&self, token: &str, environment: &str, body: &Value, priority: u8) -> Result<(u16, String)> {
        let host = if environment == "production" { "api.push.apple.com" } else { "api.sandbox.push.apple.com" };
        let resp = self
            .client
            .post(format!("https://{host}/3/device/{token}"))
            .bearer_auth(self.jwt())
            .header("apns-topic", &self.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", priority.to_string())
            .json(body)
            .send()
            .await?;
        let status = resp.status().as_u16();
        Ok((status, resp.text().await.unwrap_or_default()))
    }
}

pub fn local_minutes() -> u32 {
    let t: libc::time_t = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    (tm.tm_hour * 60 + tm.tm_min) as u32
}

fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.split_once(':')?;
    Some(h.parse::<u32>().ok()? * 60 + m.parse::<u32>().ok()?)
}

pub fn in_quiet_hours(start: Option<&str>, end: Option<&str>, now: u32) -> bool {
    let (Some(s), Some(e)) = (start.and_then(parse_hhmm), end.and_then(parse_hhmm)) else { return false };
    if s <= e { now >= s && now < e } else { now >= s || now < e }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_binding_is_canonical_and_scoped_to_each_registration() {
        let first = B64URL.encode([1u8; 32]);
        let second = B64URL.encode([2u8; 32]);
        assert!(valid_push_binding(&first));
        assert!(valid_push_binding(&second));
        for bad in [String::new(), "short".into(), B64URL.encode([3u8; 31]), first.clone() + "="] {
            assert!(!valid_push_binding(&bad));
        }
        let registration = |binding: Option<String>| Registration {
            token: "ab".into(), environment: "sandbox".into(), push_binding: binding,
            quiet_start: None, quiet_end: None, updated_at: 0,
        };
        let payload = json!({"aps": {"alert": {"title": "Pick one"}}, "alondra": {"kind": "question", "id": 7}});
        assert_eq!(payload_for_registration(&payload, &registration(Some(first.clone())))["alondra"]["push_binding"].as_str(), Some(first.as_str()));
        assert_eq!(payload_for_registration(&payload, &registration(Some(second.clone())))["alondra"]["push_binding"].as_str(), Some(second.as_str()));
        assert!(payload_for_registration(&payload, &registration(None))["alondra"].get("push_binding").is_none());
        let old: Registration = serde_json::from_value(json!({
            "token":"ab", "environment":"sandbox", "updated_at":0
        })).unwrap();
        assert!(old.push_binding.is_none(), "old push.json registrations remain readable");
    }

    #[test]
    fn quiet_hours_across_midnight() {
        assert!(in_quiet_hours(Some("23:00"), Some("08:00"), 23 * 60 + 30));
        assert!(in_quiet_hours(Some("23:00"), Some("08:00"), 3 * 60));
        assert!(!in_quiet_hours(Some("23:00"), Some("08:00"), 12 * 60));
        assert!(in_quiet_hours(Some("13:00"), Some("15:00"), 14 * 60));
        assert!(!in_quiet_hours(None, Some("08:00"), 3 * 60));
    }
}
