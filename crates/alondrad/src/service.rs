//! Optional outbound Mac connector. Enabled only by `~/.alondra/service.toml`.
//! The relay never supplies trusted endpoint keys: those are pinned in this Mac's config.
//! A device must also remain approved in the local pairing registry. Enrollment is a one-time
//! durable attempt per device ID so a relay-side revocation survives reconnects and restarts.
use crate::auth::Auth;
use crate::push::{valid_push_binding, in_quiet_hours, local_minutes};
use alondra_e2e::{Context as EnvelopeContext, EnvelopeV1, sign_http_challenge};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::{
    PublicKey, SecretKey,
    ecdsa::{SigningKey, VerifyingKey},
};
use reqwest::{Client, Method, StatusCode, Url};
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

// Base64 adds ~33%; 180 KiB plus envelope metadata stays below the relay's 256 KiB cap.
const MAX_LOCAL_RESPONSE: usize = 180 * 1024;
const MAX_RELAY_RESPONSE: usize = 256 * 1024;
// The relay wraps the envelope in a 4 KiB APNs alert. Leave room for its
// generic aps fields, binding, JSON escaping, and future metadata.
const MAX_PUSH_BODY: usize = 3_000;
const MAX_PUSH_PLAINTEXT: usize = 1_500;

/// Device preferences are stored on the executor and travel only inside E2E
/// envelopes. Clock semantics deliberately match legacy: executor local time.
#[derive(Default, Debug, PartialEq)]
struct QuietHours { start: Option<String>, end: Option<String> }
impl QuietHours {
    fn parse_update(start: Option<&Value>, end: Option<&Value>) -> Result<Option<Self>> {
        match (start, end) {
            (None, None) => Ok(None),
            (Some(Value::Null), Some(Value::Null)) => Ok(Some(Self::default())),
            (Some(Value::String(start)), Some(Value::String(end))) if Self::valid_time(start) && Self::valid_time(end) =>
                Ok(Some(Self { start: Some(start.clone()), end: Some(end.clone()) })),
            _ => bail!("quiet_start and quiet_end must both be HH:mm or both null"),
        }
    }
    fn valid_time(value: &str) -> bool {
        let b = value.as_bytes();
        b.len() == 5 && b[2] == b':' && [b[0], b[1], b[3], b[4]].iter().all(u8::is_ascii_digit)
            && value[..2].parse::<u32>().is_ok_and(|h| h < 24)
            && value[3..].parse::<u32>().is_ok_and(|m| m < 60)
    }
    fn apply(&self, payload: &Value, with_sound: bool, now: u32) -> (Value, bool) {
        let quiet = in_quiet_hours(self.start.as_deref(), self.end.as_deref(), now);
        let mut payload = payload.clone();
        payload["aps"]["interruption-level"] = json!(if quiet { "passive" } else { "active" });
        if (quiet || !with_sound) && let Some(aps) = payload["aps"].as_object_mut() { aps.remove("sound"); }
        (payload, with_sound && !quiet)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    relay_url: String,
    mac_id: String,
    owner_sub: String,
    signing_key_path: PathBuf,
    agreement_key_path: PathBuf,
    devices: Vec<DeviceConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceConfig {
    device_id: String,
    signing_key: String,
    agreement_key: String,
}

struct Device {
    signing: VerifyingKey,
    agreement: PublicKey,
    signing_wire: String,
}

pub struct Service {
    relay: Url,
    client: Client,
    mac_id: String,
    owner_sub: String,
    signing: SigningKey,
    agreement: SecretKey,
    devices: HashMap<String, Device>,
    auth: Arc<Auth>,
    journal: Journal,
    local_port: u16,
    home: PathBuf,
    transfers: Arc<crate::service_files::Transfers>,
}

#[derive(Deserialize)]
struct Work {
    request_id: String,
    envelope: EnvelopeV1,
}
#[derive(Deserialize)]
struct Bootstrap {
    epoch: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalRequest {
    method: String,
    path: String,
    #[serde(default)]
    body: Option<Value>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn decoded(s: &str) -> Result<Vec<u8>> {
    Ok(URL_SAFE_NO_PAD.decode(s.trim())?)
}

fn private_key(path: &Path) -> Result<[u8; 32]> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
            bail!("private key file {} must be owner-only", path.display());
        }
    }
    let raw = decoded(&std::fs::read_to_string(path)?)?;
    raw.try_into()
        .map_err(|_| anyhow!("private key {} must be 32 bytes", path.display()))
}

fn resolve(home: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        home.join(path)
    }
}

impl Service {
    pub fn open(home: &Path, local_port: u16, auth: Arc<Auth>) -> Result<Option<Self>> {
        let file = home.join("service.toml");
        if !file.exists() {
            return Ok(None);
        }
        let config: Config = toml::from_str(&std::fs::read_to_string(&file)?)
            .with_context(|| format!("invalid {}", file.display()))?;
        if config.mac_id.is_empty() || config.owner_sub.is_empty() {
            bail!("service Mac ID and owner subject are required");
        }
        let relay = Url::parse(&config.relay_url)?;
        let loopback =
            relay.scheme() == "http" && matches!(relay.host_str(), Some("127.0.0.1" | "localhost"));
        if !(relay.scheme() == "https" || loopback)
            || relay.path() != "/"
            || relay.query().is_some()
            || relay.fragment().is_some()
            || !relay.username().is_empty()
            || relay.password().is_some()
        {
            bail!("relay_url must be an HTTPS origin (loopback HTTP allowed for tests)");
        }
        let signing =
            SigningKey::from_slice(&private_key(&resolve(home, &config.signing_key_path))?)?;
        let agreement =
            SecretKey::from_slice(&private_key(&resolve(home, &config.agreement_key_path))?)?;
        let mut devices = HashMap::new();
        for d in config.devices {
            if d.device_id.is_empty() || d.device_id == config.mac_id {
                bail!("invalid approved device ID");
            }
            let signing_key = VerifyingKey::from_sec1_bytes(&decoded(&d.signing_key)?)?;
            let agreement_key = PublicKey::from_sec1_bytes(&decoded(&d.agreement_key)?)?;
            if devices
                .insert(
                    d.device_id,
                    Device {
                        signing: signing_key,
                        agreement: agreement_key,
                        signing_wire: d.signing_key,
                    },
                )
                .is_some()
            {
                bail!("duplicate approved device ID");
            }
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            // PR merge and GitHub issue refresh can take close to 90 seconds locally.
            .timeout(Duration::from_secs(95))
            .build()?;
        let journal = Journal::open(home.join("service-receipts.sqlite"))?;
        Ok(Some(Self {
            relay,
            client,
            mac_id: config.mac_id,
            owner_sub: config.owner_sub,
            signing,
            agreement,
            devices,
            auth,
            journal,
            local_port,
            home: home.to_path_buf(),
            transfers: Arc::default(),
        }))
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        Ok(self.relay.join(path.trim_start_matches('/'))?)
    }

    async fn bootstrap(&self) -> Result<[u8; 16]> {
        let response = self
            .client
            .get(self.endpoint("/v1/bootstrap")?)
            .send()
            .await?
            .error_for_status()?;
        let boot: Bootstrap = response.json().await?;
        decoded(&boot.epoch)?
            .try_into()
            .map_err(|_| anyhow!("invalid relay epoch"))
    }

    async fn signed(
        &self,
        method: Method,
        path: &str,
        body: Vec<u8>,
        epoch: &[u8; 16],
    ) -> Result<reqwest::Response> {
        let mut nonce = [0u8; 16];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| anyhow!("OS randomness failed"))?;
        let timestamp = now_ms();
        let signature = sign_http_challenge(
            &self.signing,
            &self.mac_id,
            method.as_str(),
            path,
            epoch,
            &body,
            timestamp,
            &nonce,
        )
        .map_err(|e| anyhow!("sign request: {e}"))?;
        Ok(self
            .client
            .request(method, self.endpoint(path)?)
            .header("x-device-id", &self.mac_id)
            .header("x-device-timestamp-ms", timestamp.to_string())
            .header("x-device-nonce", URL_SAFE_NO_PAD.encode(nonce))
            .header("x-device-signature", signature)
            .header("x-relay-epoch", URL_SAFE_NO_PAD.encode(epoch))
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await?)
    }

    async fn enroll(&self, epoch: &[u8; 16]) -> Result<()> {
        for (id, device) in &self.devices {
            if !self.auth.approved_key_matches(id, &device.signing) {
                continue;
            }
            // Record one attempt before any network write. A relay-side revocation must
            // survive retries and restarts; a new approval needs a new device ID.
            match self
                .journal
                .reserve_enrollment(id, &device.signing.to_sec1_bytes())?
            {
                EnrollmentDecision::New => {}
                EnrollmentDecision::Pending => {
                    log!(
                        "service device {id}: enrollment outcome unknown; approve again with a new device ID"
                    );
                    continue;
                }
                EnrollmentDecision::Active | EnrollmentDecision::Revoked => continue,
            }
            let body = serde_json::to_vec(
                &json!({"device_id":id,"owner_sub":self.owner_sub,"signing_key":device.signing_wire}),
            )?;
            self.signed(Method::POST, "/v1/devices", body, epoch)
                .await?
                .error_for_status()?;
            if self.auth.approved_key_matches(id, &device.signing) {
                self.journal.activate_enrollment(id)?;
            } else {
                self.journal.revoke_enrollment(id)?;
                let path = format!("/v1/devices/{id}");
                self.signed(Method::DELETE, &path, Vec::new(), epoch)
                    .await?
                    .error_for_status()?;
            }
        }
        Ok(())
    }

    async fn sync_revocations(&self, epoch: &[u8; 16], synced: &mut HashSet<String>) -> Result<()> {
        // Walk the durable enrollment journal, not service.toml. An operator may
        // remove a device from that file while the connector is stopped; the
        // relay must still receive its signed revocation on the next start.
        for (id, state) in self.journal.enrollments()? {
            let approved = self
                .devices
                .get(&id)
                .is_some_and(|device| self.auth.approved_key_matches(&id, &device.signing));
            if approved && state == "active" {
                continue;
            }
            self.journal.revoke_enrollment(&id)?;
            if synced.contains(&id) {
                continue;
            }
            let path = format!("/v1/devices/{id}");
            self.signed(Method::DELETE, &path, Vec::new(), epoch)
                .await?
                .error_for_status()?;
            synced.insert(id);
        }
        Ok(())
    }

    pub async fn run(self: Arc<Self>) {
        let mut enrolled_epoch: Option<[u8; 16]> = None;
        let mut revoked_this_epoch = HashSet::new();
        loop {
            let result = async {
                let epoch = self.bootstrap().await?;
                if enrolled_epoch != Some(epoch) {
                    revoked_this_epoch.clear();
                }
                self.sync_revocations(&epoch, &mut revoked_this_epoch)
                    .await?;
                if enrolled_epoch != Some(epoch) {
                    self.enroll(&epoch).await?;
                    enrolled_epoch = Some(epoch);
                }
                let path = "/v1/work?wait_s=25";
                let response = self
                    .signed(Method::GET, path, Vec::new(), &epoch)
                    .await?
                    .error_for_status()?;
                if response.status() == StatusCode::NO_CONTENT {
                    return Ok::<_, anyhow::Error>(());
                }
                let work: Work = response.json().await?;
                self.handle(work, &epoch).await
            }
            .await;
            if let Err(e) = result {
                log!("service connector: {e:#}");
                enrolled_epoch = None;
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }

    async fn handle(&self, work: Work, epoch: &[u8; 16]) -> Result<()> {
        let e = &work.envelope;
        if e.request_id != work.request_id
            || e.kind != "request"
            || e.recipient != self.mac_id
            || e.route != self.mac_id
        {
            bail!("work route mismatch");
        }
        let device = self
            .devices
            .get(&e.sender)
            .context("work from unapproved device")?;
        if !self.auth.approved_key_matches(&e.sender, &device.signing) {
            bail!("work from locally revoked device");
        }
        if !self.journal.enrollment_active(&e.sender)? {
            bail!("work from device without active local service enrollment");
        }
        let plaintext = e
            .open(
                &device.signing,
                &self.agreement,
                &e.sender,
                &self.mac_id,
                &self.mac_id,
                now_ms(),
            )
            .map_err(|error| anyhow!("invalid encrypted work: {error}"))?;
        let request: LocalRequest = serde_json::from_slice(&plaintext)?;
        let body = serde_json::to_vec(e)?;
        let digest: [u8; 32] = Sha256::digest(&body).into();
        let disposition = self.journal.accept(&e.sender, &e.request_id, &digest)?;
        let receipt_path = format!("/v1/work/{}/receipt", e.request_id);
        self.signed(Method::POST, &receipt_path, Vec::new(), epoch)
            .await?
            .error_for_status()?;
        let response = match disposition {
            Disposition::Completed(bytes) => bytes,
            Disposition::Executing => {
                let bytes = self.encode_response(e, device, &json!({"status":409,"error":"uncertain","detail":"execution was interrupted; inspect the Mac before retrying this action"}), &request)?;
                self.journal.complete(&e.sender, &e.request_id, &bytes)?;
                bytes
            }
            Disposition::Accepted => {
                if !self.auth.approved_key_matches(&e.sender, &device.signing) {
                    bail!("device was revoked before execution");
                }
                self.journal.executing(&e.sender, &e.request_id, &digest)?;
                let result = match self.dispatch(&e.sender, &request).await {
                    Ok(value) => value,
                    Err(error) if error.is::<ResponseTooLarge>() => {
                        json!({"status":502,"error":"response_too_large","detail":response_size_detail(&request)})
                    }
                    Err(error) if request.method == "GET" => {
                        json!({"status":502,"error":"read_failed","detail":error.to_string()})
                    }
                    Err(error) => {
                        json!({"status":503,"error":"uncertain","detail":error.to_string()})
                    }
                };
                let bytes = self.encode_response(e, device, &result, &request)?;
                self.journal.complete(&e.sender, &e.request_id, &bytes)?;
                bytes
            }
        };
        let response_path = format!("/v1/work/{}/response", e.request_id);
        self.signed(Method::POST, &response_path, response, epoch)
            .await?
            .error_for_status()?;
        Ok(())
    }

    fn encode_response(
        &self,
        request: &EnvelopeV1,
        device: &Device,
        result: &Value,
        local_request: &LocalRequest,
    ) -> Result<Vec<u8>> {
        let seal = |value: &Value| -> Result<Vec<u8>> {
            let payload = serde_json::to_vec(value)?;
            let envelope = EnvelopeV1::seal_random(
                EnvelopeContext {
                    route: &self.mac_id,
                    sender: &self.mac_id,
                    recipient: &request.sender,
                    request_id: &request.request_id,
                    kind: "response",
                    expires_at_ms: now_ms() + 600_000,
                },
                &payload,
                &self.signing,
                &device.agreement,
            )
            .map_err(|error| anyhow!("seal response: {error}"))?;
            Ok(serde_json::to_vec(&envelope)?)
        };
        let bytes = seal(result)?;
        if bytes.len() <= MAX_RELAY_RESPONSE {
            return Ok(bytes);
        }
        // A JSON string containing many escaped bytes can expand far beyond the
        // raw local response cap. Persist a small encrypted error as the receipt.
        let fallback = seal(&json!({"status":502,"error":"response_too_large","detail":response_size_detail(local_request)}))?;
        if fallback.len() > MAX_RELAY_RESPONSE {
            bail!("encrypted fallback response exceeds relay limit");
        }
        Ok(fallback)
    }

    async fn dispatch(&self, sender: &str, request: &LocalRequest) -> Result<Value> {
        if request.method == "POST" && request.path == "/v1/service/push-binding" {
            let body = request.body.as_ref().context("push binding body required")?;
            let map = body.as_object().context("invalid push binding body")?;
            if map.keys().any(|key| !matches!(key.as_str(), "push_binding" | "quiet_start" | "quiet_end" | "preferences_revision")) {
                bail!("unknown push binding field");
            }
            let binding = map.get("push_binding").and_then(Value::as_str)
                .filter(|value| valid_push_binding(value)).context("invalid push binding")?;
            let quiet = QuietHours::parse_update(map.get("quiet_start"), map.get("quiet_end"))?;
            let revision = match map.get("preferences_revision") {
                None => None,
                Some(value) => Some(value.as_u64().filter(|v| (1..=9_007_199_254_740_991).contains(v))
                    .context("preferences_revision must be an integer from 1 to 9007199254740991")? as i64),
            };
            if revision.is_some() && quiet.is_none() { bail!("versioned preferences require quiet_start and quiet_end"); }
            match self.journal.set_versioned_push_preferences(sender, binding, quiet, revision)? {
                Some(current) => return Ok(json!({"status":409,"error":"stale_push_preferences","current_revision":current})),
                None => return Ok(json!({"status":204})),
            }
        }
        if matches!(request.path.as_str(), "/v1/service/uploads/start" | "/v1/service/uploads/chunk" | "/v1/service/uploads/finish" | "/v1/service/uploads/cancel" | "/v1/service/files/chunk") {
            anyhow::ensure!(request.method == "POST", "file transfers require POST");
            let body = request.body.clone().context("file transfer body required")?;
            let transfers = self.transfers.clone();
            let home = self.home.clone();
            let sender = sender.to_owned();
            let path = request.path.clone();
            let result = tokio::task::spawn_blocking(move || transfers.dispatch(&home, &sender, &path, body)).await?;
            return Ok(match result {
                Ok(body) => json!({"status":200,"body":body}),
                Err(error) => json!({"status":422,"body":{"error":crate::service_files::error_code(&error)}}),
            });
        }
        let method = approved_method(request)?;
        let url = format!("http://127.0.0.1:{}{}", self.local_port, request.path);
        let mut builder = self.client.request(method, url);
        if let Some(body) = &request.body {
            builder = builder.json(body);
        }
        let response = builder.send().await?;
        let status = response.status().as_u16();
        let bytes = bounded_response(response).await?;
        let file_success = request.method == "GET"
            && request.path.split('?').next() == Some("/v1/file")
            && (200..300).contains(&status);
        // File contents are text even when they happen to be valid JSON. Error
        // responses and JSON API endpoints retain their structured bodies.
        let body = if file_success {
            json!({"text":String::from_utf8_lossy(&bytes)})
        } else {
            serde_json::from_slice::<Value>(&bytes)
                .unwrap_or_else(|_| json!({"text":String::from_utf8_lossy(&bytes)}))
        };
        Ok(json!({"status":status,"body":body}))
    }

    /// Send one opaque notification per enrolled phone. The relay can see only
    /// routing metadata, a random binding, and the generic APNs alert.
    pub async fn broadcast_push(&self, payload: &Value, with_sound: bool) -> Result<()> {
        let mut epoch = None;
        for (id, device) in &self.devices {
            if !self.auth.approved_key_matches(id, &device.signing)
                || !self.journal.enrollment_active(id)? {
                continue;
            }
            let Some(binding) = self.journal.push_binding(id)? else { continue };
            if let Err(error) = self.send_device_push(id, device, &binding, payload, with_sound, &mut epoch).await {
                log!("service push for {id} failed: {error:#}");
            }
        }
        Ok(())
    }

    /// An explicit local diagnostic: no Hub, task, report or legacy APNs sender.
    pub async fn test_push(&self, id: &str) -> Result<()> {
        let device = self.devices.get(id).context("device is not configured for the service")?;
        self.require_push_approval(id, device)?;
        let binding = self.journal.push_binding(id)?.context("device has no active push binding")?;
        let payload = json!({"aps":{"alert":{
            "title":"Prueba cifrada de Alondra",
            "body":"Diagnóstico enviado desde tu Mac. Si lees este texto, tu dispositivo ha descifrado la notificación."
        }}});
        self.send_device_push(id, device, &binding, &payload, true, &mut None).await
    }

    fn require_push_approval(&self, id: &str, device: &Device) -> Result<()> {
        // The diagnostic is a separate process: re-read persisted approval so
        // revocation by the running daemon is observed after bootstrap too.
        if !self.auth.approved_key_matches(id, &device.signing)
            || !Auth::open(&self.home)?.approved_key_matches(id, &device.signing)
            || !self.journal.enrollment_active(id)? {
            bail!("device is not actively enrolled and locally approved");
        }
        Ok(())
    }

    async fn send_device_push(&self, id: &str, device: &Device, binding: &str, payload: &Value,
        with_sound: bool, epoch: &mut Option<[u8; 16]>) -> Result<()> {
        self.require_push_approval(id, device)?;
        let (payload, with_sound) = self.journal.quiet_hours(id)?.apply(payload, with_sound, local_minutes());
        let body = self.encode_push(id, device, binding, &payload, with_sound)?;
        let current_epoch = match *epoch {
            Some(epoch) => epoch,
            None => {
                let fresh = self.bootstrap().await?;
                *epoch = Some(fresh);
                fresh
            }
        };
        self.require_push_approval(id, device)?;
        if self.journal.push_binding(id)?.as_deref() != Some(binding) {
            bail!("device push binding changed while preparing delivery");
        }
        let response = self.signed(Method::POST, "/v1/mac/push", body, &current_epoch).await?;
        if response.status() != reqwest::StatusCode::ACCEPTED {
            bail!("relay did not accept encrypted push (HTTP {})", response.status());
        }
        Ok(())
    }

    fn encode_push(&self, id: &str, device: &Device, binding: &str, payload: &Value, with_sound: bool) -> Result<Vec<u8>> {
        let plaintext = serde_json::to_vec(&json!({
            "push_binding": binding,
            "payload": payload,
            "with_sound": with_sound,
        }))?;
        if plaintext.len() > MAX_PUSH_PLAINTEXT { bail!("content exceeds encrypted APNs budget"); }
        let mut id_bytes = [0u8; 16];
        SystemRandom::new().fill(&mut id_bytes)
            .map_err(|_| anyhow!("OS randomness failed"))?;
        let request_id = URL_SAFE_NO_PAD.encode(id_bytes);
        let envelope = EnvelopeV1::seal_random(
            EnvelopeContext {
                route: &self.mac_id,
                sender: &self.mac_id,
                recipient: id,
                request_id: &request_id,
                kind: "push",
                expires_at_ms: now_ms() + 600_000,
            },
            &plaintext,
            &self.signing,
            &device.agreement,
        ).map_err(|error| anyhow!("seal push: {error}"))?;
        let body = serde_json::to_vec(&json!({"device_id":id,"envelope":envelope}))?;
        if body.len() > MAX_PUSH_BODY { bail!("envelope exceeds APNs budget"); }
        Ok(body)
    }
}

async fn bounded_response(mut response: reqwest::Response) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > MAX_LOCAL_RESPONSE {
            return Err(ResponseTooLarge.into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[derive(Debug)]
struct ResponseTooLarge;

impl std::fmt::Display for ResponseTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "local response exceeds encrypted relay limit")
    }
}

impl std::error::Error for ResponseTooLarge {}

fn response_size_detail(request: &LocalRequest) -> &'static str {
    if request.method == "GET" {
        "Mac response exceeded the encrypted relay limit"
    } else {
        "Mac response exceeded the encrypted relay limit; an action may already have completed"
    }
}

fn id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn number(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<u64>().is_ok()
}

fn one_id_path<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    path.strip_prefix(prefix).filter(|s| id(s))
}

fn id_action(path: &str, prefix: &str, action: &str, numeric: bool) -> bool {
    path.strip_prefix(prefix)
        .and_then(|s| s.strip_suffix(action))
        .is_some_and(|s| if numeric { number(s) } else { id(s) })
}

/// The relay carries opaque JSON. Only these app operations may be replayed on loopback.
/// There is deliberately no general URL proxy, no pairing/device management, and no upload.
fn approved_method(request: &LocalRequest) -> Result<Method> {
    let raw = request.path.as_str();
    if raw.len() > 4096 || raw.contains(['#', '\\']) {
        bail!("local operation is not approved for service access");
    }
    let (path, query) = raw.split_once('?').map_or((raw, None), |(p, q)| (p, Some(q)));
    if !path.starts_with("/v1/") || path.contains('%') || path.contains("//") || path.contains("/./") || path.contains("/../") {
        bail!("local operation is not approved for service access");
    }
    let query_ok = match (path, query) {
        (_, None) => true,
        ("/v1/options", Some(q)) => single_query(q, "project", 2048),
        ("/v1/issues", Some(q)) => single_query_value(q, "refresh", &["1"]),
        ("/v1/search", Some(q)) => single_query(q, "q", 2048),
        ("/v1/file", Some(q)) => single_query(q, "path", 2048),
        (p, Some(q)) if one_id_path(p, "/v1/nodes/").is_some() => single_query_value(q, "limit", &["1"]),
        _ => false,
    };
    if !query_ok {
        bail!("local operation is not approved for service access");
    }
    let allowed = match request.method.as_str() {
        "GET" if request.body.is_none() => {
            matches!(path, "/v1/overview" | "/v1/schedules" | "/v1/options" | "/v1/issues" | "/v1/projects" | "/v1/search" | "/v1/file")
                || one_id_path(path, "/v1/tasks/").is_some()
                || one_id_path(path, "/v1/nodes/").is_some()
                || id_action(path, "/v1/nodes/", "/activity", false)
                || id_action(path, "/v1/prs/", "/diff", true)
                || id_action(path, "/v1/prs/", "/diff-snapshot", true)
        }
        "POST" if request.body.is_some() && query.is_none() => {
            matches!(path, "/v1/requests" | "/v1/tasks/archive" | "/v1/activity" | "/v1/schedules")
                || id_action(path, "/v1/questions/", "/answer", true)
                || id_action(path, "/v1/questions/", "/more-context", true)
                || id_action(path, "/v1/prs/", "/merge", true)
                || id_action(path, "/v1/prs/", "/changes", true)
                || id_action(path, "/v1/prs/", "/ask", true)
                || id_action(path, "/v1/prs/", "/resolve-conflicts", true)
                || id_action(path, "/v1/prs/", "/dismiss", true)
                || id_action(path, "/v1/tasks/", "/cancel", false)
                || id_action(path, "/v1/tasks/", "/reopen", false)
                || id_action(path, "/v1/requests/", "/close", false)
                || id_action(path, "/v1/reports/", "/read", true)
                || id_action(path, "/v1/reports/", "/review", true)
                || id_action(path, "/v1/nodes/", "/close", false)
                || id_action(path, "/v1/nodes/", "/messages", false)
                || id_action(path, "/v1/schedules/", "/run", false)
        }
        "PUT" if request.body.is_some() && query.is_none() => one_id_path(path, "/v1/schedules/").is_some(),
        "DELETE" if request.body.is_none() && query.is_none() => one_id_path(path, "/v1/schedules/").is_some(),
        _ => false,
    };
    if !allowed {
        bail!("local operation is not approved for service access");
    }
    Ok(Method::from_bytes(request.method.as_bytes())?)
}

fn single_query(query: &str, key: &str, max_value_bytes: usize) -> bool {
    // Accept exactly one named parameter; the local handler validates its decoded value.
    let mut pairs = url::form_urlencoded::parse(query.as_bytes());
    matches!(pairs.next(), Some((k, v)) if k == key && !v.is_empty() && v.len() <= max_value_bytes)
        && pairs.next().is_none()
}

fn single_query_value(query: &str, key: &str, values: &[&str]) -> bool {
    let mut pairs = url::form_urlencoded::parse(query.as_bytes());
    matches!(pairs.next(), Some((k, v)) if k == key && values.contains(&v.as_ref()))
        && pairs.next().is_none()
}

enum Disposition {
    Accepted,
    Executing,
    Completed(Vec<u8>),
}
enum EnrollmentDecision {
    New,
    Pending,
    Active,
    Revoked,
}
struct Journal {
    path: PathBuf,
}
impl Journal {
    fn open(path: PathBuf) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            // Tighten the DB before SQLite creates WAL/SHM sidecars.
            std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        let c = Connection::open(&path)?;
        c.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS service_receipts (
              sender TEXT NOT NULL, request_id TEXT NOT NULL, digest BLOB NOT NULL,
              state TEXT NOT NULL, response BLOB, PRIMARY KEY(sender, request_id));
            CREATE TABLE IF NOT EXISTS service_enrollments (
              device_id TEXT PRIMARY KEY, signing_key BLOB NOT NULL, attempted_at_ms INTEGER NOT NULL,
              state TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS service_push_bindings (
              device_id TEXT PRIMARY KEY, binding TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS service_push_preferences (
              device_id TEXT PRIMARY KEY, quiet_start TEXT, quiet_end TEXT);
            CREATE TABLE IF NOT EXISTS service_push_preference_revisions (
              device_id TEXT PRIMARY KEY, revision INTEGER NOT NULL);",
        )?;
        Ok(Self { path })
    }
    fn db(&self) -> Result<Connection> {
        let c = Connection::open(&self.path)?;
        c.pragma_update(None, "synchronous", "FULL")?;
        Ok(c)
    }
    fn enrollments(&self) -> Result<Vec<(String, String)>> {
        let c = self.db()?;
        let mut query = c.prepare("SELECT device_id,state FROM service_enrollments")?;
        let rows = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    fn reserve_enrollment(&self, id: &str, signing_key: &[u8]) -> Result<EnrollmentDecision> {
        let mut c = self.db()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let previous: Option<(Vec<u8>, String)> = tx
            .query_row(
                "SELECT signing_key,state FROM service_enrollments WHERE device_id=?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let state = match previous {
            Some((prior, state)) if prior == signing_key => match state.as_str() {
                "pending" => EnrollmentDecision::Pending,
                "active" => EnrollmentDecision::Active,
                "revoked" => EnrollmentDecision::Revoked,
                _ => bail!("invalid stored enrollment state"),
            },
            Some(_) => bail!("device ID cannot be re-enrolled with another signing key"),
            None => {
                tx.execute("INSERT INTO service_enrollments(device_id,signing_key,attempted_at_ms,state) VALUES (?1,?2,?3,'pending')",
                    params![id,signing_key,now_ms() as i64])?;
                EnrollmentDecision::New
            }
        };
        tx.commit()?;
        Ok(state)
    }
    fn activate_enrollment(&self, id: &str) -> Result<()> {
        let c = self.db()?;
        let n = c.execute(
            "UPDATE service_enrollments SET state='active' WHERE device_id=?1 AND state='pending'",
            params![id],
        )?;
        if n != 1 {
            bail!("enrollment no longer pending");
        }
        Ok(())
    }
    fn revoke_enrollment(&self, id: &str) -> Result<()> {
        let mut c = self.db()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE service_enrollments SET state='revoked' WHERE device_id=?1",
            params![id],
        )?;
        tx.execute("DELETE FROM service_push_bindings WHERE device_id=?1", params![id])?;
        tx.execute("DELETE FROM service_push_preferences WHERE device_id=?1", params![id])?;
        tx.execute("DELETE FROM service_push_preference_revisions WHERE device_id=?1", params![id])?;
        tx.commit()?;
        Ok(())
    }
    #[cfg(test)]
    fn set_push_binding(&self, id: &str, binding: &str) -> Result<()> {
        self.set_push_preferences(id, binding, None)
    }
    #[cfg(test)]
    fn set_push_preferences(&self, id: &str, binding: &str, quiet: Option<QuietHours>) -> Result<()> {
        if self.set_versioned_push_preferences(id, binding, quiet, None)?.is_some() { bail!("stale push preferences"); }
        Ok(())
    }
    /// Returns the current revision on conflict. Checks and writes share one
    /// immediate transaction so delivery order and process restarts cannot roll back state.
    fn set_versioned_push_preferences(&self, id: &str, binding: &str, quiet: Option<QuietHours>, revision: Option<i64>) -> Result<Option<i64>> {
        if revision.is_some_and(|v| !(1..=9_007_199_254_740_991).contains(&v)) || (revision.is_some() && quiet.is_none()) {
            bail!("invalid versioned push preferences");
        }
        if !valid_push_binding(binding) { bail!("invalid push binding"); }
        let mut c = self.db()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: Option<i64> = tx.query_row("SELECT revision FROM service_push_preference_revisions WHERE device_id=?1", params![id], |row| row.get(0)).optional()?;
        if let Some(current) = current {
            if revision.is_none_or(|revision| revision < current) { return Ok(Some(current)); }
            if revision == Some(current) {
                let saved: (String, Option<String>, Option<String>) = tx.query_row(
                    "SELECT b.binding,p.quiet_start,p.quiet_end FROM service_push_bindings b JOIN service_push_preferences p USING(device_id) JOIN service_enrollments e USING(device_id) WHERE b.device_id=?1 AND e.state='active'",
                    params![id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
                return Ok(if quiet.as_ref().is_some_and(|quiet| saved.0 == binding && saved.1 == quiet.start && saved.2 == quiet.end) { None } else { Some(current) });
            }
        }
        let updated = tx.execute(
            "INSERT INTO service_push_bindings(device_id,binding) SELECT device_id,?2 FROM service_enrollments WHERE device_id=?1 AND state='active' ON CONFLICT(device_id) DO UPDATE SET binding=excluded.binding",
            params![id, binding],
        )?;
        if updated != 1 { bail!("push device is not enrolled"); }
        if let Some(quiet) = quiet {
            tx.execute("INSERT INTO service_push_preferences(device_id,quiet_start,quiet_end) VALUES(?1,?2,?3) ON CONFLICT(device_id) DO UPDATE SET quiet_start=excluded.quiet_start,quiet_end=excluded.quiet_end", params![id, quiet.start, quiet.end])?;
        }
        if let Some(revision) = revision {
            tx.execute("INSERT INTO service_push_preference_revisions(device_id,revision) VALUES(?1,?2) ON CONFLICT(device_id) DO UPDATE SET revision=excluded.revision", params![id, revision])?;
        }
        tx.commit()?;
        Ok(None)
    }
    fn quiet_hours(&self, id: &str) -> Result<QuietHours> {
        Ok(self.db()?.query_row(
            "SELECT p.quiet_start,p.quiet_end FROM service_push_preferences p JOIN service_enrollments e USING(device_id) WHERE p.device_id=?1 AND e.state='active'",
            params![id], |row| Ok(QuietHours { start: row.get(0)?, end: row.get(1)? }),
        ).optional()?.unwrap_or_default())
    }
    fn push_binding(&self, id: &str) -> Result<Option<String>> {
        let c = self.db()?;
        Ok(c.query_row(
            "SELECT b.binding FROM service_push_bindings b JOIN service_enrollments e USING(device_id) WHERE b.device_id=?1 AND e.state='active'",
            params![id], |row| row.get(0),
        ).optional()?)
    }
    #[cfg(test)]
    fn enrollment_revoked(&self, id: &str) -> Result<bool> {
        let c = self.db()?;
        Ok(c.query_row(
            "SELECT state FROM service_enrollments WHERE device_id=?1",
            params![id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .is_some_and(|state| state == "revoked"))
    }
    #[cfg(test)]
    fn enrollment_pending(&self, id: &str) -> Result<bool> {
        let c = self.db()?;
        Ok(c.query_row(
            "SELECT state FROM service_enrollments WHERE device_id=?1",
            params![id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .is_some_and(|state| state == "pending"))
    }
    fn enrollment_active(&self, id: &str) -> Result<bool> {
        let c = self.db()?;
        Ok(c.query_row(
            "SELECT state FROM service_enrollments WHERE device_id=?1",
            params![id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .is_some_and(|state| state == "active"))
    }
    fn accept(&self, sender: &str, id: &str, digest: &[u8; 32]) -> Result<Disposition> {
        let mut c = self.db()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let previous: Option<(Vec<u8>, String, Option<Vec<u8>>)> = tx.query_row(
            "SELECT digest,state,response FROM service_receipts WHERE sender=?1 AND request_id=?2",
            params![sender,id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let state = match previous {
            Some((prior, _, _)) if prior != digest => {
                bail!("request ID reused with different encrypted bytes")
            }
            Some((_, state, response)) if state == "completed" => {
                Disposition::Completed(response.context("completed receipt missing response")?)
            }
            Some((_, state, _)) if state == "executing" => Disposition::Executing,
            Some(_) => Disposition::Accepted,
            None => {
                tx.execute("INSERT INTO service_receipts(sender,request_id,digest,state) VALUES (?1,?2,?3,'accepted')", params![sender,id,digest.as_slice()])?;
                Disposition::Accepted
            }
        };
        tx.commit()?;
        Ok(state)
    }
    fn executing(&self, sender: &str, id: &str, digest: &[u8; 32]) -> Result<()> {
        let c = self.db()?;
        let n = c.execute("UPDATE service_receipts SET state='executing' WHERE sender=?1 AND request_id=?2 AND digest=?3 AND state='accepted'", params![sender,id,digest.as_slice()])?;
        if n != 1 {
            bail!("operation receipt no longer accepted");
        }
        Ok(())
    }
    fn complete(&self, sender: &str, id: &str, response: &[u8]) -> Result<()> {
        let c = self.db()?;
        let n = c.execute("UPDATE service_receipts SET state='completed', response=?3 WHERE sender=?1 AND request_id=?2 AND state='executing'", params![sender,id,response])?;
        if n != 1 {
            bail!("operation receipt no longer executing");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Bytes,
        extract::State,
        http::HeaderMap,
        routing::{delete, get, post},
    };
    use base64::engine::general_purpose::STANDARD;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    #[test]
    fn push_binding_rotates_persists_and_is_removed_on_revocation() {
        let path = std::env::temp_dir().join(format!(
            "alondra-push-bindings-{}-{}.sqlite", std::process::id(), now_ms()
        ));
        let journal = Journal::open(path.clone()).unwrap();
        let key = SigningKey::from_slice(&[33u8; 32]).unwrap();
        assert!(matches!(journal.reserve_enrollment("phone", &key.verifying_key().to_sec1_bytes()).unwrap(), EnrollmentDecision::New));
        assert!(journal.set_push_binding("phone", &URL_SAFE_NO_PAD.encode([1u8; 32])).is_err(), "pending enrollment cannot activate push");
        journal.activate_enrollment("phone").unwrap();
        let first = URL_SAFE_NO_PAD.encode([1u8; 32]);
        let second = URL_SAFE_NO_PAD.encode([2u8; 32]);
        journal.set_push_binding("phone", &first).unwrap();
        assert_eq!(Journal::open(path.clone()).unwrap().push_binding("phone").unwrap().as_deref(), Some(first.as_str()));
        journal.set_push_binding("phone", &second).unwrap();
        assert_eq!(journal.push_binding("phone").unwrap().as_deref(), Some(second.as_str()));
        assert!(journal.set_push_binding("phone", "not-base64url").is_err());
        journal.revoke_enrollment("phone").unwrap();
        assert!(Journal::open(path).unwrap().push_binding("phone").unwrap().is_none());
        assert!(journal.set_push_binding("phone", &first).is_err());
    }

    #[test]
    fn quiet_hours_validate_boundaries_and_preserve_payload() {
        let parse = |start: Value, end: Value| QuietHours::parse_update(Some(&start), Some(&end));
        for (start, end) in [(json!("24:00"), json!("08:00")), (json!("23:00"), json!("08:60")), (json!("1:00"), json!("08:00")), (json!("23:00"), Value::Null), (json!("é:00"), json!("08:00"))] {
            assert!(parse(start, end).is_err());
        }
        assert!(QuietHours::parse_update(Some(&Value::Null), None).is_err());
        assert_eq!(QuietHours::parse_update(None, None).unwrap(), None);
        let quiet = parse(json!("23:00"), json!("08:00")).unwrap().unwrap();
        let payload = json!({"aps":{"sound":"default", "alert":{"title":"private"}, "badge":3}, "alondra":{"id":9}});
        for (minute, expected) in [(22*60+59,false), (23*60,true), (0,true), (8*60-1,true), (8*60,false)] {
            let (body, sound) = quiet.apply(&payload, true, minute);
            assert_eq!(sound, !expected);
            assert_eq!(body["aps"]["interruption-level"], if expected { "passive" } else { "active" });
            assert_eq!(body["aps"].get("sound").is_none(), expected);
            assert_eq!(body["aps"]["alert"], payload["aps"]["alert"]);
            assert_eq!(body["alondra"], payload["alondra"]);
        }
        assert!(!quiet.apply(&payload, false, 12*60).0["aps"].as_object().unwrap().contains_key("sound"));
        let equal = parse(json!("08:00"), json!("08:00")).unwrap().unwrap();
        assert!(equal.apply(&payload, true, 8*60).1, "equal bounds disable silence as in legacy");
    }

    #[test]
    fn quiet_hours_persist_per_device_rotate_clear_and_revoke() {
        let path = std::env::temp_dir().join(format!("alondra-quiet-{}-{}.sqlite", std::process::id(), now_ms()));
        let journal = Journal::open(path.clone()).unwrap();
        let binding = URL_SAFE_NO_PAD.encode([7u8;32]);
        let quiet = || Some(QuietHours { start: Some("23:00".into()), end: Some("08:00".into()) });
        assert!(journal.set_push_preferences("a", &binding, quiet()).is_err());
        for id in ["a", "b"] {
            journal.reserve_enrollment(id, &[1u8;33]).unwrap();
            journal.activate_enrollment(id).unwrap();
        }
        journal.set_push_preferences("a", &binding, quiet()).unwrap();
        assert_eq!(Journal::open(path.clone()).unwrap().quiet_hours("a").unwrap(), quiet().unwrap());
        assert_eq!(journal.quiet_hours("b").unwrap(), QuietHours::default());
        journal.set_push_preferences("a", &URL_SAFE_NO_PAD.encode([8u8;32]), None).unwrap();
        assert_eq!(journal.quiet_hours("a").unwrap(), quiet().unwrap());
        journal.set_push_preferences("a", &binding, Some(QuietHours::default())).unwrap();
        assert_eq!(journal.quiet_hours("a").unwrap(), QuietHours::default());
        journal.set_push_preferences("a", &binding, quiet()).unwrap();
        journal.revoke_enrollment("a").unwrap();
        assert_eq!(journal.quiet_hours("a").unwrap(), QuietHours::default());
        let count: i64 = journal.db().unwrap().query_row("SELECT count(*) FROM service_push_preferences", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn quiet_hours_reordered_retries_cannot_restore_stale_preferences() {
        let path = std::env::temp_dir().join(format!("alondra-quiet-revision-{}-{}.sqlite", std::process::id(), now_ms()));
        let journal = Journal::open(path.clone()).unwrap();
        journal.reserve_enrollment("phone", &[1u8;33]).unwrap();
        journal.activate_enrollment("phone").unwrap();
        let binding = URL_SAFE_NO_PAD.encode([7u8;32]);
        let second_binding = URL_SAFE_NO_PAD.encode([8u8;32]);
        let quiet = || Some(QuietHours { start: Some("23:00".into()), end: Some("08:00".into()) });
        let disabled = || Some(QuietHours::default());
        assert_eq!(journal.set_versioned_push_preferences("phone", &binding, quiet(), Some(1)).unwrap(), None);
        assert_eq!(journal.set_versioned_push_preferences("phone", &second_binding, disabled(), Some(2)).unwrap(), None);
        // Process restart + an uncertain old operation retried with a fresh UUID.
        let journal = Journal::open(path).unwrap();
        assert_eq!(journal.set_versioned_push_preferences("phone", &binding, quiet(), Some(1)).unwrap(), Some(2));
        assert_eq!(journal.set_versioned_push_preferences("phone", &binding, quiet(), None).unwrap(), Some(2));
        assert_eq!(journal.set_versioned_push_preferences("phone", &binding, quiet(), Some(2)).unwrap(), Some(2));
        assert_eq!(journal.set_versioned_push_preferences("phone", &second_binding, disabled(), Some(2)).unwrap(), None);
        assert_eq!(journal.push_binding("phone").unwrap(), Some(second_binding));
        assert_eq!(journal.quiet_hours("phone").unwrap(), QuietHours::default());
        // A -> B -> A is a new logical update, never a cached old operation.
        assert_eq!(journal.set_versioned_push_preferences("phone", &binding, quiet(), Some(3)).unwrap(), None);
        assert_eq!(journal.quiet_hours("phone").unwrap(), quiet().unwrap());
        journal.revoke_enrollment("phone").unwrap();
        let count: i64 = journal.db().unwrap().query_row("SELECT count(*) FROM service_push_preference_revisions", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    fn push_fixture() -> (Service, SecretKey) {
        let home = std::env::temp_dir().join(format!(
            "alondra-encrypted-push-{}-{}", std::process::id(), crate::auth::random_hex(8)
        ));
        std::fs::create_dir_all(&home).unwrap();
        let phone_signing = SigningKey::from_slice(&[33u8; 32]).unwrap();
        let phone_agreement = SecretKey::from_slice(&[34u8; 32]).unwrap();
        // Use the exact disk contract written by service_pair::approve: the
        // signing key is locally approved and both phone keys are pinned in TOML.
        for (name, bytes) in [("signing.key", [1u8; 32]), ("agreement.key", [2u8; 32])] {
            let path = home.join(name);
            std::fs::write(&path, URL_SAFE_NO_PAD.encode(bytes)).unwrap();
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
        std::fs::write(home.join("devices.json"), serde_json::to_vec(&json!([{
            "id":"phone-1", "name":"iPhone", "created_at":0, "last_seen":null,
            "public_key":STANDARD.encode(phone_signing.verifying_key().to_sec1_bytes())
        }])).unwrap()).unwrap();
        let config = format!(r#"relay_url = "http://127.0.0.1:1/"
mac_id = "mac-1"
owner_sub = "owner"
signing_key_path = "signing.key"
agreement_key_path = "agreement.key"
[[devices]]
device_id = "phone-1"
signing_key = "{}"
agreement_key = "{}"
"#, URL_SAFE_NO_PAD.encode(phone_signing.verifying_key().to_sec1_bytes()),
            URL_SAFE_NO_PAD.encode(phone_agreement.public_key().to_sec1_bytes()));
        std::fs::write(home.join("service.toml"), config).unwrap();
        let service = Service::open(&home, 0, Arc::new(Auth::open(&home).unwrap())).unwrap().unwrap();
        (service, phone_agreement)
    }

    #[tokio::test]
    async fn service_push_contains_only_ciphertext_outside_and_enforces_budget() {
        let (service, phone_agreement) = push_fixture();
        let phone_signing = SigningKey::from_slice(&[33u8; 32]).unwrap();
        let device = service.devices.get("phone-1").unwrap();
        assert!(service.auth.approved_key_matches("phone-1", &device.signing));
        let binding = URL_SAFE_NO_PAD.encode([4u8; 32]);
        service.journal.reserve_enrollment("phone-1", &phone_signing.verifying_key().to_sec1_bytes()).unwrap();
        service.journal.activate_enrollment("phone-1").unwrap();
        let request = LocalRequest {
            method: "POST".into(), path: "/v1/service/push-binding".into(),
            body: Some(json!({"push_binding":binding})),
        };
        assert_eq!(service.dispatch("phone-1", &request).await.unwrap()["status"], 204);
        assert_eq!(service.journal.push_binding("phone-1").unwrap().as_deref(), Some(binding.as_str()));
        let bad_request = LocalRequest {
            method: "POST".into(), path: "/v1/service/push-binding".into(),
            body: Some(json!({"push_binding":binding,"unexpected":true})),
        };
        assert!(service.dispatch("phone-1", &bad_request).await.is_err());
        let quiet_request = LocalRequest {
            method: "POST".into(), path: "/v1/service/push-binding".into(),
            body: Some(json!({"push_binding":binding,"quiet_start":"23:00","quiet_end":"08:00"})),
        };
        assert_eq!(service.dispatch("phone-1", &quiet_request).await.unwrap()["status"], 204);
        let invalid_quiet = LocalRequest {
            body: Some(json!({"push_binding":URL_SAFE_NO_PAD.encode([9u8;32]),"quiet_start":"24:00","quiet_end":"08:00"})),
            ..quiet_request
        };
        assert!(service.dispatch("phone-1", &invalid_quiet).await.is_err());
        assert_eq!(service.journal.push_binding("phone-1").unwrap().as_deref(), Some(binding.as_str()));
        let secret = "private question title for iPhone only";
        let payload = json!({"aps":{"alert":{"title":secret},"category":"ALONDRA_QUESTION"},"alondra":{"kind":"question","id":7}});
        let body = service.encode_push("phone-1", &device, &binding, &payload, true).unwrap();
        assert!(body.len() <= MAX_PUSH_BODY);
        let wire = String::from_utf8(body.clone()).unwrap();
        assert!(!wire.contains(secret));
        assert!(!wire.contains("ALONDRA_QUESTION"));
        assert!(!wire.contains(&binding));
        let decoded: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(decoded["device_id"], "phone-1");
        let envelope: EnvelopeV1 = serde_json::from_value(decoded["envelope"].clone()).unwrap();
        assert_eq!(envelope.kind, "push");
        assert_eq!(envelope.route, "mac-1");
        assert_eq!(envelope.recipient, "phone-1");
        let plaintext = envelope.open(service.signing.verifying_key(), &phone_agreement,
            "mac-1", "phone-1", "mac-1", now_ms()).unwrap();
        let content: Value = serde_json::from_slice(&plaintext).unwrap();
        assert_eq!(content["push_binding"], binding);
        assert_eq!(content["payload"], payload);
        assert_eq!(content["with_sound"], true);
        let (quiet_payload, sound) = service.journal.quiet_hours("phone-1").unwrap().apply(&payload, true, 23*60);
        let quiet_wire = service.encode_push("phone-1", &device, &binding, &quiet_payload, sound).unwrap();
        let wire_text = String::from_utf8(quiet_wire.clone()).unwrap();
        assert!(!wire_text.contains("passive") && !wire_text.contains("quiet_start"));
        let envelope: EnvelopeV1 = serde_json::from_value(serde_json::from_slice::<Value>(&quiet_wire).unwrap()["envelope"].clone()).unwrap();
        let opened = envelope.open(service.signing.verifying_key(), &phone_agreement, "mac-1", "phone-1", "mac-1", now_ms()).unwrap();
        let opened: Value = serde_json::from_slice(&opened).unwrap();
        assert_eq!(opened["payload"]["aps"]["interruption-level"], "passive");
        assert_eq!(opened["with_sound"], false);
        let huge = json!({"aps":{"alert":{"body":"x".repeat(MAX_PUSH_PLAINTEXT)}}});
        assert!(service.encode_push("phone-1", &device, &binding, &huge, true).is_err());
    }
    fn activate_push(service: &Service) {
        let key = service.devices["phone-1"].signing.to_sec1_bytes();
        service.journal.reserve_enrollment("phone-1", &key).unwrap();
        service.journal.activate_enrollment("phone-1").unwrap();
        service.journal.set_push_preferences("phone-1", &URL_SAFE_NO_PAD.encode([4u8;32]), None).unwrap();
    }

    #[tokio::test]
    async fn diagnostic_push_refuses_wrong_unenrolled_unbound_and_revoked_devices() {
        let (service, _) = push_fixture();
        assert!(service.test_push("wrong").await.unwrap_err().to_string().contains("not configured"));
        assert!(service.test_push("phone-1").await.unwrap_err().to_string().contains("not actively enrolled"));
        let key = service.devices["phone-1"].signing.to_sec1_bytes();
        service.journal.reserve_enrollment("phone-1", &key).unwrap();
        service.journal.activate_enrollment("phone-1").unwrap();
        assert!(service.test_push("phone-1").await.unwrap_err().to_string().contains("no active push binding"));
        service.journal.set_push_preferences("phone-1", &URL_SAFE_NO_PAD.encode([4u8;32]), None).unwrap();
        // Another process revokes the local pairing; this Service's Auth is stale.
        Auth::open(&service.home).unwrap().revoke("phone-1").unwrap();
        assert!(service.test_push("phone-1").await.unwrap_err().to_string().contains("not actively enrolled"));
        std::fs::remove_dir_all(&service.home).unwrap();
    }

    #[tokio::test]
    async fn diagnostic_push_uses_encrypted_route_and_requires_exact_acceptance() {
        use axum::{Router, routing::{get, post}, Json};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (mut service, phone_agreement) = push_fixture();
        activate_push(&service);
        let now = local_minutes();
        let time = |m: u32| format!("{:02}:{:02}", m / 60, m % 60);
        service.journal.set_push_preferences("phone-1", &URL_SAFE_NO_PAD.encode([4u8;32]),
            Some(QuietHours { start: Some(time((now + 1380) % 1440)), end: Some(time((now + 60) % 1440)) })).unwrap();
        let requests = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let seen = requests.clone();
        let status = Arc::new(AtomicUsize::new(502));
        let answer = status.clone();
        let router = Router::new().route("/v1/bootstrap", get(|| async {
            Json(json!({"epoch":URL_SAFE_NO_PAD.encode([1u8;16])}))
        })).route("/v1/mac/push", post(move |Json(body): Json<Value>| {
            let seen = seen.clone(); let answer = answer.clone();
            async move {
                seen.lock().unwrap().push(body);
                axum::http::StatusCode::from_u16(answer.load(Ordering::SeqCst) as u16).unwrap()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        service.relay = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        assert!(service.test_push("phone-1").await.unwrap_err().to_string().contains("502"));
        status.store(200, Ordering::SeqCst);
        assert!(service.test_push("phone-1").await.unwrap_err().to_string().contains("200"));
        status.store(202, Ordering::SeqCst);
        service.test_push("phone-1").await.unwrap();
        let sent = requests.lock().unwrap();
        assert_eq!(sent.len(), 3);
        for body in sent.iter() {
            assert_eq!(body["device_id"], "phone-1");
            assert!(!body.to_string().contains("Prueba cifrada"));
            let envelope: EnvelopeV1 = serde_json::from_value(body["envelope"].clone()).unwrap();
            let plaintext = envelope.open(service.signing.verifying_key(), &phone_agreement,
                "mac-1", "phone-1", "mac-1", now_ms()).unwrap();
            let content: Value = serde_json::from_slice(&plaintext).unwrap();
            assert_eq!(content["payload"]["aps"]["alert"]["title"], "Prueba cifrada de Alondra");
            assert!(content["payload"].get("alondra").is_none(), "no invented report target");
            assert_eq!(content["payload"]["aps"]["interruption-level"], "passive");
            assert_eq!(content["with_sound"], false);
            assert_eq!(content["push_binding"], URL_SAFE_NO_PAD.encode([4u8;32]));
        }
        server.abort();
        std::fs::remove_dir_all(&service.home).unwrap();
    }

    #[tokio::test]
    async fn diagnostic_push_rechecks_binding_and_revocation_after_bootstrap() {
        use axum::{Router, routing::{get, post}, Json};
        use std::sync::atomic::{AtomicUsize, Ordering};
        for revoke in [false, true] {
            let (mut service, _) = push_fixture();
            activate_push(&service);
            let home = service.home.clone();
            let sent = Arc::new(AtomicUsize::new(0)); let observed = sent.clone();
            let router = Router::new().route("/v1/bootstrap", get(move || {
                let home = home.clone();
                async move {
                    if revoke { Auth::open(&home).unwrap().revoke("phone-1").unwrap(); }
                    else { Journal::open(home.join("service-receipts.sqlite")).unwrap()
                        .set_push_preferences("phone-1", &URL_SAFE_NO_PAD.encode([9u8;32]), None).unwrap(); }
                    Json(json!({"epoch":URL_SAFE_NO_PAD.encode([1u8;16])}))
                }
            })).route("/v1/mac/push", post(move || { let observed = observed.clone(); async move {
                observed.fetch_add(1, Ordering::SeqCst); axum::http::StatusCode::ACCEPTED
            }}));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            service.relay = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let error = service.test_push("phone-1").await.unwrap_err().to_string();
            assert!(error.contains(if revoke { "not actively enrolled" } else { "binding changed" }));
            assert_eq!(sent.load(Ordering::SeqCst), 0);
            server.abort();
            std::fs::remove_dir_all(&service.home).unwrap();
        }
    }

    #[test]
    fn local_dispatch_allowlist_rejects_path_tricks() {
        let accepts = |method: &str, path: &str, body: Option<Value>| approved_method(&LocalRequest {
            method: method.into(), path: path.into(), body
        }).is_ok();
        for path in [
            "/v1/overview", "/v1/tasks/task-1", "/v1/nodes/session_1/activity",
            "/v1/prs/7/diff", "/v1/prs/7/diff-snapshot", "/v1/search?q=hello%20world",
            "/v1/file?path=%2FUsers%2Fme%2Ffile.txt", "/v1/options?project=foo",
        ] {
            assert!(accepts("GET", path, None), "{path}");
        }
        for path in [
            "/v1/questions/123/answer", "/v1/questions/123/more-context",
            "/v1/prs/7/merge", "/v1/prs/7/changes", "/v1/prs/7/ask",
            "/v1/prs/7/resolve-conflicts", "/v1/prs/7/dismiss",
            "/v1/tasks/task_1/cancel", "/v1/tasks/archive", "/v1/requests",
            "/v1/nodes/session-1/messages", "/v1/nodes/session-1/close",
            "/v1/schedules/schedule-1/run", "/v1/reports/1/review",
        ] {
            assert!(accepts("POST", path, Some(json!({}))), "{path}");
        }
        assert!(accepts("PUT", "/v1/schedules/schedule-1", Some(json!({}))));
        assert!(accepts("DELETE", "/v1/schedules/schedule-1", None));
        for (method, path, body) in [
            ("POST", "/v1/questions/../answer", Some(json!({}))),
            ("POST", "/v1/questions/1%2f../answer", Some(json!({}))),
            ("POST", "/v1/questions/1/answer?x=1", Some(json!({}))),
            ("POST", "/v1/questions/-1/answer", Some(json!({}))),
            ("GET", "/v1/overview?next=/v1/pairing", None),
            ("GET", "//evil.example/v1/overview", None),
            ("GET", "/v1/search?q=hello&path=secret", None),
            ("GET", "/v1/nodes/s?limit=2000", None),
            ("POST", "/v1/uploads", Some(json!({}))),
            ("POST", "/v1/pairing", Some(json!({}))),
            ("DELETE", "/v1/devices/phone", None),
            ("GET", "/v1/overview", Some(json!({}))),
            ("POST", "/v1/requests", None),
            ("GET", "/v1/prs/7/diff#fragment", None),
        ] {
            assert!(!accepts(method, path, body), "{method} {path}");
        }
    }

    #[tokio::test]
    async fn dispatch_preserves_json_file_text_without_wrapping_json_endpoints_or_errors() {
        const JSON_FILE: &str = "{\n  \"text\": \"contenido á\", \"count\": 3\n}\n";
        const MARKDOWN_FILE: &str = "# Documento\n\nTexto **literal**: á.\n";
        let router = Router::new()
            .route("/v1/file", get(|axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>| async move {
                match query.get("path").map(String::as_str) {
                    Some("json.md") => (StatusCode::OK, JSON_FILE),
                    Some("markdown.md") => (StatusCode::OK, MARKDOWN_FILE),
                    Some("scalar.md") => (StatusCode::OK, "null"),
                    _ => (StatusCode::NOT_FOUND, r#"{"error":"file_not_found"}"#),
                }
            }))
            .route("/v1/prs/7/diff-snapshot", get(|| async {
                axum::Json(json!({"diff":"diff --git a/file b/file\n","head_sha":"abc","truncated":false}))
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(axum::serve(listener, router).into_future());
        let home = std::env::temp_dir().join(format!("alondra-file-contract-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&home).unwrap();
        let service = Service {
            relay: Url::parse("http://127.0.0.1:1/").unwrap(), client: Client::new(),
            mac_id: "mac-test".into(), owner_sub: "owner".into(),
            signing: SigningKey::from_slice(&[1u8; 32]).unwrap(),
            agreement: SecretKey::from_slice(&[2u8; 32]).unwrap(), devices: HashMap::new(),
            home: home.clone(), transfers: Arc::default(),
            auth: Arc::new(Auth::open(&home).unwrap()),
            journal: Journal::open(home.join("receipts.sqlite")).unwrap(), local_port,
        };
        for (file, text) in [("json.md", JSON_FILE), ("markdown.md", MARKDOWN_FILE), ("scalar.md", "null")] {
            let response = service.dispatch("client-test", &LocalRequest {
                method: "GET".into(), path: format!("/v1/file?path={file}"), body: None,
            }).await.unwrap();
            assert_eq!(response, json!({"status":200,"body":{"text":text}}), "{file}");
        }
        for (path, expected) in [
            ("/v1/file?path=missing.md", json!({"status":404,"body":{"error":"file_not_found"}})),
            ("/v1/prs/7/diff-snapshot", json!({"status":200,"body":{"diff":"diff --git a/file b/file\n","head_sha":"abc","truncated":false}})),
        ] {
            assert_eq!(service.dispatch("client-test", &LocalRequest {
                method: "GET".into(), path: path.into(), body: None,
            }).await.unwrap(), expected);
        }
        task.abort();
        drop(service);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn local_response_stops_at_encrypted_relay_boundary() {
        let exact = format!("{{\"x\":\"{}\"}}", "a".repeat(MAX_LOCAL_RESPONSE - 8));
        assert_eq!(exact.len(), MAX_LOCAL_RESPONSE);
        let too_large = format!("{exact} ");
        let router = Router::new()
            .route("/exact", get(move || async move { exact.clone() }))
            .route("/large", get(move || async move { too_large.clone() }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(axum::serve(listener, router).into_future());
        let client = Client::new();
        let exact = client.get(format!("http://127.0.0.1:{port}/exact")).send().await.unwrap();
        assert_eq!(bounded_response(exact).await.unwrap().len(), MAX_LOCAL_RESPONSE);
        let large = client.get(format!("http://127.0.0.1:{port}/large")).send().await.unwrap();
        let oversized = bounded_response(large).await.unwrap_err();
        assert!(oversized.is::<ResponseTooLarge>());
        assert!(oversized.to_string().contains("encrypted relay limit"));
        task.abort();
    }
    #[test]
    fn durable_receipt_rejects_changed_bytes_and_never_reruns_executing() {
        let path = std::env::temp_dir().join(format!(
            "alondra-service-receipt-{}-{}.sqlite",
            std::process::id(),
            now_ms()
        ));
        let journal = Journal::open(path.clone()).unwrap();
        let a = [1u8; 32];
        assert!(matches!(
            journal.accept("phone", "op", &a).unwrap(),
            Disposition::Accepted
        ));
        assert!(matches!(
            Journal::open(path)
                .unwrap()
                .accept("phone", "op", &a)
                .unwrap(),
            Disposition::Accepted
        ));
        journal.executing("phone", "op", &a).unwrap();
        assert!(matches!(
            journal.accept("phone", "op", &a).unwrap(),
            Disposition::Executing
        ));
        assert!(journal.accept("phone", "op", &[2u8; 32]).is_err());
        journal.complete("phone", "op", b"ciphertext").unwrap();
        assert!(
            matches!(journal.accept("phone", "op", &a).unwrap(), Disposition::Completed(v) if v == b"ciphertext")
        );
    }

    #[tokio::test]
    async fn ambiguous_enrollment_is_deleted_and_never_retried() {
        let registered = Arc::new(AtomicBool::new(false));
        let posts = Arc::new(AtomicUsize::new(0));
        let post_registered = registered.clone();
        let post_count = posts.clone();
        let delete_registered = registered.clone();
        let relay = Router::new()
            .route(
                "/v1/devices",
                post(move || {
                    let registered = post_registered.clone();
                    let posts = post_count.clone();
                    async move {
                        registered.store(true, Ordering::SeqCst);
                        posts.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            axum::Json(json!({"error":"response_lost"})),
                        )
                    }
                }),
            )
            .route(
                "/v1/devices/{id}",
                delete(move || {
                    let registered = delete_registered.clone();
                    async move {
                        registered.store(false, Ordering::SeqCst);
                        StatusCode::NO_CONTENT
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(axum::serve(listener, relay).into_future());
        let home = std::env::temp_dir().join(format!(
            "alondra-pending-enrollment-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let phone = SigningKey::from_slice(&[33u8; 32]).unwrap();
        let phone_agreement = SecretKey::from_slice(&[34u8; 32]).unwrap();
        std::fs::write(home.join("devices.json"),serde_json::to_vec(&json!([{
            "id":"phone-pending","name":"Pending","public_key":STANDARD.encode(phone.verifying_key().to_sec1_bytes()),
            "created_at":0,"last_seen":null
        }])).unwrap()).unwrap();
        let service = Service {
            relay: Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap(),
            client: Client::new(),
            mac_id: "mac-1".into(),
            owner_sub: "owner".into(),
            signing: SigningKey::from_slice(&[1u8; 32]).unwrap(),
            agreement: SecretKey::from_slice(&[2u8; 32]).unwrap(),
            devices: HashMap::from([(
                "phone-pending".into(),
                Device {
                    signing: *phone.verifying_key(),
                    agreement: phone_agreement.public_key(),
                    signing_wire: URL_SAFE_NO_PAD.encode(phone.verifying_key().to_sec1_bytes()),
                },
            )]),
            home: home.clone(), transfers: Arc::default(),
            auth: Arc::new(Auth::open(&home).unwrap()),
            journal: Journal::open(home.join("service-receipts.sqlite")).unwrap(),
            local_port: 0,
        };
        let epoch = [7u8; 16];
        assert!(
            service.enroll(&epoch).await.is_err(),
            "relay committed but response was ambiguous"
        );
        assert!(registered.load(Ordering::SeqCst));
        assert!(service.journal.enrollment_pending("phone-pending").unwrap());
        let request = EnvelopeV1::seal_random(
            EnvelopeContext {
                route: "mac-1",
                sender: "phone-pending",
                recipient: "mac-1",
                request_id: "op-pending",
                kind: "request",
                expires_at_ms: now_ms() + 60_000,
            },
            br#"{"method":"GET","path":"/v1/overview"}"#,
            &phone,
            &service.agreement.public_key(),
        )
        .unwrap();
        assert!(
            service
                .handle(
                    Work {
                        request_id: "op-pending".into(),
                        envelope: request
                    },
                    &epoch
                )
                .await
                .is_err(),
            "pending enrollment must not execute work"
        );
        service
            .sync_revocations(&epoch, &mut HashSet::new())
            .await
            .unwrap();
        assert!(
            !registered.load(Ordering::SeqCst),
            "compensating DELETE removes ambiguous registration"
        );
        assert!(service.journal.enrollment_revoked("phone-pending").unwrap());
        service.enroll(&[8u8; 16]).await.unwrap();
        assert_eq!(
            posts.load(Ordering::SeqCst),
            1,
            "no implicit re-enrollment after ambiguous outcome"
        );
        task.abort();
    }

    #[tokio::test]
    async fn encrypted_work_executes_once_and_replays_exact_response() {
        let calls = Arc::new(AtomicUsize::new(0));
        let local_calls = calls.clone();
        let local = Router::new().route(
            "/v1/questions/1/answer",
            post(move || {
                let calls = local_calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({"saved":true}))
                }
            }),
        );
        let local_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_port = local_listener.local_addr().unwrap().port();
        let local_task = tokio::spawn(axum::serve(local_listener, local).into_future());

        let responses = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let enrollments = Arc::new(AtomicUsize::new(0));
        let relay_enrollments = enrollments.clone();
        let deletions = Arc::new(AtomicUsize::new(0));
        let relay_deletions = deletions.clone();
        let relay_responses = responses.clone();
        let relay = Router::new()
            .route(
                "/v1/devices",
                post(move || {
                    let enrollments = relay_enrollments.clone();
                    async move {
                        enrollments.fetch_add(1, Ordering::SeqCst);
                        axum::Json(json!({"ok":true}))
                    }
                }),
            )
            .route(
                "/v1/devices/{id}",
                delete(move || {
                    let deletions = relay_deletions.clone();
                    async move {
                        deletions.fetch_add(1, Ordering::SeqCst);
                        StatusCode::NO_CONTENT
                    }
                }),
            )
            .route(
                "/v1/work/{id}/receipt",
                post(|headers: HeaderMap| async move {
                    assert!(headers.contains_key("x-device-signature"));
                    axum::Json(json!({"status":"received"}))
                }),
            )
            .route(
                "/v1/work/{id}/response",
                post(
                    move |State(saved): State<Arc<Mutex<Vec<Vec<u8>>>>>, body: Bytes| async move {
                        saved.lock().unwrap().push(body.to_vec());
                        axum::Json(json!({"status":"completed"}))
                    },
                ),
            )
            .with_state(relay_responses);
        let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay_port = relay_listener.local_addr().unwrap().port();
        let relay_task = tokio::spawn(axum::serve(relay_listener, relay).into_future());

        let mac_signing = SigningKey::from_slice(&[1u8; 32]).unwrap();
        let mac_agreement = SecretKey::from_slice(&[2u8; 32]).unwrap();
        let phone_signing = SigningKey::from_slice(&[3u8; 32]).unwrap();
        let phone_agreement = SecretKey::from_slice(&[4u8; 32]).unwrap();
        let home = std::env::temp_dir().join(format!(
            "alondra-service-e2e-home-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("devices.json"), serde_json::to_vec(&json!([{
            "id":"phone-1","name":"Test phone","public_key":STANDARD.encode(phone_signing.verifying_key().to_sec1_bytes()),
            "created_at":0,"last_seen":null
        }])).unwrap()).unwrap();
        let auth = Arc::new(Auth::open(&home).unwrap());
        let journal_path = home.join("service-receipts.sqlite");
        let service = Service {
            relay: Url::parse(&format!("http://127.0.0.1:{relay_port}/")).unwrap(),
            client: Client::new(),
            mac_id: "mac-1".into(),
            owner_sub: "owner".into(),
            signing: mac_signing,
            agreement: mac_agreement,
            devices: HashMap::from([(
                "phone-1".into(),
                Device {
                    signing: *phone_signing.verifying_key(),
                    agreement: phone_agreement.public_key(),
                    signing_wire: URL_SAFE_NO_PAD
                        .encode(phone_signing.verifying_key().to_sec1_bytes()),
                },
            )]),
            home: home.clone(), transfers: Arc::default(),
            auth: auth.clone(),
            journal: Journal::open(journal_path.clone()).unwrap(),
            local_port,
        };
        let epoch = [7u8; 16];
        service.enroll(&epoch).await.unwrap();
        service.enroll(&[8u8; 16]).await.unwrap();
        assert_eq!(
            enrollments.load(Ordering::SeqCst),
            1,
            "reconnect cannot resurrect a relay-revoked device"
        );
        let restarted = Service {
            relay: service.relay.clone(),
            client: Client::new(),
            mac_id: service.mac_id.clone(),
            owner_sub: service.owner_sub.clone(),
            signing: SigningKey::from_slice(&[1u8; 32]).unwrap(),
            agreement: SecretKey::from_slice(&[2u8; 32]).unwrap(),
            devices: HashMap::from([(
                "phone-1".into(),
                Device {
                    signing: *phone_signing.verifying_key(),
                    agreement: phone_agreement.public_key(),
                    signing_wire: URL_SAFE_NO_PAD
                        .encode(phone_signing.verifying_key().to_sec1_bytes()),
                },
            )]),
            home: home.clone(), transfers: Arc::default(),
            auth: Arc::new(Auth::open(&home).unwrap()),
            journal: Journal::open(journal_path.clone()).unwrap(),
            local_port,
        };
        restarted.enroll(&[9u8; 16]).await.unwrap();
        assert_eq!(
            enrollments.load(Ordering::SeqCst),
            1,
            "restart cannot resurrect a relay-revoked device"
        );
        let plaintext =
            br#"{"method":"POST","path":"/v1/questions/1/answer","body":{"text":"hello"}}"#;
        let e = EnvelopeV1::seal_random(
            EnvelopeContext {
                route: "mac-1",
                sender: "phone-1",
                recipient: "mac-1",
                request_id: "op-1",
                kind: "request",
                expires_at_ms: now_ms() + 60_000,
            },
            plaintext,
            &phone_signing,
            &service.agreement.public_key(),
        )
        .unwrap();
        let oversized = service.encode_response(&e, service.devices.get("phone-1").unwrap(),
            &json!({"status":200,"body":{"text":"\u{0}".repeat(MAX_LOCAL_RESPONSE)}}),
            &LocalRequest { method: "GET".into(), path: "/v1/file?path=test".into(), body: None }).unwrap();
        assert!(oversized.len() <= MAX_RELAY_RESPONSE);
        let oversized_envelope: EnvelopeV1 = serde_json::from_slice(&oversized).unwrap();
        let opened = oversized_envelope.open(service.signing.verifying_key(), &phone_agreement,
            "mac-1", "phone-1", "mac-1", now_ms()).unwrap();
        let error: Value = serde_json::from_slice(&opened).unwrap();
        assert_eq!(error["error"], "response_too_large");
        let work = || Work {
            request_id: "op-1".into(),
            envelope: e.clone(),
        };
        service.handle(work(), &epoch).await.unwrap();
        service.handle(work(), &epoch).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let sent = responses.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0], sent[1]);
        let response: EnvelopeV1 = serde_json::from_slice(&sent[0]).unwrap();
        let opened = response
            .open(
                service.signing.verifying_key(),
                &phone_agreement,
                "mac-1",
                "phone-1",
                "mac-1",
                now_ms(),
            )
            .unwrap();
        let content: Value = serde_json::from_slice(&opened).unwrap();
        assert_eq!(content["status"], 200);
        assert_eq!(content["body"]["saved"], true);
        drop(sent);
        let mut altered = e;
        altered.ciphertext = "changed".into();
        assert!(
            service
                .handle(
                    Work {
                        request_id: "op-1".into(),
                        envelope: altered
                    },
                    &epoch
                )
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let interrupted = EnvelopeV1::seal_random(
            EnvelopeContext {
                route: "mac-1",
                sender: "phone-1",
                recipient: "mac-1",
                request_id: "op-2",
                kind: "request",
                expires_at_ms: now_ms() + 60_000,
            },
            plaintext,
            &phone_signing,
            &service.agreement.public_key(),
        )
        .unwrap();
        let digest: [u8; 32] = Sha256::digest(serde_json::to_vec(&interrupted).unwrap()).into();
        service.journal.accept("phone-1", "op-2", &digest).unwrap();
        service
            .journal
            .executing("phone-1", "op-2", &digest)
            .unwrap();
        service
            .handle(
                Work {
                    request_id: "op-2".into(),
                    envelope: interrupted,
                },
                &epoch,
            )
            .await
            .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "interrupted action must not run twice"
        );
        let saved = responses.lock().unwrap();
        let uncertain: EnvelopeV1 = serde_json::from_slice(saved.last().unwrap()).unwrap();
        let opened = uncertain
            .open(
                service.signing.verifying_key(),
                &phone_agreement,
                "mac-1",
                "phone-1",
                "mac-1",
                now_ms(),
            )
            .unwrap();
        let content: Value = serde_json::from_slice(&opened).unwrap();
        assert_eq!(content["error"], "uncertain");
        drop(saved);
        assert!(auth.revoke("phone-1").unwrap());
        service
            .sync_revocations(&[10u8; 16], &mut HashSet::new())
            .await
            .unwrap();
        assert_eq!(deletions.load(Ordering::SeqCst), 1);
        service.enroll(&[10u8; 16]).await.unwrap();
        restarted
            .sync_revocations(&[11u8; 16], &mut HashSet::new())
            .await
            .unwrap();
        assert_eq!(
            deletions.load(Ordering::SeqCst),
            2,
            "restart resends signed DELETE"
        );
        restarted.enroll(&[11u8; 16]).await.unwrap();
        assert_eq!(
            enrollments.load(Ordering::SeqCst),
            1,
            "local revoke remains denied after restart and reconnect"
        );
        let removed_from_config = Service {
            relay: service.relay.clone(),
            client: Client::new(),
            mac_id: service.mac_id.clone(),
            owner_sub: service.owner_sub.clone(),
            signing: SigningKey::from_slice(&[1u8; 32]).unwrap(),
            agreement: SecretKey::from_slice(&[2u8; 32]).unwrap(),
            devices: HashMap::new(),
            home: home.clone(), transfers: Arc::default(),
            auth: Arc::new(Auth::open(&home).unwrap()),
            journal: Journal::open(journal_path).unwrap(),
            local_port,
        };
        removed_from_config
            .sync_revocations(&[12u8; 16], &mut HashSet::new())
            .await
            .unwrap();
        assert_eq!(
            deletions.load(Ordering::SeqCst),
            3,
            "removing a phone from service.toml must not suppress its relay revocation"
        );
        let revoked = EnvelopeV1::seal_random(
            EnvelopeContext {
                route: "mac-1",
                sender: "phone-1",
                recipient: "mac-1",
                request_id: "op-3",
                kind: "request",
                expires_at_ms: now_ms() + 60_000,
            },
            plaintext,
            &phone_signing,
            &service.agreement.public_key(),
        )
        .unwrap();
        assert!(
            service
                .handle(
                    Work {
                        request_id: "op-3".into(),
                        envelope: revoked
                    },
                    &epoch
                )
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        local_task.abort();
        relay_task.abort();
    }
}
