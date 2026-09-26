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
        let client = reqwest::Client::builder()
            .timeout(APNS_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            team_id: config.team_id,
            key_id: config.key_id,
            topic: config.topic,
            private_key,
            client,
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
            client: reqwest::Client::new(),
            test_endpoint: Some(endpoint),
        }
    }

    fn bearer(&self) -> Result<String> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
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
            .await?;
        if !response.status().is_success() {
            bail!(
                "APNs rejected encrypted push with HTTP {}",
                response.status()
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
