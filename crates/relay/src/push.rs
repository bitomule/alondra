//! APNs transport for Mac-signed, per-phone encrypted notification envelopes.
//! APNs sees a device token, routing metadata and ciphertext, never notification content.

use crate::auth::PushRegistration;
use alondra_e2e::Envelope;
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
    pub counters: std::sync::Mutex<Counters>,
    provider_token: std::sync::Mutex<Option<(u64, String)>>,
    #[cfg(test)]
    test_endpoint: Option<String>,
}

#[derive(Default, Clone, serde::Serialize)]
pub struct Counters {
    pub accepted: u64,
    pub token_expired: u64,
    pub provider_token_expired: u64,
    pub retryable: u64,
    pub rejected: std::collections::BTreeMap<String, u64>,
}

pub fn validate_registration(reg: &PushRegistration) -> Result<()> {
    if reg.token.len() != 64 || !reg.token.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid APNs token");
    }
    if !matches!(reg.environment.as_str(), "sandbox" | "production") {
        bail!("invalid APNs environment");
    }
    if !valid_binding(&reg.push_binding) {
        bail!("push binding must be 32 canonical base64url bytes");
    }
    Ok(())
}

pub fn valid_binding(binding: &str) -> bool {
    URL_SAFE_NO_PAD
        .decode(binding)
        .is_ok_and(|bytes| bytes.len() == 32 && URL_SAFE_NO_PAD.encode(bytes) == binding)
}

/// How APNs hands the push to the phone: an alert the notification service extension
/// decrypts and shows, a silent wake of the app that shows nothing, or a new icon badge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushType {
    #[default]
    Alert,
    Background,
    Badge,
}

/// What the Mac asks of APNs besides the envelope. The collapse id is an opaque digest and the
/// badge a count: APNs and the relay read both in the clear.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Delivery {
    pub push_type: PushType,
    pub collapse_id: Option<String>,
    pub badge: Option<u32>,
    pub urgent: Option<bool>,
}

/// Far above any real queue; keeps a forged count from reaching the icon.
pub const MAX_BADGE: u32 = 99_999;

impl Delivery {
    /// A badge push carries its count and nothing else; the other types carry none.
    pub fn is_valid(&self) -> bool {
        let collapse_ok = self.collapse_id.as_deref().is_none_or(valid_collapse_id);
        match self.push_type {
            PushType::Badge => self.collapse_id.is_none() && self.badge.is_some_and(|badge| badge <= MAX_BADGE),
            PushType::Alert | PushType::Background => collapse_ok && self.badge.is_none(),
        }
    }
}

/// Apple caps `apns-collapse-id` at 64 bytes; only base64url keeps it a safe header value.
pub fn valid_collapse_id(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Exact payload submitted to APNs. The caller must enforce its serialized byte limit.
pub fn payload(envelope: &Envelope, binding: &str) -> Value {
    json!({
        "aps": {"alert": {"title": "Alondra", "body": "New activity in Alondra", "loc-key": "New activity in Alondra"}, "mutable-content": 1},
        "alondra": {"envelope": envelope, "push_binding": binding}
    })
}

/// A background push carries only `content-available`: an alert, badge or sound would make
/// APNs treat it as an alert.
pub fn background_payload(envelope: &Envelope, binding: &str) -> Value {
    json!({
        "aps": {"content-available": 1},
        "alondra": {"envelope": envelope, "push_binding": binding}
    })
}

/// The system sets the icon badge from `aps` itself, even for an app the user force-quit, and
/// shows nothing. The notification service extension never runs for it, so the count travels
/// in the clear and the envelope, already checked, stays with the relay.
pub fn badge_payload(badge: u32) -> Value {
    json!({"aps": {"badge": badge}})
}

pub fn payload_for(delivery: &Delivery, envelope: &Envelope, binding: &str) -> Value {
    match delivery.push_type {
        PushType::Alert => payload(envelope, binding),
        PushType::Background => background_payload(envelope, binding),
        PushType::Badge => badge_payload(delivery.badge.unwrap_or_default()),
    }
}

/// Apple: a background push must use priority 5; 10 is an error. A badge only push is an alert
/// that does not need the user right away.
fn headers_for(delivery: &Delivery) -> [(&'static str, &'static str); 2] {
    match delivery.push_type {
        PushType::Alert => [("apns-push-type", "alert"), ("apns-priority", if delivery.urgent == Some(false) { "5" } else { "10" })],
        PushType::Background => [("apns-push-type", "background"), ("apns-priority", "5")],
        PushType::Badge => [("apns-push-type", "alert"), ("apns-priority", "5")],
    }
}

/// APNs keeps only one push per app while the phone is offline. A badge push is delivered once
/// or never, so it can never take the place of an alert still waiting there.
fn expiration_for(delivery: &Delivery, envelope: &Envelope) -> String {
    match delivery.push_type {
        PushType::Badge => "0".into(),
        PushType::Alert | PushType::Background => (envelope.expires_at_ms / 1000).to_string(),
    }
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
            counters: Default::default(),
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
            counters: Default::default(),
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

    pub async fn send(
        &self,
        reg: &PushRegistration,
        envelope: &Envelope,
        delivery: &Delivery,
    ) -> Result<()> {
        let body = serde_json::to_vec(&payload_for(delivery, envelope, &reg.push_binding))?;
        let mut headers = vec![
            ("apns-topic", self.topic.clone()),
            ("apns-expiration", expiration_for(delivery, envelope)),
        ];
        headers.extend(headers_for(delivery).map(|(name, value)| (name, value.to_owned())));
        if let Some(collapse_id) = &delivery.collapse_id {
            headers.push(("apns-collapse-id", collapse_id.clone()));
        }
        self.post(&reg.token, &reg.environment, headers, body).await
    }

    /// Starts, updates or ends a Live Activity. Apple wants the app's topic with the
    /// `.push-type.liveactivity` suffix and a priority of 5 or 10.
    pub async fn send_live_activity(
        &self,
        token: &str,
        environment: &str,
        payload: &Value,
        priority: u8,
        expires_at_s: u64,
    ) -> Result<()> {
        if !matches!(priority, 5 | 10) {
            bail!("a Live Activity push has priority 5 or 10");
        }
        let headers = vec![
            ("apns-topic", live_activity_topic(&self.topic)),
            ("apns-push-type", "liveactivity".to_owned()),
            ("apns-priority", priority.to_string()),
            ("apns-expiration", expires_at_s.to_string()),
        ];
        self.post(token, environment, headers, serde_json::to_vec(payload)?).await
    }

    async fn post(
        &self,
        token: &str,
        environment: &str,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
    ) -> Result<()> {
        if body.len() > APNS_MAX_BYTES {
            bail!("APNs payload exceeds 4096 bytes");
        }
        let host = if environment == "sandbox" {
            "api.sandbox.push.apple.com"
        } else {
            "api.push.apple.com"
        };
        let url = format!("https://{host}/3/device/{token}");
        #[cfg(test)]
        let url = self
            .test_endpoint
            .as_ref()
            .map(|base| format!("{base}/3/device/{token}"))
            .unwrap_or(url);
        let mut bearer = self.bearer()?;
        for attempt in 0..2 {
            let mut request = self.client.post(&url)
                .header("authorization", format!("bearer {bearer}"))
                .header("content-type", "application/json");
            for (name, value) in &headers {
                request = request.header(*name, value);
            }
            let response = request.body(body.clone()).send().await
                .map_err(reqwest::Error::without_url)?;
            let status = response.status();
            let apns_id = response.headers().get("apns-id")
                .and_then(|value| value.to_str().ok()).and_then(log_safe);
            if status.is_success() {
                self.counters.lock().unwrap().accepted += 1;
                eprintln!("alondra-relay: APNs accepted apns-id={}", apns_id.as_deref().unwrap_or("none"));
                return Ok(());
            }
            let body = response.bytes().await.unwrap_or_default();
            let rejection = ApnsRejection::new(status.as_u16(), apns_id.as_deref(), &body);
            let expired = rejection.status == 403 && rejection.reason.as_deref() == Some("ExpiredProviderToken");
            {
                let mut counters = self.counters.lock().unwrap();
                if expired {
                    counters.provider_token_expired += 1;
                } else if rejection.registration_is_dead() {
                    counters.token_expired += 1;
                } else if rejection.is_retryable() {
                    counters.retryable += 1;
                } else {
                    *counters.rejected.entry(rejection.reason.clone().unwrap_or_else(|| "unknown".into())).or_default() += 1;
                }
            }
            if expired && attempt == 0 {
                let mut cached = self.provider_token.lock().unwrap();
                // Concurrent rejections of the same token reuse its replacement.
                if let Some((_, current)) = cached.as_ref() && current != &bearer {
                    bearer = current.clone();
                } else {
                    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
                    bearer = self.sign_provider_token(now)?;
                    *cached = Some((now, bearer.clone()));
                }
                continue;
            }
            return Err(rejection.into());
        }
        Ok(())
    }
}

pub fn live_activity_topic(topic: &str) -> String {
    format!("{topic}.push-type.liveactivity")
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

    pub fn is_retryable(&self) -> bool {
        self.status == 429 || (500..600).contains(&self.status)
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

    #[tokio::test]
    async fn apns_outcomes_refresh_once_and_count_every_response() {
        use axum::{Router, Json, routing::post, http::{HeaderMap, StatusCode}, response::IntoResponse};
        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured = seen.clone();
        let app = Router::new().route("/3/device/{token}", post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let captured = captured.clone();
            async move {
                let mut seen = captured.lock().unwrap();
                seen.push(headers["authorization"].to_str().unwrap().into());
                let status = body["status"].as_u64().unwrap() as u16;
                if status == 403 && seen.len() == 1 || status == 401 {
                    (StatusCode::FORBIDDEN, Json(json!({"reason":"ExpiredProviderToken"}))).into_response()
                } else if status == 403 {
                    (StatusCode::OK, [("apns-id", "accepted-id")]).into_response()
                } else {
                    (StatusCode::from_u16(status).unwrap(), Json(json!({"reason":"TestReason"}))).into_response()
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let apns = Apns::test(format!("http://{}", listener.local_addr().unwrap()));
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        apns.send_live_activity("token", "sandbox", &json!({"status":403}), 5, 0).await.unwrap();
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 2);
            assert_ne!(seen[0], seen[1], "expired JWT must be replaced");
        }
        for status in [429, 500, 503, 400, 401, 410] {
            let failure = apns.send_live_activity("token", "sandbox", &json!({"status":status}), 5, 0).await.unwrap_err();
            let response = crate::apns_failure_response(&failure);
            if matches!(status, 429 | 500 | 503) {
                assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(response.headers()["retry-after"], "30");
                let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
                assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["error"], "apns_retry_later");
            } else if status == 410 {
                assert_eq!(response.status(), StatusCode::GONE);
            } else {
                assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            }
        }
        assert_eq!(seen.lock().unwrap().len(), 9, "at most one refresh retry per send");
        let counters = apns.counters.lock().unwrap().clone();
        assert_eq!(counters.accepted, 1);
        assert_eq!(counters.provider_token_expired, 3);
        assert_eq!(counters.token_expired, 1);
        assert_eq!(counters.retryable, 3);
        assert_eq!(counters.rejected.get("TestReason"), Some(&1));
        task.abort();
    }

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
    fn encrypted_envelope_matches_the_fixture_with_a_localizable_fallback() {
        let mut fixture: Value =
            serde_json::from_str(include_str!("../../../fixtures/push/encrypted.json")).unwrap();
        fixture["apns"]["aps"]["alert"] = json!({"title":"Alondra", "body":"New activity in Alondra", "loc-key":"New activity in Alondra"});
        let alondra = &fixture["apns"]["alondra"];
        let envelope: Envelope = serde_json::from_value(alondra["envelope"].clone()).unwrap();
        assert_eq!(
            payload(&envelope, alondra["push_binding"].as_str().unwrap()),
            fixture["apns"],
            "the notification service extension decrypts exactly this shape"
        );
    }

    #[test]
    fn background_payload_matches_the_retract_fixture() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../../fixtures/push/retract.json")).unwrap();
        let alondra = &fixture["apns"]["alondra"];
        let envelope: Envelope = serde_json::from_value(alondra["envelope"].clone()).unwrap();
        let delivery = Delivery { push_type: PushType::Background, ..Delivery::default() };
        assert_eq!(
            payload_for(&delivery, &envelope, alondra["push_binding"].as_str().unwrap()),
            fixture["apns"],
            "the app opens exactly this shape when it wakes in the background"
        );
        assert_eq!(headers_for(&delivery), [("apns-push-type", "background"), ("apns-priority", "5")]);
    }

    #[test]
    fn a_badge_push_shows_only_the_count_and_is_never_stored_in_place_of_an_alert() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../../fixtures/push/retract.json")).unwrap();
        let envelope: Envelope =
            serde_json::from_value(fixture["apns"]["alondra"]["envelope"].clone()).unwrap();
        let delivery = Delivery { push_type: PushType::Badge, badge: Some(3), ..Delivery::default() };
        assert_eq!(payload_for(&delivery, &envelope, "binding"), json!({"aps": {"badge": 3}}));
        assert_eq!(headers_for(&delivery), [("apns-push-type", "alert"), ("apns-priority", "5")]);
        assert_eq!(expiration_for(&delivery, &envelope), "0");
        let alert = Delivery::default();
        assert_eq!(expiration_for(&alert, &envelope), (envelope.expires_at_ms / 1000).to_string());
    }

    #[test]
    fn only_a_badge_push_carries_a_count_and_it_carries_nothing_else() {
        let badge = |push_type, badge, collapse_id: Option<&str>| Delivery {
            push_type, badge, collapse_id: collapse_id.map(str::to_owned), urgent: None,
        };
        assert!(badge(PushType::Badge, Some(0), None).is_valid());
        assert!(badge(PushType::Badge, Some(MAX_BADGE), None).is_valid());
        assert!(!badge(PushType::Badge, None, None).is_valid());
        assert!(!badge(PushType::Badge, Some(MAX_BADGE + 1), None).is_valid());
        assert!(!badge(PushType::Badge, Some(1), Some("abc")).is_valid());
        assert!(!badge(PushType::Alert, Some(1), None).is_valid());
        assert!(!badge(PushType::Background, Some(1), None).is_valid());
        assert!(badge(PushType::Alert, None, Some("abc")).is_valid());
        assert!(!badge(PushType::Alert, None, Some("card id")).is_valid());
    }

    #[test]
    fn collapse_ids_are_short_header_safe_tokens() {
        assert!(valid_collapse_id(&"A-_9".repeat(16)));
        for bad in ["", &"a".repeat(65), "card id", "card\nforged", "card/1", "café"] {
            assert!(!valid_collapse_id(bad), "{bad:?}");
        }
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
