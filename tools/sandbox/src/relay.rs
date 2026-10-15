use std::path::Path;

use alondra_e2e::sign_http_challenge;
use anyhow::{Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use p256::ecdsa::SigningKey;
use serde_json::{Value, json};

use crate::{
    issuer::{AUDIENCE, OWNER},
    sandbox::{http, now_ms, random},
};

pub struct Relay {
    origin: String,
}

pub struct Signer<'a> {
    pub id: &'a str,
    pub key: &'a SigningKey,
}

impl Relay {
    pub fn new(url: &str) -> Self {
        Self { origin: url.trim_end_matches('/').into() }
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn epoch(&self) -> Result<[u8; 16]> {
        let mut response = http().get(format!("{}/v1/bootstrap", self.origin)).call()?;
        if response.status().as_u16() != 200 {
            bail!("relay bootstrap failed ({})", response.status());
        }
        let boot: Value = response.body_mut().read_json()?;
        B64.decode(boot["epoch"].as_str().unwrap_or_default())?
            .try_into()
            .map_err(|_| anyhow!("invalid relay epoch"))
    }

    pub fn call(
        &self,
        signer: &Signer,
        token: Option<&str>,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<(u16, Vec<u8>)> {
        let epoch = self.epoch()?;
        let headers = signed_headers(signer, &epoch, method, path, body)?;
        let url = format!("{}{path}", self.origin);
        let agent = http();
        let mut request = match method {
            "GET" => agent.get(&url).force_send_body(),
            "POST" => agent.post(&url),
            "DELETE" => agent.delete(&url).force_send_body(),
            other => bail!("unsupported relay method {other}"),
        };
        for (name, value) in headers {
            request = request.header(name, value);
        }
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let mut response = request.header("content-type", "application/json").send(body)?;
        Ok((response.status().as_u16(), response.body_mut().read_to_vec()?))
    }
}

pub fn signed_headers(
    signer: &Signer,
    epoch: &[u8; 16],
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<Vec<(&'static str, String)>> {
    let nonce = random::<16>()?;
    let timestamp = now_ms();
    let signature = sign_http_challenge(signer.key, signer.id, method, path, epoch, body, timestamp, &nonce)
        .map_err(|error| anyhow!("sign {method} {path}: {error}"))?;
    Ok(vec![
        ("x-device-id", signer.id.into()),
        ("x-relay-epoch", B64.encode(epoch)),
        ("x-device-timestamp-ms", timestamp.to_string()),
        ("x-device-nonce", B64.encode(nonce)),
        ("x-device-signature", signature),
    ])
}

/// The relay needs one bootstrap Mac in its config; the sandbox Macs enroll through
/// `POST /v1/macs` like real ones, so this one is a placeholder nobody holds.
pub fn write_config(listen: &str, issuer: &str, registry: &Path, out: &Path) -> Result<()> {
    let placeholder = loop {
        if let Ok(key) = SigningKey::from_slice(&random::<32>()?) {
            break key;
        }
    };
    let config = json!({
        "listen": listen,
        "issuer": issuer,
        "audience": AUDIENCE,
        "jwks_url": format!("{}/jwks.json", issuer.trim_end_matches('/')),
        "owner_sub": OWNER,
        "mac_id": "mac-sandbox-bootstrap",
        "mac_signing_key": B64.encode(placeholder.verifying_key().to_sec1_point(false).as_bytes()),
        "device_registry_path": registry,
    });
    std::fs::write(out, serde_json::to_vec_pretty(&config)?)?;
    Ok(())
}
