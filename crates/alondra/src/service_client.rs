//! A terminal is an approved client, never the executor identity of its host Mac.
use alondra_e2e::{Context as EnvelopeContext, EnvelopeV1, sign_http_challenge};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use p256::{
    PublicKey, SecretKey,
    ecdsa::{SigningKey, VerifyingKey},
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    sync::{Arc, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const RELAY: &str = "https://alondra-api.davidcollado.dev";
static ACTIVE: OnceLock<Arc<Profile>> = OnceLock::new();
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub mac_id: String,
    pub owner_sub: String,
    pub device_id: String,
    pub mac_signing_key: String,
    pub mac_agreement_key: String,
    pub signing_key: String,
    pub agreement_key: String,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Profiles {
    selected: Option<String>,
    profiles: Vec<Profile>,
}

fn remaining(deadline: Instant) -> Result<Duration> {
    deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).context("remote operation deadline exceeded; inspect the destination before repeating an action")
}

pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow!("OS randomness failed"))?;
    Ok(bytes)
}
pub fn now_ms() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64)
}
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub fn http(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .build(),
        )
        .timeout_global(Some(timeout))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into()
}
fn store_path() -> std::path::PathBuf {
    crate::client::config().home.join("terminal-service.json")
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.is_file() || meta.file_type().is_symlink() {
            bail!("unsafe terminal profile path");
        }
    }
    let tmp = path.with_extension(format!("tmp-{}", B64.encode(random::<12>()?)));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    fs::File::open(path.parent().context("profile parent")?)?.sync_all()?;
    Ok(())
}
fn load() -> Result<Profiles> {
    let path = store_path();
    let meta = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Profiles::default()),
        Err(e) => return Err(e.into()),
    };
    if !meta.is_file() || meta.file_type().is_symlink() || meta.permissions().mode() & 0o077 != 0 {
        bail!("terminal profile must be a regular owner-only file");
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn save(profiles: &Profiles) -> Result<()> {
    atomic_write(&store_path(), &serde_json::to_vec_pretty(profiles)?)
}
pub fn active() -> Option<Arc<Profile>> {
    ACTIVE.get().cloned()
}
pub fn target() -> Option<&'static str> {
    ACTIVE.get().map(|p| p.mac_id.as_str())
}
pub fn machines() -> Result<Vec<String>> {
    let token = crate::account::token_for_service()?;
    let mut response = http(Duration::from_secs(15))
        .get(format!("{RELAY}/v1/macs"))
        .header("authorization", format!("Bearer {}", token.access_token))
        .call()?;
    if response.status().as_u16() != 200 {
        bail!("Mac directory unavailable ({})", response.status());
    }
    #[derive(Deserialize)]
    struct Mac {
        mac_id: String,
    }
    #[derive(Deserialize)]
    struct Directory {
        macs: Vec<Mac>,
    }
    let directory: Directory = response
        .body_mut()
        .with_config()
        .limit(128 * 1024)
        .read_json()?;
    if directory.macs.iter().any(|m| !valid_id(&m.mac_id)) {
        bail!("invalid Mac directory");
    }
    Ok(directory.macs.into_iter().map(|m| m.mac_id).collect())
}
pub fn list() -> Result<()> {
    for mac in machines()? {
        println!("{mac}");
    }
    Ok(())
}
pub fn connect(mac: &str, repair: bool) -> Result<()> {
    let _lock = ProfileLock::acquire()?;
    if !machines()?.iter().any(|m| m == mac) {
        bail!("Mac is not available in this account");
    }
    let token = crate::account::token_for_service()?;
    let mut profiles = load()?;
    if repair
        || !profiles
            .profiles
            .iter()
            .any(|p| p.mac_id == mac && p.owner_sub == token.sub)
    {
        let profile = crate::client_pair::pair(mac, &token.sub)?;
        profiles.profiles.retain(|p| p.mac_id != mac);
        profiles.profiles.push(profile);
    }
    profiles.selected = Some(mac.into());
    save(&profiles)?;
    println!("Selected Mac: {mac}");
    Ok(())
}
pub fn initialize(requested: Option<&str>) -> Result<()> {
    crate::account::ensure_token_for_enrollment()?;
    let mut profiles = load()?;
    if let Some(mac) = requested {
        if !valid_id(mac) {
            bail!("invalid Mac ID");
        }
        profiles.selected = Some(mac.into());
    }
    if profiles.selected.is_none() {
        let macs = machines()?;
        if macs.is_empty() {
            bail!("No Macs enrolled in this account. Run alondra setup on the destination Mac.");
        }
        println!("Select the Mac this terminal will control:");
        for (i, mac) in macs.iter().enumerate() {
            println!("  {}. {mac}", i + 1);
        }
        print!("Mac number: ");
        std::io::stdout().flush()?;
        let mut choice = String::new();
        std::io::stdin().read_line(&mut choice)?;
        let index = choice
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .context("invalid selection")?;
        connect(macs.get(index).context("invalid selection")?, false)?;
        profiles = load()?;
    }
    let token = crate::account::token_for_service()?;
    let selected = profiles
        .selected
        .context("select a Mac with alondra connect")?;
    let profile = profiles
        .profiles
        .into_iter()
        .find(|p| p.mac_id == selected && p.owner_sub == token.sub)
        .context("selected Mac has no approval for this account; run alondra connect <mac-id>")?;
    profile.validate()?;
    ACTIVE
        .set(Arc::new(profile))
        .map_err(|_| anyhow!("terminal target already initialized"))?;
    Ok(())
}
impl Profile {
    fn signing(&self) -> Result<SigningKey> {
        Ok(SigningKey::from_slice(&B64.decode(&self.signing_key)?)?)
    }
    fn agreement(&self) -> Result<SecretKey> {
        Ok(SecretKey::from_slice(&B64.decode(&self.agreement_key)?)?)
    }
    pub fn validate(&self) -> Result<()> {
        if !valid_id(&self.mac_id)
            || !valid_id(&self.device_id)
            || !self.device_id.starts_with("client-")
            || self.mac_id == self.device_id
        {
            bail!("invalid terminal identity");
        }
        self.signing()?;
        self.agreement()?;
        VerifyingKey::from_sec1_bytes(&B64.decode(&self.mac_signing_key)?)?;
        PublicKey::from_sec1_bytes(&B64.decode(&self.mac_agreement_key)?)?;
        Ok(())
    }
    pub fn relay_call(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        deadline: Instant,
    ) -> Result<(u16, Value)> {
        remaining(deadline)?;
        let token = crate::account::token_for_service()?;
        if token.sub != self.owner_sub {
            bail!("terminal approval belongs to another account");
        }
        let mut response = http(remaining(deadline)?.min(Duration::from_secs(35)))
            .get(format!("{RELAY}/v1/bootstrap"))
            .call()?;
        if response.status().as_u16() != 200 {
            bail!("relay bootstrap unavailable");
        }
        let boot: Value = response.body_mut().with_config().limit(4096).read_json()?;
        let epoch: [u8; 16] = B64
            .decode(boot["epoch"].as_str().context("missing epoch")?)?
            .try_into()
            .map_err(|_| anyhow!("invalid relay epoch"))?;
        let timestamp = now_ms()?;
        let nonce = random::<16>()?;
        let signature = sign_http_challenge(
            &self.signing()?,
            &self.device_id,
            method,
            path,
            &epoch,
            body,
            timestamp,
            &nonce,
        )?;
        let headers = [
            ("authorization", format!("Bearer {}", token.access_token)),
            ("x-device-id", self.device_id.clone()),
            ("x-device-timestamp-ms", timestamp.to_string()),
            ("x-device-nonce", B64.encode(nonce)),
            ("x-device-signature", signature),
            ("x-relay-epoch", B64.encode(epoch)),
        ];
        let agent = http(remaining(deadline)?.min(Duration::from_secs(35)));
        let endpoint = format!("{RELAY}{path}");
        let mut response = if method == "POST" {
            let mut request = agent
                .post(endpoint)
                .header("content-type", "application/json");
            for (key, value) in &headers {
                request = request.header(*key, value);
            }
            request.send(body)?
        } else if method == "GET" {
            let mut request = agent.get(endpoint);
            for (key, value) in &headers {
                request = request.header(*key, value);
            }
            request.call()?
        } else {
            bail!("unsupported relay method");
        };
        let status = response.status().as_u16();
        let bytes = response
            .body_mut()
            .with_config()
            .limit(512 * 1024)
            .read_to_vec()?;
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)?
        };
        remaining(deadline)?;
        Ok((status, value))
    }
    pub fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<(u16, Value)> {
        self.request_via(
            method,
            path,
            body,
            Instant::now() + timeout,
            |method, path, body, deadline| self.relay_call(method, path, body, deadline),
        )
    }
    fn request_via(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        deadline: Instant,
        mut call: impl FnMut(&str, &str, &[u8], Instant) -> Result<(u16, Value)>,
    ) -> Result<(u16, Value)> {
        let timeout = remaining(deadline)?;
        let request_id = format!("op-{}", B64.encode(random::<16>()?));
        let plaintext = serde_json::to_vec(&json!({"method":method,"path":path,"body":body}))?;
        let envelope = EnvelopeV1::seal_random(
            EnvelopeContext {
                route: &self.mac_id,
                sender: &self.device_id,
                recipient: &self.mac_id,
                request_id: &request_id,
                kind: "request",
                expires_at_ms: now_ms()? + timeout.as_millis().min(590_000) as u64,
            },
            &plaintext,
            &self.signing()?,
            &PublicKey::from_sec1_bytes(&B64.decode(&self.mac_agreement_key)?)?,
        )?;
        let (status, _) = call(
            "POST",
            "/v1/requests",
            &serde_json::to_vec(&envelope)?,
            deadline,
        )
        .context(
            "request submission uncertain; inspect the destination before repeating an action",
        )?;
        remaining(deadline)?;
        if !matches!(status, 200 | 202) {
            bail!(
                "relay refused request ({status}); for a revoked terminal use alondra connect <mac-id> --repair"
            );
        }
        let mut revision = 0u64;
        let mut epoch: Option<String> = None;
        loop {
            remaining(deadline)?;
            let (status, job) = call("GET", &format!("/v1/requests/{request_id}"), &[], deadline)?;
            remaining(deadline)?;
            if status != 200 || job["request_id"].as_str() != Some(&request_id) {
                bail!("invalid relay request status ({status})");
            }
            if let Some(value) = job.get("response").filter(|v| !v.is_null()) {
                return self.open_response(value.clone(), &request_id, now_ms()?);
            }
            if job["status"].as_str() == Some("expired") || Instant::now() >= deadline {
                bail!("destination did not finish request; inspect it before repeating an action");
            }
            let seconds = deadline
                .saturating_duration_since(Instant::now())
                .as_secs()
                .min(25);
            let mut route = format!("/v1/changes?since={revision}&wait_s={seconds}");
            if let Some(e) = &epoch {
                route.push_str(&format!("&epoch={e}"));
            }
            let (status, changes) = call("GET", &route, &[], deadline)?;
            if status == 409 {
                revision = 0;
                epoch = None;
                continue;
            }
            if status != 200 {
                bail!("relay change wait failed ({status})");
            }
            let next = changes["revision"].as_u64().context("missing revision")?;
            let next_epoch = changes["epoch"].as_str().context("missing relay epoch")?;
            if B64.decode(next_epoch)?.len() != 16
                || next < revision
                || epoch.as_deref().is_some_and(|e| e != next_epoch)
            {
                bail!("invalid relay change cursor");
            }
            revision = next;
            epoch = Some(next_epoch.into());
        }
    }
    fn open_response(&self, value: Value, request_id: &str, now: u64) -> Result<(u16, Value)> {
        let response: EnvelopeV1 = serde_json::from_value(value)?;
        if response.kind != "response" || response.request_id != request_id {
            bail!("response correlation mismatch");
        }
        let bytes = response.open(
            &VerifyingKey::from_sec1_bytes(&B64.decode(&self.mac_signing_key)?)?,
            &self.agreement()?,
            &self.mac_id,
            &self.device_id,
            &self.mac_id,
            now,
        )?;
        let result: Value = serde_json::from_slice(&bytes)?;
        let status = result["status"]
            .as_u64()
            .filter(|s| (100..600).contains(s))
            .context("invalid endpoint status")? as u16;
        Ok((status, result.get("body").cloned().unwrap_or(result)))
    }

    pub fn upload(&self, name: &str, bytes: &[u8], timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        if bytes.is_empty() || bytes.len() > 20 * 1024 * 1024 {
            bail!("attachment must contain 1 byte to 20 MB");
        }
        let id = random::<16>()?;
        let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
        let id = format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        );
        let hash: String = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let call = |suffix: &str, body: Value| -> Result<Value> {
            let (status, value) = self.request(
                "POST",
                &format!("/v1/service/uploads/{suffix}"),
                Some(&body),
                remaining(deadline)?,
            )?;
            if status >= 400 {
                bail!("attachment transfer failed ({status}): {value}");
            }
            Ok(value)
        };
        call(
            "start",
            json!({"transfer_id":id,"name":name,"bytes":bytes.len(),"sha256":hash}),
        )?;
        let result = (|| {
            for (index, chunk) in bytes.chunks(64 * 1024).enumerate() {
                call(
                    "chunk",
                    json!({"transfer_id":id,"offset":index * 64 * 1024,"data":B64.encode(chunk)}),
                )?;
            }
            call("finish", json!({"transfer_id":id}))
        })();
        if result.is_err() {
            let _ = call("cancel", json!({"transfer_id":id}));
        }
        result
    }
    pub fn file(&self, route: &str, timeout: Duration) -> Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let query = route
            .strip_prefix("/v1/file?")
            .context("binary service route unsupported")?;
        let fields: Vec<_> = url::form_urlencoded::parse(query.as_bytes()).collect();
        if fields.len() != 1 || fields[0].0 != "path" {
            bail!("invalid file route");
        }
        let path = &fields[0].1;
        let mut bytes = Vec::new();
        let mut version: Option<String> = None;
        let mut total = None;
        loop {
            let (status, chunk) = self.request(
                "POST",
                "/v1/service/files/chunk",
                Some(&json!({"path":path,"offset":bytes.len(),"length":65536,"version":version})),
                remaining(deadline)?,
            )?;
            if status != 200 {
                bail!("file download failed ({status}): {chunk}");
            }
            let size = chunk["total"].as_u64().context("file size missing")?;
            let v = chunk["version"].as_str().context("file version missing")?;
            if size > 20 * 1024 * 1024
                || total.is_some_and(|t| t != size)
                || version.as_deref().is_some_and(|old| old != v)
                || chunk["offset"].as_u64() != Some(bytes.len() as u64)
            {
                bail!("file changed during download");
            }
            let data = B64.decode(chunk["data"].as_str().context("file data missing")?)?;
            if data.len() > 65536 || bytes.len() as u64 + data.len() as u64 > size {
                bail!("invalid file chunk");
            }
            bytes.extend_from_slice(&data);
            if chunk["next_offset"].as_u64() != Some(bytes.len() as u64) {
                bail!("invalid file offset");
            }
            if chunk["eof"] == true {
                if bytes.len() as u64 != size {
                    bail!("incomplete file");
                }
                return Ok(bytes);
            }
            if data.is_empty() {
                bail!("empty unfinished file chunk");
            }
            version = Some(v.into());
            total = Some(size);
        }
    }
}

struct ProfileLock(fs::File);
impl ProfileLock {
    fn acquire() -> Result<Self> {
        use std::os::fd::AsRawFd;
        fs::create_dir_all(&crate::client::config().home)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(crate::client::config().home.join("terminal-service.lock"))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self(file))
    }
}
impl Drop for ProfileLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Profile, SigningKey, SecretKey) {
        let mac = SigningKey::from_slice(&[1; 32]).unwrap();
        let mac_agreement = SecretKey::from_slice(&[5; 32]).unwrap();
        let signing = SigningKey::from_slice(&[2; 32]).unwrap();
        let agreement = SecretKey::from_slice(&[3; 32]).unwrap();
        (
            Profile {
                mac_id: "mac-a".into(),
                owner_sub: "owner-a".into(),
                device_id: "client-a".into(),
                mac_signing_key: B64.encode(mac.verifying_key().to_sec1_bytes()),
                mac_agreement_key: B64.encode(mac_agreement.public_key().to_sec1_bytes()),
                signing_key: B64.encode(signing.to_bytes()),
                agreement_key: B64.encode(agreement.to_bytes()),
            },
            mac,
            mac_agreement,
        )
    }

    #[test]
    fn queued_operation_is_encrypted_and_waits_for_its_correlated_reply_without_resubmitting() {
        let (profile, mac, mac_agreement) = fixture();
        let mut stage = 0;
        let mut request_id = String::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        let result = profile.request_via("POST", "/v1/questions/7/answer", Some(&json!({"text":"yes"})), deadline, |method, path, bytes, budget| {
            assert_eq!(budget, deadline, "every stage shares one absolute deadline");
            stage += 1;
            match stage {
                1 => {
                    assert_eq!((method,path), ("POST", "/v1/requests"));
                    assert!(!String::from_utf8_lossy(bytes).contains("questions"));
                    let envelope: EnvelopeV1 = serde_json::from_slice(bytes)?;
                    let plain = envelope.open(profile.signing()?.verifying_key(), &mac_agreement, "client-a", "mac-a", "mac-a", now_ms()?)?;
                    assert_eq!(serde_json::from_slice::<Value>(&plain)?, json!({"method":"POST","path":"/v1/questions/7/answer","body":{"text":"yes"}}));
                    request_id = envelope.request_id;
                    Ok((202,json!({"request_id":request_id})))
                }
                2 => { assert_eq!(path, format!("/v1/requests/{request_id}")); Ok((200,json!({"request_id":request_id,"status":"queued","response":null}))) }
                3 => { assert!(path.starts_with("/v1/changes?since=0&wait_s=")); Ok((200,json!({"epoch":B64.encode([9;16]),"revision":2,"changes":[]}))) }
                4 => {
                    assert_eq!(path, format!("/v1/requests/{request_id}"));
                    let envelope = EnvelopeV1::seal_random(EnvelopeContext { route:"mac-a",sender:"mac-a",recipient:"client-a",request_id:&request_id,kind:"response",expires_at_ms:now_ms()?+5000 }, br#"{"status":200,"body":{"ok":true}}"#, &mac, &profile.agreement()?.public_key())?;
                    Ok((200,json!({"request_id":request_id,"status":"completed","response":envelope})))
                }
                _ => panic!("operation unexpectedly resubmitted"),
            }
        }).unwrap();
        assert_eq!(result, (200, json!({"ok":true})));
        assert_eq!(stage, 4);
    }

    #[test]
    fn failed_submission_and_expired_budget_never_retry_a_mutation() {
        let (profile, _, _) = fixture();
        let mut calls = 0;
        let result = profile.request_via(
            "POST",
            "/v1/requests",
            Some(&json!({})),
            Instant::now() + Duration::from_secs(1),
            |_, _, _, _| {
                calls += 1;
                bail!("connection lost after sending")
            },
        );
        assert!(result.unwrap_err().to_string().contains("uncertain"));
        assert_eq!(calls, 1);
        let result = profile.request_via(
            "POST",
            "/v1/requests",
            Some(&json!({})),
            Instant::now(),
            |_, _, _, _| panic!("expired operations must not send"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn responses_require_the_pinned_mac_correct_route_request_and_current_expiry() {
        let mac = SigningKey::from_slice(&[1; 32]).unwrap();
        let signing = SigningKey::from_slice(&[2; 32]).unwrap();
        let agreement = SecretKey::from_slice(&[3; 32]).unwrap();
        let profile = Profile {
            mac_id: "mac-a".into(),
            owner_sub: "owner-a".into(),
            device_id: "client-a".into(),
            mac_signing_key: B64.encode(mac.verifying_key().to_sec1_bytes()),
            mac_agreement_key: B64.encode(agreement.public_key().to_sec1_bytes()),
            signing_key: B64.encode(signing.to_bytes()),
            agreement_key: B64.encode(agreement.to_bytes()),
        };
        profile.validate().unwrap();
        let seal = |route: &str, kind: &str, key: &SigningKey| {
            serde_json::to_value(
                EnvelopeV1::seal_random(
                    EnvelopeContext {
                        route,
                        sender: "mac-a",
                        recipient: "client-a",
                        request_id: "op-a",
                        kind,
                        expires_at_ms: 2000,
                    },
                    br#"{"status":200,"body":{"tick":7}}"#,
                    key,
                    &agreement.public_key(),
                )
                .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(
            profile
                .open_response(seal("mac-a", "response", &mac), "op-a", 1000)
                .unwrap(),
            (200, json!({"tick":7}))
        );
        assert!(
            profile
                .open_response(seal("mac-a", "response", &mac), "op-other", 1000)
                .is_err()
        );
        assert!(
            profile
                .open_response(seal("mac-b", "response", &mac), "op-a", 1000)
                .is_err()
        );
        assert!(
            profile
                .open_response(seal("mac-a", "push", &mac), "op-a", 1000)
                .is_err()
        );
        assert!(
            profile
                .open_response(seal("mac-a", "response", &signing), "op-a", 1000)
                .is_err()
        );
        assert!(
            profile
                .open_response(seal("mac-a", "response", &mac), "op-a", 2001)
                .is_err()
        );
    }
}
