//! Optional outbound Mac connector. Enabled only by `~/.alondra/service.toml`.
//! The relay never supplies trusted endpoint keys: those are pinned in this Mac's config.
//! A device must also remain approved in the local pairing registry. Enrollment is a one-time
//! durable attempt per device ID so a relay-side revocation survives reconnects and restarts.
use crate::auth::Auth;
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

    pub async fn run(self) {
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
                let bytes = self.encode_response(e, device, &json!({"status":409,"error":"uncertain","detail":"execution was interrupted; inspect the Mac before retrying this action"}))?;
                self.journal.complete(&e.sender, &e.request_id, &bytes)?;
                bytes
            }
            Disposition::Accepted => {
                if !self.auth.approved_key_matches(&e.sender, &device.signing) {
                    bail!("device was revoked before execution");
                }
                self.journal.executing(&e.sender, &e.request_id, &digest)?;
                let result = match self.dispatch(&request).await {
                    Ok(value) => value,
                    Err(error) => {
                        json!({"status":503,"error":"uncertain","detail":error.to_string()})
                    }
                };
                let bytes = self.encode_response(e, device, &result)?;
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
    ) -> Result<Vec<u8>> {
        let payload = serde_json::to_vec(result)?;
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
    }

    async fn dispatch(&self, request: &LocalRequest) -> Result<Value> {
        let method = approved_method(request)?;
        let url = format!("http://127.0.0.1:{}{}", self.local_port, request.path);
        let mut builder = self.client.request(method, url);
        if let Some(body) = &request.body {
            builder = builder.json(body);
        }
        let mut response = builder.send().await?;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len().saturating_add(chunk.len()) > MAX_LOCAL_RESPONSE {
                bail!("local response exceeds encrypted relay limit");
            }
            bytes.extend_from_slice(&chunk);
        }
        let body = serde_json::from_slice::<Value>(&bytes)
            .unwrap_or_else(|_| json!({"text":String::from_utf8_lossy(&bytes)}));
        Ok(json!({"status":status,"body":body}))
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
    // Reject ambiguous/double encoded query values and all extra parameters.
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
        let c = Connection::open(&path)?;
        c.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS service_receipts (
              sender TEXT NOT NULL, request_id TEXT NOT NULL, digest BLOB NOT NULL,
              state TEXT NOT NULL, response BLOB, PRIMARY KEY(sender, request_id));
            CREATE TABLE IF NOT EXISTS service_enrollments (
              device_id TEXT PRIMARY KEY, signing_key BLOB NOT NULL, attempted_at_ms INTEGER NOT NULL,
              state TEXT NOT NULL);",
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
        let c = self.db()?;
        c.execute(
            "UPDATE service_enrollments SET state='revoked' WHERE device_id=?1",
            params![id],
        )?;
        Ok(())
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
        routing::{delete, post},
    };
    use base64::engine::general_purpose::STANDARD;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    #[test]
    fn local_dispatch_allowlist_rejects_path_tricks() {
        let accepts = |method: &str, path: &str, body: Option<Value>| approved_method(&LocalRequest {
            method: method.into(), path: path.into(), body
        }).is_ok();
        for path in [
            "/v1/overview", "/v1/tasks/task-1", "/v1/nodes/session_1/activity",
            "/v1/prs/7/diff", "/v1/search?q=hello%20world",
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
