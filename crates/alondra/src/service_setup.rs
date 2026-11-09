//! Source-free Mac enrollment. Every Mac gets its service keys and a `service.toml` without an
//! account, so devices can pair with it on the local network. Enrolling with the relay is a
//! separate, explicit step: user OIDC authorizes only a new route; phone pairing still
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

const RELAY: &str = alondra_core::service_config::RELAY_ORIGIN;
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

/// The fields every `service.toml` has, with or without a relay.
#[derive(Deserialize)]
struct KeyPaths {
    mac_id: String,
    signing_key_path: String,
    agreement_key_path: String,
    #[serde(default)]
    journal_path: Option<String>,
    #[serde(default)]
    relay_url: Option<String>,
}

/// A Mac without a relay: devices reach it only on the local network.
#[derive(Serialize)]
struct LocalServiceConfig {
    mac_id: String,
    signing_key_path: String,
    agreement_key_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    journal_path: Option<String>,
    devices: Vec<toml::Value>,
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
        let shown = path.display();
        bail!(trf!("private key file has unsafe permissions: {shown}", "el archivo de clave privada tiene permisos inseguros: {shown}"));
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
                bail!(tr!("service key directory must be a real directory", "el directorio de claves del servicio debe ser un directorio real"));
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
        bail!(tr!("incomplete service key set; refusing to replace an existing Mac identity", "conjunto de claves del servicio incompleto; no se sustituirá la identidad existente del Mac"));
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
            bail!(tr!("service public identity does not match local private keys", "la identidad pública del servicio no coincide con las claves privadas locales"));
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
        bail!(tr!("service key directory appeared during setup; retry after inspecting it", "el directorio de claves del servicio apareció durante la configuración; revísalo y reinténtalo"));
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

struct MacProof {
    epoch: [u8; 16],
    timestamp: u64,
    nonce: [u8; 16],
    signature: String,
}

impl MacProof {
    fn new(http: &ureq::Agent, relay: &str, identity: &PublicMac, signing: &SigningKey, method: &str, path: &str, body: &[u8]) -> Result<Self> {
        let mut bootstrap = http.get(format!("{relay}/v1/bootstrap")).call()?;
        if bootstrap.status().as_u16() != 200 {
            let status = bootstrap.status();
            bail!(trf!("relay bootstrap failed ({status})", "falló el arranque del relay ({status})"));
        }
        let bootstrap: Bootstrap = bootstrap.body_mut().with_config().limit(4096).read_json()?;
        let epoch: [u8; 16] = URL_SAFE_NO_PAD
            .decode(bootstrap.epoch)?
            .try_into()
            .map_err(|_| anyhow!("invalid relay epoch"))?;
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
        let nonce = random::<16>()?;
        let signature = sign_http_challenge(signing, &identity.mac_id, method, path, &epoch, body, timestamp, &nonce)
            .map_err(|e| anyhow!("sign Mac request: {e}"))?;
        Ok(Self { epoch, timestamp, nonce, signature })
    }

    fn apply<B>(&self, mac_id: &str, request: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        request
            .header("x-device-id", mac_id)
            .header("x-relay-epoch", URL_SAFE_NO_PAD.encode(self.epoch))
            .header("x-device-timestamp-ms", self.timestamp.to_string())
            .header("x-device-nonce", URL_SAFE_NO_PAD.encode(self.nonce))
            .header("x-device-signature", &self.signature)
    }
}

// Explicit function argument enables isolated test transport; production callers
// always pass the fixed service origin above. No runtime URL/token override.
fn register_at(identity: &PublicMac, signing: &SigningKey, token: &str, relay: &str) -> Result<()> {
    let http = http();
    let body = serde_json::to_vec(identity)?;
    let proof = MacProof::new(&http, relay, identity, signing, "POST", "/v1/macs", &body)?;
    let response = proof
        .apply(&identity.mac_id, http.post(format!("{relay}/v1/macs")))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .send(&body[..])?;
    match response.status().as_u16() {
        200 | 201 => Ok(()),
        401 => bail!(tr!(
            "Mac enrollment refused the account token or signed proof; run `alondra account login` and retry",
            "el registro del Mac rechazó el token de la cuenta o la prueba firmada; ejecuta `alondra account login` y reinténtalo"
        )),
        409 => bail!(tr!(
            "Mac identity already belongs to another key or account; explicit recovery is required",
            "la identidad del Mac ya pertenece a otra clave o cuenta; hace falta una recuperación explícita"
        )),
        status => bail!(trf!("Mac enrollment failed ({status})", "falló el registro del Mac ({status})")),
    }
}

fn revoke_mac(identity: &PublicMac, signing: &SigningKey, token: &str) -> Result<()> {
    revoke_mac_at(identity, signing, token, RELAY)
}

#[derive(Debug, PartialEq, Eq)]
enum Revocation {
    Done,
    CheckDirectory,
    Failed,
}

/// The relay forgets a revoked Mac, so a repeated request is refused as an unknown device;
/// only the owner's Mac directory tells that apart from a refused signature.
fn revocation(status: u16, error: Option<&str>) -> Revocation {
    match (status, error) {
        (204, _) | (403, Some("account_access_denied")) => Revocation::Done,
        (401, Some("unauthorized")) => Revocation::CheckDirectory,
        _ => Revocation::Failed,
    }
}

fn listed(directory: &serde_json::Value, mac_id: &str) -> Result<bool> {
    let macs = directory["macs"].as_array().context("invalid Mac directory")?;
    Ok(macs.iter().any(|mac| mac["mac_id"].as_str() == Some(mac_id)))
}

/// Signed with the identity being revoked: the relay lets only a Mac revoke itself, which also
/// ends the push registrations of the phones linked to it.
fn revoke_mac_at(identity: &PublicMac, signing: &SigningKey, token: &str, relay: &str) -> Result<()> {
    let http = http();
    let path = format!("/v1/macs/{}", identity.mac_id);
    let proof = MacProof::new(&http, relay, identity, signing, "DELETE", &path, &[])?;
    let mut response = proof.apply(&identity.mac_id, http.delete(format!("{relay}{path}"))).call()?;
    let status = response.status().as_u16();
    let body: Option<serde_json::Value> = response.body_mut().with_config().limit(4096).read_json().ok();
    let error = body.as_ref().and_then(|body| body["error"].as_str());
    match revocation(status, error) {
        Revocation::Done => Ok(()),
        Revocation::Failed => bail!(trf!(
            "the relay did not retire this Mac's previous identity ({status}); run the recovery again",
            "el relay no dio de baja la identidad anterior de este Mac ({status}); vuelve a ejecutar la recuperación"
        )),
        Revocation::CheckDirectory => {
            let mut directory = http
                .get(format!("{relay}/v1/macs"))
                .header("authorization", format!("Bearer {token}"))
                .call()?;
            let status = directory.status().as_u16();
            if status != 200 {
                bail!(trf!(
                    "could not confirm with the relay that this Mac's previous identity is retired ({status}); run the recovery again",
                    "no se pudo confirmar con el relay que la identidad anterior de este Mac está dada de baja ({status}); vuelve a ejecutar la recuperación"
                ));
            }
            let directory: serde_json::Value = directory.body_mut().with_config().limit(65536).read_json()?;
            if listed(&directory, &identity.mac_id)? {
                bail!(tr!(
                    "the relay still accepts this Mac's previous identity and refused its signed retirement; check this Mac's clock and run the recovery again",
                    "el relay aún acepta la identidad anterior de este Mac y rechazó su baja firmada; revisa la hora de este Mac y vuelve a ejecutar la recuperación"
                ));
            }
            Ok(())
        }
    }
}

fn service_file(home: &Path, identity: &PublicMac, owner_sub: &str, write: bool) -> Result<bool> {
    if write {
        alondra_core::service_config::migrate_previous_relay(home)?;
    }
    service_file_at(home, identity, owner_sub, write, RELAY)
}

fn key_paths(home: &Path) -> Result<(String, String, Option<String>)> {
    let base = identity_base(home)?;
    let prefix = base.strip_prefix(home)?.to_string_lossy();
    let prefix = if prefix.is_empty() { String::new() } else { format!("{prefix}/") };
    Ok((
        format!("{prefix}service-keys/{SIGNING_FILE}"),
        format!("{prefix}service-keys/{AGREEMENT_FILE}"),
        if prefix.is_empty() { None } else { Some(format!("{prefix}service-receipts.sqlite")) },
    ))
}

fn existing_paths(path: &Path) -> Result<Option<KeyPaths>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(Some(toml::from_str(&fs::read_to_string(path)?)?)),
        Ok(_) => bail!(tr!("service.toml must be a regular file", "service.toml debe ser un archivo normal")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn check_identity(existing: &KeyPaths, identity: &PublicMac, paths: &(String, String, Option<String>)) -> Result<()> {
    if existing.mac_id != identity.mac_id
        || existing.signing_key_path != paths.0
        || existing.agreement_key_path != paths.1
        || existing.journal_path != paths.2
    {
        bail!(tr!("existing service.toml belongs to another Mac or key path", "el service.toml existente pertenece a otro Mac o a otra ruta de claves"));
    }
    Ok(())
}

/// Creates `service.toml` without a relay, or checks the one that exists (with or without).
fn local_service_file(home: &Path, identity: &PublicMac) -> Result<bool> {
    let path = home.join(SERVICE_FILE);
    let paths = key_paths(home)?;
    if let Some(existing) = existing_paths(&path)? {
        check_identity(&existing, identity, &paths)?;
        return Ok(false);
    }
    let config = LocalServiceConfig {
        mac_id: identity.mac_id.clone(),
        signing_key_path: paths.0,
        agreement_key_path: paths.1,
        journal_path: paths.2,
        devices: vec![],
    };
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    file.write_all(toml::to_string_pretty(&config)?.as_bytes())?;
    file.sync_all()?;
    Ok(true)
}

/// Adds the relay to a `service.toml` that has none, keeping its devices and other settings.
fn add_relay(home: &Path, owner_sub: &str, relay: &str) -> Result<()> {
    let _lock = alondra_core::service_config::ServiceConfigLock::exclusive(home)?;
    let path = home.join(SERVICE_FILE);
    let mut table: toml::Table = toml::from_str(&fs::read_to_string(&path)?)?;
    table.insert("relay_url".into(), relay.into());
    table.insert("owner_sub".into(), owner_sub.into());
    let temp = path.with_extension("toml.relay");
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temp)?;
    file.write_all(toml::to_string_pretty(&table)?.as_bytes())?;
    file.sync_all()?;
    fs::rename(&temp, &path)?;
    Ok(())
}

/// Takes the relay out of `service.toml`, keeping this Mac's identity, keys and devices, so
/// `add_relay` can put it back. Returns whether there was one to take out.
pub fn remove_relay(home: &Path) -> Result<bool> {
    let _home = alondra_core::home_lock::HomeLock::shared(home)?;
    let _lock = alondra_core::service_config::ServiceConfigLock::exclusive(home)?;
    let path = home.join(SERVICE_FILE);
    if existing_paths(&path)?.is_none() {
        return Ok(false);
    }
    let mut table: toml::Table = toml::from_str(&fs::read_to_string(&path)?)?;
    let had = table.remove("relay_url").is_some();
    table.remove("owner_sub");
    if !had {
        return Ok(false);
    }
    let temp = path.with_extension("toml.relay");
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temp)?;
    file.write_all(toml::to_string_pretty(&table)?.as_bytes())?;
    file.sync_all()?;
    fs::rename(&temp, &path)?;
    Ok(true)
}

fn service_file_at(home: &Path, identity: &PublicMac, owner_sub: &str, write: bool, relay: &str) -> Result<bool> {
    let path = home.join(SERVICE_FILE);
    if let Some(existing) = existing_paths(&path)?
        && existing.relay_url.is_none()
    {
        check_identity(&existing, identity, &key_paths(home)?)?;
        if write {
            add_relay(home, owner_sub, relay)?;
        }
        return Ok(write);
    }
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
        Ok(_) => bail!(tr!("service.toml must be a regular file", "service.toml debe ser un archivo normal")),
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
            bail!(tr!("existing service.toml belongs to another Mac, account or key path", "el service.toml existente pertenece a otro Mac, cuenta o ruta de claves"));
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

/// Whether this Mac's `service.toml` names a relay; setup keeps enrolling such a Mac.
pub fn has_relay(home: &Path) -> Result<bool> {
    Ok(existing_paths(&home.join(SERVICE_FILE))?.is_some_and(|paths| paths.relay_url.is_some()))
}


/// Gives this Mac its service keys and a relay-free `service.toml`, so devices on the local
/// network can pair with it; quiet, for `alondra doctor --fix --json`. Returns whether the file
/// is new, and this Mac's id.
pub fn prepare_local_quietly(home: &Path) -> Result<(bool, String)> {
    let _lock = alondra_core::home_lock::HomeLock::shared(home)?;
    alondra_core::home_lock::ensure_ready(home)?;
    let (identity, _) = load_or_create_keys(&identity_base(home)?)?;
    let created = local_service_file(home, &identity)?;
    Ok((created, identity.mac_id.to_string()))
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
        let scratch_directory = crate::test_support::Directory::new();
        let home =
            scratch_directory.parent().unwrap().join(format!("alondra-service-keys-test-{}", std::process::id()));
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
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!(
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
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!(
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

    #[test]
    fn relay_off_takes_out_only_the_relay_and_relay_on_puts_it_back() {
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&*home).unwrap();
        let (_, mac_id) = prepare_local_quietly(&home).unwrap();
        add_relay(&home, "owner", "https://relay.example").unwrap();
        let path = home.join(SERVICE_FILE);
        let mut table: toml::Table = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        table.insert("devices".into(), toml::Value::Array(vec![toml::Value::String("phone".into())]));
        fs::write(&path, toml::to_string_pretty(&table).unwrap()).unwrap();
        let before = table.clone();

        assert!(remove_relay(&home).unwrap());
        let after: toml::Table = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!has_relay(&home).unwrap());
        assert!(after.get("owner_sub").is_none());
        for key in ["mac_id", "signing_key_path", "agreement_key_path", "devices"] {
            assert_eq!(after.get(key), before.get(key), "{key}");
        }
        assert_eq!(after["mac_id"].as_str(), Some(mac_id.as_str()));
        assert!(!remove_relay(&home).unwrap(), "a Mac without a relay has nothing to take out");

        add_relay(&home, "owner", "https://relay.example").unwrap();
        assert!(has_relay(&home).unwrap());
    }

    #[test]
    fn a_new_mac_is_ready_for_local_devices_without_an_account() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!("alondra-service-local-{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&home).unwrap();
        assert!(prepare_local_quietly(&home).unwrap().0);
        assert!(!prepare_local_quietly(&home).unwrap().0);
        let table: toml::Table = toml::from_str(&fs::read_to_string(home.join(SERVICE_FILE)).unwrap()).unwrap();
        assert!(table.get("relay_url").is_none() && table.get("owner_sub").is_none());
        assert!(table["mac_id"].as_str().unwrap().starts_with("mac-"));
        assert!(!has_relay(&home).unwrap());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn enrolling_a_local_mac_adds_the_relay_and_keeps_its_devices() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!("alondra-service-upgrade-{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&home).unwrap();
        prepare_local_quietly(&home).unwrap();
        let (identity, _) = load_or_create_keys(&home).unwrap();
        let path = home.join(SERVICE_FILE);
        let with_device = fs::read_to_string(&path).unwrap().replace(
            "devices = []",
            "[[devices]]\ndevice_id = \"client-1\"\nsigning_key = \"s\"\nagreement_key = \"a\"",
        );
        fs::write(&path, format!("{with_device}\n[direct]\nport = 47999\n")).unwrap();
        assert!(!service_file(&home, &identity, "owner", false).unwrap());
        assert!(service_file(&home, &identity, "owner", true).unwrap());
        let table: toml::Table = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(table["relay_url"].as_str(), Some(RELAY));
        assert_eq!(table["owner_sub"].as_str(), Some("owner"));
        assert_eq!(table["devices"][0]["device_id"].as_str(), Some("client-1"));
        assert_eq!(table["direct"]["port"].as_integer(), Some(47999));
        assert!(has_relay(&home).unwrap());
        assert!(!service_file(&home, &identity, "owner", false).unwrap());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn enrolling_again_accepts_a_service_file_that_names_the_previous_relay() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!("alondra-service-previous-{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&home).unwrap();
        prepare_local_quietly(&home).unwrap();
        let (identity, _) = load_or_create_keys(&home).unwrap();
        assert!(service_file(&home, &identity, "owner", true).unwrap());
        let path = home.join(SERVICE_FILE);
        let previous = fs::read_to_string(&path).unwrap().replace(RELAY, "https://alondra-api.davidcollado.dev");
        fs::write(&path, previous).unwrap();
        assert!(!service_file(&home, &identity, "owner", true).unwrap());
        let table: toml::Table = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(table["relay_url"].as_str(), Some(RELAY));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn revocation_is_confirmed_only_by_the_relay() {
        assert_eq!(revocation(204, None), Revocation::Done);
        assert_eq!(revocation(403, Some("account_access_denied")), Revocation::Done);
        assert_eq!(revocation(401, Some("unauthorized")), Revocation::CheckDirectory);
        assert_eq!(revocation(401, Some("relay_epoch_changed")), Revocation::Failed);
        assert_eq!(revocation(503, Some("mac_revocation_failed")), Revocation::Failed);
        assert_eq!(revocation(403, Some("forbidden")), Revocation::Failed);
        let directory = serde_json::json!({"macs":[{"mac_id":"mac-new"}]});
        assert!(listed(&directory, "mac-new").unwrap());
        assert!(!listed(&directory, "mac-old").unwrap());
        assert!(listed(&serde_json::json!({}), "mac-old").is_err());
    }
}

fn identity_base(home: &Path) -> Result<std::path::PathBuf> {
    let path = home.join(SERVICE_FILE);
    if !path.try_exists()? { return Ok(home.to_owned()); }
    identity_base_for(home, &fs::read_to_string(path)?)
}

fn identity_base_for(home: &Path, config: &str) -> Result<std::path::PathBuf> {
    let config: KeyPaths = toml::from_str(config)?;
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

pub fn recover(home: &Path, mac_name: &str, stop: impl FnOnce() -> Result<()>, input: &mut impl std::io::BufRead, output: &mut impl Write) -> Result<RecoveryOutcome> {
    recovery::interactive(home, mac_name, stop, input, output)
}
