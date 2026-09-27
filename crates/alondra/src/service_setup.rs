//! Source-free Mac enrollment. User OIDC authorizes only a new route; phone pairing still
//! requires the Mac-local key and cannot be granted by an account administrator.
mod recovery;
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

#[derive(Clone, Serialize, Deserialize)]
struct PublicMac {
    mac_id: String,
    signing_key: String,
    agreement_key: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct ServiceConfig {
    relay_url: String,
    mac_id: String,
    owner_sub: String,
    signing_key_path: String,
    agreement_key_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    journal_path: Option<String>,
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
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
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
    fs::create_dir_all(home)?;
    let dir_exists = match fs::symlink_metadata(&dir) {
        Ok(dir_meta) => {
            if dir_meta.file_type().is_symlink() || !dir_meta.is_dir() {
                bail!("service key directory must be a real directory");
            }
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    let signing_path = dir.join(SIGNING_FILE);
    let agreement_path = dir.join(AGREEMENT_FILE);
    let public_path = dir.join("public.json");
    let files = [
        signing_path.exists(),
        agreement_path.exists(),
        public_path.exists(),
    ];
    if dir_exists && !files.iter().all(|exists| *exists) {
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
    let identity = PublicMac {
        mac_id,
        signing_key,
        agreement_key,
    };
    // A crash before rename leaves only an unused staging directory. The visible
    // identity is either complete or absent, so retry can generate a fresh one.
    let stage = home.join(format!(
        ".service-keys-staging-{}",
        URL_SAFE_NO_PAD.encode(random::<16>()?)
    ));
    fs::create_dir(&stage)?;
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o700))?;
    write_private(&stage.join(SIGNING_FILE), &signing)?;
    write_private(&stage.join(AGREEMENT_FILE), &agreement)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(stage.join("public.json"))?;
    file.write_all(&serde_json::to_vec_pretty(&identity)?)?;
    file.sync_all()?;
    fs::File::open(&stage)?.sync_all()?;
    if fs::symlink_metadata(&dir).is_ok() {
        bail!("service key directory appeared during setup; retry after inspecting it");
    }
    fs::rename(&stage, &dir)?;
    fs::File::open(home)?.sync_all()?;
    Ok((identity, SigningKey::from_slice(&signing)?))
}

fn http() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .build(),
        )
        .timeout_global(Some(Duration::from_secs(15)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into()
}

fn register(identity: &PublicMac, signing: &SigningKey, token: &str) -> Result<()> {
    register_at(identity, signing, token, RELAY)
}

// Explicit function argument enables isolated test transport; production callers
// always pass the fixed service origin above. No runtime URL/token override.
fn register_at(identity: &PublicMac, signing: &SigningKey, token: &str, relay: &str) -> Result<()> {
    let http = http();
    let mut bootstrap = http.get(format!("{relay}/v1/bootstrap")).call()?;
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
        .post(format!("{relay}/v1/macs"))
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

fn service_file(home: &Path, identity: &PublicMac, owner_sub: &str, write: bool) -> Result<bool> {
    service_file_at(home, identity, owner_sub, write, RELAY)
}
fn service_file_at(home: &Path, identity: &PublicMac, owner_sub: &str, write: bool, relay: &str) -> Result<bool> {
    let path = home.join(SERVICE_FILE);
    let base = identity_base(home)?;
    let prefix = base.strip_prefix(home)?.to_string_lossy();
    let prefix = if prefix.is_empty() { String::new() } else { format!("{prefix}/") };
    let expected = ServiceConfig {
        relay_url: relay.into(),
        mac_id: identity.mac_id.clone(),
        owner_sub: owner_sub.into(),
        signing_key_path: format!("{prefix}service-keys/{SIGNING_FILE}"),
        agreement_key_path: format!("{prefix}service-keys/{AGREEMENT_FILE}"),
        journal_path: if prefix.is_empty() { None } else { Some(format!("{prefix}service-receipts.sqlite")) },
        devices: vec![],
    };
    let existing_path = match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => true,
        Ok(_) => bail!("service.toml must be a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if existing_path {
        let existing: ServiceConfig = toml::from_str(&fs::read_to_string(&path)?)?;
        if existing.mac_id != expected.mac_id
            || existing.owner_sub != expected.owner_sub
            || existing.relay_url.trim_end_matches('/') != relay
            || existing.signing_key_path != expected.signing_key_path
            || existing.agreement_key_path != expected.agreement_key_path
            || existing.journal_path != expected.journal_path
        {
            bail!("existing service.toml belongs to another Mac, account or key path");
        }
        return Ok(false);
    }
    if !write {
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
pub fn enroll(home: &Path, no_input: bool) -> Result<bool> {
    let _lock = alondra_core::home_lock::HomeLock::shared(home)?;
    alondra_core::home_lock::ensure_ready(home)?;
    let account = crate::account::token_for_setup(no_input)?;
    let (identity, signing) = load_or_create_keys(&identity_base(home)?)?;
    // Validate every field of any existing configuration before changing relay state.
    service_file(home, &identity, &account.sub, false)?;
    println!(
        "Registering Mac {} with the Alondra service…",
        identity.mac_id
    );
    register(&identity, &signing, &account.access_token)
        .context("registering this Mac with the Alondra service")?;
    let created = service_file(home, &identity, &account.sub, true)?;
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

    #[test]
    fn interrupted_staging_does_not_block_a_new_complete_identity() {
        let home = std::env::temp_dir().join(format!(
            "alondra-service-staging-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(home.join(".service-keys-staging-interrupted")).unwrap();
        fs::write(
            home.join(".service-keys-staging-interrupted/signing.key"),
            "partial",
        )
        .unwrap();
        let (identity, _) = load_or_create_keys(&home).unwrap();
        assert!(identity.mac_id.starts_with("mac-"));
        assert!(home.join("service-keys/agreement.key").is_file());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn existing_config_is_checked_before_enrollment() {
        let home = std::env::temp_dir().join(format!(
            "alondra-service-config-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&home).unwrap();
        let (identity, _) = load_or_create_keys(&home).unwrap();
        assert!(!service_file(&home, &identity, "owner", false).unwrap());
        assert!(service_file(&home, &identity, "owner", true).unwrap());
        assert!(!service_file(&home, &identity, "owner", false).unwrap());
        let path = home.join(SERVICE_FILE);
        let changed = fs::read_to_string(&path)
            .unwrap()
            .replace("service-keys/signing.key", "old-signing.key");
        fs::write(&path, changed).unwrap();
        assert!(service_file(&home, &identity, "owner", false).is_err());
        fs::remove_dir_all(home).unwrap();
    }
}

fn identity_base(home: &Path) -> Result<std::path::PathBuf> {
    let path = home.join(SERVICE_FILE);
    if !path.try_exists()? { return Ok(home.to_owned()); }
    let config: ServiceConfig = toml::from_str(&fs::read_to_string(path)?)?;
    if config.signing_key_path == "service-keys/signing.key" && config.agreement_key_path == "service-keys/agreement.key" && config.journal_path.is_none() {
        return Ok(home.to_owned());
    }
    let pieces: Vec<_> = config.signing_key_path.split('/').collect();
    anyhow::ensure!(pieces.len() == 4 && pieces[0] == "service-generations" && recovery::valid_id(pieces[1])
        && pieces[2] == "service-keys" && pieces[3] == SIGNING_FILE, "unrecognized service key generation");
    let prefix = format!("service-generations/{}", pieces[1]);
    anyhow::ensure!(config.agreement_key_path == format!("{prefix}/service-keys/{AGREEMENT_FILE}")
        && config.journal_path.as_deref() == Some(format!("{prefix}/service-receipts.sqlite").as_str()), "mixed service generation");
    let base = home.join(prefix);
    anyhow::ensure!(base.canonicalize()?.starts_with(home.canonicalize()?), "service generation escapes HOME");
    Ok(base)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryOutcome { Cancelled, Committed }

pub fn recover(home: &Path, stop: impl FnOnce() -> Result<()>, input: &mut impl std::io::BufRead, output: &mut impl Write) -> Result<RecoveryOutcome> {
    recovery::interactive(home, stop, input, output)
}
