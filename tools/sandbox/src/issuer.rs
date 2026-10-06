//! The sandbox's stand-in for the account login: RS256 tokens the local relay trusts.
use std::path::Path;

use anyhow::{Result, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ring::{
    rand::SystemRandom,
    signature::{RSA_PKCS1_SHA256, RsaKeyPair, RsaPublicKeyComponents},
};
use serde_json::json;

use crate::sandbox::now_ms;

pub const AUDIENCE: &str = "alondra-sandbox";
pub const OWNER: &str = "sandbox-owner";
const KID: &str = "sandbox";

pub struct Issuer {
    key: RsaKeyPair,
    url: String,
}

impl Issuer {
    pub fn open(key: &Path, url: &str) -> Result<Self> {
        let key = load(key)?;
        Ok(Self { key, url: url.into() })
    }

    pub fn token(&self) -> Result<String> {
        let now = now_ms() / 1000;
        let header = B64.encode(serde_json::to_vec(&json!({"alg":"RS256","kid":KID,"typ":"JWT"}))?);
        let claims = B64.encode(serde_json::to_vec(&json!({
            "iss": self.url, "sub": OWNER, "aud": AUDIENCE, "iat": now, "exp": now + 3600
        }))?);
        let message = format!("{header}.{claims}");
        let mut signature = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), message.as_bytes(), &mut signature)
            .map_err(|_| anyhow!("sign sandbox token"))?;
        Ok(format!("{message}.{}", B64.encode(signature)))
    }
}

/// `openssl genpkey -outform DER` writes PKCS#8 or PKCS#1 depending on its version.
fn load(path: &Path) -> Result<RsaKeyPair> {
    let der = std::fs::read(path)?;
    RsaKeyPair::from_pkcs8(&der)
        .or_else(|_| RsaKeyPair::from_der(&der))
        .map_err(|error| anyhow!("sandbox issuer key {}: {error}", path.display()))
}

pub fn write_jwks(key: &Path, out: &Path) -> Result<()> {
    let key = load(key)?;
    let public: RsaPublicKeyComponents<Vec<u8>> = key.public().into();
    let jwks = json!({"keys": [{
        "kid": KID, "kty": "RSA", "use": "sig", "alg": "RS256",
        "n": B64.encode(public.n), "e": B64.encode(public.e)
    }]});
    std::fs::write(out, serde_json::to_vec_pretty(&jwks)?)?;
    Ok(())
}
