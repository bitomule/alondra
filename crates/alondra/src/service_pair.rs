//! Interactive, Mac-owned service pairing. The account token only reaches the rendezvous;
//! this Mac's signing and agreement keys authorize content access.
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use alondra_core::home_lock::HomeLock;
use alondra_e2e::{Envelope, sign_http_challenge};
use anyhow::{Context, Result, anyhow, bail};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use p256::{
    SecretKey,
    ecdsa::{Signature, SigningKey, VerifyingKey, signature::Signer},
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

const MAX_WAIT: Duration = Duration::from_secs(600);
const DAEMON_POLL: Duration = Duration::from_millis(500);
const DAEMON_POLLS: u32 = 120;

#[derive(Serialize, Deserialize)]
struct ServiceConfig {
    relay_url: String,
    mac_id: String,
    owner_sub: String,
    signing_key_path: String,
    agreement_key_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    journal_path: Option<String>,
    devices: Vec<ServiceDevice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    direct: Option<toml::Value>,
}
#[derive(Serialize, Deserialize)]
struct ServiceDevice {
    device_id: String,
    signing_key: String,
    agreement_key: String,
}
#[derive(Serialize, Deserialize)]
struct PublicMac {
    mac_id: String,
    signing_key: String,
    agreement_key: String,
}
#[derive(Serialize)]
struct Qr<'a> {
    version: u8,
    url: &'a str,
    mac_id: &'a str,
    mac_signing_key: &'a str,
    mac_agreement_key: &'a str,
    session_id: &'a str,
    secret: &'a str,
    expires_at_ms: u64,
}
#[derive(Serialize)]
struct Session<'a> {
    session_id: &'a str,
    mac_id: &'a str,
    secret_hash: &'a str,
    expires_at_ms: u64,
}
#[derive(Deserialize)]
struct Bootstrap {
    epoch: String,
}
#[derive(Deserialize)]
struct Candidate {
    device_id: String,
    signing_key: String,
    envelope: Envelope,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidatePlain {
    device_id: String,
    signing_key: String,
    agreement_key: String,
    display_name: String,
    owner_sub: String,
    session_id: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    session_id: String,
    mac_id: String,
    expires_at_ms: u64,
    phone: CandidatePlain,
}
#[derive(Serialize)]
struct PhoneRegistration<'a> {
    device_id: &'a str,
    owner_sub: &'a str,
    signing_key: &'a str,
}
#[derive(Serialize)]
struct Approval<'a> {
    v: u8,
    session_id: &'a str,
    owner_sub: &'a str,
    mac_id: &'a str,
    mac_signing_key: &'a str,
    mac_agreement_key: &'a str,
    device_id: &'a str,
    phone_signing_key: &'a str,
    phone_agreement_key: &'a str,
    expires_at_ms: u64,
}
#[derive(Serialize)]
struct ApprovalProof<'a> {
    approval: Approval<'a>,
    signature: String,
}
#[derive(Serialize, Deserialize)]
struct LocalDevice {
    id: String,
    name: String,
    public_key: String,
    created_at: i64,
    #[serde(default)]
    last_seen: Option<i64>,
}

fn now_ms() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64)
}
fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow!("OS randomness failed"))?;
    Ok(bytes)
}
fn private_key(home: &Path, wire: &str) -> Result<[u8; 32]> {
    let path = home.join(wire);
    let meta = fs::symlink_metadata(&path)?;
    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
        bail!(tr!("unsafe service key permissions", "permisos inseguros en la clave del servicio"));
    }
    let bytes = URL_SAFE_NO_PAD.decode(fs::read_to_string(path)?.trim())?;
    bytes
        .try_into()
        .map_err(|_| anyhow!("invalid service key length"))
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
fn call(
    agent: &ureq::Agent,
    url: &str,
    method: &str,
    path: &str,
    body: &[u8],
    mac_id: &str,
    signing: &SigningKey,
    epoch: &[u8; 16],
) -> Result<(u16, Vec<u8>)> {
    let timestamp = now_ms()?;
    let nonce = random::<16>()?;
    let signature = sign_http_challenge(
        signing, mac_id, method, path, epoch, body, timestamp, &nonce,
    )
    .map_err(|e| anyhow!("sign relay request: {e}"))?;
    let endpoint = format!("{}{}", url.trim_end_matches('/'), path);
    let mut response = match method {
        "POST" => agent
            .post(endpoint)
            .header("x-device-id", mac_id)
            .header("x-device-timestamp-ms", timestamp.to_string())
            .header("x-device-nonce", URL_SAFE_NO_PAD.encode(nonce))
            .header("x-device-signature", signature)
            .header("x-relay-epoch", URL_SAFE_NO_PAD.encode(epoch))
            .header("content-type", "application/json")
            .send(body)?,
        "GET" => agent
            .get(endpoint)
            .header("x-device-id", mac_id)
            .header("x-device-timestamp-ms", timestamp.to_string())
            .header("x-device-nonce", URL_SAFE_NO_PAD.encode(nonce))
            .header("x-device-signature", signature)
            .header("x-relay-epoch", URL_SAFE_NO_PAD.encode(epoch))
            .call()?,
        "DELETE" => agent
            .delete(endpoint)
            .header("x-device-id", mac_id)
            .header("x-device-timestamp-ms", timestamp.to_string())
            .header("x-device-nonce", URL_SAFE_NO_PAD.encode(nonce))
            .header("x-device-signature", signature)
            .header("x-relay-epoch", URL_SAFE_NO_PAD.encode(epoch))
            .call()?,
        _ => bail!(tr!("unsupported relay method", "método de relay no admitido")),
    };
    let status = response.status().as_u16();
    let bytes = response
        .body_mut()
        .with_config()
        .limit(32 * 1024)
        .read_to_vec()?;
    Ok((status, bytes))
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.is_file() || meta.file_type().is_symlink() {
            bail!(tr!("unsafe pairing state path", "ruta insegura del estado de emparejamiento"));
        }
    }
    let temp = path.with_extension(format!(
        "pairing-{}",
        URL_SAFE_NO_PAD.encode(random::<8>()?)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    fs::File::open(path.parent().context("state directory")?)?.sync_all()?;
    Ok(())
}
fn approval_bytes(a: &Approval<'_>) -> Vec<u8> {
    let mut out = b"alondra-service-pairing-approval-v1\0".to_vec();
    out.push(a.v);
    for field in [
        a.session_id,
        a.owner_sub,
        a.mac_id,
        a.mac_signing_key,
        a.mac_agreement_key,
        a.device_id,
        a.phone_signing_key,
        a.phone_agreement_key,
    ] {
        out.extend_from_slice(&(field.len() as u32).to_be_bytes());
        out.extend_from_slice(field.as_bytes());
    }
    out.extend_from_slice(&a.expires_at_ms.to_be_bytes());
    out
}

#[derive(Deserialize)]
struct LocalServiceDevices {
    #[serde(default)]
    devices: Vec<ServiceDevice>,
}

pub fn has_relay(home: &Path) -> Result<bool> {
    let path = home.join("service.toml");
    if !path.exists() {
        return Ok(false);
    }
    Ok(toml::from_str::<toml::Table>(&fs::read_to_string(path)?)?.contains_key("relay_url"))
}

pub fn has_paired_device(home: &Path) -> Result<bool> {
    if home.join("service-pairing-pending.json").exists() {
        return Ok(false);
    }
    let path = home.join("service.toml");
    if !path.exists() {
        return Ok(false);
    }
    let config: LocalServiceDevices = toml::from_str(&fs::read_to_string(path)?)?;
    let local_path = home.join("devices.json");
    if !local_path.exists() {
        return Ok(false);
    }
    let local: Vec<LocalDevice> = serde_json::from_slice(&fs::read(local_path)?)?;
    Ok(config.devices.iter().any(|service| {
        local.iter().any(|device| device.id == service.device_id
            && matches!((STANDARD.decode(&device.public_key), URL_SAFE_NO_PAD.decode(&service.signing_key)),
                (Ok(local), Ok(service)) if local == service))
    }))
}

/// Each daemon process answers with its own random challenge epoch.
fn local_epoch(agent: &ureq::Agent, local_origin: &str) -> Option<[u8; 16]> {
    let mut response = agent
        .get(format!("{local_origin}/service/bootstrap"))
        .call()
        .ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let boot: Bootstrap = response.body_mut().with_config().limit(1024).read_json().ok()?;
    URL_SAFE_NO_PAD.decode(boot.epoch).ok()?.try_into().ok()
}

fn pause() {
    std::thread::sleep(DAEMON_POLL);
}

fn wait_for_daemon(mut ready: impl FnMut() -> bool, pause: impl Fn()) -> Result<()> {
    for _ in 0..DAEMON_POLLS {
        if ready() {
            return Ok(());
        }
        pause();
    }
    bail!(tr!("alondrad is not answering on this Mac; check ~/.alondra/alondrad.log, then rerun alondra pair", "alondrad no responde en este Mac; revisa ~/.alondra/alondrad.log y vuelve a ejecutar alondra pair"))
}

fn local_approve(
    agent: &ureq::Agent,
    local_origin: &str,
    public: &PublicMac,
    signing: &SigningKey,
    phone: &CandidatePlain,
) -> Result<()> {
    let mut local_boot = agent
        .get(format!("{local_origin}/service/bootstrap"))
        .call()?;
    if local_boot.status().as_u16() != 200 {
        bail!(tr!("Mac approval service is unavailable", "el servicio de aprobación del Mac no está disponible"));
    }
    let local_boot: Bootstrap = local_boot
        .body_mut()
        .with_config()
        .limit(1024)
        .read_json()?;
    let local_epoch: [u8; 16] = URL_SAFE_NO_PAD
        .decode(local_boot.epoch)?
        .try_into()
        .map_err(|_| anyhow!("invalid local approval challenge"))?;
    let local_body = serde_json::to_vec(&serde_json::json!({
        "device_id": phone.device_id, "name": phone.display_name,
        "signing_key": phone.signing_key,
    }))?;
    let (status, detail) = call(
        agent,
        local_origin,
        "POST",
        "/service/approve-device",
        &local_body,
        &public.mac_id,
        signing,
        &local_epoch,
    )?;
    if status != 204 {
        let detail = String::from_utf8_lossy(&detail);
        bail!(trf!("Mac rejected local device approval: {detail}", "el Mac rechazó la aprobación local del dispositivo: {detail}"));
    }
    Ok(())
}

fn complete_pending(
    home: &Path,
    local_origin: &str,
    config: &mut ServiceConfig,
    public: &PublicMac,
    signing: &SigningKey,
    agent: &ureq::Agent,
    epoch: &[u8; 16],
    pending: &Pending,
) -> Result<()> {
    if pending.mac_id != public.mac_id
        || pending.phone.owner_sub != config.owner_sub
        || pending.phone.session_id != pending.session_id
    {
        bail!(tr!("pending pairing belongs to another Mac or account", "el emparejamiento pendiente pertenece a otro Mac o cuenta"));
    }
    if now_ms()? >= pending.expires_at_ms {
        bail!(tr!("pending pairing expired", "el emparejamiento pendiente ha caducado"));
    }
    let phone = &pending.phone;
    let found = config
        .devices
        .iter()
        .find(|d| d.device_id == phone.device_id);
    match found {
        Some(device)
            if device.signing_key == phone.signing_key
                && device.agreement_key == phone.agreement_key => {}
        Some(_) => bail!(tr!("pending device ID conflicts with Mac service config", "el ID del dispositivo pendiente entra en conflicto con la configuración del servicio del Mac")),
        None => {
            let lock = alondra_core::service_config::ServiceConfigLock::exclusive(home)?;
            alondra_core::service_config::add_device(
                home,
                &lock,
                &alondra_core::service_config::NewDevice {
                    device_id: &phone.device_id,
                    signing_key: &phone.signing_key,
                    agreement_key: &phone.agreement_key,
                },
            )?;
            config.devices.push(ServiceDevice {
                device_id: phone.device_id.clone(),
                signing_key: phone.signing_key.clone(),
                agreement_key: phone.agreement_key.clone(),
            });
        }
    }
    local_approve(agent, local_origin, public, signing, phone)?;
    let registration = serde_json::to_vec(&PhoneRegistration {
        device_id: &phone.device_id,
        owner_sub: &config.owner_sub,
        signing_key: &phone.signing_key,
    })?;
    let (status, _) = call(
        agent,
        &config.relay_url,
        "POST",
        "/v1/devices",
        &registration,
        &public.mac_id,
        signing,
        epoch,
    )?;
    if status != 201 && status != 200 {
        bail!(trf!("relay device enrollment failed ({status})", "falló el registro del dispositivo en el relay ({status})"));
    }
    let approval = Approval {
        v: 1,
        session_id: &pending.session_id,
        owner_sub: &config.owner_sub,
        mac_id: &public.mac_id,
        mac_signing_key: &public.signing_key,
        mac_agreement_key: &public.agreement_key,
        device_id: &phone.device_id,
        phone_signing_key: &phone.signing_key,
        phone_agreement_key: &phone.agreement_key,
        expires_at_ms: pending.expires_at_ms,
    };
    let signature: Signature = signing.sign(&approval_bytes(&approval));
    let proof = ApprovalProof {
        approval,
        signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
    };
    let route = format!("/v1/pairings/{}/approval", pending.session_id);
    let (status, _) = call(
        agent,
        &config.relay_url,
        "POST",
        &route,
        &serde_json::to_vec(&proof)?,
        &public.mac_id,
        signing,
        epoch,
    )?;
    if status != 202 {
        bail!(trf!("relay approval handoff failed ({status})", "falló el traspaso de la aprobación al relay ({status})"));
    }
    fs::remove_file(home.join("service-pairing-pending.json"))?;
    fs::File::open(home)?.sync_all()?;
    println!("Client approved on this Mac.");
    Ok(())
}

fn abandon_expired(
    home: &Path,
    local_origin: &str,
    config: &mut ServiceConfig,
    public: &PublicMac,
    signing: &SigningKey,
    agent: &ureq::Agent,
    epoch: &[u8; 16],
    pending: &Pending,
) -> Result<()> {
    // Tombstone the phone ID first. A previous enrollment may have succeeded even if its
    // response was lost. Never leave that key usable after discarding local approval.
    let route = format!("/v1/devices/{}", pending.phone.device_id);
    let (status, _) = call(
        agent,
        &config.relay_url,
        "DELETE",
        &route,
        &[],
        &public.mac_id,
        signing,
        epoch,
    )?;
    if status != 204 {
        bail!(trf!("could not revoke expired phone ({status}); retry setup", "no se pudo revocar el teléfono caducado ({status}); reintenta la configuración"));
    }
    let lock = alondra_core::service_config::ServiceConfigLock::exclusive(home)?;
    alondra_core::service_config::remove_device(home, &lock, &pending.phone.device_id)?;
    config
        .devices
        .retain(|d| d.device_id != pending.phone.device_id);
    let local_url = format!("{}/devices/{}", local_origin, pending.phone.device_id);
    let response = agent.delete(local_url).call()?;
    if !matches!(response.status().as_u16(), 200 | 404) {
        bail!(tr!("could not revoke expired phone locally", "no se pudo revocar el teléfono caducado en local"));
    }
    fs::remove_file(home.join("service-pairing-pending.json"))?;
    fs::File::open(home)?.sync_all()?;
    Ok(())
}

// Keep comparison, explicit approval and pending persistence in one boundary.
// Inject only terminal I/O so adversarial candidates can exercise this actual flow.
#[allow(clippy::too_many_arguments)]
fn confirm_and_persist_candidate(
    home: &Path, owner_sub: &str, public: &PublicMac, agreement: &SecretKey,
    session_id: &str, expires: u64, candidate: Candidate,
    input: &mut impl io::BufRead, output: &mut impl Write,
) -> Result<Pending> {
    let phone_signing =
        VerifyingKey::from_sec1_bytes(&URL_SAFE_NO_PAD.decode(&candidate.signing_key)?)?;
    let plaintext = candidate
        .envelope
        .open(
            &phone_signing,
            &agreement,
            &candidate.device_id,
            &public.mac_id,
            &public.mac_id,
            now_ms()?,
        )
        .map_err(|e| anyhow!("invalid encrypted pairing candidate: {e}"))?;
    let phone: CandidatePlain = serde_json::from_slice(&plaintext)?;
    if phone.device_id != candidate.device_id
        || phone.signing_key != candidate.signing_key
        || phone.owner_sub != owner_sub
        || phone.session_id != session_id
        || !(phone.device_id.starts_with("client-") || phone.device_id.starts_with("phone-"))
        || phone.device_id.len() > 128
        || !phone
            .device_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        || phone.display_name.len() > 80
        || phone.display_name.chars().any(char::is_control)
    {
        bail!(tr!("pairing candidate mismatch", "el candidato de emparejamiento no coincide"));
    }
    p256::PublicKey::from_sec1_bytes(&URL_SAFE_NO_PAD.decode(&phone.agreement_key)?)?;
    let fingerprint =
        URL_SAFE_NO_PAD.encode(&Sha256::digest(URL_SAFE_NO_PAD.decode(&phone.signing_key)?)[..6]);
    writeln!(output, "Pairing request from {}.", phone.display_name)?;
    write!(output, "Enter the 8-character code shown on that client: ")?;
    output.flush()?;
    let mut comparison = String::new();
    input.read_line(&mut comparison)?;
    if !bool::from(comparison.trim().as_bytes().ct_eq(fingerprint.as_bytes())) {
        bail!(tr!("Client comparison code does not match; pairing declined", "el código de comparación del cliente no coincide; emparejamiento rechazado"));
    }
    write!(output, "Approve this device on this Mac? [y/N] ")?;
    output.flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    if !matches!(
        answer.trim(),
        "y" | "Y" | "yes" | "YES" | "s" | "S" | "sí" | "Sí"
    ) {
        bail!(tr!("pairing declined", "emparejamiento rechazado"));
    }
    let pending = Pending {
        session_id: session_id.to_owned(),
        mac_id: public.mac_id.clone(),
        expires_at_ms: expires,
        phone,
    };
    atomic_write(
        &home.join("service-pairing-pending.json"),
        &serde_json::to_vec_pretty(&pending)?,
    )?;
    Ok(pending)
}

pub fn run(home: &Path) -> Result<()> {
    let agent = http();
    let local = format!("http://127.0.0.1:{}", crate::client::config().port);
    // A daemon that is starting needs HOME exclusively: wait for it before sharing HOME.
    if local_epoch(&agent, &local).is_none() {
        println!("Waiting for alondrad to answer on this Mac…");
        wait_for_daemon(|| local_epoch(&agent, &local).is_some(), pause)?;
    }
    let _home_lock = HomeLock::shared(home)?;
    alondra_core::home_lock::ensure_ready(home)?;
    let path = home.join("service.toml");
    let text = fs::read_to_string(&path)?;
    if toml::from_str::<toml::Table>(&text)?.get("relay_url").is_none() {
        bail!(tr!("this Mac has no relay: devices on the same network pair on their own (alondra connect, or the app's search); to pair through the relay, run alondra setup --relay first", "este Mac no tiene relay: los dispositivos de la misma red se emparejan solos (alondra connect, o la búsqueda de la app); para emparejar a través del relay, ejecuta antes alondra setup --relay"));
    }
    let mut config: ServiceConfig = toml::from_str(&text)?;
    let origin = url::Url::parse(&config.relay_url)?;
    if origin.scheme() != "https"
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
        || origin.host_str().is_none()
        || !origin.username().is_empty()
        || origin.password().is_some()
    {
        bail!(tr!("invalid service relay origin", "origen del relay del servicio no válido"));
    }
    let public: PublicMac =
        serde_json::from_slice(&fs::read(alondra_core::home_lock::public_identity_path(home)?)?)?;
    if config.mac_id != public.mac_id {
        bail!(tr!("service Mac ID differs from its public identity", "el ID del Mac del servicio difiere de su identidad pública"));
    }
    let signing = SigningKey::from_slice(&private_key(home, &config.signing_key_path)?)?;
    let agreement = SecretKey::from_slice(&private_key(home, &config.agreement_key_path)?)?;
    if URL_SAFE_NO_PAD.encode(signing.verifying_key().to_sec1_bytes()) != public.signing_key
        || URL_SAFE_NO_PAD.encode(agreement.public_key().to_sec1_bytes()) != public.agreement_key
    {
        bail!(tr!("service public keys differ from private keys", "las claves públicas del servicio difieren de las privadas"));
    }
    let mut boot = agent
        .get(format!(
            "{}/v1/bootstrap",
            config.relay_url.trim_end_matches('/')
        ))
        .call()?;
    if boot.status().as_u16() != 200 {
        bail!(tr!("relay bootstrap unavailable", "el arranque del relay no está disponible"));
    }
    let boot: Bootstrap = boot.body_mut().with_config().limit(4096).read_json()?;
    let epoch: [u8; 16] = URL_SAFE_NO_PAD
        .decode(boot.epoch)?
        .try_into()
        .map_err(|_| anyhow!("invalid relay epoch"))?;
    let pending_path = home.join("service-pairing-pending.json");
    if pending_path.exists() {
        let pending: Pending = serde_json::from_slice(&fs::read(&pending_path)?)?;
        if now_ms()? >= pending.expires_at_ms {
            abandon_expired(
                home,
                &local,
                &mut config,
                &public,
                &signing,
                &agent,
                &epoch,
                &pending,
            )?;
            println!("Previous pairing expired and was revoked. Scan a new QR.");
        } else {
            return complete_pending(
                home,
                &local,
                &mut config,
                &public,
                &signing,
                &agent,
                &epoch,
                &pending,
            );
        }
    }
    let session_id = format!("pair-{}", URL_SAFE_NO_PAD.encode(random::<16>()?));
    let secret_bytes = random::<16>()?;
    let secret = URL_SAFE_NO_PAD.encode(secret_bytes);
    let secret_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(secret_bytes));
    let expires = now_ms()?.saturating_add(MAX_WAIT.as_millis() as u64);
    let body = serde_json::to_vec(&Session {
        session_id: &session_id,
        mac_id: &public.mac_id,
        secret_hash: &secret_hash,
        expires_at_ms: expires,
    })?;
    let (status, _) = call(
        &agent,
        &config.relay_url,
        "POST",
        "/v1/pairings",
        &body,
        &public.mac_id,
        &signing,
        &epoch,
    )?;
    if status != 201 {
        bail!(trf!("pairing session refused ({status})", "sesión de emparejamiento rechazada ({status})"));
    }
    let qr = serde_json::to_string(&Qr {
        version: 1,
        url: config.relay_url.trim_end_matches('/'),
        mac_id: &public.mac_id,
        mac_signing_key: &public.signing_key,
        mac_agreement_key: &public.agreement_key,
        session_id: &session_id,
        secret: &secret,
        expires_at_ms: expires,
    })?;
    let art = qrcode::QrCode::new(qr.as_bytes())?
        .render::<qrcode::render::unicode::Dense1x2>()
        .quiet_zone(true)
        .build();
    println!("{art}\nScan this QR in Alondra on your iPhone. It expires in 10 minutes.");
    println!("Terminal invitation (paste into alondra connect on the client):\n{qr}");
    let candidate = loop {
        if now_ms()? >= expires {
            bail!(tr!("pairing session expired", "la sesión de emparejamiento ha caducado"));
        }
        let route = format!("/v1/pairings/{session_id}");
        let (status, bytes) = call(
            &agent,
            &config.relay_url,
            "GET",
            &route,
            &[],
            &public.mac_id,
            &signing,
            &epoch,
        )?;
        match status {
            204 => std::thread::sleep(Duration::from_secs(2)),
            200 => break serde_json::from_slice::<Candidate>(&bytes)?,
            _ => bail!(trf!("pairing poll failed ({status})", "falló la consulta del emparejamiento ({status})")),
        }
    };
    let pending = confirm_and_persist_candidate(
        home, &config.owner_sub, &public, &agreement, &session_id, expires,
        candidate, &mut io::stdin().lock(), &mut io::stdout().lock(),
    )?;
    complete_pending(
        home,
        &local,
        &mut config,
        &public,
        &signing,
        &agent,
        &epoch,
        &pending,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substituted_candidate_cannot_persist_approval_with_original_client_code() {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        let scratch_directory = crate::test_support::Directory::new();
        let home=scratch_directory.parent().unwrap().join(format!("alondra-pairing-substitution-{}",URL_SAFE_NO_PAD.encode(random::<8>().unwrap())));
        fs::create_dir(&home).unwrap();
        let mac=SigningKey::from_slice(&[21;32]).unwrap();
        let agreement=SecretKey::from_slice(&[22;32]).unwrap();
        let original=SigningKey::from_slice(&[23;32]).unwrap();
        let impostor=SigningKey::from_slice(&[24;32]).unwrap();
        let public=PublicMac {mac_id:"mac-test".into(),signing_key:URL_SAFE_NO_PAD.encode(mac.verifying_key().to_sec1_bytes()),
            agreement_key:URL_SAFE_NO_PAD.encode(agreement.public_key().to_sec1_bytes())};
        let expires=now_ms().unwrap()+60_000;
        let candidate=|signing: &SigningKey, agreement_byte: u8| {
            let key=URL_SAFE_NO_PAD.encode(signing.verifying_key().to_sec1_bytes());
            let plain=CandidatePlain {device_id:"phone-test".into(),signing_key:key.clone(),
                agreement_key:URL_SAFE_NO_PAD.encode(SecretKey::from_slice(&[agreement_byte;32]).unwrap().public_key().to_sec1_bytes()),
                display_name:"My iPhone".into(),owner_sub:"owner".into(),session_id:"pair-test".into()};
            Candidate {device_id:plain.device_id.clone(),signing_key:key,
                envelope:Envelope::seal_random(alondra_e2e::Context {route:"mac-test",sender:"phone-test",recipient:"mac-test",
                    request_id:"pair-test",kind:"pairing",expires_at_ms:expires},
                    &serde_json::to_vec(&plain).unwrap(),signing,&agreement.public_key()).unwrap()}
        };
        let code=URL_SAFE_NO_PAD.encode(&Sha256::digest(original.verifying_key().to_sec1_bytes())[..6]);
        let answers=format!("{code}\ny\n");
        // Same displayed name, owner, session and device ID, freshly valid
        // ciphertext/signature, but attacker-controlled signing AND agreement keys.
        let error=confirm_and_persist_candidate(&home,"owner",&public,&agreement,"pair-test",expires,
            candidate(&impostor,25),&mut io::Cursor::new(answers.as_bytes()),&mut Vec::new()).err().unwrap();
        assert!(error.to_string().contains("comparison code does not match"));
        assert_eq!(fs::read_dir(&home).unwrap().count(),0,"no pending or local approval may be written");
        // Positive control follows exactly the same crypto, prompt and durable
        // persistence path with the legitimate client's code and keys.
        confirm_and_persist_candidate(&home,"owner",&public,&agreement,"pair-test",expires,
            candidate(&original,26),&mut io::Cursor::new(answers.as_bytes()),&mut Vec::new()).unwrap();
        let pending: Pending=serde_json::from_slice(&fs::read(home.join("service-pairing-pending.json")).unwrap()).unwrap();
        assert_eq!(pending.phone.signing_key,URL_SAFE_NO_PAD.encode(original.verifying_key().to_sec1_bytes()));
        fs::remove_dir_all(home).unwrap();
    }

    fn temp_home(_name: &str) -> crate::test_support::Directory {
        let home = crate::test_support::Directory::new();
        fs::create_dir(&home).unwrap();
        home
    }

    /// Answers every request on its own connection and records `METHOD path`.
    fn fake_server(
        requests: usize,
        answer: impl Fn(&str) -> &'static str + Send + 'static,
        seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for _ in 0..requests {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request = line.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" || header.is_empty() {
                        break;
                    }
                }
                let status = answer(&request);
                seen.lock().unwrap().push(request);
                (&stream)
                    .write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes())
                    .unwrap();
            }
        });
        (origin, server)
    }

    #[test]
    fn expired_pending_is_revoked_on_the_relay_and_this_mac_then_cleared() {
        let home = temp_home("pair-expired");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (origin, relay) = fake_server(
            1,
            |request| match request {
                "DELETE /v1/devices/phone-test" => "204 No Content",
                _ => "404 Not Found",
            },
            seen.clone(),
        );
        // alondrad's local route is `/devices/{id}`; it answers 404 to any other path.
        let (local, daemon_server) = fake_server(
            1,
            |request| match request {
                "DELETE /devices/phone-test" => "200 OK",
                _ => "404 Not Found",
            },
            seen.clone(),
        );
        let mac = SigningKey::from_slice(&[21; 32]).unwrap();
        let public = PublicMac {
            mac_id: "mac-test".into(),
            signing_key: URL_SAFE_NO_PAD.encode(mac.verifying_key().to_sec1_bytes()),
            agreement_key: "unused".into(),
        };
        let mut config = ServiceConfig {
            journal_path: None,
            relay_url: origin.clone(),
            mac_id: "mac-test".into(),
            owner_sub: "owner".into(),
            signing_key_path: "signing.key".into(),
            agreement_key_path: "agreement.key".into(),
            devices: vec![ServiceDevice {
                device_id: "phone-test".into(),
                signing_key: "key".into(),
                agreement_key: "key".into(),
            }],
            direct: None,
        };
        let pending = Pending {
            session_id: "pair-test".into(),
            mac_id: "mac-test".into(),
            expires_at_ms: 1,
            phone: CandidatePlain {
                device_id: "phone-test".into(),
                signing_key: "key".into(),
                agreement_key: "key".into(),
                display_name: "iPhone".into(),
                owner_sub: "owner".into(),
                session_id: "pair-test".into(),
            },
        };
        fs::write(home.join("service-pairing-pending.json"), serde_json::to_vec(&pending).unwrap()).unwrap();
        fs::write(home.join("service.toml"), toml::to_string(&config).unwrap()).unwrap();
        abandon_expired(&home, &local, &mut config, &public, &mac, &http(), &[3; 16], &pending).unwrap();
        relay.join().unwrap();
        daemon_server.join().unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["DELETE /v1/devices/phone-test", "DELETE /devices/phone-test"]
        );
        assert!(!home.join("service-pairing-pending.json").exists());
        let saved: ServiceConfig = toml::from_str(&fs::read_to_string(home.join("service.toml")).unwrap()).unwrap();
        assert!(saved.devices.is_empty());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn expired_cleanup_can_send_signed_delete() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = [0u8; 4096];
            let n = stream.read(&mut bytes).unwrap();
            let request = String::from_utf8_lossy(&bytes[..n]);
            assert!(request.starts_with("DELETE /v1/devices/phone-test HTTP/1.1"));
            assert!(request.to_ascii_lowercase().contains("x-device-signature:"));
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        let signing = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let (status, body) = call(
            &http(),
            &origin,
            "DELETE",
            "/v1/devices/phone-test",
            &[],
            "mac-test",
            &signing,
            &[3u8; 16],
        )
        .unwrap();
        assert_eq!(status, 204);
        assert!(body.is_empty());
        server.join().unwrap();
    }

    #[test]
    fn setup_skips_qr_only_for_matching_local_and_service_approval() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(format!(
            "alondra-pair-status-{}",
            URL_SAFE_NO_PAD.encode(random::<8>().unwrap())
        ));
        fs::create_dir_all(&dir).unwrap();
        let config = ServiceConfig {
            journal_path: None,
            relay_url: "https://example.test/".into(),
            mac_id: "mac-a".into(),
            owner_sub: "owner".into(),
            signing_key_path: "signing.key".into(),
            agreement_key_path: "agreement.key".into(),
            devices: vec![ServiceDevice {
                device_id: "phone-a".into(),
                signing_key: URL_SAFE_NO_PAD.encode([7u8; 65]),
                agreement_key: "unused".into(),
            }],
            direct: None,
        };
        fs::write(dir.join("service.toml"), toml::to_string(&config).unwrap()).unwrap();
        let local = vec![LocalDevice {
            id: "phone-a".into(),
            name: "iPhone".into(),
            public_key: STANDARD.encode([7u8; 65]),
            created_at: 1,
            last_seen: None,
        }];
        fs::write(
            dir.join("devices.json"),
            serde_json::to_vec(&local).unwrap(),
        )
        .unwrap();
        assert!(has_paired_device(&dir).unwrap());
        fs::write(dir.join("service-pairing-pending.json"), b"pending").unwrap();
        assert!(!has_paired_device(&dir).unwrap());
        fs::remove_file(dir.join("service-pairing-pending.json")).unwrap();
        let mut wrong = local;
        wrong[0].public_key = "malformed".into();
        fs::write(
            dir.join("devices.json"),
            serde_json::to_vec(&wrong).unwrap(),
        )
        .unwrap();
        assert!(!has_paired_device(&dir).unwrap());
    }

    #[test]
    fn a_device_paired_without_relay_counts_as_paired() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(format!(
            "alondra-pair-local-{}",
            URL_SAFE_NO_PAD.encode(random::<8>().unwrap())
        ));
        fs::create_dir_all(&dir).unwrap();
        assert!(!has_relay(&dir).unwrap());
        let service = format!(
            "mac_id = \"mac-a\"\nsigning_key_path = \"signing.key\"\nagreement_key_path = \"agreement.key\"\n\n[[devices]]\ndevice_id = \"phone-a\"\nsigning_key = \"{}\"\nagreement_key = \"unused\"\n",
            URL_SAFE_NO_PAD.encode([7u8; 65])
        );
        fs::write(dir.join("service.toml"), &service).unwrap();
        assert!(!has_relay(&dir).unwrap());
        assert!(!has_paired_device(&dir).unwrap());
        let local = vec![LocalDevice {
            id: "phone-a".into(),
            name: "iPhone".into(),
            public_key: STANDARD.encode([7u8; 65]),
            created_at: 1,
            last_seen: None,
        }];
        fs::write(dir.join("devices.json"), serde_json::to_vec(&local).unwrap()).unwrap();
        assert!(has_paired_device(&dir).unwrap());
        fs::write(dir.join("service.toml"), format!("relay_url = \"https://example.test/\"\nowner_sub = \"owner\"\n{service}")).unwrap();
        assert!(has_relay(&dir).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }
}
