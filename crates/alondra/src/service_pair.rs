//! Interactive, Mac-owned service pairing. The account token only reaches the rendezvous;
//! this Mac's signing and agreement keys authorize content access.
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use alondra_e2e::{EnvelopeV1, sign_http_challenge};
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

#[derive(Serialize, Deserialize)]
struct ServiceConfig {
    relay_url: String,
    mac_id: String,
    owner_sub: String,
    signing_key_path: String,
    agreement_key_path: String,
    devices: Vec<ServiceDevice>,
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
    envelope: EnvelopeV1,
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
        bail!("unsafe service key permissions");
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
        _ => bail!("unsupported relay method"),
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
            bail!("unsafe pairing state path");
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

pub fn has_paired_device(home: &Path) -> Result<bool> {
    if home.join("service-pairing-pending.json").exists() {
        return Ok(false);
    }
    let path = home.join("service.toml");
    if !path.exists() {
        return Ok(false);
    }
    let config: ServiceConfig = toml::from_str(&fs::read_to_string(path)?)?;
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

fn restart_daemon() -> Result<()> {
    let target = format!(
        "gui/{}/{}",
        unsafe { libc::getuid() },
        crate::client::config().service_label
    );
    let restarted = std::process::Command::new("launchctl")
        .args(["kickstart", "-k", &target])
        .status()?;
    if !restarted.success() {
        bail!("alondrad could not restart; rerun setup to repair");
    }
    Ok(())
}

fn local_approve(
    agent: &ureq::Agent,
    public: &PublicMac,
    signing: &SigningKey,
    phone: &CandidatePlain,
) -> Result<()> {
    let local_origin = format!("http://127.0.0.1:{}", crate::client::config().port);
    let mut local_boot = agent
        .get(format!("{local_origin}/v1/service/bootstrap"))
        .call()?;
    if local_boot.status().as_u16() != 200 {
        bail!("Mac approval service is unavailable");
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
        &local_origin,
        "POST",
        "/v1/service/approve-device",
        &local_body,
        &public.mac_id,
        signing,
        &local_epoch,
    )?;
    if status != 204 {
        bail!(
            "Mac rejected local device approval: {}",
            String::from_utf8_lossy(&detail)
        );
    }
    Ok(())
}

fn complete_pending(
    home: &Path,
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
        bail!("pending pairing belongs to another Mac or account");
    }
    if now_ms()? >= pending.expires_at_ms {
        bail!("pending pairing expired");
    }
    let phone = &pending.phone;
    local_approve(agent, public, signing, phone)?;
    let found = config
        .devices
        .iter()
        .find(|d| d.device_id == phone.device_id);
    match found {
        Some(device)
            if device.signing_key == phone.signing_key
                && device.agreement_key == phone.agreement_key => {}
        Some(_) => bail!("pending device ID conflicts with Mac service config"),
        None => {
            config.devices.push(ServiceDevice {
                device_id: phone.device_id.clone(),
                signing_key: phone.signing_key.clone(),
                agreement_key: phone.agreement_key.clone(),
            });
            atomic_write(
                &home.join("service.toml"),
                toml::to_string_pretty(config)?.as_bytes(),
            )?;
        }
    }
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
        bail!("relay device enrollment failed ({status})");
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
        bail!("relay approval handoff failed ({status})");
    }
    restart_daemon()?;
    fs::remove_file(home.join("service-pairing-pending.json"))?;
    fs::File::open(home)?.sync_all()?;
    println!("Client approved and Alondra restarted on this Mac.");
    Ok(())
}

fn abandon_expired(
    home: &Path,
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
        bail!("could not revoke expired phone ({status}); retry setup");
    }
    config
        .devices
        .retain(|d| d.device_id != pending.phone.device_id);
    atomic_write(
        &home.join("service.toml"),
        toml::to_string_pretty(config)?.as_bytes(),
    )?;
    let local_url = format!(
        "http://127.0.0.1:{}/v1/devices/{}",
        crate::client::config().port,
        pending.phone.device_id
    );
    let response = agent.delete(local_url).call()?;
    if !matches!(response.status().as_u16(), 200 | 404) {
        bail!("could not revoke expired phone locally");
    }
    restart_daemon()?;
    fs::remove_file(home.join("service-pairing-pending.json"))?;
    fs::File::open(home)?.sync_all()?;
    Ok(())
}

pub fn run(home: &Path) -> Result<()> {
    let path = home.join("service.toml");
    let mut config: ServiceConfig = toml::from_str(&fs::read_to_string(&path)?)?;
    let origin = url::Url::parse(&config.relay_url)?;
    if origin.scheme() != "https"
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
        || origin.host_str().is_none()
        || !origin.username().is_empty()
        || origin.password().is_some()
    {
        bail!("invalid service relay origin");
    }
    let public: PublicMac =
        serde_json::from_slice(&fs::read(home.join("service-keys/public.json"))?)?;
    if config.mac_id != public.mac_id {
        bail!("service Mac ID differs from its public identity");
    }
    let signing = SigningKey::from_slice(&private_key(home, &config.signing_key_path)?)?;
    let agreement = SecretKey::from_slice(&private_key(home, &config.agreement_key_path)?)?;
    if URL_SAFE_NO_PAD.encode(signing.verifying_key().to_sec1_bytes()) != public.signing_key
        || URL_SAFE_NO_PAD.encode(agreement.public_key().to_sec1_bytes()) != public.agreement_key
    {
        bail!("service public keys differ from private keys");
    }
    let agent = http();
    let mut boot = agent
        .get(format!(
            "{}/v1/bootstrap",
            config.relay_url.trim_end_matches('/')
        ))
        .call()?;
    if boot.status().as_u16() != 200 {
        bail!("relay bootstrap unavailable");
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
        bail!("pairing session refused ({status})");
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
            bail!("pairing session expired");
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
            _ => bail!("pairing poll failed ({status})"),
        }
    };
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
        || phone.owner_sub != config.owner_sub
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
        bail!("pairing candidate mismatch");
    }
    p256::PublicKey::from_sec1_bytes(&URL_SAFE_NO_PAD.decode(&phone.agreement_key)?)?;
    let fingerprint =
        URL_SAFE_NO_PAD.encode(&Sha256::digest(URL_SAFE_NO_PAD.decode(&phone.signing_key)?)[..6]);
    println!("Pairing request from {}.", phone.display_name);
    print!("Enter the 8-character code shown on that client: ");
    io::stdout().flush()?;
    let mut comparison = String::new();
    io::stdin().read_line(&mut comparison)?;
    if !bool::from(comparison.trim().as_bytes().ct_eq(fingerprint.as_bytes())) {
        bail!("Client comparison code does not match; pairing declined");
    }
    print!("Approve this device on this Mac? [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if !matches!(
        answer.trim(),
        "y" | "Y" | "yes" | "YES" | "s" | "S" | "sí" | "Sí"
    ) {
        bail!("pairing declined");
    }
    let pending = Pending {
        session_id,
        mac_id: public.mac_id.clone(),
        expires_at_ms: expires,
        phone,
    };
    atomic_write(
        &home.join("service-pairing-pending.json"),
        &serde_json::to_vec_pretty(&pending)?,
    )?;
    complete_pending(
        home,
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
        let dir = std::env::temp_dir().join(format!(
            "alondra-pair-status-{}",
            URL_SAFE_NO_PAD.encode(random::<8>().unwrap())
        ));
        fs::create_dir_all(&dir).unwrap();
        let config = ServiceConfig {
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
}
