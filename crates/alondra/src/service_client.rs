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
struct Profiles {
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
#[derive(Default)]
struct EpochCache {
    generation: u64,
    value: Option<[u8; 16]>,
}

/// One origin, one pool. No credentials are stored in the transport.
struct RelayTransport {
    origin: String,
    agent: ureq::Agent,
    epoch: std::sync::Mutex<EpochCache>,
}
impl RelayTransport {
    fn new(origin: &str) -> Self {
        Self { origin: origin.into(), agent: http(Duration::from_secs(35)),
            epoch: std::sync::Mutex::new(EpochCache::default()) }
    }
    fn invalidate(&self, used: [u8; 16]) {
        let mut cached = self.epoch.lock().unwrap();
        if cached.value == Some(used) {
            cached.value = None;
            cached.generation += 1;
        }
    }
    fn epoch(&self, deadline: Instant) -> Result<[u8; 16]> {
        let generation = {
            let cached = self.epoch.lock().unwrap();
            if let Some(epoch) = cached.value { return Ok(epoch); }
            cached.generation
        };
        // No lock is held across network I/O, including long polling.
        let mut response = self.agent.get(format!("{}/v1/bootstrap", self.origin))
            .config().timeout_global(Some(remaining(deadline)?.min(Duration::from_secs(35))))
            .build().call()?;
        if response.status().as_u16() != 200 { bail!("relay bootstrap unavailable"); }
        let boot: Value = response.body_mut().with_config().limit(4096).read_json()?;
        remaining(deadline)?;
        if boot["protocol"].as_u64() != Some(1) { bail!("unsupported relay protocol"); }
        let fetched: [u8; 16] = B64.decode(boot["epoch"].as_str().context("missing epoch")?)?
            .try_into().map_err(|_| anyhow!("invalid relay epoch"))?;
        let mut cached = self.epoch.lock().unwrap();
        if cached.generation == generation && cached.value.is_none() {
            cached.value = Some(fetched);
        }
        // An older bootstrap must not overwrite a concurrently refreshed epoch.
        Ok(cached.value.unwrap_or(fetched))
    }
    #[allow(clippy::too_many_arguments)]
    fn call(&self, profile: &Profile, owner: &str, access: &str,
            method: &str, path: &str, body: &[u8], deadline: Instant) -> Result<(u16, Value)> {
        remaining(deadline)?;
        if owner != profile.owner_sub { bail!("terminal approval belongs to another account"); }
        if !matches!(method, "GET" | "POST") { bail!("unsupported relay method"); }
        let epoch = self.epoch(deadline)?;
        let first = self.send(profile, access, method, path, body, epoch, deadline)?;
        if first.0 == 401 {
            self.invalidate(epoch);
            // Only reads can be retried, and only after proving the relay restarted.
            // POST responses, timeouts and disconnects never cause automatic replay.
            if method == "GET" {
                let current = self.epoch(deadline)?;
                if current != epoch {
                    let second = self.send(profile, access, method, path, body, current, deadline)?;
                    self.observe(path, current, &second);
                    return Ok(second);
                }
            }
        } else { self.observe(path, epoch, &first); }
        Ok(first)
    }
    fn observe(&self, path: &str, used: [u8; 16], result: &(u16, Value)) {
        if result.0 == 401 || (result.0 == 409
            && (path == "/v1/changes" || path.starts_with("/v1/changes?"))
            && result.1["error"] == "relay_epoch_changed") {
            self.invalidate(used);
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn send(&self, profile: &Profile, access: &str, method: &str, path: &str,
            body: &[u8], epoch: [u8; 16], deadline: Instant) -> Result<(u16, Value)> {
        let timestamp = now_ms()?;
        let nonce = random::<16>()?;
        let signature = sign_http_challenge(&profile.signing()?, &profile.device_id,
            method, path, &epoch, body, timestamp, &nonce)?;
        let headers = [
            ("authorization", format!("Bearer {access}")),
            ("x-device-id", profile.device_id.clone()),
            ("x-device-timestamp-ms", timestamp.to_string()),
            ("x-device-nonce", B64.encode(nonce)),
            ("x-device-signature", signature),
            ("x-relay-epoch", B64.encode(epoch)),
        ];
        let endpoint = format!("{}{path}", self.origin);
        let timeout = remaining(deadline)?.min(Duration::from_secs(35));
        let mut response = if method == "POST" {
            let mut request = self.agent.post(endpoint).config().timeout_global(Some(timeout)).build()
                .header("content-type", "application/json");
            for (key, value) in &headers { request = request.header(*key, value); }
            request.send(body)?
        } else {
            let mut request = self.agent.get(endpoint).config().timeout_global(Some(timeout)).build();
            for (key, value) in &headers { request = request.header(*key, value); }
            request.call()?
        };
        let status = response.status().as_u16();
        // Fully drain the bounded body so the connection can return to the pool.
        let bytes = response.body_mut().with_config().limit(512 * 1024).read_to_vec()?;
        let value = if bytes.is_empty() { Value::Null } else {
            match serde_json::from_slice(&bytes) {
                Ok(value) => value,
                // Proxy errors need not be JSON. Preserve status (including 401
                // epoch invalidation), without exposing an untrusted response body.
                Err(_) if !(200..300).contains(&status) => Value::Null,
                Err(_) => bail!("invalid relay JSON response (HTTP {status})"),
            }
        };
        remaining(deadline)?;
        Ok((status, value))
    }
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
    save(&profiles)?;
    println!("Paired Mac: {mac}");
    Ok(())
}
pub fn initialize(requested: Option<&str>) -> Result<()> {
    let mac = requested.context("remote delivery requires an explicit Mac ID")?;
    if !valid_id(mac) {
        bail!("invalid Mac ID");
    }
    crate::account::ensure_token_for_enrollment()?;
    let profiles = load()?;
    let token = crate::account::token_for_service()?;
    let profile = profiles
        .profiles
        .into_iter()
        .find(|p| p.mac_id == mac && p.owner_sub == token.sub)
        .context("this Mac has no approval for the signed-in account; run alondra connect <mac-id>")?;
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
        // Account lookup preserves cross-process logout and account changes. Its
        // existing OIDC timeout is independent; never reset the operation deadline.
        let token = crate::account::token_for_service()?;
        remaining(deadline)?;
        static TRANSPORT: OnceLock<RelayTransport> = OnceLock::new();
        TRANSPORT.get_or_init(|| RelayTransport::new(RELAY)).call(
            self, &token.sub, &token.access_token, method, path, body, deadline,
        )
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
        let (status, receipt) = call(
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
            if status == 401 {
                bail!("relay authentication failed (HTTP 401); request was not retried. Check account and device access.");
            }
            bail!("relay refused request (HTTP {status}); request was not retried");
        }
        let receipt_status = receipt["status"].as_str();
        if receipt["request_id"].as_str() != Some(&request_id)
            || !matches!((status, receipt_status),
                (202, Some("queued" | "dispatched" | "received")) | (200, Some("completed"))) {
            bail!("invalid relay submission receipt; inspect destination before repeating an action");
        }
        let mut revision = receipt["revision"].as_u64().filter(|n| *n > 0)
            .context("invalid relay submission revision; inspect destination before repeating an action")?;
        let mut epoch: Option<String> = None;
        loop {
            remaining(deadline)?;
            let (status, job) = call("GET", &format!("/v1/requests/{request_id}"), &[], deadline)?;
            remaining(deadline)?;
            if status != 200 || job["request_id"].as_str() != Some(&request_id) {
                bail!("invalid relay request status ({status})");
            }
            let observed_revision = job["revision"].as_u64().filter(|n| *n > 0)
                .context("invalid relay request revision")?;
            revision = revision.max(observed_revision);
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
        Self::upload_via(name, bytes, timeout, |suffix, body, deadline| {
            let (status, value) = self.request(
                "POST", &format!("/v1/service/uploads/{suffix}"), Some(&body), remaining(deadline)?,
            )?;
            if status >= 400 { bail!("attachment transfer failed ({status})"); }
            Ok(value)
        })
    }
    pub(crate) fn upload_via(name: &str, bytes: &[u8], timeout: Duration,
        mut call: impl FnMut(&str, Value, Instant) -> Result<Value>) -> Result<Value> {
        let started = Instant::now();
        let deadline = started + timeout;
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
        let mut phase = "start";
        let mut confirmed = 0usize;
        let mut accepted = false;
        let result = (|| {
            call("start", json!({"transfer_id":id,"name":name,"bytes":bytes.len(),"sha256":hash}), deadline)?;
            accepted = true;
            phase = "chunk";
            for (index, chunk) in bytes.chunks(64 * 1024).enumerate() {
                call("chunk", json!({"transfer_id":id,"offset":index * 64 * 1024,"data":B64.encode(chunk)}), deadline)?;
                confirmed += chunk.len();
            }
            phase = "finish";
            call("finish", json!({"transfer_id":id}), deadline)
        })();
        // Snapshot before best-effort cancellation: never expose the name, path,
        // transfer ID, credentials or payload in the progress diagnostic.
        let result = result.with_context(|| format!(
            "attachment upload failed: phase={phase}, bytes_confirmed={confirmed}/{}, elapsed_ms={}",
            bytes.len(), started.elapsed().as_millis(),
        ));
        if result.is_err() && accepted {
            let _ = call("cancel", json!({"transfer_id":id}), deadline);
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
mod live_transfer_test;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_store_ignores_unknown_root_fields_but_rewrites_only_current_fields() {
        let profiles: Profiles = serde_json::from_str(
            r#"{"profiles":[],"future_setting":"ignored"}"#,
        )
        .unwrap();
        assert!(profiles.profiles.is_empty());
        let saved = serde_json::to_value(profiles).unwrap();
        assert!(saved.get("future_setting").is_none());
    }

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

    // Socket timeouts are stop-polling opportunities, not request boundaries.
    // Accumulate bytes explicitly: read_line/read_exact can consume a prefix
    // before returning WouldBlock, losing that prefix if their buffers are reset.
    fn read_fixture_request(reader: &mut impl std::io::Read, pending: &mut Vec<u8>,
        stop: &std::sync::atomic::AtomicBool) -> std::io::Result<Option<(String, Vec<u8>)>> {
        use std::io::{Error, ErrorKind};
        loop {
            if stop.load(std::sync::atomic::Ordering::SeqCst) { return Ok(None); }
            if let Some(end) = pending.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&pending[..end+2])
                    .map_err(|_| Error::new(ErrorKind::InvalidData,"invalid fixture HTTP headers"))?;
                let mut length=0usize;
                for line in headers.lines() {
                    if let Some(value)=line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length=value.trim().parse().map_err(|_| Error::new(ErrorKind::InvalidData,"invalid fixture content length"))?;
                    }
                }
                let total=(end+4).checked_add(length)
                    .filter(|total| *total <= 512*1024)
                    .ok_or_else(|| Error::new(ErrorKind::InvalidData,"fixture request too large"))?;
                if pending.len() >= total {
                    let headers=headers.to_owned();
                    let body=pending[end+4..total].to_vec();
                    pending.drain(..total);
                    return Ok(Some((headers,body)));
                }
            }
            if pending.len() >= 512*1024 { return Err(Error::new(ErrorKind::InvalidData,"fixture request too large")); }
            let mut bytes=[0u8;8192];
            match reader.read(&mut bytes) {
                Ok(0) if pending.is_empty() => return Ok(None),
                Ok(0) => return Err(Error::new(ErrorKind::UnexpectedEof,"truncated fixture request")),
                Ok(count) => pending.extend_from_slice(&bytes[..count]),
                Err(error) if matches!(error.kind(),ErrorKind::WouldBlock|ErrorKind::TimedOut|ErrorKind::Interrupted) => continue,
                Err(error) => return Err(error),
            }
        }
    }

    struct HttpFixture {
        origin: String,
        requests: Arc<std::sync::Mutex<Vec<String>>>,
        connections: Arc<std::sync::atomic::AtomicUsize>,
        stop: Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl HttpFixture {
        fn start(replies: Vec<(u16, Value, Duration)>) -> Self {
            Self::start_raw(replies.into_iter().map(|(status, body, delay)|
                (status, serde_json::to_vec(&body).unwrap(), delay)).collect())
        }
        fn start_raw(replies: Vec<(u16, Vec<u8>, Duration)>) -> Self {
            use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
            let connections = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let (seen, accepted, stopping) = (requests.clone(), connections.clone(), stop.clone());
            let thread = std::thread::spawn(move || {
                let mut replies = replies.into_iter();
                while !stopping.load(Ordering::SeqCst) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(value) => value,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2)); continue;
                        }
                        Err(error) => panic!("accept: {error}"),
                    };
                    accepted.fetch_add(1, Ordering::SeqCst);
                    stream.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
                    let mut pending = Vec::new();
                    loop {
                        let Some((headers, _body)) = read_fixture_request(&mut stream, &mut pending, &stopping)
                            .expect("read fixture request") else { break; };
                        seen.lock().unwrap().push(headers);
                        let Some((status, body, delay)) = replies.next() else { panic!("unexpected replay"); };
                        std::thread::sleep(delay);
                        let header = format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n", body.len());
                        if stream.write_all(header.as_bytes()).and_then(|_| stream.write_all(&body)).is_err() { break; }
                    }
                }
            });
            Self { origin, requests, connections, stop, thread: Some(thread) }
        }
        fn count(&self) -> usize { self.requests.lock().unwrap().len() }
    }
    impl Drop for HttpFixture {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                if let Err(panic) = thread.join() {
                    // Preserve a fixture failure on normal teardown, but never
                    // abort the entire suite with a second panic during unwind.
                    if !std::thread::panicking() { std::panic::resume_unwind(panic); }
                }
            }
        }
    }
    #[test]
    fn fixture_retains_fragmented_headers_and_body_across_socket_timeouts() {
        use std::collections::VecDeque;
        use std::io::{Error,ErrorKind,Read};
        struct Reads(VecDeque<std::result::Result<Vec<u8>,ErrorKind>>);
        impl Read for Reads {
            fn read(&mut self, out:&mut [u8]) -> std::io::Result<usize> {
                match self.0.pop_front() {
                    Some(Ok(bytes)) => { assert!(bytes.len()<=out.len()); out[..bytes.len()].copy_from_slice(&bytes); Ok(bytes.len()) },
                    Some(Err(kind)) => Err(Error::from(kind)),
                    None => Ok(0),
                }
            }
        }
        let mut reader=Reads(VecDeque::from([
            Ok(b"POST /v1/req".to_vec()), Err(ErrorKind::WouldBlock),
            Ok(b"uests HTTP/1.1\r\nContent-L".to_vec()), Err(ErrorKind::TimedOut),
            Ok(b"ength: 4\r\n\r".to_vec()), Err(ErrorKind::Interrupted),
            Ok(b"\n\0".to_vec()), Err(ErrorKind::WouldBlock), Err(ErrorKind::TimedOut),
            Ok(b"\xff{}GET /next HTTP/1.1\r\n\r\n".to_vec()),
        ]));
        let stop=std::sync::atomic::AtomicBool::new(false);
        let mut pending=Vec::new();
        let (headers,body)=read_fixture_request(&mut reader,&mut pending,&stop).unwrap().unwrap();
        assert_eq!(headers,"POST /v1/requests HTTP/1.1\r\nContent-Length: 4\r\n");
        assert_eq!(body,b"\0\xff{}");
        assert!(!pending.is_empty(),"next request bytes must remain on the same connection");
        let (headers,body)=read_fixture_request(&mut reader,&mut pending,&stop).unwrap().unwrap();
        assert_eq!(headers,"GET /next HTTP/1.1\r\n");
        assert!(body.is_empty());
        assert!(read_fixture_request(&mut reader,&mut pending,&stop).unwrap().is_none());
        let mut truncated=std::io::Cursor::new(b"POST / HTTP/1.1\r\nContent-Length: 4\r\n\r\n{}".to_vec());
        assert_eq!(read_fixture_request(&mut truncated,&mut pending,&stop).unwrap_err().kind(),ErrorKind::UnexpectedEof);
    }

    #[test]
    fn fixture_drop_preserves_server_failure_without_double_panic_during_unwind() {
        fn failed_fixture() -> HttpFixture {
            HttpFixture {
                origin:String::new(),requests:Arc::new(std::sync::Mutex::new(Vec::new())),
                connections:Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                stop:Arc::new(std::sync::atomic::AtomicBool::new(false)),
                thread:Some(std::thread::spawn(|| panic!("server failure"))),
            }
        }
        let normal=std::panic::catch_unwind(|| drop(failed_fixture())).unwrap_err();
        assert_eq!(normal.downcast_ref::<&str>(),Some(&"server failure"));
        let unwinding=std::panic::catch_unwind(|| {
            let _fixture=failed_fixture();
            panic!("original assertion failure");
        }).unwrap_err();
        assert_eq!(unwinding.downcast_ref::<&str>(),Some(&"original assertion failure"));
    }

    fn boot(epoch: u8) -> (u16, Value, Duration) {
        (200, json!({"protocol":1,"epoch":B64.encode([epoch;16])}), Duration::ZERO)
    }
    fn reply(status: u16, error: &str) -> (u16, Value, Duration) {
        (status, json!({"error":error}), Duration::ZERO)
    }
    fn transport_call(t: &RelayTransport, method: &str) -> Result<(u16, Value)> {
        t.call(&fixture().0, "owner-a", "test-bearer", method, "/v1/requests", if method == "POST" { b"{}" } else { b"" }, Instant::now()+Duration::from_secs(2))
    }

    #[test]
    fn proxy_non_json_status_is_preserved_without_post_replay_or_body_exposure() {
        for status in [401, 502] {
            let bootstrap = serde_json::to_vec(&boot(1).1).unwrap();
            let server = HttpFixture::start_raw(vec![(200, bootstrap, Duration::ZERO),
                (status, b"<html>private proxy diagnostic</html>".to_vec(), Duration::ZERO)]);
            let transport = RelayTransport::new(&server.origin);
            let response = transport_call(&transport, "POST").unwrap();
            assert_eq!(response, (status, Value::Null));
            assert_eq!(server.requests.lock().unwrap().len(), 2);
            if status == 401 { assert!(transport.epoch.lock().unwrap().value.is_none()); }
        }
    }

    #[test]
    fn successful_non_json_response_remains_an_error_without_body_exposure() {
        let bootstrap = serde_json::to_vec(&boot(1).1).unwrap();
        let server = HttpFixture::start_raw(vec![(200, bootstrap, Duration::ZERO),
            (200, b"private invalid response".to_vec(), Duration::ZERO)]);
        let error = transport_call(&RelayTransport::new(&server.origin), "POST").unwrap_err();
        assert_eq!(error.to_string(), "invalid relay JSON response (HTTP 200)");
        assert_eq!(server.requests.lock().unwrap().len(), 2);
    }

    #[test]
    fn submission_401_does_not_diagnose_revocation_or_repeat_action() {
        let mut calls = 0;
        let error = fixture().0.request_via("POST", "/v1/action", None,
            Instant::now()+Duration::from_secs(1), |method, path, _, _| {
                calls += 1;
                assert_eq!((method, path), ("POST", "/v1/requests"));
                Ok((401, Value::Null))
            }).unwrap_err();
        assert_eq!(calls, 1);
        assert!(error.to_string().contains("HTTP 401"));
        assert!(!error.to_string().contains("--repair"));
    }

    #[test]
    fn upload_failure_reports_acknowledged_progress_before_cancel() {
        let mut calls = Vec::new();
        let mut original_deadline = None;
        let result = Profile::upload_via("private-name.txt", &vec![7; 65537], Duration::from_secs(1),
            |phase, _, deadline| {
                if let Some(expected) = original_deadline { assert_eq!(deadline, expected); }
                original_deadline = Some(deadline);
                calls.push(phase.to_owned());
                match calls.len() {
                    1 | 2 => Ok(json!({"ok":true})),
                    3 => bail!("deadline exceeded"),
                    4 => { assert_eq!(phase,"cancel"); bail!("cancel also failed") },
                    _ => panic!("unexpected retry"),
                }
            });
        assert_eq!(calls,["start","chunk","chunk","cancel"]);
        let error = result.unwrap_err();
        let diagnostic = error.to_string();
        assert!(diagnostic.starts_with("attachment upload failed: phase=chunk, bytes_confirmed=65536/65537, elapsed_ms="));
        assert!(!diagnostic.contains("private-name"));
        assert!(format!("{error:#}").contains("deadline exceeded"));
        assert!(!format!("{error:#}").contains("cancel also failed"));
    }

    #[test]
    fn upload_start_and_finish_failures_keep_distinct_progress() {
        for failed_phase in ["start", "finish"] {
            let mut phases = Vec::new();
            let error = Profile::upload_via("private.txt", &[1,2,3], Duration::from_secs(1),
                |phase, _, _| { phases.push(phase.to_owned());
                    if phase == failed_phase { bail!("rejected"); } Ok(json!({"ok":true}))
                }).unwrap_err();
            let message = error.to_string();
            assert!(message.contains(&format!("phase={failed_phase}")));
            if failed_phase == "start" {
                assert_eq!(phases,["start"]);
                assert!(message.contains("bytes_confirmed=0/3"));
            } else {
                assert_eq!(phases,["start","chunk","finish","cancel"]);
                assert!(message.contains("bytes_confirmed=3/3"));
            }
        }
    }

    #[test]
    fn pooled_transport_drains_error_bodies_and_bootstraps_once() {
        let server = HttpFixture::start(vec![boot(1), reply(400,"bad_request"), reply(409,"conflict"), reply(200,"ok")]);
        let transport = RelayTransport::new(&server.origin);
        assert_eq!(transport_call(&transport,"GET").unwrap().0,400);
        assert_eq!(transport_call(&transport,"POST").unwrap().0,409);
        assert_eq!(transport_call(&transport,"GET").unwrap().0,200);
        assert_eq!(server.count(),4);
        assert_eq!(server.connections.load(std::sync::atomic::Ordering::SeqCst),1);
    }

    #[test]
    fn changed_epoch_retries_read_once_with_new_nonce_but_never_post() {
        let server = HttpFixture::start(vec![boot(1),reply(401,"unauthorized"),boot(2),reply(401,"unauthorized")]);
        let transport = RelayTransport::new(&server.origin);
        assert_eq!(transport_call(&transport,"GET").unwrap().0,401);
        let seen = server.requests.lock().unwrap();
        assert_eq!(seen.len(),4);
        assert!(seen[1].contains(&B64.encode([1;16])));
        assert!(seen[3].contains(&B64.encode([2;16])));
        let nonce = |headers: &str| headers.lines().find(|l| l.starts_with("x-device-nonce:")).unwrap().to_string();
        assert_ne!(nonce(&seen[1]),nonce(&seen[3]));
        drop(seen);
        let server = HttpFixture::start(vec![boot(1),reply(401,"unauthorized")]);
        let transport = RelayTransport::new(&server.origin);
        assert_eq!(transport_call(&transport,"POST").unwrap().0,401);
        assert_eq!(server.count(),2);
        assert!(transport.epoch.lock().unwrap().value.is_none());
    }

    #[test]
    fn unchanged_epoch_and_cursor_conflict_do_not_trigger_replay() {
        let server = HttpFixture::start(vec![boot(1),reply(401,"unauthorized"),boot(1),reply(409,"stale_cursor"),reply(200,"ok")]);
        let transport = RelayTransport::new(&server.origin);
        assert_eq!(transport_call(&transport,"GET").unwrap().0,401);
        assert_eq!(server.count(),3);
        assert_eq!(transport_call(&transport,"GET").unwrap().0,409);
        assert_eq!(transport_call(&transport,"GET").unwrap().0,200);
        assert_eq!(server.count(),5);
    }

    #[test]
    fn relay_epoch_conflict_invalidates_and_old_failure_cannot_erase_new_epoch() {
        let server = HttpFixture::start(vec![boot(1),reply(409,"relay_epoch_changed"),boot(2),reply(200,"ok")]);
        let transport = RelayTransport::new(&server.origin);
        assert_eq!(transport.call(&fixture().0,"owner-a","token","GET","/v1/changes?since=0",b"",Instant::now()+Duration::from_secs(2)).unwrap().0,409);
        assert_eq!(transport_call(&transport,"GET").unwrap().0,200);
        transport.invalidate([1;16]);
        assert_eq!(transport.epoch.lock().unwrap().value,Some([2;16]));
        assert_eq!(server.count(),4);
        assert!(transport.call(&fixture().0,"other-owner","token","POST","/v1/requests",b"{}",Instant::now()+Duration::from_secs(1)).is_err());
        assert_eq!(server.count(),4,"account switch must fail before network I/O");
    }

    #[test]
    fn late_bootstrap_does_not_overwrite_newer_concurrent_epoch() {
        let mut bootstrap = boot(1); bootstrap.2=Duration::from_millis(120);
        let server = HttpFixture::start(vec![bootstrap]);
        let transport = RelayTransport::new(&server.origin);
        std::thread::scope(|scope| {
            let pending = scope.spawn(|| transport.epoch(Instant::now()+Duration::from_secs(2)).unwrap());
            let deadline = Instant::now()+Duration::from_secs(1);
            while server.count() == 0 {
                assert!(Instant::now()<deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            {
                let mut newer = transport.epoch.lock().unwrap();
                newer.generation += 1;
                newer.value = Some([2;16]);
            }
            assert_eq!(pending.join().unwrap(),[2;16]);
        });
        assert_eq!(transport.epoch.lock().unwrap().value,Some([2;16]));
    }

    #[test]
    fn bootstrap_and_request_share_deadline_and_post_timeout_does_not_replay() {
        let mut bootstrap = boot(1); bootstrap.2=Duration::from_millis(120);
        let mut slow = reply(200,"ok"); slow.2=Duration::from_millis(120);
        let server = HttpFixture::start(vec![bootstrap,slow]);
        let transport = RelayTransport::new(&server.origin);
        assert!(transport.call(&fixture().0,"owner-a","token","POST","/v1/requests",b"{}",Instant::now()+Duration::from_millis(200)).is_err());
        assert_eq!(server.count(),2,"timed out POST must not be repeated");
    }

    #[test]
    fn queued_operation_is_encrypted_and_waits_for_its_correlated_reply_without_resubmitting() {
        let (profile, mac, mac_agreement) = fixture();
        let mut stage = 0;
        let mut request_id = String::new();
        let mut retained: Vec<Value> = (490..=1000).map(|revision|
            json!({"request_id":format!("historical-{revision}"),"status":"completed","revision":revision}))
            .collect();
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
                    retained.push(json!({"request_id":request_id,"status":"queued","revision":1001}));
                    Ok((202,json!({"request_id":request_id,"status":"queued","revision":1001})))
                }
                2 => { assert_eq!(path, format!("/v1/requests/{request_id}")); Ok((200,json!({"request_id":request_id,"status":"queued","revision":1001,"response":null}))) }
                3 => {
                    assert_eq!(method,"GET");
                    assert!(path.starts_with("/v1/changes?"));
                    let wait_s = url::form_urlencoded::parse(path.split_once('?').unwrap().1.as_bytes())
                        .find(|(key,_)| key == "wait_s").expect("long poll wait parameter").1.parse::<u64>().unwrap();
                    assert!(wait_s <= 25);
                    // The request was created just before this callback. Permit at
                    // most the second crossed while entering it, never a fresh budget.
                    assert!(wait_s <= deadline.saturating_duration_since(Instant::now()).as_secs().saturating_add(1));
                    let since = url::form_urlencoded::parse(path.split_once('?').unwrap().1.as_bytes())
                        .find(|(key,_)| key == "since").unwrap().1.parse::<u64>().unwrap();
                    assert_eq!(since,1001,"the receipt and observed state are already known");
                    *retained.iter_mut().find(|event| event["request_id"] == request_id).unwrap() =
                        json!({"request_id":request_id,"status":"completed","revision":1002});
                    let changes: Vec<_> = retained.iter().filter(|event| event["revision"].as_u64().unwrap()>since).cloned().collect();
                    assert_eq!(changes.len(),1,"historical jobs and our own queued event must not trigger another GET");
                    assert_eq!(changes[0]["request_id"],request_id);
                    Ok((200,json!({"epoch":B64.encode([9;16]),"revision":1002,"changes":changes})))
                }
                4 => {
                    assert_eq!(path, format!("/v1/requests/{request_id}"));
                    let envelope = EnvelopeV1::seal_random(EnvelopeContext { route:"mac-a",sender:"mac-a",recipient:"client-a",request_id:&request_id,kind:"response",expires_at_ms:now_ms()?+5000 }, br#"{"status":200,"body":{"ok":true}}"#, &mac, &profile.agreement()?.public_key())?;
                    Ok((200,json!({"request_id":request_id,"status":"completed","revision":1002,"response":envelope})))
                }
                _ => panic!("operation unexpectedly resubmitted"),
            }
        }).unwrap();
        assert_eq!(result, (200, json!({"ok":true})));
        assert_eq!(stage, 4);
    }

    #[test]
    fn invalid_receipts_fail_closed_without_retrying_submission() {
        for invalid in ["missing-id","wrong-id","missing-status","wrong-status","missing-revision","fractional-revision","negative-revision","zero-revision"] {
            let mut calls=0;
            let result=fixture().0.request_via("POST","/v1/action",None,Instant::now()+Duration::from_secs(1),
                |method,_,bytes,_| {
                    calls+=1;
                    assert_eq!(method,"POST");
                    let envelope: EnvelopeV1=serde_json::from_slice(bytes)?;
                    let mut receipt=json!({"request_id":envelope.request_id,"status":"queued","revision":1001});
                    match invalid {
                        "missing-id" => { receipt.as_object_mut().unwrap().remove("request_id"); },
                        "wrong-id" => receipt["request_id"]=json!("other-request"),
                        "missing-status" => { receipt.as_object_mut().unwrap().remove("status"); },
                        "wrong-status" => receipt["status"]=json!("completed"),
                        "missing-revision" => { receipt.as_object_mut().unwrap().remove("revision"); },
                        "fractional-revision" => receipt["revision"]=json!(1.5),
                        "negative-revision" => receipt["revision"]=json!(-1),
                        "zero-revision" => receipt["revision"]=json!(0),
                        _ => unreachable!(),
                    }
                    Ok((202,receipt))
                });
            assert!(result.is_err(),"accepted invalid receipt: {invalid}");
            assert_eq!(calls,1,"invalid receipt must not repeat a possibly accepted mutation");
        }
    }

    #[test]
    fn observed_job_revision_advances_cursor_and_missing_revision_stops_polling() {
        for revision in [Some(1003),None] {
            let mut calls=0;
            let mut id=String::new();
            let result=fixture().0.request_via("POST","/v1/action",None,Instant::now()+Duration::from_secs(1),
                |_,path,bytes,_| {
                    calls+=1;
                    match calls {
                        1 => { id=serde_json::from_slice::<EnvelopeV1>(bytes)?.request_id;
                            Ok((202,json!({"request_id":id,"status":"queued","revision":1001}))) },
                        2 => Ok((200,json!({"request_id":id,"status":"received","revision":revision,"response":null}))),
                        3 => { assert!(path.starts_with("/v1/changes?since=1003&")); bail!("stop after verified poll") },
                        _ => panic!("unexpected replay"),
                    }
                });
            assert!(result.is_err());
            assert_eq!(calls,if revision.is_some() {3} else {2});
        }
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
