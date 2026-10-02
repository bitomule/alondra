//! APNs transport for Mac-signed, per-phone encrypted notification envelopes.
//! APNs sees a device token, routing metadata and ciphertext, never notification content.

use crate::auth::PushRegistration;
use alondra_e2e::EnvelopeV1;
use anyhow::{Context, Result, bail};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const APNS_MAX_BYTES: usize = 4096;
const APNS_TIMEOUT: Duration = Duration::from_secs(5);
/// Apple: refresh the provider token no more than once every 20 minutes and no less than
/// once every 60; a newer one sooner is rejected with 429 TooManyProviderTokenUpdates.
const PROVIDER_TOKEN_LIFETIME_SECS: u64 = 40 * 60;

#[derive(Clone, Deserialize)]
pub struct ApnsConfig {
    pub team_id: String,
    pub key_id: String,
    pub topic: String,
    /// Apple's downloaded .p8 key; read only by the relay process.
    pub private_key_path: PathBuf,
}

pub struct Apns {
    team_id: String,
    key_id: String,
    topic: String,
    private_key: EcdsaKeyPair,
    client: reqwest::Client,
    provider_token: std::sync::Mutex<Option<(u64, String)>>,
    #[cfg(test)]
    test_endpoint: Option<String>,
}

pub fn validate_registration(reg: &PushRegistration) -> Result<()> {
    if reg.token.len() != 64 || !reg.token.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid APNs token");
    }
    if !matches!(reg.environment.as_str(), "sandbox" | "production") {
        bail!("invalid APNs environment");
    }
    let binding = URL_SAFE_NO_PAD.decode(&reg.push_binding)?;
    if binding.len() != 32 || URL_SAFE_NO_PAD.encode(binding) != reg.push_binding {
        bail!("push binding must be 32 canonical base64url bytes");
    }
    Ok(())
}

/// Exact payload submitted to APNs. The caller must enforce its serialized byte limit.
pub fn payload(envelope: &EnvelopeV1, binding: &str) -> Value {
    json!({
        "aps": {"alert": {"title": "Alondra", "body": "Nueva actividad"}, "mutable-content": 1},
        "alondra": {"envelope": envelope, "push_binding": binding}
    })
}

impl Apns {
    fn http_client() -> Result<reqwest::Client> {
        // APNs requires HTTP/2 over TLS. Do not fall back to HTTP/1.1.
        Ok(reqwest::Client::builder()
            .http2_prior_knowledge()
            .min_tls_version(reqwest::tls::Version::TLS_1_2)
            .timeout(APNS_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()?)
    }

    pub fn open(config: ApnsConfig) -> Result<Self> {
        if config.team_id.len() != 10
            || !config.team_id.bytes().all(|b| b.is_ascii_alphanumeric())
            || config.key_id.len() != 10
            || !config.key_id.bytes().all(|b| b.is_ascii_alphanumeric())
            || config.topic.is_empty()
            || config.topic.len() > 255
            || !config
                .topic
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        {
            bail!("invalid APNs identifiers");
        }
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(&config.private_key_path)?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
            bail!("APNs private key must be an owner-only regular file");
        }
        let pem = std::fs::read_to_string(&config.private_key_path)?;
        let der = pem
            .strip_prefix("-----BEGIN PRIVATE KEY-----")
            .and_then(|s| s.trim().strip_suffix("-----END PRIVATE KEY-----"))
            .context("APNs key must be PKCS8 PEM")?;
        let der = STANDARD.decode(der.split_whitespace().collect::<String>())?;
        let rng = SystemRandom::new();
        let private_key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &der, &rng)
            .map_err(|_| anyhow::anyhow!("invalid APNs private key"))?;
        let client = Self::http_client()?;
        Ok(Self {
            team_id: config.team_id,
            key_id: config.key_id,
            topic: config.topic,
            private_key,
            client,
            provider_token: Default::default(),
            #[cfg(test)]
            test_endpoint: None,
        })
    }

    #[cfg(test)]
    pub fn test(endpoint: String) -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let private_key =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                .unwrap();
        Self {
            team_id: "TEAMID1234".into(),
            key_id: "KEYID12345".into(),
            topic: "com.bitomule.alondra".into(),
            private_key,
            client: Self::http_client().unwrap(),
            provider_token: Default::default(),
            test_endpoint: Some(endpoint),
        }
    }

    fn bearer(&self) -> Result<String> {
        self.bearer_at(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
    }

    fn bearer_at(&self, now: u64) -> Result<String> {
        let mut cached = self.provider_token.lock().unwrap();
        if let Some((issued, token)) = cached.as_ref()
            && now.saturating_sub(*issued) < PROVIDER_TOKEN_LIFETIME_SECS
        {
            return Ok(token.clone());
        }
        let token = self.sign_provider_token(now)?;
        *cached = Some((now, token.clone()));
        Ok(token)
    }

    fn sign_provider_token(&self, now: u64) -> Result<String> {
        let header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(
            &json!({"alg":"ES256","kid":self.key_id}),
        )?);
        let claims =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"iss":self.team_id,"iat":now}))?);
        let signed = format!("{header}.{claims}");
        let signature = self
            .private_key
            .sign(&SystemRandom::new(), signed.as_bytes())
            .map_err(|_| anyhow::anyhow!("APNs token signing failed"))?;
        Ok(format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(signature.as_ref())
        ))
    }

    pub async fn send(&self, reg: &PushRegistration, envelope: &EnvelopeV1) -> Result<()> {
        let body = serde_json::to_vec(&payload(envelope, &reg.push_binding))?;
        if body.len() > APNS_MAX_BYTES {
            bail!("APNs payload exceeds 4096 bytes");
        }
        let host = if reg.environment == "sandbox" {
            "api.sandbox.push.apple.com"
        } else {
            "api.push.apple.com"
        };
        let url = format!("https://{host}/3/device/{}", reg.token);
        #[cfg(test)]
        let url = self
            .test_endpoint
            .as_ref()
            .map(|base| format!("{base}/3/device/{}", reg.token))
            .unwrap_or(url);
        let response = self
            .client
            .post(url)
            .header("authorization", format!("bearer {}", self.bearer()?))
            .header("apns-topic", &self.topic)
            .header("apns-push-type", "alert")
            .header(
                "apns-expiration",
                (envelope.expires_at_ms / 1000).to_string(),
            )
            .header("apns-priority", "10")
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;
        let status = response.status();
        if !status.is_success() {
            let apns_id = response
                .headers()
                .get("apns-id")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body = response.bytes().await.unwrap_or_default();
            return Err(ApnsRejection::new(status.as_u16(), apns_id.as_deref(), &body).into());
        }
        Ok(())
    }
}

/// APNs refused a push. Carries only Apple's status, reason, apns-id and, for a dead token,
/// when Apple stopped accepting it; never the token or payload.
#[derive(Debug, PartialEq)]
pub struct ApnsRejection {
    pub status: u16,
    pub reason: Option<String>,
    pub apns_id: Option<String>,
    pub inactive_since_ms: Option<u64>,
}

impl ApnsRejection {
    pub fn new(status: u16, apns_id: Option<&str>, body: &[u8]) -> Self {
        let body = serde_json::from_slice::<Value>(body).ok();
        let field = |key: &str| body.as_ref().map(|value| &value[key]);
        Self {
            status,
            reason: field("reason").and_then(Value::as_str).and_then(log_safe),
            apns_id: apns_id.and_then(log_safe),
            inactive_since_ms: field("timestamp").and_then(Value::as_u64),
        }
    }

    /// Apple will never deliver to this token again; the phone must register a new one.
    pub fn registration_is_dead(&self) -> bool {
        self.status == 410
            || (self.status == 400
                && matches!(
                    self.reason.as_deref(),
                    Some("BadDeviceToken" | "DeviceTokenNotForTopic" | "Unregistered")
                ))
    }
}

fn log_safe(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')))
    .then(|| value.to_owned())
}

impl std::fmt::Display for ApnsRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "APNs rejected encrypted push with HTTP {} reason={} apns-id={}",
            self.status,
            self.reason.as_deref().unwrap_or("none"),
            self.apns_id.as_deref().unwrap_or("none")
        )?;
        match self.inactive_since_ms {
            Some(at) => write!(f, " inactive-since-ms={at}"),
            None => Ok(()),
        }
    }
}

impl std::error::Error for ApnsRejection {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_token_is_reused_until_it_ages() {
        let apns = Apns::test("http://127.0.0.1:1".into());
        let first = apns.bearer_at(1_000).unwrap();
        assert_eq!(apns.bearer_at(1_001).unwrap(), first, "a new token per push gets 429 TooManyProviderTokenUpdates");
        assert_eq!(apns.bearer_at(1_000 + 39 * 60).unwrap(), first);
        let refreshed = apns.bearer_at(1_000 + 40 * 60).unwrap();
        assert_ne!(refreshed, first, "APNs rejects a token older than an hour");
        assert_eq!(apns.bearer_at(1_000 + 41 * 60).unwrap(), refreshed);
    }

    #[test]
    fn apns_payload_matches_the_cross_language_fixture() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../../fixtures/push/encrypted.json")).unwrap();
        let alondra = &fixture["apns"]["alondra"];
        let envelope: EnvelopeV1 = serde_json::from_value(alondra["envelope"].clone()).unwrap();
        assert_eq!(
            payload(&envelope, alondra["push_binding"].as_str().unwrap()),
            fixture["apns"],
            "the notification service extension decrypts exactly this shape"
        );
    }

    #[test]
    fn binding_and_token_are_canonical() {
        let valid = PushRegistration {
            token: "ab".repeat(32),
            environment: "sandbox".into(),
            push_binding: URL_SAFE_NO_PAD.encode([7u8; 32]),
        };
        assert!(validate_registration(&valid).is_ok());
        let mut invalid = valid.clone();
        invalid.push_binding.push('=');
        assert!(validate_registration(&invalid).is_err());
        invalid = valid.clone();
        invalid.environment = "other".into();
        assert!(validate_registration(&invalid).is_err());
        invalid = valid.clone();
        invalid.token = "g0".repeat(32);
        assert!(validate_registration(&invalid).is_err());
    }

    #[test]
    fn rejection_keeps_apple_reason_and_apns_id() {
        let rejection = ApnsRejection::new(
            400,
            Some("EC1BF194-B3B2-424A-89A9-5A918A6E6B5C"),
            br#"{"reason":"BadDeviceToken"}"#,
        );
        assert_eq!(rejection.reason.as_deref(), Some("BadDeviceToken"));
        assert_eq!(
            rejection.to_string(),
            "APNs rejected encrypted push with HTTP 400 reason=BadDeviceToken apns-id=EC1BF194-B3B2-424A-89A9-5A918A6E6B5C"
        );
        assert_eq!(
            ApnsRejection::new(500, None, b"<html>oops</html>").to_string(),
            "APNs rejected encrypted push with HTTP 500 reason=none apns-id=none"
        );
        let injected = ApnsRejection::new(400, Some("id\nforged"), br#"{"reason":"Bad\nline"}"#);
        assert_eq!((injected.reason, injected.apns_id), (None, None));
        assert_eq!(
            ApnsRejection::new(410, None, br#"{"reason":"Unregistered","timestamp":1759398000000}"#)
                .to_string(),
            "APNs rejected encrypted push with HTTP 410 reason=Unregistered apns-id=none inactive-since-ms=1759398000000"
        );
    }

    #[test]
    fn only_dead_token_rejections_mark_the_registration_gone() {
        let dead = |status, body: &[u8]| ApnsRejection::new(status, None, body).registration_is_dead();
        assert!(dead(410, br#"{"reason":"Unregistered","timestamp":1}"#));
        assert!(dead(410, br#"{"reason":"ExpiredToken"}"#));
        assert!(dead(410, b""));
        assert!(dead(400, br#"{"reason":"BadDeviceToken"}"#));
        assert!(dead(400, br#"{"reason":"DeviceTokenNotForTopic"}"#));
        assert!(dead(400, br#"{"reason":"Unregistered"}"#));
        assert!(!dead(400, br#"{"reason":"BadTopic"}"#));
        assert!(!dead(400, b""));
        assert!(!dead(403, br#"{"reason":"InvalidProviderToken"}"#));
        assert!(!dead(429, br#"{"reason":"TooManyRequests"}"#));
        assert!(!dead(503, br#"{"reason":"ServiceUnavailable"}"#));
    }
}
