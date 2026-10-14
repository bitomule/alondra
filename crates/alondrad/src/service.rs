//! The Mac's encrypted service, enabled by `~/.alondra/service.toml`. Devices reach it through the
//! relay connector (`service/relay.rs`) or directly over the local network (`crate::direct`);
//! both carry the same end-to-end envelopes into this core.
//! Trusted endpoint keys are pinned in this Mac's config, never supplied by a transport.
//! A device must also remain approved in the local pairing registry. Relay enrollment is a
//! one-time durable attempt per device ID so a relay-side revocation survives reconnects and
//! restarts; the direct transport needs only the local approval.
use crate::auth::Auth;
use crate::push::{in_quiet_hours, valid_push_binding};
use alondra_e2e::{Context as EnvelopeContext, Envelope};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::{
    PublicKey, SecretKey,
    ecdsa::{SigningKey, VerifyingKey},
};
use reqwest::{Client, Method};
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

mod relay;
pub(crate) mod operation_results;

// Base64 adds ~33%; 760 KiB plus encryption and envelope metadata stays below 1 MiB.
pub(crate) const MAX_LOCAL_RESPONSE: usize = 760 * 1024;
pub(crate) const MAX_RELAY_RESPONSE: usize = 1024 * 1024;
// A 2 MiB diff whose every byte needs a six-byte JSON escape, plus the snapshot fields.
const MAX_LOCAL_DIFF_SNAPSHOT: usize = 12 * 1024 * 1024 + 4096;
// The relay wraps the envelope in a 4 KiB APNs alert. Leave room for its
// generic aps fields, binding, JSON escaping, and future metadata.
const MAX_PUSH_BODY: usize = 3_000;
const MAX_PUSH_PLAINTEXT: usize = 1_500;
// The legacy changes route remains available until its final caller migrates.
// The views feed supersedes its prior waiter independently of this limit.
const MAX_CHANGES_PER_DEVICE: usize = 1;
const MAX_CHANGES_TOTAL: usize = 16;
const MAX_JOURNAL_ACCEPTS: usize = MAX_CHANGES_TOTAL + relay::MAX_ORDINARY_DELIVERIES;
const MAX_JOURNAL_RECEIPTS: usize = MAX_CHANGES_TOTAL + relay::MAX_ORDINARY_DELIVERIES + 1;

type WorkKey = (String, String);
type InFlight = Arc<InFlightSet>;

/// The envelopes executing now, and a wake-up for a direct retry waiting on one of them.
#[derive(Default)]
struct InFlightSet {
    work: Mutex<HashMap<WorkKey, ([u8; 32], bool)>>,
    done: tokio::sync::Notify,
}

impl std::ops::Deref for InFlightSet {
    type Target = Mutex<HashMap<WorkKey, ([u8; 32], bool)>>;
    fn deref(&self) -> &Self::Target {
        &self.work
    }
}

/// The same request ID arrived with different encrypted bytes.
#[derive(Debug)]
struct ReusedRequestId;

impl std::fmt::Display for ReusedRequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("request ID reused with different encrypted bytes")
    }
}

impl std::error::Error for ReusedRequestId {}

/// Why the direct transport refused an envelope; it maps to the HTTP status.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DirectRefusal {
    /// Addressed to another Mac, or not a request.
    Misdirected,
    /// Not from a device approved on this Mac, badly signed or expired.
    Unauthorized,
    /// The request ID was used before with other bytes.
    Conflict,
    /// From an approved device, but not something the service runs.
    Rejected(String),
    /// From a device revoked on this Mac and signed by it: the sealed answer saying so.
    Revoked(Vec<u8>),
}

/// A live executor owns the receipt until response upload finishes. A lease
/// redelivery must not mistake its durable `executing` row for a crashed process.
struct WorkGuard {
    inflight: InFlight,
    key: WorkKey,
    changes: bool,
    saturated: bool,
}
impl Drop for WorkGuard {
    fn drop(&mut self) {
        self.inflight
            .lock()
            .expect("inflight lock poisoned")
            .remove(&self.key);
        self.inflight.done.notify_waiters();
    }
}

enum MutationTurn {
    Acquired(tokio::sync::OwnedMutexGuard<()>),
    Waiting(std::pin::Pin<Box<dyn std::future::Future<Output = tokio::sync::OwnedMutexGuard<()>> + Send>>),
}

impl MutationTurn {
    async fn acquire(self) -> tokio::sync::OwnedMutexGuard<()> {
        match self { Self::Acquired(guard) => guard, Self::Waiting(waiting) => waiting.await }
    }
}

enum ExecutionLane {
    Serial,
    Read,
    Mutation(MutationTurn),
}

struct PreparedWork {
    request: LocalRequest,
    digest: [u8; 32],
    guard: Arc<WorkGuard>,
    execution: ExecutionLane,
}

/// Device preferences are stored on the executor and travel only inside E2E
/// envelopes. Clock semantics deliberately match legacy: executor local time.
#[derive(Default, Debug, PartialEq)]
struct QuietHours {
    start: Option<String>,
    end: Option<String>,
}
impl QuietHours {
    fn parse_update(start: Option<&Value>, end: Option<&Value>) -> Result<Option<Self>> {
        match (start, end) {
            (None, None) => Ok(None),
            (Some(Value::Null), Some(Value::Null)) => Ok(Some(Self::default())),
            (Some(Value::String(start)), Some(Value::String(end)))
                if Self::valid_time(start) && Self::valid_time(end) =>
            {
                Ok(Some(Self {
                    start: Some(start.clone()),
                    end: Some(end.clone()),
                }))
            }
            _ => bail!("quiet_start and quiet_end must both be HH:mm or both null"),
        }
    }
    fn valid_time(value: &str) -> bool {
        let b = value.as_bytes();
        b.len() == 5
            && b[2] == b':'
            && [b[0], b[1], b[3], b[4]].iter().all(u8::is_ascii_digit)
            && value[..2].parse::<u32>().is_ok_and(|h| h < 24)
            && value[3..].parse::<u32>().is_ok_and(|m| m < 60)
    }
    fn apply(&self, payload: &Value, with_sound: bool, now: u32) -> (Value, bool) {
        let quiet = in_quiet_hours(self.start.as_deref(), self.end.as_deref(), now);
        let mut payload = payload.clone();
        payload["aps"]["interruption-level"] = json!(if quiet { "passive" } else { "active" });
        if (quiet || !with_sound)
            && let Some(aps) = payload["aps"].as_object_mut()
        {
            aps.remove("sound");
        }
        (payload, with_sound && !quiet)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    relay_url: Option<String>,
    mac_id: String,
    #[serde(default)]
    owner_sub: Option<String>,
    signing_key_path: PathBuf,
    agreement_key_path: PathBuf,
    #[serde(default)]
    journal_path: Option<PathBuf>,
    devices: Vec<DeviceConfig>,
    #[serde(default)]
    direct: DirectConfig,
}

/// The local-network listener in `service.toml`'s `[direct]` table.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct DirectConfig {
    pub(crate) enabled: bool,
    pub(crate) port: u16,
    pub(crate) listen: std::net::IpAddr,
}

impl Default for DirectConfig {
    fn default() -> Self {
        Self { enabled: true, port: 47810, listen: std::net::Ipv4Addr::UNSPECIFIED.into() }
    }
}

/// How an envelope reached the Mac. Both need the device approved on this Mac; only the relay
/// also needs the device enrolled with the relay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Transport {
    Relay,
    Direct,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceConfig {
    device_id: String,
    signing_key: String,
    agreement_key: String,
}

#[derive(Clone)]
struct Device {
    signing: VerifyingKey,
    agreement: PublicKey,
    signing_wire: String,
}

/// The approved phones, replaceable in place when `service.toml` changes.
#[derive(Default)]
struct Devices(RwLock<HashMap<String, Arc<Device>>>);

impl Devices {
    fn get(&self, id: &str) -> Option<Arc<Device>> {
        self.0
            .read()
            .expect("devices lock poisoned")
            .get(id)
            .cloned()
    }

    fn snapshot(&self) -> Vec<(String, Arc<Device>)> {
        self.0
            .read()
            .expect("devices lock poisoned")
            .iter()
            .map(|(id, device)| (id.clone(), device.clone()))
            .collect()
    }

    fn replace(&self, devices: HashMap<String, Device>) {
        *self.0.write().expect("devices lock poisoned") = devices
            .into_iter()
            .map(|(id, device)| (id, Arc::new(device)))
            .collect();
    }
}

impl From<HashMap<String, Device>> for Devices {
    fn from(devices: HashMap<String, Device>) -> Self {
        let set = Self::default();
        set.replace(devices);
        set
    }
}

fn load_devices(mac_id: &str, configured: Vec<DeviceConfig>) -> Result<HashMap<String, Device>> {
    let mut devices = HashMap::new();
    for d in configured {
        if d.device_id.is_empty() || d.device_id == mac_id {
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
    Ok(devices)
}

pub struct Service {
    relay: Option<relay::RelayConnector>,
    client: Client,
    mac_id: String,
    signing: SigningKey,
    agreement: SecretKey,
    devices: Devices,
    revoked: crate::revoked_devices::RevokedDevices,
    auth: Arc<Auth>,
    journal: Journal,
    local_port: u16,
    home: PathBuf,
    transfers: Arc<crate::service_files::Transfers>,
    inflight: InFlight,
    hub: Option<Arc<crate::hub::Hub>>,
    domain: Option<Arc<crate::domain_runtime::DomainRuntime>>,
    routes: Option<Arc<crate::client_routes::Client>>,
    direct: DirectConfig,
    /// Writers from either transport share their turn; scheduled relay reads bypass it.
    lane: Arc<tokio::sync::Mutex<()>>,
    #[cfg(test)]
    envelope_clock: Option<Arc<std::sync::atomic::AtomicU64>>,
}

/// A verified encrypted sender with persisted approval (and, through the relay, enrollment).
/// Only Service can construct it; readers recheck after asynchronous work.
pub(crate) struct ApprovedDevice<'a> {
    service: &'a Service,
    sender: &'a str,
    device: &'a Device,
    transport: Transport,
}
impl ApprovedDevice<'_> {
    pub(crate) fn recheck(&self) -> Result<()> {
        self.service
            .require_device_approval(self.sender, self.device, self.transport)
    }
}

#[derive(Deserialize)]
struct Work {
    request_id: String,
    envelope: Envelope,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalRequest {
    method: String,
    path: String,
    #[serde(default)]
    body: Option<Value>,
}

fn approval_bound_read_digest(envelope: &[u8], binding: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"alondra:approved-read:receipt:v1\0");
    hash.update(envelope);
    hash.update([0]);
    hash.update(binding.as_bytes());
    hash.finalize().into()
}

/// Where a domain client registers its push preferences; the older route serves the legacy daemon.
const PUSH_BINDING_ROUTE: &str = "/device/push-binding";
const PUSH_BINDING_REMOVE_ROUTE: &str = "/device/push-binding/remove";

fn parse_push_binding_removal(request: &LocalRequest) -> Result<String> {
    let map = request
        .body
        .as_ref()
        .and_then(Value::as_object)
        .context("push binding removal body required")?;
    if map.keys().any(|key| key != "push_binding") {
        bail!("unknown push binding removal field");
    }
    Ok(map
        .get("push_binding")
        .and_then(Value::as_str)
        .filter(|value| valid_push_binding(value))
        .context("invalid push binding")?
        .to_owned())
}

fn parse_push_preferences(
    request: &LocalRequest,
) -> Result<(String, Option<QuietHours>, Option<i64>)> {
    let body = request
        .body
        .as_ref()
        .context("push binding body required")?;
    let map = body.as_object().context("invalid push binding body")?;
    if map.keys().any(|key| {
        !matches!(
            key.as_str(),
            "push_binding" | "quiet_start" | "quiet_end" | "preferences_revision"
        )
    }) {
        bail!("unknown push binding field");
    }
    let binding = map
        .get("push_binding")
        .and_then(Value::as_str)
        .filter(|value| valid_push_binding(value))
        .context("invalid push binding")?;
    let quiet = QuietHours::parse_update(map.get("quiet_start"), map.get("quiet_end"))?;
    let revision = match map.get("preferences_revision") {
        None => None,
        Some(value) => Some(
            value
                .as_u64()
                .filter(|v| (1..=9_007_199_254_740_991).contains(v))
                .context("preferences_revision must be an integer from 1 to 9007199254740991")?
                as i64,
        ),
    };
    if revision.is_some() && quiet.is_none() {
        bail!("versioned preferences require quiet_start and quiet_end");
    }
    Ok((binding.to_owned(), quiet, revision))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Once approval is checked, revocation cannot complete until the encrypted
/// response has been accepted by the relay. The lease is independent of the
/// SQLite receipt transaction, which has already committed at this point.
async fn approval_bound_delivery<T, F, C, D>(home: &Path, check: C, deliver: D) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
    C: FnOnce() -> Result<()>,
    D: FnOnce() -> F,
{
    let _lease = crate::approval_lease::ApprovalLease::shared(home)?;
    check()?;
    deliver().await
}

/// For a local durable write, hold the same approval lease from the final
/// check through the SQLite commit. Revocation takes this lease exclusively.
fn approval_bound_commit<T>(
    home: &Path,
    check: impl FnOnce() -> Result<()>,
    commit: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let _lease = crate::approval_lease::ApprovalLease::shared(home)?;
    check()?;
    commit()
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
    fn envelope_now(&self) -> u64 {
        #[cfg(test)]
        if let Some(clock) = &self.envelope_clock { return clock.load(std::sync::atomic::Ordering::Relaxed); }
        now_ms()
    }

    pub fn with_hub(mut self, hub: Arc<crate::hub::Hub>) -> Self {
        self.hub = Some(hub);
        self
    }
    pub fn with_domain(mut self, domain: Arc<crate::domain_runtime::DomainRuntime>) -> Self {
        self.domain = Some(domain);
        self
    }
    /// Resolves the receipts whose executor died with the previous daemon process. Only the daemon
    /// calls it, once, before it takes any work.
    pub(crate) fn recover_interrupted(&self) -> Result<()> {
        let (reads, operations) = self.journal.recover_interrupted()?;
        if reads + operations > 0 {
            log!("the previous daemon stopped in the middle of {operations} operation(s) and {reads} read(s); they are marked interrupted so devices can retire or recover them");
        }
        Ok(())
    }

    pub(crate) fn with_client(mut self, client: Arc<crate::client_routes::Client>) -> Self {
        self.routes = Some(client);
        self
    }

    fn read_receipt_binding(
        &self,
        sender: &str,
        device: &Device,
        transport: Transport,
    ) -> Result<String> {
        let binding = self.approval_binding(sender, device, transport)?;
        let domain = self
            .domain
            .as_deref()
            .or_else(|| self.hub.as_deref().map(|hub| &*hub.domain));
        match domain {
            Some(domain) => Ok(format!(
                "{binding}:domain-v2:{}",
                domain.state()?.data_epoch
            )),
            None => Ok(binding),
        }
    }

    fn atomic_local(&self, request: &LocalRequest) -> bool {
        if self.domain.is_some() {
            return crate::domain_api::is_resource_mutation_path(&request.method, &request.path)
                || crate::views::actions::approved_path(&request.method, &request.path)
                || request.method == "POST"
                    && (request.path == PUSH_BINDING_ROUTE
                        || request.path == PUSH_BINDING_REMOVE_ROUTE
                        || crate::client_routes::atomic_write_path(&request.path)
                        || agent_message(&request.path).is_some()
                        || crate::domain_orchestrator::decision_route(&request.path).is_some()
                        || crate::domain_api::resolve_delivery_attempt_id(&request.path)
                            .is_some())
                || crate::domain_schedule_api::approved_mutation_path(
                    &request.method,
                    &request.path,
                    request.body.is_some(),
                );
        }
        request.method == "POST"
            && (request.path == "/v1/service/push-binding"
                || self.hub.is_some()
                    && crate::domain_api::resolve_delivery_attempt_id(&request.path).is_some())
            || self.hub.is_some()
                && (crate::http::atomic_local_path(&request.method, &request.path)
                    || crate::schedules::atomic_local_path(&request.method, &request.path))
    }

    async fn execute_atomic_local(
        &self,
        envelope: &Envelope,
        device: &Device,
        request: &LocalRequest,
        digest: &[u8; 32],
        transport: Transport,
        receipt: Arc<ReceiptAdmission>,
    ) -> Result<Vec<u8>> {
        self.require_device_approval(&envelope.sender, device, transport)?;
        if self.domain.is_some() && request.path == PUSH_BINDING_REMOVE_ROUTE {
            let result = match parse_push_binding_removal(request) {
                Ok(binding) => {
                    self.journal.forget_push_binding(&envelope.sender, &binding)?;
                    json!({"status":204})
                }
                Err(_) => json!({"status":422,"body":{"error":"invalid push binding removal"}}),
            };
            let bytes = self.encode_response(envelope, device, &result, request)?;
            let bytes = self.journal
                .complete_admitted(&envelope.sender, &envelope.request_id, bytes, receipt.clone()).await?;
            return Ok(bytes);
        }
        if request.path == "/v1/service/push-binding"
            || self.domain.is_some() && request.path == PUSH_BINDING_ROUTE
        {
            let (binding, quiet, revision) = match parse_push_preferences(request) {
                Ok(parsed) => parsed,
                Err(_) => {
                    let bytes = self.encode_response(
                        envelope,
                        device,
                        &json!({"status":422,"body":{"error":"invalid push preferences"}}),
                        request,
                    )?;
                    let bytes = self.journal
                        .complete_admitted(&envelope.sender, &envelope.request_id, bytes, receipt.clone()).await?;
                    return Ok(bytes);
                }
            };
            #[cfg(test)]
            self.journal.before_write(ReceiptWrite::PushPreferences, &envelope.request_id).await;
            let (home, auth, envelope, device, request, digest, signing, mac_id) = (
                self.home.clone(), self.auth.clone(), envelope.clone(), device.clone(),
                request.clone(), *digest, self.signing.clone(), self.mac_id.clone(),
            );
            #[cfg(test)]
            let clock = self.envelope_clock.clone();
            return self.journal.write_receipt(receipt, move |journal| {
                approval_bound_commit(
                    &home,
                    || Self::require_approval(&auth, &home, journal, &envelope.sender, &device, transport),
                    || journal.complete_push_preferences(
                        &envelope.sender, &envelope.request_id, &digest, &binding, quiet, revision,
                        |mut result| {
                            if request.path == PUSH_BINDING_ROUTE && result["error"] == "stale_push_preferences" {
                                result["kind"] = json!("stale_push_preferences");
                            }
                            let now = now_ms();
                            #[cfg(test)]
                            let now = clock.as_ref().map_or(now, |clock| clock.load(std::sync::atomic::Ordering::Relaxed));
                            Self::seal_response(&signing, &mac_id, now + 600_000, &envelope, &device, &result, &request)
                        },
                    ),
                )
            }).await;
        }
        approved_method(request)?;
        if let Some(domain) = &self.domain {
            let external = request.path == "/requests" || request.path == "/orchestrator/messages"
                || agent_message(&request.path).is_some()
                || request.path == "/actions" && request.body.as_ref().is_some_and(crate::views::actions::external_request);
            let result = {
                let (domain, routes, home, auth, journal, device, sender, request_id, request, digest) = (
                    domain.clone(), self.routes.clone(), self.home.clone(), self.auth.clone(),
                    self.journal.clone(), device.clone(), envelope.sender.clone(),
                    envelope.request_id.clone(), request.clone(), *digest,
                );
                let work = if external { domain.external_work.clone() } else { domain.command_work.clone() };
                let runtime = tokio::runtime::Handle::current();
                let physical_receipt = receipt.clone();
                work.run(move || {
                    let _receipt = physical_receipt;
                    let approval_home = home.clone();
                    let approval_sender = sender.clone();
                    runtime.block_on(Self::execute_domain_request(
                        domain, routes, home, sender, request_id, request, digest,
                        move || Self::require_approval(&auth, &approval_home, &journal, &approval_sender, &device, transport),
                    ))
                }).await??
            };
            let bytes = self.encode_response(envelope, device, &result, request)?;
            let bytes = self.journal
                .complete_admitted(&envelope.sender, &envelope.request_id, bytes, receipt.clone()).await?;
            return Ok(bytes);
        }
        let hub = self
            .hub
            .as_ref()
            .context("atomic local executor unavailable")?;
        let result = if crate::domain_api::resolve_delivery_attempt_id(&request.path).is_some() {
            json!({"status":409,"body":crate::domain_api::legacy_write_blocked()})
        } else if crate::schedules::atomic_local_path(&request.method, &request.path) {
            crate::schedules::atomic_local_request(
                hub,
                &envelope.sender,
                &envelope.request_id,
                digest,
                &request.method,
                &request.path,
                request.body.as_ref(),
            )?
        } else {
            crate::http::atomic_local_request(
                hub,
                &envelope.sender,
                &envelope.request_id,
                digest,
                &request.path,
                request.body.as_ref(),
                || self.require_device_approval(&envelope.sender, device, transport),
            )
            .await?
        };
        let bytes = self.encode_response(envelope, device, &result, request)?;
        let bytes = self.journal
            .complete_admitted(&envelope.sender, &envelope.request_id, bytes, receipt.clone()).await?;
        Ok(bytes)
    }

    async fn execute_domain_request(
        domain: Arc<crate::domain_runtime::DomainRuntime>,
        routes: Option<Arc<crate::client_routes::Client>>,
        home: PathBuf,
        sender: String,
        request_id: String,
        request: LocalRequest,
        digest: [u8; 32],
        authorize: impl Fn() -> Result<()> + Send + Sync,
    ) -> Result<Value> {
        if authorize().is_err() {
            return Ok(json!({"status":403, "body":alondra_core::domain::DomainError::new("forbidden", "device approval was revoked")}));
        }
        let result = if crate::views::actions::approved_path(&request.method, &request.path) {
            let routes = routes.as_ref().context("client routes unavailable")?;
            let body = request.body.as_ref().context("action body required")?;
            crate::views::actions::relay(
                routes,
                &sender,
                body,
                (&sender, &request_id, &digest),
                &authorize,
            )
            .await?
        } else if crate::domain_api::is_resource_mutation_path(&request.method, &request.path) {
            crate::domain_api::encrypted_resource_command_runtime(
                &domain,
                routes.as_deref(),
                &sender,
                &request.method,
                &request.path,
                request.body.as_ref(),
                (&sender, &request_id, &digest),
                &authorize,
            )
            .await?
        } else if let Some(attempt_id) =
            crate::domain_api::resolve_delivery_attempt_id(&request.path)
        {
            crate::domain_api::encrypted_resolve_delivery_runtime(
                &domain,
                &home,
                &sender,
                attempt_id,
                request.body.as_ref(),
                (&sender, &request_id, &digest),
                &authorize,
            )?
        } else if let Some(agent) = agent_message(&request.path) {
            crate::domain_messages::relayed(
                &domain,
                &home,
                &sender,
                agent,
                request.body.as_ref(),
            )
            .await
        } else if let Some(decision) = crate::domain_orchestrator::decision_route(&request.path)
        {
            crate::domain_orchestrator::decide(
                &crate::domain_orchestrator::executor(domain.clone(), &home),
                decision,
                request.body.as_ref(),
            )
        } else if let Some(route) = crate::client_routes::parse_write(&request.path)
            .filter(|_| crate::client_routes::atomic_write_path(&request.path))
        {
            let routes = routes.as_ref().context("client routes unavailable")?;
            let body = request.body.as_ref().context("request body required")?;
            let identity = (
                sender.as_str(),
                request_id.as_str(),
                &digest,
            );
            routes
                .write(
                    &format!("device:{}", sender),
                    route,
                    body,
                    Some(identity),
                    &authorize,
                )
                .await?
                .into_value()
        } else if crate::domain_schedule_api::approved_mutation_path(
            &request.method,
            &request.path,
            request.body.is_some(),
        ) {
            crate::domain_schedule_api::encrypted_mutation(
                &domain,
                &home,
                &sender,
                &request.method,
                &request.path,
                request.body.as_ref(),
                (&sender, &request_id, &digest),
                &authorize,
            )
        } else {
            bail!("domain service command path is not approved");
        };
        Ok(result)
    }

    pub fn open(home: &Path, local_port: u16, auth: Arc<Auth>) -> Result<Option<Self>> {
        let file = home.join("service.toml");
        if !file.exists() {
            return Ok(None);
        }
        let config: Config = toml::from_str(&std::fs::read_to_string(&file)?)
            .with_context(|| format!("invalid {}", file.display()))?;
        if config.mac_id.is_empty() {
            bail!("service Mac ID is required");
        }
        let relay = match (config.relay_url, config.owner_sub) {
            (None, None) => None,
            (Some(url), Some(owner)) if !owner.is_empty() => {
                Some(relay::RelayConnector::new(relay::checked_origin(&url)?, owner))
            }
            _ => bail!("relay_url and owner_sub go together"),
        };
        let signing =
            SigningKey::from_slice(&private_key(&resolve(home, &config.signing_key_path))?)?;
        let agreement =
            SecretKey::from_slice(&private_key(&resolve(home, &config.agreement_key_path))?)?;
        let devices = load_devices(&config.mac_id, config.devices)?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            // PR merge and GitHub issue refresh can take close to 90 seconds locally.
            .timeout(Duration::from_secs(95))
            .connect_timeout(Duration::from_secs(10))
            .tcp_keepalive(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(60))
            // A pooled relay connection the network dropped silently fails the next upload with
            // a broken pipe; pings find it first.
            .http2_keep_alive_interval(Duration::from_secs(20))
            .http2_keep_alive_timeout(Duration::from_secs(10))
            .http2_keep_alive_while_idle(true)
            .build()?;
        let journal_path = match config.journal_path {
            Some(path) => {
                anyhow::ensure!(
                    !path.is_absolute()
                        && path
                            .components()
                            .all(|c| matches!(c, std::path::Component::Normal(_))),
                    "service journal must be relative to HOME"
                );
                let joined = home.join(&path);
                anyhow::ensure!(
                    joined
                        .parent()
                        .context("journal parent")?
                        .canonicalize()?
                        .starts_with(home.canonicalize()?),
                    "service journal escapes HOME"
                );
                joined
            }
            None => home.join("service-receipts.sqlite"),
        };
        let journal = Journal::open(journal_path)?;
        Ok(Some(Self {
            relay,
            client,
            mac_id: config.mac_id,
            signing,
            agreement,
            devices: devices.into(),
            revoked: crate::revoked_devices::RevokedDevices::open(home),
            auth,
            journal,
            local_port,
            home: home.to_path_buf(),
            transfers: Arc::default(),
            inflight: Arc::default(),
            hub: None,
            domain: None,
            routes: None,
            direct: config.direct,
            lane: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(test)]
            envelope_clock: None,
        }))
    }

    /// Applies the device list in `service.toml` to the running connector: the new
    /// list is live before this returns and the loop re-enrolls and re-syncs revocations.
    pub fn reload_devices(&self) -> Result<()> {
        let file = self.home.join("service.toml");
        let config: Config = toml::from_str(&std::fs::read_to_string(&file)?)
            .with_context(|| format!("invalid {}", file.display()))?;
        anyhow::ensure!(
            config.mac_id == self.mac_id,
            "service Mac ID changed; restart alondrad"
        );
        self.devices
            .replace(load_devices(&self.mac_id, config.devices)?);
        if let Some(relay) = &self.relay {
            relay.resync();
        }
        Ok(())
    }

    /// Called once a device's approval is gone: its keys stay in `service.toml`, and a direct
    /// request from it is from then on answered, sealed, that it was revoked.
    pub(crate) fn remember_revoked(&self, id: &str) {
        let Some(device) = self.devices.get(id) else { return };
        let tombstone = crate::revoked_devices::Tombstone::new(id, &device.signing, &device.agreement, now_ms());
        if let Err(error) = self.revoked.add(vec![tombstone]) {
            log!("revoked device {id} not remembered: {error:#}");
        }
    }

    pub(crate) fn has_relay(&self) -> bool {
        self.relay.is_some()
    }

    pub(crate) fn direct_config(&self) -> &DirectConfig {
        &self.direct
    }

    pub(crate) fn mac_id(&self) -> &str {
        &self.mac_id
    }

    pub(crate) fn signing_key(&self) -> &SigningKey {
        &self.signing
    }

    pub(crate) fn agreement_public_key(&self) -> PublicKey {
        self.agreement.public_key()
    }

    pub(crate) fn agreement_secret(&self) -> &SecretKey {
        &self.agreement
    }

    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    #[cfg(test)]
    pub(crate) fn enroll_with_relay_for_tests(&self, id: &str) {
        let key = self.devices.get(id).expect("device configured").signing.to_sec1_bytes();
        self.journal.reserve_enrollment(id, &key).unwrap();
        self.journal.activate_enrollment(id).unwrap();
    }

    #[cfg(test)]
    pub(crate) fn set_relay_for_tests(&mut self, url: reqwest::Url) {
        self.set_relay_url(url);
    }

    #[cfg(test)]
    pub(crate) async fn handle_relay_for_tests(&self, envelope: Envelope, epoch: &[u8; 16]) -> Result<()> {
        self.handle(Work { request_id: envelope.request_id.clone(), envelope }, epoch).await
    }

    #[cfg(test)]
    pub(crate) fn has_device(&self, id: &str) -> bool {
        self.devices.get(id).is_some()
    }

    /// How a paired device was paired, for the Mac app's device list: on the local network, or
    /// through the relay with an account. Unknown IDs are this Mac's own clients.
    pub(crate) fn device_origin(&self, id: &str) -> &'static str {
        if self.devices.get(id).is_none() {
            return "mac";
        }
        if crate::direct::paired_directly(&self.home, id) { "direct" } else { "relay" }
    }

    fn prepare_work(&self, work: &Work, transport: Transport) -> Result<Option<PreparedWork>> {
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
        self.require_device_approval(&e.sender, &device, transport)?;
        let plaintext = e
            .open(
                &device.signing,
                &self.agreement,
                &e.sender,
                &self.mac_id,
                &self.mac_id,
                self.envelope_now(),
            )
            .map_err(|error| anyhow!("invalid encrypted work: {error}"))?;
        self.auth.usage().touch(&e.sender, alondra_core::now_millis(), false);
        let request: LocalRequest = serde_json::from_slice(&plaintext)?;
        if e.request_id.starts_with("op_") {
            anyhow::ensure!(alondra_core::operation_id::OperationId::parse(&e.request_id).is_some()
                && crate::views::actions::approved_path(&request.method,&request.path)
                && request.body.as_ref().and_then(|body|body["command_id"].as_str())==Some(e.request_id.as_str()),
                "structured operation cannot be reclassified as a legacy request");
        }
        anyhow::ensure!(
            request.path != "/commands",
            "generic command endpoint is not part of the service contract"
        );
        if self.domain.is_some() {
            anyhow::ensure!(
                (crate::domain_api::approved_resource_read_path(&request.path)
                    || operation_results::lookup(&request.path).is_some()
                    || crate::operation_journal::control(&request.method,&request.path,request.body.is_some())
                    || crate::domain_api::is_resource_mutation_path(
                        &request.method,
                        &request.path
                    )
                    || crate::domain_api::resolve_delivery_attempt_id(&request.path).is_some()
                    || crate::domain_schedule_api::approved_read_path(&request.path)
                    || crate::domain_schedule_api::approved_mutation_path(
                        &request.method,
                        &request.path,
                        request.body.is_some()
                    )
                    || crate::client_routes::approved_read_path(&request.path)
                    || crate::client_routes::approved_write_path(&request.path)
                    || crate::client_routes::upload_step(&request.path).is_some()
                    || crate::client_routes::parse_put(&request.path).is_some()
                    || agent_message(&request.path).is_some()
                    || crate::domain_orchestrator::decision_route(&request.path).is_some()
                    || request.path == PUSH_BINDING_ROUTE
                    || request.path == PUSH_BINDING_REMOVE_ROUTE
                    || request.path == crate::direct::routes::PATH
                    || crate::views::routes::approved_read_path(&request.path)
                    || crate::views::actions::approved_path(&request.method, &request.path))
                    && approved_method(&request).is_ok(),
                "domain service route is not approved"
            );
        } else {
            anyhow::ensure!(
                request.path != PUSH_BINDING_ROUTE,
                "a domain route is not served by the legacy daemon"
            );
        }
        let body = serde_json::to_vec(e)?;
        let approval_bound_read = request.body.is_none() && domain_read_route(&request);
        let digest: [u8; 32] = if approval_bound_read {
            // Completed encrypted receipts must not bypass a newer approval generation.
            // Legacy routes retain their original digest contract.
            let binding = match self.read_receipt_binding(&e.sender, &device, transport) {
                Ok(binding) => binding,
                Err(error) => return Err(error),
            };
            approval_bound_read_digest(&body, &binding)
        } else {
            Sha256::digest(&body).into()
        };
        let is_changes = matches!(
            request.path.split('?').next(),
            Some("/v1/changes" | "/changes")
        ) && approved_method(&request).is_ok();
        let key = (e.sender.clone(), e.request_id.clone());
        let mut inflight = self.inflight.lock().expect("inflight lock poisoned");
        if let Some((prior, _)) = inflight.get(&key) {
            if *prior != digest {
                return Err(ReusedRequestId.into());
            }
            return Ok(None);
        }
        if is_changes && crate::views::feed::approved_path(&request.path) {
            for ((sender, _), (_, changes)) in inflight.iter_mut() {
                if sender == &e.sender && *changes {
                    *changes = false;
                }
            }
        }
        let saturated = is_changes
            && (inflight.values().filter(|(_, changes)| *changes).count() >= MAX_CHANGES_TOTAL
                || inflight
                    .iter()
                    .filter(|((sender, _), (_, changes))| sender == &e.sender && *changes)
                    .count()
                    >= MAX_CHANGES_PER_DEVICE);
        let changes = is_changes && !saturated;
        inflight.insert(key.clone(), (digest, changes));
        drop(inflight);
        Ok(Some(PreparedWork {
            request,
            digest,
            execution: ExecutionLane::Serial,
            guard: Arc::new(WorkGuard {
                inflight: self.inflight.clone(),
                key,
                changes,
                saturated,
            }),
        }))
    }

    /// Runs one prepared envelope exactly once and returns its sealed response. `accepted` runs
    /// once the journal holds the receipt, before execution: the relay acknowledges there.
    /// The guard stays with the caller until the response is delivered.
    async fn execute<A, F>(
        &self,
        e: &Envelope,
        prepared: PreparedWork,
        transport: Transport,
        accepted: A,
    ) -> Result<(Vec<u8>, Arc<Device>, Arc<WorkGuard>)>
    where
        A: FnOnce() -> F,
        F: std::future::Future<Output = Result<()>>,
    {
        let PreparedWork { request, digest, guard, execution } = prepared;
        let device = self
            .devices
            .get(&e.sender)
            .context("work from unapproved device")?;
        let _lane = if guard.changes {
            None
        } else {
            match execution {
                ExecutionLane::Read => None,
                ExecutionLane::Serial => Some(self.lane.clone().lock_owned().await),
                ExecutionLane::Mutation(turn) => Some(turn.acquire().await),
            }
        };
        if crate::operation_journal::control(&request.method,&request.path,request.body.is_some()) {
            accepted().await?;
            self.require_device_approval(&e.sender,&device,transport)?;
            let result = self.operation_control(&e.sender,&request,transport).await?;
            let response = self.encode_response(e,&device,&result,&request)?;
            return Ok((response,device,guard));
        }
        let receipt = self.journal.reserve_receipt(guard.clone())?;
        let recovery_scope = self.operation_scope(e, &request, &device, transport)?;
        let legacy_scope = self.domain.as_ref().map(|domain| {
            self.operation_journal_scope(&e.sender, &device, transport, &domain.state()?.data_epoch.0)
        }).transpose()?;
        let disposition = match self.journal.accept_admitted(&e.sender, &e.request_id, &digest, recovery_scope,request.method=="GET" && request.body.is_none(), legacy_scope, receipt.clone()).await {
            Ok(disposition)=>disposition,
            Err(error) if error.is::<crate::operation_journal::Capacity>()=>{
                accepted().await?;
                self.require_device_approval(&e.sender,&device,transport)?;
                let response = self.encode_response(e,&device,&json!({"status":429,"body":{"error":"operation_capacity"}}),&request)?;
                return Ok((response,device,guard));
            },
            Err(error) if error.is::<crate::operation_journal::Retired>()=>{
                accepted().await?;
                self.require_device_approval(&e.sender,&device,transport)?;
                let response=self.encode_response(e,&device,&json!({"status":410,"body":{"error":"outcome_unknown"}}),&request)?;
                return Ok((response,device,guard));
            },
            Err(error)=>return Err(error),
        };
        accepted().await?;
        if request.body.is_none() && domain_read_route(&request) {
            let binding = self.read_receipt_binding(&e.sender, &device, transport)?;
            let current = approval_bound_read_digest(&serde_json::to_vec(e)?, &binding);
            anyhow::ensure!(
                current == digest,
                "resource read approval generation changed while acknowledging work"
            );
        }
        self.require_device_approval(&e.sender, &device, transport)?;
        // A read interrupted by a crash or restart has no effect to protect, so it runs
        // again; an interrupted command stays uncertain and is never applied twice.
        let interrupted_read = matches!(disposition, Disposition::Executing)
            && request.method == "GET"
            && request.body.is_none();
        let response = match disposition {
            Disposition::Retired => self.encode_response(e,&device,&json!({"status":410,"body":{"error":"outcome_unknown"}}),&request)?,
            Disposition::Completed(bytes) => bytes,
            Disposition::ExecutingLocal => {
                self.execute_atomic_local(e, &device, &request, &digest, transport, receipt.clone())
                    .await?
            }
            Disposition::Executing if !interrupted_read => {
                let bytes = self.encode_response(e, &device, &json!({"status":409,"error":"uncertain","detail":"execution was interrupted; inspect the Mac before retrying this action"}), &request)?;
                #[cfg(test)]
                self.journal.before_write(ReceiptWrite::Complete, &e.request_id).await;
                let bytes = self.journal.complete_admitted(&e.sender, &e.request_id, bytes, receipt.clone()).await?;
                bytes
            }
            Disposition::Accepted | Disposition::Executing => {
                // Acknowledging can wait on the relay; re-read durable local
                // approval (and enrollment) immediately before dispatch.
                self.require_device_approval(&e.sender, &device, transport)?;
                if !interrupted_read && self.atomic_local(&request) {
                    #[cfg(test)]
                    self.journal.before_write(ReceiptWrite::ExecutingLocal, &e.request_id).await;
                    self.journal
                        .executing_admitted(&e.sender, &e.request_id, &digest, true, receipt.clone()).await?;
                    self.execute_atomic_local(e, &device, &request, &digest, transport, receipt.clone())
                        .await?
                } else {
                    if !interrupted_read {
                        #[cfg(test)]
                        self.journal.before_write(ReceiptWrite::Executing, &e.request_id).await;
                        self.journal.executing_admitted(&e.sender, &e.request_id, &digest, false, receipt.clone()).await?;
                        self.require_device_approval(&e.sender, &device, transport)?;
                    }
                    let result = if guard.saturated {
                        json!({"status":429,"error":"changes_busy"})
                    } else {
                        match self.dispatch(&e.sender, &request, transport).await {
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
                        }
                    };
                    let bytes = self.encode_response(e, &device, &result, &request)?;
                    #[cfg(test)]
                    self.journal.before_write(ReceiptWrite::Complete, &e.request_id).await;
                    let bytes = self.journal.complete_admitted(&e.sender, &e.request_id, bytes, receipt.clone()).await?;
                    bytes
                }
            }
        };
        Ok((response, device, guard))
    }

    /// One envelope that reached the Mac directly over the local network. The answer is the
    /// sealed response, returned with the approval lease the caller holds while handing it over.
    /// A retry of an envelope still executing waits for it and gets the stored response.
    pub(crate) async fn handle_direct(
        &self,
        envelope: Envelope,
    ) -> std::result::Result<(Vec<u8>, crate::approval_lease::ApprovalLease), DirectRefusal> {
        if envelope.kind != "request" || envelope.route != self.mac_id || envelope.recipient != self.mac_id {
            return Err(DirectRefusal::Misdirected);
        }
        let Some(device) = self.devices.get(&envelope.sender) else {
            return Err(self.revoked_refusal(&envelope));
        };
        self.require_device_approval(&envelope.sender, &device, Transport::Direct)
            .map_err(|_| self.revoked_refusal(&envelope))?;
        envelope
            .verify_signature(&device.signing, &envelope.sender, &self.mac_id, &self.mac_id, self.envelope_now())
            .map_err(|_| DirectRefusal::Unauthorized)?;
        let refusal = |error: anyhow::Error| {
            if error.is::<ReusedRequestId>() {
                DirectRefusal::Conflict
            } else {
                DirectRefusal::Rejected(format!("{error:#}"))
            }
        };
        let work = Work { request_id: envelope.request_id.clone(), envelope };
        let prepared = loop {
            let finished = self.inflight.done.notified();
            tokio::pin!(finished);
            finished.as_mut().enable();
            match self.prepare_work(&work, Transport::Direct).map_err(refusal)? {
                Some(prepared) => break prepared,
                None => finished.await,
            }
        };
        let (response, device, _guard) = self
            .execute(&work.envelope, prepared, Transport::Direct, || async { Ok(()) })
            .await
            .map_err(refusal)?;
        let lease = crate::approval_lease::ApprovalLease::shared(&self.home)
            .map_err(|error| DirectRefusal::Rejected(format!("{error:#}")))?;
        self.require_device_approval(&work.envelope.sender, &device, Transport::Direct)
            .map_err(|_| DirectRefusal::Unauthorized)?;
        Ok((response, lease))
    }

    /// A device revoked here that signs its request is told so, sealed to its own keys; anyone
    /// else gets the plain refusal, which proves nothing to the sender.
    fn revoked_refusal(&self, request: &Envelope) -> DirectRefusal {
        let Some(agreement) = self.revoked.proven_sender(request, &self.mac_id, now_ms()) else {
            return DirectRefusal::Unauthorized;
        };
        let answer = json!({"status": 403, "error": "device_revoked", "body": {"error": "device_revoked"}});
        let sealed = serde_json::to_vec(&answer).ok().and_then(|payload| {
            Envelope::seal_random(
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
                &agreement,
            )
            .ok()
        });
        match sealed.and_then(|envelope| serde_json::to_vec(&envelope).ok()) {
            Some(bytes) => DirectRefusal::Revoked(bytes),
            None => DirectRefusal::Unauthorized,
        }
    }

    fn encode_response(
        &self,
        request: &Envelope,
        device: &Device,
        result: &Value,
        local_request: &LocalRequest,
    ) -> Result<Vec<u8>> {
        Self::seal_response(&self.signing, &self.mac_id, self.envelope_now() + 600_000, request, device, result, local_request)
    }

    fn seal_response(
        signing: &SigningKey, mac_id: &str, expires_at_ms: u64,
        request: &Envelope, device: &Device, result: &Value, local_request: &LocalRequest,
    ) -> Result<Vec<u8>> {
        let seal = |value: &Value| -> Result<Vec<u8>> {
            let payload = serde_json::to_vec(value)?;
            let envelope = Envelope::seal_random(
                EnvelopeContext {
                    route: mac_id,
                    sender: mac_id,
                    recipient: &request.sender,
                    request_id: &request.request_id,
                    kind: "response",
                    expires_at_ms,
                },
                &payload,
                signing,
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
        let fallback = seal(
            &json!({"status":502,"error":"response_too_large","detail":response_size_detail(local_request)}),
        )?;
        if fallback.len() > MAX_RELAY_RESPONSE {
            bail!("encrypted fallback response exceeds relay limit");
        }
        Ok(fallback)
    }

    async fn dispatch(
        &self,
        sender: &str,
        request: &LocalRequest,
        transport: Transport,
    ) -> Result<Value> {
        self.dispatch_with(
            sender,
            request,
            transport,
            &crate::prs::GhCli,
            &crate::http::LiveReviewHead,
        )
        .await
    }

    /// This Mac's settings for an approved device, the same answer the local listener gives.
    async fn settings(&self, sender: &str, request: &LocalRequest) -> Result<Option<crate::client_routes::Reply>> {
        let home = self.home.clone();
        let relay = self.has_relay();
        let user_home = crate::options::user_home();
        let work: Box<dyn FnOnce() -> crate::client_routes::Reply + Send> = if request.method == "PUT" && request.path == crate::settings::PATH {
            let body = request.body.clone().context("settings body required")?;
            let lang = self.routes.as_ref().map_or(alondra_core::views::Lang::En, |client| client.language_for_device(sender));
            Box::new(move || crate::settings::write(&home, &user_home, relay, &body, lang))
        } else if request.method == "GET" && request.path == crate::settings::PATH {
            Box::new(move || crate::settings::read(&home, &user_home, relay))
        } else if let Some(path) = crate::settings::folders_query(&request.path).filter(|_| request.method == "GET") {
            Box::new(move || crate::settings::folders(&user_home, path.as_deref()))
        } else {
            return Ok(None);
        };
        Ok(Some(tokio::task::spawn_blocking(work).await?))
    }

    async fn dispatch_domain(
        &self,
        domain: &Arc<crate::domain_runtime::DomainRuntime>,
        sender: &str,
        request: &LocalRequest,
        transport: Transport,
    ) -> Result<Value> {
        approved_method(request)?;
        let device = self.devices.get(sender).context("unapproved device")?;
        let approved = self.approved_device(sender, &device, transport)?;
        if crate::operation_journal::control(&request.method,&request.path,request.body.is_some()) {
            return self.operation_control(sender,request,transport).await;
        }
        if let Some(lookup) = operation_results::lookup(&request.path) {
            let result = self.operation_result(sender, &lookup, transport).await?;
            approved.recheck()?;
            return Ok(result);
        }
        if let Some(reply) = self.settings(sender, request).await? {
            approved.recheck()?;
            return Ok(reply.into_value());
        }
        if request.method == "POST" || request.method == "PUT" {
            if crate::domain_api::is_resource_mutation_path(&request.method, &request.path) {
                let body = request.body.as_ref().context("resource body required")?;
                let result = crate::domain_api::resource_command_runtime(
                    domain,
                    self.routes.as_deref(),
                    sender,
                    &request.method,
                    &request.path,
                    Some(body),
                    None,
                    || approved.recheck(),
                )
                .await?;
                approved.recheck()?;
                return Ok(result);
            }
            if crate::domain_schedule_api::approved_mutation_path(
                &request.method,
                &request.path,
                request.body.is_some(),
            ) {
                let result = crate::domain_schedule_api::mutation(
                    domain,
                    &self.home,
                    &request.method,
                    &request.path,
                    request.body.as_ref(),
                    None,
                    || approved.recheck(),
                );
                approved.recheck()?;
                return Ok(result);
            }
            if crate::client_routes::upload_step(&request.path).is_some() {
                let body = request
                    .body
                    .clone()
                    .context("file transfer body required")?;
                let (transfers, home, owner, route) = (
                    self.transfers.clone(),
                    self.home.clone(),
                    sender.to_owned(),
                    request.path.clone(),
                );
                let result = tokio::task::spawn_blocking(move || {
                    transfers.dispatch_domain(&home, &owner, &route, body)
                })
                .await?;
                approved.recheck()?;
                return Ok(match result {
                    Ok(body) => json!({"status":200,"body":body}),
                    Err(error) => {
                        json!({"status":422,"body":{"error":crate::service_files::error_code(&error)}})
                    }
                });
            }
            let routes = self.routes.as_ref().context("client routes unavailable")?;
            let route = crate::client_routes::parse_write(&request.path)
                .or_else(|| crate::client_routes::parse_put(&request.path))
                .context("domain write is not approved")?;
            let body = request.body.as_ref().context("write body required")?;
            let reply = routes
                .write(&format!("device:{sender}"), route, body, None, &|| {
                    approved.recheck()
                })
                .await?;
            approved.recheck()?;
            return Ok(reply.into_value());
        }
        anyhow::ensure!(
            request.method == "GET" && request.body.is_none(),
            "domain read must be GET"
        );
        let client_read = match &self.routes {
            Some(client) if crate::views::feed::approved_path(&request.path) => {
                Some(crate::views::routes::relay_feed(client, sender, &request.path).await)
            }
            Some(client) if crate::views::routes::approved_read_path(&request.path) => {
                Some(crate::views::routes::relay(client, sender, &request.path).await)
            }
            Some(client) => client.read(&request.path).await,
            None => None,
        };
        let response = match client_read {
            Some(response) => {
                approved.recheck()?;
                response
            }
            None => {
                crate::domain_api::encrypted_read_runtime(
                    domain,
                    &self.home,
                    &approved,
                    &request.path,
                )
                .await?
            }
        };
        let status = response.status().as_u16();
        let bytes = axum::body::to_bytes(response.into_body(), MAX_LOCAL_RESPONSE).await?;
        approved.recheck()?;
        let mut body: Value = serde_json::from_slice(&bytes)?;
        if request.path == "/capabilities" && status == 200 { body["operation_results"] = json!(true);body["operation_journal"] = json!(true); }
        Ok(json!({"status":status,"body":body}))
    }

    async fn dispatch_with(
        &self,
        sender: &str,
        request: &LocalRequest,
        transport: Transport,
        gh: &impl crate::prs::Gh,
        reader: &impl crate::http::ReviewHead,
    ) -> Result<Value> {
        if request.path == crate::direct::routes::PATH {
            anyhow::ensure!(request.method == "GET" && request.body.is_none(), "routes is a read");
            let device = self.devices.get(sender).context("unapproved device")?;
            self.require_device_approval(sender, &device, transport)?;
            if !self.direct.enabled {
                return Ok(json!({"status":404,"body":{"error":"direct_off"}}));
            }
            let routes = crate::direct::routes::current(self.direct.port, self.direct.listen).await;
            self.require_device_approval(sender, &device, transport)?;
            return Ok(json!({"status":200,"body":routes}));
        }
        if let Some(domain) = &self.domain {
            return self.dispatch_domain(domain, sender, request, transport).await;
        }
        if request.path == "/schedules" || request.path.starts_with("/schedules/") {
            approved_method(request)?;
            let device = self.devices.get(sender).context("unapproved device")?;
            self.require_device_approval(sender, &device, transport)?;
            return Ok(json!({"status":409,"body":{
                "code":"incompatible_protocol",
                "message":"Schedules require the domain store's authority",
                "current_revision":null,"subject":null
            }}));
        }
        if matches!(
            request.path.split('?').next(),
            Some("/v1/changes" | "/changes")
        ) && approved_method(request).is_err()
        {
            return Ok(json!({"status":400,"error":"invalid_changes_query"}));
        }
        if request.method == "GET"
            && (crate::domain_api::approved_resource_read_path(&request.path)
                || crate::client_routes::approved_read_path(&request.path))
            && request.body.is_none()
        {
            approved_method(request)?;
            let hub = self.hub.clone().context("domain read requires local hub")?;
            let device = self.devices.get(sender).context("unapproved device")?;
            let approved = self.approved_device(sender, &device, transport)?;
            let response = crate::domain_api::encrypted_read(hub, &approved, &request.path).await?;
            let status = response.status().as_u16();
            let bytes = axum::body::to_bytes(response.into_body(), MAX_LOCAL_RESPONSE).await?;
            approved.recheck()?;
            let body: Value = serde_json::from_slice(&bytes)?;
            return Ok(json!({"status":status,"body":body}));
        }
        if request.method == "POST" && request.path == "/v1/service/push-binding" {
            let (binding, quiet, revision) = parse_push_preferences(request)?;
            match self
                .journal
                .set_versioned_push_preferences(sender, &binding, quiet, revision)?
            {
                Some(current) => {
                    return Ok(
                        json!({"status":409,"error":"stale_push_preferences","current_revision":current}),
                    );
                }
                None => return Ok(json!({"status":204})),
            }
        }
        if matches!(
            request.path.as_str(),
            "/v1/service/uploads/start"
                | "/v1/service/uploads/chunk"
                | "/v1/service/uploads/finish"
                | "/v1/service/uploads/cancel"
                | "/v1/service/files/chunk"
        ) {
            anyhow::ensure!(request.method == "POST", "file transfers require POST");
            let body = request
                .body
                .clone()
                .context("file transfer body required")?;
            let transfers = self.transfers.clone();
            let home = self.home.clone();
            let sender = sender.to_owned();
            let path = request.path.clone();
            let result = tokio::task::spawn_blocking(move || {
                transfers.dispatch(&home, &sender, &path, body)
            })
            .await?;
            return Ok(match result {
                Ok(body) => json!({"status":200,"body":body}),
                Err(error) => {
                    json!({"status":422,"body":{"error":crate::service_files::error_code(&error)}})
                }
            });
        }
        let method = approved_method(request)?;
        if method == Method::POST && crate::http::prepared_service_path(&request.path) {
            return self.dispatch_prepared(sender, request, transport, gh, reader).await;
        }
        let url = format!("http://127.0.0.1:{}{}", self.local_port, request.path);
        let mut builder = self.client.request(method, url);
        if let Some(body) = &request.body {
            builder = builder.json(body);
        }
        let response = builder.send().await?;
        let status = response.status().as_u16();
        let diff_snapshot =
            request.method == "GET" && id_action(&request.path, "/v1/prs/", "/diff-snapshot", true);
        if diff_snapshot && status == 200 {
            // The local diff may reach 2 MiB; cut it to the relay budget and flag it instead of failing.
            let bytes = bounded_response_within(response, MAX_LOCAL_DIFF_SNAPSHOT).await?;
            let mut body: Value = serde_json::from_slice(&bytes)?;
            crate::domain_api::bound_diff_json(&mut body);
            return Ok(json!({"status":status,"body":body}));
        }
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

    async fn dispatch_prepared(
        &self,
        sender: &str,
        request: &LocalRequest,
        transport: Transport,
        gh: &impl crate::prs::Gh,
        reader: &impl crate::http::ReviewHead,
    ) -> Result<Value> {
        let hub = self
            .hub
            .as_ref()
            .context("prepared operation requires local hub")?;
        let device = self.devices.get(sender).context("unapproved device")?;
        let response = crate::http::prepared_service_request_with(
            hub.clone(),
            &request.path,
            request
                .body
                .clone()
                .context("prepared operation body required")?,
            gh,
            reader,
            || self.require_device_approval(sender, &device, transport),
        )
        .await;
        let status = response.status().as_u16();
        let bytes = axum::body::to_bytes(response.into_body(), MAX_LOCAL_RESPONSE).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        Ok(json!({"status":status,"body":body}))
    }

    /// Send one opaque notification per enrolled phone. The relay can see only
    /// routing metadata, a random binding, and the generic APNs alert.
    pub async fn broadcast_push(&self, payload: &Value, with_sound: bool) -> Result<()> {
        let _ = self.broadcast_push_report(payload, with_sound).await?;
        Ok(())
    }

    /// Count relay acknowledgements for durable domain outbox handoff.
    pub async fn broadcast_push_report(
        &self,
        payload: &Value,
        with_sound: bool,
    ) -> Result<(usize, usize)> {
        self.broadcast_push_report_for_device(with_sound, |_| payload.clone()).await
    }

    pub async fn broadcast_push_report_for_device(
        &self,
        with_sound: bool,
        payload_for_device: impl Fn(&str) -> Value,
    ) -> Result<(usize, usize)> {
        self.broadcast_to_devices(PushDelivery::Alert, with_sound, payload_for_device).await
    }

    /// A silent wake of the app on every enrolled phone; it shows nothing by itself.
    pub async fn broadcast_background_push(
        &self,
        payload_for_device: impl Fn(&str) -> Value,
    ) -> Result<(usize, usize)> {
        self.broadcast_to_devices(PushDelivery::Background, false, payload_for_device).await
    }

    /// Sets the icon badge on every enrolled phone, even where the app was force-quit; it shows
    /// nothing. The relay and APNs read the count in the clear.
    pub async fn broadcast_badge_push(&self, badge: u32) -> Result<(usize, usize)> {
        let payload = json!({"aps": {"badge": badge}, "alondra": {"kind": "badge"}});
        self.broadcast_to_devices(PushDelivery::Badge, false, |_| payload.clone()).await
    }

    async fn broadcast_to_devices(
        &self,
        delivery: PushDelivery,
        with_sound: bool,
        payload_for_device: impl Fn(&str) -> Value,
    ) -> Result<(usize, usize)> {
        if self.relay.is_none() {
            return Ok((0, 0));
        }
        let mut epoch = None;
        let mut attempted = 0;
        let mut accepted = 0;
        for (id, device) in &self.devices.snapshot() {
            if !self.auth.approved_key_matches(id, &device.signing)
                || !self.journal.enrollment_active(id)?
            {
                continue;
            }
            let Some(binding) = self.journal.push_binding(id)? else {
                continue;
            };
            attempted += 1;
            let payload = payload_for_device(id);
            if let Err(error) = self
                .send_device_push(id, device, &binding, &payload, with_sound, delivery, &mut epoch)
                .await
            {
                log!("service push for {id} failed: {error:#}");
            } else {
                accepted += 1;
            }
        }
        Ok((attempted, accepted))
    }

    /// An explicit local diagnostic: no Hub, task, report or legacy APNs sender.
    pub async fn test_push(&self, id: &str) -> Result<()> {
        let device = self
            .devices
            .get(id)
            .context("device is not configured for the service")?;
        self.require_device_approval(id, &device, Transport::Relay)?;
        let binding = self
            .journal
            .push_binding(id)?
            .context("device has no active push binding")?;
        let payload = json!({"aps":{"alert":{
            "title":"Prueba cifrada de Alondra",
            "body":"Diagnóstico enviado desde tu Mac. Si lees este texto, tu dispositivo ha descifrado la notificación."
        }}, "alondra": {}});
        self.send_device_push(id, &device, &binding, &payload, true, PushDelivery::Alert, &mut None)
            .await
    }

    fn approval_binding(&self, id: &str, device: &Device, transport: Transport) -> Result<String> {
        self.require_device_approval(id, device, transport)?;
        let persisted = Auth::open(&self.home)?;
        let approval = persisted
            .list()
            .into_iter()
            .find(|d| d.id == id)
            .context("device approval missing")?;
        Ok(format!(
            "{}:{}:{}",
            approval.created_at,
            device.signing_wire,
            URL_SAFE_NO_PAD.encode(device.agreement.to_sec1_bytes())
        ))
    }

    fn require_device_approval(&self, id: &str, device: &Device, transport: Transport) -> Result<()> {
        Self::require_approval(&self.auth, &self.home, &self.journal, id, device, transport)
    }

    fn require_approval(auth: &Auth, home: &Path, journal: &Journal, id: &str, device: &Device, transport: Transport) -> Result<()> {
        // The diagnostic is a separate process: re-read persisted approval so
        // revocation by the running daemon is observed after bootstrap too.
        if !auth.approved_key_matches(id, &device.signing)
            || !Auth::open(home)?.approved_key_matches(id, &device.signing)
        {
            bail!("device is not locally approved");
        }
        if transport == Transport::Relay && !journal.enrollment_active(id)? {
            bail!("device is not actively enrolled with the relay");
        }
        Ok(())
    }
    fn approved_device<'a>(
        &'a self,
        id: &'a str,
        device: &'a Device,
        transport: Transport,
    ) -> Result<ApprovedDevice<'a>> {
        self.require_device_approval(id, device, transport)?;
        Ok(ApprovedDevice {
            service: self,
            sender: id,
            device,
            transport,
        })
    }

    /// Content that does not fit Apple's encrypted budget is never dropped: the
    /// device gets a generic alert with the same item reference and fetches the
    /// content when the notification is opened or acted on.
    fn encode_push_or_minimal(
        &self,
        id: &str,
        device: &Device,
        binding: &str,
        payload: &Value,
        with_sound: bool,
        delivery: PushDelivery,
    ) -> Result<Vec<u8>> {
        match self.encode_push(id, device, binding, payload, with_sound, delivery) {
            Err(error) if error.to_string().contains("APNs budget") => {
                self.encode_push(id, device, binding, &minimal_push(payload), with_sound, delivery)
            }
            result => result,
        }
    }

    fn encode_push(
        &self,
        id: &str,
        device: &Device,
        binding: &str,
        payload: &Value,
        with_sound: bool,
        delivery: PushDelivery,
    ) -> Result<Vec<u8>> {
        seal_push(&self.mac_id, &self.signing, id, &device.agreement, binding, payload, with_sound, delivery)
    }
}

/// How the relay asks APNs to hand a push over: an alert the notification service extension
/// opens, a silent background wake of the app, or only a new icon badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushDelivery {
    Alert,
    Background,
    Badge,
}

/// The `apns-collapse-id` of a card's alert: pushing the same card again replaces its
/// notification instead of adding one. A digest, because APNs and the relay read it in the
/// clear and card ids can carry task names; the binding keeps one phone's ids unlinkable to
/// another's.
pub(crate) fn collapse_id(binding: &str, card: &str) -> String {
    let digest = Sha256::new()
        .chain_update(b"alondra-collapse-v1\0")
        .chain_update(binding.as_bytes())
        .chain_update([0])
        .chain_update(card.as_bytes())
        .finalize();
    URL_SAFE_NO_PAD.encode(&digest[..16])
}

/// A relay older than collapse ids and background pushes rejects unknown fields: the same
/// request without them, or None when there was nothing to drop.
fn without_relay_extensions(body: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut value: Value = serde_json::from_slice(body)?;
    let Some(fields) = value.as_object_mut() else { return Ok(None) };
    let dropped = fields.remove("collapse_id").is_some()
        | fields.remove("push_type").is_some()
        | fields.remove("badge").is_some();
    Ok(if dropped { Some(serde_json::to_vec(&value)?) } else { None })
}

/// The `/v1/mac/push` body: the relay forwards `envelope` to APNs and the phone's
/// notification service extension opens it with the pairing's agreement key.
pub(crate) fn seal_push(
    mac_id: &str,
    signing: &SigningKey,
    device_id: &str,
    agreement: &PublicKey,
    binding: &str,
    payload: &Value,
    with_sound: bool,
    delivery: PushDelivery,
) -> Result<Vec<u8>> {
    let plaintext = serde_json::to_vec(&json!({
        "push_binding": binding,
        "payload": payload,
        "with_sound": with_sound,
    }))?;
    if plaintext.len() > MAX_PUSH_PLAINTEXT {
        bail!("content exceeds encrypted APNs budget");
    }
    let mut id_bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut id_bytes)
        .map_err(|_| anyhow!("OS randomness failed"))?;
    let request_id = URL_SAFE_NO_PAD.encode(id_bytes);
    let envelope = Envelope::seal_random(
        EnvelopeContext {
            route: mac_id,
            sender: mac_id,
            recipient: device_id,
            request_id: &request_id,
            kind: "push",
            expires_at_ms: now_ms() + 600_000,
        },
        &plaintext,
        signing,
        agreement,
    )
    .map_err(|error| anyhow!("seal push: {error}"))?;
    let mut body = json!({"device_id":device_id,"envelope":envelope});
    match delivery {
        PushDelivery::Alert => {
            if let Some(card) = payload.pointer("/alondra/card").and_then(Value::as_str) {
                body["collapse_id"] = json!(collapse_id(binding, card));
            }
        }
        PushDelivery::Background => body["push_type"] = json!("background"),
        PushDelivery::Badge => {
            let badge = payload.pointer("/aps/badge").and_then(Value::as_u64).context("a badge push needs aps.badge")?;
            body["push_type"] = json!("badge");
            body["badge"] = json!(badge);
        }
    }
    let body = serde_json::to_vec(&body)?;
    if body.len() > MAX_PUSH_BODY {
        bail!("envelope exceeds APNs budget");
    }
    Ok(body)
}

/// The generic alert that stands in for content too large to push, keeping the
/// `alondra` reference (kind, id, cursor) the app needs to fetch it.
fn minimal_push(payload: &Value) -> Value {
    let mut aps = json!({"alert": {"title": "Alondra"}});
    for key in ["sound", "category", "interruption-level", "thread-id"] {
        if let Some(value) = payload.pointer(&format!("/aps/{key}")) {
            aps[key] = value.clone();
        }
    }
    json!({"aps": aps, "alondra": payload.get("alondra").cloned().unwrap_or(json!({}))})
}

async fn bounded_response(response: reqwest::Response) -> Result<Vec<u8>> {
    bounded_response_within(response, MAX_LOCAL_RESPONSE).await
}

async fn bounded_response_within(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > limit {
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

fn relay_error_code(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value["error"]
                .as_str()
                .filter(|code| code.len() <= 64 && id(code))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".into())
}

/// The relay confirmed it ended the phone's registration: the phone signed out, its session
/// or pairing ended, or APNs said the token is dead. A plain "not found" can be a relay that
/// lost its registry for a while, so it ends nothing.
fn registration_ended(status: reqwest::StatusCode, code: &str) -> bool {
    status == reqwest::StatusCode::GONE && code == "push_registration_gone"
}

fn id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
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

fn domain_read_route(request: &LocalRequest) -> bool {
    request.method == "GET"
        && (crate::operation_journal::query_epoch(&request.path).is_some()
            || operation_results::lookup(&request.path).is_some()
            || crate::domain_api::approved_resource_read_path(&request.path)
            || crate::client_routes::approved_read_path(&request.path)
            || crate::views::routes::approved_read_path(&request.path))
}

/// The agent a relayed `POST /agents/{id}/messages` writes to.
fn agent_message(path: &str) -> Option<alondra_core::domain::AgentId> {
    path.strip_prefix("/agents/")?
        .strip_suffix("/messages")
        .filter(|id| domain_read_id(id))
        .map(Into::into)
}

fn domain_read_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// The relay carries opaque JSON. Only these app operations may be replayed on loopback.
/// There is deliberately no general URL proxy, no pairing/device management, and no upload.
fn approved_method(request: &LocalRequest) -> Result<Method> {
    if crate::operation_journal::control(&request.method,&request.path,request.body.is_some()) {
        return Ok(Method::from_bytes(request.method.as_bytes())?);
    }
    if request.method == "GET" && request.body.is_none() && operation_results::lookup(&request.path).is_some() {
        return Ok(Method::GET);
    }
    if request.method == "GET" && request.body.is_none() && crate::settings::approved_read(&request.path) {
        return Ok(Method::GET);
    }
    if request.method == "PUT" && request.body.is_some() && request.path == crate::settings::PATH {
        return Ok(Method::PUT);
    }
    if request.method == "GET" && request.body.is_none() && request.path == crate::direct::routes::PATH {
        return Ok(Method::GET);
    }
    if request.method == "GET"
        && request.body.is_none()
        && crate::views::feed::approved_path(&request.path)
    {
        return Ok(Method::GET);
    }
    if request.method == "GET"
        && request.body.is_none()
        && crate::domain_api::approved_resource_read_path(&request.path)
    {
        return Ok(Method::GET);
    }
    if crate::domain_schedule_api::approved_mutation_path(
        &request.method,
        &request.path,
        request.body.is_some(),
    ) {
        return Ok(Method::from_bytes(request.method.as_bytes())?);
    }
    if request.method == "GET"
        && request.body.is_none()
        && crate::client_routes::approved_read_path(&request.path)
    {
        return Ok(Method::GET);
    }
    if request.method == "GET"
        && request.body.is_none()
        && crate::views::routes::approved_read_path(&request.path)
    {
        return Ok(Method::GET);
    }
    if crate::domain_api::is_resource_mutation_path(&request.method, &request.path)
        && request.body.is_some()
    {
        return Ok(Method::from_bytes(request.method.as_bytes())?);
    }
    if crate::views::actions::approved_path(&request.method, &request.path)
        && request.body.is_some()
    {
        return Ok(Method::POST);
    }
    if request.method == "POST"
        && request.body.is_some()
        && (request.path == PUSH_BINDING_ROUTE
            || request.path == PUSH_BINDING_REMOVE_ROUTE
            || crate::client_routes::upload_step(&request.path).is_some()
            || agent_message(&request.path).is_some()
            || crate::domain_orchestrator::decision_route(&request.path).is_some()
            || crate::domain_api::resolve_delivery_attempt_id(&request.path).is_some())
    {
        return Ok(Method::POST);
    }
    if request.method == "POST"
        && request.body.is_some()
        && crate::client_routes::approved_write_path(&request.path)
    {
        return Ok(Method::POST);
    }
    if request.method == "PUT"
        && request.body.is_some()
        && crate::client_routes::parse_put(&request.path).is_some()
    {
        return Ok(Method::from_bytes(request.method.as_bytes())?);
    }

    let raw = request.path.as_str();
    if raw.len() > 4096 || raw.contains(['#', '\\']) {
        bail!("local operation is not approved for service access");
    }
    let (path, query) = raw
        .split_once('?')
        .map_or((raw, None), |(p, q)| (p, Some(q)));
    if !path.starts_with("/v1/")
        || path.contains('%')
        || path.contains("//")
        || path.contains("/./")
        || path.contains("/../")
    {
        bail!("local operation is not approved for service access");
    }
    let query_ok = match (path, query) {
        ("/v1/changes", Some(q)) => changes_query(q),
        (_, None) => true,
        ("/v1/options", Some(q)) => single_query(q, "project", 2048),
        ("/v1/issues", Some(q)) => single_query_value(q, "refresh", &["1"]),
        ("/v1/search", Some(q)) => single_query(q, "q", 2048),
        ("/v1/file", Some(q)) => single_query(q, "path", 2048),
        (p, Some(q)) if one_id_path(p, "/v1/nodes/").is_some() => {
            single_query_value(q, "limit", &["1"])
        }
        _ => false,
    };
    if !query_ok {
        bail!("local operation is not approved for service access");
    }
    let allowed = match request.method.as_str() {
        "GET" if request.body.is_none() => {
            matches!(
                path,
                "/v1/changes"
                    | "/v1/delivery-attempts"
                    | "/v1/schedules"
                    | "/v1/options"
                    | "/v1/issues"
                    | "/v1/projects"
                    | "/v1/search"
                    | "/v1/file"
            ) || one_id_path(path, "/v1/tasks/").is_some()
                || one_id_path(path, "/v1/nodes/").is_some()
                || id_action(path, "/v1/nodes/", "/activity", false)
                || id_action(path, "/v1/prs/", "/diff", true)
                || id_action(path, "/v1/prs/", "/diff-snapshot", true)
        }
        "POST" if request.body.is_some() && query.is_none() => {
            matches!(
                path,
                "/v1/orchestrator/acknowledge"
                    | "/v1/orchestrator/cancel"
                    | "/v1/orchestrator/retry"
                    | "/v1/requests"
                    | "/v1/tasks/archive"
                    | "/v1/activity"
                    | "/v1/schedules"
            ) || id_action(path, "/v1/questions/", "/answer", true)
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
                || id_action(path, "/v1/delivery-attempts/", "/retry", true)
                || id_action(path, "/v1/delivery-attempts/", "/discard", true)
                || id_action(path, "/v1/nodes/", "/close", false)
                || id_action(path, "/v1/nodes/", "/messages", false)
                || id_action(path, "/v1/schedules/", "/run", false)
        }
        "PUT" if request.body.is_some() && query.is_none() => {
            one_id_path(path, "/v1/schedules/").is_some()
        }
        "DELETE" if request.body.is_none() && query.is_none() => {
            one_id_path(path, "/v1/schedules/").is_some()
        }
        _ => false,
    };
    if !allowed {
        bail!("local operation is not approved for service access");
    }
    Ok(Method::from_bytes(request.method.as_bytes())?)
}

/// Raw decimal parameters only: no duplicate, unknown or percent-encoded
/// spellings that could be interpreted differently by the loopback handler.
fn changes_query(query: &str) -> bool {
    let mut after = false;
    let mut wait = false;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            return false;
        };
        if !number(value) {
            return false;
        }
        match key {
            "after" if !after => after = true,
            "wait" if !wait && value.parse::<u64>().is_ok_and(|seconds| seconds <= 50) => {
                wait = true
            }
            _ => return false,
        }
    }
    true
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
    Retired,
    Accepted,
    Executing,
    ExecutingLocal,
    Completed(Vec<u8>),
}
enum EnrollmentDecision {
    New,
    Pending,
    Active,
    Revoked,
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiptWrite { Executing, ExecutingLocal, Complete, PushPreferences }
#[cfg(test)]
type ReceiptWriteHook = dyn Fn(ReceiptWrite, &str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync;

struct ReceiptAdmission {
    _permit: tokio::sync::OwnedSemaphorePermit,
    reservation: Arc<WorkGuard>,
}

#[derive(Clone)]
struct Journal {
    path: PathBuf,
    receipt_admissions: Arc<tokio::sync::Semaphore>,
    accept_admissions: Arc<tokio::sync::Semaphore>,
    accept_jobs: Arc<tokio::sync::Semaphore>,
    #[cfg(test)]
    before_write: Option<Arc<ReceiptWriteHook>>,
    #[cfg(test)]
    push_sql_entry: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    _fixture_directory: Option<Arc<crate::test_support::Directory>>,
}
impl Journal {
    fn reserve_receipt(&self, reservation: Arc<WorkGuard>) -> Result<Arc<ReceiptAdmission>> {
        let permit = self.receipt_admissions.clone().try_acquire_owned()
            .context("receipt journal lifecycle admission full before acknowledgement")?;
        Ok(Arc::new(ReceiptAdmission { _permit: permit, reservation }))
    }

    async fn write_receipt<T: Send + 'static>(
        &self,
        receipt: Arc<ReceiptAdmission>,
        operation: impl FnOnce(&Journal) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let physical = self.accept_jobs.clone().acquire_owned().await?;
        let journal = self.clone();
        tokio::task::spawn_blocking(move || {
            let (_physical, _receipt) = (physical, receipt);
            operation(&journal)
        }).await?
    }

    async fn executing_admitted(
        &self, sender: &str, id: &str, digest: &[u8; 32], local: bool, receipt: Arc<ReceiptAdmission>,
    ) -> Result<()> {
        let (sender, id, digest) = (sender.to_owned(), id.to_owned(), *digest);
        self.write_receipt(receipt, move |journal| {
            if local { journal.executing_local(&sender, &id, &digest) }
            else { journal.executing(&sender, &id, &digest) }
        }).await
    }

    async fn complete_admitted(
        &self, sender: &str, id: &str, response: Vec<u8>, receipt: Arc<ReceiptAdmission>,
    ) -> Result<Vec<u8>> {
        let (sender, id) = (sender.to_owned(), id.to_owned());
        self.write_receipt(receipt, move |journal| {
            journal.complete(&sender, &id, &response)?;
            Ok(response)
        }).await
    }

    #[cfg(test)]
    async fn before_write(&self, stage: ReceiptWrite, id: &str) {
        if let Some(hook) = &self.before_write { hook(stage, id).await }
    }

    async fn off_runtime_admitted<T: Send + 'static>(
        &self,
        reservation: Arc<WorkGuard>,
        operation: impl FnOnce(&Journal) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let admission = self.accept_admissions.clone().try_acquire_owned()
            .context("receipt journal admission full before acknowledgement")?;
        let physical = self.accept_jobs.clone().acquire_owned().await?;
        let journal = self.clone();
        tokio::task::spawn_blocking(move || {
            let (_admission, _physical, _reservation) = (admission, physical, reservation);
            operation(&journal)
        }).await?
    }

    async fn accept_admitted(
        &self, sender: &str, id: &str, digest: &[u8; 32], recovery: Option<operation_results::RecoveryScope>, read_only:bool, legacy_scope:Option<crate::operation_journal::Scope>, receipt: Arc<ReceiptAdmission>,
    ) -> Result<Disposition> {
        let (sender, id, digest) = (sender.to_owned(), id.to_owned(), *digest);
        self.off_runtime_admitted(receipt.reservation.clone(), move |journal| {
            let _receipt = receipt;
            journal.accept_classified(&sender, &id, &digest, recovery.as_ref(),read_only,legacy_scope.as_ref())
        }).await
    }

    /// Exclusive approval leases wait for shared leases that other tasks hold
    /// across relay awaits, so they must never block a runtime worker.
    async fn off_runtime<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Journal) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let journal = self.clone();
        tokio::task::spawn_blocking(move || operation(&journal)).await?
    }
    fn open(path: PathBuf) -> Result<Self> {
        crate::operation_journal::guard(&path)?;
        // Resolve only the parent (macOS temporary roots may themselves be
        // aliases). Never canonicalize the final filename: that would erase
        // evidence of a symlink before SQLite can enforce NOFOLLOW.
        let path = path
            .parent()
            .context("journal parent missing")?
            .canonicalize()?
            .join(path.file_name().context("journal filename missing")?);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            // Tighten the DB before SQLite creates WAL/SHM sidecars.
            let file = std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
                .open(&path)?;
            anyhow::ensure!(
                file.metadata()?.is_file(),
                "service journal must be a regular file"
            );
            // Change only the opened inode, never follow a swapped path for chmod.
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        let mut c = Self::connect(&path)?;
        c.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS service_receipts (
              sender TEXT NOT NULL, request_id TEXT NOT NULL, digest BLOB NOT NULL,
              state TEXT NOT NULL, response BLOB, execution_mode TEXT NOT NULL DEFAULT 'legacy', PRIMARY KEY(sender, request_id));
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
        let migration = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let has_mode = {
            let mut statement = migration.prepare("PRAGMA table_info(service_receipts)")?;
            let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
            columns
                .collect::<rusqlite::Result<Vec<_>>>()?
                .iter()
                .any(|name| name == "execution_mode")
        };
        if !has_mode {
            migration.execute_batch("ALTER TABLE service_receipts ADD COLUMN execution_mode TEXT NOT NULL DEFAULT 'legacy'")?;
        }
        for name in ["recovery_digest", "recovery_binding"] {
            let exists = {
                let mut statement = migration.prepare("PRAGMA table_info(service_receipts)")?;
                let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?.iter().any(|column| column == name)
            };
            if !exists { migration.execute_batch(&format!("ALTER TABLE service_receipts ADD COLUMN {name} BLOB"))?; }
        }
        migration.commit()?;
        crate::operation_journal::initialize(&path,&mut c,crate::operation_journal::Backend::Encrypted)?;
        Ok(Self { path,
            receipt_admissions: Arc::new(tokio::sync::Semaphore::new(MAX_JOURNAL_RECEIPTS)),
            accept_admissions: Arc::new(tokio::sync::Semaphore::new(MAX_JOURNAL_ACCEPTS)),
            accept_jobs: Arc::new(tokio::sync::Semaphore::new(1)),
            #[cfg(test)]
            before_write: None,
            #[cfg(test)]
            push_sql_entry: None,
            #[cfg(test)]
            _fixture_directory: None,
        })
    }
    fn connect(path: &Path) -> Result<Connection> {
        // Enforce no-follow in SQLite itself as well, closing the race between
        // file preparation and sqlite3_open. Reopens never create a missing DB.
        Ok(Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?)
    }
    fn db(&self) -> Result<Connection> {
        let c = Self::connect(&self.path)?;
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
        let _lease = crate::approval_lease::ApprovalLease::exclusive(
            self.path.parent().context("journal home missing")?,
        )?;
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
        let _lease = crate::approval_lease::ApprovalLease::exclusive(
            self.path.parent().context("journal home missing")?,
        )?;
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
        let _lease = crate::approval_lease::ApprovalLease::exclusive(
            self.path.parent().context("journal home missing")?,
        )?;
        let mut c = self.db()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE service_enrollments SET state='revoked' WHERE device_id=?1",
            params![id],
        )?;
        tx.execute(
            "DELETE FROM service_push_bindings WHERE device_id=?1",
            params![id],
        )?;
        tx.execute(
            "DELETE FROM service_push_preferences WHERE device_id=?1",
            params![id],
        )?;
        tx.execute(
            "DELETE FROM service_push_preference_revisions WHERE device_id=?1",
            params![id],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// The phone, or the relay on its behalf, said this binding gets no more pushes. One the
    /// phone registered since, after pairing again, stays.
    fn forget_push_binding(&self, id: &str, binding: &str) -> Result<()> {
        self.db()?.execute(
            "DELETE FROM service_push_bindings WHERE device_id=?1 AND binding=?2",
            params![id, binding],
        )?;
        Ok(())
    }
    #[cfg(test)]
    fn set_push_binding(&self, id: &str, binding: &str) -> Result<()> {
        self.set_push_preferences(id, binding, None)
    }
    #[cfg(test)]
    fn set_push_preferences(
        &self,
        id: &str,
        binding: &str,
        quiet: Option<QuietHours>,
    ) -> Result<()> {
        if self
            .set_versioned_push_preferences(id, binding, quiet, None)?
            .is_some()
        {
            bail!("stale push preferences");
        }
        Ok(())
    }
    /// Returns the current revision on conflict. Checks and writes share one
    /// immediate transaction so delivery order and process restarts cannot roll back state.
    fn set_versioned_push_preferences(
        &self,
        id: &str,
        binding: &str,
        quiet: Option<QuietHours>,
        revision: Option<i64>,
    ) -> Result<Option<i64>> {
        let mut c = self.db()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let result = Self::write_push_preferences(&tx, id, binding, quiet, revision)?;
        tx.commit()?;
        Ok(result)
    }

    fn write_push_preferences(
        tx: &rusqlite::Transaction<'_>,
        id: &str,
        binding: &str,
        quiet: Option<QuietHours>,
        revision: Option<i64>,
    ) -> Result<Option<i64>> {
        if revision.is_some_and(|v| !(1..=9_007_199_254_740_991).contains(&v))
            || (revision.is_some() && quiet.is_none())
        {
            bail!("invalid versioned push preferences");
        }
        if !valid_push_binding(binding) {
            bail!("invalid push binding");
        }
        let current: Option<i64> = tx
            .query_row(
                "SELECT revision FROM service_push_preference_revisions WHERE device_id=?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(current) = current {
            if revision.is_none_or(|revision| revision < current) {
                return Ok(Some(current));
            }
            if revision == Some(current) {
                let saved: (String, Option<String>, Option<String>) = tx.query_row(
                    "SELECT b.binding,p.quiet_start,p.quiet_end FROM service_push_bindings b JOIN service_push_preferences p USING(device_id) JOIN service_enrollments e USING(device_id) WHERE b.device_id=?1 AND e.state='active'",
                    params![id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
                return Ok(
                    if quiet.as_ref().is_some_and(|quiet| {
                        saved.0 == binding && saved.1 == quiet.start && saved.2 == quiet.end
                    }) {
                        None
                    } else {
                        Some(current)
                    },
                );
            }
        }
        let updated = tx.execute(
            "INSERT INTO service_push_bindings(device_id,binding) SELECT device_id,?2 FROM service_enrollments WHERE device_id=?1 AND state='active' ON CONFLICT(device_id) DO UPDATE SET binding=excluded.binding",
            params![id, binding],
        )?;
        if updated != 1 {
            bail!("push device is not enrolled");
        }
        if let Some(quiet) = quiet {
            tx.execute("INSERT INTO service_push_preferences(device_id,quiet_start,quiet_end) VALUES(?1,?2,?3) ON CONFLICT(device_id) DO UPDATE SET quiet_start=excluded.quiet_start,quiet_end=excluded.quiet_end", params![id, quiet.start, quiet.end])?;
        }
        if let Some(revision) = revision {
            tx.execute("INSERT INTO service_push_preference_revisions(device_id,revision) VALUES(?1,?2) ON CONFLICT(device_id) DO UPDATE SET revision=excluded.revision", params![id, revision])?;
        }
        Ok(None)
    }

    fn complete_push_preferences(
        &self,
        sender: &str,
        id: &str,
        digest: &[u8; 32],
        binding: &str,
        quiet: Option<QuietHours>,
        revision: Option<i64>,
        encode: impl FnOnce(Value) -> Result<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        let mut c = self.db()?;
        #[cfg(test)]
        if let Some(enter) = &self.push_sql_entry { enter(); }
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (stored_digest, state, response, mode): (Vec<u8>, String, Option<Vec<u8>>, String) = tx.query_row(
            "SELECT digest,state,response,execution_mode FROM service_receipts WHERE sender=?1 AND request_id=?2",
            params![sender,id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        anyhow::ensure!(
            stored_digest == digest,
            "request ID reused with different encrypted bytes"
        );
        if state == "completed" {
            return response.context("completed receipt missing response");
        }
        anyhow::ensure!(
            state == "executing" && mode == "local",
            "operation is not an atomic local execution"
        );
        let result = match Self::write_push_preferences(&tx, sender, binding, quiet, revision)? {
            Some(current) => {
                json!({"status":409,"error":"stale_push_preferences","current_revision":current})
            }
            None => json!({"status":204}),
        };
        let bytes = encode(result)?;
        tx.execute("UPDATE service_receipts SET state='completed',response=?3 WHERE sender=?1 AND request_id=?2",
            params![sender,id,&bytes])?;
        self.trim_replies(&tx,sender)?;
        tx.commit()?;
        Ok(bytes)
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
    #[cfg(test)]
    fn accept(&self, sender: &str, id: &str, digest: &[u8; 32]) -> Result<Disposition> {
        self.accept_scoped(sender, id, digest, None)
    }
    #[cfg(test)]
    fn accept_scoped(&self, sender: &str, id: &str, digest: &[u8; 32], recovery: Option<&operation_results::RecoveryScope>) -> Result<Disposition> {
        self.accept_classified(sender,id,digest,recovery,false,None)
    }
    fn accept_classified(&self, sender: &str, id: &str, digest: &[u8; 32], recovery: Option<&operation_results::RecoveryScope>,read_only:bool,legacy_scope:Option<&crate::operation_journal::Scope>) -> Result<Disposition> {
        let mut c = self.db()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let identity = match recovery.and_then(|recovery|recovery.operation.as_ref()) {
            Some(scope)=>crate::operation_journal::admit(&tx,scope,id,crate::operation_journal::Backend::Encrypted)?,
            None=>None,
        };
        let previous: Option<(Vec<u8>, String, Option<Vec<u8>>, String)> = tx.query_row(
            "SELECT digest,state,response,execution_mode FROM service_receipts WHERE sender=?1 AND request_id=?2",
            params![sender,id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        if previous.as_ref().is_some_and(|(prior, state, _, _)| prior == digest && state == "interrupted") {
            tx.execute("UPDATE service_receipts SET state='executing' WHERE sender=?1 AND request_id=?2 AND state='interrupted'", params![sender,id])?;
        }
        let state = match previous {
            Some((prior, _, _, _)) if prior != digest => {
                return Err(ReusedRequestId.into());
            }
            Some((_, state, _, _)) if state=="retired" => Disposition::Retired,
            Some((_, state, response, _)) if state == "completed" => {
                Disposition::Completed(response.context("completed receipt missing response")?)
            }
            Some((_, state, _, mode)) if (state == "executing" || state == "interrupted") && mode == "local" => {
                Disposition::ExecutingLocal
            }
            Some((_, state, _, _)) if state == "executing" || state == "interrupted" => Disposition::Executing,
            Some(_) => Disposition::Accepted,
            None => {
                if identity.is_none() && !read_only && legacy_scope.map(|scope|crate::operation_journal::opted_in(&tx,scope)).transpose()?.unwrap_or(false) {
                    let count:i64 = tx.query_row("SELECT count(*) FROM service_receipts WHERE sender=?1 AND journal_issuer IS NULL AND execution_mode<>'read'",params![sender],|row|row.get(0))?;
                    if count>=crate::operation_journal::MAX_LEGACY as i64 { return Err(crate::operation_journal::Capacity.into()); }
                }
                tx.execute("INSERT INTO service_receipts(sender,request_id,digest,state,recovery_digest,recovery_binding) VALUES (?1,?2,?3,'accepted',?4,?5)",
                    params![sender,id,digest.as_slice(),recovery.map(|value|value.digest.as_slice()),recovery.map(|value|value.binding.as_slice())])?;
                if read_only { tx.execute("UPDATE service_receipts SET execution_mode='read' WHERE sender=?1 AND request_id=?2",params![sender,id])?; }
                if let Some(identity) = &identity {
                    let scope = recovery.and_then(|recovery|recovery.operation.as_ref()).unwrap();
                    tx.execute("UPDATE service_receipts SET journal_issuer=?3,journal_generation=?4,journal_sequence=?5 WHERE sender=?1 AND request_id=?2",
                        params![sender,id,scope.issuer,crate::operation_journal::sql_counter(identity.generation)?,crate::operation_journal::sql_counter(identity.sequence)?])?;
                }
                Disposition::Accepted
            }
        };
        tx.commit()?;
        Ok(state)
    }
    /// Run once when the daemon starts, before it takes work: no receipt can still have a live
    /// executor, so whatever the previous process left running died with it. A read has no effect
    /// and is forgotten; any other operation becomes `interrupted`, which a redelivery recovers
    /// and an acknowledgement retires. Journal rows are never deleted here: their count is proof.
    fn recover_interrupted(&self) -> Result<(usize, usize)> {
        let mut c = self.db()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let reads = tx.execute("DELETE FROM service_receipts WHERE state IN ('accepted','executing') AND execution_mode='read' AND journal_issuer IS NULL", [])?;
        let interrupted = tx.execute("UPDATE service_receipts SET state='interrupted' WHERE state='executing'", [])?;
        tx.commit()?;
        Ok((reads, interrupted))
    }
    fn executing(&self, sender: &str, id: &str, digest: &[u8; 32]) -> Result<()> {
        let c = self.db()?;
        let n = c.execute("UPDATE service_receipts SET state='executing' WHERE sender=?1 AND request_id=?2 AND digest=?3 AND state='accepted'", params![sender,id,digest.as_slice()])?;
        if n != 1 {
            bail!("operation receipt no longer accepted");
        }
        Ok(())
    }
    fn executing_local(&self, sender: &str, id: &str, digest: &[u8; 32]) -> Result<()> {
        let n = self.db()?.execute("UPDATE service_receipts SET state='executing',execution_mode='local' WHERE sender=?1 AND request_id=?2 AND digest=?3 AND state='accepted'", params![sender,id,digest.as_slice()])?;
        anyhow::ensure!(n == 1, "operation receipt no longer accepted");
        Ok(())
    }

    fn complete(&self, sender: &str, id: &str, response: &[u8]) -> Result<()> {
        let mut c = self.db()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let n = tx.execute("UPDATE service_receipts SET state='completed', response=?3 WHERE sender=?1 AND request_id=?2 AND state='executing'", params![sender,id,response])?;
        if n!=1 { bail!("operation receipt no longer executing"); }
        self.trim_replies(&tx,sender)?;
        tx.commit()?;Ok(())
    }
    fn trim_replies(&self,tx:&rusqlite::Transaction<'_>,sender:&str)->Result<()> {
        for read_only in [true,false] {
            let predicate = if read_only { "execution_mode='read'" } else { "execution_mode<>'read' AND journal_issuer IS NULL" };
            let rows:Vec<(String,i64)> = {
                let mut statement = tx.prepare(&format!("SELECT request_id,length(response) FROM service_receipts WHERE sender=?1 AND state='completed' AND response IS NOT NULL AND {predicate} ORDER BY rowid DESC"))?;
                statement.query_map(params![sender],|row|Ok((row.get(0)?,row.get(1)?)))?.collect::<rusqlite::Result<_>>()?
            };
            let mut bytes=0usize;
            for (index,(id,len)) in rows.into_iter().enumerate() {
                bytes=bytes.saturating_add(usize::try_from(len)?);
                if index>=crate::operation_journal::MAX_CACHED_REPLIES || bytes>crate::operation_journal::MAX_CACHED_BYTES {
                    if read_only { tx.execute("DELETE FROM service_receipts WHERE sender=?1 AND request_id=?2",params![sender,id])?; }
                    else { tx.execute("UPDATE service_receipts SET response=NULL,state='retired',recovery_digest=NULL,recovery_binding=NULL WHERE sender=?1 AND request_id=?2",params![sender,id])?; }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "service/domain_parity_tests.rs"]
mod domain_parity_tests;

#[cfg(test)]
mod tests {
    mod relay_scheduler_tests;
    mod journal_admission_tests;
    mod journal_transition_tests;
    mod push_commit_tests;
    mod operation_result_tests;
    mod approval_lease_deadlock_tests;
    use super::*;
    use reqwest::{StatusCode, Url};
    use std::collections::HashSet;
    use crate::push::local_minutes;
    use axum::{
        Router,
        body::Bytes,
        extract::State,
        http::HeaderMap,
        response::IntoResponse,
        routing::{delete, get, post},
    };
    use base64::engine::general_purpose::STANDARD;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    #[test]
    fn relay_error_code_is_read_from_the_relay_error_body() {
        assert_eq!(
            relay_error_code(br#"{"error":"push_registration_gone"}"#),
            "push_registration_gone"
        );
        assert_eq!(
            relay_error_code(br#"{"error":"apns_delivery_failed"}"#),
            "apns_delivery_failed"
        );
        assert_eq!(relay_error_code(b"<html>bad gateway</html>"), "unknown");
        assert_eq!(relay_error_code(br#"{"error":"forged\nline"}"#), "unknown");
        assert_eq!(relay_error_code(b""), "unknown");
    }
    #[tokio::test]
    async fn revocation_waits_until_approved_response_ack_finishes() {
        let scratch_directory = crate::test_support::Directory::new();
        use std::sync::mpsc;
        use std::time::Duration;

        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-response-lease-{}",
            crate::auth::random_hex(16)
        ));
        std::fs::create_dir_all(&home).unwrap();
        let phone = SigningKey::from_slice(&[33u8; 32]).unwrap();
        std::fs::write(
            home.join("devices.json"),
            serde_json::to_vec(&json!([{
                "id":"phone-response","name":"Phone",
                "public_key":STANDARD.encode(phone.verifying_key().to_sec1_bytes()),
                "created_at":0,"last_seen":null
            }]))
            .unwrap(),
        )
        .unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let delivery_home = home.clone();
        let delivery = tokio::spawn(async move {
            approval_bound_delivery(
                &delivery_home,
                || {
                    anyhow::ensure!(
                        Auth::open(&delivery_home)?
                            .list()
                            .iter()
                            .any(|device| device.id == "phone-response")
                    );
                    Ok(())
                },
                || async {
                    entered_tx.send(()).unwrap();
                    release_rx.await.unwrap(); // stand-in for relay response acknowledgement
                    Ok(())
                },
            )
            .await
        });
        entered_rx.await.unwrap();
        let (revoked_tx, revoked_rx) = mpsc::channel();
        let revoke_home = home.clone();
        let revoker = std::thread::spawn(move || {
            revoked_tx
                .send(
                    Auth::open(&revoke_home)
                        .unwrap()
                        .revoke("phone-response")
                        .unwrap(),
                )
                .unwrap();
        });
        assert!(
            revoked_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "revocation returned before the relay acknowledged the response"
        );
        release_tx.send(()).unwrap();
        delivery.await.unwrap().unwrap();
        assert!(revoked_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        revoker.join().unwrap();
        let started = AtomicBool::new(false);
        assert!(
            approval_bound_delivery(
                &home,
                || {
                    anyhow::ensure!(
                        Auth::open(&home)?
                            .list()
                            .iter()
                            .any(|device| device.id == "phone-response")
                    );
                    Ok(())
                },
                || async {
                    started.store(true, Ordering::SeqCst);
                    Ok(())
                }
            )
            .await
            .is_err()
        );
        assert!(
            !started.load(Ordering::SeqCst),
            "revoked response was uploaded"
        );
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn push_binding_commit_cannot_follow_completed_revocation() {
        let scratch_directory = crate::test_support::Directory::new();
        use std::sync::mpsc;
        use std::time::Duration;

        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-push-lease-{}",
            crate::auth::random_hex(16)
        ));
        std::fs::create_dir_all(&home).unwrap();
        let phone = SigningKey::from_slice(&[35u8; 32]).unwrap();
        std::fs::write(
            home.join("devices.json"),
            serde_json::to_vec(&json!([{
                "id":"phone-push","name":"Phone",
                "public_key":STANDARD.encode(phone.verifying_key().to_sec1_bytes()),
                "created_at":0,"last_seen":null
            }]))
            .unwrap(),
        )
        .unwrap();
        let path = home.join("service-receipts.sqlite");
        let journal = Journal::open(path.clone()).unwrap();
        journal
            .reserve_enrollment("phone-push", &[1u8; 33])
            .unwrap();
        journal.activate_enrollment("phone-push").unwrap();
        let digest = [1u8; 32];
        assert!(matches!(
            journal
                .accept("phone-push", "binding-work", &digest)
                .unwrap(),
            Disposition::Accepted
        ));
        journal
            .executing_local("phone-push", "binding-work", &digest)
            .unwrap();
        let binding = URL_SAFE_NO_PAD.encode([8u8; 32]);
        let (checked_tx, checked_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let writer_home = home.clone();
        let writer = std::thread::spawn(move || {
            approval_bound_commit(
                &writer_home,
                || {
                    anyhow::ensure!(
                        Auth::open(&writer_home)?
                            .list()
                            .iter()
                            .any(|device| device.id == "phone-push")
                    );
                    checked_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                    Ok(())
                },
                || {
                    Journal::open(path)?.complete_push_preferences(
                        "phone-push",
                        "binding-work",
                        &digest,
                        &binding,
                        None,
                        None,
                        |result| Ok(serde_json::to_vec(&result)?),
                    )
                },
            )
        });
        checked_rx.recv().unwrap();
        let (revoked_tx, revoked_rx) = mpsc::channel();
        let revoker_home = home.clone();
        let revoker = std::thread::spawn(move || {
            revoked_tx
                .send(
                    Auth::open(&revoker_home)
                        .unwrap()
                        .revoke("phone-push")
                        .unwrap(),
                )
                .unwrap();
        });
        assert!(
            revoked_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "revocation returned before push-binding commit"
        );
        resume_tx.send(()).unwrap();
        let receipt = writer.join().unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&receipt).unwrap()["status"],
            204
        );
        assert!(revoked_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        revoker.join().unwrap();
        let attempted = AtomicBool::new(false);
        assert!(
            approval_bound_commit(
                &home,
                || {
                    anyhow::ensure!(
                        Auth::open(&home)?
                            .list()
                            .iter()
                            .any(|device| device.id == "phone-push")
                    );
                    Ok(())
                },
                || {
                    attempted.store(true, Ordering::SeqCst);
                    Ok(())
                }
            )
            .is_err()
        );
        assert!(!attempted.load(Ordering::SeqCst));
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn legacy_receipt_schema_migrates_without_reexecuting_ambiguous_work() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-legacy-receipts-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let path = home.join("receipts.sqlite");
        let old = Connection::open(&path).unwrap();
        old.execute_batch("CREATE TABLE service_receipts(sender TEXT NOT NULL, request_id TEXT NOT NULL,
            digest BLOB NOT NULL, state TEXT NOT NULL, response BLOB, PRIMARY KEY(sender,request_id));").unwrap();
        for (id, state, response) in [
            ("accepted", "accepted", None),
            ("executing", "executing", None),
            (
                "completed",
                "completed",
                Some(b"original ciphertext".as_slice()),
            ),
        ] {
            old.execute("INSERT INTO service_receipts(sender,request_id,digest,state,response) VALUES('client',?1,?2,?3,?4)",
                params![id, [1u8;32].as_slice(), state, response]).unwrap();
        }
        drop(old);
        let journal = Journal::open(path.clone()).unwrap();
        assert!(matches!(
            journal.accept("client", "accepted", &[1u8; 32]).unwrap(),
            Disposition::Accepted
        ));
        assert!(
            matches!(
                journal.accept("client", "executing", &[1u8; 32]).unwrap(),
                Disposition::Executing
            ),
            "old executing state must take the uncertain branch, never atomic recovery"
        );
        assert!(
            matches!(journal.accept("client", "completed", &[1u8;32]).unwrap(),
            Disposition::Completed(bytes) if bytes == b"original ciphertext")
        );
        assert!(journal.accept("client", "completed", &[2u8; 32]).is_err());
        journal.accept("client", "new-local", &[3u8; 32]).unwrap();
        journal
            .executing_local("client", "new-local", &[3u8; 32])
            .unwrap();
        let reopened = Journal::open(path).unwrap();
        assert!(matches!(
            reopened.accept("client", "new-local", &[3u8; 32]).unwrap(),
            Disposition::ExecutingLocal
        ));
        assert!(matches!(
            reopened.accept("client", "executing", &[1u8; 32]).unwrap(),
            Disposition::Executing
        ));
        let state: String = reopened
            .db()
            .unwrap()
            .query_row(
                "SELECT state FROM service_receipts WHERE request_id='new-local'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            state, "executing",
            "a downgraded binary must still refuse redispatch"
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn atomic_push_preferences_roll_back_with_receipt_and_recover_once() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-atomic-preferences-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let path = home.join("receipts.sqlite");
        let journal = Journal::open(path.clone()).unwrap();
        journal.reserve_enrollment("client-a", &[1u8; 33]).unwrap();
        journal.activate_enrollment("client-a").unwrap();
        let binding = URL_SAFE_NO_PAD.encode([7u8; 32]);
        journal
            .accept("client-a", "operation-1", &[1u8; 32])
            .unwrap();
        journal
            .executing_local("client-a", "operation-1", &[1u8; 32])
            .unwrap();
        let apply = |journal: &Journal| {
            journal.complete_push_preferences(
                "client-a",
                "operation-1",
                &[1u8; 32],
                &binding,
                Some(QuietHours::default()),
                Some(1),
                |v| Ok(serde_json::to_vec(&v)?),
            )
        };
        journal.db().unwrap().execute_batch("CREATE TRIGGER fail_receipt BEFORE UPDATE OF response ON service_receipts BEGIN SELECT RAISE(ABORT,'injected commit failure'); END;").unwrap();
        assert!(apply(&journal).is_err());
        assert!(journal.push_binding("client-a").unwrap().is_none());
        assert!(matches!(
            journal
                .accept("client-a", "operation-1", &[1u8; 32])
                .unwrap(),
            Disposition::ExecutingLocal
        ));
        journal
            .db()
            .unwrap()
            .execute_batch("DROP TRIGGER fail_receipt;")
            .unwrap();
        let journal = Journal::open(path.clone()).unwrap();
        let bytes = apply(&journal).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["status"],
            204
        );
        assert_eq!(
            journal.push_binding("client-a").unwrap(),
            Some(binding.clone())
        );
        let replay = journal
            .complete_push_preferences(
                "client-a",
                "operation-1",
                &[1u8; 32],
                &binding,
                None,
                None,
                |_| panic!("completed preferences must not execute again"),
            )
            .unwrap();
        assert_eq!(bytes, replay);
        assert!(
            journal
                .complete_push_preferences(
                    "client-a",
                    "operation-1",
                    &[2u8; 32],
                    &binding,
                    None,
                    None,
                    |_| panic!()
                )
                .is_err()
        );
        journal.accept("client-a", "external", &[3u8; 32]).unwrap();
        journal
            .executing("client-a", "external", &[3u8; 32])
            .unwrap();
        assert!(
            matches!(
                Journal::open(path)
                    .unwrap()
                    .accept("client-a", "external", &[3u8; 32])
                    .unwrap(),
                Disposition::Executing
            ),
            "legacy/external executing operations remain uncertain"
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn journal_rejects_final_symlink_without_touching_target_on_open_or_reopen() {
        let scratch_directory = crate::test_support::Directory::new();
        use std::os::unix::fs::{PermissionsExt, symlink};
        let home =
            scratch_directory.parent().unwrap().join(format!("journal-no-follow-{}", crate::auth::random_hex(8)));
        std::fs::create_dir_all(&home).unwrap();
        let target = home.join("preserved-old.sqlite");
        let db = Connection::open(&target).unwrap();
        db.execute_batch(
            "CREATE TABLE preserved(value TEXT); INSERT INTO preserved VALUES('old receipts');",
        )
        .unwrap();
        drop(db);
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        let original = std::fs::read(&target).unwrap();
        let path = home.join("new-generation.sqlite");
        symlink(&target, &path).unwrap();
        assert!(
            Journal::open(path.clone()).is_err(),
            "preparation must not follow the final symlink or chmod its target"
        );
        assert!(
            Journal::connect(&path).is_err(),
            "SQLite NOFOLLOW must reject a symlink swapped in after preparation"
        );
        assert_eq!(std::fs::read(&target).unwrap(), original);
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o644
        );
        std::fs::remove_file(&path).unwrap();
        let journal = Journal::open(path.clone()).unwrap();
        journal
            .accept("client-new", "request-new", &[7; 32])
            .unwrap();
        std::fs::rename(&path, home.join("actual-new.sqlite")).unwrap();
        symlink(&target, &path).unwrap();
        assert!(
            journal.db().is_err(),
            "every later connection must also reject a replaced symlink"
        );
        assert_eq!(std::fs::read(&target).unwrap(), original);
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o644
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn push_binding_rotates_persists_and_is_removed_on_revocation() {
        let scratch_directory = crate::test_support::Directory::new();
        let path = scratch_directory.parent().unwrap().join(format!(
            "alondra-push-bindings-{}-{}.sqlite",
            std::process::id(),
            now_ms()
        ));
        let journal = Journal::open(path.clone()).unwrap();
        let key = SigningKey::from_slice(&[33u8; 32]).unwrap();
        assert!(matches!(
            journal
                .reserve_enrollment("phone", &key.verifying_key().to_sec1_bytes())
                .unwrap(),
            EnrollmentDecision::New
        ));
        assert!(
            journal
                .set_push_binding("phone", &URL_SAFE_NO_PAD.encode([1u8; 32]))
                .is_err(),
            "pending enrollment cannot activate push"
        );
        journal.activate_enrollment("phone").unwrap();
        let first = URL_SAFE_NO_PAD.encode([1u8; 32]);
        let second = URL_SAFE_NO_PAD.encode([2u8; 32]);
        journal.set_push_binding("phone", &first).unwrap();
        assert_eq!(
            Journal::open(path.clone())
                .unwrap()
                .push_binding("phone")
                .unwrap()
                .as_deref(),
            Some(first.as_str())
        );
        journal.set_push_binding("phone", &second).unwrap();
        assert_eq!(
            journal.push_binding("phone").unwrap().as_deref(),
            Some(second.as_str())
        );
        assert!(journal.set_push_binding("phone", "not-base64url").is_err());
        journal.revoke_enrollment("phone").unwrap();
        assert!(
            Journal::open(path)
                .unwrap()
                .push_binding("phone")
                .unwrap()
                .is_none()
        );
        assert!(journal.set_push_binding("phone", &first).is_err());
    }

    #[test]
    fn quiet_hours_validate_boundaries_and_preserve_payload() {
        let parse = |start: Value, end: Value| QuietHours::parse_update(Some(&start), Some(&end));
        for (start, end) in [
            (json!("24:00"), json!("08:00")),
            (json!("23:00"), json!("08:60")),
            (json!("1:00"), json!("08:00")),
            (json!("23:00"), Value::Null),
            (json!("é:00"), json!("08:00")),
        ] {
            assert!(parse(start, end).is_err());
        }
        assert!(QuietHours::parse_update(Some(&Value::Null), None).is_err());
        assert_eq!(QuietHours::parse_update(None, None).unwrap(), None);
        let quiet = parse(json!("23:00"), json!("08:00")).unwrap().unwrap();
        let payload = json!({"aps":{"sound":"default", "alert":{"title":"private"}, "badge":3}, "alondra":{"id":9}});
        for (minute, expected) in [
            (22 * 60 + 59, false),
            (23 * 60, true),
            (0, true),
            (8 * 60 - 1, true),
            (8 * 60, false),
        ] {
            let (body, sound) = quiet.apply(&payload, true, minute);
            assert_eq!(sound, !expected);
            assert_eq!(
                body["aps"]["interruption-level"],
                if expected { "passive" } else { "active" }
            );
            assert_eq!(body["aps"].get("sound").is_none(), expected);
            assert_eq!(body["aps"]["alert"], payload["aps"]["alert"]);
            assert_eq!(body["alondra"], payload["alondra"]);
        }
        assert!(
            !quiet.apply(&payload, false, 12 * 60).0["aps"]
                .as_object()
                .unwrap()
                .contains_key("sound")
        );
        let equal = parse(json!("08:00"), json!("08:00")).unwrap().unwrap();
        assert!(
            equal.apply(&payload, true, 8 * 60).1,
            "equal bounds disable silence as in legacy"
        );
    }

    #[test]
    fn quiet_hours_persist_per_device_rotate_clear_and_revoke() {
        let scratch_directory = crate::test_support::Directory::new();
        let path = scratch_directory.parent().unwrap().join(format!(
            "alondra-quiet-{}-{}.sqlite",
            std::process::id(),
            now_ms()
        ));
        let journal = Journal::open(path.clone()).unwrap();
        let binding = URL_SAFE_NO_PAD.encode([7u8; 32]);
        let quiet = || {
            Some(QuietHours {
                start: Some("23:00".into()),
                end: Some("08:00".into()),
            })
        };
        assert!(
            journal
                .set_push_preferences("a", &binding, quiet())
                .is_err()
        );
        for id in ["a", "b"] {
            journal.reserve_enrollment(id, &[1u8; 33]).unwrap();
            journal.activate_enrollment(id).unwrap();
        }
        journal
            .set_push_preferences("a", &binding, quiet())
            .unwrap();
        assert_eq!(
            Journal::open(path.clone())
                .unwrap()
                .quiet_hours("a")
                .unwrap(),
            quiet().unwrap()
        );
        assert_eq!(journal.quiet_hours("b").unwrap(), QuietHours::default());
        journal
            .set_push_preferences("a", &URL_SAFE_NO_PAD.encode([8u8; 32]), None)
            .unwrap();
        assert_eq!(journal.quiet_hours("a").unwrap(), quiet().unwrap());
        journal
            .set_push_preferences("a", &binding, Some(QuietHours::default()))
            .unwrap();
        assert_eq!(journal.quiet_hours("a").unwrap(), QuietHours::default());
        journal
            .set_push_preferences("a", &binding, quiet())
            .unwrap();
        journal.revoke_enrollment("a").unwrap();
        assert_eq!(journal.quiet_hours("a").unwrap(), QuietHours::default());
        let count: i64 = journal
            .db()
            .unwrap()
            .query_row("SELECT count(*) FROM service_push_preferences", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn quiet_hours_reordered_retries_cannot_restore_stale_preferences() {
        let scratch_directory = crate::test_support::Directory::new();
        let path = scratch_directory.parent().unwrap().join(format!(
            "alondra-quiet-revision-{}-{}.sqlite",
            std::process::id(),
            now_ms()
        ));
        let journal = Journal::open(path.clone()).unwrap();
        journal.reserve_enrollment("phone", &[1u8; 33]).unwrap();
        journal.activate_enrollment("phone").unwrap();
        let binding = URL_SAFE_NO_PAD.encode([7u8; 32]);
        let second_binding = URL_SAFE_NO_PAD.encode([8u8; 32]);
        let quiet = || {
            Some(QuietHours {
                start: Some("23:00".into()),
                end: Some("08:00".into()),
            })
        };
        let disabled = || Some(QuietHours::default());
        assert_eq!(
            journal
                .set_versioned_push_preferences("phone", &binding, quiet(), Some(1))
                .unwrap(),
            None
        );
        assert_eq!(
            journal
                .set_versioned_push_preferences("phone", &second_binding, disabled(), Some(2))
                .unwrap(),
            None
        );
        // Process restart + an uncertain old operation retried with a fresh UUID.
        let journal = Journal::open(path).unwrap();
        assert_eq!(
            journal
                .set_versioned_push_preferences("phone", &binding, quiet(), Some(1))
                .unwrap(),
            Some(2)
        );
        assert_eq!(
            journal
                .set_versioned_push_preferences("phone", &binding, quiet(), None)
                .unwrap(),
            Some(2)
        );
        assert_eq!(
            journal
                .set_versioned_push_preferences("phone", &binding, quiet(), Some(2))
                .unwrap(),
            Some(2)
        );
        assert_eq!(
            journal
                .set_versioned_push_preferences("phone", &second_binding, disabled(), Some(2))
                .unwrap(),
            None
        );
        assert_eq!(journal.push_binding("phone").unwrap(), Some(second_binding));
        assert_eq!(journal.quiet_hours("phone").unwrap(), QuietHours::default());
        // A -> B -> A is a new logical update, never a cached old operation.
        assert_eq!(
            journal
                .set_versioned_push_preferences("phone", &binding, quiet(), Some(3))
                .unwrap(),
            None
        );
        assert_eq!(journal.quiet_hours("phone").unwrap(), quiet().unwrap());
        journal.revoke_enrollment("phone").unwrap();
        let count: i64 = journal
            .db()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM service_push_preference_revisions",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn routes_is_a_read_every_device_may_make() {
        let request = |method: &str, body: Option<Value>| LocalRequest { method: method.into(), path: "/routes".into(), body };
        assert_eq!(approved_method(&request("GET", None)).unwrap(), Method::GET);
        assert!(approved_method(&request("POST", Some(json!({})))).is_err());
        assert!(approved_method(&request("GET", Some(json!({})))).is_err());
    }

    fn push_fixture() -> (crate::test_support::Directory, Service, SecretKey) {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let phone_signing = SigningKey::from_slice(&[33u8; 32]).unwrap();
        let phone_agreement = SecretKey::from_slice(&[34u8; 32]).unwrap();
        // Use the exact disk contract written by service_pair::approve: the
        // signing key is locally approved and both phone keys are pinned in TOML.
        for (name, bytes) in [("signing.key", [1u8; 32]), ("agreement.key", [2u8; 32])] {
            let path = home.join(name);
            std::fs::write(&path, URL_SAFE_NO_PAD.encode(bytes)).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
        std::fs::write(
            home.join("devices.json"),
            serde_json::to_vec(&json!([{
                "id":"phone-1", "name":"iPhone", "created_at":0, "last_seen":null,
                "public_key":STANDARD.encode(phone_signing.verifying_key().to_sec1_bytes())
            }]))
            .unwrap(),
        )
        .unwrap();
        let config = format!(
            r#"relay_url = "http://127.0.0.1:1/"
mac_id = "mac-1"
owner_sub = "owner"
signing_key_path = "signing.key"
agreement_key_path = "agreement.key"
[[devices]]
device_id = "phone-1"
signing_key = "{}"
agreement_key = "{}"
"#,
            URL_SAFE_NO_PAD.encode(phone_signing.verifying_key().to_sec1_bytes()),
            URL_SAFE_NO_PAD.encode(phone_agreement.public_key().to_sec1_bytes())
        );
        std::fs::write(home.join("service.toml"), config).unwrap();
        let service = Service::open(&home, 0, Arc::new(Auth::open(&home).unwrap()))
            .unwrap()
            .unwrap();
        (home, service, phone_agreement)
    }

    #[test]
    fn runtime_progress_encrypted_mutations_run_off_worker_and_recheck_queued_approval() {
        use std::future::Future;
        for revoke in ["none", "approval", "enrollment"] {
            let (directory, service, phone) = push_fixture();
            activate_push(&service);
            let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&directory).unwrap());
            let client = crate::client_routes::Client::open(domain.clone(), &directory, Default::default());
            let service = service.with_domain(domain.clone()).with_client(client);
            let work = changes_work(&service, "queued-write", json!({"method":"POST","path":"/requests", "body":{
                "request_id":"isolated-write", "text":"Ship it", "options":{}, "attachments":[]
            }}));
            let prepared = service.prepare_work(&work, Transport::Relay).unwrap().unwrap();
            service.journal.accept("phone-1", "queued-write", &prepared.digest).unwrap();
            service.journal.executing_local("phone-1", "queued-write", &prepared.digest).unwrap();
            let receipt = service.journal.reserve_receipt(prepared.guard.clone()).unwrap();
            let device = service.devices.get("phone-1").unwrap();
            let driver = std::thread::current().id();
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime.block_on(async {
                let mut holders = Vec::new();
                let mut releases = Vec::new();
                for _ in 0..2 {
                    let lane = domain.external_work.clone();
                    let (entered, started) = tokio::sync::oneshot::channel();
                    let (release, held) = std::sync::mpsc::channel();
                    holders.push(tokio::spawn(async move {
                        lane.run(move || { entered.send(()).unwrap(); held.recv().unwrap(); }).await.unwrap()
                    }));
                    started.await.unwrap();
                    releases.push(release);
                }
                let threads = Arc::new(Mutex::new(Vec::new()));
                let running = threads.clone();
                domain.external_work.on_start(move || {
                    let thread = std::thread::current().id();
                    assert_ne!(thread, driver, "encrypted mutation occupied the async worker");
                    running.lock().unwrap().push(thread);
                });
                let mut pending = Box::pin(service.execute_atomic_local(
                    &work.envelope, &device, &prepared.request, &prepared.digest, Transport::Relay, receipt,
                ));
                assert!(pending.as_mut().poll(&mut std::task::Context::from_waker(std::task::Waker::noop())).is_pending());
                assert!(!domain.state().unwrap().requests.contains_key(&"isolated-write".into()));
                match revoke {
                    "approval" => { service.auth.revoke("phone-1").unwrap(); }
                    "enrollment" => service.journal.revoke_enrollment("phone-1").unwrap(),
                    _ => (),
                }
                for release in releases { release.send(()).unwrap(); }
                let bytes = pending.await.unwrap();
                for holder in holders { holder.await.unwrap(); }
                let envelope: Envelope = serde_json::from_slice(&bytes).unwrap();
                let answer: Value = serde_json::from_slice(&envelope.open(
                    service.signing.verifying_key(), &phone, "mac-1", "phone-1", "mac-1", now_ms(),
                ).unwrap()).unwrap();
                assert!(!threads.lock().unwrap().is_empty(), "the real encrypted executor entered its worker");
                if revoke == "none" {
                    assert_eq!(answer["status"], 200, "{answer}");
                    assert!(domain.state().unwrap().requests.contains_key(&"isolated-write".into()));
                } else {
                    assert_eq!(answer["status"], 403, "revocation must fence the queued mutation: {answer}");
                    assert!(!domain.state().unwrap().requests.contains_key(&"isolated-write".into()));
                }
                println!("queued_encrypted_write revoke={revoke} status={} off_worker=true", answer["status"]);
            });
        }
    }

    #[tokio::test]
    async fn encrypted_resource_read_receipts_are_approval_generation_bound() {
        let (_directory, mut service, phone) = push_fixture();
        activate_push(&service);
        let home = service.home.clone();
        let config = alondra_core::config::Config::resolve(
            home.clone(),
            &Default::default(),
            |_| None,
            || Some("Synthetic".into()),
        );
        let hub = Arc::new(crate::hub::Hub::open(config).unwrap());
        service = service.with_hub(hub);
        let responses = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let sent = responses.clone();
        let relay = Router::new()
            .route(
                "/v1/work/{id}/receipt",
                post(|| async { StatusCode::ACCEPTED }),
            )
            .route(
                "/v1/work/{id}/response",
                post(move |bytes: Bytes| {
                    let sent = sent.clone();
                    async move {
                        assert!(bytes.len() <= MAX_RELAY_RESPONSE);
                        sent.lock().unwrap().push(bytes.to_vec());
                        StatusCode::ACCEPTED
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
        let relay_task = tokio::spawn(axum::serve(listener, relay).into_future());
        let epoch = [7; 16];
        let work = changes_work(
            &service,
            "resource-read",
            json!({"method":"GET","path":"/capabilities"}),
        );
        let replay = repeat_work(&work);
        service.handle(work, &epoch).await.unwrap();
        let result = opened_results(&service, &phone, &responses).pop().unwrap();
        assert_eq!(result["status"], 200);

        // Reapproving the same key under a new generation cannot reuse the old receipt.
        let approvals = home.join("devices.json");
        let mut approved: Value =
            serde_json::from_slice(&std::fs::read(&approvals).unwrap()).unwrap();
        approved[0]["created_at"] = json!(1);
        std::fs::write(&approvals, serde_json::to_vec(&approved).unwrap()).unwrap();
        let sent_before = responses.lock().unwrap().len();
        assert!(service.handle(replay, &epoch).await.is_err());
        assert_eq!(responses.lock().unwrap().len(), sent_before);
        relay_task.abort();
        drop(service);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn atomic_local_service_recovers_commit_before_transport_receipt() {
        let (_directory, mut service, phone) = push_fixture();
        activate_push(&service);
        let home = service.home.clone();
        let config = alondra_core::config::Config::resolve(
            home.clone(),
            &Default::default(),
            |_| None,
            || Some("David".into()),
        );
        let hub = Arc::new(crate::hub::Hub::open(config.clone()).unwrap());
        hub.append(alondra_core::Event::Hook(alondra_core::HookEvent {
            session_id: "atomic-session".into(),
            event: "UserPromptSubmit".into(),
            ..Default::default()
        }))
        .unwrap();
        hub.append(alondra_core::Event::ReportPosted(alondra_core::Report {
            id: 7,
            session_id: "atomic-session".into(),
            node_name: None,
            title: "An unread report".into(),
            body: None,
            links: vec![],
            attachments: vec![],
            kind: alondra_core::ReportKind::Notice,
            created_at: 1,
            read: false,
            to_orchestrator: false,
            mission: None,
            looks_like_question: false,
            task: None,
        }))
        .unwrap();
        service = service.with_hub(hub.clone());
        let responses = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let sent = responses.clone();
        let relay = Router::new()
            .route(
                "/v1/work/{id}/receipt",
                post(|| async { StatusCode::ACCEPTED }),
            )
            .route(
                "/v1/work/{id}/response",
                post(move |bytes: axum::body::Bytes| {
                    let sent = sent.clone();
                    async move {
                        sent.lock().unwrap().push(bytes.to_vec());
                        StatusCode::ACCEPTED
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let relay_task = tokio::spawn(async move { axum::serve(listener, relay).await.unwrap() });
        service.set_relay_url(url.clone());
        let work = changes_work(
            &service,
            "atomic-report-read",
            json!({"method":"POST","path":"/v1/reports/7/read","body":{}}),
        );
        service.journal.db().unwrap().execute_batch("CREATE TRIGGER fail_transport_receipt BEFORE UPDATE OF response ON service_receipts BEGIN SELECT RAISE(ABORT,'crash window after local commit'); END;").unwrap();
        assert!(
            service
                .handle(repeat_work(&work), &[1u8; 16])
                .await
                .is_err()
        );
        assert!(
            hub.read(|v| v.reports[&7].read),
            "the local mutation committed before transport failed"
        );
        let committed_seq = hub.read(|v| v.seq);
        assert!(responses.lock().unwrap().is_empty());
        service
            .journal
            .db()
            .unwrap()
            .execute_batch("DROP TRIGGER fail_transport_receipt;")
            .unwrap();
        drop(service);
        drop(hub);
        let hub = Arc::new(crate::hub::Hub::open(config).unwrap());
        let mut service = Service::open(&home, 0, Arc::new(Auth::open(&home).unwrap()))
            .unwrap()
            .unwrap()
            .with_hub(hub.clone());
        service.set_relay_url(url);
        service
            .handle(repeat_work(&work), &[1u8; 16])
            .await
            .unwrap();
        service.handle(work, &[1u8; 16]).await.unwrap();
        assert_eq!(
            hub.read(|v| v.seq),
            committed_seq,
            "recovery must not append another ReportRead"
        );
        let results = opened_results(&service, &phone, &responses);
        assert_eq!(results, vec![json!({"status":200,"body":{"id":7}}); 2]);
        let bytes = responses.lock().unwrap();
        assert_eq!(
            bytes[0], bytes[1],
            "completed retransmission uses identical encrypted bytes"
        );
        drop(bytes);
        relay_task.abort();
        drop(service);
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }

    struct PausedPreparation {
        resume: tokio::sync::Notify,
        entered: std::sync::atomic::AtomicBool,
    }
    impl PausedPreparation {
        async fn wait(&self) {
            self.entered
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.resume.notified().await;
        }
    }
    impl crate::prs::Gh for PausedPreparation {
        async fn edit_base(&self, _: &str, _: &str) -> std::result::Result<(), String> {
            unreachable!()
        }
        async fn view(&self, _: &str) -> Option<Value> {
            unreachable!()
        }
        async fn exists(&self, _: &str) -> bool {
            unreachable!()
        }
        async fn issue_view(&self, _: &str, _: u64) -> std::result::Result<Value, String> {
            self.wait().await;
            Ok(json!({"title":"Issue", "body":"Public fixture", "state":"OPEN"}))
        }
    }
    impl crate::http::ReviewHead for PausedPreparation {
        async fn read(
            &self,
            _: &str,
        ) -> std::result::Result<(String, bool), axum::response::Response> {
            self.wait().await;
            Ok(("a".repeat(40), true))
        }
    }

    #[tokio::test]
    async fn prepared_actions_recheck_local_and_enrollment_revocation_before_commit() {
        use alondra_core::{Event, HookEvent};
        for route in ["/v1/requests", "/v1/prs/1/changes", "/v1/prs/1/ask"] {
            for revoke in ["none", "approval", "enrollment"] {
                let (_directory, mut service, _) = push_fixture();
                activate_push(&service);
                let home = service.home.clone();
                let config = alondra_core::config::Config::resolve(
                    home.clone(),
                    &Default::default(),
                    |_| None,
                    || Some("David".into()),
                );
                let hub = Arc::new(crate::hub::Hub::open(config).unwrap());
                hub.append(Event::Hook(HookEvent {
                    session_id: "s".into(),
                    event: "UserPromptSubmit".into(),
                    ..Default::default()
                }))
                .unwrap();
                hub.append(Event::MarkedOrchestrator {
                    session_id: "s".into(),
                })
                .unwrap();
                hub.append(Event::PrOpened {
                    url: "https://github.com/example/fixture/pull/1".into(),
                    session_id: "s".into(),
                    task: None,
                })
                .unwrap();
                let mut pr = hub.read(|v| v.prs[&1].clone());
                pr.head_sha = Some("a".repeat(40));
                hub.append(Event::PrUpdated(pr)).unwrap();
                service = service.with_hub(hub.clone());
                let body = if route == "/v1/requests" {
                    json!({"text":"Read issue", "issue":"example/fixture#1"})
                } else {
                    json!({"text":"Review this", "expected_head_sha":"a".repeat(40)})
                };
                let request = LocalRequest {
                    method: "POST".into(),
                    path: route.into(),
                    body: Some(body),
                };
                assert!(crate::http::prepared_service_path(route));
                assert!(
                    !service.atomic_local(&request),
                    "preserve conservative external receipt recovery"
                );
                service
                    .require_device_approval("phone-1", &service.devices.get("phone-1").unwrap(), Transport::Relay)
                    .unwrap();
                service
                    .journal
                    .accept("phone-1", "prepared", &[4; 32])
                    .unwrap();
                service
                    .journal
                    .executing("phone-1", "prepared", &[4; 32])
                    .unwrap();
                let seq = hub.read(|v| v.seq);
                let preparation = PausedPreparation {
                    resume: tokio::sync::Notify::new(),
                    entered: std::sync::atomic::AtomicBool::new(false),
                };
                let mut pending = Box::pin(service.dispatch_with(
                    "phone-1",
                    &request,
                    Transport::Relay,
                    &preparation,
                    &preparation,
                ));
                let waker = std::task::Waker::noop();
                assert!(
                    std::future::Future::poll(
                        pending.as_mut(),
                        &mut std::task::Context::from_waker(waker)
                    )
                    .is_pending()
                );
                assert!(
                    preparation
                        .entered
                        .load(std::sync::atomic::Ordering::SeqCst),
                    "must actually reach external preparation: {route}"
                );
                assert_eq!(hub.read(|v| v.seq), seq);
                match revoke {
                    "approval" => {
                        Auth::open(&home).unwrap().revoke("phone-1").unwrap();
                    }
                    "enrollment" => service.journal.revoke_enrollment("phone-1").unwrap(),
                    _ => (),
                }
                preparation.resume.notify_one();
                let result = tokio::time::timeout(Duration::from_secs(2), pending)
                    .await
                    .unwrap()
                    .unwrap();
                if revoke == "none" {
                    assert_eq!(result["status"], 200, "{route}: {result}");
                    assert_eq!(hub.read(|v| v.messages.len()), 1);
                    assert!(hub.read(|v| v.seq) > seq);
                } else {
                    assert_eq!(result["status"], 403, "{route} {revoke}: {result}");
                    assert_eq!(
                        hub.read(|v| v.seq),
                        seq,
                        "revoked preparation must append nothing"
                    );
                    assert_eq!(hub.read(|v| v.messages.len()), 0);
                }
                // No automatic retry after interruption, with or without final commit.
                assert!(matches!(
                    service
                        .journal
                        .accept("phone-1", "prepared", &[4; 32])
                        .unwrap(),
                    Disposition::Executing
                ));
                drop(service);
                drop(hub);
                std::fs::remove_dir_all(home).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn atomic_composed_message_rechecks_revocation_after_waiting_for_start_lock() {
        for revoke in ["none", "approval", "enrollment"] {
            let (_directory, mut service, _) = push_fixture();
            activate_push(&service);
            let home = service.home.clone();
            let config = alondra_core::config::Config::resolve(
                home.clone(),
                &Default::default(),
                |_| None,
                || Some("David".into()),
            );
            let hub = Arc::new(crate::hub::Hub::open(config).unwrap());
            hub.append(alondra_core::Event::Hook(alondra_core::HookEvent {
                session_id: "s".into(),
                event: "UserPromptSubmit".into(),
                ..Default::default()
            }))
            .unwrap();
            hub.append(alondra_core::Event::TaskCreated(
                alondra_core::tasks::NewTask {
                    id: "t-1".into(),
                    request: "r-1".into(),
                    title: "Task".into(),
                    brief: None,
                    depends_on: vec![],
                    options: Default::default(),
                    session_id: None,
                    launched: false,
                    launched_as: None,
                    start: None,
                },
            ))
            .unwrap();
            service = service.with_hub(hub.clone());
            let request = LocalRequest {
                method: "POST".into(),
                path: "/v1/nodes/s/messages".into(),
                body: Some(json!({"text":"Do this","task":"t-1"})),
            };
            let work = changes_work(
                &service,
                "waiting-message",
                json!({"method":request.method,"path":request.path,"body":request.body}),
            );
            service
                .journal
                .accept("phone-1", "waiting-message", &[1; 32])
                .unwrap();
            service
                .journal
                .executing_local("phone-1", "waiting-message", &[1; 32])
                .unwrap();
            let seq = hub.read(|v| v.seq);
            let held = hub.starts.lock().await;
            let phone = service.devices.get("phone-1").unwrap();
            let prepared = service.prepare_work(&work, Transport::Relay).unwrap().unwrap();
            let receipt = service.journal.reserve_receipt(prepared.guard).unwrap();
            let mut pending = Box::pin(service.execute_atomic_local(
                &work.envelope, &phone, &request, &[1; 32], Transport::Relay, receipt,
            ));
            // Poll exactly to the mutex wait: authorization already passed, mutation has not run.
            let waker = std::task::Waker::noop();
            assert!(
                std::future::Future::poll(
                    pending.as_mut(),
                    &mut std::task::Context::from_waker(waker)
                )
                .is_pending()
            );
            assert_eq!(hub.read(|v| v.seq), seq);
            match revoke {
                "approval" => {
                    Auth::open(&home).unwrap().revoke("phone-1").unwrap();
                }
                "enrollment" => service.journal.revoke_enrollment("phone-1").unwrap(),
                _ => (),
            }
            drop(held);
            let result = tokio::time::timeout(Duration::from_secs(2), pending)
                .await
                .unwrap();
            if revoke == "none" {
                assert!(result.is_ok(), "positive control: {result:?}");
                assert_eq!(hub.read(|v| v.messages.len()), 1);
                assert!(hub.read(|v| v.board.tasks["t-1"].is_assigned_to("s")));
            } else {
                assert!(
                    result.is_err(),
                    "revoked {revoke} must not execute after the wait"
                );
                assert_eq!(hub.read(|v| v.seq), seq);
                assert!(hub.read(|v| v.messages.is_empty()));
                assert!(hub.read(|v| v.board.tasks["t-1"].assignee.is_none()));
            }
            drop(service);
            drop(hub);
            std::fs::remove_dir_all(home).unwrap();
        }
    }

    #[tokio::test]
    async fn atomic_composed_review_recovers_commit_before_transport_receipt() {
        let (_directory, mut service, phone) = push_fixture();
        activate_push(&service);
        let home = service.home.clone();
        let config = alondra_core::config::Config::resolve(
            home.clone(),
            &Default::default(),
            |_| None,
            || Some("David".into()),
        );
        let hub = Arc::new(crate::hub::Hub::open(config.clone()).unwrap());
        hub.append(alondra_core::Event::Hook(alondra_core::HookEvent {
            session_id: "atomic-session".into(),
            event: "UserPromptSubmit".into(),
            ..Default::default()
        }))
        .unwrap();
        hub.append(alondra_core::Event::ReportPosted(alondra_core::Report {
            id: 7,
            session_id: "atomic-session".into(),
            node_name: None,
            title: "An unread report".into(),
            body: None,
            links: vec![alondra_core::Link {
                url: "https://claude.ai/artifact/plan".into(),
                title: None,
            }],
            attachments: vec![],
            kind: alondra_core::ReportKind::Document,
            created_at: 1,
            read: false,
            to_orchestrator: false,
            mission: None,
            looks_like_question: false,
            task: None,
        }))
        .unwrap();
        service = service.with_hub(hub.clone());
        let responses = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let sent = responses.clone();
        let relay = Router::new()
            .route(
                "/v1/work/{id}/receipt",
                post(|| async { StatusCode::ACCEPTED }),
            )
            .route(
                "/v1/work/{id}/response",
                post(move |bytes: axum::body::Bytes| {
                    let sent = sent.clone();
                    async move {
                        sent.lock().unwrap().push(bytes.to_vec());
                        StatusCode::ACCEPTED
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let relay_task = tokio::spawn(async move { axum::serve(listener, relay).await.unwrap() });
        service.set_relay_url(url.clone());
        let work = changes_work(
            &service,
            "atomic-artifact-review",
            json!({"method":"POST","path":"/v1/reports/7/review","body":{"verdict":"comments"}}),
        );
        service.journal.db().unwrap().execute_batch("CREATE TRIGGER fail_transport_receipt BEFORE UPDATE OF response ON service_receipts BEGIN SELECT RAISE(ABORT,'crash window after local commit'); END;").unwrap();
        assert!(
            service
                .handle(repeat_work(&work), &[1u8; 16])
                .await
                .is_err()
        );
        assert!(
            hub.read(|v| v.reports[&7].read),
            "the local mutation committed before transport failed"
        );
        let committed_seq = hub.read(|v| v.seq);
        assert_eq!(
            hub.read(|v| v.messages.len()),
            1,
            "intent committed together with review"
        );
        assert!(responses.lock().unwrap().is_empty());
        service
            .journal
            .db()
            .unwrap()
            .execute_batch("DROP TRIGGER fail_transport_receipt;")
            .unwrap();
        drop(service);
        drop(hub);
        let hub = Arc::new(crate::hub::Hub::open(config).unwrap());
        let mut service = Service::open(&home, 0, Arc::new(Auth::open(&home).unwrap()))
            .unwrap()
            .unwrap()
            .with_hub(hub.clone());
        service.set_relay_url(url);
        service
            .handle(repeat_work(&work), &[1u8; 16])
            .await
            .unwrap();
        service.handle(work, &[1u8; 16]).await.unwrap();
        assert_eq!(
            hub.read(|v| v.seq),
            committed_seq,
            "recovery must not append another review or message"
        );
        assert_eq!(hub.read(|v| v.messages.len()), 1);
        let results = opened_results(&service, &phone, &responses);
        assert_eq!(
            results,
            vec![json!({"status":200,"body":{"id":7,"verdict":"comments"}}); 2]
        );
        let bytes = responses.lock().unwrap();
        assert_eq!(
            bytes[0], bytes[1],
            "completed retransmission uses identical encrypted bytes"
        );
        drop(bytes);
        relay_task.abort();
        drop(service);
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn atomic_schedule_recovers_commit_before_transport_receipt() {
        let (_directory, mut service, phone) = push_fixture();
        activate_push(&service);
        let home = service.home.clone();
        let config = alondra_core::config::Config::resolve(
            home.clone(),
            &Default::default(),
            |_| None,
            || Some("David".into()),
        );
        let hub = Arc::new(crate::hub::Hub::open(config.clone()).unwrap());
        let projects = home.join("projects");
        std::fs::create_dir(&projects).unwrap();
        let options = alondra_core::options::OptionsFile {
            projects_root: Some(projects.to_str().unwrap().into()),
            ..Default::default()
        };
        std::fs::write(
            alondra_core::options::path_in(&home),
            toml::to_string(&options).unwrap(),
        )
        .unwrap();
        service = service.with_hub(hub.clone());
        let responses = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let sent = responses.clone();
        let relay = Router::new()
            .route(
                "/v1/work/{id}/receipt",
                post(|| async { StatusCode::ACCEPTED }),
            )
            .route(
                "/v1/work/{id}/response",
                post(move |bytes: axum::body::Bytes| {
                    let sent = sent.clone();
                    async move {
                        sent.lock().unwrap().push(bytes.to_vec());
                        StatusCode::ACCEPTED
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let relay_task = tokio::spawn(async move { axum::serve(listener, relay).await.unwrap() });
        service.set_relay_url(url.clone());
        let work = changes_work(
            &service,
            "atomic-schedule-create",
            json!({"method":"POST","path":"/v1/schedules","body":{"title":"Durable schedule","text":"Review","frequency":"daily","hour":10,"minute":0,"enabled":false}}),
        );
        service.journal.db().unwrap().execute_batch("CREATE TRIGGER fail_transport_receipt BEFORE UPDATE OF response ON service_receipts BEGIN SELECT RAISE(ABORT,'crash window after local commit'); END;").unwrap();
        assert!(
            service
                .handle(repeat_work(&work), &[1u8; 16])
                .await
                .is_err()
        );
        let schedule_db = Connection::open(home.join("schedules.db")).unwrap();
        let committed: String = schedule_db
            .query_row("SELECT body FROM schedules", [], |r| r.get(0))
            .unwrap();
        drop(schedule_db);
        assert!(responses.lock().unwrap().is_empty());
        service
            .journal
            .db()
            .unwrap()
            .execute_batch("DROP TRIGGER fail_transport_receipt;")
            .unwrap();
        drop(service);
        drop(hub);
        let hub = Arc::new(crate::hub::Hub::open(config).unwrap());
        let mut service = Service::open(&home, 0, Arc::new(Auth::open(&home).unwrap()))
            .unwrap()
            .unwrap()
            .with_hub(hub.clone());
        service.set_relay_url(url);
        service
            .handle(repeat_work(&work), &[1u8; 16])
            .await
            .unwrap();
        service.handle(work, &[1u8; 16]).await.unwrap();
        let schedule_db = Connection::open(home.join("schedules.db")).unwrap();
        assert_eq!(
            schedule_db
                .query_row("SELECT count(*) FROM schedules", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            schedule_db
                .query_row("SELECT body FROM schedules", [], |r| r.get::<_, String>(0))
                .unwrap(),
            committed
        );
        drop(schedule_db);
        let results = opened_results(&service, &phone, &responses);
        assert_eq!(
            results,
            vec![
                json!({"status":201,"body":serde_json::from_str::<Value>(&committed).unwrap()});
                2
            ]
        );
        let bytes = responses.lock().unwrap();
        assert_eq!(
            bytes[0], bytes[1],
            "completed retransmission uses identical encrypted bytes"
        );
        drop(bytes);
        relay_task.abort();
        drop(service);
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn service_push_contains_only_ciphertext_outside_and_enforces_budget() {
        let (_directory, service, phone_agreement) = push_fixture();
        let phone_signing = SigningKey::from_slice(&[33u8; 32]).unwrap();
        let device = service.devices.get("phone-1").unwrap();
        assert!(
            service
                .auth
                .approved_key_matches("phone-1", &device.signing)
        );
        let binding = URL_SAFE_NO_PAD.encode([4u8; 32]);
        service
            .journal
            .reserve_enrollment("phone-1", &phone_signing.verifying_key().to_sec1_bytes())
            .unwrap();
        service.journal.activate_enrollment("phone-1").unwrap();
        let request = LocalRequest {
            method: "POST".into(),
            path: "/v1/service/push-binding".into(),
            body: Some(json!({"push_binding":binding})),
        };
        assert_eq!(
            service.dispatch("phone-1", &request, Transport::Relay).await.unwrap()["status"],
            204
        );
        assert_eq!(
            service.journal.push_binding("phone-1").unwrap().as_deref(),
            Some(binding.as_str())
        );
        let bad_request = LocalRequest {
            method: "POST".into(),
            path: "/v1/service/push-binding".into(),
            body: Some(json!({"push_binding":binding,"unexpected":true})),
        };
        assert!(service.dispatch("phone-1", &bad_request, Transport::Relay).await.is_err());
        let quiet_request = LocalRequest {
            method: "POST".into(),
            path: "/v1/service/push-binding".into(),
            body: Some(json!({"push_binding":binding,"quiet_start":"23:00","quiet_end":"08:00"})),
        };
        assert_eq!(
            service.dispatch("phone-1", &quiet_request, Transport::Relay).await.unwrap()["status"],
            204
        );
        let invalid_quiet = LocalRequest {
            body: Some(
                json!({"push_binding":URL_SAFE_NO_PAD.encode([9u8;32]),"quiet_start":"24:00","quiet_end":"08:00"}),
            ),
            ..quiet_request
        };
        assert!(service.dispatch("phone-1", &invalid_quiet, Transport::Relay).await.is_err());
        assert_eq!(
            service.journal.push_binding("phone-1").unwrap().as_deref(),
            Some(binding.as_str())
        );
        let secret = "private question title for iPhone only";
        let payload = json!({"aps":{"alert":{"title":secret},"category":"ALONDRA_QUESTION"},"alondra":{"kind":"question","id":7}});
        let body = service
            .encode_push("phone-1", &device, &binding, &payload, true, PushDelivery::Alert)
            .unwrap();
        assert!(body.len() <= MAX_PUSH_BODY);
        let wire = String::from_utf8(body.clone()).unwrap();
        assert!(!wire.contains(secret));
        assert!(!wire.contains("ALONDRA_QUESTION"));
        assert!(!wire.contains(&binding));
        let decoded: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(decoded["device_id"], "phone-1");
        let envelope: Envelope = serde_json::from_value(decoded["envelope"].clone()).unwrap();
        assert_eq!(envelope.kind, "push");
        assert_eq!(envelope.route, "mac-1");
        assert_eq!(envelope.recipient, "phone-1");
        let plaintext = envelope
            .open(
                service.signing.verifying_key(),
                &phone_agreement,
                "mac-1",
                "phone-1",
                "mac-1",
                now_ms(),
            )
            .unwrap();
        let content: Value = serde_json::from_slice(&plaintext).unwrap();
        assert_eq!(content["push_binding"], binding);
        assert_eq!(content["payload"], payload);
        assert_eq!(content["with_sound"], true);
        let (quiet_payload, sound) =
            service
                .journal
                .quiet_hours("phone-1")
                .unwrap()
                .apply(&payload, true, 23 * 60);
        let quiet_wire = service
            .encode_push("phone-1", &device, &binding, &quiet_payload, sound, PushDelivery::Alert)
            .unwrap();
        let wire_text = String::from_utf8(quiet_wire.clone()).unwrap();
        assert!(!wire_text.contains("passive") && !wire_text.contains("quiet_start"));
        let envelope: Envelope = serde_json::from_value(
            serde_json::from_slice::<Value>(&quiet_wire).unwrap()["envelope"].clone(),
        )
        .unwrap();
        let opened = envelope
            .open(
                service.signing.verifying_key(),
                &phone_agreement,
                "mac-1",
                "phone-1",
                "mac-1",
                now_ms(),
            )
            .unwrap();
        let opened: Value = serde_json::from_slice(&opened).unwrap();
        assert_eq!(opened["payload"]["aps"]["interruption-level"], "passive");
        assert_eq!(opened["with_sound"], false);
        let huge = json!({"aps":{"alert":{"title":"Long question","body":"x".repeat(MAX_PUSH_PLAINTEXT)},
            "sound":"default","category":"ALONDRA_QUESTION"},"alondra":{"kind":"question","id":7}});
        assert!(
            service
                .encode_push("phone-1", &device, &binding, &huge, true, PushDelivery::Alert)
                .is_err()
        );
        let minimal = service
            .encode_push_or_minimal("phone-1", &device, &binding, &huge, true, PushDelivery::Alert)
            .expect("oversized content becomes a generic alert instead of being dropped");
        assert!(minimal.len() <= MAX_PUSH_BODY);
        let envelope: Envelope = serde_json::from_value(
            serde_json::from_slice::<Value>(&minimal).unwrap()["envelope"].clone(),
        )
        .unwrap();
        let opened: Value = serde_json::from_slice(
            &envelope
                .open(
                    service.signing.verifying_key(),
                    &phone_agreement,
                    "mac-1",
                    "phone-1",
                    "mac-1",
                    now_ms(),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            opened["payload"]["aps"]["alert"],
            json!({"title": "Alondra"}),
            "the phone words the generic line in its own language"
        );
        assert_eq!(opened["payload"]["aps"]["category"], "ALONDRA_QUESTION");
        assert_eq!(
            opened["payload"]["alondra"],
            json!({"kind":"question","id":7}),
            "the item reference survives"
        );
        assert!(!String::from_utf8_lossy(&minimal).contains("Long question"));
        let unshrinkable = json!({"aps":{"alert":{"body":"x"}},"alondra":{"kind":"question","id":"y".repeat(MAX_PUSH_PLAINTEXT)}});
        assert!(
            service
                .encode_push_or_minimal("phone-1", &device, &binding, &unshrinkable, true, PushDelivery::Alert)
                .is_err(),
            "a reference that itself exceeds the budget cannot be pushed"
        );
    }
    #[test]
    fn encrypted_upload_steps_cross_the_domain_admission_gate_without_opening_other_routes() {
        let (_directory, service, _) = push_fixture();
        activate_push(&service);
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&service.home).unwrap());
        let service = service.with_domain(domain);
        for (index, step) in ["start", "chunk", "finish", "cancel"].into_iter().enumerate() {
            let path = format!("/uploads/{step}");
            let valid = changes_work(&service, &format!("upload-{index}"), json!({"method":"POST","path":path,"body":{}}));
            let prepared = service.prepare_work(&valid, Transport::Relay).unwrap().unwrap();
            assert_eq!(prepared.request.path, path);
            drop(prepared);
            for (method, body) in [("GET",Some(json!({}))), ("PUT",Some(json!({}))), ("POST",None)] {
                let mut payload = json!({"method":method,"path":path});
                if let Some(body) = body { payload["body"]=body; }
                let invalid = changes_work(&service,&format!("invalid-{index}-{method}"),payload);
                assert!(service.prepare_work(&invalid,Transport::Relay).is_err());
            }
        }
        let other=changes_work(&service,"unknown-upload",json!({"method":"POST","path":"/uploads/other","body":{}}));
        assert!(service.prepare_work(&other,Transport::Relay).is_err());
        assert!(service.inflight.lock().unwrap().is_empty());
    }

    fn activate_push(service: &Service) {
        let key = service
            .devices
            .get("phone-1")
            .unwrap()
            .signing
            .to_sec1_bytes();
        service.journal.reserve_enrollment("phone-1", &key).unwrap();
        service.journal.activate_enrollment("phone-1").unwrap();
        service
            .journal
            .set_push_preferences("phone-1", &URL_SAFE_NO_PAD.encode([4u8; 32]), None)
            .unwrap();
    }

    #[tokio::test]
    async fn work_epoch_is_reused_until_failure_and_revocations_resync_after_restart() {
        use axum::{Json, http::HeaderMap, routing::delete};
        let (_directory, mut service, _) = push_fixture();
        activate_push(&service);
        let boots = Arc::new(AtomicUsize::new(0));
        let polls = Arc::new(AtomicUsize::new(0));
        let revokes = Arc::new(AtomicUsize::new(0));
        let router =
            Router::new()
                .route(
                    "/v1/bootstrap",
                    get({
                        let boots = boots.clone();
                        move || {
                            let epoch = boots.fetch_add(1, Ordering::SeqCst) + 1;
                            async move {
                                Json(json!({"epoch":URL_SAFE_NO_PAD.encode([epoch as u8;16])}))
                            }
                        }
                    }),
                )
                .route(
                    "/v1/work",
                    get({
                        let polls = polls.clone();
                        move |headers: HeaderMap| {
                            let count = polls.fetch_add(1, Ordering::SeqCst) + 1;
                            async move {
                                assert_eq!(
                                    headers["x-relay-epoch"],
                                    URL_SAFE_NO_PAD
                                        .encode([if count <= 3 { 1u8 } else { 2u8 }; 16])
                                );
                                if count == 3 {
                                    StatusCode::UNAUTHORIZED
                                } else {
                                    StatusCode::NO_CONTENT
                                }
                            }
                        }
                    }),
                )
                .route(
                    "/v1/devices/phone-1",
                    delete({
                        let revokes = revokes.clone();
                        move || {
                            revokes.fetch_add(1, Ordering::SeqCst);
                            async { StatusCode::NO_CONTENT }
                        }
                    }),
                );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let service = Arc::new(service);
        let mut epoch = None;
        let mut revoked = HashSet::new();
        let mut tasks = tokio::task::JoinSet::new();
        service
            .poll_work(&mut epoch, &mut revoked, &mut tasks)
            .await
            .unwrap();
        service.journal.revoke_enrollment("phone-1").unwrap();
        service
            .poll_work(&mut epoch, &mut revoked, &mut tasks)
            .await
            .unwrap();
        assert_eq!(boots.load(Ordering::SeqCst), 1);
        assert_eq!(
            revokes.load(Ordering::SeqCst),
            1,
            "local revocation must sync while epoch is cached"
        );
        assert!(
            service
                .poll_work(&mut epoch, &mut revoked, &mut tasks)
                .await
                .is_err()
        );
        assert!(epoch.is_none());
        assert_eq!(
            polls.load(Ordering::SeqCst),
            3,
            "a failed poll must not replay internally"
        );
        service
            .poll_work(&mut epoch, &mut revoked, &mut tasks)
            .await
            .unwrap();
        assert_eq!(boots.load(Ordering::SeqCst), 2);
        assert_eq!(
            revokes.load(Ordering::SeqCst),
            2,
            "restart resends durable revocation"
        );
        assert_eq!(epoch, Some([2u8; 16]));
        server.abort();
        std::fs::remove_dir_all(&service.home).unwrap();
    }

    async fn ack_server(
        replies: Vec<(u16, Vec<u8>)>,
    ) -> (
        Url,
        Arc<AtomicUsize>,
        Arc<std::sync::Mutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let connections = Arc::new(AtomicUsize::new(0));
        let paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (accepted, seen) = (connections.clone(), paths.clone());
        let server = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(5),async move {
                let mut replies=replies.into_iter().peekable();
                while replies.peek().is_some() {
                    let (stream,_)=listener.accept().await.unwrap(); accepted.fetch_add(1,Ordering::SeqCst);
                    let (read,mut write)=stream.into_split(); let mut reader=BufReader::new(read);
                    while replies.peek().is_some() {
                        let mut line=String::new();
                        match reader.read_line(&mut line).await { Ok(0)|Err(_)=>break, Ok(_)=>() }
                        seen.lock().unwrap().push(line.trim().to_owned());
                        let mut size=0usize;
                        loop {
                            let mut header=String::new(); reader.read_line(&mut header).await.unwrap();
                            if header=="\r\n" { break; }
                            if let Some(value)=header.to_ascii_lowercase().strip_prefix("content-length:") { size=value.trim().parse().unwrap(); }
                        }
                        let mut body=vec![0;size]; reader.read_exact(&mut body).await.unwrap();
                        let (status,body)=replies.next().unwrap();
                        let header=format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",body.len());
                        if write.write_all(header.as_bytes()).await.is_err() { break; }
                        // Headers arrive before the ACK body. Dropping Response without
                        // draining cannot accidentally pass through a fully buffered body.
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        if write.write_all(&body).await.is_err() { break; }
                    }
                }
            }).await.expect("ACK fixture timed out");
        });
        (url, connections, paths, server)
    }

    #[tokio::test]
    async fn receipt_and_response_ack_bodies_are_drained_for_connection_reuse() {
        let (_directory, mut service, _) = push_fixture();
        activate_push(&service);
        let (url, connections, paths, server) =
            ack_server(vec![(200, br#"{"ok":true}"#.to_vec()); 4]).await;
        service.set_relay_url(url);
        for id in ["ack-one", "ack-two"] {
            // Invalid local route returns a bounded encrypted error without any
            // loopback request, leaving only the two real relay POSTs per work.
            let work = changes_work(&service, id, json!({"method":"GET","path":"/not-allowed"}));
            tokio::time::timeout(Duration::from_secs(3), service.handle(work, &[1u8; 16]))
                .await
                .unwrap()
                .unwrap();
        }
        server.await.unwrap();
        assert_eq!(connections.load(Ordering::SeqCst), 1);
        assert_eq!(
            *paths.lock().unwrap(),
            [
                "POST /v1/work/ack-one/receipt HTTP/1.1",
                "POST /v1/work/ack-one/response HTTP/1.1",
                "POST /v1/work/ack-two/receipt HTTP/1.1",
                "POST /v1/work/ack-two/response HTTP/1.1"
            ]
        );
        std::fs::remove_dir_all(&service.home).unwrap();
    }

    #[tokio::test]
    async fn invalid_receipt_ack_is_bounded_and_never_reposted() {
        for (status, body) in [(200, vec![b'a'; 4097]), (500, b"{}".to_vec())] {
            let (_directory, mut service, _) = push_fixture();
            activate_push(&service);
            let (url, _, paths, server) = ack_server(vec![(status, body)]).await;
            service.set_relay_url(url);
            let work = changes_work(
                &service,
                "bad-ack",
                json!({"method":"GET","path":"/not-allowed"}),
            );
            assert!(
                tokio::time::timeout(Duration::from_secs(3), service.handle(work, &[1u8; 16]))
                    .await
                    .unwrap()
                    .is_err()
            );
            server.await.unwrap();
            assert_eq!(
                *paths.lock().unwrap(),
                ["POST /v1/work/bad-ack/receipt HTTP/1.1"]
            );
            let count: i64 = service
                .journal
                .db()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM service_receipts WHERE state='accepted'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "invalid ACK must not advance execution");
            std::fs::remove_dir_all(&service.home).unwrap();
        }
    }

    #[tokio::test]
    async fn diagnostic_push_refuses_wrong_unenrolled_unbound_and_revoked_devices() {
        let (_directory, service, _) = push_fixture();
        assert!(
            service
                .test_push("wrong")
                .await
                .unwrap_err()
                .to_string()
                .contains("not configured")
        );
        assert!(
            service
                .test_push("phone-1")
                .await
                .unwrap_err()
                .to_string()
                .contains("not actively enrolled")
        );
        let key = service
            .devices
            .get("phone-1")
            .unwrap()
            .signing
            .to_sec1_bytes();
        service.journal.reserve_enrollment("phone-1", &key).unwrap();
        service.journal.activate_enrollment("phone-1").unwrap();
        assert!(
            service
                .test_push("phone-1")
                .await
                .unwrap_err()
                .to_string()
                .contains("no active push binding")
        );
        service
            .journal
            .set_push_preferences("phone-1", &URL_SAFE_NO_PAD.encode([4u8; 32]), None)
            .unwrap();
        // Another process revokes the local pairing; this Service's Auth is stale.
        Auth::open(&service.home)
            .unwrap()
            .revoke("phone-1")
            .unwrap();
        assert!(
            service
                .test_push("phone-1")
                .await
                .unwrap_err()
                .to_string()
                .contains("not locally approved")
        );
        std::fs::remove_dir_all(&service.home).unwrap();
    }

    #[tokio::test]
    async fn diagnostic_push_uses_encrypted_route_and_requires_exact_acceptance() {
        use axum::{
            Json, Router,
            routing::{get, post},
        };
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (_directory, mut service, phone_agreement) = push_fixture();
        activate_push(&service);
        let now = local_minutes();
        let time = |m: u32| format!("{:02}:{:02}", m / 60, m % 60);
        service
            .journal
            .set_push_preferences(
                "phone-1",
                &URL_SAFE_NO_PAD.encode([4u8; 32]),
                Some(QuietHours {
                    start: Some(time((now + 1380) % 1440)),
                    end: Some(time((now + 60) % 1440)),
                }),
            )
            .unwrap();
        let requests = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let seen = requests.clone();
        let status = Arc::new(AtomicUsize::new(502));
        let answer = status.clone();
        let router = Router::new()
            .route(
                "/v1/bootstrap",
                get(|| async { Json(json!({"epoch":URL_SAFE_NO_PAD.encode([1u8;16])})) }),
            )
            .route(
                "/v1/mac/push",
                post(move |Json(body): Json<Value>| {
                    let seen = seen.clone();
                    let answer = answer.clone();
                    async move {
                        seen.lock().unwrap().push(body);
                        axum::http::StatusCode::from_u16(answer.load(Ordering::SeqCst) as u16)
                            .unwrap()
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        assert!(
            service
                .test_push("phone-1")
                .await
                .unwrap_err()
                .to_string()
                .contains("502")
        );
        status.store(200, Ordering::SeqCst);
        assert!(
            service
                .test_push("phone-1")
                .await
                .unwrap_err()
                .to_string()
                .contains("200")
        );
        status.store(202, Ordering::SeqCst);
        service.test_push("phone-1").await.unwrap();
        let sent = requests.lock().unwrap();
        assert_eq!(sent.len(), 3);
        for body in sent.iter() {
            assert_eq!(body["device_id"], "phone-1");
            assert!(!body.to_string().contains("Prueba cifrada"));
            let envelope: Envelope = serde_json::from_value(body["envelope"].clone()).unwrap();
            let plaintext = envelope
                .open(
                    service.signing.verifying_key(),
                    &phone_agreement,
                    "mac-1",
                    "phone-1",
                    "mac-1",
                    now_ms(),
                )
                .unwrap();
            let content: Value = serde_json::from_slice(&plaintext).unwrap();
            assert_eq!(
                content["payload"]["aps"]["alert"]["title"],
                "Prueba cifrada de Alondra"
            );
            // ServiceNotificationDecoder requires a metadata dictionary before
            // adding the verified binding. An empty dictionary has no fake target/actions.
            assert_eq!(content["payload"]["alondra"], json!({}));
            assert_eq!(content["payload"]["aps"]["interruption-level"], "passive");
            assert_eq!(content["with_sound"], false);
            assert_eq!(content["push_binding"], URL_SAFE_NO_PAD.encode([4u8; 32]));
        }
        server.abort();
        std::fs::remove_dir_all(&service.home).unwrap();
    }

    #[tokio::test]
    async fn diagnostic_push_rechecks_binding_and_revocation_after_bootstrap() {
        use axum::{
            Json, Router,
            routing::{get, post},
        };
        use std::sync::atomic::{AtomicUsize, Ordering};
        for revoke in [false, true] {
            let (_directory, mut service, _) = push_fixture();
            activate_push(&service);
            let home = service.home.clone();
            let sent = Arc::new(AtomicUsize::new(0));
            let observed = sent.clone();
            let router = Router::new()
                .route(
                    "/v1/bootstrap",
                    get(move || {
                        let home = home.clone();
                        async move {
                            if revoke {
                                Auth::open(&home).unwrap().revoke("phone-1").unwrap();
                            } else {
                                Journal::open(home.join("service-receipts.sqlite"))
                                    .unwrap()
                                    .set_push_preferences(
                                        "phone-1",
                                        &URL_SAFE_NO_PAD.encode([9u8; 32]),
                                        None,
                                    )
                                    .unwrap();
                            }
                            Json(json!({"epoch":URL_SAFE_NO_PAD.encode([1u8;16])}))
                        }
                    }),
                )
                .route(
                    "/v1/mac/push",
                    post(move || {
                        let observed = observed.clone();
                        async move {
                            observed.fetch_add(1, Ordering::SeqCst);
                            axum::http::StatusCode::ACCEPTED
                        }
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            service.set_relay_url(
                Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap(),
            );
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let error = service.test_push("phone-1").await.unwrap_err().to_string();
            assert!(error.contains(if revoke {
                "not locally approved"
            } else {
                "binding changed"
            }));
            assert_eq!(sent.load(Ordering::SeqCst), 0);
            server.abort();
            std::fs::remove_dir_all(&service.home).unwrap();
        }
    }

    #[tokio::test]
    async fn domain_push_report_counts_relay_rejections() {
        let (_directory, mut service, _) = push_fixture();
        activate_push(&service);
        let router = Router::new()
            .route(
                "/v1/bootstrap",
                get(|| async { axum::Json(json!({"epoch":URL_SAFE_NO_PAD.encode([1u8;16])})) }),
            )
            .route(
                "/v1/mac/push",
                post(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let report = service
            .broadcast_push_report(&json!({"aps":{"alert":"hint"},"alondra":{}}), true)
            .await
            .unwrap();
        assert_eq!(report, (1, 0));
        server.abort();
        std::fs::remove_dir_all(&service.home).unwrap();
    }

    /// A phone that signed out, lost its session or was unlinked on the web is gone from the
    /// relay before the Mac hears of it: the relay's answer is what stops the Mac pushing.
    #[tokio::test]
    async fn the_mac_stops_pushing_to_a_phone_the_relay_no_longer_knows() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for (status, code, ended) in [
            (StatusCode::GONE, "push_registration_gone", true),
            (StatusCode::NOT_FOUND, "push_registration_not_found", false),
            (StatusCode::NOT_FOUND, "not_found", false),
            (StatusCode::SERVICE_UNAVAILABLE, "push_registration_failed", false),
        ] {
            let (_directory, mut service, _) = push_fixture();
            activate_push(&service);
            let calls = Arc::new(AtomicUsize::new(0));
            let counted = calls.clone();
            let router = Router::new()
                .route(
                    "/v1/bootstrap",
                    get(|| async { axum::Json(json!({"epoch":URL_SAFE_NO_PAD.encode([1u8;16])})) }),
                )
                .route(
                    "/v1/mac/push",
                    post(move || {
                        counted.fetch_add(1, Ordering::SeqCst);
                        async move { (status, axum::Json(json!({"error": code}))) }
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let payload = json!({"aps":{"alert":"hint"},"alondra":{}});
            assert_eq!(service.broadcast_push_report(&payload, true).await.unwrap(), (1, 0));
            assert_eq!(service.journal.push_binding("phone-1").unwrap().is_none(), ended, "{code}");
            let again = service.broadcast_push_report(&payload, true).await.unwrap();
            assert_eq!(again, if ended { (0, 0) } else { (1, 0) }, "{code}");
            assert_eq!(calls.load(Ordering::SeqCst), if ended { 1 } else { 2 }, "{code}");
            server.abort();
            std::fs::remove_dir_all(&service.home).unwrap();
        }
    }

    #[test]
    fn a_binding_the_phone_registered_after_the_relay_dropped_the_old_one_stays() {
        let (_directory, service, _) = push_fixture();
        activate_push(&service);
        let old = URL_SAFE_NO_PAD.encode([4u8; 32]);
        let new = URL_SAFE_NO_PAD.encode([5u8; 32]);
        service.journal.set_push_binding("phone-1", &new).unwrap();
        service.journal.forget_push_binding("phone-1", &old).unwrap();
        assert_eq!(service.journal.push_binding("phone-1").unwrap(), Some(new.clone()));
        service.journal.forget_push_binding("phone-1", &new).unwrap();
        assert_eq!(service.journal.push_binding("phone-1").unwrap(), None);
        std::fs::remove_dir_all(&service.home).unwrap();
    }

    #[test]
    fn a_cards_collapse_id_is_an_opaque_stable_header_per_phone() {
        let (first, second) = (URL_SAFE_NO_PAD.encode([1u8; 32]), URL_SAFE_NO_PAD.encode([2u8; 32]));
        let id = collapse_id(&first, "t-push-group-retract-result-1");
        assert_eq!(id, collapse_id(&first, "t-push-group-retract-result-1"), "the same card replaces its notification");
        assert_ne!(id, collapse_id(&first, "t-push-group-retract-result-2"));
        assert_ne!(id, collapse_id(&second, "t-push-group-retract-result-1"), "unlinkable across phones");
        assert!(id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')));
        assert!(!id.contains("push"), "APNs never reads the card id");
    }

    /// The relay on the Pi is deployed by hand and an older one refuses unknown fields: alerts
    /// still arrive without a collapse id, a background push fails without reaching the phone.
    #[tokio::test]
    async fn pushes_carry_collapse_id_or_background_type_and_alerts_survive_an_older_relay() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (_directory, mut service, phone_agreement) = push_fixture();
        activate_push(&service);
        let binding = URL_SAFE_NO_PAD.encode([5u8; 32]);
        service.journal.set_push_preferences("phone-1", &binding, None).unwrap();
        let requests = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let seen = requests.clone();
        let older = Arc::new(AtomicBool::new(false));
        let is_older = older.clone();
        let router = Router::new()
            .route(
                "/v1/bootstrap",
                get(|| async { axum::Json(json!({"epoch":URL_SAFE_NO_PAD.encode([1u8;16])})) }),
            )
            .route(
                "/v1/mac/push",
                post(move |axum::Json(body): axum::Json<Value>| {
                    let seen = seen.clone();
                    let is_older = is_older.clone();
                    async move {
                        let extended = body.get("collapse_id").is_some() || body.get("push_type").is_some();
                        seen.lock().unwrap().push(body);
                        if is_older.load(Ordering::SeqCst) && extended {
                            (StatusCode::BAD_REQUEST, axum::Json(json!({"error":"invalid_push"}))).into_response()
                        } else {
                            StatusCode::ACCEPTED.into_response()
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let card = json!({"aps":{"alert":{"title":"Approve"}},"alondra":{"card":"t-1-result-1"}});
        let retract = json!({"aps":{"badge":0},"alondra":{"kind":"retract","open":[]}});

        assert_eq!(service.broadcast_push_report(&card, true).await.unwrap(), (1, 1));
        assert_eq!(service.broadcast_background_push(|_| retract.clone()).await.unwrap(), (1, 1));
        assert_eq!(service.broadcast_badge_push(2).await.unwrap(), (1, 1));
        {
            let sent = requests.lock().unwrap();
            assert_eq!(sent[2]["push_type"], "badge");
            assert_eq!(sent[2]["badge"], 2, "the count the relay hands APNs in the clear");
            assert!(sent[2].get("collapse_id").is_none());
            assert_eq!(sent[0]["collapse_id"], collapse_id(&binding, "t-1-result-1"));
            assert!(sent[0].get("push_type").is_none());
            assert_eq!(sent[1]["push_type"], "background");
            assert!(sent[1].get("collapse_id").is_none());
            let envelope: Envelope = serde_json::from_value(sent[1]["envelope"].clone()).unwrap();
            let plaintext: Value = serde_json::from_slice(&envelope
                .open(service.signing.verifying_key(), &phone_agreement, "mac-1", "phone-1", "mac-1", now_ms())
                .unwrap()).unwrap();
            assert_eq!(plaintext["payload"], retract, "quiet hours never touch a background push");
        }

        older.store(true, Ordering::SeqCst);
        requests.lock().unwrap().clear();
        assert_eq!(service.broadcast_push_report(&card, true).await.unwrap(), (1, 1), "the alert still arrives");
        assert_eq!(service.broadcast_background_push(|_| retract.clone()).await.unwrap(), (1, 0));
        assert_eq!(service.broadcast_badge_push(2).await.unwrap(), (1, 0));
        {
            let sent = requests.lock().unwrap();
            assert_eq!(sent.len(), 4, "one retry for the alert, none for the background or badge push: as an alert it would show a banner");
            assert!(sent[1].get("collapse_id").is_none());
            assert_eq!(sent[0]["envelope"], sent[1]["envelope"], "the retry resends the same sealed push");
        }
        server.abort();
        std::fs::remove_dir_all(&service.home).unwrap();
    }

    fn changes_work(service: &Service, request_id: &str, request: Value) -> Work {
        let envelope = Envelope::seal_random(
            EnvelopeContext {
                route: "mac-1",
                sender: "phone-1",
                recipient: "mac-1",
                request_id,
                kind: "request",
                expires_at_ms: now_ms() + 600_000,
            },
            &serde_json::to_vec(&request).unwrap(),
            &SigningKey::from_slice(&[33u8; 32]).unwrap(),
            &service.agreement.public_key(),
        )
        .unwrap();
        Work {
            request_id: request_id.into(),
            envelope,
        }
    }

    fn repeat_work(work: &Work) -> Work {
        Work {
            request_id: work.request_id.clone(),
            envelope: work.envelope.clone(),
        }
    }

    #[test]
    fn completed_domain_read_is_not_redelivered_after_persisted_revocation() {
        let (_directory, service, _) = push_fixture();
        activate_push(&service);
        let home = service.home.clone();
        let config = alondra_core::config::Config::resolve(
            home.clone(),
            &Default::default(),
            |_| None,
            || Some("Fixture".into()),
        );
        let hub = Arc::new(crate::hub::Hub::open(config).unwrap());
        let service = service.with_hub(hub);
        let work = changes_work(
            &service,
            "domain-read-before-revoke",
            json!({"method":"GET","path":"/capabilities"}),
        );
        let prepared = service.prepare_work(&work, Transport::Relay).unwrap().unwrap();
        let digest = prepared.digest;
        drop(prepared);
        assert!(matches!(
            service
                .journal
                .accept("phone-1", &work.request_id, &digest)
                .unwrap(),
            Disposition::Accepted
        ));
        service
            .journal
            .executing("phone-1", &work.request_id, &digest)
            .unwrap();
        service
            .journal
            .complete("phone-1", &work.request_id, b"cached ciphertext")
            .unwrap();
        assert!(matches!(
            service
                .journal
                .accept("phone-1", &work.request_id, &digest)
                .unwrap(),
            Disposition::Completed(_)
        ));
        Auth::open(&home).unwrap().revoke("phone-1").unwrap();
        assert!(
            service.prepare_work(&work, Transport::Relay).is_err(),
            "cached ciphertext must not reach revoked devices"
        );
        drop(service);
        std::fs::remove_dir_all(home).unwrap();
    }

    fn opened_results(
        service: &Service,
        phone: &SecretKey,
        responses: &Mutex<Vec<Vec<u8>>>,
    ) -> Vec<Value> {
        responses
            .lock()
            .unwrap()
            .iter()
            .map(|bytes| {
                let envelope: Envelope = serde_json::from_slice(bytes).unwrap();
                let bytes = envelope
                    .open(
                        service.signing.verifying_key(),
                        phone,
                        "mac-1",
                        "phone-1",
                        "mac-1",
                        now_ms(),
                    )
                    .unwrap();
                serde_json::from_slice(&bytes).unwrap()
            })
            .collect()
    }

    #[tokio::test]
    async fn a_domain_service_registers_the_phones_push_preferences_and_the_legacy_one_refuses_the_route()
     {
        let (_directory, service, phone_agreement) = push_fixture();
        let phone_signing = SigningKey::from_slice(&[33u8; 32]).unwrap();
        let home = service.home.clone();
        let sealed = |service: &Service, request_id: &str, path: &str, body: Value| Work {
            request_id: request_id.into(),
            envelope: Envelope::seal_random(
                EnvelopeContext {
                    route: "mac-1",
                    sender: "phone-1",
                    recipient: "mac-1",
                    request_id,
                    kind: "request",
                    expires_at_ms: now_ms() + 60_000,
                },
                serde_json::to_vec(
                    &json!({"method":"POST","path":path,"body":body}),
                )
                .unwrap()
                .as_slice(),
                &phone_signing,
                &service.agreement.public_key(),
            )
            .unwrap(),
        };
        let binding = URL_SAFE_NO_PAD.encode([4u8; 32]);
        service
            .journal
            .reserve_enrollment("phone-1", &phone_signing.verifying_key().to_sec1_bytes())
            .unwrap();
        service.journal.activate_enrollment("phone-1").unwrap();
        assert!(
            service
                .prepare_work(&sealed(
                    &service,
                    "legacy", PUSH_BINDING_ROUTE,
                    json!({"push_binding": binding})
                ), Transport::Relay)
                .is_err(),
            "the legacy daemon does not serve domain routes"
        );
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let service = service.with_domain(domain);
        let send = |request_id: &'static str, path: &'static str, body: Value| {
            let service = &service;
            let phone_agreement = &phone_agreement;
            let work = sealed(service, request_id, path, body);
            async move {
                let prepared = service.prepare_work(&work, Transport::Relay).unwrap().unwrap();
                assert!(
                    service.atomic_local(&prepared.request),
                    "a registration commits atomically with its receipt"
                );
                service
                    .journal
                    .accept("phone-1", request_id, &prepared.digest)
                    .unwrap();
                service
                    .journal
                    .executing_local("phone-1", request_id, &prepared.digest)
                    .unwrap();
                let device = service.devices.get("phone-1").unwrap();
                let bytes = service
                    .execute_atomic_local(
                        &work.envelope,
                        &device,
                        &prepared.request,
                        &prepared.digest, Transport::Relay,
                        service.journal.reserve_receipt(prepared.guard.clone()).unwrap(),
                    )
                    .await
                    .unwrap();
                let answer: Envelope = serde_json::from_slice(&bytes).unwrap();
                serde_json::from_slice::<Value>(
                    &answer
                        .open(
                            service.signing.verifying_key(),
                            phone_agreement,
                            "mac-1",
                            "phone-1",
                            "mac-1",
                            now_ms(),
                        )
                        .unwrap(),
                )
                .unwrap()
            }
        };
        let register = |request_id: &'static str, body: Value| send(request_id, PUSH_BINDING_ROUTE, body);
        let first = register("first", json!({"push_binding": binding, "preferences_revision": 2, "quiet_start": "23:00", "quiet_end": "08:00"})).await;
        assert_eq!(first["status"], 204, "{first}");
        assert_eq!(
            service.journal.push_binding("phone-1").unwrap().as_deref(),
            Some(binding.as_str())
        );
        let stale = register("stale", json!({"push_binding": URL_SAFE_NO_PAD.encode([9u8; 32]), "preferences_revision": 1, "quiet_start": "22:00", "quiet_end": "07:00"})).await;
        assert_eq!(
            (
                stale["status"].as_i64(),
                stale["kind"].as_str(),
                stale["current_revision"].as_i64()
            ),
            (Some(409), Some("stale_push_preferences"), Some(2)),
            "{stale}"
        );
        assert_eq!(
            service.journal.push_binding("phone-1").unwrap().as_deref(),
            Some(binding.as_str()),
            "a stale registration changes nothing"
        );
        let refused = register(
            "invalid",
            json!({"push_binding": binding, "unexpected": true}),
        )
        .await;
        assert_eq!(refused["status"], 422, "{refused}");
        let unnamed = send("remove-unnamed", PUSH_BINDING_REMOVE_ROUTE, json!({})).await;
        assert_eq!(unnamed["status"], 422, "{unnamed}");
        let other = send("remove-other", PUSH_BINDING_REMOVE_ROUTE, json!({"push_binding": URL_SAFE_NO_PAD.encode([9u8; 32])})).await;
        assert_eq!(other["status"], 204, "{other}");
        assert_eq!(
            service.journal.push_binding("phone-1").unwrap().as_deref(),
            Some(binding.as_str()),
            "removing an older binding keeps the one registered since"
        );
        let removed = send("remove", PUSH_BINDING_REMOVE_ROUTE, json!({"push_binding": binding})).await;
        assert_eq!(removed["status"], 204, "{removed}");
        assert_eq!(service.journal.push_binding("phone-1").unwrap(), None);
        assert_eq!(
            service.journal.quiet_hours("phone-1").unwrap().start.as_deref(),
            Some("23:00"),
            "signing out keeps the phone's quiet hours for when it signs back in"
        );
        let again = send("remove-again", PUSH_BINDING_REMOVE_ROUTE, json!({"push_binding": binding})).await;
        assert_eq!(again["status"], 204, "{again}");
        assert!(
            approved_method(&LocalRequest {
                method: "POST".into(),
                path: PUSH_BINDING_ROUTE.into(),
                body: None
            })
            .is_err(),
            "it needs its body"
        );
        assert!(
            approved_method(&LocalRequest {
                method: "GET".into(),
                path: PUSH_BINDING_ROUTE.into(),
                body: None
            })
            .is_err()
        );
        drop(service);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_message_to_an_agent_is_a_relayed_mutation_with_a_body() {
        let post = |path: &str, body: Option<Value>| LocalRequest {
            method: "POST".into(),
            path: path.into(),
            body,
        };
        assert!(
            approved_method(&post(
                "/agents/agent-a/messages",
                Some(json!({"text": "hi"}))
            ))
            .is_ok()
        );
        assert!(approved_method(&post("/agents/agent-a/messages", None)).is_err());
        for path in [
            "/agents//messages",
            "/agents/a/b/messages",
            "/agents/../messages",
            "/agents/agent-a/other",
        ] {
            assert!(
                approved_method(&post(path, Some(json!({"text": "hi"})))).is_err(),
                "{path}"
            );
        }
        assert!(
            approved_method(&LocalRequest {
                method: "GET".into(),
                path: "/agents/agent-a/messages".into(),
                body: None
            })
            .is_err()
        );
    }

    #[test]
    fn an_orchestrator_decision_is_a_relayed_mutation_with_a_body() {
        let request = |method: &str, path: &str, body: Option<Value>| LocalRequest {
            method: method.into(),
            path: path.into(),
            body,
        };
        for path in [
            "/orchestrator/cancel",
            "/orchestrator/acknowledge",
            "/orchestrator/retry",
        ] {
            assert!(
                approved_method(&request("POST", path, Some(json!({"run_id": "run-7"})))).is_ok(),
                "{path}"
            );
            assert!(
                approved_method(&request("POST", path, None)).is_err(),
                "{path}"
            );
            assert!(
                approved_method(&request("GET", path, None)).is_err(),
                "{path}"
            );
        }
        assert!(
            approved_method(&request(
                "POST",
                "/orchestrator/restart",
                Some(json!({"run_id": "r"}))
            ))
            .is_err()
        );
    }

    #[test]
    fn changes_queries_are_bounded_and_unambiguous() {
        for path in [
            "/v1/changes",
            "/v1/changes?after=0",
            "/v1/changes?wait=0",
            "/v1/changes?after=18446744073709551615&wait=50",
            "/v1/changes?wait=1&after=2",
        ] {
            assert!(
                approved_method(&LocalRequest {
                    method: "GET".into(),
                    path: path.into(),
                    body: None
                })
                .is_ok(),
                "{path}"
            );
        }
        for query in [
            "",
            "after=-1",
            "after=18446744073709551616",
            "after=1.0",
            "after=+1",
            "after=%31",
            "%61fter=1",
            "wait=51",
            "wait=-1",
            "wait=",
            "wait=1&wait=2",
            "after=1&after=2",
            "after=0&other=1",
            "after=0&",
            "after=1;wait=2",
        ] {
            assert!(
                approved_method(&LocalRequest {
                    method: "GET".into(),
                    path: format!("/v1/changes?{query}"),
                    body: None
                })
                .is_err(),
                "{query}"
            );
        }
        for method in ["POST", "DELETE"] {
            assert!(
                approved_method(&LocalRequest {
                    method: method.into(),
                    path: "/v1/changes".into(),
                    body: None
                })
                .is_err()
            );
        }
        assert!(
            approved_method(&LocalRequest {
                method: "GET".into(),
                path: "/v1/changes".into(),
                body: Some(json!({}))
            })
            .is_err()
        );
    }

    #[test]
    fn retired_domain_reads_are_not_relay_approved() {
        for path in [
            "/summary", "/inbox", "/inbox/i-1", "/tasks", "/tasks/t-1/interactions",
            "/interactions", "/interactions/i-1", "/interactions/i-1/body?offset=0",
            "/requests", "/requests/r-1", "/requests/r-1/body?offset=0",
            "/agents", "/pull-requests", "/pull-requests/pr-1", "/documents",
            "/documents/d-1", "/merge-authorizations", "/merge-authorizations/auth-1",
            "/stack-readiness", "/superseded-acceptances", "/tasks/t-1/actions",
            "/runtime",
            "/changes?data_epoch=epoch&after=0&wait=0",
        ] {
            assert!(approved_method(&LocalRequest {
                method: "GET".into(), path: path.into(), body: None,
            }).is_err(), "retired read remained approved: {path}");
        }
    }

    #[tokio::test]
    async fn changes_wait_does_not_block_mutation_or_misclassify_live_redelivery() {
        let (_directory, mut service, phone) = push_fixture();
        activate_push(&service);
        let entered = Arc::new(tokio::sync::Semaphore::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let reads = Arc::new(AtomicUsize::new(0));
        let writes = Arc::new(AtomicUsize::new(0));
        let local = Router::new()
            .route(
                "/v1/changes",
                get({
                    let entered = entered.clone();
                    let release = release.clone();
                    let reads = reads.clone();
                    move || {
                        let entered = entered.clone();
                        let release = release.clone();
                        let reads = reads.clone();
                        async move {
                            reads.fetch_add(1, Ordering::SeqCst);
                            entered.add_permits(1);
                            release.acquire().await.unwrap().forget();
                            axum::Json(json!({"tick":42}))
                        }
                    }
                }),
            )
            .route(
                "/v1/questions/1/answer",
                post({
                    let writes = writes.clone();
                    move || {
                        let writes = writes.clone();
                        async move {
                            writes.fetch_add(1, Ordering::SeqCst);
                            axum::Json(json!({"saved":true}))
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        service.local_port = listener.local_addr().unwrap().port();
        let local_server = tokio::spawn(axum::serve(listener, local).into_future());
        let responses = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let relay = Router::new()
            .route(
                "/v1/work/{id}/receipt",
                post(|| async { StatusCode::ACCEPTED }),
            )
            .route(
                "/v1/work/{id}/response",
                post({
                    let responses = responses.clone();
                    move |body: Bytes| {
                        let responses = responses.clone();
                        async move {
                            responses.lock().unwrap().push(body.to_vec());
                            StatusCode::ACCEPTED
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
        let relay_server = tokio::spawn(axum::serve(listener, relay).into_future());
        let service = Arc::new(service);
        let mut tasks = tokio::task::JoinSet::new();
        let epoch = [7; 16];
        let request = json!({"method":"GET","path":"/v1/changes?after=0&wait=50"});
        let first = changes_work(&service, "wait-1", request.clone());
        service
            .schedule_work(repeat_work(&first), epoch, &mut tasks)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(20), entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        // This is exactly the relay's same-envelope lease retry, while the local
        // HTTP read is suspended. It must neither execute again nor write 409.
        service
            .schedule_work(repeat_work(&first), epoch, &mut tasks)
            .await
            .unwrap();
        assert!(responses.lock().unwrap().is_empty());
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        let digest = Sha256::digest(serde_json::to_vec(&first.envelope).unwrap()).into();
        assert!(matches!(
            service
                .journal
                .accept("phone-1", "wait-1", &digest)
                .unwrap(),
            Disposition::Executing
        ));
        let altered = changes_work(&service, "wait-1", request.clone());
        assert!(
            service
                .schedule_work(altered, epoch, &mut tasks)
                .await
                .is_err()
        );
        service
            .schedule_work(
                changes_work(&service, "wait-2", request.clone()),
                epoch,
                &mut tasks,
            )
            .await
            .unwrap();
        assert_eq!(
            opened_results(&service, &phone, &responses)[0],
            json!({"status":429,"error":"changes_busy"})
        );
        tokio::time::timeout(
            Duration::from_secs(20),
            service.schedule_work(
                changes_work(&service, "wait-full", request.clone()),
                epoch,
                &mut tasks,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            opened_results(&service, &phone, &responses)[0],
            json!({"status":429,"error":"changes_busy"})
        );
        tokio::time::timeout(
            Duration::from_secs(20),
            service.schedule_work(
                changes_work(
                    &service,
                    "write-1",
                    json!({"method":"POST","path":"/v1/questions/1/answer","body":{"text":"yes"}}),
                ),
                epoch,
                &mut tasks,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        tasks.join_next().await.unwrap().unwrap().unwrap();
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "the occupied read slot leaves mutations available"
        );
        service
            .schedule_work(
                changes_work(
                    &service,
                    "bad-query",
                    json!({"method":"GET","path":"/v1/changes?wait=51"}),
                ),
                epoch,
                &mut tasks,
            )
            .await
            .unwrap();
        tasks.join_next().await.unwrap().unwrap().unwrap();
        assert_eq!(tasks.len(), 1, "the original changes read remains held while ordinary replies complete");
        assert!(
            opened_results(&service, &phone, &responses)
                .contains(&json!({"status":400,"error":"invalid_changes_query"}))
        );
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        release.add_permits(1);
        while let Some(result) = tasks.join_next().await {
            result.unwrap().unwrap();
        }
        assert!(service.inflight.lock().unwrap().is_empty());
        let results = opened_results(&service, &phone, &responses);
        assert_eq!(
            results
                .iter()
                .filter(|value| value["body"]["tick"] == 42)
                .count(),
            1
        );
        assert!(!results.iter().any(|value| value["error"] == "uncertain"));

        let interrupted = changes_work(&service, "interrupted", request);
        service
            .schedule_work(repeat_work(&interrupted), epoch, &mut tasks)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(20), entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        assert!(
            service.inflight.lock().unwrap().is_empty(),
            "cancellation releases the admission guard"
        );
        let mut restarted = Service::open(
            &service.home,
            service.local_port,
            Arc::new(Auth::open(&service.home).unwrap()),
        )
        .unwrap()
        .unwrap();
        restarted.set_relay_url(service.relay_url());
        // The aborted read's server handler may still wait for a permit: release one for it
        // and one for the restarted read, or it can take the only one and starve the read.
        release.add_permits(2);
        restarted.handle(interrupted, &epoch).await.unwrap();
        assert_eq!(
            opened_results(&service, &phone, &responses).last().unwrap(),
            &json!({"status":200,"body":{"tick":42}}),
            "a read the old process left executing is answered, not reported uncertain"
        );
        assert_eq!(reads.load(Ordering::SeqCst), 3, "the restarted daemon runs the read again");
        local_server.abort();
        relay_server.abort();
        std::fs::remove_dir_all(&service.home).unwrap();
    }

    #[tokio::test]
    async fn changes_global_limit_releases_on_drop_and_does_not_limit_mutations() {
        let (_directory, service, _) = push_fixture();
        activate_push(&service);
        let first = changes_work(
            &service,
            "first-device-waiter",
            json!({"method":"GET","path":"/v1/changes"}),
        );
        let first = service.prepare_work(&first, Transport::Relay).unwrap().unwrap();
        assert!(first.guard.changes);
        let second = changes_work(
            &service,
            "second-device-waiter",
            json!({"method":"GET","path":"/v1/changes"}),
        );
        let second = service.prepare_work(&second, Transport::Relay).unwrap().unwrap();
        assert!(second.guard.saturated, "one device may keep only one legacy changes waiter");
        drop(second);
        drop(first);
        for n in 0..MAX_CHANGES_TOTAL {
            service
                .inflight
                .lock()
                .unwrap()
                .insert((format!("other-{n}"), "read".into()), ([0; 32], true));
        }
        let read = changes_work(
            &service,
            "bounded",
            json!({"method":"GET","path":"/v1/changes"}),
        );
        let prepared = service.prepare_work(&read, Transport::Relay).unwrap().unwrap();
        assert!(prepared.guard.saturated);
        assert!(!prepared.guard.changes);
        drop(prepared);
        assert_eq!(service.inflight.lock().unwrap().len(), MAX_CHANGES_TOTAL);
        let mutation = changes_work(
            &service,
            "write",
            json!({"method":"POST","path":"/v1/questions/1/answer","body":{}}),
        );
        let prepared = service.prepare_work(&mutation, Transport::Relay).unwrap().unwrap();
        assert!(!prepared.guard.saturated);
        drop(prepared);
        service.inflight.lock().unwrap().clear();
        assert!(service.prepare_work(&read, Transport::Relay).unwrap().unwrap().guard.changes);
        assert!(service.inflight.lock().unwrap().is_empty());
        std::fs::remove_dir_all(&service.home).unwrap();
    }

    // The transport is deliberately dishonest: its device registry and HTTP
    // authorization cannot be trusted as the Mac's content authorization.
    #[tokio::test]
    async fn malicious_relay_key_substitution_and_false_revocation_ack_cannot_dispatch() {
        let (_directory, mut service, phone) = push_fixture();
        activate_push(&service);
        let home = service.home.clone();
        let config = alondra_core::config::Config::resolve(
            home,
            &Default::default(),
            |_| None,
            || Some("Synthetic".into()),
        );
        let hub = Arc::new(crate::hub::Hub::open(config).unwrap());
        service = service.with_hub(hub);
        let attacker = SigningKey::from_slice(&[71; 32]).unwrap();
        let trusted = SigningKey::from_slice(&[33; 32]).unwrap();
        let seal = |key: &SigningKey, id: &str| {
            Envelope::seal_random(
                EnvelopeContext {
                    route: "mac-1",
                    sender: "phone-1",
                    recipient: "mac-1",
                    request_id: id,
                    kind: "request",
                    expires_at_ms: now_ms() + 60_000,
                },
                br#"{"method":"GET","path":"/capabilities"}"#,
                key,
                &service.agreement.public_key(),
            )
            .unwrap()
        };
        let substituted = seal(&attacker, "substituted-key");
        let previously_trusted = seal(&trusted, "after-revocation");
        let delivered = Arc::new(Mutex::new(json!({
            "request_id":"positive-control", "envelope":seal(&trusted, "positive-control")
        })));
        let work_reads = Arc::new(AtomicUsize::new(0));
        let deletes = Arc::new(AtomicUsize::new(0));
        let receipts = Arc::new(AtomicUsize::new(0));
        let responses = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let relay = Router::new()
            .route(
                "/v1/work/{id}/response",
                post({
                    let responses = responses.clone();
                    move |body: Bytes| {
                        let responses = responses.clone();
                        async move {
                            responses.lock().unwrap().push(body.to_vec());
                            StatusCode::ACCEPTED
                        }
                    }
                }),
            )
            .route(
                "/v1/bootstrap",
                get(|| async { axum::Json(json!({"epoch":URL_SAFE_NO_PAD.encode([7;16])})) }),
            )
            .route(
                "/v1/work",
                get({
                    let data = delivered.clone();
                    let reads = work_reads.clone();
                    move || {
                        let data = data.clone();
                        let reads = reads.clone();
                        async move {
                            reads.fetch_add(1, Ordering::SeqCst);
                            axum::Json(data.lock().unwrap().clone())
                        }
                    }
                }),
            )
            // Lie about deletion: retain and redeliver the revoked device's work.
            .route(
                "/v1/devices/{id}",
                delete({
                    let deletes = deletes.clone();
                    move || {
                        let deletes = deletes.clone();
                        async move {
                            deletes.fetch_add(1, Ordering::SeqCst);
                            StatusCode::NO_CONTENT
                        }
                    }
                }),
            )
            .route(
                "/v1/work/{id}/receipt",
                post({
                    let receipts = receipts.clone();
                    move || {
                        let receipts = receipts.clone();
                        async move {
                            receipts.fetch_add(1, Ordering::SeqCst);
                            StatusCode::ACCEPTED
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        service.set_relay_url(Url::parse(&origin).unwrap());
        let relay_server = tokio::spawn(axum::serve(listener, relay).into_future());
        let home = service.home.clone();
        let service = Arc::new(service);
        let mut tasks = tokio::task::JoinSet::new();
        service
            .poll_work(&mut None, &mut HashSet::new(), &mut tasks)
            .await
            .unwrap();
        while let Some(result) = tasks.join_next().await { result.unwrap().unwrap(); }
        let results = opened_results(&service, &phone, &responses);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0]["status"], 200,
            "the resource endpoint accepts an approved signed request"
        );
        assert_eq!(results[0]["body"]["protocol_version"], 2);
        assert_eq!(receipts.load(Ordering::SeqCst), 1);
        *delivered.lock().unwrap() = json!({"request_id":"substituted-key","envelope":substituted});
        let error = service
            .poll_work(&mut None, &mut HashSet::new(), &mut tasks)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("invalid encrypted work"));
        assert_eq!(work_reads.load(Ordering::SeqCst), 2);
        assert_eq!(
            responses.lock().unwrap().len(),
            1,
            "invalid relay key never gets a response receipt"
        );
        assert_eq!(receipts.load(Ordering::SeqCst), 1);
        // Persist revocation, then construct a fresh daemon from disk. The
        // service TOML still contains the old device, just as a stale registry can.
        Auth::open(&home).unwrap().revoke("phone-1").unwrap();
        drop(service);
        *delivered.lock().unwrap() =
            json!({"request_id":"after-revocation","envelope":previously_trusted});
        let mut restarted = Service::open(&home, 0, Arc::new(Auth::open(&home).unwrap()))
            .unwrap()
            .unwrap();
        restarted.set_relay_url(Url::parse(&origin).unwrap());
        let restarted = Arc::new(restarted);
        assert!(
            restarted
                .poll_work(&mut None, &mut HashSet::new(), &mut tasks)
                .await
                .is_err()
        );
        assert_eq!(
            deletes.load(Ordering::SeqCst),
            1,
            "dishonest relay acknowledged deletion"
        );
        assert_eq!(
            work_reads.load(Ordering::SeqCst),
            3,
            "revoked work actually redelivered after restart"
        );
        assert_eq!(
            responses.lock().unwrap().len(),
            1,
            "revoked device never gets a response receipt"
        );
        assert_eq!(receipts.load(Ordering::SeqCst), 1);
        assert!(restarted.journal.enrollment_revoked("phone-1").unwrap());
        relay_server.abort();
        drop(restarted);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn changes_rechecks_persisted_revocation_after_receipt_before_dispatch() {
        let (_directory, mut service, _) = push_fixture();
        activate_push(&service);
        let received = Arc::new(tokio::sync::Semaphore::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let local_calls = Arc::new(AtomicUsize::new(0));
        let local = Router::new().route(
            "/v1/changes",
            get({
                let calls = local_calls.clone();
                move || {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        axum::Json(json!({"tick":1}))
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        service.local_port = listener.local_addr().unwrap().port();
        let local_server = tokio::spawn(axum::serve(listener, local).into_future());
        let relay = Router::new().route(
            "/v1/work/{id}/receipt",
            post({
                let received = received.clone();
                let release = release.clone();
                move || {
                    let received = received.clone();
                    let release = release.clone();
                    async move {
                        received.add_permits(1);
                        release.acquire().await.unwrap().forget();
                        StatusCode::ACCEPTED
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
        let relay_server = tokio::spawn(axum::serve(listener, relay).into_future());
        let service = Arc::new(service);
        let mut tasks = tokio::task::JoinSet::new();
        service
            .schedule_work(
                changes_work(
                    &service,
                    "revoke",
                    json!({"method":"GET","path":"/v1/changes?wait=0"}),
                ),
                [7; 16],
                &mut tasks,
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), received.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        Auth::open(&service.home)
            .unwrap()
            .revoke("phone-1")
            .unwrap();
        release.add_permits(1);
        let result = tokio::time::timeout(Duration::from_secs(2), tasks.join_next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        assert_eq!(local_calls.load(Ordering::SeqCst), 0);
        assert!(service.inflight.lock().unwrap().is_empty());
        local_server.abort();
        relay_server.abort();
        std::fs::remove_dir_all(&service.home).unwrap();
    }

    #[test]
    fn local_dispatch_allowlist_rejects_path_tricks() {
        let accepts = |method: &str, path: &str, body: Option<Value>| {
            approved_method(&LocalRequest {
                method: method.into(),
                path: path.into(),
                body,
            })
            .is_ok()
        };
        for path in [
            "/v1/tasks/task-1",
            "/v1/nodes/session_1/activity",
            "/v1/prs/7/diff",
            "/v1/prs/7/diff-snapshot",
            "/v1/search?q=hello%20world",
            "/v1/file?path=%2FUsers%2Fme%2Ffile.txt",
            "/v1/options?project=foo",
        ] {
            assert!(accepts("GET", path, None), "{path}");
        }
        assert!(accepts("GET", "/views/home?protocol=1&client=ios&lang=en", None));
        assert!(accepts("GET", "/delivery-attempts?after=0", None));
        assert!(accepts(
            "POST",
            "/delivery-attempts/1/resolve",
            Some(json!({}))
        ));
        assert!(!accepts("GET", "/delivery-attempts?after=0&all=1", None));
        assert!(!accepts(
            "POST",
            "/delivery-attempts/../resolve",
            Some(json!({}))
        ));
        for path in [
            "/views/home?protocol=1&client=ios&lang=en",
            "/views/card:i-1?protocol=1&client=ios&lang=en",
        ] {
            assert!(accepts("GET", path, None), "{path}");
            assert!(!accepts("GET", path, Some(json!({}))), "{path}");
            assert!(!accepts("POST", path, None), "{path}");
        }
        assert!(!accepts("GET", "/inbox?filter=all", None));
        assert!(!accepts("GET", "/interactions", None));
        assert!(!accepts("GET", "/summary?include=inbox,tasks", None));
        for path in [
            "/v1/questions/123/answer",
            "/v1/questions/123/more-context",
            "/v1/prs/7/merge",
            "/v1/prs/7/changes",
            "/v1/prs/7/ask",
            "/v1/prs/7/resolve-conflicts",
            "/v1/prs/7/dismiss",
            "/v1/tasks/task_1/cancel",
            "/v1/tasks/archive",
            "/v1/requests",
            "/v1/nodes/session-1/messages",
            "/v1/nodes/session-1/close",
            "/v1/schedules/schedule-1/run",
            "/v1/reports/1/review",
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
            ("GET", "/v1/overview", None),
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
        let scratch_directory = crate::test_support::Directory::new();
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
        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-file-contract-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let service = Service {
            relay: Some(relay::RelayConnector::new(Url::parse("http://127.0.0.1:1/").unwrap(), "owner".into())),
            direct: DirectConfig::default(),
            lane: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(test)]
            envelope_clock: None,
            client: Client::new(),
            mac_id: "mac-test".into(),
            signing: SigningKey::from_slice(&[1u8; 32]).unwrap(),
            agreement: SecretKey::from_slice(&[2u8; 32]).unwrap(),
            devices: HashMap::new().into(),
            home: home.clone(),
            transfers: Arc::default(),
            inflight: Arc::default(),
            hub: None,
            domain: None,
            routes: None,
            revoked: crate::revoked_devices::RevokedDevices::empty(),
            auth: Arc::new(Auth::open(&home).unwrap()),
            journal: Journal::open(home.join("receipts.sqlite")).unwrap(),
            local_port,
        };
        for (file, text) in [
            ("json.md", JSON_FILE),
            ("markdown.md", MARKDOWN_FILE),
            ("scalar.md", "null"),
        ] {
            let response = service
                .dispatch(
                    "client-test",
                    &LocalRequest {
                        method: "GET".into(),
                        path: format!("/v1/file?path={file}"),
                        body: None,
                    }, Transport::Relay,
                )
                .await
                .unwrap();
            assert_eq!(
                response,
                json!({"status":200,"body":{"text":text}}),
                "{file}"
            );
        }
        for (path, expected) in [
            (
                "/v1/file?path=missing.md",
                json!({"status":404,"body":{"error":"file_not_found"}}),
            ),
            (
                "/v1/prs/7/diff-snapshot",
                json!({"status":200,"body":{"diff":"diff --git a/file b/file\n","head_sha":"abc","truncated":false}}),
            ),
        ] {
            assert_eq!(
                service
                    .dispatch(
                        "client-test",
                        &LocalRequest {
                            method: "GET".into(),
                            path: path.into(),
                            body: None,
                        }, Transport::Relay
                    )
                    .await
                    .unwrap(),
                expected
            );
        }
        task.abort();
        drop(service);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn oversized_diff_snapshot_is_cut_to_the_relay_budget_instead_of_failing() {
        let scratch_directory = crate::test_support::Directory::new();
        let large = "+".repeat(MAX_LOCAL_RESPONSE * 4);
        let local = large.clone();
        let router = Router::new().route(
            "/v1/prs/7/diff-snapshot",
            get(move || {
                let diff = local.clone();
                async move {
                    axum::Json(json!({"diff":diff,"head_sha":"a".repeat(40),"truncated":false}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(axum::serve(listener, router).into_future());
        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-diff-budget-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let service = Service {
            relay: Some(relay::RelayConnector::new(Url::parse("http://127.0.0.1:1/").unwrap(), "owner".into())),
            direct: DirectConfig::default(),
            lane: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(test)]
            envelope_clock: None,
            client: Client::new(),
            mac_id: "mac-test".into(),
            signing: SigningKey::from_slice(&[1u8; 32]).unwrap(),
            agreement: SecretKey::from_slice(&[2u8; 32]).unwrap(),
            devices: HashMap::new().into(),
            home: home.clone(),
            transfers: Arc::default(),
            inflight: Arc::default(),
            hub: None,
            domain: None,
            routes: None,
            revoked: crate::revoked_devices::RevokedDevices::empty(),
            auth: Arc::new(Auth::open(&home).unwrap()),
            journal: Journal::open(home.join("receipts.sqlite")).unwrap(),
            local_port,
        };
        let response = service
            .dispatch(
                "client-test",
                &LocalRequest {
                    method: "GET".into(),
                    path: "/v1/prs/7/diff-snapshot".into(),
                    body: None,
                }, Transport::Relay,
            )
            .await
            .unwrap();
        assert_eq!(response["status"], 200);
        assert_eq!(response["body"]["truncated"], true);
        assert_eq!(response["body"]["head_sha"], "a".repeat(40));
        let diff = response["body"]["diff"].as_str().unwrap();
        assert!(!diff.is_empty() && large.starts_with(diff));
        assert!(serde_json::to_vec(&response).unwrap().len() <= MAX_LOCAL_RESPONSE);
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
        let exact = client
            .get(format!("http://127.0.0.1:{port}/exact"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            bounded_response(exact).await.unwrap().len(),
            MAX_LOCAL_RESPONSE
        );
        let large = client
            .get(format!("http://127.0.0.1:{port}/large"))
            .send()
            .await
            .unwrap();
        let oversized = bounded_response(large).await.unwrap_err();
        assert!(oversized.is::<ResponseTooLarge>());
        assert!(oversized.to_string().contains("encrypted relay limit"));
        task.abort();
    }
    #[derive(Default)]
    struct CutExternal {
        merges: AtomicUsize,
        retargets: AtomicUsize,
        removes: AtomicUsize,
    }
    impl crate::prs::Gh for CutExternal {
        async fn merge(
            &self,
            _: &str,
            _: alondra_core::pr::MergeMethod,
            _: Option<&str>,
        ) -> std::result::Result<(), String> {
            self.merges.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn edit_base(&self, _: &str, _: &str) -> std::result::Result<(), String> {
            self.retargets.fetch_add(1, Ordering::SeqCst);
            // Deliberate provider refusal prevents unrelated background GitHub polling.
            Err("synthetic retarget refusal".into())
        }
        async fn view(&self, _: &str) -> Option<Value> {
            panic!("no provider read expected")
        }
        async fn exists(&self, _: &str) -> bool {
            panic!("no provider read expected")
        }
    }
    impl crate::http::Agents for CutExternal {
        async fn run(&self, args: &[&str]) -> std::result::Result<String, String> {
            match args {
                ["rm", "nd"] => {
                    self.removes.fetch_add(1, Ordering::SeqCst);
                    Ok("removed nd".into())
                }
                ["agents", "--json", "--all"] => Ok("[]".into()),
                _ => panic!("unexpected external command"),
            }
        }
    }

    /// Real service envelope -> loopback HTTP -> production merge/close executors.
    /// SQLite aborts at the local marker after the provider has succeeded. A second
    /// injected failure prevents completing the transport receipt, modelling loss
    /// of the result at this cut. Reopening must return uncertainty, never dispatch.
    async fn external_cut_recovery(merge: bool, cut: bool) {
        use alondra_core::{
            Event,
            pr::{MergeMethod, PrState},
        };
        let (_directory, mut service, phone) = push_fixture();
        activate_push(&service);
        let config = alondra_core::config::Config::resolve(
            service.home.clone(),
            &Default::default(),
            |_| None,
            || Some("Fixture".into()),
        );
        let hub = Arc::new(crate::hub::Hub::open(config).unwrap());
        hub.append(Event::DaemonSeen(alondra_core::DaemonObservation {
            session_id: "nd".into(),
            short_id: "nd".into(),
            pid: Some(1),
            ..Default::default()
        }))
        .unwrap();
        if merge {
            for (id, base, head) in [(1, "main", "branch-a"), (2, "branch-a", "branch-b")] {
                hub.append(Event::PrOpened {
                    url: format!("https://github.com/fixture/repo/pull/{id}"),
                    session_id: "nd".into(),
                    task: None,
                })
                .unwrap();
                let mut pr = hub.read(|v| v.prs[&id].clone());
                pr.base = Some(base.into());
                pr.head = Some(head.into());
                hub.append(Event::PrUpdated(pr)).unwrap();
            }
            assert_eq!(
                hub.read(|v| v.retarget_plan(1).len()),
                1,
                "retarget boundary must be non-vacuous"
            );
        }
        let db = Connection::open(service.home.join("alondra.db")).unwrap();
        if cut {
            let marker = if merge { "pr_merged" } else { "node_removed" };
            db.execute_batch(&format!("CREATE TRIGGER cut_local BEFORE INSERT ON event_log WHEN NEW.body LIKE '%{marker}%' BEGIN SELECT RAISE(ABORT,'synthetic post-effect cut'); END;")).unwrap();
            service.journal.db().unwrap().execute_batch("CREATE TRIGGER cut_receipt BEFORE UPDATE OF response ON service_receipts BEGIN SELECT RAISE(ABORT,'synthetic lost receipt'); END;").unwrap();
        }
        let external = Arc::new(CutExternal::default());
        let entered = Arc::new(AtomicUsize::new(0));
        let route = if merge {
            "/v1/prs/1/merge"
        } else {
            "/v1/nodes/nd/close"
        };
        let local = Router::new().route(
            route,
            post({
                let hub = hub.clone();
                let external = external.clone();
                let entered = entered.clone();
                move || {
                    let hub = hub.clone();
                    let external = external.clone();
                    let entered = entered.clone();
                    async move {
                        entered.fetch_add(1, Ordering::SeqCst);
                        if merge {
                            match crate::http::merge(
                                &hub,
                                1,
                                MergeMethod::Merge,
                                crate::http::Merger::User("fixture".into()),
                                external.as_ref(),
                            )
                            .await
                            {
                                Ok(()) => (StatusCode::OK, axum::Json(json!({"merged":true}))),
                                Err(_) => (
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    axum::Json(json!({"error":"local_marker_failed"})),
                                ),
                            }
                            .into_response()
                        } else {
                            crate::http::close_with(&hub, "nd", false, None, external.as_ref())
                                .await
                        }
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        service.local_port = listener.local_addr().unwrap().port();
        let local_server = tokio::spawn(axum::serve(listener, local).into_future());
        let responses = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let relay = Router::new()
            .route(
                "/v1/work/{id}/receipt",
                post(|| async { StatusCode::ACCEPTED }),
            )
            .route(
                "/v1/work/{id}/response",
                post({
                    let responses = responses.clone();
                    move |body: Bytes| {
                        let responses = responses.clone();
                        async move {
                            responses.lock().unwrap().push(body.to_vec());
                            StatusCode::ACCEPTED
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
        let relay_server = tokio::spawn(axum::serve(listener, relay).into_future());
        let work = changes_work(
            &service,
            "external-cut",
            json!({"method":"POST","path":route,"body":{"method":"merge"}}),
        );
        let digest: [u8; 32] = Sha256::digest(serde_json::to_vec(&work.envelope).unwrap()).into();
        let result = service.handle(repeat_work(&work), &[7; 16]).await;
        assert_eq!(result.is_err(), cut);
        assert_eq!(entered.load(Ordering::SeqCst), 1);
        assert_eq!(external.merges.load(Ordering::SeqCst), usize::from(merge));
        assert_eq!(external.removes.load(Ordering::SeqCst), usize::from(!merge));
        assert_eq!(
            external.retargets.load(Ordering::SeqCst),
            usize::from(merge && !cut)
        );
        if merge {
            assert_eq!(
                hub.read(|v| v.prs[&1].state),
                if cut { PrState::Open } else { PrState::Merged }
            );
        } else {
            assert_eq!(hub.read(|v| v.nodes.contains_key("nd")), cut);
        }
        if cut {
            assert!(responses.lock().unwrap().is_empty());
            assert!(matches!(
                service
                    .journal
                    .accept("phone-1", "external-cut", &digest)
                    .unwrap(),
                Disposition::Executing
            ));
            db.execute_batch("DROP TRIGGER cut_local").unwrap();
            service
                .journal
                .db()
                .unwrap()
                .execute_batch("DROP TRIGGER cut_receipt")
                .unwrap();
        }
        let reopened_hub = crate::hub::Hub::open(hub.config.clone()).unwrap();
        if merge {
            assert_eq!(
                reopened_hub.read(|v| v.prs[&1].state),
                if cut { PrState::Open } else { PrState::Merged }
            );
        } else {
            assert_eq!(reopened_hub.read(|v| v.nodes.contains_key("nd")), cut);
        }
        let mut restarted = Service::open(
            &service.home,
            service.local_port,
            Arc::new(Auth::open(&service.home).unwrap()),
        )
        .unwrap()
        .unwrap();
        restarted.set_relay_url(service.relay_url());
        restarted
            .handle(repeat_work(&work), &[7; 16])
            .await
            .unwrap();
        let recovered = opened_results(&restarted, &phone, &responses);
        if cut {
            assert_eq!(recovered.last().unwrap()["error"], "uncertain");
            assert_eq!(recovered.last().unwrap()["status"], 409);
        } else {
            assert_eq!(recovered.last().unwrap()["status"], 200);
        }
        restarted.handle(work, &[7; 16]).await.unwrap();
        let bytes = responses.lock().unwrap();
        assert_eq!(
            bytes[bytes.len() - 1],
            bytes[bytes.len() - 2],
            "recovery receipt must replay identical ciphertext"
        );
        drop(bytes);
        assert_eq!(
            entered.load(Ordering::SeqCst),
            1,
            "restart must not redispatch either executor"
        );
        assert_eq!(external.merges.load(Ordering::SeqCst), usize::from(merge));
        assert_eq!(external.removes.load(Ordering::SeqCst), usize::from(!merge));
        assert_eq!(
            external.retargets.load(Ordering::SeqCst),
            usize::from(merge && !cut)
        );
        local_server.abort();
        relay_server.abort();
        let _ = local_server.await;
        let _ = relay_server.await;
        let home = service.home.clone();
        drop(reopened_hub);
        drop(db);
        drop(hub);
        drop(restarted);
        drop(service);
        std::fs::remove_dir_all(home).unwrap();
    }
    #[tokio::test]
    async fn merge_external_success_before_local_marker_recovers_without_replay() {
        external_cut_recovery(true, true).await;
        external_cut_recovery(true, false).await;
    }
    #[tokio::test]
    async fn close_external_success_before_node_removed_recovers_without_replay() {
        external_cut_recovery(false, true).await;
        external_cut_recovery(false, false).await;
    }

    #[test]
    fn revocation_waits_for_in_flight_delivery_without_stalling_the_runtime() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-revoke-during-delivery-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let scenario_home = home.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let outcome = runtime.block_on(async move {
                let deletions = Arc::new(AtomicUsize::new(0));
                let counted = deletions.clone();
                let relay = axum::Router::new().route(
                    "/v1/devices/{id}",
                    delete(move || {
                        let counted = counted.clone();
                        async move {
                            counted.fetch_add(1, Ordering::SeqCst);
                            StatusCode::NO_CONTENT
                        }
                    }),
                );
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let port = listener.local_addr().unwrap().port();
                tokio::spawn(axum::serve(listener, relay).into_future());
                let service = Service {
                    relay: Some(relay::RelayConnector::new(Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap(), "owner".into())),
                    direct: DirectConfig::default(),
                    lane: Arc::new(tokio::sync::Mutex::new(())),
                    #[cfg(test)]
                    envelope_clock: None,
                    client: Client::new(),
                    mac_id: "mac-1".into(),
                    signing: SigningKey::from_slice(&[1u8; 32]).unwrap(),
                    agreement: SecretKey::from_slice(&[2u8; 32]).unwrap(),
                    devices: HashMap::new().into(),
                    home: scenario_home.clone(),
                    transfers: Arc::default(),
                    inflight: Arc::default(),
                    hub: None,
                    domain: None,
                    routes: None,
                    revoked: crate::revoked_devices::RevokedDevices::empty(),
                    auth: Arc::new(Auth::open(&scenario_home).unwrap()),
                    journal: Journal::open(scenario_home.join("service-receipts.sqlite")).unwrap(),
                    local_port: 0,
                };
                service.journal.reserve_enrollment("phone-gone", &[1u8; 33]).unwrap();
                service.journal.activate_enrollment("phone-gone").unwrap();
                let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
                let delivered = Arc::new(AtomicBool::new(false));
                let delivery_flag = delivered.clone();
                let delivery_home = scenario_home.clone();
                let delivery = tokio::spawn(async move {
                    approval_bound_delivery(
                        &delivery_home,
                        || Ok(()),
                        || async move {
                            entered_tx.send(()).unwrap();
                            tokio::time::sleep(Duration::from_millis(200)).await;
                            delivery_flag.store(true, Ordering::SeqCst);
                            Ok(())
                        },
                    )
                    .await
                });
                entered_rx.await.unwrap();
                service
                    .sync_revocations(&[3u8; 16], &mut HashSet::new())
                    .await
                    .unwrap();
                assert!(
                    delivered.load(Ordering::SeqCst),
                    "revocation completed while a delivery still held its approval lease"
                );
                delivery.await.unwrap().unwrap();
                assert!(service.journal.enrollment_revoked("phone-gone").unwrap());
                deletions.load(Ordering::SeqCst)
            });
            done_tx.send(outcome).unwrap();
        });
        let deletions = done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("revocation deadlocked the runtime behind an in-flight delivery");
        assert_eq!(deletions, 1);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn durable_receipt_rejects_changed_bytes_and_never_reruns_executing() {
        let scratch_directory = crate::test_support::Directory::new();
        let path = scratch_directory.parent().unwrap().join(format!(
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
        let scratch_directory = crate::test_support::Directory::new();
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
        let home = scratch_directory.parent().unwrap().join(format!(
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
            relay: Some(relay::RelayConnector::new(Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap(), "owner".into())),
            direct: DirectConfig::default(),
            lane: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(test)]
            envelope_clock: None,
            client: Client::new(),
            mac_id: "mac-1".into(),
            signing: SigningKey::from_slice(&[1u8; 32]).unwrap(),
            agreement: SecretKey::from_slice(&[2u8; 32]).unwrap(),
            devices: HashMap::from([(
                "phone-pending".into(),
                Device {
                    signing: *phone.verifying_key(),
                    agreement: phone_agreement.public_key(),
                    signing_wire: URL_SAFE_NO_PAD.encode(phone.verifying_key().to_sec1_bytes()),
                },
            )])
            .into(),
            home: home.clone(),
            transfers: Arc::default(),
            inflight: Arc::default(),
            hub: None,
            domain: None,
            routes: None,
            revoked: crate::revoked_devices::RevokedDevices::empty(),
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
        let request = Envelope::seal_random(
            EnvelopeContext {
                route: "mac-1",
                sender: "phone-pending",
                recipient: "mac-1",
                request_id: "op-pending",
                kind: "request",
                expires_at_ms: now_ms() + 60_000,
            },
            br#"{"method":"GET","path":"/summary"}"#,
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
        let scratch_directory = crate::test_support::Directory::new();
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
        let home = scratch_directory.parent().unwrap().join(format!(
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
            relay: Some(relay::RelayConnector::new(Url::parse(&format!("http://127.0.0.1:{relay_port}/")).unwrap(), "owner".into())),
            direct: DirectConfig::default(),
            lane: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(test)]
            envelope_clock: None,
            client: Client::new(),
            mac_id: "mac-1".into(),
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
            )])
            .into(),
            home: home.clone(),
            transfers: Arc::default(),
            inflight: Arc::default(),
            hub: None,
            domain: None,
            routes: None,
            revoked: crate::revoked_devices::RevokedDevices::empty(),
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
            relay: Some(relay::RelayConnector::new(service.relay_url(), "owner".into())),
            direct: DirectConfig::default(),
            lane: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(test)]
            envelope_clock: None,
            client: Client::new(),
            mac_id: service.mac_id.clone(),
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
            )])
            .into(),
            home: home.clone(),
            transfers: Arc::default(),
            inflight: Arc::default(),
            hub: None,
            domain: None,
            routes: None,
            revoked: crate::revoked_devices::RevokedDevices::empty(),
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
        let e = Envelope::seal_random(
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
        let near_limit = json!({"status":200,"body":{"text":"a".repeat(MAX_LOCAL_RESPONSE - 128)}});
        assert!(serde_json::to_vec(&near_limit).unwrap().len() <= MAX_LOCAL_RESPONSE);
        let delivered = service.encode_response(
            &e,
            &service.devices.get("phone-1").unwrap(),
            &near_limit,
            &LocalRequest { method: "GET".into(), path: "/views/card:i-1".into(), body: None },
        ).unwrap();
        assert!(delivered.len() > 256 * 1024 && delivered.len() <= MAX_RELAY_RESPONSE);
        let envelope: Envelope = serde_json::from_slice(&delivered).unwrap();
        let opened = envelope.open(service.signing.verifying_key(), &phone_agreement,
            "mac-1", "phone-1", "mac-1", now_ms()).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&opened).unwrap(), near_limit);
        let oversized = service
            .encode_response(
                &e,
                &service.devices.get("phone-1").unwrap(),
                &json!({"status":200,"body":{"text":"\u{0}".repeat(MAX_LOCAL_RESPONSE)}}),
                &LocalRequest {
                    method: "GET".into(),
                    path: "/v1/file?path=test".into(),
                    body: None,
                },
            )
            .unwrap();
        assert!(oversized.len() <= MAX_RELAY_RESPONSE);
        let oversized_envelope: Envelope = serde_json::from_slice(&oversized).unwrap();
        let opened = oversized_envelope
            .open(
                service.signing.verifying_key(),
                &phone_agreement,
                "mac-1",
                "phone-1",
                "mac-1",
                now_ms(),
            )
            .unwrap();
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
        let response: Envelope = serde_json::from_slice(&sent[0]).unwrap();
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
        let interrupted = Envelope::seal_random(
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
        let uncertain: Envelope = serde_json::from_slice(saved.last().unwrap()).unwrap();
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
            relay: Some(relay::RelayConnector::new(service.relay_url(), "owner".into())),
            direct: DirectConfig::default(),
            lane: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(test)]
            envelope_clock: None,
            client: Client::new(),
            mac_id: service.mac_id.clone(),
            signing: SigningKey::from_slice(&[1u8; 32]).unwrap(),
            agreement: SecretKey::from_slice(&[2u8; 32]).unwrap(),
            devices: HashMap::new().into(),
            home: home.clone(),
            transfers: Arc::default(),
            inflight: Arc::default(),
            hub: None,
            domain: None,
            routes: None,
            revoked: crate::revoked_devices::RevokedDevices::empty(),
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
        let revoked = Envelope::seal_random(
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
    #[tokio::test]
    async fn reloading_service_toml_adds_and_removes_a_phone_without_a_restart() {
        let scratch_directory = crate::test_support::Directory::new();
        use std::os::unix::fs::PermissionsExt;
        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-service-reload-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir_all(&home).unwrap();
        let phone_signing = SigningKey::from_slice(&[3u8; 32]).unwrap();
        let phone_agreement = SecretKey::from_slice(&[4u8; 32]).unwrap();
        let signing_wire = URL_SAFE_NO_PAD.encode(phone_signing.verifying_key().to_sec1_bytes());
        let agreement_wire = URL_SAFE_NO_PAD.encode(phone_agreement.public_key().to_sec1_bytes());
        for (name, secret) in [
            ("mac-signing.key", [1u8; 32]),
            ("mac-agreement.key", [2u8; 32]),
        ] {
            let path = home.join(name);
            std::fs::write(&path, URL_SAFE_NO_PAD.encode(secret)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let config = |devices: &str| {
            format!(
                "relay_url = \"http://127.0.0.1:1/\"\nmac_id = \"mac-1\"\nowner_sub = \"owner\"\nsigning_key_path = \"mac-signing.key\"\nagreement_key_path = \"mac-agreement.key\"\n{devices}"
            )
        };
        std::fs::write(home.join("service.toml"), config("devices = []\n")).unwrap();
        let auth = Arc::new(Auth::open(&home).unwrap());
        let service = Service::open(&home, 0, auth.clone()).unwrap().unwrap();
        let work = Work {
            request_id: "request-1".into(),
            envelope: Envelope::seal_random(
                EnvelopeContext {
                    route: "mac-1",
                    sender: "phone-a",
                    recipient: "mac-1",
                    request_id: "request-1",
                    kind: "request",
                    expires_at_ms: now_ms() + 60_000,
                },
                serde_json::to_vec(&json!({"method":"GET","path":"/capabilities"}))
                    .unwrap()
                    .as_slice(),
                &phone_signing,
                &service.agreement.public_key(),
            )
            .unwrap(),
        };
        assert!(
            service.prepare_work(&work, Transport::Relay).is_err(),
            "no phone is configured yet"
        );

        auth.approve_service("phone-a", "Test", &signing_wire)
            .unwrap();
        service
            .journal
            .reserve_enrollment("phone-a", &phone_signing.verifying_key().to_sec1_bytes())
            .unwrap();
        service.journal.activate_enrollment("phone-a").unwrap();
        std::fs::write(home.join("service.toml"), config(&format!(
            "[[devices]]\ndevice_id = \"phone-a\"\nsigning_key = \"{signing_wire}\"\nagreement_key = \"{agreement_wire}\"\n"
        ))).unwrap();
        assert!(
            service.prepare_work(&work, Transport::Relay).is_err(),
            "the running service still has the list it started with"
        );
        service.reload_devices().unwrap();
        assert!(
            service.relay.as_ref().unwrap().resync_pending(),
            "the connector loop must enroll the new phone"
        );
        assert!(
            service.prepare_work(&work, Transport::Relay).unwrap().is_some(),
            "the reloaded service accepts the phone"
        );

        std::fs::write(home.join("service.toml"), config("devices = []\n")).unwrap();
        service.reload_devices().unwrap();
        assert!(
            !service.has_device("phone-a"),
            "a phone removed from service.toml is gone without a restart"
        );
        assert!(service.prepare_work(&work, Transport::Relay).is_err());

        std::fs::write(home.join("service.toml"), "not toml").unwrap();
        assert!(service.reload_devices().is_err());
        drop(service);
        drop(auth);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn committed_service_accepts_only_encrypted_domain_routes_and_fences_revoked_reader() {
        let scratch_directory = crate::test_support::Directory::new();
        use std::os::unix::fs::PermissionsExt;
        let home =
            scratch_directory.parent().unwrap().join(format!("alondra-service-{}", crate::auth::random_hex(8)));
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let phone_signing = SigningKey::from_slice(&[3u8; 32]).unwrap();
        let phone_agreement = SecretKey::from_slice(&[4u8; 32]).unwrap();
        let signing_wire = URL_SAFE_NO_PAD.encode(phone_signing.verifying_key().to_sec1_bytes());
        let agreement_wire = URL_SAFE_NO_PAD.encode(phone_agreement.public_key().to_sec1_bytes());
        for (name, secret) in [
            ("mac-signing.key", [1u8; 32]),
            ("mac-agreement.key", [2u8; 32]),
        ] {
            let path = home.join(name);
            std::fs::write(&path, URL_SAFE_NO_PAD.encode(secret)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::fs::write(home.join("service.toml"), format!(
            "relay_url = \"http://127.0.0.1:1/\"\nmac_id = \"mac-1\"\nowner_sub = \"owner\"\nsigning_key_path = \"mac-signing.key\"\nagreement_key_path = \"mac-agreement.key\"\n[[devices]]\ndevice_id = \"phone-a\"\nsigning_key = \"{signing_wire}\"\nagreement_key = \"{agreement_wire}\"\n"
        )).unwrap();
        let auth = Arc::new(Auth::open(&home).unwrap());
        auth.approve_service("phone-a", "Test", &signing_wire)
            .unwrap();
        let service = Service::open(&home, 0, auth.clone())
            .unwrap()
            .unwrap()
            .with_domain(domain.clone());
        service
            .journal
            .reserve_enrollment("phone-a", &phone_signing.verifying_key().to_sec1_bytes())
            .unwrap();
        service.journal.activate_enrollment("phone-a").unwrap();
        let sealed = |request_id: &str, path: &str| Work {
            request_id: request_id.into(),
            envelope: Envelope::seal_random(
                EnvelopeContext {
                    route: "mac-1",
                    sender: "phone-a",
                    recipient: "mac-1",
                    request_id,
                    kind: "request",
                    expires_at_ms: now_ms() + 60_000,
                },
                serde_json::to_vec(&json!({"method":"GET","path":path}))
                    .unwrap()
                    .as_slice(),
                &phone_signing,
                &service.agreement.public_key(),
            )
            .unwrap(),
        };
        assert!(
            service
                .prepare_work(&sealed("old", "/v1/overview"), Transport::Relay)
                .is_err()
        );
        assert!(service.prepare_work(&sealed("pair", "/v1/pair"), Transport::Relay).is_err());
        let valid = sealed("domain", "/capabilities");
        assert!(service.prepare_work(&valid, Transport::Relay).unwrap().is_some());
        let read = service
            .dispatch(
                "phone-a",
                &LocalRequest {
                    method: "GET".into(),
                    path: "/capabilities".into(),
                    body: None,
                }, Transport::Relay,
            )
            .await
            .unwrap();
        assert_eq!(read["status"], 200);
        assert_eq!(read["body"]["protocol_version"], 2);
        auth.revoke("phone-a").unwrap();
        assert!(service.prepare_work(&sealed("revoked", "/domain"), Transport::Relay).is_err());
        assert!(
            service
                .dispatch(
                    "phone-a",
                    &LocalRequest {
                        method: "GET".into(),
                        path: "/domain".into(),
                        body: None,
                    }, Transport::Relay
                )
                .await
                .is_err()
        );
        drop(service);
        drop(domain);
        drop(auth);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn committed_service_relays_the_client_reads_and_nothing_else_outside_the_domain() {
        let scratch_directory = crate::test_support::Directory::new();
        use std::os::unix::fs::PermissionsExt;
        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-client-relay-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let state = domain.state().unwrap();
        domain
            .execute(
                &alondra_core::domain::Actor::System,
                &alondra_core::domain::CommandEnvelope {
                    protocol_version: alondra_core::domain::PROTOCOL_VERSION,
                    data_epoch: state.data_epoch.clone(),
                    command_id: "agent".into(),
                    expected_revision: state.revision,
                    command: alondra_core::domain::Command::RegisterAgent {
                        agent_id: "agent-a".into(),
                        name: "Instance A".into(), at: 0
                    },
                },
                None,
                || Ok(()),
            )
            .unwrap();
        domain.live.observe(&json!({"session_id": "agent-a", "hook_event_name": "SessionStart", "cwd": "/Users/me/Projects/App"}));
        let phone_signing = SigningKey::from_slice(&[3u8; 32]).unwrap();
        let phone_agreement = SecretKey::from_slice(&[4u8; 32]).unwrap();
        let signing_wire = URL_SAFE_NO_PAD.encode(phone_signing.verifying_key().to_sec1_bytes());
        let agreement_wire = URL_SAFE_NO_PAD.encode(phone_agreement.public_key().to_sec1_bytes());
        for (name, secret) in [
            ("mac-signing.key", [1u8; 32]),
            ("mac-agreement.key", [2u8; 32]),
        ] {
            let path = home.join(name);
            std::fs::write(&path, URL_SAFE_NO_PAD.encode(secret)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::fs::write(home.join("service.toml"), format!(
            "relay_url = \"http://127.0.0.1:1/\"\nmac_id = \"mac-1\"\nowner_sub = \"owner\"\nsigning_key_path = \"mac-signing.key\"\nagreement_key_path = \"mac-agreement.key\"\n[[devices]]\ndevice_id = \"phone-a\"\nsigning_key = \"{signing_wire}\"\nagreement_key = \"{agreement_wire}\"\n"
        )).unwrap();
        let auth = Arc::new(Auth::open(&home).unwrap());
        auth.approve_service("phone-a", "Test", &signing_wire)
            .unwrap();
        let client =
            crate::client_routes::Client::open(domain.clone(), &home, Default::default());
        let service = Service::open(&home, 0, auth.clone())
            .unwrap()
            .unwrap()
            .with_domain(domain.clone())
            .with_client(client);
        service
            .journal
            .reserve_enrollment("phone-a", &phone_signing.verifying_key().to_sec1_bytes())
            .unwrap();
        service.journal.activate_enrollment("phone-a").unwrap();
        let sealed = |request_id: &str, path: &str| Work {
            request_id: request_id.into(),
            envelope: Envelope::seal_random(
                EnvelopeContext {
                    route: "mac-1",
                    sender: "phone-a",
                    recipient: "mac-1",
                    request_id,
                    kind: "request",
                    expires_at_ms: now_ms() + 60_000,
                },
                serde_json::to_vec(&json!({"method":"GET","path":path}))
                    .unwrap()
                    .as_slice(),
                &phone_signing,
                &service.agreement.public_key(),
            )
            .unwrap(),
        };
        let read = |path: &str| LocalRequest {
            method: "GET".into(),
            path: path.into(),
            body: None,
        };
        for (index, path) in [
            "/views/home?protocol=1&client=ios&lang=en",
            "/views/tasks?protocol=1&client=ios&lang=en",
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                service
                    .prepare_work(&sealed(&format!("accepted-{index}"), path), Transport::Relay)
                    .unwrap()
                    .is_some(),
                "{path} is accepted from a paired phone"
            );
            let answer = service.dispatch("phone-a", &read(path), Transport::Relay).await.unwrap();
            assert_eq!(answer["status"], 200, "{path}: {answer}");
        }
        let home_view = service
            .dispatch("phone-a", &read("/views/home?protocol=1&client=ios&lang=en"), Transport::Relay)
            .await
            .unwrap();
        assert_eq!(home_view["body"]["key"], "home");
        assert!(home_view["body"]["sections"].is_array());
        for (index, path) in ["/nothing", "/runtime?x=1", "/agents/agent-a/nothing"]
            .into_iter()
            .enumerate()
        {
            assert!(
                service
                    .prepare_work(&sealed(&format!("refused-{index}"), path), Transport::Relay)
                    .is_err(),
                "{path} is refused"
            );
        }
        assert!(
            !service.atomic_local(&LocalRequest {
                method: "POST".into(),
                path: "/agents/agent-a/close".into(),
                body: Some(json!({}))
            }),
            "closing a session is journaled as uncertain when it is interrupted, never replayed"
        );
        let closed = service
            .dispatch(
                "phone-a",
                &LocalRequest {
                    method: "POST".into(),
                    path: "/agents/nobody/close".into(),
                    body: Some(json!({})),
                }, Transport::Relay,
            )
            .await
            .unwrap();
        assert_eq!(
            (closed["status"].as_i64(), closed["body"]["code"].as_str()),
            (Some(404), Some("not_found"))
        );
        for (index, (method, path)) in [
            ("POST", "/agents/agent-a/close/extra"),
            ("PUT", "/agents/agent-a/close"),
            ("DELETE", "/agents/agent-a/close"),
        ]
        .into_iter()
        .enumerate()
        {
            let request = LocalRequest {
                method: method.into(),
                path: path.into(),
                body: Some(json!({})),
            };
            assert!(
                approved_method(&request).is_err(),
                "{method} {path} is not a relayed operation ({index})"
            );
        }
        assert!(
            approved_method(&LocalRequest {
                method: "POST".into(),
                path: "/agents/agent-a/close".into(),
                body: None
            })
            .is_err(),
            "a write needs its body"
        );

        let bytes: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        let digest: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let transfer = "01234567-89ab-cdef-0123-456789abcdef";
        let step = |path: &str, body: Value| {
            let request = LocalRequest {
                method: "POST".into(),
                path: format!("/uploads/{path}"),
                body: Some(body),
            };
            let service = &service;
            async move { service.dispatch("phone-a", &request, Transport::Relay).await }
        };
        let started = step("start", json!({"transfer_id": transfer, "name": "shot.png", "bytes": bytes.len(), "sha256": digest})).await.unwrap();
        assert_eq!(started["status"], 200, "{started}");
        for (index, chunk) in bytes.chunks(64 * 1024).enumerate() {
            let sent = step("chunk", json!({"transfer_id": transfer, "offset": index * 64 * 1024, "data": URL_SAFE_NO_PAD.encode(chunk)})).await.unwrap();
            assert_eq!(sent["status"], 200, "{sent}");
        }
        let finished = step("finish", json!({"transfer_id": transfer}))
            .await
            .unwrap();
        assert_eq!(finished["status"], 200, "{finished}");
        let attachment: alondra_core::domain::AttachmentRef =
            serde_json::from_value(finished["body"]["attachment"].clone()).unwrap();
        assert_eq!(
            (
                attachment.name.as_str(),
                attachment.bytes,
                attachment.content_digest.as_str()
            ),
            ("shot.png", 70_000, digest.as_str())
        );
        assert!(
            home.join(&attachment.content_reference).is_file(),
            "the reference is relative to this home: {}",
            attachment.content_reference
        );

        let mut fetched = Vec::new();
        let mut version: Option<String> = None;
        loop {
            let mut path = format!(
                "/files/chunk?path={}&offset={}",
                url::form_urlencoded::byte_serialize(attachment.content_reference.as_bytes())
                    .collect::<String>(),
                fetched.len()
            );
            if let Some(version) = &version {
                path.push_str(&format!("&version={version}"));
            }
            let chunk = service.dispatch("phone-a", &read(&path), Transport::Relay).await.unwrap();
            assert_eq!(chunk["status"], 200, "{chunk}");
            let chunk = &chunk["body"];
            assert_eq!(
                chunk["content_digest"],
                digest.as_str(),
                "the daemon names the digest of the whole file"
            );
            version = Some(chunk["version"].as_str().unwrap().to_owned());
            fetched.extend(
                URL_SAFE_NO_PAD
                    .decode(chunk["data"].as_str().unwrap())
                    .unwrap(),
            );
            assert_eq!(chunk["next_offset"], fetched.len());
            if chunk["eof"] == true {
                break;
            }
        }
        assert_eq!(
            fetched, bytes,
            "the file comes back whole, in more than one chunk"
        );
        let outside = service
            .dispatch(
                "phone-a",
                &read("/files/chunk?path=%2Fetc%2Fhosts&offset=0"), Transport::Relay,
            )
            .await
            .unwrap();
        assert_eq!(
            outside["body"]["code"], "content_unavailable",
            "only stored copies are served"
        );

        let other = "89abcdef-0123-4567-89ab-cdef01234567";
        assert_eq!(
            step(
                "start",
                json!({"transfer_id": other, "name": "b.png", "bytes": 3, "sha256": "a".repeat(64)})
            )
            .await
            .unwrap()["status"],
            200
        );
        assert_eq!(
            step("cancel", json!({"transfer_id": other})).await.unwrap()["body"],
            json!({"cancelled": true})
        );
        let after_cancel = step(
            "chunk",
            json!({"transfer_id": other, "offset": 0, "data": "AAEC"}),
        )
        .await
        .unwrap();
        assert_eq!(
            (
                after_cancel["status"].as_i64(),
                after_cancel["body"]["error"].as_str()
            ),
            (Some(422), Some("attachment_transfer_unknown"))
        );

        for path in [
            "/options",
            "/options?project=App",
            "/projects",
            "/issues?refresh=1",
        ] {
            assert!(
                approved_method(&LocalRequest {
                    method: "GET".into(),
                    path: path.into(),
                    body: None
                })
                .is_ok(),
                "{path} is relayed"
            );
        }
        let options = service
            .dispatch("phone-a", &read("/options"), Transport::Relay)
            .await
            .unwrap();
        assert_eq!(
            (
                options["status"].as_i64(),
                options["body"]["options"][0]["id"].as_str()
            ),
            (Some(200), Some("pr"))
        );
        assert_eq!(
            service
                .dispatch("phone-a", &read("/projects"), Transport::Relay)
                .await
                .unwrap()["status"],
            200
        );
        assert!(service.atomic_local(&LocalRequest {
            method: "POST".into(),
            path: "/requests".into(),
            body: Some(json!({}))
        }));
        let new_request = Work {
            request_id: "request-1".into(),
            envelope: Envelope::seal_random(EnvelopeContext {
                route: "mac-1", sender: "phone-a", recipient: "mac-1", request_id: "request-1",
                kind: "request", expires_at_ms: now_ms() + 60_000,
            }, serde_json::to_vec(&json!({"method":"POST","path":"/requests","body":{"request_id": "request-9", "text": "Ship it", "options": {"effort": "hard"}, "attachments": []}})).unwrap().as_slice(),
            &phone_signing, &service.agreement.public_key()).unwrap(),
        };
        let prepared = service.prepare_work(&new_request, Transport::Relay).unwrap().unwrap();
        service
            .journal
            .accept("phone-a", "request-1", &prepared.digest)
            .unwrap();
        service
            .journal
            .executing_local("phone-a", "request-1", &prepared.digest)
            .unwrap();
        let device = service.devices.get("phone-a").unwrap();
        let bytes = service
            .execute_atomic_local(
                &new_request.envelope,
                &device,
                &prepared.request,
                &prepared.digest, Transport::Relay,
                service.journal.reserve_receipt(prepared.guard.clone()).unwrap(),
            )
            .await
            .unwrap();
        let sealed_answer: Envelope = serde_json::from_slice(&bytes).unwrap();
        let answer: Value = serde_json::from_slice(
            &sealed_answer
                .open(
                    service.signing.verifying_key(),
                    &phone_agreement,
                    "mac-1",
                    "phone-a",
                    "mac-1",
                    now_ms(),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            (
                answer["status"].as_i64(),
                answer["body"]["created_ids"].clone()
            ),
            (Some(200), json!(["request-9"])),
            "{answer}"
        );
        assert_eq!(
            domain
                .request_launch(&"request-9".into())
                .unwrap()
                .unwrap()
                .options
                .effort,
            "hard",
            "the phone's choices are kept with its request"
        );

        for path in ["/tasks/task-1/history"] {
            assert!(
                approved_method(&LocalRequest {
                    method: "GET".into(),
                    path: path.into(),
                    body: None
                })
                .is_ok(),
                "{path} is relayed"
            );
        }
        assert_eq!(
            service
                .dispatch("phone-a", &read("/tasks/nothing/history"), Transport::Relay)
                .await
                .unwrap()["status"],
            404
        );
        let put = |path: &str, body: Option<Value>| LocalRequest {
            method: "PUT".into(),
            path: path.into(),
            body,
        };
        assert!(
            approved_method(&put(
                "/tasks/task-1/cancel-reason",
                Some(json!({"reason": "x"}))
            ))
            .is_ok()
        );
        assert!(
            approved_method(&put("/tasks/task-1/cancel-reason", None)).is_err(),
            "a write needs its body"
        );
        assert!(
            approved_method(&LocalRequest {
                method: "POST".into(),
                path: "/tasks/task-1/cancel-reason".into(),
                body: Some(json!({}))
            })
            .is_err(),
            "it is a PUT"
        );
        assert!(
            !service.atomic_local(&put("/tasks/task-1/cancel-reason", Some(json!({})))),
            "recording a reason is journaled as uncertain when interrupted"
        );
        let missing = service
            .dispatch(
                "phone-a",
                &put("/tasks/nothing/cancel-reason", Some(json!({"reason": "x"}))), Transport::Relay,
            )
            .await
            .unwrap();
        assert_eq!(
            (missing["status"].as_i64(), missing["body"]["code"].as_str()),
            (Some(404), Some("not_found"))
        );
        let presence = service
            .dispatch(
                "phone-a",
                &LocalRequest {
                    method: "POST".into(),
                    path: "/activity".into(),
                    body: Some(json!({"client": "phone"})),
                }, Transport::Relay,
            )
            .await
            .unwrap();
        assert_eq!(
            (presence["status"].as_i64(), &presence["body"]),
            (Some(200), &json!({}))
        );

        auth.revoke("phone-a").unwrap();
        assert!(
            service
                .dispatch("phone-a", &read("/runtime"), Transport::Relay)
                .await
                .is_err(),
            "a revoked phone reads nothing"
        );
        drop(service);
        drop(domain);
        drop(auth);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn a_paired_phone_reads_and_changes_the_settings_through_the_relay() {
        use std::os::unix::fs::PermissionsExt;
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*home).unwrap();
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let phone_signing = SigningKey::from_slice(&[3u8; 32]).unwrap();
        let phone_agreement = SecretKey::from_slice(&[4u8; 32]).unwrap();
        let signing_wire = URL_SAFE_NO_PAD.encode(phone_signing.verifying_key().to_sec1_bytes());
        let agreement_wire = URL_SAFE_NO_PAD.encode(phone_agreement.public_key().to_sec1_bytes());
        for (name, secret) in [("mac-signing.key", [1u8; 32]), ("mac-agreement.key", [2u8; 32])] {
            let path = home.join(name);
            std::fs::write(&path, URL_SAFE_NO_PAD.encode(secret)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::fs::write(home.join("service.toml"), format!(
            "relay_url = \"http://127.0.0.1:1/\"\nmac_id = \"mac-1\"\nowner_sub = \"owner\"\nsigning_key_path = \"mac-signing.key\"\nagreement_key_path = \"mac-agreement.key\"\n[[devices]]\ndevice_id = \"phone-a\"\nsigning_key = \"{signing_wire}\"\nagreement_key = \"{agreement_wire}\"\n"
        )).unwrap();
        let auth = Arc::new(Auth::open(&home).unwrap());
        auth.approve_service("phone-a", "Test", &signing_wire).unwrap();
        let client = crate::client_routes::Client::open(domain.clone(), &home, Default::default());
        let service = Service::open(&home, 0, auth.clone()).unwrap().unwrap()
            .with_domain(domain.clone()).with_client(client);
        service.journal.reserve_enrollment("phone-a", &phone_signing.verifying_key().to_sec1_bytes()).unwrap();
        service.journal.activate_enrollment("phone-a").unwrap();
        let request = |method: &str, path: &str, body: Option<Value>| LocalRequest { method: method.into(), path: path.into(), body };

        for (method, path, body) in [("GET", "/settings", None), ("GET", "/settings/folders", None),
            ("GET", "/settings/folders?path=%2FUsers", None), ("PUT", "/settings", Some(json!({})))] {
            assert!(approved_method(&request(method, path, body)).is_ok(), "{method} {path} is relayed");
        }
        for (method, path, body) in [("PUT", "/settings", None), ("GET", "/settings", Some(json!({}))),
            ("POST", "/settings", Some(json!({}))), ("GET", "/settings/folders?path=a&x=1", None), ("GET", "/settings/x", None)] {
            assert!(approved_method(&request(method, path, body)).is_err(), "{method} {path} is refused");
        }

        let read = service.dispatch("phone-a", &request("GET", "/settings", None), Transport::Relay).await.unwrap();
        assert_eq!((read["status"].as_i64(), read["body"]["relay"]["on"].as_bool()), (Some(200), Some(true)), "{read}");
        let chosen = home.join("Chosen");
        let changed = service.dispatch("phone-a",
            &request("PUT", "/settings", Some(json!({"projects_root": chosen, "lang": "en"}))), Transport::Relay).await.unwrap();
        assert_eq!(changed["status"], 200, "{changed}");
        assert_eq!(changed["body"]["projects_root"], json!(chosen));
        assert!(chosen.is_dir(), "a missing folder is created");
        let refused = service.dispatch("phone-a",
            &request("PUT", "/settings", Some(json!({"projects_root": "/"}))), Transport::Relay).await.unwrap();
        assert_eq!((refused["status"].as_i64(), refused["body"]["error"].as_str()), (Some(422), Some("whole_disk")));
        let browse = service.dispatch("phone-a", &request("GET", "/settings/folders", None), Transport::Relay).await.unwrap();
        assert_eq!(browse["status"], 200, "{browse}");
        assert!(service.dispatch("phone-z", &request("GET", "/settings", None), Transport::Relay).await.is_err(),
            "a device that is not paired gets nothing");
        drop(service);
        drop(domain);
    }
}

#[cfg(test)]
impl Service {
    pub(crate) async fn process_fixture_service(
        directory: Arc<crate::test_support::Directory>,
        domain: Arc<crate::domain_runtime::DomainRuntime>,
        relay_url: &str,
    ) -> Arc<Self> {
        let home: &Path = &directory;
        let signing = SigningKey::from_slice(&[33u8; 32]).unwrap();
        let agreement = SecretKey::from_slice(&[34u8; 32]).unwrap();
        let noise_signing = SigningKey::from_slice(&[35u8; 32]).unwrap();
        let noise_agreement = SecretKey::from_slice(&[36u8; 32]).unwrap();
        if !home.join("service.toml").exists() {
            for (name, bytes) in [("signing.key", [1u8; 32]), ("agreement.key", [2u8; 32])] {
                let path = home.join(name);
                std::fs::write(&path, URL_SAFE_NO_PAD.encode(bytes)).unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
                }
            }
            std::fs::write(
            home.join("devices.json"),
            serde_json::to_vec(&json!([{
                "id":"phone-1","name":"Fixture iPhone","created_at":0,"last_seen":null,
                "public_key":base64::engine::general_purpose::STANDARD.encode(signing.verifying_key().to_sec1_bytes())
            }, {
                "id":"noise-phone","name":"Fixture noisy iPhone","created_at":0,"last_seen":null,
                "public_key":base64::engine::general_purpose::STANDARD.encode(noise_signing.verifying_key().to_sec1_bytes())
            }]))
            .unwrap(),
        )
        .unwrap();
            let config = format!(
                r#"relay_url = "{relay_url}"
mac_id = "mac-1"
owner_sub = "owner"
signing_key_path = "signing.key"
agreement_key_path = "agreement.key"
[[devices]]
device_id = "phone-1"
signing_key = "{}"
agreement_key = "{}"
[[devices]]
device_id = "noise-phone"
signing_key = "{}"
agreement_key = "{}"
"#,
                URL_SAFE_NO_PAD.encode(signing.verifying_key().to_sec1_bytes()),
                URL_SAFE_NO_PAD.encode(agreement.public_key().to_sec1_bytes()),
                URL_SAFE_NO_PAD.encode(noise_signing.verifying_key().to_sec1_bytes()),
                URL_SAFE_NO_PAD.encode(noise_agreement.public_key().to_sec1_bytes())
            );
            std::fs::write(home.join("service.toml"), config).unwrap();
        }
        let client = crate::client_routes::Client::open(domain.clone(), home, Default::default());
        let mut service = Service::open(home, 0, Arc::new(Auth::open(home).unwrap()))
            .unwrap()
            .unwrap()
            .with_domain(domain)
            .with_client(client);
        assert_eq!(
            service.relay_url(),
            reqwest::Url::parse(relay_url).unwrap(),
            "fixture relay identity changed"
        );
        assert_eq!(&service.signing.to_bytes()[..], &[1u8; 32]);
        assert_eq!(&service.agreement.to_bytes()[..], &[2u8; 32]);
        service.client = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        service.journal._fixture_directory = Some(directory.clone());
        let service = Arc::new(service);
        let epoch = service.bootstrap().await.unwrap();
        service.enroll(&epoch).await.unwrap();
        service
    }
}
