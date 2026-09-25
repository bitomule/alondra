//! Source-free Mac enrollment. User OIDC authorizes only a new route; phone pairing still
//! requires the Mac-local key and cannot be granted by an account administrator.
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use alondra_e2e::sign_http_challenge;
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::{SecretKey, ecdsa::SigningKey, elliptic_curve::sec1::ToSec1Point};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

const RELAY: &str = "https://alondra-api.davidcollado.dev";
const SERVICE_FILE: &str = "service.toml";
const SIGNING_FILE: &str = "signing.key";
const AGREEMENT_FILE: &str = "agreement.key";

#[derive(Serialize, Deserialize)]
struct PublicMac {
    mac_id: String,
    signing_key: String,
    agreement_key: String,
}

#[derive(Serialize, Deserialize)]
struct ServiceConfig {
    relay_url: String,
    mac_id: String,
    owner_sub: String,
    signing_key_path: String,
    agreement_key_path: String,
    devices: Vec<toml::Value>,
}

#[derive(Deserialize)]
struct Bootstrap {
    epoch: String,
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut value = [0u8; N];
    SystemRandom::new()
        .fill(&mut value)
        .map_err(|_| anyhow!("OS randomness failed"))?;
    Ok(value)
}

fn read_private(path: &Path) -> Result<[u8; 32]> {
    if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
        bail!(
            "private key file has unsafe permissions: {}",
            path.display()
        );
    }
    let bytes = URL_SAFE_NO_PAD.decode(fs::read_to_string(path)?.trim())?;
    bytes
        .try_into()
        .map_err(|_| anyhow!("invalid private key length"))
}

fn write_private(path: &Path, bytes: &[u8; 32]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(URL_SAFE_NO_PAD.encode(bytes).as_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn public_keys(signing: &[u8; 32], agreement: &[u8; 32]) -> Result<(String, String)> {
    let signing = SigningKey::from_slice(signing)?;
    let agreement = SecretKey::from_slice(agreement)?;
    Ok((
        URL_SAFE_NO_PAD.encode(signing.verifying_key().to_sec1_point(false).as_bytes()),
        URL_SAFE_NO_PAD.encode(agreement.public_key().to_sec1_point(false).as_bytes()),
    ))
}

fn load_or_create_keys(home: &Path) -> Result<(PublicMac, SigningKey)> {
    let dir = home.join("service-keys");
    fs::create_dir_all(&dir)?;
    let dir_meta = fs::symlink_metadata(&dir)?;
    if dir_meta.file_type().is_symlink() || !dir_meta.is_dir() {
        bail!("service key directory must be a real directory");
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    let signing_path = dir.join(SIGNING_FILE);
    let agreement_path = dir.join(AGREEMENT_FILE);
    let public_path = dir.join("public.json");
    let files = [
        signing_path.exists(),
        agreement_path.exists(),
        public_path.exists(),
    ];
    if files.iter().any(|exists| *exists) && !files.iter().all(|exists| *exists) {
        bail!("incomplete service key set; refusing to replace an existing Mac identity");
    }
    if files.iter().all(|exists| *exists) {
        let signing = read_private(&signing_path)?;
        let agreement = read_private(&agreement_path)?;
        let (signing_key, agreement_key) = public_keys(&signing, &agreement)?;
        let identity: PublicMac = serde_json::from_slice(&fs::read(public_path)?)?;
        if identity.signing_key != signing_key
            || identity.agreement_key != agreement_key
            || !identity.mac_id.starts_with("mac-")
        {
            bail!("service public identity does not match local private keys");
        }
        return Ok((identity, SigningKey::from_slice(&signing)?));
    }
    let signing = loop {
        let candidate = random::<32>()?;
        if SigningKey::from_slice(&candidate).is_ok() {
            break candidate;
        }
    };
    let agreement = loop {
        let candidate = random::<32>()?;
        if SecretKey::from_slice(&candidate).is_ok() {
            break candidate;
        }
    };
    let (signing_key, agreement_key) = public_keys(&signing, &agreement)?;
    let mac_id = format!(
        "mac-{}",
        random::<16>()?
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    write_private(&signing_path, &signing)?;
    write_private(&agreement_path, &agreement)?;
    let identity = PublicMac {
        mac_id,
        signing_key,
        agreement_key,
    };
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(public_path)?;
    file.write_all(&serde_json::to_vec_pretty(&identity)?)?;
    file.sync_all()?;
    Ok((identity, SigningKey::from_slice(&signing)?))
}

fn http() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into()
}

fn register(identity: &PublicMac, signing: &SigningKey, token: &str) -> Result<()> {
    let http = http();
    let mut bootstrap = http.get(format!("{RELAY}/v1/bootstrap")).call()?;
    if bootstrap.status().as_u16() != 200 {
        bail!("relay bootstrap failed ({})", bootstrap.status());
    }
    let bootstrap: Bootstrap = bootstrap.body_mut().with_config().limit(4096).read_json()?;
    let epoch: [u8; 16] = URL_SAFE_NO_PAD
        .decode(bootstrap.epoch)?
        .try_into()
        .map_err(|_| anyhow!("invalid relay epoch"))?;
    let body = serde_json::to_vec(identity)?;
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
    let nonce = random::<16>()?;
    let signature = sign_http_challenge(
        signing,
        &identity.mac_id,
        "POST",
        "/v1/macs",
        &epoch,
        &body,
        timestamp,
        &nonce,
    )
    .map_err(|e| anyhow!("sign Mac enrollment: {e}"))?;
    let response = http
        .post(format!("{RELAY}/v1/macs"))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .header("x-device-id", &identity.mac_id)
        .header("x-relay-epoch", URL_SAFE_NO_PAD.encode(epoch))
        .header("x-device-timestamp-ms", timestamp.to_string())
        .header("x-device-nonce", URL_SAFE_NO_PAD.encode(nonce))
        .header("x-device-signature", signature)
        .send(&body[..])?;
    match response.status().as_u16() {
        200 | 201 => Ok(()),
        401 => bail!(
            "Mac enrollment refused the account token or signed proof; run `alondra login` and retry"
        ),
        409 => bail!(
            "Mac identity already belongs to another key or account; explicit recovery is required"
        ),
        status => bail!("Mac enrollment failed ({status})"),
    }
}

fn service_file(home: &Path, identity: &PublicMac, owner_sub: &str) -> Result<bool> {
    let path = home.join(SERVICE_FILE);
    let expected = ServiceConfig {
        relay_url: RELAY.into(),
        mac_id: identity.mac_id.clone(),
        owner_sub: owner_sub.into(),
        signing_key_path: format!("service-keys/{SIGNING_FILE}"),
        agreement_key_path: format!("service-keys/{AGREEMENT_FILE}"),
        devices: vec![],
    };
    if path.exists() {
        let existing: ServiceConfig = toml::from_str(&fs::read_to_string(&path)?)?;
        if existing.mac_id != expected.mac_id
            || existing.owner_sub != expected.owner_sub
            || existing.relay_url.trim_end_matches('/') != RELAY
            || existing.signing_key_path != expected.signing_key_path
            || existing.agreement_key_path != expected.agreement_key_path
        {
            bail!("existing service.toml belongs to another Mac, account or key path");
        }
        return Ok(false);
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(toml::to_string_pretty(&expected)?.as_bytes())?;
    file.sync_all()?;
    Ok(true)
}

/// Returns whether service.toml was newly created and an existing daemon must restart.
pub fn enroll(home: &Path) -> Result<bool> {
    let account = crate::account::ensure_token_for_enrollment()?;
    let (identity, signing) = load_or_create_keys(home)?;
    let existing = home.join(SERVICE_FILE);
    if existing.exists() {
        // Check the local account binding before sending a network request.
        let config: ServiceConfig = toml::from_str(&fs::read_to_string(&existing)?)?;
        if config.owner_sub != account.sub || config.mac_id != identity.mac_id {
            bail!("this Mac is already enrolled for a different account or identity");
        }
    }
    println!(
        "Registering Mac {} with the Alondra service…",
        identity.mac_id
    );
    register(&identity, &signing, &account.access_token)
        .context("registering this Mac with the Alondra service")?;
    let created = service_file(home, &identity, &account.sub)?;
    println!("Mac {} enrolled in Alondra service.", identity.mac_id);
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_public_keys_match_private_files_and_survive_reopen() {
        let home =
            std::env::temp_dir().join(format!("alondra-service-keys-test-{}", std::process::id()));
        fs::create_dir_all(&home).unwrap();
        let (first, _) = load_or_create_keys(&home).unwrap();
        let (second, _) = load_or_create_keys(&home).unwrap();
        assert_eq!(first.mac_id, second.mac_id);
        assert_eq!(first.signing_key, second.signing_key);
        assert_eq!(first.agreement_key, second.agreement_key);
        assert!(
            fs::metadata(home.join("service-keys/signing.key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077
                == 0
        );
        fs::remove_dir_all(home).unwrap();
    }
}
